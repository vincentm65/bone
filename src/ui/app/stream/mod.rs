use crate::chat::{Message, build_chat_history};
use crate::llm::{ChatMessage, ChatRole};
use crate::tools::edit_file::preview_edit_file;
use crate::tools::shell::ShellTool;
use crate::tools::types::ToolLiveEvent;
use crate::tools::{ApprovalMode, Tool, ToolCall, ToolResult};
use crate::ui::input::{InputAction, InputState};
use crate::ui::pane_page::PanePage;
use crate::ui::render::{BoneTerminal, PaneDraw};
use crate::ui::tool_display::build_tool_row;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::pin_mut;
use std::collections::VecDeque;
use std::io;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};

use super::{App, apply_input_key_with_paste_burst};

/// One place that resolves a `KeyEvent` to whichever caller is blocked
/// waiting for one. A pending key request can come from two sources:
///   - `Direct`: a `ctx.ui.key()` call inside a blocking Lua tool, delivered
///     via the `ToolLiveEvent` channel (the future holds a `oneshot` sender).
///   - `Runtime`: the Driver requested a key via `RuntimeEvent::KeyRequest`,
///     resolved by id through the `KeyReplyRegistry`.
/// Both code paths (`drive_live`, `stream_runtime`) own a `KeySink` and pass
/// it to `drain_keys`; a key event from the terminal is delivered here.
pub(crate) struct KeySink {
    pending: Option<PendingKeyReply>,
    /// Keys read from the terminal while no reply slot was registered, held for
    /// the tool's next key request. A single `drain_keys` pass can read several
    /// keystrokes (fast typing) but only one reply slot exists at a time; the
    /// tool re-arms via the channel only after `drain_keys` returns. Without
    /// this buffer the extra keys leak into the main chat input.
    buffer: std::collections::VecDeque<crate::pane_content::KeyEvent>,
    /// Latched true once a tool has requested at least one key, marking the tool
    /// as the input owner so subsequent keys buffer instead of falling through.
    owns_input: bool,
}

enum PendingKeyReply {
    Direct(tokio::sync::oneshot::Sender<crate::pane_content::KeyEvent>),
    Runtime {
        id: u64,
        registry: crate::runtime::KeyReplyRegistry,
    },
}

impl KeySink {
    pub fn new() -> Self {
        Self {
            pending: None,
            buffer: std::collections::VecDeque::new(),
            owns_input: false,
        }
    }

    /// True when a key from `drain_keys` should be routed to the tool — either a
    /// reply slot is armed, or a tool owns input and the key can be buffered for
    /// its next request. When false, keys fall through to the main chat input.
    pub fn wants_key(&self) -> bool {
        self.pending.is_some() || self.owns_input
    }

    /// Resolve a freshly registered reply slot from the buffer if a key is
    /// already waiting; otherwise store `reply` as the pending slot.
    fn arm(&mut self, reply: PendingKeyReply) {
        self.owns_input = true;
        match self.buffer.pop_front() {
            Some(key) => match reply {
                PendingKeyReply::Direct(tx) => {
                    let _ = tx.send(key);
                }
                PendingKeyReply::Runtime { id, registry } => {
                    registry.resolve(id, key);
                }
            },
            None => self.pending = Some(reply),
        }
    }

    /// Register a direct oneshot channel (from `ctx.ui.key()` via the
    /// `ToolLiveEvent` channel transport).
    pub fn set_direct(&mut self, tx: tokio::sync::oneshot::Sender<crate::pane_content::KeyEvent>) {
        self.arm(PendingKeyReply::Direct(tx));
    }

    /// Register a runtime key request (from `RuntimeEvent::KeyRequest`).
    pub fn set_runtime(&mut self, id: u64, registry: crate::runtime::KeyReplyRegistry) {
        self.arm(PendingKeyReply::Runtime { id, registry });
    }

