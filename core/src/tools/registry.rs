//! Tool registry: dispatch, approval gating, and live-event plumbing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::pane_content::KeyRequest;
use crate::tools::ApprovalMode;
use crate::tools::command_policy::CommandSafety;
use crate::tools::state_map::ToolStateMap;
use crate::tools::types::{
    Tool, ToolCall, ToolDefinition, ToolDisplayConfig, ToolExecutionContext, ToolResult,
};
use futures_util::future::join_all;

fn emit_tool_result(
    events: &Option<tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeEvent>>,
    result: &ToolResult,
) {
    if let Some(events) = events {
        let _ = events.send(crate::runtime::RuntimeEvent::ToolResult {
            name: result.name.clone(),
            call_id: result.call_id.clone(),
            is_error: result.is_error,
            content: result.content.clone(),
        });
    }
}

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

    pub fn register_mut<T: Tool + 'static>(&mut self, tool: T) {
        let name = tool.definition().name;
        self.tools.insert(name, Arc::new(tool));
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
        events: Option<tokio::sync::mpsc::UnboundedSender<KeyRequest>>,
        session_state: Option<String>,
        owner: String,
        cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        agent_depth: usize,
        tool_call_depth: usize,
        tool_handler: Option<ToolHandler>,
        app_state: Option<crate::ext::ctx::AppCtxState>,
        approval_gate: Option<crate::tools::SharedGate>,
        runtime_events: Option<tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeEvent>>,
    ) -> ToolResult {
        let name = call.name.clone();
        let call_id = call.id.clone();
        match self.tools.get(&name) {
            Some(tool) => {
                if let Some(msg) = reject_degenerate_arguments(tool.as_ref(), &call.arguments) {
                    return ToolResult::error(call_id, name, msg);
                }
                let snapshots = tool_handler
                    .as_ref()
                    .map(|h| h.snapshots.clone())
                    .unwrap_or_default();
                let working_dir = tool_handler.as_ref().and_then(|h| h.working_dir.clone());
                match tool
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
                            runtime_events,
                            approval_gate,
                            snapshots,
                            working_dir,
                        },
                    )
                    .await
                {
                    Ok(output) => ToolResult::ok(call_id, name, output),
                    Err(content) => ToolResult::error(call_id, name, content),
                }
            }
            None => ToolResult::error(call_id, name, "Unknown tool"),
        }
    }
}

