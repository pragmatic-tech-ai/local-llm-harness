//! In-process pure-Rust candle engine (behind the `candle` feature).
//!
//! This module is compiled only when the `candle` cargo feature is enabled. Unlike the llama.cpp
//! engine, candle's CPU path is pure Rust: the default build still never sees this code, but when
//! enabled it needs no CUDA/clang/cmake toolchain. It runs a quantized Gemma 3 GGUF through
//! HuggingFace `candle` (`quantized_gemma3`), driving the decode loop, KV-cache stepping, sampling
//! and streaming by hand.
//!
//! The engine stays stateless: the session always re-sends the whole prompt, so each `complete`
//! call must start from an empty KV cache (see [`CandleEngine::model`] for how the reset is done).
//! `candle` tensors/models are not guaranteed `Send` across an `.await`, so — exactly like the
//! llama.cpp engine — generation runs on a dedicated OS thread and streams `CompletionChunk`s back
//! over a `tokio::sync::mpsc` channel bridged into a `BoxStream`.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;

use candle_core::quantized::gguf_file;
use candle_core::{Device, Tensor};
use candle_transformers::generation::{LogitsProcessor, Sampling};
use candle_transformers::models::quantized_gemma3::ModelWeights;
use candle_transformers::utils::apply_repeat_penalty;
use tokenizers::Tokenizer;

use super::logprobs::{token_info, TOP_LOGPROBS};
use super::template::ChatTemplate;
use super::{
    ChatMessage, CompletionChunk, Engine, EngineError, ModelProps, Result, SamplingConfig,
    TokenizeResult,
};

/// GGUF metadata key that holds the model's raw Jinja chat template.
const CHAT_TEMPLATE_KEY: &str = "tokenizer.chat_template";
/// Gemma's two end-of-generation markers; either one terminates a reply.
const EOS_TOKENS: [&str; 2] = ["<end_of_turn>", "<eos>"];
/// How many recent tokens the repeat penalty looks back over (llama.cpp's default).
const PENALTY_LAST_N: usize = 64;
/// Bound on the in-flight `CompletionChunk` channel between the worker thread and the stream.
const CHANNEL_CAPACITY: usize = 64;

/// `Engine` that runs a quantized Gemma 3 GGUF in-process via HuggingFace `candle`.
///
/// `model` holds a *pristine* [`ModelWeights`] — one whose per-layer KV caches are still empty and
/// which is never itself stepped. Every `complete` call `clone`s it (the quantized weight tensors
/// are `Arc`-shared, so the clone is cheap: it bumps reference counts and copies only the small
/// per-layer RoPE sin/cos tensors and the `None` caches) and steps the clone on its own thread.
/// That gives each turn a fresh, empty cache without a mutex or a model reload — the correct reset
/// for a stateless engine (turn 2 must never see turn 1's cached K/V).
pub struct CandleEngine {
    model: ModelWeights,
    tokenizer: Arc<Tokenizer>,
    device: Device,
    n_ctx: usize,
    model_name: String,
    /// Raw Jinja chat template from the GGUF, if the model carried one.
    chat_template: Option<String>,
    /// Token ids that stop generation (`<end_of_turn>` / `<eos>`), whichever the vocab defines.
    eos_ids: Vec<u32>,
}