    /// Route `key` to the tool. Delivers to the pending reply slot if armed;
    /// otherwise, if a tool owns input, buffers it for the next request. Returns
    /// true if the key was consumed (delivered or buffered), false if it should
    /// fall through to the main chat input.
    pub fn deliver(&mut self, key: crate::pane_content::KeyEvent) -> bool {
        match self.pending.take() {
            Some(PendingKeyReply::Direct(tx)) => {
                let _ = tx.send(key);
                true
            }
            Some(PendingKeyReply::Runtime { id, registry }) => {
                registry.resolve(id, key);
                true
            }
            None if self.owns_input => {
                self.buffer.push_back(key);
                true
            }
            None => false,
        }
    }

    /// Release input ownership when the owning tool finishes. `owns_input`
    /// latches across the gaps between a tool's successive key requests (so
    /// keys typed mid-loop buffer instead of leaking to chat); without this
    /// reset it would stay latched for the rest of the turn, swallowing every
    /// keystroke after the tool returns. Called from the `ToolResult` branch —
    /// the only reliable "tool done, no re-arm coming" signal. Drops any
    /// buffered keys so they can't bleed into a later tool's key request.
    pub fn clear_owner(&mut self) {
        self.owns_input = false;
        self.buffer.clear();
    }
}

/// Tracks which pane component ids a blocking tool has opened, so they can be
/// removed if the tool is cancelled mid-block (its future is dropped, so it
/// can't emit its own removal `ViewDiff::Remove`). Fed by `track` from the
/// `drain_view_diffs` loop in `drive_live`; cleaned up by `drain_for_cancel`.
pub(crate) struct PaneOwnership {
    sources: std::collections::HashSet<String>,
}

impl PaneOwnership {
    pub fn new() -> Self {
        Self {
            sources: std::collections::HashSet::new(),
        }
    }

    /// Record the component id from a `ViewDiff::Upsert` or clear it on
    /// `ViewDiff::Remove`. `SetHighlight` carries no pane ownership.
    pub fn track(&mut self, diff: &crate::runtime::view::ViewDiff) {
        use crate::runtime::view::ViewDiff;
        match diff {
            ViewDiff::Upsert { component } => {
                self.sources.insert(component.id().to_string());
            }
            ViewDiff::Remove { id } => {
                self.sources.remove(id);
            }
            ViewDiff::SetHighlight { .. } => {}
        }
    }

    /// Remove every owned pane from `pages`. Called on cancel.
    pub fn drain_for_cancel(&mut self, pages: &mut Vec<PanePage>, active_page: &mut usize) {
        for source in self.sources.drain() {
            *active_page = PanePage::remove(pages, &source, *active_page);
        }
    }
}

fn key_event_from_crossterm(
    code: KeyCode,
    modifiers: KeyModifiers,
) -> crate::pane_content::KeyEvent {
    let (name, ch) = match code {
        KeyCode::Backspace => ("Backspace".to_string(), None),
        KeyCode::Enter => ("Enter".to_string(), None),
        KeyCode::Left => ("Left".to_string(), None),
        KeyCode::Right => ("Right".to_string(), None),
        KeyCode::Up => ("Up".to_string(), None),
        KeyCode::Down => ("Down".to_string(), None),
        KeyCode::Home => ("Home".to_string(), None),
        KeyCode::End => ("End".to_string(), None),
        KeyCode::PageUp => ("PageUp".to_string(), None),
        KeyCode::PageDown => ("PageDown".to_string(), None),
        KeyCode::Tab => ("Tab".to_string(), None),
        KeyCode::BackTab => ("BackTab".to_string(), None),
        KeyCode::Delete => ("Delete".to_string(), None),
        KeyCode::Insert => ("Insert".to_string(), None),
        KeyCode::Esc => ("Esc".to_string(), None),
        KeyCode::Char(c) => ("Char".to_string(), Some(c.to_string())),
        KeyCode::F(n) => (format!("F{n}"), None),
        KeyCode::Null => ("Null".to_string(), None),
        KeyCode::CapsLock => ("CapsLock".to_string(), None),
        KeyCode::ScrollLock => ("ScrollLock".to_string(), None),
        KeyCode::NumLock => ("NumLock".to_string(), None),
        KeyCode::PrintScreen => ("PrintScreen".to_string(), None),
        KeyCode::Pause => ("Pause".to_string(), None),
        KeyCode::Menu => ("Menu".to_string(), None),
        KeyCode::KeypadBegin => ("KeypadBegin".to_string(), None),
        KeyCode::Media(_) => ("Media".to_string(), None),
        KeyCode::Modifier(_) => ("Modifier".to_string(), None),
    };
    crate::pane_content::KeyEvent {
        code: name,
        char: ch,
        ctrl: modifiers.contains(KeyModifiers::CONTROL),
        alt: modifiers.contains(KeyModifiers::ALT),
        shift: modifiers.contains(KeyModifiers::SHIFT),
    }
}

