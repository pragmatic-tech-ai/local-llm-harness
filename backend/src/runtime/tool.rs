use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};

/// Describes a tool to the model and to the policy.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub side_effects: bool,
}

/// Per-call context handed to a tool.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub call_id: String,
}

/// Result of running a tool.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    Ok(Value),
    Err(String),
}

impl ToolOutcome {
    /// The JSON value fed back to the model: the result, or `{"error": msg}`.
    pub fn result_value(&self) -> Value {
        match self {
            ToolOutcome::Ok(v) => v.clone(),
            ToolOutcome::Err(msg) => json!({ "error": msg }),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, ToolOutcome::Ok(_))
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutcome;
}

/// A provider of tools (built-ins, MCP servers, ...).
pub trait ToolSource: Send + Sync {
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
}

/// A tool call as emitted by the model.
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    pub parse_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_result_value_wraps_errors() {
        assert_eq!(
            ToolOutcome::Err("boom".into()).result_value(),
            json!({"error": "boom"})
        );
        assert_eq!(ToolOutcome::Ok(json!(1)).result_value(), json!(1));
        assert!(ToolOutcome::Ok(json!(1)).is_ok());
    }
}