/// The tool's declared required fields, in schema order (empty if none).
fn required_fields(tool: &dyn Tool) -> Vec<String> {
    tool.definition()
        .input_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Reject calls whose arguments cannot possibly satisfy the tool's schema:
/// null, a non-object, or an empty object when required fields are declared.
/// Models in a degenerate loop emit these; a uniform, actionable error beats
/// serde's deserialization message. Tools without required fields still accept
/// empty/absent arguments.
fn reject_degenerate_arguments(tool: &dyn Tool, arguments: &serde_json::Value) -> Option<String> {
    // The provider wraps arguments truncated mid-stream (usually the output
    // token cap) as `{ TRUNCATED_ARGS_KEY: "<raw>" }`. Resending the same call
    // reproduces the truncation, so steer the model toward smaller edits rather
    // than an identical retry. Checked before the non-empty-object accept below,
    // since the wrapper is a non-empty object.
    if let Some(raw) = arguments
        .get(crate::tools::TRUNCATED_ARGS_KEY)
        .and_then(|v| v.as_str())
    {
        let required = required_fields(tool);
        return Some(format!(
            "tool call arguments were truncated ({} bytes of incomplete JSON, likely the output-token limit); \
             do not resend the same call — split the work into smaller edits with the required field(s): {}",
            raw.len(),
            if required.is_empty() {
                "(see tool schema)".to_string()
            } else {
                required.join(", ")
            }
        ));
    }
    let required = required_fields(tool);
    if let Some(fields) = arguments.as_object() {
        if !fields.is_empty() || required.is_empty() {
            return None;
        }
    }
    let got = match arguments {
        serde_json::Value::Null => "no arguments".to_string(),
        serde_json::Value::Object(_) => "an empty arguments object".to_string(),
        other => format!(
            "arguments of type {}",
            match other {
                serde_json::Value::String(_) => "string",
                serde_json::Value::Array(_) => "array",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::Bool(_) => "boolean",
                _ => "unknown",
            }
        ),
    };
    let required = if required.is_empty() {
        "(see tool schema)".to_string()
    } else {
        required.join(", ")
    };
    Some(format!(
        "tool call arrived with {got}; re-send the call as a JSON object with required field(s): {required}"
    ))
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
    pub approval_gate: Option<crate::tools::SharedGate>,
    /// App-derived ctx snapshot, refreshed per turn by the TUI before dispatch.
    /// Propagated to nested/subagent calls via the recursive `self.clone()` in
    /// `execute_one_live`, so tools see the same `ctx` as slash commands.
    pub(crate) app_state: Option<crate::ext::ctx::AppCtxState>,
    /// Stable project directory used to resolve relative tool paths.
    pub working_dir: Option<std::path::PathBuf>,
    /// Session-scoped file snapshots backing `read_file`/`write_file`/
    /// `edit_file`. Behind an `Arc<RwLock<..>>` so every cloned handler in a
    /// turn (and across turns) shares one store — the driver clones the
    /// `ToolHandler` per turn but never swaps this `Arc`, so snapshots persist
    /// for the whole session.
    pub snapshots: std::sync::Arc<std::sync::RwLock<crate::tools::snapshot::SnapshotStore>>,
    /// Conversation-scoped `ctx.state` map. Shared (via `Arc`) with Lua tools,
    /// slash commands, and `before_turn` so they see the same keys within one
    /// session without a process-global singleton.
    pub shared_state: crate::ext::ctx::SharedState,
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
            approval_gate: None,
            app_state: None,
            working_dir: std::env::current_dir().ok(),
            snapshots: std::sync::Arc::new(std::sync::RwLock::new(Default::default())),
            shared_state: crate::ext::ctx::new_shared_state(),
        }
    }

    pub fn with_enabled_safety_and_display(
        registry: ToolRegistry,
        enabled: &[String],
        dynamic_display: HashMap<String, ToolDisplayConfig>,
        dynamic_safety: HashMap<String, CommandSafety>,
        host_state_keys: HashMap<String, String>,
    ) -> Self {
        let mut handler = Self::new(registry);
        handler.enabled = enabled.iter().cloned().collect();
        handler.dynamic_display = dynamic_display;
        handler.dynamic_safety = dynamic_safety;
        handler.host_state_keys = host_state_keys;
        handler
    }

    pub fn with_working_dir(mut self, working_dir: impl Into<std::path::PathBuf>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }

    /// Use a pre-allocated conversation-scoped `ctx.state` map (e.g. the Arc
    /// already captured by Lua tools during boot).
    pub fn with_shared_state(mut self, shared_state: crate::ext::ctx::SharedState) -> Self {
        self.shared_state = shared_state;
        self
    }

    /// After an extension reload, keep session-scoped fields from `previous`
    /// while adopting the freshly booted registry/definitions/display maps.
    ///
    /// Snapshots, host tool state (`task_list`, etc.), the approval gate, cancel
    /// token, and app ctx are conversation state and must not reset when Lua
    /// tools are reloaded mid-session.
    pub fn adopt_session_state_from(&mut self, previous: &ToolHandler) {
        self.snapshots = previous.snapshots.clone();
        self.working_dir = previous.working_dir.clone();
        self.state_map = previous.state_map.clone();
        self.shared_state = previous.shared_state.clone();
        self.approval_gate = previous.approval_gate.clone();
        self.cancel_token = previous.cancel_token.clone();
        self.app_state = previous.app_state.clone();
        self.owner = previous.owner.clone();
    }

    /// Drop host-held tool state for a conversation reset (`/new`, `/clear`,
    /// load another chat). Clears the driver `state_map`, file snapshots, and
    /// the live `ctx.state` map used by Lua tools / `before_turn`.
    pub fn clear_host_state(&mut self) {
        self.state_map.clear();
        self.snapshots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let mut map = self.shared_state.lock().unwrap_or_else(|e| e.into_inner());
        map.clear();
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

    /// All registered tool display configs (`name → config`). Lets the daemon
    /// ship them to a VM-less frontend so it can render custom tool rows.
    pub fn display_map(&self) -> &HashMap<String, ToolDisplayConfig> {
        &self.dynamic_display
    }

    pub fn all_definitions(&self) -> Vec<ToolDefinition> {
        self.registry
            .definitions()
            .into_iter()
            .filter(|tool| !tool.name.starts_with('/'))
            .collect()
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
        self.execute_all_live(calls, None, agent_depth, 0, None)
            .await
    }

    pub async fn execute_all_live(
        &self,
        calls: Vec<ToolCall>,
        events: Option<tokio::sync::mpsc::UnboundedSender<KeyRequest>>,
        agent_depth: usize,
        tool_call_depth: usize,
        runtime_events: Option<tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeEvent>>,
    ) -> Vec<ToolResult> {
        // Bail out early if cancellation was requested.
        if self
            .cancel_token
            .as_ref()
            .is_some_and(|t| t.load(std::sync::atomic::Ordering::Relaxed))
        {
            let results: Vec<_> = calls
                .into_iter()
                .map(|call| ToolResult::error(call.id, call.name, "cancelled by user"))
                .collect();
            if tool_call_depth == 0 {
                for result in &results {
                    emit_tool_result(&runtime_events, result);
                }
            }
            return results;
        }

        if calls
            .iter()
            .filter(|call| self.is_host_stateful_name(&call.name))
            .count()
            > 1
        {
            return self
                .execute_all_serial(calls, events, agent_depth, tool_call_depth, runtime_events)
                .await;
        }

        join_all(calls.into_iter().map(|call| {
            let events = events.clone();
            let session_state = self.session_state_for_call(&call);
            let owner = self.owner.clone();
            let runtime_events = runtime_events.clone();
            let result_events = runtime_events.clone();
            async move {
                let result = self
                    .execute_one_live(
                        call,
                        events,
                        session_state,
                        owner,
                        agent_depth,
                        tool_call_depth,
                        runtime_events,
                    )
                    .await;
                if tool_call_depth == 0 {
                    emit_tool_result(&result_events, &result);
                }
                result
            }
        }))
        .await
    }

    async fn execute_all_serial(
        &self,
        calls: Vec<ToolCall>,
        events: Option<tokio::sync::mpsc::UnboundedSender<KeyRequest>>,
        agent_depth: usize,
        tool_call_depth: usize,
        runtime_events: Option<tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeEvent>>,
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
                    runtime_events.clone(),
                )
                .await;
            if let Some(key) = self.host_state_key_for_name(&result.name) {
                if Self::result_clears_default_state(&result, key) {
                    state_overrides.insert(key.to_string(), None);
                } else if let Some(state) = result.state.clone() {
                    state_overrides.insert(key.to_string(), Some(state));
                }
            }
            if tool_call_depth == 0 {
                emit_tool_result(&runtime_events, &result);
            }
            results.push(result);
        }

        results
    }

    async fn execute_one_live(
        &self,
        call: ToolCall,
        events: Option<tokio::sync::mpsc::UnboundedSender<KeyRequest>>,
        session_state: Option<String>,
        owner: String,
        agent_depth: usize,
        tool_call_depth: usize,
        runtime_events: Option<tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeEvent>>,
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
                    self.approval_gate.clone(),
                    runtime_events,
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

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
