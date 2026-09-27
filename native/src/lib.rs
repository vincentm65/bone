//! Desktop frontend laid out like the TUI: a conversation history sidebar, the
//! transcript, the input, the live pane (Lua panes and interactive menus such as
//! `/config`), and a one-row status bar. One window drives one daemon session;
//! the daemon is authoritative and this UI renders RuntimeEvents and sends
//! RuntimeCommands.
#[cfg(test)]
mod app_tests;
mod cli;
mod commands;
pub mod connection;
mod daemon;
mod graphics;
mod grid;

mod keymap;
mod keys;
mod live_pane;
mod local;
mod pages;
mod sidebar;
mod state;
mod status_bar;
mod theme;
mod transcript;

use base64::Engine;
use bone_protocol::tools::CallOutcome;
use bone_protocol::{
    CommandAction, ConfigAction, ConversationMeta, HostRequest, HostResponse, ImageData,
    KeymapDispatchKind, RuntimeCommand, RuntimeEvent,
};
use connection::{Command, Event};
use eframe::egui;
use local::LocalResult;
use state::{State, ToolCard, ToolState};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Bound event-drain work per frame while the worker's bounded channel applies
/// backpressure.
const MAX_EVENTS_PER_FRAME: usize = 256;

/// Maximum recalled-prompt entries, matching the TUI's
/// `MAX_INPUT_HISTORY_ENTRIES`.
const MAX_HISTORY: usize = 500;

/// Maximum size of a single image attachment (bytes).
const MAX_ATTACHMENT_BYTES: usize = 15 * 1024 * 1024;

/// Pastes longer than this many characters collapse to a `[Pasted text #N +M
/// chars]` placeholder, mirroring the TUI's `PASTE_PLACEHOLDER_THRESHOLD`.
const PASTE_PLACEHOLDER_THRESHOLD: usize = 500;

/// Newest display messages loaded on conversation open; older pages are fetched
/// on demand.
const LOAD_WINDOW: u32 = 200;

/// How long to wait for the conversation list before surfacing an error.
const HOST_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const SIDEBAR_WIDTH: f32 = 240.0;
/// Below this window width the sidebar starts collapsed and, when opened,
/// takes the whole window like a phone drawer.
const NARROW_WIDTH: f32 = 700.0;
/// Room kept at the end of the input row for the touch Stop button.
const STOP_BUTTON_WIDTH: f32 = 72.0;

fn brief_error(error: &str) -> &'static str {
    if error.to_ascii_lowercase().contains("conversation") {
        "Conversation could not be loaded"
    } else {
        "The requested operation could not be completed"
    }
}

/// A captured command completion awaiting post-reduce rendering:
/// `(request_id, output, submit, display_role, action)`.
type CommandCompletion = (
    Option<u64>,
    String,
    bool,
    Option<String>,
    Option<CommandAction>,
);

/// One image attached to the input, ready to send with the next prompt.
#[derive(Debug, Clone)]
struct Attachment {
    name: String,
    media_type: String,
    /// base64 STANDARD-encoded image bytes (matches the TUI clipboard convention).
    data_b64: String,
    byte_size: usize,
}

impl Attachment {
    /// Validate and encode a file as an attachment. The media type is inferred
    /// from the extension; unsupported files and files over the size cap are
    /// rejected with a reason the UI can display.
    fn from_file(name: &str, bytes: Vec<u8>) -> Result<Attachment, String> {
        let media_type = media_type_for(name).ok_or_else(|| {
            format!("\"{name}\" is not a supported image (use .png, .jpeg, .jpg, .webp, .gif)")
        })?;
        if bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "\"{name}\" is {} MB, over the {} MB attachment limit",
                bytes.len() / (1024 * 1024),
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            ));
        }
        Ok(Attachment {
            name: name.to_owned(),
            media_type: media_type.to_string(),
            byte_size: bytes.len(),
            data_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }

    /// Map to the wire `ImageData`; dimensions and hash are left for the daemon.
    fn into_image_data(self) -> ImageData {
        ImageData {
            media_type: self.media_type,
            data: self.data_b64,
            width: None,
            height: None,
            sha256: None,
        }
    }
}

