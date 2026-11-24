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

/// Allows every call: the executor already refuses tools the agent has not enabled, so the
/// template's tool list is the only gate.
pub struct EnabledPolicy;

impl ToolPolicy for EnabledPolicy {
    fn decide(&self, _spec: &ToolSpec, _args: &Value) -> Decision {
        Decision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn enabled_policy_allows_side_effects() {
        for side_effects in [false, true] {
            let spec = ToolSpec {
                name: "t".into(),
                description: String::new(),
                parameters: json!({}),
                side_effects,
            };
            assert_eq!(EnabledPolicy.decide(&spec, &json!({})), Decision::Allow);
        }
    }
}
