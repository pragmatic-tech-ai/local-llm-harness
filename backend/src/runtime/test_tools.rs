//! Test-only tools shared by runtime tests (`crate::runtime::test_tools`).

use std::sync::atomic::{AtomicBool, Ordering};
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

/// Panics when called.
pub(crate) struct PanicTool;

/// Sets the flag after a 200 ms sleep; used to prove aborted calls never finish.
pub(crate) struct FlagTool {
    pub(crate) flag: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for PanicTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "panic".into(),
            description: "Panics".into(),
            parameters: json!({"type": "object"}),
            side_effects: false,
        }
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutcome {
        panic!("boom");
    }
}

#[async_trait]
impl Tool for FlagTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "flag".into(),
            description: "Sets a flag after sleeping".into(),
            parameters: json!({"type": "object"}),
            side_effects: false,
        }
    }

    async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutcome {
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.flag.store(true, Ordering::SeqCst);
        ToolOutcome::Ok(json!({}))
    }
}

/// Reports the write folder from its context's scope.
pub(crate) struct ScopeTool;

#[async_trait]
impl Tool for ScopeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "scope".into(),
            description: "Reports its scope".into(),
            parameters: json!({"type": "object"}),
            side_effects: false,
        }
    }

    async fn call(&self, _args: Value, ctx: &ToolContext) -> ToolOutcome {
        ToolOutcome::Ok(json!({"workspace": ctx.scope.workspace}))
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

    pub(crate) fn panic_boxed() -> Box<dyn ToolSource> {
        Box::new(Self::new(vec![Arc::new(PanicTool)]))
    }

    pub(crate) fn flag_boxed(flag: Arc<AtomicBool>) -> Box<dyn ToolSource> {
        Box::new(Self::new(vec![Arc::new(FlagTool { flag })]))
    }

    pub(crate) fn writer_boxed() -> Box<dyn ToolSource> {
        Box::new(Self::new(vec![Arc::new(WriterTool)]))
    }

    pub(crate) fn scope_boxed() -> Box<dyn ToolSource> {
        Box::new(Self::new(vec![Arc::new(ScopeTool)]))
    }
}

impl ToolSource for TestSource {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}