/// Media type for a supported image extension, or `None` for anything else.
fn media_type_for(name: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpeg" | "jpg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

/// A large pasted blob held out of the visible input. `token` is the short
/// placeholder shown; `content` is spliced back in on submit. Mirrors the TUI's
/// `PasteBlob`.
struct PasteBlob {
    token: String,
    content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AgentNavAction {
    Unhandled,
    InputChanged,
    SelectionChanged,
    Open(live_pane::Open),
}

/// The window's one daemon session: its socket, reducer state, input, and
/// transcript cache.
struct Session {
    /// The tab this session belongs to.
    id: u64,
    /// Seeded transcript used when no daemon is available (BONE_DESKTOP_DEMO).
    demo: bool,
    /// Authoritative conversation id once attached; `None` until the first
    /// `conversation_loaded` pins a new conversation.
    conversation_id: Option<i64>,
    connected: bool,
    connecting: bool,
    /// The most recent connect attempt was refused (nothing listening).
    connect_failed_refused: bool,
    /// The most recent connect attempt failed for another reason.
    connect_failed_other: bool,
    /// An established socket dropped; drives a bounded reconnect.
    mid_drop: bool,
    /// `host_api_version` from the daemon's `frontend_state`, 0 until observed.
    host_api_version: u16,
    /// Daemon-reported working directory, never inferred from the client cwd.
    workspace: String,
    connection_status: String,
    state: State,
    composer: String,
    pastes: Vec<PasteBlob>,
    paste_seq: usize,
    attachments: Vec<Attachment>,
    stick_to_bottom: bool,
    jump_to_latest: bool,
    has_new_output: bool,
    /// Bottom-relative viewport offset to restore after an older page prepends
    /// rows: `(distance from bottom, content height on the previous frame)`.
    load_anchor: Option<(f32, f32)>,
    last_bottom_distance: f32,
    transcript: transcript::Cache,
    live_pane: live_pane::LivePane,
    repair_at: Instant,
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    /// Host responses observed since the last drain, correlated by the app.
    host_responses: Vec<(u64, HostResponse)>,
    /// Keymap dispatch results observed since the last drain.
    keymap_dispatched: Vec<(Option<u64>, KeymapDispatchKind)>,
    /// Request id of the in-flight daemon-run slash command.
    pending_command: Option<u64>,
    /// The `/name arg` echo for the in-flight command.
    pending_command_echo: Option<String>,
    autocomplete: Option<commands::AutocompleteState>,
    ctx: egui::Context,
    local_tx: std::sync::mpsc::Sender<LocalResult>,
    /// Prompts typed while a turn is busy, drained in order when it finishes.
    queue: std::collections::VecDeque<String>,
    /// Submitted prompt history for Ctrl+Up/Down recall (oldest first).
    history: Vec<String>,
    history_index: Option<usize>,
    /// When the current turn started, for the status-bar timer.
    turn_started: Option<Instant>,
    /// Busy state last frame, to refresh conversation titles after a turn.
    was_busy: bool,
    /// A turn finished while this tab was not being viewed; cleared on view.
    unread: bool,
    /// The approval prompt for the first pending approval, keyed by its id.
    prompt: Option<(u64, bone_render::prompt::Prompt)>,
    /// Characters typed into a menu beyond the one sent for the current key
    /// request; each later request takes the next one.
    typed_ahead: VecDeque<char>,
    /// True once the user picked Advise and is typing advice in the input.
    advising: bool,
    /// Full-screen pages this chat asked for (`/stats`, `/setup`, `/catalog`).
    page_requests: Vec<PageRequest>,
    /// The boot banner (the daemon's `bone.banner()` plus hints), shown on an
    /// empty chat like the TUI's first screen.
    banner: Option<String>,
}

impl Session {
    /// A session for tab `id`; `conversation` loads a stored conversation on
    /// connect instead of starting a new one.
    fn new(
        id: u64,
        conversation: Option<i64>,
        ctx: &egui::Context,
        local_tx: std::sync::mpsc::Sender<LocalResult>,
    ) -> Self {
        let (commands, events) = connection::spawn(ctx.clone());
        let mut session = Self {
            id,
            demo: false,
            conversation_id: conversation,
            connected: false,
            connecting: false,
            connect_failed_refused: false,
            connect_failed_other: false,
            mid_drop: false,
            host_api_version: 0,
            workspace: String::new(),
            connection_status: "Disconnected".into(),
            state: State::default(),
            composer: String::new(),
            pastes: Vec::new(),
            paste_seq: 0,
            attachments: Vec::new(),
            stick_to_bottom: true,
            jump_to_latest: false,
            has_new_output: false,
            load_anchor: None,
            last_bottom_distance: 0.0,
            transcript: transcript::Cache::new(),
            live_pane: live_pane::LivePane::default(),
            repair_at: Instant::now(),
            commands,
            events,
            host_responses: Vec::new(),
            keymap_dispatched: Vec::new(),
            pending_command: None,
            pending_command_echo: None,
            autocomplete: None,
            ctx: ctx.clone(),
            local_tx,
            queue: std::collections::VecDeque::new(),
            history: Vec::new(),
            history_index: None,
            turn_started: None,
            was_busy: false,
            unread: false,
            prompt: None,
            typed_ahead: VecDeque::new(),
            advising: false,
            page_requests: Vec::new(),
            banner: None,
        };
        session.state.window = Some(LOAD_WINDOW);
        session.reset_for_attach();
        session
    }

    /// Re-arm the reducer for a (re)connect: a new conversation skips the
    /// default-actor replay; a loaded one filters the replay by its id.
    fn reset_for_attach(&mut self) {
        match self.conversation_id {
            Some(id) => self.state.reset(Some(id)),
            None => self.state.reset_new(),
        }
        self.transcript.reset();
        self.live_pane.reset_for_attach();
        self.pending_command = None;
        self.pending_command_echo = None;
        self.autocomplete = None;
    }

    /// Retry the authoritative load after a conversation-load failure without
    /// mutating any daemon-owned history.
    fn retry_failed_load(&mut self) -> bool {
        let Some(id) = self.state.expected_id else {
            return false;
        };
        self.state.last_error = None;
        self.state.status = "Loading conversation…".into();
        let window = self.state.window;
        self.command(RuntimeCommand::LoadConversation { id, window })
    }

    fn command(&mut self, command: RuntimeCommand) -> bool {
        if !self.connected {
            return false;
        }
        if self.commands.send(Command::Send(command)).is_err() {
            self.connected = false;
            self.connection_status = "Connection worker stopped; delivery may be uncertain".into();
            return false;
        }
        true
    }

    /// Send a daemon-run slash command. The daemon answers with a correlated
    /// `CommandComplete`, applied by `handle_event`.
    fn run_command(&mut self, name: &str, input: &str) -> bool {
        if !self.connected {
            return false;
        }
        let request_id = self.state.next_id();
        let sent = self.command(RuntimeCommand::RunCommand {
            request_id: Some(request_id),
            name: name.to_string(),
            input: input.to_string(),
        });
        if sent {
            self.pending_command = Some(request_id);
            self.pending_command_echo = Some(if input.is_empty() {
                format!("/{name}")
            } else {
                format!("/{name} {input}")
            });
        }
        sent
    }

    /// Whether the daemon advertises a Lua command by this name.
    fn has_lua_command(&self, name: &str) -> bool {
        self.state
            .frontend
            .as_ref()
            .is_some_and(|frontend| frontend.commands.iter().any(|(n, _)| n == name))
    }

    /// Send the input. Client built-ins are handled here, other `/` names run
    /// as daemon slash commands, and everything else submits a prompt.
    fn submit_composer(&mut self) {
        let expanded = self.expanded_composer();
        let text = expanded.trim().to_string();
        if let Some(command) = text.strip_prefix(':').or_else(|| text.strip_prefix('!')) {
            let command = command.trim();
            if command == "q" || command == "q!" {
                self.clear_input();
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            if !command.is_empty() {
                self.start_inline_shell(command);
                return;
            }
        }
        if let Some(command) = text.strip_prefix('/') {
            let mut parts = command.splitn(2, ' ');
            let name = parts.next().unwrap_or("");
            let arg = parts.next().unwrap_or("").trim_end();
            if !name.is_empty() {
                if self.handle_builtin_command(name, arg) {
                    return;
                }
                if self.can_send() {
                    self.clear_input();
                    self.run_command(name, arg);
                }
                return;
            }
        }
        if self.can_send() {
            self.send_prompt();
        }
    }

    /// Reset the input and its transient state after a command consumed it.
    fn clear_input(&mut self) {
        self.composer.clear();
        self.pastes.clear();
        self.attachments.clear();
        self.autocomplete = None;
        self.history_index = None;
    }

    /// The input with every paste placeholder substituted back.
    fn expanded_composer(&self) -> String {
        let mut out = self.composer.clone();
        for blob in &self.pastes {
            out = out.replace(&blob.token, &blob.content);
        }
        out
    }

    /// Collapse a large paste into a `[Pasted text #N +M chars]` placeholder at
    /// `char_index`. Returns the placeholder's char length.
    fn insert_paste_placeholder(&mut self, text: &str, char_index: usize) -> usize {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let char_count = normalized.chars().count();
        self.paste_seq += 1;
        let token = format!("[Pasted text #{} +{} chars]", self.paste_seq, char_count);
        let token_len = token.chars().count();
        let byte = self
            .composer
            .char_indices()
            .nth(char_index)
            .map(|(i, _)| i)
            .unwrap_or(self.composer.len());
        self.composer.insert_str(byte, &token);
        self.pastes.push(PasteBlob {
            token,
            content: normalized,
        });
        token_len
    }

    fn attach(&mut self, name: &str, bytes: Vec<u8>) {
        match Attachment::from_file(name, bytes) {
            Ok(attachment) => {
                self.attachments.push(attachment);
                self.state.last_error = None;
            }
            Err(reason) => self.state.last_error = Some(reason),
        }
    }

    /// Attach the image on the system clipboard, like the TUI's paste_image.
    fn paste_clipboard_image(&mut self) {
        match bone_render::clipboard::clipboard_image() {
            Ok(image) => {
                let byte_size = image.data.len() * 3 / 4;
                self.attachments.push(Attachment {
                    name: "clipboard".into(),
                    media_type: image.media_type,
                    data_b64: image.data,
                    byte_size,
                });
                self.state.last_error = None;
            }
            Err(error) => self.reply(format!("image paste failed: {error}")),
        }
    }

    /// The running-shell strip shown above the input, as the TUI draws it.
    fn running_shells(
        &self,
        theme: &bone_render::theme::Theme,
        time: f64,
    ) -> Vec<ratatui::text::Line<'static>> {
        const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let running: Vec<(String, Instant)> = self
            .state
            .toolcards
            .iter()
            .flatten()
            .filter(|card| card.name == "shell" && card.state == state::ToolState::Running)
            .filter_map(|card| {
                let arguments = card
                    .args
                    .as_deref()
                    .and_then(|args| serde_json::from_str(args).ok())
                    .unwrap_or_default();
                Some((
                    bone_render::tool_display::format_shell_call_label(&arguments),
                    card.started?,
                ))
            })
            .collect();
        let spinner = SPINNER[(time * 12.5) as usize % SPINNER.len()];
        let total = running.len();
        running
            .iter()
            .enumerate()
            .map(|(index, (label, started))| {
                ratatui::text::Line::from(bone_render::messages::running_shell_line(
                    label,
                    started.elapsed(),
                    (total > 1).then_some((index + 1, total)),
                    Some(spinner),
                    200,
                    theme,
                ))
            })
            .collect()
    }

    /// Up/Down, Enter, and the queue keys for the live pane's list pages,
    /// plus the input-history bridge used when no selectable pane is active.
    fn handle_list_keys(&mut self, ui: &mut egui::Ui) -> Option<live_pane::Open> {
        let list = self.live_pane.active_list().map(str::to_owned);
        let key =
            |ui: &mut egui::Ui, modifiers, key| ui.input_mut(|i| i.consume_key(modifiers, key));
        let none = egui::Modifiers::NONE;
        let shift = egui::Modifiers::SHIFT;

        // Queue navigation is deliberately separate from agent/process history
        // navigation and remains available only for an exactly empty buffer.
        if list.as_deref() == Some("queue") && self.composer.is_empty() {
            let len = self.queue.len();
            let index = self.live_pane.queue.min(len.saturating_sub(1));
            if key(ui, shift, egui::Key::ArrowUp) && index > 0 {
                self.queue.swap(index, index - 1);
                self.live_pane.queue = index - 1;
            } else if key(ui, shift, egui::Key::ArrowDown) && index + 1 < len {
                self.queue.swap(index, index + 1);
                self.live_pane.queue = index + 1;
            } else if key(ui, none, egui::Key::ArrowUp) {
                self.live_pane.queue = index.saturating_sub(1);
            } else if key(ui, none, egui::Key::ArrowDown) {
                self.live_pane.queue = (index + 1).min(len.saturating_sub(1));
            } else if key(ui, none, egui::Key::Enter) {
                if let Some(text) = self.queue.remove(index) {
                    self.queue.push_front(text);
                    self.live_pane.queue = 0;
                }
            } else if key(ui, none, egui::Key::F2) {
                if let Some(text) = self.queue.remove(index) {
                    self.composer = text;
                }
            } else if key(ui, none, egui::Key::Delete) {
                self.queue.remove(index);
            }
            return None;
        }

        let agent_list = match list.as_deref() {
            Some("jobs") | Some("processes") => list.as_deref(),
            _ => None,
        };
        let active_ids: Vec<String> = match agent_list {
            Some("jobs") => self.state.jobs.iter().map(|job| job.id.clone()).collect(),
            Some("processes") => self
                .state
                .processes
                .iter()
                .map(|process| process.id.clone())
                .collect(),
            _ => Vec::new(),
        };

        // Consume every unmodified Up/Down before TextEdit can move its
        // multiline cursor. The helper intentionally reports Unhandled at a
        // history boundary, but that key is still a TUI-style no-op here.
        let arrow = if plain_key_pressed(ui, egui::Key::ArrowUp)
            && key(ui, none, egui::Key::ArrowUp)
        {
            Some(egui::Key::ArrowUp)
        } else if plain_key_pressed(ui, egui::Key::ArrowDown) && key(ui, none, egui::Key::ArrowDown)
        {
            Some(egui::Key::ArrowDown)
        } else {
            None
        };
        if let Some(code) = arrow {
            let action = self.apply_agent_nav_key(code, none, &active_ids, agent_list);
            return self.finish_agent_nav(action);
        }

        // Enter is handed to the input editor unless the selectable pane has a
        // valid selected row and the trimmed input is empty, matching the TUI's
        // should_open_agent_log guard.
        if agent_list.is_some() && plain_key_pressed(ui, egui::Key::Enter) {
            let action = self.apply_agent_nav_key(egui::Key::Enter, none, &active_ids, agent_list);
            if !matches!(action, AgentNavAction::Unhandled) {
                key(ui, none, egui::Key::Enter);
                return self.finish_agent_nav(action);
            }
        }
        None
    }

    fn apply_agent_nav_key(
        &mut self,
        code: egui::Key,
        modifiers: egui::Modifiers,
        active_ids: &[String],
        list: Option<&str>,
    ) -> AgentNavAction {
        if modifiers != egui::Modifiers::NONE {
            return AgentNavAction::Unhandled;
        }
        let focused = match list {
            Some("jobs") => self.live_pane.job_focused,
            Some("processes") => self.live_pane.process_focused,
            _ => false,
        };
        let selected = match list {
            Some("jobs") => self.live_pane.job.clone(),
            Some("processes") => self.live_pane.process.clone(),
            _ => None,
        };

        match code {
            egui::Key::ArrowUp if !focused => {
                if self.history_prev() {
                    AgentNavAction::InputChanged
                } else {
                    AgentNavAction::Unhandled
                }
            }
            egui::Key::ArrowDown if !focused => {
                if self.history_next() {
                    AgentNavAction::InputChanged
                } else if self.history_index.is_none()
                    && self.composer.is_empty()
                    && let Some(first) = active_ids.first()
                {
                    self.set_agent_focus(list, true);
                    self.set_agent_selection(list, Some(first.clone()));
                    AgentNavAction::SelectionChanged
                } else {
                    AgentNavAction::Unhandled
                }
            }
            egui::Key::ArrowUp if focused && !active_ids.is_empty() => {
                let current = selected
                    .as_deref()
                    .and_then(|id| active_ids.iter().position(|active| active == id))
                    .unwrap_or(0);
                if current > 0 {
                    self.set_agent_selection(list, Some(active_ids[current - 1].clone()));
                    AgentNavAction::SelectionChanged
                } else {
                    self.set_agent_focus(list, false);
                    self.select_live_input();
                    AgentNavAction::InputChanged
                }
            }
            egui::Key::ArrowDown if focused && !active_ids.is_empty() => {
                let current = selected
                    .as_deref()
                    .and_then(|id| active_ids.iter().position(|active| active == id))
                    .unwrap_or(0);
                let next = (current + 1).min(active_ids.len() - 1);
                self.set_agent_selection(list, Some(active_ids[next].clone()));
                AgentNavAction::SelectionChanged
            }
            egui::Key::Enter if self.composer.trim().is_empty() => {
                let Some(index) = selected
                    .as_deref()
                    .and_then(|id| active_ids.iter().position(|active| active == id))
                else {
                    return AgentNavAction::Unhandled;
                };
                let Some(id) = active_ids.get(index).cloned() else {
                    return AgentNavAction::Unhandled;
                };
                match list {
                    Some("jobs") => AgentNavAction::Open(live_pane::Open::Job(id)),
                    Some("processes") => AgentNavAction::Open(live_pane::Open::Process(id)),
                    _ => AgentNavAction::Unhandled,
                }
            }
            _ => AgentNavAction::Unhandled,
        }
    }

    fn finish_agent_nav(&mut self, action: AgentNavAction) -> Option<live_pane::Open> {
        match action {
            AgentNavAction::Unhandled
            | AgentNavAction::InputChanged
            | AgentNavAction::SelectionChanged => None,
            AgentNavAction::Open(target) => Some(target),
        }
    }

    fn set_agent_selection(&mut self, list: Option<&str>, selection: Option<String>) {
        match list {
            Some("jobs") => self.live_pane.job = selection,
            Some("processes") => self.live_pane.process = selection,
            _ => {}
        }
    }

    fn set_agent_focus(&mut self, list: Option<&str>, focused: bool) {
        match list {
            Some("jobs") => self.live_pane.job_focused = focused,
            Some("processes") => self.live_pane.process_focused = focused,
            _ => {}
        }
    }

    /// Clear a focused list before ordinary text-editor input. Arrow keys and
    /// page navigation are left alone so their own handlers retain focus.
    fn clear_agent_focus_for_input(&mut self, ui: &egui::Ui) {
        if !self.live_pane.job_focused && !self.live_pane.process_focused {
            return;
        }
        let editing = ui.input(|input| {
            input.events.iter().any(|event| match event {
                egui::Event::Text(text) | egui::Event::Paste(text) => !text.is_empty(),
                egui::Event::Key {
                    key, pressed: true, ..
                } => !matches!(
                    key,
                    egui::Key::ArrowUp
                        | egui::Key::ArrowDown
                        | egui::Key::Tab
                        | egui::Key::PageUp
                        | egui::Key::PageDown
                ),
                _ => false,
            })
        });
        if editing {
            self.live_pane.clear_focus();
        }
    }

    fn reply(&mut self, text: impl Into<String>) {
        self.state.push_row("system", text);
    }

    /// Handle a built-in slash command client-side. Returns true when `name` is
    /// a built-in this client owns; unknown names fall through to the daemon.
    fn handle_builtin_command(&mut self, name: &str, arg: &str) -> bool {
        let arg = arg.trim();
        match name {
            "help" => {
                self.clear_input();
                let advertised: &[(String, String)] = self
                    .state
                    .frontend
                    .as_ref()
                    .map(|frontend| frontend.commands.as_slice())
                    .unwrap_or(&[]);
                let text = commands::help(advertised);
                self.reply(text);
            }
            "clear" | "new" => {
                self.clear_input();
                if !self.new_conversation() {
                    self.reply("Not connected to the daemon.");
                }
            }
            // The bundled Lua `config` command owns the interactive settings
            // and provider menus; they render in the live pane.
            "config" | "provider" if arg.is_empty() || name == "config" => {
                self.clear_input();
                let input = if name == "provider" { "providers" } else { arg };
                if !self.has_lua_command("config") {
                    self.reply("The config command is unavailable (is the daemon up to date?).");
                } else if !self.run_command("config", input) {
                    self.reply("Not connected to the daemon.");
                }
            }
            "provider" => {
                self.clear_input();
                if self.command(RuntimeCommand::SwitchProvider {
                    provider_id: arg.to_string(),
                }) {
                    self.reply(format!("Switching provider to {arg}…"));
                } else {
                    self.reply("Not connected to the daemon.");
                }
            }
            "model" => {
                self.clear_input();
                if arg.is_empty() {
                    let text = format!(
                        "{} ({})",
                        self.state.snapshot.provider_model, self.state.snapshot.provider_id
                    );
                    self.reply(text);
                } else if self.host_api_version < 3 {
                    self.reply("Update the daemon to choose a model per conversation.");
                } else if self.command(RuntimeCommand::SetConversationModel {
                    provider_id: self.state.snapshot.provider_id.clone(),
                    model: arg.to_string(),
                }) {
                    self.reply(format!("Selecting model {arg}…"));
                } else {
                    self.reply("Not connected to the daemon.");
                }
            }
            "incognito" => {
                self.clear_input();
                let enabled = match arg {
                    "" => !self.state.snapshot.incognito,
                    "on" => true,
                    "off" => false,
                    other => {
                        self.reply(format!(
                            "Unknown option `{other}` — usage: /incognito [on|off]"
                        ));
                        return true;
                    }
                };
                if self.command(RuntimeCommand::SetIncognito { enabled }) {
                    self.reply(format!("Incognito {}.", if enabled { "on" } else { "off" }));
                } else {
                    self.reply("Not connected to the daemon.");
                }
            }
            "update" => {
                self.clear_input();
                match daemon::resolve_binary() {
                    Some(binary) => {
                        self.state.status = "Checking for updates…".into();
                        local::spawn_update(
                            self.ctx.clone(),
                            self.local_tx.clone(),
                            self.id,
                            binary,
                        );
                    }
                    None => self.reply(local::update_reply(None)),
                }
            }
            "edit" | "e" => {
                let draft = self.expanded_composer();
                self.clear_input();
                local::spawn_editor(self.ctx.clone(), self.local_tx.clone(), self.id, draft);
            }
            "stats" => {
                self.clear_input();
                self.page_requests.push(PageRequest::Stats);
            }
            "setup" => {
                self.clear_input();
                self.page_requests.push(PageRequest::Setup);
            }
            "catalog" => {
                self.clear_input();
                if arg.is_empty() {
                    self.page_requests.push(PageRequest::Catalog);
                    return true;
                }
                let mut parts = arg.split_whitespace();
                match (parts.next(), parts.next(), parts.next()) {
                    (Some(action @ ("install" | "remove")), Some(name), None) => {
                        self.page_requests.push(PageRequest::CatalogAction {
                            install: action == "install",
                            name: name.to_string(),
                        });
                    }
                    _ => self.reply("Usage: /catalog install|remove NAME"),
                }
            }
            "quit" | "exit" => {
                self.clear_input();
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => return false,
        }
        true
    }

    /// Start a fresh conversation on this session, dropping queued prompts.
    fn new_conversation(&mut self) -> bool {
        if !self.connected {
            return false;
        }
        self.queue.clear();
        self.state.clear_conversation();
        self.transcript.reset();
        self.command(RuntimeCommand::NewConversation)
    }

    /// Switch this session to a stored conversation.
    fn load_conversation(&mut self, id: i64) -> bool {
        if !self.connected || self.conversation_id == Some(id) {
            return false;
        }
        self.queue.clear();
        self.conversation_id = Some(id);
        self.reset_for_attach();
        let window = self.state.window;
        self.command(RuntimeCommand::LoadConversation { id, window })
    }

    /// Client-local `:`/`!`: run the command in a local shell.
    fn start_inline_shell(&mut self, command: &str) {
        self.clear_input();
        self.state.status = "Running inline command…".into();
        local::spawn_shell(
            self.ctx.clone(),
            self.local_tx.clone(),
            self.id,
            command.to_string(),
        );
    }

    /// Render a finished inline-shell command as a local tool row plus the
    /// folded `$ cmd\n<output>` message the daemon keeps in the conversation.
    fn append_shell_result(&mut self, command: &str, output: &str, is_error: bool) {
        let i = self.state.push_row(
            if is_error {
                "tool: shell (error)"
            } else {
                "tool: shell"
            },
            output,
        );
        self.state.toolcards[i] = Some(ToolCard {
            name: "shell".into(),
            state: if is_error {
                ToolState::Error
            } else {
                ToolState::Done
            },
            args: Some(command.to_string()),
            started: None,
        });
        let _ = self.command(RuntimeCommand::AppendMessage {
            role: "user".into(),
            content: local::shell_transcript_text(command, output),
        });
    }

    /// Recompute the `/` autocomplete from the current input, keeping the
    /// selection while the query and command set are unchanged.
    fn refresh_autocomplete(&mut self) {
        let Some(query) = commands::slash_query(&self.composer) else {
            self.autocomplete = None;
            return;
        };
        let advertised: &[(String, String)] = self
            .state
            .frontend
            .as_ref()
            .map(|frontend| frontend.commands.as_slice())
            .unwrap_or(&[]);
        let all = commands::merge_commands(advertised);
        if self
            .autocomplete
            .as_ref()
            .is_none_or(|ac| ac.all_commands() != all.as_slice())
        {
            self.autocomplete = Some(commands::AutocompleteState::new(all));
        }
        if let Some(ac) = self.autocomplete.as_mut() {
            ac.update(query);
        }
    }

    fn autocomplete_open(&self) -> bool {
        self.autocomplete
            .as_ref()
            .is_some_and(|ac| !ac.matches.is_empty())
    }

    /// Fill the input with the highlighted `/name` and close the list.
    fn accept_autocomplete(&mut self) -> bool {
        let Some(name) = self
            .autocomplete
            .as_ref()
            .and_then(|ac| ac.selected_command().map(str::to_string))
        else {
            self.autocomplete = None;
            return false;
        };
        self.composer = format!("/{name}");
        self.autocomplete = None;
        true
    }

    fn send_prompt(&mut self) {
        if !self.can_send() {
            return;
        }
        let request_id = self.state.next_id();
        let images: Vec<ImageData> = self
            .attachments
            .iter()
            .cloned()
            .map(Attachment::into_image_data)
            .collect();
        if self.command(RuntimeCommand::SubmitPrompt {
            request_id: Some(request_id),
            text: self.expanded_composer(),
            images,
        }) {
            self.record_history();
            self.clear_input();
            self.state.busy = true;
            self.state.status = "Sending…".into();
        }
    }

    /// Apply the result of this session's in-flight daemon slash command;
    /// another client's broadcast is ignored.
    fn apply_command_complete(
        &mut self,
        request_id: Option<u64>,
        mut output: String,
        submit: bool,
        display_role: Option<String>,
        action: Option<CommandAction>,
    ) {
        let mine = match request_id {
            Some(id) => self.pending_command == Some(id),
            None => self.pending_command.is_some(),
        };
        if !mine {
            return;
        }
        let echo = self.pending_command_echo.take();
        self.pending_command = None;
        if let Some(action) = action
            && let Some(reply) = self.apply_command_action(action)
        {
            output = reply;
        }
        if submit && !output.is_empty() {
            // The daemon already pushed `output` as the user message and runs
            // the turn; show the `/cmd` echo.
            if let Some(echo) = echo {
                self.state.push_row("user", echo);
            }
            self.state.busy = true;
            self.state.status = "Working…".into();
        } else {
            // No turn started (a cancelled menu, a config reply, or nothing):
            // settle the busy state a menu's `KeyRequest` set.
            if !output.is_empty() {
                let role = if display_role.as_deref() == Some("assistant") {
                    "assistant"
                } else {
                    "system"
                };
                self.state.push_row(role, output);
            }
            self.state.finish_command();
        }
    }

    /// Send the daemon command(s) a frontend action requests. Returns a status
    /// reply when the action is config-related.
    fn apply_command_action(&mut self, action: CommandAction) -> Option<String> {
        if let Some(messages) = action.conversation_replace {
            self.command(RuntimeCommand::ReplaceConversation { messages });
        }
        if let Some(load) = action.conversation_load
            && let Some(id) = load.conversation_id
        {
            let window = self.state.window;
            self.command(RuntimeCommand::LoadConversation { id, window });
        }
        action.config_action.map(|action| match action {
            ConfigAction::Apply => {
                self.command(RuntimeCommand::ReloadSettings);
                "Configuration applied.".to_string()
            }
            ConfigAction::ApplyRestartRequired => {
                self.command(RuntimeCommand::ReloadSettings);
                "Configuration saved. Restart required for tool/command changes.".to_string()
            }
            ConfigAction::ReloadTools => {
                self.command(RuntimeCommand::ReloadExtensions);
                "Reloading tools and Lua extensions…".to_string()
            }
            ConfigAction::SwitchProvider { id } => {
                self.command(RuntimeCommand::SwitchProvider {
                    provider_id: id.clone(),
                });
                format!("Switching provider to {id}…")
            }
        })
    }

    /// Drain socket events. Returns true when the attached conversation changed.
    fn drain_events(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        for _ in 0..MAX_EVENTS_PER_FRAME {
            let Ok(event) = self.events.try_recv() else {
                break;
            };
            changed |= self.handle_event(event);
        }
        if !self.events.is_empty() {
            ctx.request_repaint();
        }
        // Only repair a busy/lagged attachment. No idle polling.
        if self.connected && self.state.repairing {
            if Instant::now() >= self.repair_at {
                let command = self.state.synchronize();
                self.command(command);
                self.repair_at = Instant::now() + Duration::from_millis(500);
            }
            ctx.request_repaint_after(self.repair_at.saturating_duration_since(Instant::now()));
        }
        changed
    }

    fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::Connected => {
                self.connected = true;
                self.connecting = false;
                self.connect_failed_refused = false;
                self.connect_failed_other = false;
                self.mid_drop = false;
                self.host_api_version = 0;
                self.connection_status = "Connected".into();
                self.reset_for_attach();
                let window = self.state.window;
                let command = match self.conversation_id {
                    Some(id) => RuntimeCommand::LoadConversation { id, window },
                    None => RuntimeCommand::NewConversation,
                };
                if !self.command(command) {
                    self.connection_status =
                        "Connection worker stopped before attach could be sent".into();
                }
                false
            }
            Event::Disconnected(reason) => {
                self.mid_drop = self.connected && daemon::is_mid_session_drop(&reason);
                self.connected = false;
                self.connecting = false;
                self.state.ready = false;
                // Interactive requests cannot be answered after the socket is gone.
                self.state.approvals.clear();
                self.state.pending_key = None;
                self.state.busy = false;
                self.connection_status = reason.clone();
                if reason.starts_with("Connect failed") {
                    if daemon::is_refused(&reason) {
                        self.connect_failed_refused = true;
                    } else {
                        self.connect_failed_other = true;
                    }
                }
                false
            }
            Event::Runtime(event) => {
                let mut command_complete: Option<CommandCompletion> = None;
                match &event {
                    RuntimeEvent::HostResponse {
                        request_id,
                        response,
                    } => self.host_responses.push((*request_id, response.clone())),
                    RuntimeEvent::FrontendState {
                        host_api_version,
                        cwd,
                        banner,
                        catalog_updates,
                        ..
                    } => {
                        self.host_api_version = *host_api_version;
                        self.workspace = cwd.clone().unwrap_or_default();
                        if self.banner.is_none() {
                            let mut lines: Vec<String> = Vec::new();
                            if !banner.is_empty() {
                                lines.push(banner.clone());
                            }
                            if *catalog_updates > 0 {
                                lines.push(format!(
                                    "{catalog_updates} catalog update{} available — run /catalog",
                                    if *catalog_updates == 1 { "" } else { "s" }
                                ));
                            }
                            lines.push(format!(
                                "bone-desktop v{} — type /help for commands.",
                                env!("CARGO_PKG_VERSION")
                            ));
                            self.banner = Some(lines.join("\n"));
                        }
                    }
                    RuntimeEvent::CommandComplete {
                        request_id,
                        output,
                        submit,
                        display_role,
                        action,
                    } => {
                        command_complete = Some((
                            *request_id,
                            output.clone(),
                            *submit,
                            display_role.clone(),
                            action.clone(),
                        ));
                    }
                    RuntimeEvent::KeymapDispatched { request_id, kind } => {
                        self.keymap_dispatched.push((*request_id, kind.clone()));
                    }
                    _ => {}
                }
                let before = self.conversation_id;
                if let Some(command) = self.state.reduce(event) {
                    self.command(command);
                }
                if let Some((request_id, output, submit, display_role, action)) = command_complete {
                    self.apply_command_complete(request_id, output, submit, display_role, action);
                }
                if let Some(id) = self.state.snapshot.conversation_id {
                    self.conversation_id = Some(id);
                }
                before != self.conversation_id
            }
        }
    }

    fn can_send(&self) -> bool {
        self.connected
            && self.state.ready
            && !self.state.busy
            && (!self.composer.trim().is_empty() || !self.attachments.is_empty())
    }

    /// Whether the input can be queued for after the current turn. Images are
    /// not preserved in the queue, matching the TUI.
    fn can_queue(&self) -> bool {
        self.connected && self.state.ready && self.state.busy && !self.composer.trim().is_empty()
    }

    fn enqueue_composer(&mut self) {
        if !self.can_queue() {
            return;
        }
        self.queue
            .push_back(self.expanded_composer().trim().to_string());
        self.clear_input();
    }

    /// Submit an idle input behind prompts already queued. A draft with
    /// attachments is sent directly.
    fn submit_composer_in_order(&mut self) {
        if self.queue.is_empty() || !self.attachments.is_empty() {
            self.submit_composer();
            return;
        }
        let text = self.expanded_composer().trim().to_string();
        if !text.is_empty() {
            self.queue.push_back(text);
            self.clear_input();
        }
        self.drain_queue();
    }

    /// Steer the agent mid-turn (Ctrl/Alt+Enter while busy).
    fn steer_composer(&mut self) {
        if !self.can_queue() {
            return;
        }
        let text = self.expanded_composer().trim().to_string();
        if self.command(RuntimeCommand::Steer { text }) {
            self.record_history();
            self.clear_input();
            self.state.status = "Steering…".into();
        }
    }

    /// Send the next queued prompt once idle with an empty input, mirroring the
    /// TUI's `drain_queue_when_input_empty`.
    fn drain_queue(&mut self) {
        if self.queue.is_empty()
            || self.state.busy
            || !self.composer.is_empty()
            || !self.attachments.is_empty()
            || !self.connected
            || !self.state.ready
        {
            return;
        }
        if let Some(next) = self.queue.pop_front() {
            self.composer = next;
            self.history_index = None;
            self.submit_composer();
        }
    }

    fn record_history(&mut self) {
        let text = self.expanded_composer().trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(pos) = self.history.iter().rposition(|entry| entry == &text) {
            self.history.remove(pos);
        }
        self.history.push(text);
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
        self.history_index = None;
    }

    /// Move toward older submitted prompts. Returns whether the input moved.
    fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let index = self.history_index.unwrap_or(self.history.len());
        if index == 0 {
            return false;
        }
        self.history_index = Some(index - 1);
        self.composer = self.history[index - 1].clone();
        self.autocomplete = None;
        true
    }

    /// Move toward newer submitted prompts. Returns whether the input moved.
    fn history_next(&mut self) -> bool {
        let Some(index) = self.history_index else {
            return false;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.composer = self.history[index + 1].clone();
        } else {
            self.history_index = None;
            self.composer.clear();
        }
        self.autocomplete = None;
        true
    }

    /// Return to the live draft after leaving a focused agent/process list.
    fn select_live_input(&mut self) {
        self.composer.clear();
        self.pastes.clear();
        self.attachments.clear();
        self.autocomplete = None;
        self.history_index = None;
    }

    /// Keep the approval prompt on the first pending approval; a new approval
    /// starts a fresh prompt.
    fn sync_prompt(&mut self) {
        let first = self.state.approvals.first();
        if first.map(|approval| approval.id) != self.prompt.as_ref().map(|(id, _)| *id) {
            self.advising = false;
            self.prompt = first.map(|approval| {
                (
                    approval.id,
                    bone_render::approval::approval_prompt(&approval.call),
                )
            });
        }
    }

    /// The approval prompt's live-pane lines, as the TUI draws them.
    /// The approval pane's lines, and how many trailing lines are choices.
    fn approval_lines(
        &self,
        theme: &bone_render::theme::Theme,
    ) -> Option<(Vec<ratatui::text::Line<'static>>, usize)> {
        let (_, prompt) = self.prompt.as_ref()?;
        let lines = bone_render::approval::approval_pane_lines(theme, prompt, self.advising, 100);
        let choices = if self.advising {
            0
        } else {
            prompt.options.len()
        };
        Some((lines, choices))
    }

    /// A tapped or clicked approval choice: select it, then act as Enter.
    fn choose_approval(&mut self, index: usize) {
        let Some((_, prompt)) = self.prompt.as_mut() else {
            return;
        };
        prompt.selected = index.min(prompt.options.len().saturating_sub(1));
        let decision = prompt.decision();
        if matches!(decision, bone_render::prompt::Decision::Advise(_)) {
            self.advising = true;
        } else {
            self.resolve_approval(decision);
        }
    }

    /// A tapped or clicked menu line: answer the pending key request with a
    /// `Click` carrying the line's value (`ui.menu` sends the option index).
    fn click_menu(&mut self, value: String) {
        let Some(id) = self.state.pending_key else {
            return;
        };
        self.reply_key(
            id,
            bone_protocol::KeyEvent {
                code: "Click".into(),
                char: Some(value),
                ctrl: false,
                alt: false,
                shift: false,
            },
        );
    }

    fn reply_key(&mut self, id: u64, key: bone_protocol::KeyEvent) {
        self.state.answer_key(id);
        if !self.command(RuntimeCommand::KeyReply { id, key }) {
            self.state.last_error =
                Some("Could not send the key reply; the connection is closed.".into());
        }
    }

    /// Answer the pending approval: Accept approves, Advise sends the typed
    /// advice as the tool result, Cancel denies and stops the turn.
    fn resolve_approval(&mut self, decision: bone_render::prompt::Decision) {
        let Some((id, _)) = self.prompt.take() else {
            return;
        };
        self.advising = false;
        let outcome = match decision {
            bone_render::prompt::Decision::Accept => CallOutcome::Approve,
            bone_render::prompt::Decision::Advise(advice) => {
                CallOutcome::Blocked(bone_render::approval::advice_reply(&advice))
            }
            bone_render::prompt::Decision::Cancel => {
                self.command(RuntimeCommand::Cancel);
                CallOutcome::Denied
            }
        };
        if self.command(RuntimeCommand::ApprovalReply { id, outcome }) {
            self.state.answered(id);
        }
    }

    /// Approval prompt keys, mirroring the TUI: Up/Down/PageUp/PageDown
    /// select, P shows the full command, Enter confirms, Esc cancels, and
    /// typing while Advise is selected starts advice entry.
    fn handle_approval_keys(&mut self, ui: &mut egui::Ui) {
        self.sync_prompt();
        if self.advising {
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                self.resolve_approval(bone_render::prompt::Decision::Cancel);
                self.clear_input();
            } else if plain_enter(ui) {
                let advice = self.expanded_composer().trim().to_string();
                self.clear_input();
                self.resolve_approval(bone_render::prompt::Decision::Advise(advice));
            }
            return;
        }
        let Some((_, prompt)) = self.prompt.as_mut() else {
            return;
        };
        let key =
            |ui: &mut egui::Ui, key| ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        if key(ui, egui::Key::ArrowUp) {
            prompt.up();
        }
        if key(ui, egui::Key::ArrowDown) {
            prompt.down();
        }
        if key(ui, egui::Key::PageUp) {
            prompt.page_up();
        }
        if key(ui, egui::Key::PageDown) {
            prompt.page_down();
        }
        if key(ui, egui::Key::P) {
            prompt.toggle_peek();
        }
        let advise_selected = prompt.selected == 1;
        if key(ui, egui::Key::Escape) {
            self.resolve_approval(bone_render::prompt::Decision::Cancel);
            return;
        }
        if key(ui, egui::Key::Enter) {
            let decision = prompt.decision();
            if matches!(decision, bone_render::prompt::Decision::Advise(_)) {
                self.advising = true;
            } else {
                self.resolve_approval(decision);
            }
            return;
        }
        // Typing while Advise is selected starts advice entry with that text.
        if advise_selected {
            let typed: String = ui.input_mut(|input| {
                let text = input
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                input
                    .events
                    .retain(|event| !matches!(event, egui::Event::Text(_)));
                text
            });
            if !typed.is_empty() {
                self.composer.push_str(&typed);
                self.advising = true;
            }
        }
    }

    /// Forward the first non-modifier key press to a pending `ctx.ui.key()`
    /// request (interactive menus such as `/config`).
    fn capture_key(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.state.pending_key else {
            // Between an open menu's key requests, keep typing for its next one.
            if self.menu_open() {
                self.typed_ahead.extend(take_typed(ui).chars());
            }
            return;
        };
        ui.memory_mut(|memory| memory.stop_text_input());
        self.typed_ahead.extend(take_typed(ui).chars());
        if let Some(ch) = self.typed_ahead.pop_front() {
            let key = bone_protocol::KeyEvent {
                code: "Char".into(),
                char: Some(ch.to_string()),
                ctrl: false,
                alt: false,
                shift: false,
            };
            self.reply_key(id, key);
            return;
        }
        let captured = ui.input_mut(|input| {
            let captured = input.events.iter().find_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if !keys::is_modifier(*key) => Some((*key, *modifiers)),
                _ => None,
            });
            if let Some((key, modifiers)) = captured {
                input.consume_key(modifiers, key);
                input
                    .events
                    .retain(|event| !matches!(event, egui::Event::Text(_)));
            }
            captured
        });
        if let Some((key, modifiers)) = captured {
            self.reply_key(id, keys::key_event(key, modifiers));
        }
    }

    /// Attach any image files dropped into the window.
    fn apply_drops(&mut self, dropped: &[egui::DroppedFileHandle]) {
        for handle in dropped {
            let path = handle.path();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            match handle.bytes() {
                Ok(bytes) => self.attach(&name, bytes),
                Err(error) => {
                    self.state.last_error =
                        Some(format!("Could not read dropped file {name}: {error}"));
                }
            }
        }
    }
}