impl CandleEngine {
    /// Opens the GGUF at `model_path`, builds the Gemma 3 model on the best available device
    /// (CUDA when the `candle-cuda` feature is on, CPU otherwise), loads the `tokenizer.json` at
    /// `tokenizer_path`, and stashes the model's chat template and EOS ids. No generation happens
    /// here; the held model stays pristine.
    pub fn load(model_path: &str, tokenizer_path: &str, n_ctx: usize) -> Result<Self> {
        let device = Device::cuda_if_available(0)
            .map_err(|e| EngineError::Protocol(format!("device init failed: {e}")))?;

        let mut file = std::fs::File::open(model_path)
            .map_err(|e| EngineError::Protocol(format!("cannot open model `{model_path}`: {e}")))?;
        let content = gguf_file::Content::read(&mut file)
            .map_err(|e| EngineError::Protocol(format!("gguf read failed: {e}")))?;

        // Read the chat template out of the metadata before `from_gguf` consumes `content`.
        let chat_template = content
            .metadata
            .get(CHAT_TEMPLATE_KEY)
            .and_then(|v| v.to_string().ok())
            .cloned();

        let model = ModelWeights::from_gguf(content, &mut file, &device)
            .map_err(|e| EngineError::Protocol(format!("model build failed: {e}")))?;

        let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(|e| {
            EngineError::Protocol(format!("cannot load tokenizer `{tokenizer_path}`: {e}"))
        })?;

        let eos_ids = EOS_TOKENS
            .iter()
            .filter_map(|t| tokenizer.token_to_id(t))
            .collect();

        Ok(Self {
            model,
            tokenizer: Arc::new(tokenizer),
            device,
            n_ctx,
            model_name: Self::file_stem(model_path),
            chat_template,
            eos_ids,
        })
    }

    /// Model label fallback: the gguf file stem (e.g. `gemma-3-4b-it` from `.../gemma-3-4b-it.gguf`).
    fn file_stem(model_path: &str) -> String {
        Path::new(model_path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| model_path.to_owned())
    }

    /// Maps `SamplingConfig` onto candle's `Sampling` strategy. `temperature <= 0` collapses to
    /// greedy (argmax); otherwise `top_k == 0` means "no top-k cut" (top-p only), and a positive
    /// `top_k` is applied before top-p. Mirrors the llama.cpp engine's greedy-on-zero-temp rule.
    fn sampling_from(cfg: &SamplingConfig) -> Sampling {
        if cfg.temperature <= 0.0 {
            return Sampling::ArgMax;
        }
        let temperature = cfg.temperature as f64;
        let top_p = cfg.top_p as f64;
        if cfg.top_k == 0 {
            Sampling::TopP {
                p: top_p,
                temperature,
            }
        } else {
            Sampling::TopKThenTopP {
                k: cfg.top_k as usize,
                p: top_p,
                temperature,
            }
        }
    }

