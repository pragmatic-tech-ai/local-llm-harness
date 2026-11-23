use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use thiserror::Error;

use super::tool::{Tool, ToolSource, ToolSpec};

#[derive(Debug, Error, PartialEq)]
pub enum RegistryError {
    #[error("duplicate tool name: {0}")]
    Duplicate(String),
}

/// Named collection of tools, preserving source order.
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
    index: HashMap<String, usize>,
}

impl ToolRegistry {
    pub fn new(sources: Vec<Box<dyn ToolSource>>) -> Result<Self, RegistryError> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        let mut index = HashMap::new();
        for source in &sources {
            for tool in source.tools() {
                let name = tool.spec().name;
                if index.contains_key(&name) {
                    return Err(RegistryError::Duplicate(name));
                }
                index.insert(name, tools.len());
                tools.push(tool);
            }
        }
        Ok(Self { tools, index })
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.index.get(name).map(|&i| self.tools[i].clone())
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }

    /// Tool list in the OpenAI chat-completions `tools` shape.
    pub fn openai_tools(&self) -> Vec<Value> {
        self.specs()
            .into_iter()
            .map(|s| {
                json!({
                    "type": "function",
                    "function": {
                        "name": s.name,
                        "description": s.description,
                        "parameters": s.parameters,
                    }
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::test_tools::TestSource;

    #[test]
    fn registry_rejects_duplicate_names() {
        let sources: Vec<Box<dyn ToolSource>> =
            vec![TestSource::echo_boxed(), TestSource::echo_boxed()];
        match ToolRegistry::new(sources) {
            Err(e) => assert_eq!(e, RegistryError::Duplicate("echo".into())),
            Ok(_) => panic!("expected duplicate error"),
        }
    }

    #[test]
    fn registry_exports_openai_tool_shape() {
        let reg = ToolRegistry::new(vec![TestSource::echo_boxed()]).unwrap();
        let spec = reg.specs().remove(0);
        assert_eq!(
            reg.openai_tools()[0],
            json!({"type":"function","function":{
                "name":"echo",
                "description": spec.description,
                "parameters": spec.parameters,
            }})
        );
    }
}