/// A tab: a chat with its own daemon session, or one of the TUI's full-screen
/// pages.
enum TabKind {
    Chat(Box<Session>),
    Page(Box<pages::Page>),
}

struct Tab {
    id: u64,
    kind: TabKind,
}

/// The desktop UI. `bone-desktop` runs it via [`main`]; the Android app embeds
/// it with [`DesktopApp::remote`].
pub struct DesktopApp {
    /// Demo mode: self-contained seeded transcript, no daemon.
    demo: bool,
    address: String,
    /// A daemon on another machine (`--ssh <host>`, or an embedder's
    /// connector) used instead of `address`; the remote side starts its own
    /// daemon.
    remote: Option<connection::Target>,
    /// Sidebar shown or hidden by the user; `None` follows the window width.
    sidebar_open: Option<bool>,
    /// The window was narrower than [`NARROW_WIDTH`] last frame.
    narrow: bool,
    ctx: egui::Context,
    tabs: Vec<Tab>,
    /// Index of the selected tab.
    selected: usize,
    /// Tab id of the chat that owns the status bar, keymap, and theme: the
    /// selected tab when it is a chat, otherwise the last chat selected.
    active_chat: u64,
    next_tab_id: u64,
    /// Local-daemon lifecycle used by the auto-connect coordinator.
    daemon_phase: daemon::Phase,
    /// Next frame at which the coordinator retries connecting.
    retry_at: Option<Instant>,
    /// Daemon binary override (tests inject a path).
    daemon_bin: Option<PathBuf>,
    /// The app-spawned daemon process, kept so its death can be detected.
    daemon_child: Option<std::process::Child>,
    /// Transient coordinator message (e.g. why auto-start gave up).
    daemon_notice: String,
    /// Remaining reconnect rounds: None before recovery, Some(0) when exhausted.
    reconnect_budget: Option<u32>,
    /// Most recent daemon conversation list for the history sidebar.
    conversations: Vec<ConversationMeta>,
    /// (chat tab id, request id, send time) of the in-flight `Conversations`
    /// request.
    conversations_request: Option<(u64, u64, Instant)>,
    /// Set when the sidebar should refetch the conversation list.
    conversations_stale: bool,
    sidebar_notice: String,
    /// Last `ViewDiff::SetTheme` payload applied to egui visuals.
    applied_theme: Option<serde_json::Value>,
    /// The TUI color theme resolved from the daemon's theme payload.
    render_theme: bone_render::theme::Theme,
    /// Configurable keymap from the daemon's resolved settings.
    keymap: keymap::Keymap,
    keymap_revision: Option<u64>,
    /// Request id of an in-flight `KeymapDispatch`.
    pending_keymap: Option<u64>,
    /// Locally requested approval mode, until the daemon's settings catch up.
    approval_override: Option<String>,
    /// Whether the live pane is shown (`toggle_panes` keymap action).
    panes_visible: bool,
    /// In-flight `/catalog install|remove` commands.
    catalog_ops: Vec<CatalogOp>,
    local_tx: std::sync::mpsc::Sender<LocalResult>,
    local_rx: std::sync::mpsc::Receiver<LocalResult>,
}