    /// Resolves the `SamplingConfig` seed to a concrete `u64`. A negative seed (the `-1` "pick a
    /// random seed" sentinel) is replaced with a time-derived value so each run differs; a
    /// non-negative seed is used verbatim for reproducible sampling.
    fn resolve_seed(seed: i64) -> u64 {
        if seed < 0 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
        } else {
            seed as u64
        }
    }

    /// Builds a `(1, len)` U32 token tensor for `model.forward`.
    fn ids_tensor(ids: &[u32], device: &Device) -> Result<Tensor> {
        Tensor::new(ids, device)
            .and_then(|t| t.reshape((1, ids.len())))
            .map_err(|e| EngineError::Protocol(format!("input tensor build failed: {e}")))
    }

    /// Worker-thread entry point: runs generation and funnels any setup/decode error into a single
    /// terminal `Err` on the channel.
    // reason: these decode-loop helpers take the full per-run context (model, tokenizer, device,
    // n_ctx, eos ids, prompt, config, sender); bundling it into a struct would not improve clarity
    // and would diverge from the sibling llama.cpp engine's matching helper shape.
    #[allow(clippy::too_many_arguments)]
    fn run_generation(
        model: &mut ModelWeights,
        tokenizer: &Tokenizer,
        device: &Device,
        n_ctx: usize,
        eos_ids: &[u32],
        prompt: &str,
        cfg: &SamplingConfig,
        tx: &tokio::sync::mpsc::Sender<Result<CompletionChunk>>,
    ) {
        if let Err(e) =
            Self::try_generate(model, tokenizer, device, n_ctx, eos_ids, prompt, cfg, tx)
        {
            let _ = tx.blocking_send(Err(e));
        }
    }

    /// The decode/sample loop. The prompt is forwarded in one pass at `index_pos = 0` (which, on a
    /// freshly `clone`d model, writes the cache from scratch), then tokens are generated one at a
    /// time with `index_pos` advancing past the cache. Each generated token yields one non-stop
    /// chunk, and the loop always finishes with a single `stop: true` chunk — on EOS, the
    /// `max_tokens` cap (`max_tokens < 0` = unbounded), or context exhaustion. A dropped receiver
    /// is a clean early return, not an error.
    // reason: same as `run_generation` — this carries the full per-run decode context by value;
    // a struct wrapper would add indirection without improving clarity, mirroring llama.cpp.
    #[allow(clippy::too_many_arguments)]
    fn try_generate(
        model: &mut ModelWeights,
        tokenizer: &Tokenizer,
        device: &Device,
        n_ctx: usize,
        eos_ids: &[u32],
        prompt: &str,
        cfg: &SamplingConfig,
        tx: &tokio::sync::mpsc::Sender<Result<CompletionChunk>>,
    ) -> Result<()> {
        // add_special_tokens = true: let the tokenizer add BOS (the chat template omits it).
        let encoding = tokenizer
            .encode(prompt, true)
            .map_err(|e| EngineError::Protocol(format!("tokenize failed: {e}")))?;
        let mut tokens: Vec<u32> = encoding.get_ids().to_vec();

        // Degenerate inputs: nothing to prompt with, or a zero-token budget. Emit only the terminal
        // stop chunk rather than forwarding an empty sequence.
        if tokens.is_empty() || cfg.max_tokens == 0 {
            let _ = tx.blocking_send(Ok(CompletionChunk {
                text: String::new(),
                tokens: Vec::new(),
                stop: true,
            }));
            return Ok(());
        }

        if tokens.len() > n_ctx {
            return Err(EngineError::Protocol(format!(
                "prompt is {} tokens, which exceeds the context size {n_ctx}",
                tokens.len()
            )));
        }

        let mut processor =
            LogitsProcessor::from_sampling(Self::resolve_seed(cfg.seed), Self::sampling_from(cfg));
        // Incremental streaming detokenizer: `step(id)` buffers partial SentencePiece byte-fallback
        // tokens (`<0xNN>`) until they form a complete char, then returns the reassembled delta. This
        // is the only path that correctly reassembles multibyte chars (emoji, non-Latin scripts) that
        // span several tokens — decoding each token in isolation would yield `U+FFFD`.
        let mut stream = tokenizer.decode_stream(false);

        // Forward the whole prompt; `logits` are the last-position logits, shape (1, vocab).
        let input = Self::ids_tensor(&tokens, device)?;
        let mut logits = model
            .forward(&input, 0)
            .map_err(|e| EngineError::Protocol(format!("prompt forward failed: {e}")))?;
        let mut index_pos = tokens.len();
        let mut generated: i32 = 0;

        loop {
            let logits_1d = logits
                .squeeze(0)
                .map_err(|e| EngineError::Protocol(format!("logits reshape failed: {e}")))?;
            // Raw (pre-penalty) logits reported as logprobs, matching the other engines' shape.
            let raw: Vec<f32> = logits_1d
                .to_vec1()
                .map_err(|e| EngineError::Protocol(format!("logits read failed: {e}")))?;

            // Apply the repeat penalty (over the recent window) to a copy used only for sampling.
            let sample_logits = if cfg.repeat_penalty != 1.0 {
                let start = tokens.len().saturating_sub(PENALTY_LAST_N);
                apply_repeat_penalty(&logits_1d, cfg.repeat_penalty, &tokens[start..])
                    .map_err(|e| EngineError::Protocol(format!("repeat penalty failed: {e}")))?
            } else {
                logits_1d
            };

            let next = processor
                .sample(&sample_logits)
                .map_err(|e| EngineError::Protocol(format!("sampling failed: {e}")))?;

            if eos_ids.contains(&next) {
                break;
            }

            // Label each token with its decoded piece (`▁`→space, `<0x0A>`→newline), matching the
            // readable surface forms the sibling engines report — not the raw vocab symbols.
            let info = token_info(
                &raw,
                next as usize,
                |id| tokenizer.decode(&[id as u32], false).unwrap_or_default(),
                TOP_LOGPROBS,
            );
            // Advance the streaming decoder; it returns the correctly-assembled text delta (or none
            // while it is still buffering a partial byte-fallback char).
            let text = stream
                .step(next)
                .map_err(|e| EngineError::Protocol(format!("stream decode failed: {e}")))?
                .unwrap_or_default();
            tokens.push(next);
            generated += 1;

            if tx
                .blocking_send(Ok(CompletionChunk {
                    text,
                    tokens: vec![info],
                    stop: false,
                }))
                .is_err()
            {
                // Receiver gone: the consumer stopped caring. Drop the model clone and exit quietly.
                return Ok(());
            }

            if cfg.max_tokens >= 0 && generated >= cfg.max_tokens {
                break;
            }
            // Stop before stepping past the context window, so a context-capped reply still ends
            // with the terminal stop chunk instead of erroring out mid-generation.
            if index_pos >= n_ctx {
                break;
            }

            let input = Self::ids_tensor(&[next], device)?;
            logits = model
                .forward(&input, index_pos)
                .map_err(|e| EngineError::Protocol(format!("forward failed: {e}")))?;
            index_pos += 1;
        }

        // Terminal stop chunk. `DecodeStream` emits each char as soon as it completes and keeps no
        // flushable tail (any trailing incomplete byte-fallback fragment is simply never a char), so
        // there is nothing to drain here.
        let _ = tx.blocking_send(Ok(CompletionChunk {
            text: String::new(),
            tokens: Vec::new(),
            stop: true,
        }));
        Ok(())
    }
}

