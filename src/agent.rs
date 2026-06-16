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
use std::path::PathBuf;
use std::sync::Arc;

/// Thin wrapper around the optional session DB. It stores only Send data so
/// headless agent futures can run concurrently on the async runtime. Public so
/// the TUI can hand the runtime `Driver` a sink for its active conversation.
pub struct SessionWriter {
    db_path: PathBuf,
    conv_id: Option<i64>,
}

impl SessionWriter {
    /// Build a sink that appends to an existing conversation (`conv_id`), or a
    /// no-op when `conv_id` is `None`.
    pub fn new(db_path: PathBuf, conv_id: Option<i64>) -> Self {
        Self { db_path, conv_id }
    }

    fn conv_id(&self) -> Option<i64> {
        self.conv_id
    }

    fn append_message(
        &self,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls: Option<&str>,
        seq: i64,
    ) {
        let Some(conv_id) = self.conv_id else {
            return;
        };
        match SessionDb::open(&self.db_path) {
            Ok(db) => {
                if let Err(e) = db.append_message(
                    conv_id,
                    role,
                    content,
                    tool_name,
                    tool_call_id,
                    tool_calls,
                    seq,
                ) {
                    eprintln!("bone: warning: session db append_message failed: {e}");
                }
            }
            Err(e) => eprintln!("bone: warning: session db append_message failed: {e}"),
        }
    }

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
        match SessionDb::open(&self.db_path) {
            Ok(db) => {
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
                    eprintln!(
                        "bone: warning: session db record_usage{} failed: {e}",
                        if is_estimated { " (estimated)" } else { "" }
                    );
                }
            }
            Err(e) => eprintln!(
                "bone: warning: session db record_usage{} failed: {e}",
                if is_estimated { " (estimated)" } else { "" }
            ),
        }
    }

    fn end(&self) {
        let Some(conv_id) = self.conv_id else {
            return;
        };
        match SessionDb::open(&self.db_path) {
            Ok(db) => {
                if let Err(e) = db.end_conversation(conv_id) {
                    eprintln!("bone: warning: session db end_conversation failed: {e}");
                }
            }
            Err(e) => eprintln!("bone: warning: session db end_conversation failed: {e}"),
        }
    }
}

impl SessionSink for SessionWriter {
    fn conv_id(&self) -> Option<i64> {
        SessionWriter::conv_id(self)
    }

