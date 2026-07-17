//! Streaming turn driver: renders provider/tool events and handles keys during a turn.

use crate::chat::Message;
use crate::runtime::RuntimeCommand;
use crate::tools::shell::ShellTool;
use crate::tools::{ApprovalMode, Tool, ToolCall};
use crate::ui::input::{InputAction, InputState};
use crate::ui::pane_page::PanePage;
use crate::ui::render::{BoneTerminal, PaneDraw};
use crate::ui::tool_display::build_tool_row;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::collections::VecDeque;
use std::io;
use std::time::Instant;
use tokio::time::Duration;

use super::{App, apply_input_key_with_paste_burst, should_open_agent_log};

/// One place that resolves a `KeyEvent` to a blocked `ctx.ui.key()` request.
/// The daemon asks for a key via `RuntimeEvent::KeyRequest`, and the reply goes
/// back as `RuntimeCommand::KeyReply` over the command channel. The event pumps
/// own a `KeySink` and pass it to `drain_keys`; a terminal key is delivered here.
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

#[derive(Default)]
struct DrainKeysResult {
    mode_changed: bool,
    open_transcript: bool,
    open_job: bool,
    jobs_changed: bool,
}

/// A pending `ctx.ui.key()` request from the daemon, delivered via
/// `RuntimeEvent::KeyRequest`; the reply goes back over the runtime command
/// channel as `KeyReply`.
struct PendingKeyReply {
    id: u64,
    command_tx: tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeCommand>,
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
            Some(key) => {
                let _ = reply
                    .command_tx
                    .send(crate::runtime::RuntimeCommand::KeyReply { id: reply.id, key });
            }
            None => self.pending = Some(reply),
        }
    }

    /// Register a daemon key request (from `RuntimeEvent::KeyRequest`).
    pub fn set_daemon(
        &mut self,
        id: u64,
        command_tx: tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeCommand>,
    ) {
        self.arm(PendingKeyReply { id, command_tx });
    }

    /// Route `key` to the tool. Delivers to the pending reply slot if armed;
    /// otherwise, if a tool owns input, buffers it for the next request. Returns
    /// true if the key was consumed (delivered or buffered), false if it should
    /// fall through to the main chat input.
    pub fn deliver(&mut self, key: crate::pane_content::KeyEvent) -> bool {
        match self.pending.take() {
            Some(reply) => {
                let _ = reply
                    .command_tx
                    .send(crate::runtime::RuntimeCommand::KeyReply { id: reply.id, key });
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
/// remote command pump (`run_remote_command`); cleaned up by `drain_for_cancel`.
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

fn format_elapsed_ms(ms: u64) -> String {
    let total = ms / 1000;
    let mins = total / 60;
    let secs = total % 60;
    format!("{mins}:{secs:02}")
}

fn refresh_queue_page(
    queue: &VecDeque<String>,
    selected: &mut usize,
    pages: &mut Vec<PanePage>,
    active_page: &mut usize,
    panes_visible: &mut bool,
) {
    if queue.is_empty() {
        *selected = 0;
        *active_page = PanePage::remove(pages, crate::ui::queue_pane::PANE_SOURCE, *active_page);
    } else {
        *selected = (*selected).min(queue.len() - 1);
        if let Some(page) = crate::ui::queue_pane::render(queue, *selected) {
            let (_, active) = PanePage::upsert(pages, *active_page, page);
            *active_page = active;
            *panes_visible = true;
        }
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
            if cmd.trim() == "q" || cmd.trim() == "q!" {
                if let Some(notice) = self.request_quit() {
                    self.messages.push(Message::system(notice));
                    self.renderer
                        .flush_new_to_scrollback(&self.messages, term)?;
                    self.redraw(term)?;
                }
                self.input.reset();
                return Ok(());
            }
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
        // User message metadata only — daemon handles transcript/DB/dispatch.
        let image_count = images.len();
        self.messages.push(Message::user_with_images(
            display_text.as_deref().unwrap_or(&text),
            image_count,
        ));
        // Reset the input and shrink the viewport to its final (empty-input)
        // height *before* flushing the message to scrollback. `flush_*` uses
        // `insert_before`, which inserts directly above the *current* viewport;
        // if we flushed while the viewport was still sized for the just-typed
        // (possibly multi-line) input, the subsequent `redraw` shrinks the
        // viewport by recreating it (`resize_viewport` → `term.clear()` +
        // new inline viewport) at a position that can paint over the freshly
        // inserted user-message row — so the message intermittently failed to
        // appear whenever the input had grown past one line. Resetting first
        // means the viewport is already at its final height when we insert.
        self.input.reset();
        self.redraw(term)?;
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)?;
        self.streaming = true;
        self.cancel_streaming = false;
        self.shown_tool_rows.clear();
        // Seed the live output-token estimate from the running total so the
        // status bar ticks up from where it left off as text/tools stream in.
        self.stream_estimated_received = Some(self.view.received);
        self.turn_start = Some(std::time::Instant::now());
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;

        // Send the turn to the daemon (it handles message push, Driver, and persistence).
        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::SubmitPrompt {
                text: text.clone(),
                images,
            });

        self.run_event_pump(term).await
    }

    /// Pump the daemon's `RuntimeEvent` stream for the in-flight turn until
    /// `TurnComplete`, rendering text/tool/approval/key events and forwarding
    /// interactive replies. Assumes the streaming flags + timers were already
    /// set by the caller (`submit_user_turn`, or the submit branch of
    /// `run_remote_command`). Shared so a model turn and a command-triggered
    /// turn render identically.
    pub(super) async fn run_event_pump(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        use crate::runtime::RuntimeEvent;
        use crate::tools::CallOutcome;

        let mut cur_idx: Option<usize> = None;
        let mut pending: std::collections::HashMap<String, crate::tools::ToolCall> =
            std::collections::HashMap::new();
        let mut pending_key = KeySink::new();
        let mut cancel_sent = false;
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(90));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Separate the render cadence (90ms spinner) from key responsiveness.
        // Keys are drained at the top of the loop, but the loop only re-runs
        // when a `select!` branch wakes — so without this a Ctrl+C lands up to
        // 90ms late whenever the model is "thinking" and no events are flowing.
        // This branch wakes the loop ~60x/s to drain keys (cheap: a `poll(0)`),
        // without paying for a full repaint each time.
        let mut key_poll = tokio::time::interval(std::time::Duration::from_millis(16));
        key_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            // Always drain keys before polling events. A fast stream can
            // otherwise starve a timer branch and make Esc lag until a lull.
            // While an approval prompt is up, keys drive the prompt instead of
            // the input field — but the loop keeps pumping the spinner/events.
            if self.pending_approval.is_some() {
                self.drain_approval_keys(term)?;
            } else {
                let drained = Self::drain_keys(
                    &mut self.input,
                    &mut self.queue,
                    &mut self.approval_mode,
                    &mut self.cancel_streaming,
                    &mut self.panes_visible,
                    &mut self.pages,
                    &mut self.active_page,
                    &mut self.selected_job_id,
                    &mut self.queue_selected,
                    &mut self.queue_editing,
                    &mut pending_key,
                    &self.command_tx,
                );
                if drained.mode_changed {
                    self.user_config.approval_mode = self.approval_mode;
                    self.persist_runtime_config();
                }
                if drained.open_transcript {
                    self.open_transcript_view(term)?;
                }
                if drained.jobs_changed {
                    self.refresh_jobs_pane();
                }
                if drained.open_job {
                    self.open_selected_job(term)?;
                }
            }
            if self.cancel_streaming {
                // Cancel the in-flight turn via the daemon, then keep pumping
                // until its `TurnComplete` arrives — don't break here. The daemon
                // stays busy until cancellation propagates; returning to input
                // now would let a prompt submitted in that window be dropped by
                // the daemon's busy branch, and the cancelled turn's late
                // `TurnComplete` would be mistaken for the new turn's completion.
                if !cancel_sent {
                    let _ = self.command_tx.send(crate::runtime::RuntimeCommand::Cancel);
                    cancel_sent = true;
                }
                self.cancel_streaming = false;
            }

            tokio::select! {
                ev = self.events_rx.recv() => match ev {
                    // The daemon signals turn completion with TurnComplete.
                    Ok(RuntimeEvent::TurnComplete) => break,
                    // Approval requests are handled here because collecting a
                    // decision may block the turn. The daemon supplies any edit
                    // preview, so the frontend never resolves tool paths itself.
                    Ok(RuntimeEvent::ApprovalRequest {
                        id, call_id, name, arguments, auto_allows, preview, ..
                    }) => {
                        let call = crate::tools::ToolCall { id: call_id, name, arguments };
                        if let Some(preview) = preview.as_deref() {
                            self.pump_show_edit_preview(&call.id, preview, term)?;
                        }
                        // Danger UI means every tool is allowed. Even if the daemon
                        // still sent a prompt (mode desync), auto-accept and reassert
                        // Danger so the gate catches up for subsequent calls.
                        if auto_allows || matches!(self.approval_mode, crate::tools::ApprovalMode::Danger) {
                            if !auto_allows {
                                self.user_config.approval_mode = crate::tools::ApprovalMode::Danger;
                                self.persist_runtime_config();
                            }
                            let _ = self.command_tx.send(crate::runtime::RuntimeCommand::ApprovalReply {
                                id,
                                outcome: CallOutcome::Approve,
                            });
                        } else {
                            // Show the prompt; the decision is collected by
                            // drain_approval_keys at the top of the loop and
                            // routed back through the daemon command channel.
                            self.begin_approval(&call, id, term)?;
                        }
                    }
                    Ok(ev) => {
                        self.pump_apply_event(ev, &mut cur_idx, &mut pending, &mut pending_key, term)?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_n)) => {
                        // Fell behind the broadcast ring: the oldest events were
                        // dropped. Do NOT `resubscribe()` — that discards the
                        // events still buffered, including `TurnComplete` (the
                        // newest event, otherwise still retained), which would
                        // hang this loop forever. Continuing recv() drains the
                        // retained backlog; only the dropped oldest deltas are
                        // lost, a cosmetic gap the next StateSnapshot reconciles.
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = ticker.tick() => {
                    self.pump_tick(term)?;
                }
                // Wake to drain keys promptly (esp. Ctrl+C); no repaint here.
                _ = key_poll.tick() => {}
            }
        }

        // The daemon applied the outcome; resync the frontend view from the
        // shared session so status bar reads reflect the post-turn truth.

        // Use `cancel_sent`, not `self.cancel_streaming`: the in-loop branch
        // above resets `cancel_streaming` to false every iteration (so a late
        // re-press doesn't re-send Cancel), so by the time we break on
        // TurnComplete it is always false. `cancel_sent` records that this turn
        // was actually cancelled.
        if cancel_sent {
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
        self.running_shells.clear();
        self.pending_shells.clear();
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

    /// Run a slash command on a *remote* daemon's Lua VM over the protocol.
    ///
    /// The in-process path (`run_lua_command`) runs the handler against the
    /// TUI's own VM; this is its remote counterpart. It sends
    /// `RuntimeCommand::RunCommand` and pumps the daemon's interactive event
    /// stream — pane diffs (`ViewDiff`), key requests (`KeyRequest`, answered
    /// with `KeyReply`), and notices (`Status`) — until `CommandComplete`. If
    /// the handler asked to submit its output as a turn, the daemon runs that
    /// turn itself; we push the `/cmd` echo and hand off to [`run_event_pump`]
    /// to render it. Display-only output is shown via `show_reply`.
    pub(super) async fn run_remote_command(
        &mut self,
        cmd: &str,
        arg: &str,
        term: &mut BoneTerminal,
    ) -> Option<()> {
        use crate::runtime::RuntimeEvent;

        // Mirror `run_lua_command`'s turn-timer/flag setup so the status bar
        // ticks elapsed time while the command works.
        self.live_command = true;
        self.cancel_streaming = false;
        self.turn_start = Some(std::time::Instant::now());
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;

        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::RunCommand {
                name: cmd.to_string(),
                input: arg.to_string(),
            });

        let mut pending_key = KeySink::new();
        // Panes the running command opened, so we can tear them down if the
        // user cancels before the daemon emits their removal.
        let mut live_sources = PaneOwnership::new();
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(90));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // See `run_event_pump`: wake fast to drain keys (incl. Ctrl+C) so a
        // cancel isn't delayed until the next 90ms render tick when the command
        // is busy and emitting no events.
        let mut key_poll = tokio::time::interval(std::time::Duration::from_millis(16));
        key_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Captured from `CommandComplete`; rendered after the interactive phase.
        let mut completion: Option<(
            String,
            bool,
            Option<String>,
            Option<crate::runtime::CommandAction>,
        )> = None;

        'command: loop {
            // While an approval prompt is up (a tool the command invoked needs
            // a decision), keys drive the prompt instead of the input field.
            if self.pending_approval.is_some() {
                self.drain_approval_keys(term).ok();
            } else {
                let drained = Self::drain_keys(
                    &mut self.input,
                    &mut self.queue,
                    &mut self.approval_mode,
                    &mut self.cancel_streaming,
                    &mut self.panes_visible,
                    &mut self.pages,
                    &mut self.active_page,
                    &mut self.selected_job_id,
                    &mut self.queue_selected,
                    &mut self.queue_editing,
                    &mut pending_key,
                    &self.command_tx,
                );
                if drained.mode_changed {
                    self.user_config.approval_mode = self.approval_mode;
                    self.persist_runtime_config();
                }
                if drained.open_transcript {
                    self.open_transcript_view(term).ok();
                }
                if drained.jobs_changed {
                    self.refresh_jobs_pane();
                }
                if drained.open_job {
                    self.open_selected_job(term).ok();
                }
            }
            if self.cancel_streaming {
                let _ = self.command_tx.send(crate::runtime::RuntimeCommand::Cancel);
                // The daemon dropped the handler future, so it can't emit its
                // panes' removal — clear what we tracked.
                live_sources.drain_for_cancel(&mut self.pages, &mut self.active_page);
                break 'command;
            }

            tokio::select! {
                ev = self.events_rx.recv() => match ev {
                    Ok(RuntimeEvent::ViewDiff { diff }) => {
                        live_sources.track(&diff);
                        self.apply_view_diff(diff);
                    }
                    Ok(RuntimeEvent::KeyRequest { id }) => {
                        pending_key.set_daemon(id, self.command_tx.clone());
                    }
                    // A tool the command invoked needs approval. Mirror the turn
                    // pump: show the edit preview, auto-allow if permitted, else
                    // raise the prompt (resolved by drain_approval_keys above).
                    // Without this arm the request falls into `Ok(_) => {}` and
                    // the user is never asked.
                    Ok(RuntimeEvent::ApprovalRequest {
                        id, call_id, name, arguments, auto_allows, preview, ..
                    }) => {
                        let call = crate::tools::ToolCall { id: call_id, name, arguments };
                        if let Some(preview) = preview.as_deref() {
                            self.pump_show_edit_preview(&call.id, preview, term).ok();
                        }
                        if auto_allows || matches!(self.approval_mode, crate::tools::ApprovalMode::Danger) {
                            if !auto_allows {
                                self.user_config.approval_mode = crate::tools::ApprovalMode::Danger;
                                self.persist_runtime_config();
                            }
                            let _ = self.command_tx.send(crate::runtime::RuntimeCommand::ApprovalReply {
                                id,
                                outcome: crate::tools::CallOutcome::Approve,
                            });
                        } else {
                            self.begin_approval(&call, id, term).ok();
                        }
                    }
                    Ok(RuntimeEvent::Status { message }) => {
                        // Surface daemon notices (incl. the deferred-config note)
                        // in scrollback, like `pump_notice`.
                        self.messages.push(Message::system(message));
                        self.renderer.flush_new_to_scrollback(&self.messages, term).ok();
                    }
                    Ok(RuntimeEvent::CommandComplete { output, submit, display_role, action }) => {
                        completion = Some((output, submit, display_role, action));
                        break 'command;
                    }
                    // Keep the view-model synced if the daemon publishes state
                    // (non-submit commands publish a snapshot before completing).
                    Ok(RuntimeEvent::StateSnapshot { snapshot }) => {
                        self.apply_snapshot(snapshot);
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Don't resubscribe: that would drop the retained
                        // backlog, including `CommandComplete` (the newest
                        // event), and hang this loop. Keep draining instead.
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break 'command,
                },
                _ = ticker.tick() => {
                    self.promote_pending_shells();
                    self.maybe_refresh_jobs_pane();
                    self.render_streaming(term).ok();
                }
                // Wake to drain keys promptly (esp. Ctrl+C); no repaint here.
                _ = key_poll.tick() => {}
            }
        }

        // Tear down the command's turn-timer/flags (mirrors `run_lua_command`).
        self.live_command = false;
        self.turn_start = None;
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;
        // Reset the cancel flag here too: on the cancel path `completion` is
        // None and we return below before the submit branch's reset, so without
        // this a cancelled command would leave `cancel_streaming` true and make
        // the next submitted turn self-cancel in `run_event_pump`.
        self.cancel_streaming = false;
        // Drop any approval prompt left up if the command was cancelled mid-ask
        // (dropping the reply lets the daemon's gate fall back to its
        // non-interactive decision).
        self.pending_approval = None;
        self.active_prompt = None;
        self.clear_approval_pane();

        let (mut output, submit, display_role, action) = completion?;

        // Apply any frontend action the handler requested, mirroring the local
        // path (`run_lua_command` → `apply_lua_action`). A reply-bearing action
        // (config_action) replaces the displayed output with its status reply;
        // submit is already false in that case (enforced daemon-side), so this
        // only affects rendering.
        if let Some(action) = action
            && let Ok(Some(action_reply)) = self.apply_lua_action(action.into(), term).await
        {
            output = action_reply;
        }

        if submit && !output.is_empty() {
            // The daemon already pushed `output` as the user message and is
            // running the turn; show the `/cmd` echo and render the turn. We
            // must NOT send a SubmitPrompt — that would double-run it.
            let display = if arg.is_empty() {
                format!("/{cmd}")
            } else {
                format!("/{cmd} {arg}")
            };
            self.messages.push(Message::user(display));
            // Reset input + shrink viewport before flushing (see the note in
            // `submit_user_turn`): flushing against a still-tall viewport lets
            // the follow-up redraw's viewport recreation clobber the echo row.
            self.input.reset();
            self.redraw(term).ok();
            self.renderer
                .flush_new_to_scrollback(&self.messages, term)
                .ok();
            self.streaming = true;
            self.shown_tool_rows.clear();
            self.stream_estimated_received = Some(self.view.received);
            self.turn_start = Some(std::time::Instant::now());
            self.cancel_streaming = false;
            self.run_event_pump(term).await.ok();
        } else if !output.is_empty() {
            if display_role.as_deref() == Some("assistant") {
                self.show_assistant_reply(output, term).ok();
            } else {
                self.show_reply(output, term).ok();
            }
        } else {
            self.redraw(term).ok();
        }

        Some(())
    }

    /// Apply one `RuntimeEvent` to the TUI's rendering state (used by the pump's
    /// select loop and its post-loop drain).
    fn pump_apply_event(
        &mut self,
        ev: crate::runtime::RuntimeEvent,
        cur_idx: &mut Option<usize>,
        pending: &mut std::collections::HashMap<String, crate::tools::ToolCall>,
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
                // Keep the view-model cache in lockstep so render code reading
                // `self.view` sees the same authoritative mid-turn totals.
                self.view.sent = sent;
                self.view.received = received;
                self.view.context_length = context_length;
                // Keep the accumulated token_stats in sync for app_ctx_state.
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
            // A turn that ultimately failed (provider error after retries, or
            // too many stream errors). The daemon owns the transcript now, so its
            // `outcome.result` never reaches us — this event is the only signal.
            // Surface it as a persistent notice so the turn doesn't end silently.
            RuntimeEvent::Failed { message } => {
                self.pump_notice(format!("⚠ turn failed: {message}"), cur_idx, term)?;
            }
            RuntimeEvent::ConversationLoadFailed { message, .. } => {
                self.pump_notice(message, cur_idx, term)?;
            }
            RuntimeEvent::WorkElapsed { elapsed_ms } => {
                self.pump_notice(format!("worked for {}", format_elapsed_ms(elapsed_ms)), cur_idx, term)?;
            }
            RuntimeEvent::Started { .. }
            | RuntimeEvent::Finished { .. }
            // Approval requests are handled in the select loop (they need the
            // async edit-preview path); the gate is always resolved before the
            // turn ends, so any that reaches this sync pump is a no-op.
            | RuntimeEvent::ApprovalRequest { .. }
            // CommandComplete is consumed by the remote command pump, not the
            // turn pump; if one arrives here it's a no-op.
            | RuntimeEvent::CommandComplete { .. }
            | RuntimeEvent::KeymapDispatched { .. }
            // Boot-time display state; consumed at attach (apply_idle_event),
            // not mid-turn. A no-op here.
            | RuntimeEvent::FrontendState { .. }
            | RuntimeEvent::TurnComplete => {}
            // A pane/UI diff forwarded by a remote daemon (in-process we drain
            // the shared UiState directly, so this only fires over a socket).
            RuntimeEvent::ViewDiff { diff } => {
                self.apply_view_diff(diff);
            }
            // State sync: update the view-model from the daemon's authoritative
            // snapshot. Mid-turn this is redundant (TokenUsage already keeps
            // the view in sync); post-turn / on attach it's the primary source.
            RuntimeEvent::StateSnapshot { snapshot } => {
                self.apply_snapshot(snapshot);
            }
            // Conversation lifecycle: update the view and rebuild scrollback
            // from the loaded messages. Arrives on /history load or attach.
            RuntimeEvent::ConversationLoaded { messages, snapshot } => {
                self.reset_transient_ui_state(true);
                self.cancel_streaming = false;
                self.apply_snapshot(snapshot);
                self.messages.clear();
                let rows = self.rebuild_scrollback_from_transcript(&messages);
                self.messages.extend(rows);
                self.renderer.scrollback_cursor = 0;
                let _ = self
                    .renderer
                    .flush_new_to_scrollback(&self.messages, term);
            }
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
                if self
                    .wire_tools
                    .display_for_call(&call)
                    .and_then(|d| d.eager)
                    .unwrap_or(false)
                {
                    // Tools that declare `display.eager` (e.g. `subagent`, whose
                    // dispatch/wait calls block until the agents finish) would
                    // otherwise only show their row on completion. Render it now;
                    // the id is recorded in `shown_tool_rows` so the later
                    // `ToolResult` event doesn't render a duplicate.
                    self.pump_show_eager_row(&call, cur_idx, term)?;
                }
                if call.name == "shell" {
                    let label = call
                        .arguments
                        .get("command")
                        .and_then(|v| v.as_str())
                        .map(crate::ui::tool_display::format_shell_label)
                        .unwrap_or_else(|| "shell".to_string());
                    self.pending_shells.push((id.clone(), label, Instant::now()));
                }
                pending.insert(id, call);
            }
            // ToolResult carries the complete capped output. Waiting for it
            // prevents chunks from parallel shells appearing under each other.
            RuntimeEvent::ToolOutput { .. } => {}
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
                self.running_shells.retain(|(cid, _)| cid != &call_id);
                self.pending_shells.retain(|(cid, _, _)| cid != &call_id);
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
                    ..Default::default()
                };
                if self.shown_tool_rows.remove(&call_id) {
                    pending.remove(&call_id);
                } else if let Some(call) = pending.remove(&call_id) {
                    let display = self.wire_tools.display_for_call(&call);
                    self.messages.push(build_tool_row(&call, &result, display));
                } else {
                    self.messages.push(Message::tool_row(name, is_error));
                }
                self.renderer
                    .flush_new_to_scrollback(&self.messages, term)?;
            }
            RuntimeEvent::KeyRequest { id } => {
                pending_key.set_daemon(id, self.command_tx.clone());
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
            ..Default::default()
        };
        let display = self.wire_tools.display_for_call(call);
        self.messages.push(build_tool_row(call, &result, display));
        self.shown_tool_rows.insert(call.id.clone());
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        Ok(())
    }

    /// Render a daemon-computed `edit_file` diff to scrollback. The call id is
    /// recorded in `shown_tool_rows` so the later `ToolResult` does not render
    /// a duplicate row (the preview already shows the edit).
    pub(crate) fn pump_show_edit_preview(
        &mut self,
        call_id: &str,
        preview: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        self.messages.push(Message::system(preview.to_string()));
        self.shown_tool_rows.insert(call_id.to_string());
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
        self.promote_pending_shells();
        self.maybe_refresh_jobs_pane();
        self.render_streaming(term)
    }

    /// Promote shell calls that have been alive longer than the display
    /// threshold (500ms) from the hidden `pending_shells` list to the visible
    /// `running_shells` strip, so only long-running commands appear.
    fn promote_pending_shells(&mut self) {
        const SHELL_DISPLAY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
        let now = Instant::now();
        let mut promoted = Vec::new();
        self.pending_shells.retain(|(id, label, start)| {
            if now.duration_since(*start) >= SHELL_DISPLAY_DELAY {
                promoted.push((id.clone(), label.clone()));
                false
            } else {
                true
            }
        });
        self.running_shells.extend(promoted);
    }

    /// Render the bottom pane during a live turn or tool run: spinner-animated,
    /// current panes (when visible), no autocomplete. Shared by the model-turn
    /// tick and the remote-command pump so both paint identically.
    fn render_streaming(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        // During streaming/tool loops the main event loop is not running, so
        // physical resizes must be handled here too. A plain ratatui draw after
        // resize can strand the old inline viewport rows in scrollback, showing
        // duplicate input borders/fields. Use the same hard-reset path as idle
        // redraws before repainting.
        let size = crossterm::terminal::size()?;
        if self.renderer.last_size.is_some_and(|last| last != size) {
            return self.force_redraw(term);
        }
        self.renderer.last_size = Some(size);

        self.renderer.tick_spinner(
            term,
            &PaneDraw {
                input: &self.input,
                status_info: &self.status_info(),
                pages: if self.panes_visible { &self.pages } else { &[] },
                active_page: self.active_page,
                autocomplete: None,
                running: &self.running_shells,
            },
        )
    }

    async fn run_inline_command(&mut self, cmd: &str, term: &mut BoneTerminal) -> io::Result<()> {
        let result = ShellTool
            .execute(serde_json::json!({
                "command": cmd,
                "timeout_ms": 60_000,
            }))
            .await
            .unwrap_or_else(|e| format!("[error: {e}]"));

        let is_error = result.contains("exit code: 1") || result.contains("timed out");
        self.input.reset();
        self.messages.push(crate::ui::tool_display::shell_row(
            cmd,
            result.clone(),
            is_error,
        ));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;

        // Fold the (truncated) output into the daemon's transcript so a later
        // model turn can answer questions about it. The daemon owns the
        // transcript now, so frontend scrollback alone wouldn't reach the model.
        let transcript_text =
            crate::tools::shell::truncate_output(&format!("$ {cmd}\n{result}"), 200);
        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::AppendMessage {
                role: "user".to_string(),
                content: transcript_text,
            });

        self.redraw(term)?;
        Ok(())
    }

    /// Drain pending key events into input edits or queue. Used during streaming.
    #[allow(clippy::too_many_arguments)] // eight independent streaming UI-state slots
    fn drain_keys(
        input: &mut InputState,
        queue: &mut VecDeque<String>,
        mode: &mut ApprovalMode,
        cancel: &mut bool,
        panes_visible: &mut bool,
        pages: &mut Vec<PanePage>,
        active_page: &mut usize,
        selected_job_id: &mut Option<String>,
        queue_selected: &mut usize,
        queue_editing: &mut Option<(usize, String)>,
        pending_key: &mut KeySink,
        command_tx: &tokio::sync::mpsc::UnboundedSender<RuntimeCommand>,
    ) -> DrainKeysResult {
        let mut result = DrainKeysResult::default();
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
                    if key.code == KeyCode::Char('o')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        result.open_transcript = true;
                        continue;
                    }
                    if queue_editing.is_some()
                        && key.modifiers.is_empty()
                        && matches!(key.code, KeyCode::Enter | KeyCode::Esc)
                    {
                        super::finish_queue_edit(
                            queue,
                            queue_selected,
                            queue_editing,
                            input,
                            key.code == KeyCode::Enter,
                        );
                        refresh_queue_page(
                            queue,
                            queue_selected,
                            pages,
                            active_page,
                            panes_visible,
                        );
                        continue;
                    }
                    let queue_active = *panes_visible
                        && input.buffer.is_empty()
                        && queue_editing.is_none()
                        && pages
                            .get(*active_page)
                            .is_some_and(|p| p.source == crate::ui::queue_pane::PANE_SOURCE);
                    if queue_active
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    {
                        let index = (*queue_selected).min(queue.len().saturating_sub(1));
                        let handled = match key.code {
                            KeyCode::Up
                                if key.modifiers.contains(KeyModifiers::SHIFT) && index > 0 =>
                            {
                                queue.swap(index, index - 1);
                                *queue_selected = index - 1;
                                true
                            }
                            KeyCode::Down
                                if key.modifiers.contains(KeyModifiers::SHIFT)
                                    && index + 1 < queue.len() =>
                            {
                                queue.swap(index, index + 1);
                                *queue_selected = index + 1;
                                true
                            }
                            KeyCode::Up if key.modifiers.is_empty() => {
                                *queue_selected = index.saturating_sub(1);
                                true
                            }
                            KeyCode::Down if key.modifiers.is_empty() => {
                                *queue_selected = (index + 1).min(queue.len().saturating_sub(1));
                                true
                            }
                            KeyCode::Enter if key.modifiers.is_empty() => {
                                if let Some(text) = queue.remove(index) {
                                    queue.push_front(text);
                                    *queue_selected = 0;
                                }
                                true
                            }
                            KeyCode::F(2) if key.modifiers.is_empty() => {
                                if let Some(text) = queue.remove(index) {
                                    input.buffer = text.clone();
                                    input.cursor_pos = input.buffer.chars().count();
                                    *queue_editing = Some((index, text));
                                }
                                true
                            }
                            KeyCode::Delete if key.modifiers.is_empty() => {
                                queue.remove(index);
                                *queue_selected = index.min(queue.len().saturating_sub(1));
                                true
                            }
                            _ => false,
                        };
                        if handled {
                            refresh_queue_page(
                                queue,
                                queue_selected,
                                pages,
                                active_page,
                                panes_visible,
                            );
                            continue;
                        }
                    }
                    let agents_active = *panes_visible
                        && pages
                            .get(*active_page)
                            .is_some_and(|p| p.source == crate::ui::jobs_pane::PANE_SOURCE);
                    if agents_active && key.modifiers.is_empty() {
                        let jobs = crate::ext::jobs::registry().running_jobs();
                        let current = selected_job_id
                            .as_deref()
                            .and_then(|id| jobs.iter().position(|j| j.id == id))
                            .unwrap_or(0);
                        match key.code {
                            KeyCode::Up if !jobs.is_empty() => {
                                *selected_job_id = Some(jobs[current.saturating_sub(1)].id.clone());
                                result.jobs_changed = true;
                                continue;
                            }
                            KeyCode::Down if !jobs.is_empty() => {
                                *selected_job_id =
                                    Some(jobs[(current + 1).min(jobs.len() - 1)].id.clone());
                                result.jobs_changed = true;
                                continue;
                            }
                            KeyCode::Enter if should_open_agent_log(input) => {
                                result.open_job = true;
                                continue;
                            }
                            KeyCode::Char('k') => {
                                if let Some(id) = selected_job_id.as_deref() {
                                    crate::ext::jobs::registry().cancel(id);
                                }
                                result.jobs_changed = true;
                                continue;
                            }
                            _ => {}
                        }
                    }
                    // Ctrl+Enter during streaming: steer the agent mid-turn.
                    if key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL) {
                        let text = input.expanded().trim().to_string();
                        if !text.is_empty() {
                            let _ = command_tx.send(RuntimeCommand::Steer { text });
                            input.reset();
                        }
                        continue;
                    }
                    // ── Page navigation (Tab/PageUp/PageDown) ─────
                    // BackTab is reserved for approval-mode cycle (CycleMode below).
                    if *panes_visible && !pages.is_empty() {
                        *active_page = (*active_page).min(pages.len() - 1);
                        match (key.code, key.modifiers) {
                            (KeyCode::Tab, m) if m.is_empty() => {
                                *active_page = (*active_page + 1) % pages.len();
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
                                let applied = match apply_input_key_with_paste_burst(input, key) {
                                    Ok(result) => result,
                                    Err(_) => return result,
                                };
                                next = applied.trailing;
                                match applied.action {
                                    InputAction::Cancel => {
                                        *cancel = true;
                                        return result;
                                    }
                                    InputAction::Submit => {
                                        // Expand placeholders now; the queued string is fed
                                        // back through send_message later with no blobs.
                                        let text = input.expanded().trim().to_string();
                                        if !text.is_empty() {
                                            queue.push_back(text);
                                            *queue_selected = queue.len() - 1;
                                            input.reset();
                                            refresh_queue_page(
                                                queue,
                                                queue_selected,
                                                pages,
                                                active_page,
                                                panes_visible,
                                            );
                                        }
                                    }
                                    InputAction::ClearQueue => {
                                        queue.clear();
                                        *queue_editing = None;
                                        refresh_queue_page(
                                            queue,
                                            queue_selected,
                                            pages,
                                            active_page,
                                            panes_visible,
                                        );
                                    }
                                    InputAction::CycleMode => {
                                        let new_mode = mode.cycle();
                                        *mode = new_mode;
                                        result.mode_changed = true;
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
        result
    }
}

#[cfg(test)]
#[path = "keysink_tests.rs"]
mod keysink_tests;
