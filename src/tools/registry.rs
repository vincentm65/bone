use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::tools::types::{Tool, ToolCall, ToolDefinition, ToolResult};
use futures_util::future::join_all;

#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register<T: Tool + 'static>(mut self, tool: T) -> Self {
        let name = tool.definition().name;
        self.tools.insert(name, Arc::new(tool));
        self
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }

    pub async fn execute(&self, call: ToolCall) -> ToolResult {
        let name = call.name.clone();
        match self.tools.get(&name) {
            Some(tool) => match tool.execute(call.arguments).await {
                Ok(content) => ToolResult {
                    call_id: call.id,
                    name,
                    content,
                    is_error: false,
                },
                Err(content) => ToolResult {
                    call_id: call.id,
                    name,
                    content,
                    is_error: true,
                },
            },
            None => ToolResult {
                call_id: call.id,
                name,
                content: "Unknown tool".to_string(),
                is_error: true,
            },
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ToolHandler {
    registry: ToolRegistry,
    enabled: HashSet<String>,
}

impl ToolHandler {
    pub fn new(registry: ToolRegistry) -> Self {
        let enabled = registry
            .definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        Self { registry, enabled }
    }

    pub fn with_enabled(registry: ToolRegistry, enabled: &[String]) -> Self {
        Self {
            registry,
            enabled: enabled.iter().cloned().collect(),
        }
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        if enabled {
            self.enabled.insert(name.to_string());
        } else {
            self.enabled.remove(name);
        }
    }

    pub fn enabled_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.enabled.iter().cloned().collect();
        names.sort();
        names
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.registry
            .definitions()
            .into_iter()
            .filter(|tool| self.is_enabled(&tool.name))
            .collect()
    }

    pub fn available_definitions(&self) -> Vec<ToolDefinition> {
        self.registry.definitions()
    }

    pub async fn execute_all(&self, calls: Vec<ToolCall>) -> Vec<ToolResult> {
        join_all(calls.into_iter().map(|call| async move {
            if self.is_enabled(&call.name) {
                self.registry.execute(call).await
            } else {
                ToolResult {
                    call_id: call.id,
                    name: call.name,
                    content: "Tool disabled in /tools settings".to_string(),
                    is_error: true,
                }
            }
        }))
        .await
    }
}