    fn append_message(
        &self,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls: Option<&str>,
        seq: i64,
    ) {
        SessionWriter::append_message(
            self,
            role,
            content,
            tool_name,
            tool_call_id,
            tool_calls,
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
    /// constructing one from config (Step 0 injection seam). Lets callers
    /// (tests, a future Driver) own and share a provider with the loop.
    pub llm: Option<Arc<dyn crate::llm::provider::LlmProvider>>,
    /// Injected session sink. When set, `agent_setup` reuses it as-is instead
    /// of constructing a `SessionWriter` backed by SQLite (Step 3 injection
    /// seam). Lets tests and a future Driver run the loop with zero DB I/O.
    pub session_sink: Option<Arc<dyn SessionSink>>,
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

#[derive(Debug, Clone)]
pub enum AgentRunEvent {
    Started {
        approval: String,
        task: String,
        model: String,
    },
    Status {
        message: String,
    },
    ToolCall {
        name: String,
        summary: String,
    },
    ToolResult {
        name: String,
        is_error: bool,
    },
    TokenUsage {
        sent: u64,
        received: u64,
    },
    Finished {
        content: String,
    },
    Failed {
        message: String,
    },
}

pub(crate) enum AgentEvent<'a> {
    Started {
        approval: &'a str,
        task: &'a str,
        model: &'a str,
    },
    Status {
        message: &'a str,
    },
    ToolCall {
        name: &'a str,
        summary: &'a str,
    },
    ToolResult {
        name: &'a str,
        is_error: bool,
    },
    TokenUsage {
        sent: u64,
        received: u64,
    },
    Finished {
        content: &'a str,
    },
    Failed {
        message: &'a str,
    },
}

pub(crate) fn emit_event(
    events: bool,
    sender: Option<&tokio::sync::mpsc::UnboundedSender<AgentRunEvent>>,
    event: &AgentEvent,
) {
    if let Some(sender) = sender {
        let owned = match event {
            AgentEvent::Started {
                approval,
                task,
                model,
            } => AgentRunEvent::Started {
                approval: (*approval).to_string(),
                task: (*task).to_string(),
                model: (*model).to_string(),
            },
            AgentEvent::Status { message } => AgentRunEvent::Status {
                message: (*message).to_string(),
            },
            AgentEvent::ToolCall { name, summary } => AgentRunEvent::ToolCall {
                name: (*name).to_string(),
                summary: (*summary).to_string(),
            },
            AgentEvent::ToolResult { name, is_error } => AgentRunEvent::ToolResult {
                name: (*name).to_string(),
                is_error: *is_error,
            },
            AgentEvent::TokenUsage { sent, received } => AgentRunEvent::TokenUsage {
                sent: *sent,
                received: *received,
            },
            AgentEvent::Finished { content } => AgentRunEvent::Finished {
                content: (*content).to_string(),
            },
            AgentEvent::Failed { message } => AgentRunEvent::Failed {
                message: (*message).to_string(),
            },
        };
        let _ = sender.send(owned);
    }
    if !events {
        return;
    }
    let json = match event {
        AgentEvent::Started {
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
        AgentEvent::Status { message } => {
            serde_json::json!({ "type": "status", "message": message })
        }
        AgentEvent::ToolCall { name, summary } => {
            let summary = truncate_str(summary, 200);
            serde_json::json!({
                "type": "tool_call",
                "name": name,
                "summary": summary
            })
        }
        AgentEvent::ToolResult { name, is_error } => {
            serde_json::json!({
                "type": "tool_result",
                "name": name,
                "is_error": is_error
            })
        }
        AgentEvent::TokenUsage { sent, received } => {
            serde_json::json!({
                "type": "token_usage",
                "sent": sent,
                "received": received
            })
        }
        AgentEvent::Finished { content } => {
            serde_json::json!({ "type": "finished", "content": content })
        }
        AgentEvent::Failed { message } => {
            serde_json::json!({ "type": "failed", "message": message })
        }
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
    let boxed =
        create_provider_with_config(&provider_id, providers_config).map_err(|e| e.to_string())?;
    Ok(Arc::from(boxed))
}

fn agent_setup(request: &AgentRequest) -> Result<AgentSetup, String> {
    let mut custom = CustomConfigs::load();
    let _user_config = UserConfig::from_custom_configs(&custom);
    let mut providers_config = custom.derive_providers_config();

    let llm = resolve_provider(request, &mut custom, &mut providers_config)?;

    // Boot Lua extension system and build tool handler.
    let booted = crate::ext::boot_with_tools(
        &crate::config::bone_dir(),
        &std::env::current_dir().unwrap_or_default(),
        &mut custom,
        true,
        crate::ext::BootOptions {
            agent_depth: request.agent_depth,
            headless: true,
        },
    );
    let extensions = booted.manager;
    let tools = booted.tools;

    let transcript = vec![ChatMessage::new(ChatRole::User, &request.prompt)];
    extensions.dispatch_simple(
        "message",
        serde_json::json!({ "role": "user", "content": &request.prompt }),
    );
    let system_prompt_override = if request.agent_depth > 0 {
        Some(crate::llm::prompts::subagent_system_prompt(
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

    // The loop itself lives in the core `Driver`. Headless runs use the
    // non-interactive `AutoApprovalGate`; interactive frontends supply their
    // own gate. This is the single agent loop, shared with every frontend.
    let driver = crate::runtime::Driver {
        llm,
        extensions,
        tools,
        session,
        gate: Arc::new(crate::tools::AutoApprovalGate),
        approval_mode: request.approval_mode,
        agent_depth: request.agent_depth,
        activity: request.activity.clone(),
        on_token_usage: request.on_token_usage.clone(),
        events: request.events,
        event_sender: request.event_sender.clone(),
        runtime_events: None,
        reply_registry: None,
        cancel: None,
        history,
        transcript,
        token_stats,
        system_prompt_override,
    };

    driver.run(&request.prompt).await
}

pub(crate) fn estimate_tokens(chars: usize) -> u32 {
    (chars as f64 / CHARS_PER_TOKEN).ceil() as u32
}

fn opt_str_chars(s: Option<&str>) -> usize {
    s.map(str::chars).map(Iterator::count).unwrap_or(0)
}

pub(crate) fn estimate_context_chars(
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
    let conv_id = SessionDb::open(&path)
        .ok()
        .and_then(|db| db.create_conversation(provider, model).ok());
    SessionWriter {
        db_path: path,
        conv_id,
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
        "read_file" | "write_file" | "edit_file" => call
            .arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => call
            .arguments
            .as_object()
            .and_then(|m| m.values().next())
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default(),
    }
}

// ── CLI argument parsing ────────────────────────────────────────────────────

pub fn parse_agent_args(args: &[String]) -> Result<AgentRequest, String> {
    let mut prompt: Option<String> = None;
    let mut approval: Option<String> = None;
    let mut provider: Option<String> = None;
    let mut model: Option<String> = None;
    let mut system_prompt: Option<String> = None;
    let mut events = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--prompt" => {
                i += 1;
                prompt = Some(args.get(i).ok_or("--prompt requires a value")?.clone());
            }
            "--approval" => {
                i += 1;
                let val = args.get(i).ok_or("--approval requires a value")?;
                approval = Some(val.clone());
            }
            "--provider" => {
                i += 1;
                provider = Some(args.get(i).ok_or("--provider requires a value")?.clone());
            }
            "--model" => {
                i += 1;
                model = Some(args.get(i).ok_or("--model requires a value")?.clone());
            }
            "--events" => {
                events = true;
            }
            "--system-prompt" => {
                i += 1;
                system_prompt = Some(
                    args.get(i)
                        .ok_or("--system-prompt requires a value")?
                        .clone(),
                );
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
        i += 1;
    }

    // If no --prompt, read from stdin
    let prompt = prompt.unwrap_or_else(|| {
        use std::io::Read;
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        buf.trim().to_string()
    });

    if prompt.is_empty() {
        return Err("no prompt provided; use --prompt or pipe to stdin".to_string());
    }

    let approval_mode = match approval.as_deref() {
        Some("read_only") | Some("safe") => ApprovalMode::Safe,
        Some("danger") => ApprovalMode::Danger,
        None => ApprovalMode::Safe,
        Some(other) => return Err(format!("unknown approval mode: {other}")),
    };

    Ok(AgentRequest {
        prompt,
        approval_mode,
        provider,
        model,
        system_prompt,
        events,
        event_sender: None,
        agent_depth: 0,
        on_token_usage: None,
        activity: None,
        llm: None,
        session_sink: None,
    })
}
