use crate::chat::build_chat_history;
use crate::config::{UserConfig, custom::CustomConfigs, load_providers, save_providers};
use crate::llm::{
    ChatEvent, ChatMessage, ChatRole, TokenStats, providers::create_provider_with_config,
    token_tracker::CHARS_PER_TOKEN,
};
use crate::session_db::{SessionDb, db_path};
use crate::tools::{ApprovalMode, ToolHandler, load_tools};
use futures_util::StreamExt;

/// Thin wrapper around the optional session DB that eliminates repetitive
/// `if let Some((db, conv_id))` guards throughout the agent loop.
struct SessionWriter<'a> {
    inner: Option<(&'a SessionDb, i64)>,
}

/// Execute a session DB operation if a session is active.
macro_rules! session_op {
    ($self:expr, $db:ident, $conv_id:ident, $body:expr) => {
        if let Some(($db, $conv_id)) = $self.inner {
            let $db: &SessionDb = $db;
            $body
        }
    };
}

impl<'a> SessionWriter<'a> {
    fn from_opt(opt: &'a Option<(SessionDb, i64)>) -> Self {
        Self {
            inner: opt.as_ref().map(|(db, id)| (db, *id)),
        }
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
        session_op!(self, db, conv_id, {
            if let Err(e) = db.append_message(
                conv_id, role, content, tool_name, tool_call_id, tool_calls, seq,
            ) {
                eprintln!("bone: warning: session db append_message failed: {e}");
            }
        });
    }

    fn record_real_usage(
        &self,
        provider: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
    ) {
        session_op!(self, db, conv_id, {
            if let Err(e) = db.record_usage(
                conv_id, provider, model, prompt_tokens, completion_tokens, cached_tokens, cost, false,
            ) {
                eprintln!("bone: warning: session db record_usage failed: {e}");
            }
        });
    }

    fn record_estimated_usage(
        &self,
        provider: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
    ) {
        session_op!(self, db, conv_id, {
            if let Err(e) = db.record_usage(
                conv_id, provider, model, prompt_tokens, completion_tokens, None, None, true,
            ) {
                eprintln!("bone: warning: session db record_usage (estimated) failed: {e}");
            }
        });
    }

    fn end(&self) {
        session_op!(self, db, conv_id, {
            if let Err(e) = db.end_conversation(conv_id) {
                eprintln!("bone: warning: session db end_conversation failed: {e}");
            }
        });
    }
}

// ── Public types ────────────────────────────────────────────────────────────

pub struct AgentRequest {
    pub prompt: String,
    pub approval_mode: ApprovalMode,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub events: bool,
}

pub struct AgentResponse {
    pub content: String,
}

// ── JSONL event helpers ─────────────────────────────────────────────────────

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

fn emit_event(events: bool, event: &AgentEvent) {
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

// ── Recursion guard ─────────────────────────────────────────────────────────

const MAX_AGENT_DEPTH: u32 = 3;
/// Truncate a string to at most `max` bytes, respecting char boundaries.
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut i = max;
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        &s[..i]
    }
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() { None } else { Some(value) }
}

fn check_depth() -> Result<u32, String> {
    let depth: u32 = std::env::var("BONE_AGENT_DEPTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if depth >= MAX_AGENT_DEPTH {
        return Err(format!(
            "sub-agent recursion depth {depth} >= {MAX_AGENT_DEPTH}; refusing to launch"
        ));
    }
    Ok(depth + 1)
}

// ── Headless agent loop ─────────────────────────────────────────────────────