impl DesktopApp {
    fn new(ctx: egui::Context, cli: cli::Cli) -> Self {
        let demo = std::env::var_os("BONE_DESKTOP_DEMO").is_some();
        let mut app = Self::open(ctx, demo);
        if let Some(address) = cli.address {
            app.address = address;
        }
        app.remote = cli.ssh_host.map(connection::Target::Ssh);
        app.connect_all();
        app
    }

    /// The app attached to a remote daemon, for embedding (the Android app).
    pub fn remote(ctx: egui::Context, target: connection::Target) -> Self {
        let mut app = Self::open(ctx, false);
        app.remote = Some(target);
        app.connect_all();
        app
    }

    fn sidebar_visible(&self) -> bool {
        self.sidebar_open.unwrap_or(!self.narrow)
    }

    fn toggle_sidebar(&mut self) {
        self.sidebar_open = Some(!self.sidebar_visible());
    }

    /// Why the app gave up connecting, once it has.
    pub fn connection_failure(&self) -> Option<&str> {
        match &self.daemon_phase {
            daemon::Phase::Stopped(message) => Some(message),
            _ => None,
        }
    }

    fn open(ctx: egui::Context, demo: bool) -> Self {
        let (local_tx, local_rx) = std::sync::mpsc::channel();
        install_look(&ctx);
        let mut session = Session::new(1, None, &ctx, local_tx.clone());
        if demo {
            seed_demo(&mut session);
        }
        Self {
            demo,
            address: daemon::DEFAULT_ADDRESS.into(),
            remote: None,
            sidebar_open: None,
            narrow: false,
            ctx,
            tabs: vec![Tab {
                id: 1,
                kind: TabKind::Chat(Box::new(session)),
            }],
            selected: 0,
            active_chat: 1,
            next_tab_id: 2,
            daemon_phase: daemon::Phase::Probe,
            retry_at: None,
            daemon_bin: None,
            daemon_child: None,
            daemon_notice: String::new(),
            reconnect_budget: None,
            conversations: Vec::new(),
            conversations_request: None,
            conversations_stale: true,
            sidebar_notice: String::new(),
            applied_theme: None,
            render_theme: bone_render::theme::Theme::default(),
            keymap: keymap::Keymap::default(),
            keymap_revision: None,
            pending_keymap: None,
            approval_override: None,
            panes_visible: true,
            catalog_ops: Vec::new(),
            local_tx,
            local_rx,
        }
    }

    // ── Tabs ──────────────────────────────────────────────────────────────

    fn chat(&self, id: u64) -> Option<&Session> {
        self.tabs.iter().find_map(|tab| match &tab.kind {
            TabKind::Chat(session) if tab.id == id => Some(session.as_ref()),
            _ => None,
        })
    }

    fn chat_mut(&mut self, id: u64) -> Option<&mut Session> {
        self.tabs.iter_mut().find_map(|tab| match &mut tab.kind {
            TabKind::Chat(session) if tab.id == id => Some(session.as_mut()),
            _ => None,
        })
    }

    /// The active chat session. There is always at least one chat tab.
    fn session(&self) -> &Session {
        self.chat(self.active_chat)
            .expect("the active chat tab exists")
    }

    fn session_mut(&mut self) -> &mut Session {
        let id = self.active_chat;
        self.chat_mut(id).expect("the active chat tab exists")
    }

    fn sessions_mut(&mut self) -> impl Iterator<Item = &mut Session> {
        self.tabs.iter_mut().filter_map(|tab| match &mut tab.kind {
            TabKind::Chat(session) => Some(session.as_mut()),
            TabKind::Page(_) => None,
        })
    }

    fn selected_page(&mut self) -> Option<&mut pages::Page> {
        match &mut self.tabs.get_mut(self.selected)?.kind {
            TabKind::Page(page) => Some(page.as_mut()),
            TabKind::Chat(_) => None,
        }
    }

    fn select(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        self.selected = index;
        if matches!(tab.kind, TabKind::Chat(_)) {
            self.active_chat = tab.id;
        }
    }

