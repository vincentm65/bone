use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::tools::ApprovalMode;
use crate::tools::command_policy::CommandSafety;
use crate::tools::types::{Tool, ToolCall, ToolDefinition, ToolDisplayConfig, ToolResult};
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
        let call_id = call.id.clone();
        match self.tools.get(&name) {
            Some(tool) => match tool.execute_output(call.arguments).await {
                Ok(output) => ToolResult {
                    call_id,
                    name,
                    content: output.content,
                    is_error: false,
                    pane_page: output.pane_page,
                },
                Err(content) => ToolResult {
                    call_id,
                    name,
                    content,
                    is_error: true,
                    pane_page: None,
                },
            },
            None => ToolResult {
                call_id,
                name,
                content: "Unknown tool".to_string(),
                is_error: true,
                pane_page: None,
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
    dynamic_safety: HashMap<String, CommandSafety>,
    dynamic_display: HashMap<String, ToolDisplayConfig>,
}

impl ToolHandler {
    pub fn new(registry: ToolRegistry) -> Self {
        let enabled = registry
            .definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        Self {
            registry,
            enabled,
            dynamic_safety: HashMap::new(),
            dynamic_display: HashMap::new(),
        }
    }

    pub fn with_enabled(registry: ToolRegistry, enabled: &[String]) -> Self {
        Self {
            registry,
            enabled: enabled.iter().cloned().collect(),
            dynamic_safety: HashMap::new(),
            dynamic_display: HashMap::new(),
        }
    }

    pub fn with_enabled_and_safety(
        registry: ToolRegistry,
        enabled: &[String],
        dynamic_safety: HashMap<String, CommandSafety>,
    ) -> Self {
        Self {
            registry,
            enabled: enabled.iter().cloned().collect(),
            dynamic_safety,
            dynamic_display: HashMap::new(),
        }
    }

    pub fn with_enabled_safety_and_display(
        registry: ToolRegistry,
        enabled: &[String],
        dynamic_safety: HashMap<String, CommandSafety>,
        dynamic_display: HashMap<String, ToolDisplayConfig>,
    ) -> Self {
        Self {
            registry,
            enabled: enabled.iter().cloned().collect(),
            dynamic_safety,
            dynamic_display,
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

    pub fn safety_for_call(&self, call: &ToolCall) -> CommandSafety {
        self.dynamic_safety
            .get(&call.name)
            .copied()
            .unwrap_or_else(|| CommandSafety::for_call(call))
    }

    pub fn allows_call(&self, mode: ApprovalMode, call: &ToolCall) -> bool {
        mode.allows_safety(self.safety_for_call(call))
    }

    pub fn display_for_call(&self, call: &ToolCall) -> Option<&ToolDisplayConfig> {
        self.dynamic_display.get(&call.name)
    }

    pub async fn execute_all(&self, calls: Vec<ToolCall>) -> Vec<ToolResult> {
        if calls.iter().filter(|call| call.name == "task_list").count() > 1 {
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                if self.is_enabled(&call.name) {
                    results.push(self.registry.execute(call).await);
                } else {
                    results.push(ToolResult {
                        call_id: call.id,
                        name: call.name,
                        content: "Tool disabled in /tools settings".to_string(),
                        is_error: true,
                        pane_page: None,
                    });
                }
            }
            return results;
        }

        join_all(calls.into_iter().map(|call| async move {
            if self.is_enabled(&call.name) {
                self.registry.execute(call).await
            } else {
                ToolResult {
                    call_id: call.id,
                    name: call.name,
                    content: "Tool disabled in /tools settings".to_string(),
                    is_error: true,
                    pane_page: None,
                }
            }
        }))
        .await
    }
}
