use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::policy::{Decision, ToolPolicy};
use super::registry::ToolRegistry;
use super::tool::{ToolCallRequest, ToolContext, ToolOutcome};

/// Minimal JSON-schema subset check: object args, `required`, and primitive `type`s.
pub struct SchemaCheck;

impl SchemaCheck {
    pub fn validate(schema: &Value, args: &Value) -> Result<(), String> {
        let obj = args
            .as_object()
            .ok_or_else(|| "arguments must be an object".to_string())?;
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !obj.contains_key(key) {
                    return Err(format!("missing required argument: {key}"));
                }
            }
        }
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (key, value) in obj {
                let expected = props
                    .get(key)
                    .and_then(|p| p.get("type"))
                    .and_then(Value::as_str);
                if let Some(ty) = expected {
                    if !Self::type_matches(ty, value) {
                        return Err(format!("argument {key} must be of type {ty}"));
                    }
                }
            }
        }
        Ok(())
    }

    fn type_matches(ty: &str, value: &Value) -> bool {
        match ty {
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            // Unknown or unsupported schema types (e.g. "null") are not enforced.
            _ => true,
        }
    }
}

/// Runs a tool call through lookup, policy, validation and a timeout.
pub struct ToolExecutor {
    registry: Arc<ToolRegistry>,
    policy: Box<dyn ToolPolicy>,
    timeout: Duration,
}

impl ToolExecutor {
    pub fn new(
        registry: Arc<ToolRegistry>,
        policy: Box<dyn ToolPolicy>,
        timeout: Duration,
    ) -> Self {
        Self {
            registry,
            policy,
            timeout,
        }
    }

    pub async fn execute(&self, call: &ToolCallRequest) -> ToolOutcome {
        if let Some(e) = &call.parse_error {
            return ToolOutcome::Err(format!("could not parse tool call: {e}"));
        }
        let Some(tool) = self.registry.get(&call.name) else {
            return ToolOutcome::Err(format!("unknown tool: {}", call.name));
        };
        let spec = tool.spec();
        match self.policy.decide(&spec, &call.arguments) {
            Decision::Allow => {}
            Decision::Deny(reason) => return ToolOutcome::Err(reason),
            Decision::Ask => {
                return ToolOutcome::Err("approval required (not supported yet)".to_string())
            }
        }
        if let Err(e) = SchemaCheck::validate(&spec.parameters, &call.arguments) {
            return ToolOutcome::Err(format!("invalid arguments: {e}"));
        }
        let ctx = ToolContext {
            call_id: call.id.clone(),
        };
        let call_args = call.arguments.clone();
        let mut handle = tokio::spawn(async move { tool.call(call_args, &ctx).await });
        match tokio::time::timeout(self.timeout, &mut handle).await {
            Ok(Ok(outcome)) => outcome,
            // A panicking tool is reported as data, not unwound into the caller.
            Ok(Err(e)) => ToolOutcome::Err(format!("tool panicked: {e}")),
            Err(_) => {
                // Aborting only cancels the async task. Work a tool handed to
                // `spawn_blocking` (the file tools do) cannot be interrupted: it runs to
                // completion on the blocking pool and its result is dropped. Tools that must be
                // stoppable have to check for cancellation themselves.
                handle.abort();
                ToolOutcome::Err(Self::timeout_message(self.timeout))
            }
        }
    }