    fn push_tab(&mut self, kind: TabKind) -> usize {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab { id, kind });
        let index = self.tabs.len() - 1;
        self.select(index);
        index
    }

    /// Open a chat tab, optionally loading a stored conversation, and connect it.
    fn new_chat_tab(&mut self, conversation: Option<i64>) {
        let id = self.next_tab_id;
        let session = Session::new(id, conversation, &self.ctx, self.local_tx.clone());
        self.push_tab(TabKind::Chat(Box::new(session)));
        self.connect_all();
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let chats = self
            .tabs
            .iter()
            .filter(|tab| matches!(tab.kind, TabKind::Chat(_)))
            .count();
        if matches!(self.tabs[index].kind, TabKind::Chat(_)) && chats == 1 {
            // The last chat is never closed; it starts over instead.
            if let TabKind::Chat(session) = &mut self.tabs[index].kind {
                session.new_conversation();
            }
            return;
        }
        let closed = self.tabs.remove(index);
        if self.selected >= self.tabs.len() || self.selected > index {
            self.selected = self.selected.saturating_sub(1);
        }
        if closed.id == self.active_chat
            && let Some(tab) = self
                .tabs
                .iter()
                .rev()
                .find(|tab| matches!(tab.kind, TabKind::Chat(_)))
        {
            self.active_chat = tab.id;
        }
        self.select(self.selected.min(self.tabs.len() - 1));
    }

    fn close_tab_id(&mut self, id: u64) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
            self.close_tab(index);
        }
    }

    fn cycle_tab(&mut self, delta: isize) {
        let len = self.tabs.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.select(next);
    }

    fn tab_title(&self, tab: &Tab) -> String {
        match &tab.kind {
            TabKind::Page(page) => page.title.clone(),
            TabKind::Chat(session) => {
                let title = session
                    .conversation_id
                    .and_then(|id| self.conversations.iter().find(|meta| meta.id == id))
                    .map(|meta| meta.title.trim().to_owned())
                    .filter(|title| !title.is_empty())
                    .or_else(|| {
                        session
                            .state
                            .rows
                            .iter()
                            .find(|(role, _)| role == "user")
                            .map(|(_, text)| text.split_whitespace().collect::<Vec<_>>().join(" "))
                    })
                    .unwrap_or_else(|| "new chat".into());
                let title: String = title.chars().take(24).collect();
                if session.state.busy {
                    format!("● {title}")
                } else {
                    title
                }
            }
        }
    }

    /// Open a page tab served by the active chat's connection.
    fn open_page(&mut self, page: pages::Page, effect: pages::Effect) {
        let index = self.push_tab(TabKind::Page(Box::new(page)));
        let id = self.tabs[index].id;
        self.apply_page_effect(id, effect);
    }

    /// Carry out what a page asked for.
    fn apply_page_effect(&mut self, tab_id: u64, effect: pages::Effect) {
        let Some(chat) = self.tabs.iter().find_map(|tab| match &tab.kind {
            TabKind::Page(page) if tab.id == tab_id => Some(page.chat),
            _ => None,
        }) else {
            return;
        };
        match effect {
            pages::Effect::None => {}
            pages::Effect::Close => self.close_tab_id(tab_id),
            pages::Effect::CloseWith(message) => {
                if let Some(session) = self.chat_mut(chat) {
                    session.reply(message);
                }
                self.close_tab_id(tab_id);
            }
            pages::Effect::Command(command) => {
                if let Some(session) = self.chat_mut(chat) {
                    session.command(command);
                }
            }
            pages::Effect::Request(request) => {
                let sent = self.chat_mut(chat).and_then(|session| {
                    let request_id = session.state.next_id();
                    session
                        .command(RuntimeCommand::HostRequest {
                            request_id,
                            request,
                        })
                        .then_some(request_id)
                });
                if let Some(TabKind::Page(page)) = self
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.id == tab_id)
                    .map(|tab| &mut tab.kind)
                {
                    match sent {
                        Some(request_id) => page.pending = Some(request_id),
                        None => page.error = Some("Not connected to the daemon.".into()),
                    }
                }
            }
        }
    }

    /// Route a host response to the page waiting for it.
    fn page_response(&mut self, chat: u64, request_id: u64, response: HostResponse) -> bool {
        let theme = self.render_theme.clone();
        let effect = self.tabs.iter_mut().find_map(|tab| match &mut tab.kind {
            TabKind::Page(page) if page.chat == chat && page.pending == Some(request_id) => {
                Some((tab.id, page.response(response.clone(), &theme)))
            }
            _ => None,
        });
        match effect {
            Some((tab_id, effect)) => {
                self.apply_page_effect(tab_id, effect);
                true
            }
            None => false,
        }
    }

    /// Act on a clicked live-pane row: open a process or agent viewer, or
    /// answer a menu or approval.
    fn open_live_target(&mut self, target: live_pane::Open) {
        let chat = self.active_chat;
        match target {
            live_pane::Open::Click(value) => self.session_mut().click_menu(value),
            live_pane::Open::Approval(index) => self.session_mut().choose_approval(index),
            live_pane::Open::Process(id) => {
                let Some(process) = self
                    .session()
                    .state
                    .processes
                    .iter()
                    .find(|process| process.id == id)
                    .cloned()
                else {
                    return;
                };
                let (page, effect) = pages::Page::process(chat, process);
                self.open_page(page, effect);
            }
            live_pane::Open::Job(id) => {
                let session = self.session();
                let Some(job) = session.state.jobs.iter().find(|job| job.id == id) else {
                    return;
                };
                let messages = bone_render::transcript::job_messages(job, |call| {
                    session.state.tool_display.get(&call.name)
                });
                let title = format!("agent: {}", job.agent);
                let page = pages::Page::transcript(chat, &title, messages);
                self.push_tab(TabKind::Page(Box::new(page)));
            }
        }
    }

    fn effective_address(&self) -> String {
        daemon::ensure_port(self.address.trim())
    }

    fn target(&self) -> connection::Target {
        match &self.remote {
            Some(target) => target.clone(),
            None => connection::Target::Local(self.effective_address()),
        }
    }

    fn connect_all(&mut self) {
        if self.demo {
            return;
        }
        let target = self.target();
        for session in self.sessions_mut() {
            if session.connected || session.connecting {
                continue;
            }
            session.connecting = true;
            session.connect_failed_refused = false;
            session.connect_failed_other = false;
            session.connection_status = "Connecting…".into();
            if session
                .commands
                .send(Command::Connect(target.clone()))
                .is_err()
            {
                session.connecting = false;
                session.connection_status = "Connection worker stopped".into();
            }
        }
    }

    fn drain(&mut self, ctx: &egui::Context) {
        let mut host = Vec::new();
        let mut keymaps = Vec::new();
        let mut stale = false;
        for session in self.sessions_mut() {
            stale |= session.drain_events(ctx);
            let id = session.id;
            host.extend(
                std::mem::take(&mut session.host_responses)
                    .into_iter()
                    .map(|(request, response)| (id, request, response)),
            );
            keymaps.extend(std::mem::take(&mut session.keymap_dispatched));
            let busy = session.state.busy;
            match (session.was_busy, busy) {
                (false, true) => session.turn_started = Some(Instant::now()),
                (true, false) => {
                    session.turn_started = None;
                    // Cleared below if this tab is the one being viewed.
                    session.unread = true;
                    stale = true;
                }
                _ => {}
            }
            session.was_busy = busy;
        }
        self.conversations_stale |= stale;
        if ctx.input(|input| input.focused)
            && let Some(Tab {
                kind: TabKind::Chat(session),
                ..
            }) = self.tabs.get_mut(self.selected)
        {
            session.unread = false;
        }
        for (chat, request_id, response) in host {
            if self
                .conversations_request
                .is_some_and(|(tab, id, _)| tab == chat && id == request_id)
            {
                self.conversations_request = None;
                match response {
                    HostResponse::Conversations(conversations) => {
                        self.conversations = conversations;
                        self.sidebar_notice.clear();
                    }
                    HostResponse::Error { message, .. } => {
                        self.sidebar_notice = format!("Conversation list unavailable: {message}");
                    }
                    _ => {}
                }
            } else if !self.catalog_response(chat, request_id, response.clone()) {
                self.page_response(chat, request_id, response);
            }
        }
        for (request_id, kind) in keymaps {
            if request_id.is_some() && request_id != self.pending_keymap {
                continue;
            }
            self.pending_keymap = None;
            self.apply_keymap_kind(ctx, kind);
        }
        while let Ok(result) = self.local_rx.try_recv() {
            match result {
                LocalResult::Update { tab, reply } => {
                    if let Some(session) = self.chat_mut(tab) {
                        session.reply(reply);
                        session.state.status = "Ready".into();
                    }
                }
                LocalResult::Shell {
                    tab,
                    command,
                    output,
                    is_error,
                } => {
                    if let Some(session) = self.chat_mut(tab) {
                        session.append_shell_result(&command, &output, is_error);
                    }
                }
                LocalResult::Edited { tab, text } => {
                    if let Some(session) = self.chat_mut(tab) {
                        match text {
                            Ok(text) => {
                                let text = text.trim_end_matches(['\r', '\n']);
                                if !text.trim().is_empty() {
                                    session.composer = text.to_string();
                                }
                            }
                            Err(message) => session.reply(message),
                        }
                    }
                }
            }
        }
        self.drain_page_requests();
        self.update_process_pages();
        self.sync_settings();
        self.resolve_danger_approvals();
        for session in self.sessions_mut() {
            session.sync_prompt();
            session.drain_queue();
        }
        self.poll_conversations();
    }

    /// Open the pages chat sessions asked for (`/stats`, `/setup`, `/catalog`).
    fn drain_page_requests(&mut self) {
        let requests: Vec<(u64, PageRequest)> = self
            .sessions_mut()
            .flat_map(|session| {
                let id = session.id;
                std::mem::take(&mut session.page_requests)
                    .into_iter()
                    .map(move |request| (id, request))
            })
            .collect();
        for (chat, request) in requests {
            let (page, effect) = match request {
                PageRequest::Stats => pages::Page::stats(chat, &self.render_theme),
                PageRequest::Setup => pages::Page::setup(chat),
                PageRequest::Catalog => pages::Page::catalog(chat),
                PageRequest::CatalogAction { install, name } => {
                    self.catalog_request(chat, install, name, None);
                    continue;
                }
            };
            self.open_page(page, effect);
        }
    }

    /// Send the next request of a `/catalog install|remove NAME`: the fresh
    /// snapshot first, then the apply against its revision.
    fn catalog_request(
        &mut self,
        chat: u64,
        install: bool,
        name: String,
        revision: Option<String>,
    ) {
        let applying = revision.is_some();
        let request = match revision {
            None => HostRequest::Catalog { refresh: true },
            Some(revision) => bone_render::screens::host::catalog_apply_request(
                revision,
                vec![bone_protocol::CatalogAction {
                    name: name.clone(),
                    action: catalog_kind(install),
                }],
            ),
        };
        let Some(session) = self.chat_mut(chat) else {
            return;
        };
        let request_id = session.state.next_id();
        if session.command(RuntimeCommand::HostRequest {
            request_id,
            request,
        }) {
            self.catalog_ops.push(CatalogOp {
                chat,
                request_id,
                install,
                name,
                applying,
            });
        } else {
            session.reply("Catalog unavailable: not connected to the daemon.");
        }
    }

    /// Route a host response to an in-flight catalog command.
    fn catalog_response(&mut self, chat: u64, request_id: u64, response: HostResponse) -> bool {
        let Some(index) = self
            .catalog_ops
            .iter()
            .position(|op| op.chat == chat && op.request_id == request_id)
        else {
            return false;
        };
        let op = self.catalog_ops.remove(index);
        use bone_render::screens::host;
        if op.applying {
            let reply = match host::catalog_applied(Ok(response)) {
                Ok(result) => {
                    host::catalog_action_message(catalog_kind(op.install), &op.name, &result)
                }
                Err(message) => format!("Catalog failed: {message}"),
            };
            if let Some(session) = self.chat_mut(chat) {
                session.reply(reply);
            }
        } else {
            match host::catalog_snapshot(Ok(response)) {
                Ok(snapshot) => {
                    self.catalog_request(chat, op.install, op.name, Some(snapshot.revision))
                }
                Err(message) => {
                    if let Some(session) = self.chat_mut(chat) {
                        session.reply(format!("Catalog failed: {message}"));
                    }
                }
            }
        }
        true
    }

    /// Feed process pages the latest snapshots; close pages whose process is gone.
    fn update_process_pages(&mut self) {
        let snapshots: HashMap<u64, Vec<bone_protocol::ProcessSnapshot>> = self
            .tabs
            .iter()
            .filter_map(|tab| match &tab.kind {
                TabKind::Chat(session) => Some((tab.id, session.state.processes.clone())),
                TabKind::Page(_) => None,
            })
            .collect();
        let gone: Vec<u64> = self
            .tabs
            .iter_mut()
            .filter_map(|tab| match &mut tab.kind {
                TabKind::Page(page) => snapshots
                    .get(&page.chat)
                    .is_some_and(|processes| !page.update_processes(processes))
                    .then_some(tab.id),
                TabKind::Chat(_) => None,
            })
            .collect();
        for id in gone {
            self.close_tab_id(id);
        }
    }

    /// Keep one `Conversations` request in flight while the list is stale.
    fn poll_conversations(&mut self) {
        if let Some((chat, _, sent)) = self.conversations_request {
            if !self.chat(chat).is_some_and(|session| session.connected) {
                self.conversations_request = None;
                self.conversations_stale = true;
            } else if sent.elapsed() >= HOST_REQUEST_TIMEOUT {
                self.conversations_request = None;
                self.sidebar_notice =
                    "No response from the daemon; the history may be stale.".into();
            }
            return;
        }
        if self.demo || !self.conversations_stale || !self.session().connected {
            return;
        }
        let chat = self.active_chat;
        let session = self.session_mut();
        let request_id = session.state.next_id();
        if session.command(RuntimeCommand::HostRequest {
            request_id,
            request: HostRequest::Conversations { limit: 0 },
        }) {
            self.conversations_request = Some((chat, request_id, Instant::now()));
            self.conversations_stale = false;
        }
    }

    fn settings(&self) -> Option<&serde_json::Value> {
        self.session()
            .state
            .frontend
            .as_ref()
            .map(|frontend| &frontend.settings)
    }

    /// Reparse the keymap when the daemon's settings revision changes.
    fn sync_settings(&mut self) {
        let Some(value) = self.settings() else {
            return;
        };
        let revision = value.get("revision").and_then(serde_json::Value::as_u64);
        if revision.is_some() && revision == self.keymap_revision {
            return;
        }
        self.keymap = keymap::Keymap::parse(value);
        self.keymap_revision = revision;
        self.approval_override = None;
    }

    fn approval_mode(&self) -> &str {
        self.approval_override
            .as_deref()
            .or_else(|| {
                self.settings()?
                    .pointer("/general/approval")
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or("safe")
    }

    fn set_approval_mode(&mut self, mode: &str) {
        if self.session_mut().command(RuntimeCommand::SetApprovalMode {
            mode: mode.to_owned(),
        }) {
            self.approval_override = Some(mode.to_owned());
        }
    }

    /// Approve non-blocked calls already waiting when the mode is Danger.
    fn resolve_danger_approvals(&mut self) {
        if self.approval_mode() != "danger" {
            return;
        }
        for session in self.sessions_mut() {
            let pending: Vec<u64> = session
                .state
                .approvals
                .iter()
                .filter(|approval| approval.blocked.is_none())
                .map(|approval| approval.id)
                .collect();
            for id in pending {
                if session.command(RuntimeCommand::ApprovalReply {
                    id,
                    outcome: CallOutcome::Approve,
                }) {
                    session.state.answered(id);
                }
            }
        }
    }

    /// Consume a configured keybinding; the daemon classifies the action.
    fn handle_keymap(&mut self, ui: &mut egui::Ui) {
        if self.demo || self.keymap.bindings.is_empty() {
            return;
        }
        let mut matched: Option<String> = None;
        ui.input_mut(|input| {
            for binding in &self.keymap.bindings {
                let Some((modifiers, key)) = keymap::parse_key(&binding.key) else {
                    continue;
                };
                if input.consume_key(modifiers, key) {
                    matched = Some(binding.action.clone());
                    break;
                }
            }
        });
        if let Some(action) = matched {
            let session = self.session_mut();
            let request_id = session.state.next_id();
            if session.command(RuntimeCommand::KeymapDispatch {
                request_id: Some(request_id),
                action,
            }) {
                self.pending_keymap = Some(request_id);
            }
        }
    }

    fn apply_keymap_kind(&mut self, ctx: &egui::Context, kind: KeymapDispatchKind) {
        match kind {
            KeymapDispatchKind::Noop => {}
            KeymapDispatchKind::Builtin { action } => match action.as_str() {
                "toggle_panes" => self.panes_visible = !self.panes_visible,
                "cycle_approval_mode" => {
                    let next = if self.approval_mode() == "safe" {
                        "danger"
                    } else {
                        "safe"
                    };
                    self.set_approval_mode(next);
                }
                "paste_image" => self.session_mut().paste_clipboard_image(),
                "cursor_to_start" | "cursor_to_end" => {
                    let char_index = if action == "cursor_to_start" {
                        0
                    } else {
                        self.session().composer.chars().count()
                    };
                    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, editor_id())
                    {
                        state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::one(
                                egui::text::CCursor::new(char_index),
                            )));
                        state.store(ctx, editor_id());
                    }
                }
                _ => {}
            },
            KeymapDispatchKind::Command { text } | KeymapDispatchKind::Prompt { text } => {
                let session = self.session_mut();
                session.composer = text;
                session.attachments.clear();
                session.autocomplete = None;
                session.submit_composer();
            }
        }
    }

    /// Window-level keys, handled before any widget sees them: tab shortcuts,
    /// then either the selected page or the active chat.
    fn handle_keys(&mut self, ui: &mut egui::Ui) {
        // Android's Back button/gesture arrives as BrowserBack. Treat it as Esc
        // so every Esc action (leave a menu, cancel an edit, deny an approval,
        // stop a turn, close a page) has a touch equivalent.
        ui.input_mut(|input| {
            for event in &mut input.events {
                if let egui::Event::Key { key, .. } = event
                    && *key == egui::Key::BrowserBack
                {
                    *key = egui::Key::Escape;
                }
            }
        });
        let consume =
            |ui: &mut egui::Ui, modifiers, key| ui.input_mut(|i| i.consume_key(modifiers, key));
        let ctrl_shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
        if consume(ui, egui::Modifiers::COMMAND, egui::Key::T) {
            self.new_chat_tab(None);
        }
        if consume(ui, egui::Modifiers::COMMAND, egui::Key::W) {
            self.close_tab(self.selected);
        }
        if consume(ui, egui::Modifiers::COMMAND, egui::Key::B) {
            self.toggle_sidebar();
        }
        if consume(ui, ctrl_shift, egui::Key::Tab)
            || consume(ui, egui::Modifiers::COMMAND, egui::Key::PageUp)
        {
            self.cycle_tab(-1);
        }
        if consume(ui, egui::Modifiers::COMMAND, egui::Key::Tab)
            || consume(ui, egui::Modifiers::COMMAND, egui::Key::PageDown)
        {
            self.cycle_tab(1);
        }
        for (n, key) in [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ]
        .into_iter()
        .enumerate()
        {
            if consume(ui, egui::Modifiers::COMMAND, key) {
                self.select(n.min(self.tabs.len() - 1));
            }
        }
        if self.selected_page().is_some() {
            self.page_keys(ui);
            return;
        }

        if self.session().menu_open() {
            self.session_mut().capture_key(ui);
            return;
        }
        self.handle_keymap(ui);
        if !self.session().state.approvals.is_empty() {
            self.session_mut().handle_approval_keys(ui);
        }
        if consume(ui, egui::Modifiers::COMMAND, egui::Key::N) {
            self.session_mut().new_conversation();
        }
        if consume(ui, egui::Modifiers::COMMAND, egui::Key::O) {
            self.session_mut().transcript.toggle_all();
        }
        if consume(ui, egui::Modifiers::ALT, egui::Key::V) {
            self.session_mut().paste_clipboard_image();
        }
        let autocomplete_open = self.session().autocomplete_open();
        let mut opened_target = false;
        if self.session().prompt.is_none()
            && !autocomplete_open
            && let Some(target) = self.session_mut().handle_list_keys(ui)
        {
            opened_target = true;
            self.open_live_target(target);
        }
        if autocomplete_open {
            self.session_mut().clear_agent_focus_for_input(ui);
            if consume(ui, egui::Modifiers::NONE, egui::Key::Escape) {
                self.session_mut().autocomplete = None;
            }
            return;
        }
        if consume(ui, egui::Modifiers::NONE, egui::Key::Escape) && self.session().state.busy {
            self.session_mut().cancel_turn();
        }
        if self.session().live_pane.has_pages() {
            let session = self.session_mut();
            if consume(ui, egui::Modifiers::NONE, egui::Key::Tab) {
                session.live_pane.cycle();
            }
            let rows = live_pane::DEFAULT_PANE_ROWS as i64;
            if consume(ui, egui::Modifiers::NONE, egui::Key::PageUp) {
                session.live_pane.scroll_by(-rows);
            }
            if consume(ui, egui::Modifiers::NONE, egui::Key::PageDown) {
                session.live_pane.scroll_by(rows);
            }
        }
        if !opened_target {
            self.session_mut().clear_agent_focus_for_input(ui);
        }
    }

    /// Forward this frame's key presses and typed text to the selected page.
    fn page_keys(&mut self, ui: &mut egui::Ui) {
        use bone_render::screens::{Key, KeyCode};
        let keys: Vec<Key> = ui.input_mut(|input| {
            let keys = input
                .events
                .iter()
                .flat_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if !keys::is_modifier(*key) => {
                        let key = Key::from(&keys::key_event(*key, *modifiers));
                        // Printable characters arrive as text events.
                        let text = matches!(key.code, KeyCode::Char(_)) && !key.ctrl && !key.alt;
                        (!text).then_some(key).into_iter().collect::<Vec<_>>()
                    }
                    egui::Event::Text(text) => {
                        text.chars().map(|c| Key::plain(KeyCode::Char(c))).collect()
                    }
                    _ => Vec::new(),
                })
                .collect();
            input.events.clear();
            keys
        });
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        let tab_id = self.tabs[self.selected].id;
        if scroll.abs() >= 1.0
            && let Some(page) = self.selected_page()
        {
            page.scroll(-(scroll / 20.0).round() as i64);
        }
        for key in keys {
            let Some(page) = self.selected_page() else {
                return;
            };
            let effect = page.handle_key(key);
            self.apply_page_effect(tab_id, effect);
        }
    }

    /// The composer's frame for `ui.input`, mirroring the TUI: `lines` has rules
    /// above and below (drawn by the caller), `box` a border, `filled` or
    /// `fill: true` the input background; padding is in terminal cells.
    fn input_frame(&self, ui: &egui::Ui) -> egui::Frame {
        let style = &self.session().state.input_style;
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let cell = egui::vec2(
            ui.fonts_mut(|fonts| fonts.glyph_width(&font, 'M')),
            ui.text_style_height(&egui::TextStyle::Monospace),
        );
        let margin = |cells: u16, size: f32| (f32::from(cells) * size).round() as i8;
        let mut frame = egui::Frame::new().inner_margin(egui::Margin::symmetric(
            margin(style.horizontal_padding, cell.x),
            margin(style.vertical_padding, cell.y),
        ));
        let theme = &self.render_theme;
        if style.fill {
            frame =
                frame.fill(grid::to_egui(theme.input_bg).unwrap_or(ui.visuals().extreme_bg_color));
        }
        if style.preset == state::InputPreset::Box {
            let border = grid::to_egui(theme.input_border)
                .unwrap_or(ui.visuals().widgets.noninteractive.bg_stroke.color);
            frame = frame
                .stroke(egui::Stroke::new(1.0, border))
                .corner_radius(egui::CornerRadius::same(theme::CONTROL_RADIUS));
        }
        frame
    }

    /// Apply the daemon's resolved theme when the payload changes.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        // An attachment reset clears the session snapshot before the daemon
        // sends the target conversation authoritative frontend state. The theme
        // is renderer-global, so keep the last applied visuals while that
        // snapshot is in flight instead of treating the missing value as the
        // default theme.
        if let Some(theme) = self.session().state.theme.clone() {
            self.set_theme(ctx, theme);
        }
    }

    /// Apply a resolved theme payload unless it is already applied: the
    /// daemon's, or one an embedder cached from [`DesktopApp::theme`] so the
    /// first frames do not flash the default theme.
    pub fn set_theme(&mut self, ctx: &egui::Context, theme: serde_json::Value) {
        if self.applied_theme.as_ref() == Some(&theme) {
            return;
        }
        install_theme(ctx, &theme);
        self.render_theme =
            serde_json::from_value::<bone_protocol::theme::ThemeSettings>(theme.clone())
                .map(|settings| bone_render::theme::Theme::from_snapshot(&settings))
                .unwrap_or_default();
        self.applied_theme = Some(theme);
        for session in self.sessions_mut() {
            session.transcript.invalidate();
        }
        ctx.request_repaint();
    }

    /// The theme payload currently applied, if any.
    pub fn theme(&self) -> Option<&serde_json::Value> {
        self.applied_theme.as_ref()
    }

    fn schedule_retry(&mut self, ctx: &egui::Context, delay: Duration) {
        let due = Instant::now() + delay;
        self.retry_at = Some(self.retry_at.map_or(due, |at| at.min(due)));
        // A delayed repaint request alone does not reliably wake an idle,
        // unfocused window (seen on Wayland), which stalled reconnects until the
        // pointer moved. A cross-thread repaint always wakes the event loop.
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            ctx.request_repaint();
        });
    }

    fn start_local_daemon(&mut self, ctx: &egui::Context) -> bool {
        let address = match daemon::local_endpoint(&self.effective_address()) {
            Ok(endpoint) => endpoint.to_string(),
            Err(message) => {
                self.stop(message);
                return false;
            }
        };
        let Some(bin) = self.daemon_bin.clone().or_else(daemon::resolve_binary) else {
            self.stop(
                "Could not find the `bone` daemon binary (set BONE_DESKTOP_DAEMON or install bone)."
                    .into(),
            );
            return false;
        };
        let state_dir = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .map(|base| base.join("bone-desktop"));
        let log = daemon::daemon_log_path(state_dir.as_deref());
        match daemon::spawn_daemon(&bin, &address, &log) {
            Ok(child) => {
                self.daemon_child = Some(child);
                self.daemon_phase = daemon::Phase::Starting { attempts: 0 };
                self.daemon_notice.clear();
                self.schedule_retry(ctx, Duration::from_millis(daemon::DAEMON_RETRY_DELAY_MS));
                true
            }
            Err(error) => {
                self.stop(format!("Could not start the local daemon: {error}"));
                false
            }
        }
    }

    fn stop(&mut self, message: String) {
        self.daemon_phase = daemon::Phase::Stopped(message.clone());
        self.daemon_notice = message;
    }

    /// Manual recovery after the coordinator gave up.
    fn retry_now(&mut self) {
        self.daemon_phase = daemon::Phase::Probe;
        self.daemon_notice.clear();
        self.reconnect_budget = None;
        self.retry_at = None;
        self.connect_all();
    }

    /// Frame pump for the daemon lifecycle, run after events are drained: a
    /// connected chat marks the daemon Ready; a refused loopback connect starts
    /// a local daemon; retry ticks reconnect while it boots; a dropped session
    /// drives a bounded reconnect.
    fn pump_daemon(&mut self, ctx: &egui::Context) {
        if let Some(status) = self
            .daemon_child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
        {
            self.daemon_child = None;
            self.reconnect_budget = None;
            for session in self.sessions_mut() {
                session.connected = false;
            }
            if matches!(self.daemon_phase, daemon::Phase::Ready) {
                self.start_local_daemon(ctx);
            } else {
                self.stop(format!(
                    "The local daemon exited before accepting connections ({status})."
                ));
            }
        }
        if self.demo {
            return;
        }
        let (mut refused, mut other_failure, mut mid_drop) = (false, false, false);
        let (mut any_connected, mut unresolved, mut connecting) = (false, false, false);
        let mut failure = None;
        for session in self.sessions_mut() {
            if session.connect_failed_refused || session.connect_failed_other {
                failure.get_or_insert_with(|| session.connection_status.clone());
            }
            refused |= std::mem::take(&mut session.connect_failed_refused);
            other_failure |= std::mem::take(&mut session.connect_failed_other);
            mid_drop |= std::mem::take(&mut session.mid_drop);
            any_connected |= session.connected;
            connecting |= session.connecting;
            unresolved |= !session.connected && !session.connecting;
        }
        if any_connected {
            self.daemon_phase = daemon::Phase::Ready;
            self.daemon_notice.clear();
        }
        if !unresolved {
            if !connecting {
                self.retry_at = None;
                self.reconnect_budget = None;
            }
            return;
        }
        if let Some(at) = self.retry_at
            && Instant::now() >= at
        {
            self.retry_at = None;
            match self.daemon_phase {
                daemon::Phase::Starting { attempts }
                    if attempts + 1 > daemon::MAX_DAEMON_RETRIES =>
                {
                    self.stop("Started the local daemon but it never accepted connections.".into());
                }
                daemon::Phase::Starting { attempts } => {
                    self.daemon_phase = daemon::Phase::Starting {
                        attempts: attempts + 1,
                    };
                    self.connect_all();
                }
                _ => {
                    if let Some(remaining @ 1..) = self.reconnect_budget {
                        self.reconnect_budget = Some(remaining - 1);
                        self.connect_all();
                    }
                }
            }
            return;
        }
        if mid_drop || (self.daemon_phase == daemon::Phase::Ready && (refused || other_failure)) {
            self.conversations_stale = true;
            let budget = *self
                .reconnect_budget
                .get_or_insert(daemon::MAX_RECONNECT_ROUNDS);
            if budget > 0 {
                self.schedule_retry(ctx, Duration::from_millis(daemon::RECONNECT_RETRY_DELAY_MS));
            } else {
                self.stop(format!(
                    "Lost the daemon connection and could not restore it after {} attempts.",
                    daemon::MAX_RECONNECT_ROUNDS
                ));
            }
            return;
        }
        if (refused || other_failure) && self.retry_at.is_none() {
            match self.daemon_phase {
                // The remote `bone stdio` starts its own daemon; a failure here
                // is the link's (auth, host key, missing `bone`), so show it.
                daemon::Phase::Probe if self.remote.is_some() => {
                    let reason = failure.unwrap_or_default();
                    let reason = reason.strip_prefix("Connect failed: ").unwrap_or(&reason);
                    self.stop(format!(
                        "Could not connect over {}: {reason}",
                        self.target().label()
                    ));
                }
                daemon::Phase::Probe => {
                    let address = self.effective_address();
                    if refused
                        && daemon::local_endpoint(&address)
                            .is_ok_and(|endpoint| endpoint.port() == 7878)
                    {
                        self.start_local_daemon(ctx);
                    } else {
                        self.stop(format!(
                            "No daemon responding at {address}. Custom ports never auto-start a daemon."
                        ));
                    }
                }
                daemon::Phase::Starting { .. } => {
                    self.schedule_retry(ctx, Duration::from_millis(daemon::DAEMON_RETRY_DELAY_MS));
                }
                // A new tab failed while others are live: retry it.
                daemon::Phase::Ready => self.connect_all(),
                daemon::Phase::Stopped(_) => {}
            }
        }
    }

    /// Reconcile each chat's transcript cache with its authoritative rows.
    fn sync_transcripts(&mut self) {
        for session in self.sessions_mut() {
            let rows_len = session.state.rows.len();
            if let Some(added) = session.state.pending_prepend.take() {
                let _ = std::mem::take(&mut session.state.changed_rows);
                session.transcript.prepend_rows(added, rows_len);
                session.load_anchor = Some((session.last_bottom_distance, -1.0));
                continue;
            }
            let changed = std::mem::take(&mut session.state.changed_rows);
            if !session.stick_to_bottom && !changed.is_empty() {
                session.has_new_output = true;
            }
            session.transcript.sync(rows_len, &changed);
        }
    }

    /// Open a stored conversation: focus the tab showing it, reuse the active
    /// chat when it is empty, or open a new tab.
    fn open_conversation(&mut self, id: i64) {
        if let Some(index) = self.tabs.iter().position(|tab| {
            matches!(&tab.kind, TabKind::Chat(session) if session.conversation_id == Some(id))
        }) {
            self.select(index);
            return;
        }
        let session = self.session();
        let reusable = session.state.rows.is_empty() && !session.state.busy && session.connected;
        if reusable {
            if let Some(index) = self.tabs.iter().position(|tab| tab.id == self.active_chat) {
                self.select(index);
            }
            self.session_mut().load_conversation(id);
        } else {
            self.new_chat_tab(Some(id));
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        if ui
            .add(egui::Button::new("+ New chat").frame(false))
            .on_hover_text("Ctrl+T")
            .clicked()
        {
            self.new_chat_tab(None);
        }
        ui.separator();
        if !self.sidebar_notice.is_empty() {
            ui.weak(&self.sidebar_notice);
        }
        let mut activity: HashMap<i64, sidebar::Activity> = HashMap::new();
        for tab in &self.tabs {
            if let TabKind::Chat(session) = &tab.kind
                && let Some(id) = session.conversation_id
            {
                let entry = activity.entry(id).or_default();
                entry.open = true;
                entry.running |= session.state.busy;
                entry.unread |= session.unread;
            }
        }
        let mut open = None;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        // Relative stamps ("5m ago") age without any other repaint trigger.
        ui.ctx().request_repaint_after(Duration::from_secs(30));
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for meta in &self.conversations {
                    let activity = activity.get(&meta.id).copied().unwrap_or_default();
                    if sidebar::row(ui, meta, activity, &self.render_theme, now).clicked() {
                        open = Some(meta.id);
                    }
                }
            });
        if let Some(id) = open {
            self.open_conversation(id);
            // A full-window drawer gives way to the chat it opened.
            if self.narrow {
                self.sidebar_open = Some(false);
            }
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut select = None;
        let mut close = None;
        let visuals = ui.visuals().clone();
        let foreground = visuals.text_color();
        let muted = visuals.weak_text_color();
        let accent = visuals.hyperlink_color;
        let separator = visuals.widgets.noninteractive.bg_stroke.color;
        let tab_height = ui.spacing().interact_size.y + 4.0;
        let titles: Vec<(u64, String, bool)> = self
            .tabs
            .iter()
            .map(|tab| {
                (
                    tab.id,
                    self.tab_title(tab),
                    matches!(tab.kind, TabKind::Page(_)),
                )
            })
            .collect();
        egui::ScrollArea::horizontal()
            .id_salt("tab-bar")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (index, (_, title, page)) in titles.iter().enumerate() {
                        let label = if *page {
                            format!("[{title}]")
                        } else {
                            title.clone()
                        };
                        let selected = index == self.selected;
                        let response = ui.add(
                            egui::Button::new(
                                egui::RichText::new(format!(" {label} ")).color(if selected {
                                    foreground
                                } else {
                                    muted
                                }),
                            )
                            .selected(selected)
                            .frame(false)
                            .min_size(egui::vec2(0.0, tab_height)),
                        );
                        if response.clicked() {
                            select = Some(index);
                        }
                        if response.middle_clicked() {
                            close = Some(index);
                        }
                        let close_response = ui
                            .add(
                                egui::Button::new(egui::RichText::new("×").color(if selected {
                                    foreground
                                } else {
                                    muted
                                }))
                                .frame(false)
                                .min_size(egui::vec2(20.0, tab_height)),
                            )
                            .on_hover_text("Close (Ctrl+W)");
                        if close_response.clicked() {
                            close = Some(index);
                        }

                        let tab_rect = response.rect.union(close_response.rect);
                        if ui.is_rect_visible(tab_rect) {
                            if selected {
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(tab_rect.left() + 4.0, tab_rect.bottom() - 1.0),
                                        egui::pos2(tab_rect.right() - 4.0, tab_rect.bottom() - 1.0),
                                    ],
                                    egui::Stroke::new(2.0, accent),
                                );
                            } else if response.hovered() || close_response.hovered() {
                                ui.painter().line_segment(
                                    [
                                        egui::pos2(tab_rect.left() + 4.0, tab_rect.bottom() - 1.0),
                                        egui::pos2(tab_rect.right() - 4.0, tab_rect.bottom() - 1.0),
                                    ],
                                    egui::Stroke::new(1.0, accent.gamma_multiply(0.55)),
                                );
                            }
                            if close_response.hovered() {
                                ui.painter().rect_stroke(
                                    close_response.rect.shrink(2.0),
                                    egui::CornerRadius::same(2),
                                    egui::Stroke::new(1.0, accent.gamma_multiply(0.7)),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            ui.painter().line_segment(
                                [
                                    egui::pos2(
                                        close_response.rect.right() + 4.0,
                                        tab_rect.top() + 7.0,
                                    ),
                                    egui::pos2(
                                        close_response.rect.right() + 4.0,
                                        tab_rect.bottom() - 7.0,
                                    ),
                                ],
                                egui::Stroke::new(1.0, separator.gamma_multiply(0.65)),
                            );
                        }
                        ui.add_space(8.0);
                    }
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("+").color(muted))
                                .frame(false)
                                .min_size(egui::vec2(24.0, tab_height)),
                        )
                        .on_hover_text("New chat (Ctrl+T)")
                        .clicked()
                    {
                        self.new_chat_tab(None);
                    }
                });
            });
        if let Some(index) = select {
            self.select(index);
        }
        if let Some(index) = close {
            self.close_tab(index);
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let session = self.session();
        let state = &session.state;
        let mut token_stats = state.snapshot.to_token_stats();
        if let Some(usage) = state.token_usage {
            token_stats.sent = usage.sent;
            token_stats.received = usage.received;
            token_stats.context_length = usage.context_length;
        }
        let mut model = state.snapshot.provider_model.clone();
        if let Some(target) = &self.remote
            && !model.is_empty()
        {
            model = format!("{} · {model}", target.label());
        }
        if !self.demo && !(session.connected && state.ready) {
            let connection = match &self.daemon_phase {
                daemon::Phase::Stopped(_) => "daemon offline",
                _ if !session.connected => "connecting…",
                _ => "loading…",
            };
            model = if model.is_empty() {
                connection.to_owned()
            } else {
                format!("{connection} · {model}")
            };
        }
        let info = status_bar::info(status_bar::Inputs {
            model,
            incognito: state.snapshot.incognito,
            approval_danger: self.approval_mode() == "danger",
            token_stats,
            queue_len: session.queue.len(),
            busy: state.busy,
            turn_elapsed: session
                .turn_started
                .filter(|_| state.busy)
                .map(|started| started.elapsed()),
            settings: self.settings(),
            view: &state.view,
        });
        status_bar::show(ui, &info, &self.render_theme);
    }
}

