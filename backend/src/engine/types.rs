use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SamplingConfig {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub repeat_penalty: f32,
    pub seed: i64,
    pub max_tokens: i32,
}

impl Default for SamplingConfig {
    fn default() -> Self {
        Self {
            temperature: 0.8,
            top_p: 0.95,
            top_k: 40,
            repeat_penalty: 1.1,
            seed: -1,
            max_tokens: -1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenInfo {
    pub token: String,
    pub logprob: f32,
    pub top: Vec<(String, f32)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionChunk {
    pub text: String,
    pub tokens: Vec<TokenInfo>,
    pub stop: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenizeResult {
    pub tokens: Vec<i64>,
    pub count: usize,
}

/// Model properties, (de)serialized in llama.cpp's `/props` shape:
/// `{"default_generation_settings":{"n_ctx":N},"model":"..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RawProps", into = "RawProps")]
pub struct ModelProps {
    pub n_ctx: usize,
    pub model_name: String,
}

#[derive(Serialize, Deserialize)]
struct RawProps {
    default_generation_settings: RawGenerationSettings,
    model: String,
}

#[derive(Serialize, Deserialize)]
struct RawGenerationSettings {
    n_ctx: usize,
}

impl From<RawProps> for ModelProps {
    fn from(raw: RawProps) -> Self {
        Self {
            n_ctx: raw.default_generation_settings.n_ctx,
            model_name: raw.model,
        }
    }
}

impl From<ModelProps> for RawProps {
    fn from(p: ModelProps) -> Self {
        Self {
            default_generation_settings: RawGenerationSettings { n_ctx: p.n_ctx },
            model: p.model_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trips_sampling_config_and_props() {
        let cfg = SamplingConfig {
            temperature: 0.5,
            top_p: 0.9,
            top_k: 40,
            repeat_penalty: 1.1,
            seed: -1,
            max_tokens: 256,
        };
        let back: SamplingConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(cfg, back);

        let props = ModelProps {
            n_ctx: 8192,
            model_name: "m.gguf".into(),
        };
        let back: ModelProps =
            serde_json::from_str(&serde_json::to_string(&props).unwrap()).unwrap();
        assert_eq!(props, back);

        let raw = r#"{"default_generation_settings":{"n_ctx":4096}, "model":"llama.gguf"}"#;
        let p: ModelProps = serde_json::from_str(raw).unwrap();
        assert_eq!(p.n_ctx, 4096);
        assert_eq!(p.model_name, "llama.gguf");
    }

    #[test]
    fn sampling_config_defaults_and_partial_json() {
        let d = SamplingConfig::default();
        assert_eq!(d.temperature, 0.8);
        assert_eq!(d.top_p, 0.95);
        assert_eq!(d.top_k, 40);
        assert_eq!(d.repeat_penalty, 1.1);
        assert_eq!(d.seed, -1);
        assert_eq!(d.max_tokens, -1);

        let partial: SamplingConfig = serde_json::from_str(r#"{"temperature":0.2}"#).unwrap();
        assert_eq!(partial.temperature, 0.2);
        assert_eq!(
            partial,
            SamplingConfig {
                temperature: 0.2,
                ..SamplingConfig::default()
            }
        );
    }

    #[test]
    fn props_missing_n_ctx_fails_to_deserialize() {
        let raw = r#"{"default_generation_settings":{}, "model":"m.gguf"}"#;
        assert!(serde_json::from_str::<ModelProps>(raw).is_err());
        let raw = r#"{"model":"m.gguf"}"#;
        assert!(serde_json::from_str::<ModelProps>(raw).is_err());
    }
}