pub async fn run_agent(request: AgentRequest) -> Result<AgentResponse, String> {
    let new_depth = check_depth()?;

    // Load config
    let custom = CustomConfigs::load();
    let user_config = UserConfig::from_custom_configs(&custom);
    let mut providers_config = load_providers();

    let config_provider = non_empty(user_config.subagent.provider.as_str());
    let config_model = non_empty(user_config.subagent.model.as_str());

    let provider_id = request
        .provider
        .clone()
        .or_else(|| config_provider.map(str::to_string))
        .or_else(|| non_empty(providers_config.last_provider.as_str()).map(str::to_string))
        .ok_or_else(|| "no provider configured".to_string())?;

    let selected_model = request.model.as_deref().or(config_model);
    if let Some(model) = selected_model {
        let entry = providers_config
            .providers
            .get_mut(&provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        entry.model = model.to_string();
    }
    if request.provider.is_some() || request.model.is_some() {
        providers_config.last_provider = provider_id.clone();
        save_providers(&providers_config);
    }
    crate::config::warn_if_no_api_key_for(&provider_id, &providers_config);

    let llm =
        create_provider_with_config(&provider_id, &providers_config).map_err(|e| e.to_string())?;
    llm.validate()
        .await
        .map_err(|e| format!("provider validation failed: {e}"))?;

    let loaded = load_tools();
    let all_tool_names: Vec<String> = loaded
        .registry
        .definitions()
        .iter()
        .map(|d| d.name.clone())
        .collect();
    let mut synced_custom = custom;
    synced_custom.sync_tools_from_registry(&all_tool_names);
    let enabled = synced_custom.enabled_tool_names();
    let enabled = if enabled.is_empty() {
        all_tool_names
    } else {
        enabled
    };
    let mut tools = ToolHandler::with_enabled_safety_and_display(
        loaded.registry,
        &enabled,
        loaded.dynamic_safety,
        loaded.dynamic_display,
    );

    let approval_label = request.approval_mode.mode_str();

    // Set recursion depth for child processes.
    // SAFETY: no other tasks have been spawned yet; this is single-threaded code.
    unsafe {
        std::env::set_var("BONE_AGENT_DEPTH", new_depth.to_string());
    }

    // Build initial history
    let mut transcript: Vec<ChatMessage> = vec![ChatMessage::new(ChatRole::User, &request.prompt)];
    let mut history = build_chat_history(&transcript, request.system_prompt.as_deref(), "");
    let mut token_stats = TokenStats::new();
    let tool_defs = tools.definitions();
    let tool_defs_json_chars = serde_json::to_string(&tool_defs)
        .map(|j| j.chars().count())
        .unwrap_or(0);
    let session_db = open_headless_session_db(llm.id(), llm.model());
    let mut session = SessionWriter::from_opt(&session_db);
    let mut session_seq = 0i64;
    session.append_message("user", &request.prompt, None, None, None, session_seq);
    let events = request.events;
    let emit = |event: &AgentEvent| emit_event(events, event);

    emit(
        &AgentEvent::Started {
            approval: approval_label,
            task: &request.prompt,
            model: llm.model(),
        },
    );
    emit(
        &AgentEvent::Status {
            message: "thinking",
        },
    );

    let mut consecutive_errors = 0u32;
    let final_content = loop {
        // Request stream with retry
        let mut stream = None;
        for attempt in 1..=3 {
            match llm.chat_stream(history.clone(), tool_defs.clone()).await {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) if attempt < 3 => {
                    emit(
                        &AgentEvent::Status {
                            message: &format!("retry {attempt}/3: {e}"),
                        },
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Err(e) => {
                    emit(
                        &AgentEvent::Failed {
                            message: &e.to_string(),
                        },
                    );
                    session.end();
                    return Err(format!("provider error after 3 attempts: {e}"));
                }
            }
        }
        let mut stream = stream.unwrap();

        // Consume stream
        let mut assistant_text = String::new();
        let mut tool_calls = Vec::new();
        let mut stream_error = false;
        let mut had_usage = false;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(ChatEvent::TextDelta(text)) => {
                    assistant_text.push_str(&text);
                }
                Ok(ChatEvent::ReasoningDelta(_)) => {}
                Ok(ChatEvent::ToolCall(call)) => {
                    let summary = format!("{}: {}", call.name, summarize_call_args(&call));
                    emit(
                        &AgentEvent::ToolCall {
                            name: &call.name,
                            summary: &summary,
                        },
                    );
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
                    session.record_real_usage(
                        llm.id(),
                        llm.model(),
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        cost,
                    );
                    emit(
                        &AgentEvent::TokenUsage {
                            sent: token_stats.sent,
                            received: token_stats.received,
                        },
                    );
                }
                Err(e) => {
                    emit(
                        &AgentEvent::Status {
                            message: &format!("stream error, will retry: {e}"),
                        },
                    );
                    stream_error = true;
                    break;
                }
            }
        }

        if !had_usage && !stream_error {
            let prompt_chars = estimate_context_chars(&history, tool_defs_json_chars);
            let completion_chars = assistant_text.chars().count()
                + tool_calls
                    .iter()
                    .map(|call| call.arguments.to_string().chars().count())
                    .sum::<usize>();
            let prompt_tokens = estimate_tokens(prompt_chars);
            let completion_tokens = estimate_tokens(completion_chars);
            token_stats.record_estimate(prompt_chars, completion_chars);
            session.record_estimated_usage(llm.id(), llm.model(), prompt_tokens, completion_tokens);
            emit(
                &AgentEvent::TokenUsage {
                    sent: token_stats.sent,
                    received: token_stats.received,
                },
            );
        }

        if stream_error {
            consecutive_errors += 1;
            if consecutive_errors >= 5 {
                emit(
                    &AgentEvent::Failed {
                        message: "too many stream errors",
                    },
                );
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
        let assistant = ChatMessage::assistant_with_tools(&assistant_text, tool_calls.clone());
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
            emit(
                &AgentEvent::Status {
                    message: &format!("running {}: {}", call.name, summarize_call_args(call)),
                },
            );
        }

        let results = execute_tool_calls(&tools, request.approval_mode, tool_calls).await;

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
            emit(
                &AgentEvent::ToolResult {
                    name: &result.name,
                    is_error: result.is_error,
                },
            );
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

    emit(
        &AgentEvent::Finished {
            content: &final_content,
        },
    );
    session.end();

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

fn open_headless_session_db(provider: &str, model: &str) -> Option<(SessionDb, i64)> {
    let db = SessionDb::open(&db_path()).ok()?;
    let conv_id = db.create_conversation(provider, model).ok()?;
    Some((db, conv_id))
}

/// Execute tool calls respecting the approval mode.
/// Denied calls get an error result instead of execution.
async fn execute_tool_calls(
    tools: &ToolHandler,
    mode: ApprovalMode,
    calls: Vec<crate::tools::ToolCall>,
) -> Vec<crate::tools::ToolResult> {
    let mode_label = mode.mode_str();
    let mut out = Vec::with_capacity(calls.len());
    for call in calls {
        if tools.allows_call(mode, &call) {
            let mut executed = tools.execute_all(vec![call.clone()]).await;
            let result = executed
                .pop()
                .expect("execute_all returns one result per call");
            out.push(result);
        } else {
            let safety = crate::tools::command_policy::CommandSafety::for_call(&call);
            out.push(crate::tools::ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: format!(
                    "[exit_code=1] Tool not executed. Approval mode {mode_label} does not allow {safety:?}."
                ),
                is_error: true,
                pane_page: None,
                state: None,
            });
        }
    }
    out
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
        Some("edit") | Some("edits") => ApprovalMode::Edits,
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
    })
}
