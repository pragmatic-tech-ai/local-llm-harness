use serde_json::Value;

use super::tool::ToolSpec;

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    Deny(String),
    Ask,
}

pub trait ToolPolicy: Send + Sync {
    fn decide(&self, spec: &ToolSpec, args: &Value) -> Decision;
}

/// Allows only tools that declare no side effects.
pub struct ReadOnlyPolicy;

impl ReadOnlyPolicy {
    const DENY_REASON: &'static str = "tool has side effects; not allowed by the read-only policy";
}

impl ToolPolicy for ReadOnlyPolicy {
    fn decide(&self, spec: &ToolSpec, _args: &Value) -> Decision {
        if spec.side_effects {
            Decision::Deny(Self::DENY_REASON.to_string())
        } else {
            Decision::Allow
        }
    }
}