    pub fn timeout_message(timeout: Duration) -> String {
        format!("tool timed out after {}s", timeout.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::policy::ReadOnlyPolicy;
    use crate::runtime::test_tools::TestSource;
    use crate::runtime::tool::ToolSource;
    use crate::runtime::tool::ToolSpec;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct AskPolicy;

    impl ToolPolicy for AskPolicy {
        fn decide(&self, _spec: &ToolSpec, _args: &Value) -> Decision {
            Decision::Ask
        }
    }

    struct Fixture;

    impl Fixture {
        fn executor(source: Box<dyn ToolSource>, timeout: Duration) -> ToolExecutor {
            let reg = ToolRegistry::new(vec![source]).unwrap();
            ToolExecutor::new(Arc::new(reg), Box::new(ReadOnlyPolicy), timeout)
        }

        fn call(name: &str, arguments: Value) -> ToolCallRequest {
            ToolCallRequest {
                id: "c1".into(),
                name: name.into(),
                arguments,
                parse_error: None,
            }
        }
    }

    #[test]
    fn schema_requires_required_keys() {
        let err = SchemaCheck::validate(&json!({"required":["text"]}), &json!({})).unwrap_err();
        assert!(err.contains("text"));
    }

    #[test]
    fn schema_checks_primitive_types() {
        let schema = json!({"properties":{"n":{"type":"integer"}}});
        assert!(SchemaCheck::validate(&schema, &json!({"n":"1"})).is_err());
        assert!(SchemaCheck::validate(&schema, &json!({"n":1})).is_ok());
        assert!(SchemaCheck::validate(&schema, &json!("x")).is_err());
    }

    #[tokio::test]
    async fn executor_reports_unknown_tool() {
        let ex = Fixture::executor(TestSource::echo_boxed(), Duration::from_secs(1));
        let out = ex.execute(&Fixture::call("nope", json!({}))).await;
        assert_eq!(out, ToolOutcome::Err("unknown tool: nope".into()));
    }

    #[tokio::test]
    async fn executor_reports_parse_error() {
        let ex = Fixture::executor(TestSource::echo_boxed(), Duration::from_secs(1));
        let mut call = Fixture::call("echo", json!({}));
        call.parse_error = Some("bad".into());
        assert_eq!(
            ex.execute(&call).await,
            ToolOutcome::Err("could not parse tool call: bad".into())
        );
    }

    #[tokio::test]
    async fn executor_denies_side_effects_under_read_only() {
        let ex = Fixture::executor(TestSource::writer_boxed(), Duration::from_secs(1));
        let out = ex.execute(&Fixture::call("writer", json!({}))).await;
        assert_eq!(
            out,
            ToolOutcome::Err("tool has side effects; not allowed by the read-only policy".into())
        );
    }

    #[tokio::test]
    async fn executor_times_out() {
        let ex = Fixture::executor(TestSource::slow_boxed(), Duration::from_millis(50));
        let out = ex.execute(&Fixture::call("slow", json!({}))).await;
        assert_eq!(out, ToolOutcome::Err("tool timed out after 0.05s".into()));
    }

    #[test]
    fn timeout_message_renders_seconds() {
        assert_eq!(
            ToolExecutor::timeout_message(Duration::from_secs(10)),
            "tool timed out after 10s"
        );
    }

    #[tokio::test]
    async fn executor_reports_ask_as_unsupported() {
        let reg = ToolRegistry::new(vec![TestSource::echo_boxed()]).unwrap();
        let ex = ToolExecutor::new(Arc::new(reg), Box::new(AskPolicy), Duration::from_secs(1));
        let out = ex
            .execute(&Fixture::call("echo", json!({"text":"x"})))
            .await;
        assert_eq!(
            out,
            ToolOutcome::Err("approval required (not supported yet)".into())
        );
    }

    #[tokio::test]
    async fn executor_reports_panicking_tool_as_error() {
        let ex = Fixture::executor(TestSource::panic_boxed(), Duration::from_secs(1));
        let out = ex.execute(&Fixture::call("panic", json!({}))).await;
        match out {
            ToolOutcome::Err(m) => assert!(m.starts_with("tool panicked")),
            _ => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn executor_aborts_timed_out_tool() {
        let flag = Arc::new(AtomicBool::new(false));
        let ex = Fixture::executor(
            TestSource::flag_boxed(flag.clone()),
            Duration::from_millis(50),
        );
        let out = ex.execute(&Fixture::call("flag", json!({}))).await;
        assert!(!out.is_ok());
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn schema_integer_rejects_fraction_and_accepts_large_u64() {
        let schema = json!({"properties":{"n":{"type":"integer"}}});
        assert!(SchemaCheck::validate(&schema, &json!({"n":1.5})).is_err());
        assert!(SchemaCheck::validate(&schema, &json!({"n":u64::MAX})).is_ok());
    }

    #[tokio::test]
    async fn executor_runs_valid_call() {
        let ex = Fixture::executor(TestSource::echo_boxed(), Duration::from_secs(1));
        let out = ex
            .execute(&Fixture::call("echo", json!({"text":"x"})))
            .await;
        assert_eq!(out, ToolOutcome::Ok(json!({"text":"x"})));
    }

    #[tokio::test]
    async fn executor_rejects_invalid_arguments() {
        let ex = Fixture::executor(TestSource::echo_boxed(), Duration::from_secs(1));
        let out = ex.execute(&Fixture::call("echo", json!({}))).await;
        match out {
            ToolOutcome::Err(m) => assert!(m.starts_with("invalid arguments: ")),
            _ => panic!("expected error"),
        }
    }
}
