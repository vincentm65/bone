use crate::chat::build_chat_history;
use crate::config::{UserConfig, custom::CustomConfigs};
use crate::llm::{
    ChatEvent, ChatMessage, ChatRole, TokenStats, providers::create_provider_with_config,
    token_tracker::CHARS_PER_TOKEN,
};
use crate::session_db::{SessionDb, db_path};
use crate::tools::ApprovalMode;
use crate::tools::registry::ToolHandler;
use futures_util::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;

/// Thin wrapper around the optional session DB. It stores only Send data so
/// headless agent futures can run concurrently on the async runtime.
struct SessionWriter {
    db_path: PathBuf,
    conv_id: Option<i64>,
}

impl SessionWriter {
    fn conv_id(&self) -> Option<i64> {
        self.conv_id
    }

    fn append_message(
        &mut self,
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
}

/// Current time in epoch milliseconds.
pub fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Record agent activity on the shared timestamp, if present.
fn touch_activity(activity: &Option<Arc<std::sync::atomic::AtomicU64>>) {
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

enum AgentEvent<'a> {
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

fn emit_event(
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
    llm: Box<dyn crate::llm::provider::LlmProvider>,
    extensions: crate::ext::ExtensionManager,
    tools: ToolHandler,
    history: Vec<ChatMessage>,
    session: SessionWriter,
    token_stats: TokenStats,
    transcript: Vec<ChatMessage>,
}

/// Perform the synchronous setup for a headless agent (config loading,
/// provider creation, Lua boot, tool registry). Designed to run on the
/// blocking thread pool so concurrent headless agents don't starve the tokio
/// runtime.
fn agent_setup(request: &AgentRequest) -> Result<AgentSetup, String> {
    let mut custom = CustomConfigs::load();
    let _user_config = UserConfig::from_custom_configs(&custom);
    let mut providers_config = custom.derive_providers_config();

    let provider_id = request
        .provider
        .clone()
        .or_else(|| non_empty(custom.get_last_provider().as_str()).map(str::to_string))
        .ok_or_else(|| "no provider configured".to_string())?;

    // Persist last_provider before any model override (don't want to save the override).
    // Only persist when running as top-level agent, not as a subagent.
    if request.provider.is_some() && request.agent_depth == 0 {
        custom.set_last_provider(&provider_id);
        providers_config.last_provider = provider_id.clone();
    }

    // Apply model override session-only (never persisted).
    let selected_model = request.model.as_deref();
    if let Some(model) = selected_model {
        if let Some(entry) = providers_config.providers.get_mut(&provider_id) {
            entry.model = model.to_string();
        } else {
            return Err(format!("unknown provider `{provider_id}`"));
        }
    }
    crate::config::warn_if_no_api_key_for(&provider_id, &providers_config);

    let llm =
        create_provider_with_config(&provider_id, &providers_config).map_err(|e| e.to_string())?;

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
    // Sub-agents get the fixed environment/tool scaffold composed with their
    // (optional) persona; a top-level custom prompt replaces the default.
    let history = if request.agent_depth > 0 {
        let composed =
            crate::llm::prompts::subagent_system_prompt(request.system_prompt.as_deref());
        build_chat_history(&transcript, Some(&composed))
    } else {
        build_chat_history(&transcript, request.system_prompt.as_deref())
    };

    let session = open_headless_session(llm.id(), llm.model());

    Ok(AgentSetup {
        llm,
        extensions,
        tools,
        history,
        session,
        token_stats: TokenStats::new(),
        transcript,
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
        mut tools,
        mut history,
        mut session,
        mut token_stats,
        mut transcript,
    } = setup;

    let tool_defs = tools.definitions();
    let tool_defs_json_chars = serde_json::to_string(&tool_defs)
        .map(|j| j.chars().count())
        .unwrap_or(0);

    let approval_label = request.approval_mode.mode_str();

    let mut session_seq = 0i64;
    session.append_message("user", &request.prompt, None, None, None, session_seq);

    let events = request.events;
    let event_sender = request.event_sender.clone();
    let emit = |event: &AgentEvent| emit_event(events, event_sender.as_ref(), event);

    emit(&AgentEvent::Started {
        approval: approval_label,
        task: &request.prompt,
        model: llm.model(),
    });
    extensions.dispatch_simple("session_start", serde_json::json!({}));
    emit(&AgentEvent::Status {
        message: "thinking",
    });

    let mut consecutive_errors = 0u32;
    let final_content = loop {
        // Dispatch before_turn hook so Lua can compact the conversation
        // before each provider request (same as the TUI agent loop).
        {
            let defs = tools.definitions();
            let schema_json = serde_json::to_string(&defs).unwrap_or_default();
            let schema_chars = schema_json.len() as u64;
            let schema_tokens = (schema_chars as f64 / 3.8).ceil() as u64;
            let sys = crate::llm::prompts::system_prompt();
            let sys_chars = sys.len() as u64;
            let sys_tokens = (sys_chars as f64 / 3.8).ceil() as u64;

            let mut ctx_cfg = crate::ext::ctx::new_before_turn_ctx(
                crate::config::bone_dir().to_string_lossy().to_string(),
                Vec::new(),
            );
            ctx_cfg.tool_handler = Some(tools.clone());
            ctx_cfg.approval_mode = request.approval_mode;
            ctx_cfg.session_id = session.conv_id();
            ctx_cfg.provider = Some(llm.id().to_string());
            ctx_cfg.model = Some(llm.model().to_string());
            if let Some(ref mut usage) = ctx_cfg.usage {
                usage.request_count = token_stats.request_count;
                usage.sent = token_stats.sent;
                usage.received = token_stats.received;
                usage.cached = token_stats.cached;
                usage.cost = token_stats.cost;
                usage.context_length = token_stats.context_length;
                usage.tool_count = defs.len() as u64;
                usage.tool_schema_chars = schema_chars;
                usage.tool_schema_tokens = schema_tokens;
                usage.system_prompt_chars = sys_chars;
                usage.system_prompt_tokens = sys_tokens;
            }
            ctx_cfg.conversation_history = Some(transcript.clone());

            let actions = extensions.dispatch_before_turn(&ctx_cfg);
            for action in actions {
                if let Some(new_messages) = action.conversation_replace {
                    transcript = new_messages;
                    history = build_chat_history(&transcript, None);
                    let prompt_chars = estimate_context_chars(&history, tool_defs_json_chars);
                    token_stats.context_length =
                        (prompt_chars as f64 / CHARS_PER_TOKEN).ceil() as u64;
                }
            }
        }

        // Request stream with retry
        let mut stream = None;
        for attempt in 1..=3 {
            match llm.chat_stream(history.clone(), tool_defs.clone()).await {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) if attempt < 3 => {
                    emit(&AgentEvent::Status {
                        message: &format!("retry {attempt}/3: {e}"),
                    });
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(e) => {
                    emit(&AgentEvent::Failed {
                        message: &e.to_string(),
                    });
                    session.end();
                    return Err(format!("provider error after 3 attempts: {e}"));
                }
            }
        }
        let mut stream = stream.unwrap();

        // Consume stream
        let mut assistant_text = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls = Vec::new();
        let mut stream_error = false;
        let mut had_usage = false;

        while let Some(chunk) = stream.next().await {
            touch_activity(&request.activity);
            match chunk {
                Ok(ChatEvent::TextDelta(text)) => {
                    assistant_text.push_str(&text);
                }
                Ok(ChatEvent::ReasoningDelta(text)) => {
                    reasoning_content.push_str(&text);
                }
                Ok(ChatEvent::ToolCall(call)) => {
                    let summary = format!("{}: {}", call.name, summarize_call_args(&call));
                    emit(&AgentEvent::ToolCall {
                        name: &call.name,
                        summary: &summary,
                    });
                    tool_calls.push(call);
                }
                Ok(ChatEvent::TokenUsage {
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                    cost,
                }) => {
                    token_stats.record_request(
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        cost,
                    );
                    had_usage = true;
                    session.record_usage(
                        llm.id(),
                        llm.model(),
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        cost,
                        false,
                    );
                    if let Some(cb) = &request.on_token_usage {
                        cb(token_stats.sent, token_stats.received);
                    }
                    emit(&AgentEvent::TokenUsage {
                        sent: token_stats.sent,
                        received: token_stats.received,
                    });
                }
                Err(e) => {
                    emit(&AgentEvent::Status {
                        message: &format!("stream error, will retry: {e}"),
                    });
                    stream_error = true;
                    break;
                }
            }
        }

        if !had_usage && !stream_error {
            let prompt_chars = estimate_context_chars(&history, tool_defs_json_chars);
            let completion_chars = assistant_text.chars().count()
                + reasoning_content.chars().count()
                + tool_calls
                    .iter()
                    .map(|call| call.arguments.to_string().chars().count())
                    .sum::<usize>();
            let prompt_tokens = estimate_tokens(prompt_chars);
            let completion_tokens = estimate_tokens(completion_chars);
            token_stats.record_estimate(prompt_chars, completion_chars);
            session.record_usage(
                llm.id(),
                llm.model(),
                prompt_tokens,
                completion_tokens,
                None,
                None,
                true,
            );
            if let Some(cb) = &request.on_token_usage {
                cb(token_stats.sent, token_stats.received);
            }
            emit(&AgentEvent::TokenUsage {
                sent: token_stats.sent,
                received: token_stats.received,
            });
        }

        if stream_error {
            consecutive_errors += 1;
            if consecutive_errors >= 5 {
                emit(&AgentEvent::Failed {
                    message: "too many stream errors",
                });
                session.end();
                return Err("aborted after 5 consecutive stream errors".to_string());
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            continue;
        }
        consecutive_errors = 0;

        // No tool calls -> done
        if tool_calls.is_empty() {
            session_seq += 1;
            session.append_message("assistant", &assistant_text, None, None, None, session_seq);
            break assistant_text;
        }

        // Push assistant message with tool calls into history
        let mut assistant = ChatMessage::assistant_with_tools(&assistant_text, tool_calls.clone());
        if !reasoning_content.is_empty() {
            assistant.reasoning_content = Some(std::mem::take(&mut reasoning_content));
        }
        history.push(assistant.clone());
        transcript.push(assistant);
        session_seq += 1;
        let tool_calls_json = serde_json::to_string(&tool_calls).ok();
        session.append_message(
            "assistant",
            &assistant_text,
            None,
            None,
            tool_calls_json.as_deref(),
            session_seq,
        );

        // Execute tool calls
        for call in &tool_calls {
            emit(&AgentEvent::Status {
                message: &format!("running {}: {}", call.name, summarize_call_args(call)),
            });
        }

        let results = execute_tool_calls(
            &tools,
            request.approval_mode,
            tool_calls,
            &extensions,
            request.agent_depth,
        )
        .await;
        touch_activity(&request.activity);

        // Store session state (e.g. task_list state) into tool handler so
        // stateful tools persist across rounds in headless mode.
        for result in &results {
            if let Some(ref state) = result.state {
                let source = result
                    .pane_page
                    .as_ref()
                    .map(|p| p.source.as_str())
                    .unwrap_or(&result.name);
                tools.state_map.set(source, "default", state.clone());
            }
            if let Some(page) = &result.pane_page
                && page.content.is_empty()
            {
                tools.state_map.remove(&page.source, "default");
            }
        }

        for result in &results {
            emit(&AgentEvent::ToolResult {
                name: &result.name,
                is_error: result.is_error,
            });
            session_seq += 1;
            session.append_message(
                "tool",
                &result.content,
                Some(&result.name),
                Some(&result.call_id),
                None,
                session_seq,
            );
            let message = ChatMessage::tool(result.clone());
            history.push(message.clone());
            transcript.push(message);
        }
    };

    emit(&AgentEvent::Finished {
        content: &final_content,
    });
    session.end();
    extensions.dispatch_simple("session_end", serde_json::json!({}));

    Ok(AgentResponse {
        content: final_content,
    })
}

fn estimate_tokens(chars: usize) -> u32 {
    (chars as f64 / CHARS_PER_TOKEN).ceil() as u32
}

fn opt_str_chars(s: Option<&str>) -> usize {
    s.map(str::chars).map(Iterator::count).unwrap_or(0)
}

fn estimate_context_chars(history: &[ChatMessage], tool_defs_json_chars: usize) -> usize {
    let message_chars: usize = history
        .iter()
        .map(|msg| {
            msg.content.chars().count()
                + opt_str_chars(msg.reasoning_content.as_deref())
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

/// Execute tool calls respecting the approval mode.
/// Denied/blocked calls get an error result immediately;
/// allowed calls are dispatched concurrently via `execute_all`.
async fn execute_tool_calls(
    tools: &ToolHandler,
    mode: ApprovalMode,
    calls: Vec<crate::tools::ToolCall>,
    extensions: &crate::ext::ExtensionManager,
    agent_depth: usize,
) -> Vec<crate::tools::ToolResult> {
    let mode_label = mode.mode_str();
    // Track original index to preserve call order in output.
    let mut out: Vec<(usize, crate::tools::ToolResult)> = Vec::with_capacity(calls.len());
    let mut approved: Vec<(usize, crate::tools::ToolCall)> = Vec::new();

    for (i, call) in calls.into_iter().enumerate() {
        // Dispatch tool_call event, check for blocking.
        let safety_str = match crate::tools::command_policy::CommandSafety::for_call(&call) {
            crate::tools::command_policy::CommandSafety::ReadOnly => "read_only",
            crate::tools::command_policy::CommandSafety::Danger => "danger",
        };
        match extensions.dispatch_tool_call(&call.name, &call.id, &call.arguments, safety_str) {
            crate::ext::EventDispatchResult::Blocked { reason } => {
                out.push((
                    i,
                    crate::tools::ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: reason,
                        is_error: true,
                        pane_page: None,
                        state: None,
                    },
                ));
                continue;
            }
            crate::ext::EventDispatchResult::Continue => {}
        }

        if tools.allows_call(mode, &call) {
            approved.push((i, call));
        } else {
            let safety = crate::tools::command_policy::CommandSafety::for_call(&call);
            out.push((i, crate::tools::ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: format!(
                    "[exit_code=1] Tool skipped. Approval mode {mode_label} does not allow {safety:?}; continue using allowed read-only tools or report the limitation."
                ),
                is_error: true,
                pane_page: None,
                state: None,
            }));
        }
    }

    // Execute all approved calls concurrently.
    if !approved.is_empty() {
        let approved_calls: Vec<crate::tools::ToolCall> =
            approved.iter().map(|(_, c)| c.clone()).collect();
        let results = tools.execute_all(approved_calls, agent_depth).await;
        for ((orig_idx, _call), result) in approved.into_iter().zip(results) {
            extensions.dispatch_tool_result(&result.name, &result.call_id, result.is_error);
            out.push((orig_idx, result));
        }
    }

    // Restore original call order.
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

fn summarize_call_args(call: &crate::tools::ToolCall) -> String {
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
    })
}