#[async_trait]
impl Engine for CandleEngine {
    async fn props(&self) -> Result<ModelProps> {
        Ok(ModelProps {
            n_ctx: self.n_ctx,
            model_name: self.model_name.clone(),
        })
    }

    async fn tokenize(&self, text: &str) -> Result<TokenizeResult> {
        // No special tokens for the utility endpoint (the caller controls that).
        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| EngineError::Protocol(format!("tokenize failed: {e}")))?;
        let tokens: Vec<i64> = encoding.get_ids().iter().map(|&t| t as i64).collect();
        Ok(TokenizeResult {
            count: tokens.len(),
            tokens,
        })
    }

    async fn detokenize(&self, tokens: &[i64]) -> Result<String> {
        let ids: Vec<u32> = tokens.iter().map(|&t| t as u32).collect();
        self.tokenizer
            .decode(&ids, false)
            .map_err(|e| EngineError::Protocol(format!("detokenize failed: {e}")))
    }

    async fn apply_template(
        &self,
        messages: &[ChatMessage],
        add_generation_prompt: bool,
        enable_thinking: bool,
    ) -> Result<String> {
        let src = self.chat_template.as_ref().ok_or_else(|| {
            EngineError::Protocol(format!("model has no `{CHAT_TEMPLATE_KEY}` metadata"))
        })?;
        let template = ChatTemplate::from_jinja(src)
            .map_err(|e| EngineError::Protocol(format!("chat template compile failed: {e}")))?;
        template
            .render(messages, add_generation_prompt, enable_thinking)
            .map_err(|e| EngineError::Protocol(format!("chat template render failed: {e}")))
    }

    async fn complete(
        &self,
        prompt: &str,
        cfg: &SamplingConfig,
    ) -> Result<BoxStream<'static, Result<CompletionChunk>>> {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<CompletionChunk>>(CHANNEL_CAPACITY);

        // Clone the pristine model: the clone carries empty KV caches, so this turn cannot see a
        // previous turn's cached K/V. The quantized weights are `Arc`-shared, so the clone is cheap.
        let mut model = self.model.clone();
        let tokenizer = self.tokenizer.clone();
        let device = self.device.clone();
        let n_ctx = self.n_ctx;
        let eos_ids = self.eos_ids.clone();
        let prompt = prompt.to_owned();
        let cfg = cfg.clone();

        // candle tensors are not guaranteed `Send` across an `.await`: run the whole decode loop on
        // its own OS thread, which owns its model clone for its entire lifetime.
        std::thread::spawn(move || {
            Self::run_generation(
                &mut model, &tokenizer, &device, n_ctx, &eos_ids, &prompt, &cfg, &tx,
            );
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(stream.boxed())
    }
}

