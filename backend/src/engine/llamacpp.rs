//! In-process llama.cpp engine (behind the `llamacpp` feature).
//!
//! This module is compiled only when the `llamacpp` cargo feature is enabled; it is the single
//! place in the crate that links against `llama-cpp-2` (and, transitively, CUDA). The default
//! build never sees any of this code, which keeps that build toolchain-free.
//!
//! The engine stays stateless: each `complete` call spins up a fresh `LlamaContext` and feeds it
//! the whole prompt, matching the `Engine` contract (the session always re-sends the full prompt).
//! Because `LlamaContext` is `!Send`, generation runs on a dedicated OS thread and streams
//! `CompletionChunk`s back over a `tokio::sync::mpsc` channel.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::vocab::LlamaVocab;

use super::logprobs::{token_info, TOP_LOGPROBS};
use super::template::ChatTemplate;
use super::tool_format::GemmaToolFormat;
use super::{
    ChatMessage, CompletionChunk, Engine, EngineError, ModelProps, Result, SamplingConfig,
    TokenizeResult,
};

/// GGUF metadata key that holds the model's raw Jinja chat template.
const CHAT_TEMPLATE_KEY: &str = "tokenizer.chat_template";
/// How many recent tokens the repeat penalty looks back over (llama.cpp's default).
const PENALTY_LAST_N: i32 = 64;
/// `min_keep` passed to the top-p sampler: never prune below one candidate.
const TOP_P_MIN_KEEP: usize = 1;
/// Bound on the in-flight `CompletionChunk` channel between the worker thread and the stream.
const CHANNEL_CAPACITY: usize = 64;

/// `Engine` that runs llama.cpp in-process via `llama-cpp-2`.
///
/// Holds the backend and the loaded model behind `Arc`s (`LlamaModel` is `Send + Sync`, so it is
/// shared, never reloaded per turn). A fresh context is created inside the worker thread for each
/// `complete` call.
pub struct LocalLlamaEngine {
    // The backend must outlive the model; `LlamaBackend::init` is a once-per-process call and both
    // are shared (cloned into) every generation thread.
    backend: Arc<LlamaBackend>,
    model: Arc<LlamaModel>,
    n_ctx: u32,
    model_name: String,
    /// Whether the model's chat template can render tools; computed once in `load`.
    supports_tools: bool,
}

impl LocalLlamaEngine {
    /// Initializes the llama.cpp backend, loads `model_path`, and keeps both alive for the engine's
    /// lifetime. `requested_n_ctx` is the caller's context-size request: `None` (or `Some(0)`)
    /// defaults to the model's trained context length (`n_ctx_train`, the window its RoPE was
    /// trained for), so each model gets its real maximum rather than a flat guess; an explicit
    /// request is honored but capped at the trained length, since exceeding it only degrades
    /// quality via RoPE extrapolation. The resolved size is reported by `props` and used for every
    /// generation context; `n_gpu_layers < 0` keeps llama.cpp's "offload everything" default.
    pub fn load(
        model_path: &str,
        model_label: Option<&str>,
        requested_n_ctx: Option<u32>,
        n_gpu_layers: i32,
    ) -> Result<Self> {
        let backend = Self::shared_backend()?;

        let mut model_params = LlamaModelParams::default();
        // The default is -1 ("all layers on GPU"); only override when the caller gave a concrete,
        // non-negative count, so -1 keeps the sensible default rather than wrapping to a huge u32.
        if n_gpu_layers >= 0 {
            model_params = model_params.with_n_gpu_layers(n_gpu_layers as u32);
        }

        let model = LlamaModel::load_from_file(&backend, model_path, &model_params)
            .map_err(|e| EngineError::Protocol(format!("model load failed: {e}")))?;

        let n_ctx = Self::resolve_n_ctx(model.n_ctx_train(), requested_n_ctx);

        let model_name = model_label
            .map(str::to_owned)
            .unwrap_or_else(|| Self::file_stem(model_path));

        let supports_tools = model
            .meta_val_str(CHAT_TEMPLATE_KEY)
            .map(|src| GemmaToolFormat::template_supports(&src))
            .unwrap_or(false);

        Ok(Self {
            backend,
            model: Arc::new(model),
            n_ctx,
            model_name,
            supports_tools,
        })
    }

    /// Resolves the context size from the model's trained length and the caller's request.
    ///
    /// `None` or `Some(0)` yields the trained length (floored at 1, so a model that reports 0 still
    /// produces a usable context). An explicit request above the trained length is capped to it
    /// with a stderr warning, since RoPE was only trained to that window; a request at or below it
    /// is used verbatim.
    fn resolve_n_ctx(trained_n_ctx: u32, requested_n_ctx: Option<u32>) -> u32 {
        let trained = trained_n_ctx.max(1);
        match requested_n_ctx {
            None | Some(0) => trained,
            Some(req) if req > trained => {
                eprintln!(
                    "requested n_ctx {req} exceeds the model's trained context length {trained}; using {trained}"
                );
                trained
            }
            Some(req) => req,
        }
    }

