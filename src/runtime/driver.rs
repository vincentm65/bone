//! The `Driver` — the one agent loop, extracted from `agent::run_agent`.
//!
//! Previously the loop body lived inline in `run_agent` (headless) and was
//! reimplemented again in the TUI (`ui::app::stream`). The `Driver` is the
//! single owner of that logic so it lives in exactly one place, in core,
//! unit-testable without a terminal. `run_agent` now builds a `Driver` with an
//! [`AutoApprovalGate`] and calls [`Driver::run`].

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use futures_util::StreamExt;

use crate::agent::{
    AgentResponse, AgentRunEvent, emit_event, estimate_context_chars, estimate_tokens,
    summarize_call_args, touch_activity,
};
use crate::chat::build_chat_history;
use crate::ext::ExtensionManager;
use crate::llm::provider::LlmProvider;
use crate::llm::{ChatEvent, ChatMessage, TokenStats, token_tracker::CHARS_PER_TOKEN};
use crate::runtime::RuntimeEvent;
use crate::session_sink::SessionSink;
use crate::tools::registry::ToolHandler;
use crate::tools::{ApprovalGate, ApprovalMode, CallOutcome, ToolCall, ToolResult};

/// The runtime engine: owns everything a turn needs and runs the agent loop.
///
/// Construct it from the pieces produced by `agent::agent_setup` (provider,
/// extensions, tools, session sink, initial history/transcript), choose an
/// [`ApprovalGate`], then call [`Driver::run`].
pub struct Driver {
    pub llm: Arc<dyn LlmProvider>,
    pub extensions: ExtensionManager,
    pub tools: ToolHandler,
    pub session: Arc<dyn SessionSink>,
    /// Resolves tool-call approval. Headless uses [`crate::tools::AutoApprovalGate`];
    /// interactive frontends supply a gate that prompts the user.
    pub gate: Arc<dyn ApprovalGate>,
    pub approval_mode: crate::tools::SharedApprovalMode,
    pub agent_depth: usize,
    pub activity: Option<Arc<AtomicU64>>,
    pub on_token_usage: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    /// Emit JSONL events to stdout (headless `--events`).
    pub events: bool,
    pub event_sender: Option<tokio::sync::mpsc::UnboundedSender<AgentRunEvent>>,
    /// Rich, frontend-facing event stream (`TextDelta`, `ReasoningDelta`, tool
    /// lifecycle, token usage, finished/failed). The interactive frontend (the
    /// TUI, or a remote client) consumes this to render a turn. `None` for the
    /// headless JSONL path, which only needs `event_sender`.
    pub runtime_events: Option<tokio::sync::mpsc::UnboundedSender<RuntimeEvent>>,
    /// Routes `ctx.ui.key` replies back to blocked tools when a frontend is
    /// attached. Required for live tool key input; `None` headless.
    pub key_reply_registry: Option<crate::runtime::KeyReplyRegistry>,
    /// Cooperative cancel flag. When set true mid-turn, the loop stops after the
    /// current stream chunk / tool batch and ends the turn with whatever content
    /// was produced. Also wired into `tools.cancel_token` so running tools abort.
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub history: Vec<ChatMessage>,
    pub transcript: Vec<ChatMessage>,
    pub token_stats: TokenStats,
    pub system_prompt_override: Option<String>,
}

/// What [`Driver::run`] hands back so a stateful frontend (the TUI) can reabsorb
/// the turn's results. The provider and session sink are shared via `Arc` and
/// the Lua VM via the cloned `ExtensionManager`, so those need no return; the
/// transcript, token stats, and tool state (which the Driver owns by value) do.
pub struct DriverOutcome {
    pub result: Result<AgentResponse, String>,
    pub tools: ToolHandler,
    pub transcript: Vec<ChatMessage>,
    pub token_stats: TokenStats,
    /// Per-request usage captured during the turn. The Driver also reports these
    /// to its `session` sink, but a frontend that runs with a `NullSessionSink`
    /// (the TUI persists with its own continuous `session_seq`) reads them from
    /// here to write usage events itself. Empty for headless runs that discard
    /// the outcome.
    pub usage: Vec<UsageRecord>,
}

/// One provider-reported (or estimated) usage record captured during a turn.
#[derive(Clone, Debug)]
pub struct UsageRecord {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cached_tokens: Option<u32>,
    pub cost: Option<f64>,
    pub is_estimated: bool,
}

