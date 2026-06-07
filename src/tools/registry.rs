use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::tools::ApprovalMode;
use crate::tools::command_policy::CommandSafety;
use crate::tools::state_map::ToolStateMap;
use crate::tools::types::{
    Tool, ToolCall, ToolDefinition, ToolDisplayConfig, ToolExecutionContext, ToolLiveEvent,
    ToolResult,
};
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

    pub async fn execute_live(
        &self,
        call: ToolCall,
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
        session_state: Option<String>,
    ) -> ToolResult {
        let name = call.name.clone();
        let call_id = call.id.clone();
        match self.tools.get(&name) {
            Some(tool) => match tool
                .execute_output_live(
                    call.arguments,
                    events,
                    ToolExecutionContext {
                        call_id: call_id.clone(),
                        session_state,
                    },
                )
                .await
            {
                Ok(output) => ToolResult {
                    call_id,
                    name,
                    content: output.content,
                    is_error: false,
                    pane_page: output.pane_page,
                    state: output.state,
                },
                Err(content) => ToolResult {
                    call_id,
                    name,
                    content,
                    is_error: true,
                    pane_page: None,
                    state: None,
                },
            },
            None => ToolResult {
                call_id,
                name,
                content: "Unknown tool".to_string(),
                is_error: true,
                pane_page: None,
                state: None,
            },
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct ToolHandler {
    registry: ToolRegistry,
    enabled: HashSet<String>,
    dynamic_safety: HashMap<String, CommandSafety>,
    dynamic_display: HashMap<String, ToolDisplayConfig>,
    pub state_map: ToolStateMap,
}

impl ToolHandler {
    fn is_host_stateful_name(name: &str) -> bool {
        matches!(name, "task_list")
    }

    fn host_state_key_for_name(name: &str) -> Option<&'static str> {
        match name {
            "task_list" => Some("task_list"),
            _ => None,
        }
    }

    fn result_clears_default_state(result: &ToolResult, state_key: &str) -> bool {
        result
            .pane_page
            .as_ref()
            .is_some_and(|page| page.source == state_key && page.content.is_empty())
    }

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
            state_map: ToolStateMap::default(),
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
            state_map: ToolStateMap::default(),
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

    /// Execute all tool calls. Independent calls run concurrently; calls for
    /// host-stateful tools run in-order so each call sees the prior result.
    pub async fn execute_all(&self, calls: Vec<ToolCall>) -> Vec<ToolResult> {
        self.execute_all_live(calls, None).await
    }

    pub async fn execute_all_live(
        &self,
        calls: Vec<ToolCall>,
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
    ) -> Vec<ToolResult> {
        if calls
            .iter()
            .filter(|call| Self::is_host_stateful_name(&call.name))
            .count()
            > 1
        {
            return self.execute_all_serial(calls, events).await;
        }

        join_all(calls.into_iter().map(|call| {
            let events = events.clone();
            let session_state = self.session_state_for_call(&call);
            async move { self.execute_one_live(call, events, session_state).await }
        }))
        .await
    }

    async fn execute_all_serial(
        &self,
        calls: Vec<ToolCall>,
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
    ) -> Vec<ToolResult> {
        let mut results = Vec::with_capacity(calls.len());
        let mut state_overrides: HashMap<String, Option<String>> = HashMap::new();

        for call in calls {
            let state_key = Self::host_state_key_for_name(&call.name);
            let session_state = state_key.and_then(|key| {
                state_overrides
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| self.state_map.get(key, "default").map(String::from))
            });
            let result = self
                .execute_one_live(call, events.clone(), session_state)
                .await;
            if let Some(key) = Self::host_state_key_for_name(&result.name) {
                if Self::result_clears_default_state(&result, key) {
                    state_overrides.insert(key.to_string(), None);
                } else if let Some(state) = result.state.clone() {
                    state_overrides.insert(key.to_string(), Some(state));
                }
            }
            results.push(result);
        }

        results
    }

    async fn execute_one_live(
        &self,
        call: ToolCall,
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
        session_state: Option<String>,
    ) -> ToolResult {
        if self.is_enabled(&call.name) {
            self.registry
                .execute_live(call, events, session_state)
                .await
        } else {
            ToolResult {
                call_id: call.id,
                name: call.name,
                content: "Tool disabled in /tools settings".to_string(),
                is_error: true,
                pane_page: None,
                state: None,
            }
        }
    }

    fn session_state_for_call(&self, call: &ToolCall) -> Option<String> {
        Self::host_state_key_for_name(&call.name)
            .and_then(|key| self.state_map.get(key, "default").map(String::from))
    }
}
