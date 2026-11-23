//! Test-only tools shared by runtime tests (`crate::runtime::test_tools`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::tool::{Tool, ToolContext, ToolOutcome, ToolSource, ToolSpec};

/// Returns its arguments unchanged. Requires `text` (string).
pub(crate) struct EchoTool;

/// Sleeps 60 s before answering.
pub(crate) struct SlowTool;

/// Declares `side_effects: true`.
pub(crate) struct WriterTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".into(),
            description: "Echo the arguments back".into(),
            parameters: json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
            }),
            side_effects: false,
        }
    }

    async fn call(&self, args: Value, _ctx: &ToolContext) -> ToolOutcome {
        ToolOutcome::Ok(args)
    }
}

#[async_trait]
impl Tool for SlowTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "slow".into(),
            description: "Sleeps for a minute".into(),
            parameters: json!({"type": "object"}),
            side_effects: false,
        }
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutcome {
        tokio::time::sleep(Duration::from_secs(60)).await;
        ToolOutcome::Ok(json!({}))
    }
}

#[async_trait]
impl Tool for WriterTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "writer".into(),
            description: "Pretends to write".into(),
            parameters: json!({"type": "object"}),
            side_effects: true,
        }
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutcome {
        ToolOutcome::Ok(json!({"written": true}))
    }
}

/// A `ToolSource` over a fixed list of tools.
pub(crate) struct TestSource {
    tools: Vec<Arc<dyn Tool>>,
}

impl TestSource {
    pub(crate) fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self { tools }
    }

    pub(crate) fn echo_boxed() -> Box<dyn ToolSource> {
        Box::new(Self::new(vec![Arc::new(EchoTool)]))
    }

    pub(crate) fn slow_boxed() -> Box<dyn ToolSource> {
        Box::new(Self::new(vec![Arc::new(SlowTool)]))
    }

    pub(crate) fn writer_boxed() -> Box<dyn ToolSource> {
        Box::new(Self::new(vec![Arc::new(WriterTool)]))
    }
}

impl ToolSource for TestSource {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
