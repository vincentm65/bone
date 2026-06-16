use crate::chat::{Message, build_chat_history};
use crate::llm::{ChatMessage, ChatRole};
use crate::tools::edit_file::preview_edit_file;
use crate::tools::shell::ShellTool;
use crate::tools::types::{Tool, ToolLiveEvent};
use crate::tools::{ApprovalMode, ToolCall, ToolResult};
use crate::ui::input::{InputAction, InputState};
use crate::ui::pane_page::PanePage;
use crate::ui::render::{BoneTerminal, PaneDraw};
use crate::ui::tool_display::build_tool_row;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::pin_mut;
use std::collections::VecDeque;
use std::io;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};

use super::{App, apply_input_key_with_paste_burst};

pub fn tool_error(call: &ToolCall, content: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        content: content.into(),
        is_error: true,
        pane_page: None,
        state: None,
    }
}

impl App {
    pub(crate) async fn send_message(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        // Expand any collapsed paste placeholders so the model and transcript
        // receive the full pasted content, not the `[Pasted text …]` token.
        let text = self.input.expanded();
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        if let Some(cmd) = text.strip_prefix(':') {
            return self.run_inline_command(cmd, term).await;
        }

        if let Some(cmd) = text.strip_prefix('/') {
            self.input.reset();
            self.redraw(term)?;
            return self.handle_command(cmd, term).await;
        }

        // Keep the short placeholder in scrollback while sending full content.
        let display_text = self
            .input
            .has_pastes()
            .then(|| self.input.buffer.trim().to_string());
        self.submit_user_turn(text, display_text, term).await
    }

