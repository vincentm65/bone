use crate::chat::build_chat_history;
use crate::config::{UserConfig, custom::CustomConfigs, load_providers, save_providers};
use crate::llm::{
    ChatEvent, ChatMessage, ChatRole, TokenStats, providers::create_provider_with_config,
};
use crate::tools::{ApprovalMode, ToolHandler, load_tools};
use futures_util::StreamExt;

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
    unsafe { std::env::set_var("BONE_AGENT_DEPTH", new_depth.to_string()); }

    // Build initial history
    let mut transcript: Vec<ChatMessage> = vec![ChatMessage::new(ChatRole::User, &request.prompt)];
    let mut history = build_chat_history(&transcript, request.system_prompt.as_deref());
    let mut token_stats = TokenStats::new();

    emit_event(
        request.events,
        &AgentEvent::Started {
            approval: approval_label,
            task: &request.prompt,
            model: llm.model(),
        },
    );
    emit_event(
        request.events,
        &AgentEvent::Status {
            message: "thinking",
        },
    );

    let max_rounds = user_config.subagent.max_rounds;

    let mut rounds = 0u32;
    let final_content = loop {
        rounds += 1;
        if rounds > max_rounds {
            break "[tool-call round limit reached]".to_string();
        }

        // Request stream
        let stream_result = llm.chat_stream(history.clone(), tools.definitions()).await;

        let mut stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                emit_event(
                    request.events,
                    &AgentEvent::Failed {
                        message: &e.to_string(),
                    },
                );
                return Err(format!("provider error: {e}"));
            }
        };

        // Consume stream
        let mut assistant_text = String::new();
        let mut tool_calls = Vec::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(ChatEvent::TextDelta(text)) => {
                    assistant_text.push_str(&text);
                }
                Ok(ChatEvent::ReasoningDelta(_)) => {}
                Ok(ChatEvent::ToolCall(call)) => {
                    let summary = format!("{}: {}", call.name, summarize_call_args(&call));
                    emit_event(
                        request.events,
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
                    ..
                }) => {
                    token_stats.record_request(prompt_tokens, completion_tokens, None, None);
                    emit_event(
                        request.events,
                        &AgentEvent::TokenUsage {
                            sent: token_stats.sent,
                            received: token_stats.received,
                        },
                    );
                }
                Err(e) => {
                    emit_event(
                        request.events,
                        &AgentEvent::Failed {
                            message: &e.to_string(),
                        },
                    );
                    return Err(format!("stream error: {e}"));
                }
            }
        }

        // No tool calls -> done
        if tool_calls.is_empty() {
            break assistant_text;
        }

        // Push assistant message with tool calls into history
        let assistant = ChatMessage::assistant_with_tools(&assistant_text, tool_calls.clone());
        history.push(assistant.clone());
        transcript.push(assistant);

        // Execute tool calls
        for call in &tool_calls {
            emit_event(
                request.events,
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
            emit_event(
                request.events,
                &AgentEvent::ToolResult {
                    name: &result.name,
                    is_error: result.is_error,
                },
            );
            let message = ChatMessage::tool(result.clone());
            history.push(message.clone());
            transcript.push(message);
        }
    };

    emit_event(
        request.events,
        &AgentEvent::Finished {
            content: &final_content,
        },
    );

    Ok(AgentResponse {
        content: final_content,
    })
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