/// A full-screen page (or catalog change) a chat asked for with a slash command.
enum PageRequest {
    Stats,
    Setup,
    Catalog,
    /// `/catalog install|remove NAME`: fetch a fresh snapshot, then apply.
    CatalogAction {
        install: bool,
        name: String,
    },
}

/// An in-flight `/catalog install|remove NAME`.
struct CatalogOp {
    chat: u64,
    request_id: u64,
    install: bool,
    name: String,
    /// False while fetching the snapshot, true once the apply was sent.
    applying: bool,
}

fn editor_id() -> egui::Id {
    egui::Id::new("input-editor")
}

impl Session {
    /// The input region: attachments, then a key-capture hint or the editor
    /// with its inline `/` autocomplete rows. While advising on an approval,
    /// the editor takes the advice.
    /// Stop the running turn (Esc, or the touch Stop button).
    fn cancel_turn(&mut self) {
        self.command(RuntimeCommand::Cancel);
        self.state.status = state::CANCEL_STATUS.into();
    }

    /// Keys go to a menu: one is waiting for a key, or its pane (`ui.menu`,
    /// `/config`) is still open between key requests. The input stays out of
    /// the way meanwhile, so it never retakes focus mid-menu (which resets the
    /// on-screen keyboard on every keystroke).
    fn menu_open(&self) -> bool {
        self.state.pending_key.is_some()
            || self
                .state
                .view
                .components
                .iter()
                .any(|component| component.id() == MENU_PANE)
    }