impl Driver {
    /// Convenience for the headless path: run and return just the result,
    /// discarding the reclaimable state.
    pub async fn run(self, prompt: &str) -> Result<AgentResponse, String> {
        self.run_to_outcome(prompt).await.result
    }

    /// Drive the conversation to a final assistant message, returning the
    /// reclaimable [`DriverOutcome`]. `prompt` is the initiating user turn
    /// (already present in `history`/`transcript` from setup; passed here for
    /// event/session bookkeeping).
    pub async fn run_to_outcome(self, prompt: &str) -> DriverOutcome {
        let Driver {
            llm,
            extensions,
            mut tools,
            session,
            gate,
            approval_mode,
            agent_depth,
            activity,
            on_token_usage,
            events,
            event_sender,
            runtime_events,
            key_reply_registry,
            cancel,
            mut history,
            mut transcript,
            mut token_stats,
            system_prompt_override,
        } = self;
        let is_cancelled = || {
            cancel
                .as_ref()
                .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        };

        let tool_defs = tools.definitions();
        let tool_defs_json_chars = serde_json::to_string(&tool_defs)
            .map(|j| j.chars().count())
            .unwrap_or(0);

        // Reported once at turn start; the live value is re-read each round so a
        // frontend can toggle Safe/Danger mid-turn (see `SharedApprovalMode`).
        let approval_label = approval_mode.get().mode_str();

        let mut session_seq = 0i64;
        let mut usage_records: Vec<UsageRecord> = Vec::new();
        session.append_message("user", prompt, None, None, None, session_seq);

        // Rich frontend event stream (best-effort; ignored if no consumer).
        let remit = |event: RuntimeEvent| {
            if let Some(tx) = runtime_events.as_ref() {
                let _ = tx.send(event);
            }
        };
        let emit_runtime = |event: RuntimeEvent| {
            emit_event(events, event_sender.as_ref(), &event);
            remit(event);
        };

        emit_runtime(RuntimeEvent::Started {
            approval: approval_label.to_string(),
            task: prompt.to_string(),
            model: llm.model().to_string(),
        });
        extensions.dispatch_simple("session_start", serde_json::json!({}));
        extensions.dispatch_simple(
            "turn_start",
            serde_json::json!({
                "task": prompt,
                "model": llm.model(),
                "approval": approval_label,
            }),
        );
        emit_runtime(RuntimeEvent::Status {
            message: "thinking".to_string(),
        });

        let mut consecutive_errors = 0u32;
        let result: Result<String, String> = 'turn: loop {
            if is_cancelled() {
                break Ok(String::new());
            }
            // Dispatch before_turn hook so Lua can compact the conversation
            // and shape the turn (system prompt + tool visibility) before each
            // provider request. `turn_tool_defs` defaults to the full set and is
            // narrowed only when a handler returns a `tool_filter`.
            let mut turn_tool_defs = tool_defs.clone();
            {
                // Refresh context_length from the *current* pending history so
                // the before_turn snapshot reflects what this request will
                // actually send — including tool results appended mid-loop.
                // Without this, the compaction threshold check sees the stale
                // last-request size and can overshoot the model's context limit
                // before compaction ever triggers. The real provider-reported
                // value overwrites this after the request lands.
                token_stats.context_length =
                    estimate_tokens(estimate_context_chars(&history, tool_defs_json_chars)) as u64;
                let state = crate::ext::ctx::AppCtxState::new(
                    &tools,
                    &token_stats,
                    &approval_mode.get(),
                    session.conv_id(),
                    llm.id(),
                    llm.model(),
                    Vec::new(),
                    transcript.clone(),
                );
                let mut ctx_cfg = crate::ext::ctx::build_before_turn_config(&state);
                // Give before_turn handlers a live status channel so they can
                // surface progress to the attached frontend (e.g. compaction).
                ctx_cfg.runtime_status = runtime_events.clone();
                // Thread the turn cancel flag so pressing Esc aborts an
                // in-flight compaction (`ctx.agent.run` watches this).
                ctx_cfg.cancelled = cancel.clone();
                // Subagents (depth > 0) run with no runtime_status channel, so
                // `ctx.ui.status`/`notify` would otherwise fall back to stderr
                // — corrupting the parent TUI, which owns the terminal in raw
                // mode. Mark the depth so those calls drop silently instead.
                ctx_cfg.agent_depth = agent_depth;

                let mut sys_appends: Vec<String> = Vec::new();
                let mut tool_filter: Option<Vec<String>> = None;
                // Run before_turn on a blocking thread: handlers like
                // auto-compaction call `ctx.agent.run`, which blocks (via
                // `block_in_place`). Dispatching inline would freeze the
                // frontend's poll loop for the whole summarization. Cloning the
                // manager (shared `Arc<Mutex<Lua>>`) and awaiting the join lets
                // this future yield so the UI keeps animating.
                let ext_for_hook = extensions.clone();
                let actions = tokio::task::spawn_blocking(move || {
                    ext_for_hook.dispatch_before_turn(&ctx_cfg)
                })
                .await
                .unwrap_or_default();
                for action in actions {
                    if let Some(new_messages) = action.conversation_replace {
                        transcript = new_messages;
                        history =
                            build_chat_history(&transcript, system_prompt_override.as_deref());
                        let prompt_chars = estimate_context_chars(&history, tool_defs_json_chars);
                        token_stats.context_length =
                            (prompt_chars as f64 / CHARS_PER_TOKEN).ceil() as u64;
                    }
                    if let Some(s) = action.system_prompt_append {
                        sys_appends.push(s);
                    }
                    if let Some(t) = action.tool_filter {
                        tool_filter = Some(t);
                    }
                }

                // Append to the system prompt for this turn by rebuilding history
                // from the (possibly just-replaced) transcript.
                if !sys_appends.is_empty() {
                    let base = system_prompt_override
                        .clone()
                        .unwrap_or_else(crate::llm::prompts::system_prompt);
                    let combined = format!("{base}\n\n{}", sys_appends.join("\n\n"));
                    history = build_chat_history(&transcript, Some(&combined));
                    let prompt_chars = estimate_context_chars(&history, tool_defs_json_chars);
                    token_stats.context_length =
                        (prompt_chars as f64 / CHARS_PER_TOKEN).ceil() as u64;
                }

                // Narrow the tools the model sees for this turn. When several
                // `before_turn` handlers return a filter, the last in
                // registration order wins.
                if let Some(allow) = tool_filter {
                    turn_tool_defs.retain(|d| allow.iter().any(|n| n == &d.name));
                    if turn_tool_defs.is_empty() {
                        eprintln!(
                            "bone-lua warn: before_turn tool_filter hid every tool this turn"
                        );
                    }
                }
            }

            // Request stream with retry.
            let mut stream = None;
            for attempt in 1..=3 {
                match llm
                    .chat_stream(history.clone(), turn_tool_defs.clone())
                    .await
                {
                    Ok(s) => {
                        stream = Some(s);
                        break;
                    }
                    Err(e) if attempt < 3 => {
                        emit_runtime(RuntimeEvent::Status {
                            message: format!("retry {attempt}/3: {e}"),
                        });
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    Err(e) => {
                        emit_runtime(RuntimeEvent::Failed {
                            message: e.to_string(),
                        });
                        break 'turn Err(format!("provider error after 3 attempts: {e}"));
                    }
                }
            }
            let mut stream = stream.unwrap();

            // Consume stream.
            let mut assistant_text = String::new();
            let mut reasoning_text = String::new();
            let mut reasoning_echo_field: Option<String> = None;
            let mut tool_calls = Vec::new();
            let mut stream_error = false;
            let mut had_usage = false;

            while let Some(chunk) = stream.next().await {
                touch_activity(&activity);
                if is_cancelled() {
                    break;
                }
                match chunk {
                    Ok(ChatEvent::TextDelta(text)) => {
                        remit(RuntimeEvent::TextDelta { text: text.clone() });
                        assistant_text.push_str(&text);
                    }
                    Ok(ChatEvent::ReasoningDelta { text, echo_field }) => {
                        remit(RuntimeEvent::ReasoningDelta { text: text.clone() });
                        reasoning_text.push_str(&text);
                        if reasoning_echo_field.is_none() {
                            reasoning_echo_field = echo_field;
                        }
                    }
                    Ok(ChatEvent::ToolCall(call)) => {
                        let summary = format!("{}: {}", call.name, summarize_call_args(&call));
                        emit_runtime(RuntimeEvent::ToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            summary: summary.clone(),
                            arguments: call.arguments.clone(),
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
                        usage_records.push(UsageRecord {
                            provider: llm.id().to_string(),
                            model: llm.model().to_string(),
                            prompt_tokens,
                            completion_tokens,
                            cached_tokens,
                            cost,
                            is_estimated: false,
                        });
                        if let Some(cb) = &on_token_usage {
                            cb(token_stats.sent, token_stats.received);
                        }
                        emit_runtime(RuntimeEvent::TokenUsage {
                            sent: token_stats.sent,
                            received: token_stats.received,
                            context_length: token_stats.context_length,
                        });
                        extensions.dispatch_simple(
                            "token_usage",
                            serde_json::json!({
                                "sent": token_stats.sent,
                                "received": token_stats.received,
                                "context_length": token_stats.context_length,
                            }),
                        );
                    }
                    Err(e) => {
                        emit_runtime(RuntimeEvent::Status {
                            message: format!("stream error, will retry: {e}"),
                        });
                        stream_error = true;
                        break;
                    }
                }
            }

            if !had_usage && !stream_error {
                let prompt_chars = estimate_context_chars(&history, tool_defs_json_chars);
                let completion_chars = assistant_text.chars().count()
                    + reasoning_text.chars().count()
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
                usage_records.push(UsageRecord {
                    provider: llm.id().to_string(),
                    model: llm.model().to_string(),
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens: None,
                    cost: None,
                    is_estimated: true,
                });
                if let Some(cb) = &on_token_usage {
                    cb(token_stats.sent, token_stats.received);
                }
                emit_runtime(RuntimeEvent::TokenUsage {
                    sent: token_stats.sent,
                    received: token_stats.received,
                    context_length: token_stats.context_length,
                });
                extensions.dispatch_simple(
                    "token_usage",
                    serde_json::json!({
                        "sent": token_stats.sent,
                        "received": token_stats.received,
                        "context_length": token_stats.context_length,
                    }),
                );
            }

            if stream_error {
                consecutive_errors += 1;
                if consecutive_errors >= 5 {
                    emit_runtime(RuntimeEvent::Failed {
                        message: "too many stream errors".to_string(),
                    });
                    break Err("aborted after 5 consecutive stream errors".to_string());
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
            consecutive_errors = 0;

            // Cancelled mid-stream: end the turn with whatever we have.
            if is_cancelled() {
                break Ok(assistant_text);
            }

            // No tool calls -> done. Record the final assistant message in the
            // transcript (so the returned transcript is complete — the TUI
            // reabsorbs it for context and DB persistence, and the next turn's
            // history needs it).
            if tool_calls.is_empty() {
                let mut assistant = ChatMessage::assistant_with_tools(&assistant_text, Vec::new());
                if !reasoning_text.is_empty() {
                    assistant.reasoning = Some(crate::llm::Reasoning {
                        text: std::mem::take(&mut reasoning_text),
                        echo_field: reasoning_echo_field.take(),
                    });
                }
                transcript.push(assistant);
                session_seq += 1;
                session.append_message("assistant", &assistant_text, None, None, None, session_seq);
                break Ok(assistant_text);
            }

            // Push assistant message with tool calls into history.
            let mut assistant =
                ChatMessage::assistant_with_tools(&assistant_text, tool_calls.clone());
            if !reasoning_text.is_empty() {
                assistant.reasoning = Some(crate::llm::Reasoning {
                    text: std::mem::take(&mut reasoning_text),
                    echo_field: reasoning_echo_field.take(),
                });
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

            // Execute tool calls.
            for call in &tool_calls {
                emit_runtime(RuntimeEvent::Status {
                    message: format!("running {}: {}", call.name, summarize_call_args(call)),
                });
            }

            // Let running tools observe cancellation.
            tools.cancel_token = cancel.clone();
            // Re-read each round so a mid-turn Safe/Danger toggle takes effect
            // on the very next tool batch.
            let results = execute_tool_calls(
                &tools,
                &approval_mode.get(),
                gate.as_ref(),
                tool_calls,
                &extensions,
                agent_depth,
                runtime_events.clone(),
                key_reply_registry.clone(),
            )
            .await;
            touch_activity(&activity);

            // Persist stateful tool state across rounds.
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
                    && page.is_empty()
                {
                    tools.state_map.remove(&page.source, "default");
                }
            }

            for result in &results {
                emit_runtime(RuntimeEvent::ToolResult {
                    name: result.name.clone(),
                    call_id: result.call_id.clone(),
                    is_error: result.is_error,
                    content: result.content.clone(),
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

        // Emit Finished only on success (Failed was already emitted at the
        // break point for error paths).
        if let Ok(content) = &result {
            emit_runtime(RuntimeEvent::Finished {
                content: content.clone(),
            });
        }
        extensions.dispatch_simple(
            "turn_end",
            match &result {
                Ok(content) => serde_json::json!({ "ok": true, "content": content }),
                Err(message) => serde_json::json!({ "ok": false, "error": message }),
            },
        );
        session.end();
        extensions.dispatch_simple("session_end", serde_json::json!({}));

        DriverOutcome {
            result: result.map(|content| AgentResponse { content }),
            tools,
            transcript,
            token_stats,
            usage: usage_records,
        }
    }
}

/// Execute tool calls respecting the approval gate.
///
/// For each call: ask the extension hooks whether to block, compute the policy
/// allow-decision from the approval mode, then let the [`ApprovalGate`] resolve
/// the [`CallOutcome`]. Approved calls are dispatched concurrently via
/// `ToolHandler::execute_all`; blocked/denied calls get an error result.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_tool_calls(
    tools: &ToolHandler,
    mode: &ApprovalMode,
    gate: &dyn ApprovalGate,
    calls: Vec<ToolCall>,
    extensions: &ExtensionManager,
    agent_depth: usize,
    runtime_events: Option<tokio::sync::mpsc::UnboundedSender<RuntimeEvent>>,
    key_reply_registry: Option<crate::runtime::KeyReplyRegistry>,
) -> Vec<ToolResult> {
    // Track original index to preserve call order in output.
    let mut out: Vec<(usize, ToolResult)> = Vec::with_capacity(calls.len());
    let mut approved: Vec<(usize, ToolCall)> = Vec::new();

    for (i, call) in calls.into_iter().enumerate() {
        let safety_str = match crate::tools::command_policy::CommandSafety::for_call(&call) {
            crate::tools::command_policy::CommandSafety::ReadOnly => "read_only",
            crate::tools::command_policy::CommandSafety::Danger => "danger",
        };
        let blocked = match extensions.dispatch_tool_call(
            &call.name,
            &call.id,
            &call.arguments,
            safety_str,
        ) {
            crate::ext::EventDispatchResult::Blocked { reason } => Some(reason),
            crate::ext::EventDispatchResult::Continue => None,
        };
        let auto_allows = tools.allows_call(*mode, &call);

        match gate.decide(blocked, auto_allows, &call).await {
            CallOutcome::Approve => approved.push((i, call)),
            CallOutcome::Blocked(reason) => {
                out.push((
                    i,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: reason,
                        is_error: true,
                        pane_page: None,
                        state: None,
                    },
                ));
            }
            CallOutcome::Denied => {
                let safety = crate::tools::command_policy::CommandSafety::for_call(&call);
                out.push((
                    i,
                    ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: crate::tools::denied_message(*mode, safety),
                        is_error: true,
                        pane_page: None,
                        state: None,
                    },
                ));
            }
        }
    }

    // Execute all approved calls concurrently. When a frontend is attached
    // (`runtime_events`), use the live path and forward each `ToolLiveEvent`
    // (key requests) as a `RuntimeEvent` so the frontend can answer
    // `ctx.ui.key` mid-turn. Pane updates now flow through the standalone
    // `UiState` handle (drained by the TUI directly), not this channel.
    // Headless, there's no consumer, so we use the plain (non-live) path.
    if !approved.is_empty() {
        let approved_calls: Vec<ToolCall> = approved.iter().map(|(_, c)| c.clone()).collect();
        let results = if let Some(events_out) = runtime_events.clone() {
            let (live_tx, mut live_rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::tools::types::ToolLiveEvent>();
            // Forward live tool events to the frontend event stream.
            let forwarder = tokio::spawn(async move {
                use crate::tools::types::ToolLiveEvent;
                while let Some(ev) = live_rx.recv().await {
                    // ToolLiveEvent now has only the Key variant; pane diffs
                    // go through the shared UiState handle.
                    let ToolLiveEvent::Key(req) = ev;
                    if let Some(registry) = &key_reply_registry {
                        let id = registry.register(req);
                        let _ = events_out.send(RuntimeEvent::KeyRequest { id });
                    }
                }
            });
            let results = tools
                .execute_all_live(approved_calls, Some(live_tx), agent_depth, 0)
                .await;
            // All sender handles are now owned by the live tool executions.
            // When they finish, the channel closes and the forwarder exits.
            // Do not pass an extra clone into execute_all_live: if the root
            // future holds a sender across its own await, live_rx never closes
            // and the Driver wedges after ctx.ui.key replies.
            let _ = forwarder.await;
            results
        } else {
            tools.execute_all(approved_calls, agent_depth).await
        };
        for ((orig_idx, _call), result) in approved.into_iter().zip(results) {
            extensions.dispatch_tool_result(&result.name, &result.call_id, result.is_error);
            out.push((orig_idx, result));
        }
    }

    // Restore original call order.
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}