    /// Run the turn through the core `Driver` and render its `RuntimeEvent`
    /// stream, answering approval/interaction over channels. The Driver owns
    /// clones of `tools`/`extensions` (Lua VM shared via Arc) and `Arc`
    /// provider/session; it runs as a local future pumped by a `select!` loop
    /// on this task — so there's no spawn, no borrow conflict, and (crucially)
    /// the render path never touches the Lua VM while a tool runs lock-free.
    /// State (transcript/token_stats/tool-state) is reabsorbed from the
    /// returned `DriverOutcome`.
    pub(super) async fn submit_user_turn(
        &mut self,
        text: String,
        display_text: Option<String>,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        use crate::runtime::{
            ApprovalRequest, ChannelApprovalGate, Driver, ReplyRegistry, RuntimeEvent,
        };
        use crate::session_sink::{NullSessionSink, SessionSink};
        use crate::tools::CallOutcome;
        use crate::ui::prompt::Decision;
        use serde_json::Value;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // User message + DB persistence.
        self.messages
            .push(Message::user(display_text.as_deref().unwrap_or(&text)));
        self.transcript
            .push(ChatMessage::new(ChatRole::User, &text));
        if let Some(ref db) = self.session_db
            && let Some(conv_id) = self.conversation_id
        {
            self.session_seq += 1;
            db.append_message(conv_id, "user", &text, None, None, None, self.session_seq)
                .ok();
        }
        self.extensions.dispatch_simple(
            "message",
            serde_json::json!({ "role": "user", "content": text }),
        );
        // New assistant/tool transcript messages begin here; persisted at turn end
        // from the Driver's authoritative returned transcript.
        let persist_from = self.transcript.len();
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.input.reset();
        self.redraw(term)?;
        self.streaming = true;
        self.shown_tool_rows.clear();
        self.turn_start = Some(std::time::Instant::now());
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;

        let (rt_tx, mut rt_rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
        let (appr_tx, mut appr_rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let registry = ReplyRegistry::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let session: Arc<dyn SessionSink> = Arc::new(NullSessionSink);

        let driver = Driver {
            llm: self.llm.clone(),
            extensions: self.extensions.clone(),
            tools: self.tools.clone(),
            session,
            gate: Arc::new(ChannelApprovalGate::new(appr_tx)),
            approval_mode: self.approval_mode,
            agent_depth: 0,
            activity: None,
            on_token_usage: None,
            events: false,
            event_sender: None,
            runtime_events: Some(rt_tx),
            reply_registry: Some(registry.clone()),
            cancel: Some(cancel.clone()),
            history: build_chat_history(&self.transcript, None),
            transcript: self.transcript.clone(),
            token_stats: self.token_stats.clone(),
            system_prompt_override: None,
        };
        let mut run_fut = Box::pin(driver.run_to_outcome(&text));

        let mut cur_idx: Option<usize> = None;
        let mut pending: std::collections::HashMap<String, crate::tools::ToolCall> =
            std::collections::HashMap::new();
        let mut pending_interacts: std::collections::HashMap<
            u64,
            tokio::sync::oneshot::Receiver<Value>,
        > = std::collections::HashMap::new();
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(90));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let outcome = loop {
            // Always drain keys before polling Driver/events. A fast stream can
            // otherwise starve a timer branch and make Esc lag until a lull.
            if Self::drain_keys(
                &mut self.input,
                &mut self.queue,
                &mut self.approval_mode,
                &mut self.cancel_streaming,
                &mut self.panes_visible,
                &mut self.pages,
                &mut self.active_page,
            ) {
                self.user_config.approval_mode = self.approval_mode;
                self.persist_runtime_config();
            }
            if self.pump_drain_interact_replies(&registry, &mut pending_interacts) {
                self.cancel_streaming = true;
            }
            if self.cancel_streaming {
                cancel.store(true, Ordering::Relaxed);
                break None;
            }

            tokio::select! {
                outcome = &mut run_fut => break Some(outcome),
                Some(ev) = rt_rx.recv() => {
                    self.pump_apply_event(ev, &mut cur_idx, &mut pending, &mut pending_interacts, term)?;
                }
                Some(areq) = appr_rx.recv() => {
                    // Show the edit-file diff preview before deciding, so the
                    // user sees what's being changed (in Safe and Danger modes).
                    if areq.call.name == "edit_file" {
                        self.pump_show_edit_preview(&areq.call, term).await?;
                    }
                    if areq.auto_allows {
                        let _ = areq.reply.send(CallOutcome::Approve);
                    } else {
                        self.timer_pause();
                        let decision = self.prompt_and_wait(&areq.call, term)?;
                        self.timer_resume();
                        let resolved = match decision {
                            Decision::Accept => CallOutcome::Approve,
                            Decision::Advise(a) => CallOutcome::Blocked(format!(
                                "[exit_code=1] Tool not executed. User advice: {a}"
                            )),
                            Decision::Cancel => {
                                self.cancel_streaming = true;
                                cancel.store(true, Ordering::Relaxed);
                                CallOutcome::Denied
                            }
                        };
                        let _ = areq.reply.send(resolved);
                    }
                }
                _ = ticker.tick() => {
                    self.pump_tick(term)?;
                }
            }
        };

        // Drain any trailing events emitted just before the Driver returned
        // (final text deltas, the last tool result, Finished).
        while let Ok(ev) = rt_rx.try_recv() {
            self.pump_apply_event(ev, &mut cur_idx, &mut pending, &mut pending_interacts, term)?;
        }
        let _ = self.pump_drain_interact_replies(&registry, &mut pending_interacts);

        // Reabsorb authoritative state from the Driver when it completed.
        if let Some(outcome) = outcome {
            self.transcript = outcome.transcript;
            self.token_stats = outcome.token_stats;
            self.tools.state_map = outcome.tools.state_map;

            // Persist the turn's new assistant/tool messages to the session DB
            // (so Driver turns appear in /history). Clone the slice out
            // first to release the borrow before the &mut self DB helpers.
            let new_msgs: Vec<ChatMessage> = self
                .transcript
                .get(persist_from..)
                .map(<[ChatMessage]>::to_vec)
                .unwrap_or_default();
            for msg in &new_msgs {
                match msg.role {
                    ChatRole::Assistant => {
                        let tc = if msg.tool_calls.is_empty() {
                            None
                        } else {
                            serde_json::to_string(&msg.tool_calls).ok()
                        };
                        self.append_assistant_to_db(&msg.content, tc.as_deref());
                    }
                    ChatRole::Tool => {
                        self.append_tool_result_to_db(
                            msg.name.as_deref().unwrap_or("tool"),
                            msg.tool_call_id.as_deref().unwrap_or(""),
                            &msg.content,
                        );
                    }
                    _ => {}
                }
            }
        }

        if self.cancel_streaming {
            self.active_page = PanePage::remove(&mut self.pages, "interact", self.active_page);
            if let Some(idx) = cur_idx
                && !self.messages[idx].content.ends_with("\n[cancelled]")
            {
                self.messages[idx].content.push_str("\n[cancelled]");
            }
        }

        if let Some(idx) = cur_idx {
            self.renderer
                .finalize_streaming_message(&self.messages[idx].content, term)?;
        }
        self.streaming = false;
        self.turn_start = None;
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;
        self.cancel_streaming = false;
        self.last_ctrl_c = None;
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        // Single blank line between the last message and the input field. Deduped
        // against the next turn's leading blank, so spacing stays single.
        self.renderer.flush_separator(term)?;
        // Safe now: the turn is over, no tool is running Lua lock-free.
        self.redraw(term)?;
        Ok(())
    }

    /// Apply one `RuntimeEvent` to the TUI's rendering state (used by the pump's
    /// select loop and its post-loop drain).
    fn pump_apply_event(
        &mut self,
        ev: crate::runtime::RuntimeEvent,
        cur_idx: &mut Option<usize>,
        pending: &mut std::collections::HashMap<String, crate::tools::ToolCall>,
        pending_interacts: &mut std::collections::HashMap<
            u64,
            tokio::sync::oneshot::Receiver<serde_json::Value>,
        >,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        use crate::runtime::RuntimeEvent;
        match ev {
            RuntimeEvent::TextDelta { text } => {
                let idx = self.pump_ensure_assistant(cur_idx);
                self.messages[idx].content.push_str(&text);
                self.renderer
                    .flush_streaming_message(&self.messages[idx].content, term)?;
            }
            RuntimeEvent::ReasoningDelta { .. } => {
                // Reasoning is retained in the Driver transcript; the current
                // TUI has no visible reasoning pane yet.
            }
            RuntimeEvent::TokenUsage { sent, received } => {
                // Driver returns authoritative token_stats at turn end; update
                // the live copy so the status bar can reflect usage before the
                // outcome is reabsorbed.
                self.token_stats.sent = sent;
                self.token_stats.received = received;
                self.pump_tick(term)?;
            }
            RuntimeEvent::Status { .. }
            | RuntimeEvent::Started { .. }
            | RuntimeEvent::Finished { .. }
            | RuntimeEvent::Failed { .. } => {}
            RuntimeEvent::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                // Ensure an assistant message precedes the tool row so the
                // user→assistant→tool transition doesn't add an extra blank
                // line (msg_to_lines only blanks across a User boundary).
                self.pump_ensure_assistant(cur_idx);
                pending.insert(
                    id.clone(),
                    crate::tools::ToolCall {
                        id,
                        name,
                        arguments,
                    },
                );
            }
            RuntimeEvent::ToolResult {
                name,
                call_id,
                is_error,
                content,
            } => {
                if let Some(idx) = cur_idx.take() {
                    self.renderer
                        .finalize_streaming_message(&self.messages[idx].content, term)?;
                    // Streamed assistant text has no trailing blank; add one so
                    // the tool row below doesn't touch it (deduped → single).
                    self.renderer.flush_separator(term)?;
                }
                let result = crate::tools::ToolResult {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    content,
                    is_error,
                    pane_page: None,
                    state: None,
                };
                // Skip the row if a preview already showed it (edit_file).
                if self.shown_tool_rows.remove(&call_id) {
                    // preview already rendered the row + diff
                } else if let Some(call) = pending.remove(&call_id) {
                    let display = self.tools.display_for_call(&call);
                    self.messages.push(build_tool_row(&call, &result, display));
                } else {
                    self.messages.push(Message::tool_row(name, is_error));
                }
                self.renderer
                    .flush_new_to_scrollback(&self.messages, term)?;
            }
            RuntimeEvent::Pane { pane } => {
                if pane.is_empty() {
                    self.active_page =
                        PanePage::remove(&mut self.pages, &pane.source, self.active_page);
                } else {
                    let page = PanePage::from_content(&pane);
                    let (_, a) = PanePage::upsert(&mut self.pages, self.active_page, page);
                    self.active_page = a;
                    self.panes_visible = true;
                }
                self.pump_tick(term)?;
            }
            RuntimeEvent::Interact { spec } => {
                let (reply, rx) = tokio::sync::oneshot::channel();
                let req = crate::pane_content::InteractRequest {
                    question: spec.question,
                    mode: spec.mode,
                    options: spec.options,
                    default_selected: spec.default_selected,
                    allow_custom: spec.allow_custom,
                    reply,
                };
                let page = PanePage::from_interact(req);
                let (_, active) = PanePage::upsert(&mut self.pages, self.active_page, page);
                self.active_page = active;
                self.panes_visible = true;
                pending_interacts.insert(spec.id, rx);
                self.pump_tick(term)?;
            }
        }
        Ok(())
    }

