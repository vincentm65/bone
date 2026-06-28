//! Headless agent turn loop: drives a provider through chat history, tool calls, and session persistence without the TUI.

use crate::chat::build_chat_history;
use crate::config::{UserConfig, custom::CustomConfigs};
use crate::llm::{
    ChatMessage, ChatRole, TokenStats, providers::create_provider_with_config,
    token_tracker::CHARS_PER_TOKEN,
};
use crate::session_db::{SessionDb, db_path};
use crate::session_sink::SessionSink;
use crate::tools::ApprovalMode;
use crate::tools::registry::ToolHandler;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Wraps an open SQLite session connection for the headless agent path.
///
/// Previously this reopened the database on *every* `append_message` /
/// `record_usage` / `end` call. It now holds one [`SessionDb`] (and thus one
/// `Connection`) behind a `Mutex` — `SessionDb` wraps a rusqlite `Connection`
/// which is `Send` but not `Sync`, so the lock makes the sink shareable via
/// `Arc<dyn SessionSink>` and serializes concurrent writers.
///
/// The lock recovers poison (`unwrap_or_else(|e| e.into_inner())`) so a panic
/// while a write is in flight cannot wedge the sink — once `panic = "abort"`
/// is removed a poisoned lock is otherwise a real failure mode.
///
/// Public so the TUI can hand the runtime `Driver` a sink for its active
/// conversation. The TUI itself does not use this — it owns its own held
/// `SessionDb` directly.
pub struct SessionWriter {
    /// Lazily opened connection. `None` only when the DB failed to open at
    /// construction (then `conv_id` is also `None` and every method no-ops).
    db: Mutex<Option<SessionDb>>,
    conv_id: Option<i64>,
    /// Count of persistence writes that failed since construction. Surfaced
    /// via [`SessionSink::persist_failures`] so a caller can warn the user
    /// that recent history may be incomplete — without aborting the turn on a
    /// flaky disk (the write methods still return `()`).
    failures: AtomicU64,
}

impl SessionWriter {
    fn conv_id(&self) -> Option<i64> {
        self.conv_id
    }

    #[allow(clippy::too_many_arguments)]
    fn append_message(
        &self,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls: Option<&str>,
        images: Option<&str>,
        seq: i64,
    ) {
        let Some(conv_id) = self.conv_id else {
            return;
        };
        let guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let Some(db) = guard.as_ref() else {
            return;
        };
        if let Err(e) = db.append_message(
            conv_id,
            role,
            content,
            tool_name,
            tool_call_id,
            tool_calls,
            images,
            // The incremental SessionSink path doesn't carry an error flag; the
            // authoritative tool-error state is persisted via `append_turn`.
            false,
            seq,
        ) {
            self.note_failure("append_message", &e);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_usage(
        &self,
        provider: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
        is_estimated: bool,
    ) {
        let Some(conv_id) = self.conv_id else {
            return;
        };
        let guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let Some(db) = guard.as_ref() else {
            return;
        };
        if let Err(e) = db.record_usage(
            conv_id,
            provider,
            model,
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            cost,
            is_estimated,
        ) {
            self.note_failure("record_usage", &e);
        }
    }

    fn end(&self) {
        let Some(conv_id) = self.conv_id else {
            return;
        };
        let guard = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let Some(db) = guard.as_ref() else {
            return;
        };
        if let Err(e) = db.end_conversation(conv_id) {
            self.note_failure("end_conversation", &e);
        }
    }

    /// Count and log one failed write. Centralized so the call sites stay
    /// uniform and the counter stays accurate.
    fn note_failure(&self, op: &str, err: &rusqlite::Error) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        eprintln!("bone: warning: session db {op} failed: {err}");
    }
}

impl SessionSink for SessionWriter {
    fn conv_id(&self) -> Option<i64> {
        SessionWriter::conv_id(self)
    }

