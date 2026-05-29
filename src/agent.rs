use crate::chat::build_chat_history;
use crate::config::{load_providers, load_user_config, save_providers};
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
    pub events: bool,
    pub timeout_ms: u64,
}

pub struct AgentResponse {
    pub content: String,
}

// ── JSONL event helpers ─────────────────────────────────────────────────────

fn emit_event(events: bool, json: &str) {
    if events {
        println!("{json}");
    }
}

fn event_started(events: bool, approval: &str, task: &str, model: &str) {
    let task_preview = if task.len() > 200 { &task[..200] } else { task };
    emit_event(
        events,
        &serde_json::json!({
            "type": "started",
            "approval": approval,
            "task": task_preview,
            "model": model
        })
        .to_string(),
    );
}

fn event_status(events: bool, message: &str) {
    emit_event(
        events,
        &serde_json::json!({
            "type": "status",
            "message": message
        })
        .to_string(),
    );
}

fn event_tool_call(events: bool, name: &str, summary: &str) {
    let summary = if summary.len() > 200 {
        &summary[..200]
    } else {
        summary
    };
    emit_event(
        events,
        &serde_json::json!({
            "type": "tool_call",
            "name": name,
            "summary": summary
        })
        .to_string(),
    );
}

fn event_tool_result(events: bool, name: &str, is_error: bool) {
    emit_event(
        events,
        &serde_json::json!({
            "type": "tool_result",
            "name": name,
            "is_error": is_error
        })
        .to_string(),
    );
}

fn event_token_usage(events: bool, sent: u64, received: u64) {
    emit_event(
        events,
        &serde_json::json!({
            "type": "token_usage",
            "sent": sent,
            "received": received
        })
        .to_string(),
    );
}

fn event_finished(events: bool, content: &str) {
    emit_event(
        events,
        &serde_json::json!({
            "type": "finished",
            "content": content
        })
        .to_string(),
    );
}

fn event_failed(events: bool, message: &str) {
    emit_event(
        events,
        &serde_json::json!({
            "type": "failed",
            "message": message
        })
        .to_string(),
    );
}

// ── Recursion guard ─────────────────────────────────────────────────────────

pub(crate) const MAX_AGENT_DEPTH: u32 = 3;

pub(crate) fn check_depth() -> Result<u32, String> {
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
    let user_config = load_user_config();
    let mut providers_config = load_providers();

    let provider_id = request
        .provider
        .clone()
        .or_else(|| {
            if !user_config.provider.is_empty() {
                Some(user_config.provider.clone())
            } else if !providers_config.last_provider.is_empty() {
                Some(providers_config.last_provider.clone())
            } else {
                None
            }
        })
        .ok_or_else(|| "no provider configured".to_string())?;

    if let Some(model) = request.model.as_ref() {
        let entry = providers_config
            .providers
            .get_mut(&provider_id)
            .ok_or_else(|| format!("unknown provider `{provider_id}`"))?;
        entry.model = model.clone();
    }
    if request.provider.is_some() || request.model.is_some() {
        providers_config.last_provider = provider_id.clone();
        save_providers(&providers_config);
    }

    let llm =
        create_provider_with_config(&provider_id, &providers_config).map_err(|e| e.to_string())?;
    llm.validate()
        .await
        .map_err(|e| format!("provider validation failed: {e}"))?;

    let loaded = load_tools();
    let tools = ToolHandler::with_enabled_safety_and_display(
        loaded.registry,
        &user_config.enabled_tools,
        loaded.dynamic_safety,
        loaded.dynamic_display,
    );

    let approval_label = match request.approval_mode {
        ApprovalMode::Safe => "read_only",
        ApprovalMode::Edits => "edit",
        ApprovalMode::Danger => "danger",
    };

    // Set recursion depth for child processes (safe: single-threaded before any spawns)
    // SAFETY: set_var is safe in single-threaded code before any async tasks spawn.
    // The recursion depth is only read by child processes (never concurrent).
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("BONE_AGENT_DEPTH", new_depth.to_string());
    }

    // Build initial history
    let mut transcript: Vec<ChatMessage> = vec![ChatMessage::new(ChatRole::User, &request.prompt)];
    let mut history = build_chat_history(&transcript);
    let mut token_stats = TokenStats::new();

    event_started(request.events, approval_label, &request.prompt, llm.model());
    event_status(request.events, "thinking");

    let max_rounds = user_config.max_rounds;

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
                event_failed(request.events, &e.to_string());
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
                    event_tool_call(request.events, &call.name, &summary);
                    tool_calls.push(call);
                }
                Ok(ChatEvent::TokenUsage {
                    prompt_tokens,
                    completion_tokens,
                }) => {
                    token_stats.record_request(prompt_tokens, completion_tokens);
                    event_token_usage(request.events, token_stats.sent, token_stats.received);
                }
                Err(e) => {
                    event_failed(request.events, &e.to_string());
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
            event_status(
                request.events,
                &format!("running {}: {}", call.name, summarize_call_args(call)),
            );
        }

        let results = execute_tool_calls(&tools, request.approval_mode, tool_calls).await;

        for result in &results {
            event_tool_result(request.events, &result.name, result.is_error);
            let message = ChatMessage::tool(result.clone());
            history.push(message.clone());
            transcript.push(message);
        }
    };

    event_finished(request.events, &final_content);

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
    let mut out = Vec::with_capacity(calls.len());
    for call in calls {
        if tools.allows_call(mode, &call) {
            let mut executed = tools.execute_all(vec![call.clone()]).await;
            let result = executed
                .pop()
                .expect("execute_all returns one result per call");
            out.push(result);
        } else {
            let mode_label = match mode {
                ApprovalMode::Safe => "read_only",
                ApprovalMode::Edits => "edit",
                ApprovalMode::Danger => "danger",
            };
            let safety = crate::tools::command_policy::CommandSafety::for_call(&call);
            out.push(crate::tools::ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: format!(
                    "[exit_code=1] Tool not executed. Sub-agent approval mode {mode_label} does not allow {safety:?}."
                ),
                is_error: true,
                pane_page: None,
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
    let mut events = false;
    let mut timeout_ms: u64 = 300_000;

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
            "--timeout-ms" => {
                i += 1;
                timeout_ms = args
                    .get(i)
                    .ok_or("--timeout-ms requires a value")?
                    .parse::<u64>()
                    .map_err(|e| format!("invalid --timeout-ms: {e}"))?;
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
        events,
        timeout_ms,
    })
}
