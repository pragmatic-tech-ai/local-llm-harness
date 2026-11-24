use std::fmt;
use std::path::PathBuf;
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

/// A tool's view of the session it runs for; implemented above the runtime.
pub trait SessionHandle: Send + Sync {
    /// The session's notes; None when the session no longer exists.
    fn notes(&self) -> Option<Vec<String>>;
    /// Replaces the notes; false when the session no longer exists.
    fn set_notes(&self, notes: Vec<String>) -> bool;
    fn exists(&self) -> bool;
}

/// Where a call's file work is confined, and which session it belongs to.
#[derive(Clone, Default)]
pub struct ToolScope {
    /// The write folder, relative to the sandbox; None means the sandbox root.
    pub workspace: Option<PathBuf>,
    pub session: Option<Arc<dyn SessionHandle>>,
}

impl ToolScope {
    /// True when the call belongs to a session that has since been deleted.
    pub fn session_deleted(&self) -> bool {
        self.session.as_ref().is_some_and(|s| !s.exists())
    }
}

impl fmt::Debug for ToolScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolScope")
            .field("workspace", &self.workspace)
            .field("session", &self.session.is_some())
            .finish()
    }
}

impl PartialEq for ToolScope {
    /// Sessions compare by identity: the same handle, or both absent.
    fn eq(&self, other: &Self) -> bool {
        let same_session = match (&self.session, &other.session) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        self.workspace == other.workspace && same_session
    }
}

/// Per-call context handed to a tool.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub call_id: String,
    pub scope: ToolScope,
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
    use crate::runtime::test_support::FakeSession;

    #[test]
    fn outcome_result_value_wraps_errors() {
        assert_eq!(
            ToolOutcome::Err("boom".into()).result_value(),
            json!({"error": "boom"})
        );
        assert_eq!(ToolOutcome::Ok(json!(1)).result_value(), json!(1));
        assert!(ToolOutcome::Ok(json!(1)).is_ok());
    }

    #[test]
    fn scope_compares_sessions_by_identity() {
        let a: Arc<dyn SessionHandle> = FakeSession::alive();
        let b: Arc<dyn SessionHandle> = FakeSession::alive();
        let with = |s: &Arc<dyn SessionHandle>| ToolScope {
            workspace: Some(PathBuf::from("shared")),
            session: Some(s.clone()),
        };
        assert_eq!(with(&a), with(&a));
        assert_ne!(with(&a), with(&b));
        assert_eq!(ToolScope::default(), ToolScope::default());
        assert_ne!(with(&a), ToolScope::default());
        let text = format!("{:?}", with(&a));
        assert!(
            text.contains("shared") && text.contains("session: true"),
            "{text}"
        );
    }

    #[test]
    fn scope_reports_deleted_session() {
        assert!(!ToolScope::default().session_deleted());
        let gone = ToolScope {
            workspace: None,
            session: Some(FakeSession::deleted()),
        };
        assert!(gone.session_deleted());
        let live = ToolScope {
            workspace: None,
            session: Some(FakeSession::alive()),
        };
        assert!(!live.session_deleted());
    }
}