    fn pump_drain_interact_replies(
        &mut self,
        registry: &crate::runtime::ReplyRegistry,
        pending_interacts: &mut std::collections::HashMap<
            u64,
            tokio::sync::oneshot::Receiver<serde_json::Value>,
        >,
    ) -> bool {
        let mut cancelled = false;
        pending_interacts.retain(|id, rx| match rx.try_recv() {
            Ok(value) => {
                if value
                    .get("cancelled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    cancelled = true;
                }
                registry.resolve(*id, value);
                false
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => true,
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                registry.resolve(*id, serde_json::json!({ "cancelled": true }));
                cancelled = true;
                false
            }
        });
        cancelled
    }

    /// Get (creating if needed) the current streaming assistant message index,
    /// resetting the streaming flush counters for a fresh message. Creating it
    /// also fixes the user→tool blank-line gap (the placeholder's `Assistant`
    /// role suppresses the role-change blank before a tool row).
    fn pump_ensure_assistant(&mut self, cur_idx: &mut Option<usize>) -> usize {
        match *cur_idx {
            Some(i) => i,
            None => {
                self.messages.push(Message::assistant(String::new()));
                self.renderer.streaming_source_flushed = 0;
                self.renderer.streaming_lines_flushed = 0;
                self.renderer.scrollback_cursor += 1;
                let i = self.messages.len() - 1;
                *cur_idx = Some(i);
                i
            }
        }
    }