pub fn tool_error(call: &ToolCall, content: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        content: content.into(),
        images: Vec::new(),
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
        if text.is_empty() && !self.input.has_images() {
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
        let display_text = (self.input.has_pastes() || self.input.has_images())
            .then(|| self.input.buffer.trim().to_string());
        let images = self.input.take_images();
        self.submit_user_turn(text, display_text, images, term)
            .await
    }

    /// Run the turn through the core `Driver` and render its `RuntimeEvent`
    /// stream, answering approval/key requests over channels. The Driver owns
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
        images: Vec<crate::llm::ImageData>,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        use crate::runtime::{
            ApprovalRequest, ChannelApprovalGate, Driver, KeyReplyRegistry, RuntimeEvent,
        };
        use crate::session_sink::{NullSessionSink, SessionSink};
        use crate::tools::CallOutcome;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // User message + DB persistence.
        let image_count = images.len();
        self.messages.push(Message::user_with_images(
            display_text.as_deref().unwrap_or(&text),
            image_count,
        ));
        if images.is_empty() {
            self.transcript
                .push(ChatMessage::new(ChatRole::User, &text));
            self.append_user_to_db(&text, None);
        } else {
            let images_json = serde_json::to_string(&images).ok();
            self.transcript
                .push(ChatMessage::user_with_images(&text, images));
            self.append_user_to_db(&text, images_json.as_deref());
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
        // Seed the live output-token estimate from the running total so the
        // status bar ticks up from where it left off as text/tools stream in.
        self.stream_estimated_received = Some(self.token_stats.received);
        self.turn_start = Some(std::time::Instant::now());
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;

        let (rt_tx, mut rt_rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
        let (appr_tx, mut appr_rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalRequest>();
        let key_registry = KeyReplyRegistry::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let session: Arc<dyn SessionSink> = Arc::new(NullSessionSink);

        // Shared with the Driver so a mid-turn Safe/Danger toggle is observed on
        // its next tool batch. Must be interior-mutable — a plain Arc can't be
        // mutated once the Driver holds a clone.
        let approval_mode = crate::tools::SharedApprovalMode::new(self.approval_mode);
        let approval_mode_sync = approval_mode.clone();

        let driver = Driver {
            llm: self.llm.clone(),
            extensions: self.extensions.clone(),
            tools: self.tools.clone(),
            session,
            gate: Arc::new(ChannelApprovalGate::new(appr_tx)),
            approval_mode,
            agent_depth: 0,
            activity: None,
            on_token_usage: None,
            events: false,
            event_sender: None,
            runtime_events: Some(rt_tx),
            key_reply_registry: Some(key_registry.clone()),
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
        let mut pending_key = KeySink::new();
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(90));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let outcome = loop {
            // Always drain keys before polling Driver/events. A fast stream can
            // otherwise starve a timer branch and make Esc lag until a lull.
            // While an approval prompt is up, keys drive the prompt instead of
            // the input field — but the loop keeps pumping the spinner/events.
            if self.pending_approval.is_some() {
                self.drain_approval_keys(term)?;
            } else if Self::drain_keys(
                &mut self.input,
                &mut self.queue,
                &mut self.approval_mode,
                &mut self.cancel_streaming,
                &mut self.panes_visible,
                &mut self.pages,
                &mut self.active_page,
                &mut pending_key,
            ) {
                approval_mode_sync.set(self.approval_mode);
                self.user_config.approval_mode = self.approval_mode;
                self.persist_runtime_config();
            }
            if self.cancel_streaming {
                cancel.store(true, Ordering::Relaxed);
                break None;
            }

            tokio::select! {
                outcome = &mut run_fut => break Some(outcome),
                Some(ev) = rt_rx.recv() => {
                    self.pump_apply_event(ev, &mut cur_idx, &mut pending, &key_registry, &mut pending_key, term)?;
                }
                // Don't pull a new approval while one is still pending — tool
                // calls are resolved one at a time, as they were when this was
                // a synchronous prompt.
                Some(areq) = appr_rx.recv(), if self.pending_approval.is_none() => {
                    // Show the edit-file diff preview before deciding, so the
                    // user sees what's being changed (in Safe and Danger modes).
                    if areq.call.name == "edit_file" {
                        self.pump_show_edit_preview(&areq.call, term).await?;
                    }
                    if areq.auto_allows {
                        let _ = areq.reply.send(CallOutcome::Approve);
                    } else {
                        // Show the prompt; the decision is collected by
                        // drain_approval_keys at the top of the loop.
                        self.begin_approval(&areq.call, areq.reply, term)?;
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
            self.pump_apply_event(
                ev,
                &mut cur_idx,
                &mut pending,
                &key_registry,
                &mut pending_key,
                term,
            )?;
        }

        // Reabsorb authoritative state from the Driver when it completed.
        let had_outcome = outcome.is_some();
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
                    // Synthetic user messages relaying tool-returned images.
                    ChatRole::User => {
                        let images = (!msg.images.is_empty())
                            .then(|| serde_json::to_string(&msg.images).ok())
                            .flatten();
                        self.append_user_to_db(&msg.content, images.as_deref());
                    }
                    _ => {}
                }
            }

            // Persist usage events the Driver reported through its (null) sink.
            for usage in &outcome.usage {
                self.record_usage_to_db(usage);
            }

            // Surface a driver/provider failure so the turn never ends in
            // silence. The Driver aborts the turn (e.g. an HTTP 429/5xx after
            // its retries, possibly mid tool-loop) by returning `Err`; without
            // rendering it the TUI just stops with no output and looks like the
            // agent hung mid-loop. `RuntimeEvent::Failed` is intentionally not
            // drawn live — this is the single authoritative place we report it.
            if let Err(err) = &outcome.result {
                self.messages
                    .push(Message::system(format!("⚠ turn failed: {err}")));
            }
        }

        // On cancellation the driver's authoritative token_stats were
        // discarded — the transcript reverted to pre-turn state (plus the
        // user message), but context_length still holds the last completed
        // request's value. Re-estimate from the current transcript so the
        // displayed `curr` and the next turn's compaction check see the real
        // context size instead of a stale overestimate.
        if !had_outcome {
            let history = build_chat_history(&self.transcript, None);
            let prompt_chars = Self::estimate_context_chars(&history, &self.tools.definitions());
            self.token_stats.set_context_estimate(prompt_chars);
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
        // Defensive teardown: a tool-only or failed turn never emits TextDelta,
        // so clear the live thinking pane here too.
        self.clear_thinking_pane();
        self.streaming = false;
        // Authoritative token_stats are now reabsorbed; drop the live estimate
        // so the status bar shows the real `received` count.
        self.stream_estimated_received = None;
        self.turn_start = None;
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;
        self.cancel_streaming = false;
        self.last_ctrl_c = None;
        // Drop any unresolved approval (dropping the reply makes the gate fall
        // back to its non-interactive decision); clear the prompt UI too.
        self.pending_approval = None;
        self.active_prompt = None;
        self.clear_approval_pane();
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
        key_registry: &crate::runtime::KeyReplyRegistry,
        pending_key: &mut KeySink,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        use crate::runtime::RuntimeEvent;
        match ev {
            RuntimeEvent::TextDelta { text } => {
                // The answer is starting — fade out the live thinking pane,
                // honoring its minimum on-screen retention.
                self.fade_thinking_pane();
                let idx = self.pump_ensure_assistant(cur_idx);
                self.bump_estimated_received(text.len());
                self.messages[idx].content.push_str(&text);
                self.renderer
                    .flush_streaming_message(&self.messages[idx].content, term)?;
                // On Windows, `insert_before` uses the no-scrolling-regions path
                // which clears the inline viewport; repaint it immediately so the
                // status bar doesn't flicker between this flush and the next tick.
                #[cfg(windows)]
                self.pump_tick(term)?;
            }
            RuntimeEvent::ReasoningDelta { text } => {
                // Reasoning is always retained in the Driver transcript for
                // echo-back; here we optionally surface it live. When disabled
                // (default), drop it — only the spinner shows.
                if self.user_config.show_thinking {
                    self.push_thinking(&text);
                    self.pump_tick(term)?;
                }
            }
            RuntimeEvent::TokenUsage {
                sent,
                received,
                context_length,
            } => {
                // Driver returns authoritative token_stats at turn end; update
                // the live copy so the status bar can reflect usage before the
                // outcome is reabsorbed. `context_length` (the `curr` metric)
                // must update after every request so compaction sees the real
                // context size mid-turn, not just at turn end.
                self.token_stats.sent = sent;
                self.token_stats.received = received;
                self.token_stats.context_length = context_length;
                // Real count arrived for this request — rebaseline the live
                // estimate so further deltas (next request in the tool loop)
                // tick up from the authoritative total instead of the guess.
                self.stream_estimated_received = Some(received);
                self.pump_tick(term)?;
            }
            RuntimeEvent::Status { message } => {
                // Most status lines are already covered by other UI (the
                // spinner for "thinking", tool rows for "running …"). Surface
                // only the host-generated signals that would otherwise be
                // invisible: retries / stream errors. Lua that wants a message
                // kept in the transcript emits `RuntimeEvent::Notice` instead
                // (see below) rather than relying on the host to guess.
                if message.starts_with("retry") || message.contains("stream error") {
                    self.pump_notice(format!("⚠ {message}"), cur_idx, term)?;
                }
            }
            RuntimeEvent::Notice { message } => {
                // A persistent notice from Lua (e.g. auto-compaction via
                // compact.lua): always keep it in the scrollback.
                self.pump_notice(message, cur_idx, term)?;
            }
            // `Failed` stays authoritative at turn end (rendered from
            // `outcome.result`); surfacing the retry/stream-error `Status`
            // events above is enough to break the silent-hang illusion.
            RuntimeEvent::Failed { .. }
            | RuntimeEvent::Started { .. }
            | RuntimeEvent::Finished { .. } => {}
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
                // Tool-call arguments are part of the completion the model
                // generated, so count them toward the live estimate too.
                let arg_len = serde_json::to_string(&arguments)
                    .map(|s| s.len())
                    .unwrap_or(0);
                self.bump_estimated_received(arg_len);
                let call = crate::tools::ToolCall {
                    id: id.clone(),
                    name,
                    arguments,
                };
                // Tools that declare `display.eager` (e.g. `subagent`, whose
                // dispatch/wait calls block until the agents finish) would
                // otherwise only show their row on completion. Render it now;
                // the id is recorded in `shown_tool_rows` so the later
                // `ToolResult` event doesn't render a duplicate.
                if self
                    .tools
                    .display_for_call(&call)
                    .and_then(|d| d.eager)
                    .unwrap_or(false)
                {
                    self.pump_show_eager_row(&call, cur_idx, term)?;
                }
                pending.insert(id, call);
            }
            RuntimeEvent::ToolResult {
                name,
                call_id,
                is_error,
                content,
            } => {
                // The tool finished, so it won't re-arm a key request — release
                // input ownership latched by any `ctx.ui.key()` call, otherwise
                // every later keystroke this turn buffers instead of reaching
                // the chat input.
                pending_key.clear_owner();
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
                    images: Vec::new(),
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
            RuntimeEvent::KeyRequest { id } => {
                pending_key.set_runtime(id, key_registry.clone());
            }
        }
        Ok(())
    }

    /// Push a one-off notice (retry/error) into scrollback mid-turn. Finalizes
    /// any in-progress streamed assistant message first so the notice doesn't
    /// interleave with partial markdown, mirroring the `ToolResult` path.
    fn pump_notice(
        &mut self,
        message: String,
        cur_idx: &mut Option<usize>,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        if let Some(idx) = cur_idx.take() {
            self.renderer
                .finalize_streaming_message(&self.messages[idx].content, term)?;
            self.renderer.flush_separator(term)?;
        }
        self.messages.push(Message::system(message));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        Ok(())
    }

    /// Maximum chars of reasoning retained for the live pane. The pane only
    /// ever shows its tail, so keeping the whole (potentially huge) reasoning
    /// blob would make every delta re-wrap O(n) chars — exactly the quadratic
    /// blowup that makes long-reasoning turns feel frozen. Bounding the buffer
    /// keeps each redraw cheap.
    const THINKING_TAIL_CAP: usize = 4096;
    /// Total pane rows: a "Thinking" header plus the reasoning tail.
    const THINKING_MAX_ROWS: usize = 10;
    /// Minimum time the thinking pane stays on screen once shown, so a quick
    /// reasoning burst doesn't flash away the instant the answer starts.
    const THINKING_RETAIN: Duration = Duration::from_secs(1);

    /// Append reasoning text to the bounded live-pane buffer and refresh the
    /// "thinking" pane. Front-truncates to [`THINKING_TAIL_CAP`] on a char
    /// boundary so multi-byte graphemes never split.
    fn push_thinking(&mut self, text: &str) {
        self.thinking_tail.push_str(text);
        let len = self.thinking_tail.len();
        if len > Self::THINKING_TAIL_CAP {
            let mut cut = len - Self::THINKING_TAIL_CAP;
            while cut < len && !self.thinking_tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.thinking_tail.drain(..cut);
        }
        // Fresh reasoning cancels any pending teardown and starts the clock.
        self.thinking_clear_at = None;
        self.thinking_first_shown.get_or_insert_with(Instant::now);

        use ratatui::style::{Color, Modifier, Style};
        use ratatui::text::Line;
        let grey = Style::default().fg(Color::Gray);
        // Header + the last reasoning lines that fit; the header stays pinned.
        let mut tail: Vec<&str> = self
            .thinking_tail
            .rsplit('\n')
            .take(Self::THINKING_MAX_ROWS - 1)
            .collect();
        tail.reverse();
        let mut content = vec![Line::styled(
            "✻ Thinking",
            grey.add_modifier(Modifier::BOLD),
        )];
        content.extend(tail.iter().map(|l| Line::styled((*l).to_string(), grey)));
        let visible_rows = content.len();
        let page = PanePage {
            source: "thinking".to_string(),
            title: "thinking".to_string(),
            content,
            visible_rows,
            scroll: 0,
        };
        let (_, active) = PanePage::upsert(&mut self.pages, self.active_page, page);
        self.active_page = active;
    }

    /// Tear down the thinking pane, but keep it on screen for at least
    /// [`THINKING_RETAIN`] from when it first appeared. Called when the answer
    /// starts; the pump tick removes it once the retention window elapses.
    fn fade_thinking_pane(&mut self) {
        match self.thinking_first_shown.map(|t| t + Self::THINKING_RETAIN) {
            Some(deadline) if Instant::now() < deadline => self.thinking_clear_at = Some(deadline),
            _ => self.clear_thinking_pane(),
        }
    }

    /// Remove the live "thinking" pane and clear its buffer. Idempotent — a
    /// no-op when no reasoning was shown this turn.
    pub(crate) fn clear_thinking_pane(&mut self) {
        self.thinking_clear_at = None;
        self.thinking_first_shown = None;
        if self.thinking_tail.is_empty() && !self.pages.iter().any(|p| p.source == "thinking") {
            return;
        }
        self.thinking_tail.clear();
        self.active_page = PanePage::remove(&mut self.pages, "thinking", self.active_page);
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
                self.renderer.scrollback_cursor += 1;
                let i = self.messages.len() - 1;
                *cur_idx = Some(i);
                i
            }
        }
    }

    /// Render an `display.eager` tool row to scrollback at dispatch time,
    /// mirroring the `ToolResult` rendering path with a synthetic (empty,
    /// non-error) result. The label is derived purely from the call arguments
    /// (the tool's declared `display`) and such tools hide their result, so
    /// nothing is lost by showing the row before the call finishes. The call id
    /// is recorded in `shown_tool_rows` so the later `ToolResult` event skips
    /// the duplicate.
    fn pump_show_eager_row(
        &mut self,
        call: &ToolCall,
        cur_idx: &mut Option<usize>,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        if let Some(idx) = cur_idx.take() {
            self.renderer
                .finalize_streaming_message(&self.messages[idx].content, term)?;
            self.renderer.flush_separator(term)?;
        }
        let result = crate::tools::ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: String::new(),
            images: Vec::new(),
            is_error: false,
            pane_page: None,
            state: None,
        };
        let display = self.tools.display_for_call(call);
        self.messages.push(build_tool_row(call, &result, display));
        self.shown_tool_rows.insert(call.id.clone());
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        Ok(())
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
        let preview = match preview_edit_file(&call.name, call.arguments.clone()).await {
            Ok(p) => p,
            Err(_) => return Ok(()), // execution will surface the real error
        };
        self.messages.push(Message::system(preview.diff));
        self.shown_tool_rows.insert(call.id.clone());
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        Ok(())
    }

    /// Advance the live output-token estimate using the shared chars-per-token
    /// heuristic, if a turn is in progress. No-op when idle.
    fn bump_estimated_received(&mut self, chars: usize) {
        if let Some(est) = self.stream_estimated_received.as_mut() {
            *est += (chars as f64 / crate::llm::token_tracker::CHARS_PER_TOKEN).round() as u64;
        }
    }

    /// Redraw the bottom pane during the pump. Drains shared UI-state diffs
    /// (v2: safe to call while a Lua tool blocks — the UiState mutex is
    /// separate from the VM mutex), then renders the spinner and panes.
    fn pump_tick(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        // Retire the thinking pane once its retention window has elapsed.
        if self.thinking_clear_at.is_some_and(|d| Instant::now() >= d) {
            self.clear_thinking_pane();
        }
        self.apply_view_diffs();
        self.maybe_refresh_jobs_pane();
        self.render_streaming(term)
    }

    /// Render the bottom pane during a live turn or tool run: spinner-animated,
    /// current panes (when visible), no autocomplete. Shared by the model-turn
    /// tick and the Lua `drive_live` loop so both paint identically.
    fn render_streaming(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
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
    /// Drive a blocking-Lua future to completion while keeping the UI live:
    /// deliver keystrokes to any pending `ctx.ui.key()` call (the channel now
    /// carries only `Key` events — pane diffs go through the shared UiState
    /// handle), drain UI diffs each tick, render, and clean up panes on cancel.
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
        let mut spinner = time::interval(Duration::from_millis(70));
        let (tx, mut rx) = mpsc::unbounded_channel::<ToolLiveEvent>();
        let future = make_future(tx);
        pin_mut!(future);
        // Pane sources currently shown by the running tool. Used only to clean
        // up lingering panes if the tool is cancelled before emitting its own
        // removal event.
        let mut live_sources = PaneOwnership::new();
        let mut pending_key = KeySink::new();

        loop {
            tokio::select! {
                results = &mut future => {
                    // Drain trailing key requests before returning.
                    while let Ok(ToolLiveEvent::Key(req)) = rx.try_recv() {
                        pending_key.set_direct(req.reply);
                    }
                    return Ok(results);
                }
                Some(ToolLiveEvent::Key(req)) = rx.recv() => {
                    pending_key.set_direct(req.reply);
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
                        &mut pending_key,
                    ) {
                        self.user_config.approval_mode = self.approval_mode;
                        self.persist_runtime_config();
                    }
                    if self.cancel_streaming {
                        // Signal cancellation to any running subagents.
                        cancel_token.store(true, std::sync::atomic::Ordering::Relaxed);
                        // Remove any panes the cancelled tool left behind — its
                        // future was dropped, so it can't emit its own removal.
                        live_sources.drain_for_cancel(&mut self.pages, &mut self.active_page);
                        return Ok(on_cancel());
                    }

                    // Drain UI-state diffs (v2: safe even while the VM is
                    // busy). Track ownership for cancel cleanup.
                    let diffs = self.extensions.drain_view_diffs();
                    for diff in &diffs {
                        live_sources.track(diff);
                    }
                    for diff in diffs {
                        self.apply_view_diff(diff);
                    }

                    // Refresh subagent pane on registry change or ~1s ticker.
                    self.maybe_refresh_jobs_pane();

                    // Drain any key requests sent during drain_keys.
                    while let Ok(ToolLiveEvent::Key(req)) = rx.try_recv() {
                        pending_key.set_direct(req.reply);
                    }

                    self.render_streaming(term)?;
                }
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
        pending_key: &mut KeySink,
    ) -> bool {
        let mut mode_changed = false;
        while event::poll(std::time::Duration::from_millis(0)).unwrap_or(false) {
            match event::read().unwrap_or(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Null,
                KeyModifiers::NONE,
            ))) {
                Event::Paste(text) => {
                    // A bracketed paste while a Lua menu owns the key input (e.g.
                    // the /config api_key text entry) must reach the menu, not the
                    // chat box. The menu reads keys as `Char` events and appends
                    // `char`, so hand it the whole pasted string as one synthetic
                    // Char event; only fall through to chat input when unowned.
                    if pending_key.wants_key() {
                        pending_key.deliver(crate::pane_content::KeyEvent {
                            code: "Char".to_string(),
                            char: Some(text),
                            ctrl: false,
                            alt: false,
                            shift: false,
                        });
                    } else {
                        input.insert_paste(&text);
                    }
                }
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
                    // While a tool owns the key input (e.g. a Lua menu blocked on
                    // `ctx.ui.key()`), forward every key to it — including Ctrl+C,
                    // which the menu treats as cancel. Swallowing Ctrl+C here used
                    // to leave the tool's key request unresolved, wedging the turn
                    // on a oneshot that was never sent or dropped.
                    if pending_key.wants_key() {
                        pending_key.deliver(key_event_from_crossterm(key.code, key.modifiers));
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
                                        let new_mode = mode.cycle();
                                        *mode = new_mode;
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

#[cfg(test)]
mod keysink_tests {
    use super::KeySink;
    use crate::pane_content::KeyEvent;

    fn key(c: &str) -> KeyEvent {
        KeyEvent {
            code: c.to_string(),
            char: Some(c.to_string()),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    #[test]
    fn key_routes_to_armed_reply_slot() {
        let mut sink = KeySink::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        sink.set_direct(tx);
        assert!(sink.wants_key());
        assert!(sink.deliver(key("a")));
        assert_eq!(rx.blocking_recv().unwrap(), key("a"));
    }

    #[test]
    fn owner_buffers_keys_between_requests() {
        // Between a tool's successive requests the slot is empty but the tool
        // still owns input, so keys buffer rather than leaking to chat.
        let mut sink = KeySink::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        sink.set_direct(tx);
        sink.deliver(key("a")); // resolves the slot, owns_input stays latched
        assert_eq!(rx.blocking_recv().unwrap(), key("a"));
        assert!(sink.wants_key()); // still owned
        assert!(sink.deliver(key("b"))); // buffered, consumed (not leaked)

        // Next request drains the buffered key instead of blocking.
        let (tx2, rx2) = tokio::sync::oneshot::channel();
        sink.set_direct(tx2);
        assert_eq!(rx2.blocking_recv().unwrap(), key("b"));
    }

    #[test]
    fn clear_owner_releases_input_to_chat() {
        // After the owning tool finishes, keys must fall through to chat input
        // instead of staying latched/buffered for the rest of the turn.
        let mut sink = KeySink::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        sink.set_direct(tx);
        sink.deliver(key("a"));
        assert_eq!(rx.blocking_recv().unwrap(), key("a"));

        sink.clear_owner();
        assert!(!sink.wants_key());
        assert!(!sink.deliver(key("b"))); // falls through to chat
    }

    #[test]
    fn clear_owner_drops_stale_buffer() {
        // Buffered keys belong to the finished tool and must not bleed into a
        // later tool's first key request.
        let mut sink = KeySink::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        sink.set_direct(tx);
        sink.deliver(key("a"));
        let _ = rx.blocking_recv();
        sink.deliver(key("buffered")); // buffered for next request
        sink.clear_owner();

        let (tx2, rx2) = tokio::sync::oneshot::channel();
        sink.set_direct(tx2);
        sink.deliver(key("fresh"));
        assert_eq!(rx2.blocking_recv().unwrap(), key("fresh"));
    }
}