    /// Returns the process-global `LlamaBackend`, initializing it on first use.
    ///
    /// `LlamaBackend::init` must run exactly once per process; guarding it behind a `OnceLock`
    /// makes a second `load` (or any future second construction) reuse the first backend instead
    /// of re-initializing. If two threads race the first init, the first to publish wins and the
    /// loser's (equivalent) backend is dropped.
    fn shared_backend() -> Result<Arc<LlamaBackend>> {
        static BACKEND: std::sync::OnceLock<Arc<LlamaBackend>> = std::sync::OnceLock::new();
        if let Some(backend) = BACKEND.get() {
            return Ok(backend.clone());
        }
        let backend = Arc::new(
            LlamaBackend::init()
                .map_err(|e| EngineError::Protocol(format!("llama backend init failed: {e}")))?,
        );
        Ok(BACKEND.get_or_init(|| backend).clone())
    }

    /// Model label fallback: the gguf file stem (e.g. `gemma-3-12b-it` from `.../gemma-3-12b-it.gguf`).
    fn file_stem(model_path: &str) -> String {
        Path::new(model_path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| model_path.to_owned())
    }

    /// Renders one vocabulary id to its piece text (lossy UTF-8, matching how the HTTP engine
    /// surfaces per-token strings).
    fn piece_string(vocab: &LlamaVocab<'_>, token: LlamaToken) -> String {
        let bytes = vocab.token_to_piece(token, true, None);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Builds the sampler chain from `cfg`. Order mirrors llama.cpp: penalties, then truncation
    /// (top-k, top-p), then temperature, then the final distribution draw. `temperature <= 0`
    /// collapses to greedy (argmax) selection. Seed `-1` wraps to `u32::MAX`, which llama.cpp reads
    /// as its "pick a random seed" sentinel.
    fn build_sampler(cfg: &SamplingConfig, n_vocab: i32) -> LlamaSampler {
        let mut samplers = vec![LlamaSampler::penalties(
            n_vocab,
            PENALTY_LAST_N,
            cfg.repeat_penalty,
            0.0,
            0.0,
        )];
        if cfg.temperature <= 0.0 {
            samplers.push(LlamaSampler::greedy());
        } else {
            samplers.push(LlamaSampler::top_k(cfg.top_k as i32));
            samplers.push(LlamaSampler::top_p(cfg.top_p, TOP_P_MIN_KEEP));
            samplers.push(LlamaSampler::temp(cfg.temperature));
            samplers.push(LlamaSampler::dist(cfg.seed as u32));
        }
        LlamaSampler::chain_simple(samplers)
    }

    /// Worker-thread entry point: runs generation and funnels any setup error into a single
    /// terminal `Err` on the channel.
    fn run_generation(
        backend: &LlamaBackend,
        model: &LlamaModel,
        n_ctx: u32,
        prompt: &str,
        cfg: &SamplingConfig,
        tx: &tokio::sync::mpsc::Sender<Result<CompletionChunk>>,
    ) {
        if let Err(e) = Self::try_generate(backend, model, n_ctx, prompt, cfg, tx) {
            let _ = tx.blocking_send(Err(e));
        }
    }

    /// The decode/sample loop. Sends one non-stop chunk per generated token and always finishes
    /// with a single `stop: true` chunk (on EOS, the `max_tokens` cap, or context exhaustion). A
    /// dropped receiver is a clean early return, not an error.
    fn try_generate(
        backend: &LlamaBackend,
        model: &LlamaModel,
        n_ctx: u32,
        prompt: &str,
        cfg: &SamplingConfig,
        tx: &tokio::sync::mpsc::Sender<Result<CompletionChunk>>,
    ) -> Result<()> {
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(n_ctx))
            // Decode the whole prompt in one batch: keep the batch budget at the context size.
            .with_n_batch(n_ctx.max(1));
        let mut ctx = model
            .new_context(backend, ctx_params)
            .map_err(|e| EngineError::Protocol(format!("context creation failed: {e}")))?;

        let vocab = model.vocab();
        // add_special=true: llama.cpp adds BOS here (the chat template deliberately omits it);
        // parse_special=true: recognise the template's control tokens (e.g. `<start_of_turn>`).
        let tokens = vocab.tokenize(prompt.as_bytes(), true, true);

        // Degenerate inputs: nothing to prompt with, or a zero-token budget. Emit only the
        // terminal stop chunk rather than decoding an empty batch (which llama.cpp rejects).
        if tokens.is_empty() || cfg.max_tokens == 0 {
            let _ = tx.blocking_send(Ok(CompletionChunk {
                text: String::new(),
                tokens: Vec::new(),
                stop: true,
            }));
            return Ok(());
        }

        if tokens.len() > n_ctx as usize {
            return Err(EngineError::Protocol(format!(
                "prompt is {} tokens, which exceeds the context size {n_ctx}",
                tokens.len()
            )));
        }

        let mut batch = LlamaBatch::new(tokens.len(), 1);
        let last = tokens.len() - 1;
        for (i, &tok) in tokens.iter().enumerate() {
            batch
                .add(tok, i as i32, &[0], i == last)
                .map_err(|e| EngineError::Protocol(format!("batch add failed: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| EngineError::Protocol(format!("prompt decode failed: {e}")))?;

        let n_vocab = vocab.n_tokens();
        let mut sampler = Self::build_sampler(cfg, n_vocab);
        let mut n_cur = batch.n_tokens();
        let mut generated: i32 = 0;
        let max = cfg.max_tokens;
        let mut decoder = PieceDecoder::new();

        loop {
            let idx = batch.n_tokens() - 1;
            // The reported logprobs are the raw model logits for this position (via
            // `get_logits_ith`), matching the HTTP engine's shape. They are NOT the distribution
            // we actually sampled from: the sampler applies penalties/top-k/top-p/temperature to
            // this same index before drawing, so the index alignment is correct but the reported
            // distribution is the pre-sampling (raw) one, not the post-penalty sampling one.
            let logits = ctx.get_logits_ith(idx);
            let token = sampler.sample(&ctx, idx);
            sampler.accept(token);

            if vocab.is_eog(token) {
                break;
            }

            let info = token_info(
                logits,
                token.0 as usize,
                |id| Self::piece_string(&vocab, LlamaToken::new(id as i32)),
                TOP_LOGPROBS,
            );
            let piece = vocab.token_to_piece(token, true, None);
            let text = decoder.push(&piece);
            generated += 1;

            if tx
                .blocking_send(Ok(CompletionChunk {
                    text,
                    tokens: vec![info],
                    stop: false,
                }))
                .is_err()
            {
                // Receiver gone: the consumer stopped caring. Drop the context and exit quietly.
                return Ok(());
            }

            if max >= 0 && generated >= max {
                break;
            }

            // Context window is full: adding the next token at position `n_cur` and decoding it
            // would push past `n_ctx` and make `ctx.decode` fail. Stop cleanly here so a reply
            // capped by the context window ends with the terminal stop chunk, like the EOS and
            // `max_tokens` cases, instead of erroring out and dropping the partial reply.
            if n_cur >= n_ctx as i32 {
                break;
            }

            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|e| EngineError::Protocol(format!("batch add failed: {e}")))?;
            n_cur += 1;
            ctx.decode(&mut batch)
                .map_err(|e| EngineError::Protocol(format!("decode failed: {e}")))?;
        }

        // Flush any bytes left mid-character, then the terminal stop chunk.
        let tail = decoder.finish();
        let _ = tx.blocking_send(Ok(CompletionChunk {
            text: tail,
            tokens: Vec::new(),
            stop: true,
        }));
        Ok(())
    }
}

#[async_trait]
impl Engine for LocalLlamaEngine {
    async fn props(&self) -> Result<ModelProps> {
        Ok(ModelProps {
            n_ctx: self.n_ctx as usize,
            model_name: self.model_name.clone(),
        })
    }

    async fn tokenize(&self, text: &str) -> Result<TokenizeResult> {
        let vocab = self.model.vocab();
        // No BOS for the utility endpoint (the caller controls that); still parse control tokens.
        let toks = vocab.tokenize(text.as_bytes(), false, true);
        let tokens: Vec<i64> = toks.iter().map(|t| t.0 as i64).collect();
        Ok(TokenizeResult {
            count: tokens.len(),
            tokens,
        })
    }

    async fn detokenize(&self, tokens: &[i64]) -> Result<String> {
        let vocab = self.model.vocab();
        let toks: Vec<LlamaToken> = tokens.iter().map(|&t| LlamaToken::new(t as i32)).collect();
        let bytes = vocab.detokenize(&toks, false, true);
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn supports_tools(&self) -> bool {
        self.supports_tools
    }

    async fn apply_template(
        &self,
        messages: &[ChatMessage],
        add_generation_prompt: bool,
        enable_thinking: bool,
        tools: &[serde_json::Value],
    ) -> Result<String> {
        // Primary path: read the model's raw Jinja template from metadata and render it with the
        // crate-local `ChatTemplate`. This preserves the `enable_thinking` variable, which the
        // crate's native `apply_chat_template` cannot express.
        let src = self.model.meta_val_str(CHAT_TEMPLATE_KEY).map_err(|e| {
            EngineError::Protocol(format!("model has no `{CHAT_TEMPLATE_KEY}` metadata: {e}"))
        })?;
        let template = ChatTemplate::from_jinja(&src)
            .map_err(|e| EngineError::Protocol(format!("chat template compile failed: {e}")))?;
        template
            .render(messages, add_generation_prompt, enable_thinking, tools)
            .map_err(|e| EngineError::Protocol(format!("chat template render failed: {e}")))
    }

    async fn complete(
        &self,
        prompt: &str,
        cfg: &SamplingConfig,
    ) -> Result<BoxStream<'static, Result<CompletionChunk>>> {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<CompletionChunk>>(CHANNEL_CAPACITY);
        let backend = self.backend.clone();
        let model = self.model.clone();
        let n_ctx = self.n_ctx;
        let prompt = prompt.to_owned();
        let cfg = cfg.clone();

        // `LlamaContext` is `!Send`: run the whole decode loop on its own OS thread and stream the
        // results back. The thread owns its context for its entire lifetime.
        std::thread::spawn(move || {
            Self::run_generation(&backend, &model, n_ctx, &prompt, &cfg, &tx);
        });

        // Bridge the mpsc receiver to a stream using only `futures` (no extra dependency).
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(stream.boxed())
    }
}

/// Reassembles streamed token pieces into valid UTF-8. A single token's bytes can split a
/// multi-byte character, so bytes are buffered and only the valid prefix is emitted; the remainder
/// waits for the next token (or is flushed lossily at the end).
struct PieceDecoder {
    pending: Vec<u8>,
}

impl PieceDecoder {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Appends `bytes` and returns the longest now-complete UTF-8 prefix.
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        match std::str::from_utf8(&self.pending) {
            Ok(_) => {
                let taken = std::mem::take(&mut self.pending);
                // Just validated as UTF-8 above.
                String::from_utf8(taken).unwrap_or_default()
            }
            Err(e) => {
                let valid = e.valid_up_to();
                let out = String::from_utf8_lossy(&self.pending[..valid]).into_owned();
                self.pending.drain(..valid);
                out
            }
        }
    }

    /// Flushes whatever remains, replacing any dangling bytes with the replacement character.
    fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piece_decoder_passes_through_ascii() {
        let mut d = PieceDecoder::new();
        assert_eq!(d.push(b"hello"), "hello");
        assert_eq!(d.finish(), "");
    }

    #[test]
    fn piece_decoder_reassembles_split_multibyte() {
        let mut d = PieceDecoder::new();
        // "é" is 0xC3 0xA9 split across two pieces.
        assert_eq!(d.push(&[0xC3]), "");
        assert_eq!(d.push(&[0xA9]), "é");
        assert_eq!(d.finish(), "");
    }

    #[test]
    fn piece_decoder_flushes_dangling_bytes_lossily() {
        let mut d = PieceDecoder::new();
        assert_eq!(d.push(&[0xC3]), "");
        assert_eq!(d.finish(), "\u{FFFD}");
    }

    #[test]
    fn file_stem_strips_dir_and_extension() {
        assert_eq!(
            LocalLlamaEngine::file_stem("/models/gemma-3-12b-it.gguf"),
            "gemma-3-12b-it"
        );
        assert_eq!(LocalLlamaEngine::file_stem("bare"), "bare");
    }

    #[test]
    fn resolve_n_ctx_defaults_to_trained_when_unset() {
        assert_eq!(LocalLlamaEngine::resolve_n_ctx(8192, None), 8192);
        assert_eq!(LocalLlamaEngine::resolve_n_ctx(8192, Some(0)), 8192);
    }

    #[test]
    fn resolve_n_ctx_honors_request_within_trained() {
        assert_eq!(LocalLlamaEngine::resolve_n_ctx(8192, Some(2048)), 2048);
        assert_eq!(LocalLlamaEngine::resolve_n_ctx(8192, Some(8192)), 8192);
    }

    #[test]
    fn resolve_n_ctx_caps_request_at_trained() {
        assert_eq!(LocalLlamaEngine::resolve_n_ctx(4096, Some(99999)), 4096);
    }

    #[test]
    fn resolve_n_ctx_floors_trained_at_one() {
        assert_eq!(LocalLlamaEngine::resolve_n_ctx(0, None), 1);
    }
}