    /// Render the `edit_file` diff preview to scrollback (a tool row + the
    /// unified diff), mirroring the non-Driver path's `prepare_tool_call`. The
    /// call id is recorded in `shown_tool_rows` so the later `ToolResult` event
    /// doesn't render a duplicate row. (The Driver executes the edit itself, so
    /// unlike the old path we can't inject `expected_hash` — the preview here is
    /// purely for display.)
    async fn pump_show_edit_preview(
        &mut self,
        call: &ToolCall,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let preview = match preview_edit_file(call.arguments.clone()).await {
            Ok(p) => p,
            Err(_) => return Ok(()), // execution will surface the real error
        };
        let show_row = self
            .tools
            .display_for_call(call)
            .and_then(|d| d.show)
            .unwrap_or(true);
        if show_row {
            let placeholder = ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: String::new(),
                is_error: false,
                pane_page: None,
                state: None,
            };
            self.messages.push(build_tool_row(
                call,
                &placeholder,
                self.tools.display_for_call(call),
            ));
            self.shown_tool_rows.insert(call.id.clone());
        }
        self.messages.push(Message::system(preview.diff));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        Ok(())
    }

    /// Redraw the bottom pane during the pump WITHOUT touching the Lua VM
    /// (`tick_spinner` does not call `apply_view_diffs`), so it is safe to call
    /// while a Lua tool runs lock-free.
    fn pump_tick(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        self.renderer.tick_spinner(
            term,
            &PaneDraw {
                input: &self.input,
                status_info: &self.status_info(),
                pages: if self.panes_visible { &self.pages } else { &[] },
                active_page: self.active_page,
                autocomplete: None,
            },
        )
    }

    async fn run_inline_command(&mut self, cmd: &str, term: &mut BoneTerminal) -> io::Result<()> {
        use crate::tools::command_policy::classify_command;

        let safety = classify_command(cmd);
        let classification = match safety {
            crate::tools::command_policy::CommandSafety::ReadOnly => "read_only",
            crate::tools::command_policy::CommandSafety::Danger => "danger",
        };

        let result = ShellTool
            .execute(serde_json::json!({
                "command": cmd,
                "classification": classification,
                "timeout_ms": 60_000,
            }))
            .await
            .unwrap_or_else(|e| format!("[error: {e}]"));

        let is_error = result.contains("exit code: 1") || result.contains("timed out");
        let display = format!("{cmd}\n{result}");
        self.input.reset();
        self.messages
            .push(Message::terminal_output(cmd.to_string(), display, is_error));
        let transcript_text =
            crate::tools::shell::truncate_output(&format!("$ {cmd}\n{result}"), 500);
        self.transcript
            .push(ChatMessage::new(ChatRole::User, &transcript_text));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)?;
        Ok(())
    }

    /// Dispatch the `before_turn` hook on a blocking thread while keeping the
    /// UI responsive (spinner animation, input draining, Esc-to-cancel).
    ///
    /// Handlers may block on LLM calls (e.g. auto-compaction via
    /// `ctx.agent.run`), so running them on the event-loop thread would freeze
    /// the whole app for the duration. A cancel flag is threaded into the ctx
    /// so pressing Esc aborts an in-flight compaction promptly.
    fn apply_tool_live_event(&mut self, event: ToolLiveEvent) {
        match event {
            ToolLiveEvent::Pane(pc) => {
                if pc.is_empty() {
                    self.active_page =
                        PanePage::remove(&mut self.pages, &pc.source, self.active_page);
                } else {
                    let page = PanePage::from_content(&pc);
                    let (_, active) = PanePage::upsert(&mut self.pages, self.active_page, page);
                    self.active_page = active;
                }
            }
            ToolLiveEvent::Interact(req) => {
                let page = PanePage::from_interact(req);
                let (_, active) = PanePage::upsert(&mut self.pages, self.active_page, page);
                self.active_page = active;
            }
        }
    }

    /// Apply a live event and record which pane sources are currently shown by
    /// the running tool, so they can be cleaned up if the tool is cancelled
    /// before it emits its own removal (empty-content) event. Pane sources
    /// managed outside this flow (e.g. the Rust subagent pane) never pass
    /// through here and so are never tracked or removed.
    fn apply_and_track(
        &mut self,
        event: ToolLiveEvent,
        live_sources: &mut std::collections::HashSet<String>,
    ) {
        match &event {
            ToolLiveEvent::Pane(pc) => {
                if pc.is_empty() {
                    live_sources.remove(&pc.source);
                } else {
                    live_sources.insert(pc.source.clone());
                }
            }
            ToolLiveEvent::Interact(_) => {
                live_sources.insert("interact".to_string());
            }
        }
        self.apply_tool_live_event(event);
    }

    /// Drive a blocking-Lua future to completion while keeping the UI live:
    /// pump pane events emitted via the `ToolLiveEvent` sender, drain
    /// keystrokes into any active interactive pane (this is what unblocks
    /// `ctx.ui.interact`), render the spinner, and clean up panes on cancel.
    ///
    /// Generic over the future's output `T` so both model-invoked tools and
    /// slash commands share one execution loop. `on_cancel` produces the
    /// return value when the user cancels with Esc.
    pub(super) async fn drive_live<T, F, Fut>(
        &mut self,
        make_future: F,
        term: &mut BoneTerminal,
        cancel_token: std::sync::Arc<std::sync::atomic::AtomicBool>,
        on_cancel: impl FnOnce() -> T,
    ) -> io::Result<T>
    where
        F: FnOnce(mpsc::UnboundedSender<ToolLiveEvent>) -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let mut spinner = time::interval(Duration::from_millis(90));
        let (tx, mut rx) = mpsc::unbounded_channel::<ToolLiveEvent>();
        let future = make_future(tx);
        pin_mut!(future);
        // Pane sources currently shown by the running tool. Used only to clean
        // up lingering panes if the tool is cancelled before emitting its own
        // removal event.
        let mut live_sources = std::collections::HashSet::<String>::new();

        loop {
            tokio::select! {
                results = &mut future => {
                    while let Ok(event) = rx.try_recv() {
                        self.apply_and_track(event, &mut live_sources);
                    }
                    return Ok(results);
                }
                Some(event) = rx.recv() => {
                    self.apply_and_track(event, &mut live_sources);
                }
                _ = spinner.tick() => {
                    if Self::drain_keys(
                        &mut self.input,
                        &mut self.queue,
                        &mut self.approval_mode,
                        &mut self.cancel_streaming,
                        &mut self.panes_visible,
                        &mut self.pages,
                        &mut self.active_page,
                    ) {
                        self.user_config.approval_mode = self.approval_mode;
                        self.persist_runtime_config();
                    }
                    if self.cancel_streaming {
                        // Signal cancellation to any running subagents.
                        cancel_token.store(true, std::sync::atomic::Ordering::Relaxed);
                        // Drain remaining live events before returning so that
                        // any pending pane updates are processed.
                        while let Ok(event) = rx.try_recv() {
                            self.apply_and_track(event, &mut live_sources);
                        }
                        // Remove any panes the cancelled tool left behind — its
                        // future was dropped, so it can't emit its own removal.
                        for source in live_sources.drain() {
                            self.active_page =
                                PanePage::remove(&mut self.pages, &source, self.active_page);
                        }
                        return Ok(on_cancel());
                    }

                    // Refresh subagent pane on registry change or ~1s ticker, so the
                    // pane stays live while a tool blocks (e.g. subagent wait=true).
                    self.maybe_refresh_subagent_pane();
                    let visible_pages = if self.panes_visible {
                    // Drain any pane events sent during drain_keys (e.g. a
                    // multi-question ask_user replacing one question with the
                    // next). Avoids a one-frame flash of the dead pane.
                    while let Ok(event) = rx.try_recv() {
                        self.apply_and_track(event, &mut live_sources);
                    }
                        self.pages.as_slice()
                    } else {
                        &[]
                    };
                    self.renderer.tick_spinner(
                        term,
                        &PaneDraw {
                            input: &self.input,
                            status_info: &self.status_info(),
                            pages: visible_pages,
                            active_page: self.active_page,
                            autocomplete: None,
                        },
                    )?;                }
            }
        }
    }

    pub(crate) fn estimate_context_chars(
        history: &[ChatMessage],
        tools: &[crate::tools::ToolDefinition],
    ) -> usize {
        let message_chars: usize = history
            .iter()
            .map(|message| {
                message.content.chars().count()
                    + message
                        .reasoning
                        .as_ref()
                        .map_or(0, |r| r.text.chars().count())
                    + serde_json::to_string(&message.tool_calls)
                        .map(|json| json.chars().count())
                        .unwrap_or(0)
                    + message
                        .tool_call_id
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
                    + message
                        .name
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
            })
            .sum();
        let tool_chars = serde_json::to_string(tools)
            .map(|json| json.chars().count())
            .unwrap_or(0);
        message_chars + tool_chars
    }

    /// Drain pending key events into input edits or queue. Used during streaming.
    fn drain_keys(
        input: &mut InputState,
        queue: &mut VecDeque<String>,
        mode: &mut ApprovalMode,
        cancel: &mut bool,
        panes_visible: &mut bool,
        pages: &mut Vec<PanePage>,
        active_page: &mut usize,
    ) -> bool {
        let mut mode_changed = false;
        while event::poll(std::time::Duration::from_millis(0)).unwrap_or(false) {
            match event::read().unwrap_or(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Null,
                KeyModifiers::NONE,
            ))) {
                Event::Paste(text) => input.insert_paste(&text),
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if key.code == KeyCode::Char('t')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        *panes_visible = !*panes_visible;
                        continue;
                    }
                    // ── Interactive pane key handling (before navigation) ───
                    if *panes_visible
                        && PanePage::dispatch_interaction_key(
                            pages,
                            active_page,
                            key.code,
                            key.modifiers,
                        )
                    {
                        continue;
                    }
                    // ── Page navigation (Tab/BackTab/PageUp/PageDown) ─────
                    if *panes_visible && !pages.is_empty() {
                        *active_page = (*active_page).min(pages.len() - 1);
                        match (key.code, key.modifiers) {
                            (KeyCode::Tab, m) if m.is_empty() => {
                                *active_page = (*active_page + 1) % pages.len();
                                continue;
                            }
                            (KeyCode::BackTab, m) if m.is_empty() => {
                                *active_page = if *active_page == 0 {
                                    pages.len() - 1
                                } else {
                                    *active_page - 1
                                };
                                continue;
                            }
                            _ => {}
                        }
                        let page = &mut pages[*active_page];
                        match (key.code, key.modifiers) {
                            (KeyCode::PageUp, m) if m.is_empty() => {
                                page.scroll =
                                    page.scroll.saturating_sub(crate::ui::render::MAX_PANE_ROWS);
                                continue;
                            }
                            (KeyCode::PageDown, m) if m.is_empty() => {
                                let max_scroll = page.content.len().saturating_sub(
                                    crate::ui::render::clamped_pane_visible_rows(page.visible_rows),
                                );
                                page.scroll = (page.scroll + crate::ui::render::MAX_PANE_ROWS)
                                    .min(max_scroll);
                                continue;
                            }
                            (KeyCode::Up, m) if m.contains(KeyModifiers::CONTROL) => {
                                page.scroll = page.scroll.saturating_sub(1);
                                continue;
                            }
                            (KeyCode::Down, m) if m.contains(KeyModifiers::CONTROL) => {
                                let max_scroll = page.content.len().saturating_sub(
                                    crate::ui::render::clamped_pane_visible_rows(page.visible_rows),
                                );
                                page.scroll = (page.scroll + 1).min(max_scroll);
                                continue;
                            }
                            _ => {}
                        }
                    }
                    let mut next = Some(Event::Key(key));
                    while let Some(event) = next {
                        next = None;
                        match event {
                            Event::Paste(text) => input.insert_paste(&text),
                            Event::Key(key) if key.kind == KeyEventKind::Press => {
                                let result = match apply_input_key_with_paste_burst(input, key) {
                                    Ok(result) => result,
                                    Err(_) => return mode_changed,
                                };
                                next = result.trailing;
                                match result.action {
                                    InputAction::Cancel => {
                                        // If an interactive pane is active, cancel it instead of cancelling the stream.
                                        if *panes_visible {
                                            let active_idx =
                                                (*active_page).min(pages.len().saturating_sub(1));
                                            let cancelled =
                                                pages.get(active_idx).is_some_and(|page| {
                                                    page.interaction.as_ref().is_some_and(|i| {
                                                        if i.is_active() {
                                                            i.cancel();
                                                            true
                                                        } else {
                                                            false
                                                        }
                                                    })
                                                });
                                            if cancelled {
                                                continue;
                                            }
                                        }
                                        *cancel = true;
                                        queue.clear();
                                        return mode_changed;
                                    }
                                    InputAction::Submit => {
                                        // Expand placeholders now; the queued string is fed
                                        // back through send_message later with no blobs.
                                        let text = input.expanded().trim().to_string();
                                        if !text.is_empty() {
                                            queue.push_back(text);
                                            input.reset();
                                        }
                                    }
                                    InputAction::ClearQueue => queue.clear(),
                                    InputAction::CycleMode => {
                                        *mode = mode.cycle();
                                        mode_changed = true;
                                    }
                                    InputAction::Redraw
                                    | InputAction::Escape
                                    | InputAction::OpenEditor
                                    | InputAction::None => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        mode_changed
    }
}