    fn input(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.state.last_error {
            ui.colored_label(ui.visuals().error_fg_color, brief_error(error))
                .on_hover_text(error);
        }
        if !self.attachments.is_empty() {
            let mut remove = None;
            ui.horizontal_wrapped(|ui| {
                for (index, attachment) in self.attachments.iter().enumerate() {
                    if ui
                        .small_button(format!(
                            "[image {} {} KB ✕]",
                            attachment.name,
                            attachment.byte_size / 1024
                        ))
                        .on_hover_text("Remove attachment")
                        .clicked()
                    {
                        remove = Some(index);
                    }
                }
            });
            if let Some(index) = remove {
                self.attachments.remove(index);
            }
        }
        if let Some(blocked) = self
            .state
            .approvals
            .first()
            .and_then(|a| a.blocked.as_ref())
        {
            ui.colored_label(ui.visuals().warn_fg_color, blocked);
        }
        if self.prompt.is_some() && !self.advising {
            ui.weak("Choose in the approval prompt below.");
            keep_keyboard(ui);
            return;
        }
        if self.menu_open() {
            ui.weak("Keys go to the menu below.");
            keep_keyboard(ui);
            return;
        }

        // handle_keys normally consumes these first. Keep a defensive no-op
        // here so an unhandled history-boundary arrow cannot reach multiline
        // TextEdit and move its cursor.
        if !self.autocomplete_open() {
            for key in [egui::Key::ArrowUp, egui::Key::ArrowDown] {
                if plain_key_pressed(ui, key) {
                    ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key));
                }
            }
        }
        let focused = ui.memory(|memory| memory.has_focus(editor_id()));
        self.refresh_autocomplete();
        let mut autocomplete_submit = false;
        if focused && self.autocomplete_open() {
            let key = |ui: &mut egui::Ui, key| {
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key))
            };
            if key(ui, egui::Key::ArrowDown) {
                if let Some(ac) = self.autocomplete.as_mut() {
                    ac.down();
                }
            } else if key(ui, egui::Key::ArrowUp) {
                if let Some(ac) = self.autocomplete.as_mut() {
                    ac.up();
                }
            } else if key(ui, egui::Key::Tab) {
                self.accept_autocomplete();
            } else if plain_enter(ui) {
                autocomplete_submit = self.accept_autocomplete();
            }
        }
        if focused {
            self.collapse_large_pastes(ui);
        }
        let prefix = if self.state.input_style.prefix.is_empty() {
            "> ".to_owned()
        } else {
            self.state.input_style.prefix.clone()
        };
        let rows = (self.composer.matches('\n').count() + 1).clamp(1, 8);
        // Touch screens have no Esc key: pin a Stop button to the row's end.
        let stop = self.state.busy && ui.input(|input| input.has_touch_screen());
        let mut cancel = false;
        let editor = ui
            .horizontal_top(|ui| {
                ui.label(egui::RichText::new(prefix).weak());
                let reserve = if stop { STOP_BUTTON_WIDTH } else { 0.0 };
                let editor = ui.add(
                    egui::TextEdit::multiline(&mut self.composer)
                        .id(editor_id())
                        .desired_width((ui.available_width() - reserve).max(0.0))
                        .desired_rows(rows)
                        .frame(egui::Frame::NONE)
                        .return_key(egui::KeyboardShortcut::new(
                            egui::Modifiers::SHIFT,
                            egui::Key::Enter,
                        )),
                );
                if stop {
                    cancel = ui.button("■ Stop").clicked();
                }
                editor
            })
            .inner;
        if cancel {
            self.cancel_turn();
        }
        if editor.changed() {
            self.refresh_autocomplete();
        }
        if ui.memory(|memory| memory.focused().is_none()) {
            editor.request_focus();
        }
        if let Some(ac) = self
            .autocomplete
            .as_ref()
            .filter(|ac| !ac.matches.is_empty())
        {
            let selected = ac.selected;
            let more = ac.more_count();
            let rows: Vec<_> = ac
                .matches
                .iter()
                .enumerate()
                .skip(ac.scroll_offset)
                .take(commands::MAX_VISIBLE)
                .map(|(index, row)| (index, row.clone()))
                .collect();
            let mut clicked = None;
            for (index, (name, description)) in rows {
                let text = format!("/{name:<12} {description}");
                if ui.selectable_label(index == selected, text).clicked() {
                    clicked = Some(name);
                }
            }
            if more > 0 {
                ui.weak(format!("  +{more} more"));
            }
            if let Some(name) = clicked {
                self.composer = format!("/{name}");
                self.autocomplete = None;
                editor.request_focus();
            }
        }

        if !editor.has_focus() {
            return;
        }
        let steer = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
            || ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::Enter));
        let send = autocomplete_submit || plain_enter(ui);
        if self.state.busy {
            if steer {
                self.steer_composer();
            } else if send {
                self.enqueue_composer();
            }
        } else if send {
            self.submit_composer_in_order();
        } else if steer {
            self.submit_composer();
        }
        if self.composer.is_empty()
            && !self.queue.is_empty()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::D))
        {
            self.queue.clear();
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowUp)) {
            self.history_prev();
        } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowDown)) {
            self.history_next();
        }
    }

    /// Collapse large pastes into a placeholder token, mirroring the TUI. The
    /// paste event is removed so the editor does not also insert the blob.
    fn collapse_large_pastes(&mut self, ui: &mut egui::Ui) {
        let is_large = |event: &egui::Event| matches!(event, egui::Event::Paste(text) if text.chars().count() > PASTE_PLACEHOLDER_THRESHOLD);
        let large: Vec<String> = ui.input(|i| {
            i.events
                .iter()
                .filter(|event| is_large(event))
                .filter_map(|event| match event {
                    egui::Event::Paste(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        });
        if large.is_empty() {
            return;
        }
        ui.input_mut(|i| i.events.retain(|event| !is_large(event)));
        let mut char_index = egui::text_edit::TextEditState::load(ui.ctx(), editor_id())
            .and_then(|state| state.cursor.char_range())
            .map(|range| range.primary.index.0)
            .unwrap_or_else(|| self.composer.chars().count());
        for text in large {
            char_index += self.insert_paste_placeholder(&text, char_index);
        }
        if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), editor_id()) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(char_index),
                )));
            state.store(ui.ctx(), editor_id());
        }
    }

    /// The transcript: connection/load errors, rows, and the scroll helpers.
    fn transcript_view(&mut self, ui: &mut egui::Ui, theme: &bone_render::theme::Theme) {
        if let Some(error) = self.state.last_error.clone()
            && !self.state.ready
            && self.connected
        {
            ui.label(brief_error(&error));
            ui.horizontal(|ui| {
                if ui.button("Retry load").clicked() {
                    self.retry_failed_load();
                }
                if ui.button("New conversation").clicked() {
                    self.new_conversation();
                }
            });
        }
        if self.state.ready
            && self.state.rows.is_empty()
            && let Some(banner) = &self.banner
        {
            let cols = (ui.available_width() / crate::grid::metrics(ui).cell) as u16;
            let lines = bone_render::messages::msg_to_lines(
                &[bone_render::Message::system(banner.clone())],
                theme,
                None,
                cols.max(20),
                false,
            );
            crate::grid::paint_lines(ui, &lines);
        }
        let response = self.transcript.show(
            ui,
            self.stick_to_bottom,
            &self.state.rows,
            &self.state.toolcards,
            &self.state.tool_display,
            theme,
        );
        let max_scroll = (response.content_size.y - response.inner_rect.height()).max(0.0);
        self.stick_to_bottom = (max_scroll - response.state.offset.y).abs() < 8.0;
        // Preserve the reading position across an older-page prepend once the
        // taller content has been measured (it settles a frame later).
        self.last_bottom_distance =
            (response.content_size.y - response.state.offset.y - response.inner_rect.height())
                .max(0.0);
        if let Some((distance, last_h)) = self.load_anchor {
            let height = response.content_size.y;
            let target = (height - distance - response.inner_rect.height()).max(0.0);
            if (response.state.offset.y - target).abs() > 0.5 {
                let mut scroll = response.state;
                scroll.offset.y = target;
                scroll.store(ui.ctx(), response.id);
                ui.ctx().request_repaint();
            }
            self.load_anchor = ((height - last_h).abs() >= 0.5).then_some((distance, height));
        }
        let overlay = |ui: &mut egui::Ui, salt: &str, layout: egui::Layout| {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(salt)
                    .max_rect(response.inner_rect.shrink(8.0))
                    .layout(layout),
            );
            child.set_clip_rect(ui.clip_rect().intersect(response.inner_rect));
            child
        };
        if !self.stick_to_bottom {
            let mut child = overlay(
                ui,
                "jump-latest",
                egui::Layout::bottom_up(egui::Align::Center),
            );
            let label = if self.has_new_output {
                "↓ new output"
            } else {
                "↓ latest"
            };
            if child.button(label).clicked() {
                self.jump_to_latest = true;
            }
        }
        if self.state.has_older {
            let mut child = overlay(
                ui,
                "load-older",
                egui::Layout::top_down(egui::Align::Center),
            );
            let loading = self.state.loading_older;
            if child
                .add_enabled(
                    !loading && !self.state.busy,
                    egui::Button::new(if loading {
                        "loading…"
                    } else {
                        "↑ older messages"
                    }),
                )
                .clicked()
                && let Some(command) = self.state.load_older()
            {
                self.command(command);
            }
        }
        if self.jump_to_latest {
            let mut scroll = response.state;
            scroll.offset.y = max_scroll;
            scroll.store(ui.ctx(), response.id);
            self.stick_to_bottom = true;
            self.jump_to_latest = false;
            ui.ctx().request_repaint();
        }
        if self.stick_to_bottom {
            self.has_new_output = false;
        }
    }
}