#[cfg(all(test, feature = "candle"))]
mod tests {
    use super::*;
    use crate::engine::types::Role;

    // Streaming-decode correctness (byte-fallback reassembly of multibyte chars across tokens) is
    // exercised by the hive live proof with a non-ASCII prompt: it requires a real model/tokenizer
    // to produce the `<0xNN>` byte-fallback token sequence, which cannot be constructed here without
    // one. The former `PieceDecoder` unit tests fed raw split bytes that the old per-token `decode`
    // path never actually produced, so they proved nothing about real output and were removed.

    #[test]
    fn file_stem_strips_dir_and_extension() {
        assert_eq!(
            CandleEngine::file_stem("/models/gemma-3-4b-it.gguf"),
            "gemma-3-4b-it"
        );
        assert_eq!(CandleEngine::file_stem("bare"), "bare");
    }

    #[test]
    fn sampling_zero_temp_is_greedy() {
        let cfg = SamplingConfig {
            temperature: 0.0,
            ..SamplingConfig::default()
        };
        assert_eq!(CandleEngine::sampling_from(&cfg), Sampling::ArgMax);
    }

    #[test]
    fn sampling_maps_top_k_and_top_p() {
        let cfg = SamplingConfig {
            temperature: 0.7,
            top_p: 0.9,
            top_k: 40,
            ..SamplingConfig::default()
        };
        // candle's `Sampling` carries f64 fields; the mapping widens f32 config values, so compare
        // the numeric fields with a tolerance rather than relying on exact bit patterns.
        match CandleEngine::sampling_from(&cfg) {
            Sampling::TopKThenTopP { k, p, temperature } => {
                assert_eq!(k, 40);
                assert!((p - 0.9).abs() < 1e-6, "p was {p}");
                assert!(
                    (temperature - 0.7).abs() < 1e-6,
                    "temperature was {temperature}"
                );
            }
            other => panic!("expected TopKThenTopP, got {other:?}"),
        }
    }

    #[test]
    fn sampling_zero_top_k_is_top_p_only() {
        let cfg = SamplingConfig {
            temperature: 0.5,
            top_p: 0.8,
            top_k: 0,
            ..SamplingConfig::default()
        };
        match CandleEngine::sampling_from(&cfg) {
            Sampling::TopP { p, temperature } => {
                assert!((p - 0.8).abs() < 1e-6, "p was {p}");
                assert!(
                    (temperature - 0.5).abs() < 1e-6,
                    "temperature was {temperature}"
                );
            }
            other => panic!("expected TopP, got {other:?}"),
        }
    }

    #[test]
    fn resolve_seed_passes_through_non_negative_and_replaces_sentinel() {
        assert_eq!(CandleEngine::resolve_seed(42), 42);
        assert_eq!(CandleEngine::resolve_seed(0), 0);
        // -1 must not be used verbatim (would be a fixed u64::MAX); it is replaced with a
        // time-derived, non-sentinel value.
        assert_ne!(CandleEngine::resolve_seed(-1), (-1_i64) as u64);
    }

    #[test]
    fn apply_template_renders_synthetic_chat_template() {
        const CHAT: &str = "{% for m in messages %}<start_of_turn>{{ m.role }}\n{{ m.content }}<end_of_turn>\n{% endfor %}{% if add_generation_prompt %}<start_of_turn>model\n{% endif %}";
        let template = ChatTemplate::from_jinja(CHAT).unwrap();
        let messages = vec![
            ChatMessage {
                role: Role::User,
                content: "hi".into(),
            },
            ChatMessage {
                role: Role::Assistant,
                content: "hello".into(),
            },
        ];
        let rendered = template.render(&messages, true, false).unwrap();
        assert_eq!(
            rendered,
            "<start_of_turn>user\nhi<end_of_turn>\n<start_of_turn>assistant\nhello<end_of_turn>\n<start_of_turn>model\n"
        );
    }
}
