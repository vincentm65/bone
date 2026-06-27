//! Tool registry: dispatch, approval gating, and live-event plumbing.

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
        let mut definitions: Vec<_> = self.tools.values().map(|tool| tool.definition()).collect();
        definitions.sort_by(|a, b| a.name.cmp(&b.name));
        definitions
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_live(
        &self,
        call: ToolCall,
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
        session_state: Option<String>,
        owner: String,
        cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        agent_depth: usize,
        tool_call_depth: usize,
        tool_handler: Option<ToolHandler>,
        app_state: Option<crate::ext::ctx::AppCtxState>,
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
                        owner,
                        cancelled,
                        agent_depth,
                        tool_call_depth,
                        tool_handler,
                        app_state,
                    },
                )
                .await
            {
                Ok(output) => ToolResult::ok(call_id, name, output),
                Err(content) => ToolResult::error(call_id, name, content),
            },
            None => ToolResult::error(call_id, name, "Unknown tool"),
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
    dynamic_display: HashMap<String, ToolDisplayConfig>,
    pub state_map: ToolStateMap,
    pub owner: String,
    dynamic_safety: HashMap<String, CommandSafety>,
    /// Tools that hold host-managed state, mapped to their state key. A tool
    /// declares this via `stateful = true` in `register_tool`; the host then
    /// serializes its batched calls and threads state across rounds. Replaces
    /// the previous hardcoded `task_list` name check.
    host_state_keys: HashMap<String, String>,
    /// Cancellation token set by TUI when user cancels streaming.
    pub cancel_token: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// App-derived ctx snapshot, refreshed per turn by the TUI before dispatch.
    /// Propagated to nested/subagent calls via the recursive `self.clone()` in
    /// `execute_one_live`, so tools see the same `ctx` as slash commands.
    pub(crate) app_state: Option<crate::ext::ctx::AppCtxState>,
}

impl ToolHandler {
    fn is_host_stateful_name(&self, name: &str) -> bool {
        self.host_state_keys.contains_key(name)
    }

    fn host_state_key_for_name(&self, name: &str) -> Option<&str> {
        self.host_state_keys.get(name).map(String::as_str)
    }

    fn result_clears_default_state(result: &ToolResult, state_key: &str) -> bool {
        result
            .pane_page
            .as_ref()
            .is_some_and(|page| page.source == state_key && page.is_empty())
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
            dynamic_display: HashMap::new(),
            state_map: ToolStateMap::default(),
            owner: String::new(),
            dynamic_safety: HashMap::new(),
            host_state_keys: HashMap::new(),
            cancel_token: None,
            app_state: None,
        }
    }

    pub fn with_enabled_safety_and_display(
        registry: ToolRegistry,
        enabled: &[String],
        dynamic_display: HashMap<String, ToolDisplayConfig>,
        dynamic_safety: HashMap<String, CommandSafety>,
        host_state_keys: HashMap<String, String>,
    ) -> Self {
        Self {
            registry,
            enabled: enabled.iter().cloned().collect(),
            dynamic_display,
            dynamic_safety,
            host_state_keys,
            state_map: ToolStateMap::default(),
            owner: String::new(),
            cancel_token: None,
            app_state: None,
        }
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.contains(name)
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
            .filter(|tool| self.is_enabled(&tool.name) && !tool.name.starts_with('/'))
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
    pub async fn execute_all(&self, calls: Vec<ToolCall>, agent_depth: usize) -> Vec<ToolResult> {
        self.execute_all_live(calls, None, agent_depth, 0).await
    }

    pub async fn execute_all_live(
        &self,
        calls: Vec<ToolCall>,
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
        agent_depth: usize,
        tool_call_depth: usize,
    ) -> Vec<ToolResult> {
        // Bail out early if cancellation was requested.
        if self
            .cancel_token
            .as_ref()
            .is_some_and(|t| t.load(std::sync::atomic::Ordering::Relaxed))
        {
            return calls
                .into_iter()
                .map(|call| ToolResult::error(call.id, call.name, "cancelled by user"))
                .collect();
        }

        if calls
            .iter()
            .filter(|call| self.is_host_stateful_name(&call.name))
            .count()
            > 1
        {
            return self
                .execute_all_serial(calls, events, agent_depth, tool_call_depth)
                .await;
        }

        join_all(calls.into_iter().map(|call| {
            let events = events.clone();
            let session_state = self.session_state_for_call(&call);
            let owner = self.owner.clone();
            async move {
                self.execute_one_live(
                    call,
                    events,
                    session_state,
                    owner,
                    agent_depth,
                    tool_call_depth,
                )
                .await
            }
        }))
        .await
    }

    async fn execute_all_serial(
        &self,
        calls: Vec<ToolCall>,
        events: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
        agent_depth: usize,
        tool_call_depth: usize,
    ) -> Vec<ToolResult> {
        let mut results = Vec::with_capacity(calls.len());
        let mut state_overrides: HashMap<String, Option<String>> = HashMap::new();

        for call in calls {
            let state_key = self.host_state_key_for_name(&call.name);
            let session_state = state_key.and_then(|key| {
                state_overrides
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| self.state_map.get(key, "default").map(String::from))
            });
            let result = self
                .execute_one_live(
                    call,
                    events.clone(),
                    session_state,
                    self.owner.clone(),
                    agent_depth,
                    tool_call_depth,
                )
                .await;
            if let Some(key) = self.host_state_key_for_name(&result.name) {
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
        owner: String,
        agent_depth: usize,
        tool_call_depth: usize,
    ) -> ToolResult {
        if call.name.starts_with('/') {
            ToolResult::error(
                call.id,
                call.name,
                "Slash commands are UI commands, not tools.",
            )
        } else if self.is_enabled(&call.name) {
            self.registry
                .execute_live(
                    call,
                    events,
                    session_state,
                    owner,
                    self.cancel_token.clone(),
                    agent_depth,
                    tool_call_depth,
                    Some(self.clone()),
                    self.app_state.clone(),
                )
                .await
        } else {
            ToolResult::error(call.id, call.name, "Tool disabled in /tools settings")
        }
    }

    fn session_state_for_call(&self, call: &ToolCall) -> Option<String> {
        self.host_state_key_for_name(&call.name)
            .and_then(|key| self.state_map.get(key, "default").map(String::from))
    }
}

impl std::fmt::Debug for ToolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolHandler")
            .field("enabled", &self.enabled)
            .field("owner", &self.owner)
            .finish()
    }
}