/// Whether an exact, unmodified key press is waiting in egui's event queue.
fn plain_key_pressed(ui: &egui::Ui, key: egui::Key) -> bool {
    ui.input(|input| {
        input.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: pressed,
                    pressed: true,
                    modifiers,
                    ..
                } if *pressed == key && *modifiers == egui::Modifiers::NONE
            )
        })
    })
}

/// A plain Enter press (no modifiers), consumed. `consume_key` ignores extra
/// Shift/Alt, so the event is checked for exact modifiers first.
fn plain_enter(ui: &mut egui::Ui) -> bool {
    ui.input(|i| {
        i.events.iter().any(|event| {
            matches!(event,
                egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers, .. }
                if *modifiers == egui::Modifiers::NONE)
        })
    }) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
}

/// Seed a self-contained transcript so the renderer can be exercised (and
/// screenshotted) without a running daemon.
fn seed_demo(session: &mut Session) {
    session.demo = true;
    session.workspace = "demo".into();
    session.state.ready = true;
    session.state.snapshot.provider_model = "demo-model".into();
    session.state.push_row("user", "Show me a Markdown sample.");
    session.state.push_row(
        "assistant",
        "# Bone Desktop\n\nHere is some **bold**, *italic* and `inline code`.\n\n\
         - Markdown rendering\n- Code blocks\n\n```rust\nfn main() {\n    println!(\"Hello\");\n}\n```",
    );
    for (id, name, arguments, content) in [
        (
            "demo-read",
            "read_file",
            serde_json::json!({"path": "src/greeting.rs"}),
            "File: src/greeting.rs\nRange: 1-3\n1 | fn greeting() {\n2 |     println!(\"Hello\");\n3 | }",
        ),
        (
            "demo-edit",
            "edit_file",
            serde_json::json!({"path": "src/greeting.rs"}),
            "\n    edit_file src/greeting.rs (-1 | +1)\n    1   fn greeting() {\n    2 -     println!(\"Hello\");\n    2 +     println!(\"Hello, Bone!\");\n    3   }",
        ),
    ] {
        session.state.reduce(RuntimeEvent::ToolCall {
            id: id.into(),
            name: name.into(),
            summary: String::new(),
            arguments,
        });
        session.state.reduce(RuntimeEvent::ToolResult {
            call_id: id.into(),
            name: name.into(),
            content: content.into(),
            is_error: false,
        });
    }
    session
        .state
        .view
        .components
        .push(bone_protocol::Component::float_from_pane_content(
            &bone_protocol::PaneContent {
                source: "task_list".into(),
                title: "Tasks (1/3)".into(),
                visible_rows: 3,
                scroll: 0,
                placement: None,
                owner: None,
                lines: vec![
                    bone_protocol::PaneLineSpec::Plain("✓ Inspect the tool display".into()),
                    bone_protocol::PaneLineSpec::Plain("◐ Build the live pane".into()),
                    bone_protocol::PaneLineSpec::Plain("○ Verify layout".into()),
                ],
            },
        ));
}

impl eframe::App for DesktopApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl DesktopApp {
    fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.drain(&ctx);
        self.apply_theme(&ctx);
        self.pump_daemon(&ctx);
        self.sync_transcripts();
        self.handle_keys(ui);
        let dropped = ui.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            self.session_mut().apply_drops(&dropped);
        }
        let fill = ui.visuals().panel_fill;
        let side = egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(12, 4));

        self.narrow = ui.available_width() < NARROW_WIDTH;
        let sidebar = self.sidebar_visible();
        egui::Panel::top("tabs")
            .frame(side)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let label = egui::RichText::new("☰").weak();
                    if ui
                        .add(egui::Button::new(label).frame(false))
                        .on_hover_text(if sidebar {
                            "Hide sidebar (Ctrl+B)"
                        } else {
                            "Show sidebar (Ctrl+B)"
                        })
                        .clicked()
                    {
                        self.toggle_sidebar();
                    }
                    self.tab_bar(ui);
                });
            });
        let sidebar_frame = egui::Frame::new()
            .fill(ui.visuals().window_fill)
            .inner_margin(12);
        if sidebar && self.narrow {
            egui::CentralPanel::default()
                .frame(sidebar_frame)
                .show(ui, |ui| self.sidebar(ui));
            return;
        }
        if sidebar {
            egui::Panel::left("history")
                .frame(sidebar_frame)
                .resizable(true)
                .default_size(SIDEBAR_WIDTH)
                .min_size(180.0)
                .max_size(480.0)
                .show(ui, |ui| self.sidebar(ui));
        }

        if self.selected_page().is_some() {
            let theme = self.render_theme.clone();
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(fill).inner_margin(12))
                .show(ui, |ui| {
                    let rect = ui.available_rect_before_wrap();
                    if let Some(page) = self.selected_page() {
                        grid::paint_screen(ui, rect, |frame| page.draw(frame, &theme));
                    }
                    ui.ctx().request_repaint_after(Duration::from_millis(500));
                });
            return;
        }

        let theme = self.render_theme.clone();
        let session = self.session();
        let pages = session.live_pane.pages(
            live_pane::Sources {
                view: &session.state.view,
                jobs: &session.state.jobs,
                processes: &session.state.processes,
                thinking: session.state.live_reasoning.as_deref(),
                queue: &session.queue,
                approval: session.approval_lines(&theme),
            },
            &theme,
        );
        let jobs = session.state.jobs.clone();
        let processes = session.state.processes.clone();
        self.session_mut().live_pane.sync(&pages, &jobs, &processes);
        egui::Panel::bottom("status-bar")
            .frame(side)
            .show_separator_line(false)
            .show(ui, |ui| self.status_bar(ui));
        let mut open = None;
        if self.panes_visible && !pages.is_empty() {
            egui::Panel::bottom("live-pane")
                .frame(side)
                .show_separator_line(false)
                .show(ui, |ui| {
                    open = self.session_mut().live_pane.show(ui, &pages);
                });
        }
        if let Some(target) = open {
            self.open_live_target(target);
        }
        let shells = self
            .session()
            .running_shells(&theme, ui.input(|input| input.time));
        egui::Panel::bottom("input")
            .frame(side)
            .show_separator_line(false)
            .show(ui, |ui| {
                if !shells.is_empty() {
                    grid::paint_lines(ui, &shells);
                }
                let frame = self.input_frame(ui);
                let lines = self.session().state.input_style.preset == state::InputPreset::Lines;
                if lines {
                    ui.separator();
                }
                frame.show(ui, |ui| self.session_mut().input(ui));
                if lines {
                    ui.separator();
                }
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(fill)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                if let daemon::Phase::Stopped(message) = &self.daemon_phase {
                    let message = message.clone();
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(ui.visuals().error_fg_color, message);
                        if ui.button("Retry").clicked() {
                            self.retry_now();
                        }
                    });
                } else if !self.daemon_notice.is_empty() {
                    ui.weak(&self.daemon_notice);
                }
                self.session_mut().transcript_view(ui, &theme);
            });
    }
}

/// Pane id shared by `ui.menu` and `/config` while they take keys.
const MENU_PANE: &str = "interact";

/// Take this frame's typed text, dropping the key presses that produced it.
/// Typed characters win over keys: on-screen keyboards deliver text, and
/// symbols such as `@` have no egui key.
fn take_typed(ui: &mut egui::Ui) -> String {
    ui.input_mut(|input| {
        let typed: String = input
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if !typed.is_empty() {
            input.events.retain(|event| match event {
                egui::Event::Text(_) => false,
                egui::Event::Key { key, modifiers, .. } => {
                    keys::key_event(*key, *modifiers).code != "Char"
                }
                _ => true,
            });
        }
        typed
    })
}

/// Keep the on-screen keyboard up while keys go to a menu or prompt instead of
/// the input field, which is what normally asks for it.
fn keep_keyboard(ui: &egui::Ui) {
    let rect = ui.min_rect();
    ui.output_mut(|output| {
        output.ime = Some(egui::output::IMEOutput {
            purpose: egui::viewport::IMEPurpose::Normal,
            rect,
            cursor_rect: rect,
            should_interrupt_composition: false,
        });
    });
}

/// The app's fonts and default (pre-theme) style, for embedders that draw
/// before a [`DesktopApp`] exists.
pub fn install_look(ctx: &egui::Context) {
    theme::install_fonts(ctx);
    ctx.set_style_of(ctx.theme(), theme::ThemeSettings::default().style());
}

/// Style egui from a resolved theme payload (see [`DesktopApp::theme`]).
pub fn install_theme(ctx: &egui::Context, theme: &serde_json::Value) {
    let settings: theme::ThemeSettings = serde_json::from_value(theme.clone()).unwrap_or_default();
    ctx.set_style_of(ctx.theme(), settings.style());
}

/// The `bone-desktop` binary: parse the command line, then run the GUI.
pub fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match cli::parse(&args) {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    match cli.command {
        cli::Command::Version => {
            println!("{}", cli::version());
            return Ok(());
        }
        cli::Command::Help => {
            println!("{}", cli::usage());
            return Ok(());
        }
        cli::Command::DaemonSide(name) => {
            eprintln!("bone-desktop {name} is not available: the desktop app is a pure client.");
            eprintln!("Run `bone {name}` for the daemon-side operation.");
            std::process::exit(2);
        }
        cli::Command::Gui => {}
    }
    graphics::run(cli)
}

fn catalog_kind(install: bool) -> bone_protocol::CatalogActionKind {
    if install {
        bone_protocol::CatalogActionKind::Install
    } else {
        bone_protocol::CatalogActionKind::Remove
    }
}
