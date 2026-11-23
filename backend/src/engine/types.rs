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
    Tool,
}

/// A function invocation requested by the model (OpenAI-style shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<serde_json::Value>,
}

impl ChatMessage {
    /// A plain text message with no tool data.
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            tool_result: None,
        }
    }

    /// The JSON shape handed to chat templates: `{role, content, tool_calls?, tool_call_id?}`.
    /// For `Role::Tool` messages `content` carries the structured `tool_result` (the template
    /// formats it), falling back to the plain text when no result is set.
    pub fn template_view(&self) -> serde_json::Value {
        let content = match (&self.role, &self.tool_result) {
            (Role::Tool, Some(result)) => result.clone(),
            _ => serde_json::Value::String(self.content.clone()),
        };
        let mut view = serde_json::json!({ "role": self.role, "content": content });
        if let Some(calls) = &self.tool_calls {
            view["tool_calls"] = serde_json::json!(calls);
        }
        if let Some(id) = &self.tool_call_id {
            view["tool_call_id"] = serde_json::json!(id);
        }
        view
    }
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
/// `{"default_generation_settings":{"n_ctx":N},"model_path":"..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RawProps", into = "RawProps")]
pub struct ModelProps {
    pub n_ctx: usize,
    pub model_name: String,
}

#[derive(Serialize, Deserialize)]
struct RawProps {
    default_generation_settings: RawGenerationSettings,
    model_path: String,
}

#[derive(Serialize, Deserialize)]
struct RawGenerationSettings {
    n_ctx: usize,
}

impl From<RawProps> for ModelProps {
    fn from(raw: RawProps) -> Self {
        Self {
            n_ctx: raw.default_generation_settings.n_ctx,
            model_name: raw.model_path,
        }
    }
}

impl From<ModelProps> for RawProps {
    fn from(p: ModelProps) -> Self {
        Self {
            default_generation_settings: RawGenerationSettings { n_ctx: p.n_ctx },
            model_path: p.model_name,
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

        let raw = r#"{"default_generation_settings":{"n_ctx":4096}, "model_path":"C:\\models\\gemma-4-12B-it-QAT-Q4_0.gguf","model_alias":"gemma","model_ftype":"Q4_0"}"#;
        let p: ModelProps = serde_json::from_str(raw).unwrap();
        assert_eq!(p.n_ctx, 4096);
        assert_eq!(p.model_name, r"C:\models\gemma-4-12B-it-QAT-Q4_0.gguf");
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
    fn chat_message_serde_omits_absent_tool_fields_and_round_trips() {
        let plain = ChatMessage::new(Role::User, "hi");
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"{"role":"user","content":"hi"}"#
        );
        let back: ChatMessage = serde_json::from_str(r#"{"role":"tool","content":""}"#).unwrap();
        assert_eq!(back.role, Role::Tool);

        let mut call = ChatMessage::new(Role::Assistant, "");
        call.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "f".into(),
                arguments: serde_json::json!({"a": 1}),
            },
        }]);
        let json = serde_json::to_string(&call).unwrap();
        assert!(json.contains(r#""type":"function""#));
        assert_eq!(serde_json::from_str::<ChatMessage>(&json).unwrap(), call);
    }

    #[test]
    fn template_view_uses_tool_result_as_content_for_tool_role() {
        let mut tool = ChatMessage::new(Role::Tool, "ignored");
        tool.tool_call_id = Some("c1".into());
        tool.tool_result = Some(serde_json::json!({"x": 1}));
        let v = tool.template_view();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["content"], serde_json::json!({"x": 1}));
        assert_eq!(v["tool_call_id"], "c1");
        let user = ChatMessage::new(Role::User, "hi").template_view();
        assert_eq!(user, serde_json::json!({"role":"user","content":"hi"}));
    }

    #[test]
    fn props_missing_n_ctx_fails_to_deserialize() {
        let raw = r#"{"default_generation_settings":{}, "model_path":"m.gguf"}"#;
        assert!(serde_json::from_str::<ModelProps>(raw).is_err());
        let raw = r#"{"model_path":"m.gguf"}"#;
        assert!(serde_json::from_str::<ModelProps>(raw).is_err());
    }
}