    #[allow(clippy::too_many_arguments)]
    fn append_message(
        &self,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls: Option<&str>,
        images: Option<&str>,
        seq: i64,
    ) {
        SessionWriter::append_message(
            self,
            role,
            content,
            tool_name,
            tool_call_id,
            tool_calls,
            images,
            seq,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn record_usage(
        &self,
        provider: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
        is_estimated: bool,
    ) {
        SessionWriter::record_usage(
            self,
            provider,
            model,
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            cost,
            is_estimated,
        )
    }

    fn end(&self) {
        SessionWriter::end(self)
    }

    fn persist_failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
}

// ── Public types ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AgentRequest {
    pub prompt: String,
    pub approval_mode: ApprovalMode,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub events: bool,
    pub event_sender: Option<tokio::sync::mpsc::UnboundedSender<AgentRunEvent>>,
    pub agent_depth: usize,
    pub on_token_usage: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    /// Last-activity timestamp (epoch ms), updated whenever the agent makes
    /// observable progress (stream chunks, tool results). Used by callers to
    /// implement inactivity-based timeouts instead of hard cutoffs.
    pub activity: Option<Arc<std::sync::atomic::AtomicU64>>,
    /// Injected provider. When set, `agent_setup` reuses it as-is instead of
    /// constructing one from config.
    pub llm: Option<Arc<dyn crate::llm::provider::LlmProvider>>,
    /// Injected session sink. When set, `agent_setup` reuses it as-is instead
    /// of constructing a `SessionWriter` backed by SQLite.
    pub session_sink: Option<Arc<dyn SessionSink>>,
    /// Optional tool allowlist. When set, the agent only sees tools whose
    /// names appear in this list. When `None` (the default), all tools are
    /// available.
    pub tool_allowlist: Option<Vec<String>>,
    /// Optional cap on output tokens for this run. Applied to a freshly
    /// constructed provider (not an injected one). Used by context compaction
    /// to bound the summarization model's output.
    pub max_tokens: Option<u32>,
}

/// Current time in epoch milliseconds.
pub fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Record agent activity on the shared timestamp, if present.
pub(crate) fn touch_activity(activity: &Option<Arc<std::sync::atomic::AtomicU64>>) {
    if let Some(a) = activity {
        a.store(now_epoch_ms(), std::sync::atomic::Ordering::Relaxed);
    }
}

pub struct AgentResponse {
    pub content: String,
}

// ── JSONL event helpers ─────────────────────────────────────────────────────

pub type AgentRunEvent = crate::runtime::RuntimeEvent;

pub(crate) fn emit_event(
    events: bool,
    sender: Option<&tokio::sync::mpsc::UnboundedSender<AgentRunEvent>>,
    event: &crate::runtime::RuntimeEvent,
) {
    if let Some(sender) = sender {
        let _ = sender.send(event.clone());
    }
    if !events {
        return;
    }
    let json = match event {
        crate::runtime::RuntimeEvent::Started {
            approval,
            task,
            model,
        } => {
            let task_preview = truncate_str(task, 200);
            serde_json::json!({
                "type": "started",
                "approval": approval,
                "task": task_preview,
                "model": model
            })
        }
        crate::runtime::RuntimeEvent::Status { message } => {
            serde_json::json!({ "type": "status", "message": message })
        }
        crate::runtime::RuntimeEvent::Notice { message } => {
            serde_json::json!({ "type": "notice", "message": message })
        }
        crate::runtime::RuntimeEvent::ToolCall { name, summary, .. } => {
            let summary = truncate_str(summary, 200);
            serde_json::json!({
                "type": "tool_call",
                "name": name,
                "summary": summary
            })
        }
        crate::runtime::RuntimeEvent::ToolResult { name, is_error, .. } => {
            serde_json::json!({
                "type": "tool_result",
                "name": name,
                "is_error": is_error
            })
        }
        crate::runtime::RuntimeEvent::TokenUsage {
            sent,
            received,
            context_length,
        } => {
            serde_json::json!({
                "type": "token_usage",
                "sent": sent,
                "received": received,
                "context_length": context_length
            })
        }
        crate::runtime::RuntimeEvent::Finished { content } => {
            serde_json::json!({ "type": "finished", "content": content })
        }
        crate::runtime::RuntimeEvent::Failed { message } => {
            serde_json::json!({ "type": "failed", "message": message })
        }
        crate::runtime::RuntimeEvent::TextDelta { .. }
        | crate::runtime::RuntimeEvent::ReasoningDelta { .. }
        | crate::runtime::RuntimeEvent::KeyRequest { .. }
        | crate::runtime::RuntimeEvent::ApprovalRequest { .. }
        | crate::runtime::RuntimeEvent::StateSnapshot { .. }
        | crate::runtime::RuntimeEvent::FrontendState { .. }
        | crate::runtime::RuntimeEvent::ConversationLoaded { .. }
        | crate::runtime::RuntimeEvent::ViewDiff { .. }
        | crate::runtime::RuntimeEvent::CommandComplete { .. }
        | crate::runtime::RuntimeEvent::TurnComplete => return,
    };
    println!("{json}");
}

// ── Headless agent loop ─────────────────────────────────────────────────────

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() { None } else { Some(value) }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Result of the synchronous setup phase for `run_agent`.
struct AgentSetup {
    llm: Arc<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
    tools: ToolHandler,
    history: Vec<ChatMessage>,
    session: Arc<dyn SessionSink>,
    token_stats: TokenStats,
    transcript: Vec<ChatMessage>,
    system_prompt_override: Option<String>,
}

/// Perform the synchronous setup for a headless agent (config loading,
/// provider creation, Lua boot, tool registry). Designed to run on the
/// blocking thread pool so concurrent headless agents don't starve the tokio
/// runtime.
/// Resolve the LLM provider for an agent run — the Step 0 injection seam.
///
/// If `request.llm` is set, the injected provider is reused verbatim and no
/// config side-effects run (no last_provider persistence, no model override,
/// no api-key warning). Otherwise the provider is constructed from config with
/// the same behavior as before. Lets tests and a future Driver own the provider
/// instead of the loop constructing one internally.
pub fn resolve_provider(
    request: &AgentRequest,
    custom: &mut CustomConfigs,
    providers_config: &mut crate::config::ProvidersConfig,
) -> Result<Arc<dyn crate::llm::provider::LlmProvider>, String> {
    if let Some(llm) = request.llm.as_ref() {
        // max_tokens only applies to freshly-constructed providers (AgentRequest
        // doc). Reject the combination rather than silently dropping the cap.
        if request.max_tokens.is_some() {
            return Err("max_tokens is not supported with an injected provider".to_string());
        }
        return Ok(llm.clone());
    }
    let provider_id = request
        .provider
        .clone()
        .or_else(|| non_empty(custom.get_last_provider().as_str()).map(str::to_string))
        .ok_or_else(|| "no provider configured".to_string())?;
    if request.provider.is_some() && request.agent_depth == 0 {
        custom.set_last_provider(&provider_id);
        providers_config.last_provider = provider_id.clone();
    }
    if let Some(model) = request.model.as_deref() {
        if let Some(entry) = providers_config.providers.get_mut(&provider_id) {
            entry.model = model.to_string();
        } else {
            return Err(format!("unknown provider `{provider_id}`"));
        }
    }
    crate::config::warn_if_no_api_key_for(&provider_id, providers_config);
    let mut boxed =
        create_provider_with_config(&provider_id, providers_config).map_err(crate::util::errstr)?;
    boxed.set_max_tokens(request.max_tokens);
    Ok(Arc::from(boxed))
}

fn agent_setup(request: &AgentRequest) -> Result<AgentSetup, String> {
    let mut custom = CustomConfigs::load();
    let _user_config = UserConfig::from_custom_configs(&custom);
    let mut providers_config = custom.derive_providers_config();

    let llm = resolve_provider(request, &mut custom, &mut providers_config)?;

    // Boot Lua extension system and build tool handler.
    let provider = format!("{} ({})", llm.name(), llm.id());
    let model = llm.model().to_string();
    let booted = crate::ext::boot_with_tools(
        &crate::config::bone_dir(),
        &std::env::current_dir().unwrap_or_default(),
        &mut custom,
        true,
        crate::ext::BootOptions {
            agent_depth: request.agent_depth,
            headless: true,
            model: model.clone(),
            provider: provider.clone(),
            tool_allowlist: request.tool_allowlist.clone(),
        },
        &model,
        &provider,
    );
    let extensions = booted.manager;
    let tools = booted.tools;

    let transcript = vec![ChatMessage::new(ChatRole::User, &request.prompt)];
    extensions.dispatch_simple(
        "message",
        serde_json::json!({ "role": "user", "content": &request.prompt }),
    );
    // Any delegated agent (depth > 0) gets the runtime's headless contract
    // wrapped around the caller-supplied persona — independent of which tool or
    // command dispatched it (subagent, compact, memory, shotgun).
    let system_prompt_override = if request.agent_depth > 0 {
        Some(crate::llm::prompts::headless_agent_system_prompt(
            request.system_prompt.as_deref(),
        ))
    } else {
        request.system_prompt.clone()
    };
    let history = build_chat_history(&transcript, system_prompt_override.as_deref());

    let session: Arc<dyn SessionSink> = request
        .session_sink
        .clone()
        .unwrap_or_else(|| Arc::new(open_headless_session(llm.id(), llm.model())));

    Ok(AgentSetup {
        llm,
        extensions,
        tools,
        history,
        session,
        token_stats: TokenStats::new(),
        transcript,
        system_prompt_override,
    })
}

pub async fn run_agent(request: AgentRequest) -> Result<AgentResponse, String> {
    // Run synchronous setup on the blocking thread pool so concurrent
    // headless agents don't starve tokio worker threads during config loading,
    // Lua VM creation, and tool registration.
    let request_clone = request.clone();
    let setup = tokio::task::spawn_blocking(move || agent_setup(&request_clone))
        .await
        .map_err(|e| format!("agent setup panicked: {e}"))??;
    touch_activity(&request.activity);

    let AgentSetup {
        llm,
        extensions,
        tools,
        history,
        session,
        token_stats,
        transcript,
        system_prompt_override,
    } = setup;

    // Keep a handle on the session sink so we can surface any persistence
    // failures after the turn. The Driver consumes its own `Arc` clone; this
    // one keeps the underlying `SessionWriter` alive (refcount) so the
    // failure counter is readable once the run returns.
    let session_report = session.clone();

    // The loop itself lives in the core `Driver`. Headless runs use the
    // non-interactive `AutoApprovalGate`; interactive frontends supply their
    // own gate. This is the single agent loop, shared with every frontend.
    let driver = crate::runtime::Driver {
        llm,
        extensions,
        tools,
        session,
        gate: Arc::new(crate::tools::AutoApprovalGate),
        approval_mode: crate::tools::SharedApprovalMode::new(request.approval_mode),
        agent_depth: request.agent_depth,
        activity: request.activity.clone(),
        on_token_usage: request.on_token_usage.clone(),
        events: request.events,
        event_sender: request.event_sender.clone(),
        runtime_events: None,
        key_reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats,
        system_prompt_override,
        conversation_id: session_report.conv_id(),
    };

    // Snapshot before the turn so an injected sink reused across turns still
    // reports only this turn's misses (the counter is monotonic).
    let before = session_report.persist_failures();
    let result = driver.run(&request.prompt).await;

    // Only the top-level run (depth 0) warns — subagent/compaction runs are
    // internal and a stderr line would be noise. Best-effort: the turn still
    // succeeded, so this is a warning, not an error.
    let failures = session_report.persist_failures().saturating_sub(before);
    if request.agent_depth == 0 && failures > 0 {
        eprintln!(
            "bone: warning: {failures} session write(s) failed this turn; history may be incomplete"
        );
    }

    result
}

pub(crate) fn estimate_tokens(chars: usize) -> u32 {
    (chars as f64 / CHARS_PER_TOKEN).ceil() as u32
}

fn opt_str_chars(s: Option<&str>) -> usize {
    s.map(str::chars).map(Iterator::count).unwrap_or(0)
}

pub fn estimate_context_chars(
    history: &[ChatMessage],
    tool_defs_json_chars: usize,
) -> usize {
    let message_chars: usize = history
        .iter()
        .map(|msg| {
            msg.content.chars().count()
                + msg.reasoning.as_ref().map_or(0, |r| r.text.chars().count())
                + serde_json::to_string(&msg.tool_calls)
                    .map(|json| json.chars().count())
                    .unwrap_or(0)
                + opt_str_chars(msg.tool_call_id.as_deref())
                + opt_str_chars(msg.name.as_deref())
        })
        .sum();
    message_chars + tool_defs_json_chars
}

fn open_headless_session(provider: &str, model: &str) -> SessionWriter {
    let path = db_path();
    // Open once and hold the connection for the lifetime of the sink. On any
    // failure (missing/blocked DB, schema error) we record `None`/`None` and
    // every write method no-ops, matching the old fall-open-per-write behavior
    // where a closed DB simply logged and moved on.
    let (db, conv_id) = match SessionDb::open(&path).and_then(|db| {
        let id = db.create_conversation(provider, model)?;
        Ok((db, id))
    }) {
        Ok((db, id)) => (Some(db), Some(id)),
        Err(e) => {
            eprintln!("bone: warning: session db open failed: {e}");
            (None, None)
        }
    };
    SessionWriter {
        db: Mutex::new(db),
        conv_id,
        failures: AtomicU64::new(0),
    }
}

pub(crate) fn summarize_call_args(call: &crate::tools::ToolCall) -> String {
    match call.name.as_str() {
        "shell" => call
            .arguments
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "write_file" => call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "edit_file" => call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "read_file" => call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => {
            let json = serde_json::to_string(&call.arguments).unwrap_or_default();
            if json.len() > 80 {
                let mut end = 77;
                while !json.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &json[..end])
            } else {
                json
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::summarize_call_args;
    use crate::tools::ToolCall;

    #[test]
    fn summarize_call_args_truncates_json_on_char_boundary() {
        let value = format!("{}{}{}", "a".repeat(67), "😀", "b".repeat(20));
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "custom_tool".to_string(),
            arguments: serde_json::json!({ "text": value }),
        };

        let summary = summarize_call_args(&call);

        assert!(summary.ends_with("..."));
        assert!(summary.len() <= 80);
    }
}
