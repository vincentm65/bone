//! Multi-conversation desktop frontend: each tab owns one conversation socket,
//! its own reducer state, composer, and virtualized transcript cache. Only the
//! daemon is
//! authoritative; this UI renders RuntimeEvents and sends RuntimeCommands.
mod activity;
mod catalog;
mod cli;
mod commands;
mod config_view;
mod connection;
mod daemon;
mod file_refs;
mod icons;
mod images;
#[cfg(test)]
mod input_tests;
mod keymap;
mod keys;
mod layout;
mod live_pane;
#[cfg(test)]
mod live_pane_tests;
mod local;
mod markdown;
mod models;
mod palette;
mod panes;
#[cfg(test)]
mod perf_tests;
mod review;
mod setup;
mod state;
mod stats;
mod surface;
#[cfg(test)]
mod surface_tests;
mod task_row;
mod task_ui;
use task_row::{RowIndicator, task_row};
mod theme;
mod tool_display;
mod transcript;
#[cfg(test)]
mod ux_tests;
mod workspace;
mod workspace_ui;

use base64::Engine;
use bone_protocol::tools::CallOutcome;
use bone_protocol::{
    CatalogAction, CatalogActionKind, CatalogSnapshot, CommandAction, ConfigAction, ConfigSchema,
    ConfigSnapshot, ConversationMeta, DateRange, HostRequest, HostResponse, ImageData,
    KeymapDispatchKind, ProviderUpdate, RuntimeCommand, RuntimeEvent, UsageStatsSnapshot,
};
use connection::{Command, Event};
use eframe::egui;
use local::LocalResult;
use state::{State, ToolCard, ToolState};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Bound event-drain work per tab per frame while the worker's bounded channel
/// applies backpressure.
const MAX_EVENTS_PER_FRAME: usize = 256;

/// Maximum recalled-prompt entries kept per tab, matching the TUI's
/// `MAX_INPUT_HISTORY_ENTRIES`.
const MAX_HISTORY: usize = 500;

/// Minimum idle time after a layout-affecting change before the restart
/// layout file is rewritten.
const LAYOUT_SAVE_DEBOUNCE_MS: u64 = 400;

/// Maximum size of a single image attachment (bytes).
const MAX_ATTACHMENT_BYTES: usize = 15 * 1024 * 1024;

/// Pastes longer than this many characters collapse to a `[Pasted text #N +M
/// chars]` placeholder in the composer instead of filling it with the whole
/// blob, mirroring the TUI's `PASTE_PLACEHOLDER_THRESHOLD`.
const PASTE_PLACEHOLDER_THRESHOLD: usize = 500;

/// How long to wait for a host request (stats/catalog) before surfacing an
/// error. Guards against a daemon that accepts the socket but never replies.
const HOST_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Minimum delay before auto-retrying a timed-out host request, so the error
/// notice stays visible instead of being cleared by the next frame's retry.
const HOST_REQUEST_RETRY_DELAY: Duration = Duration::from_secs(30);

fn brief_error(error: &str) -> &'static str {
    if error.to_ascii_lowercase().contains("conversation") {
        "Conversation could not be loaded"
    } else {
        "The requested operation could not be completed"
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    New,
    Load(i64),
}

/// A captured command completion awaiting post-reduce rendering:
/// `(request_id, kind, ok, message, action)`.
type CommandCompletion = (
    Option<u64>,
    String,
    bool,
    Option<String>,
    Option<CommandAction>,
);

/// One image attached to the composer, ready to send with the next prompt.
#[derive(Debug, Clone)]
struct Attachment {
    name: String,
    media_type: String,
    /// base64 STANDARD-encoded image bytes (matches the TUI clipboard convention).
    data_b64: String,
    byte_size: usize,
    /// Stable image-cache key, hashed once so redraws don't re-hash the payload.
    cache_key: String,
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
        let byte_size = bytes.len();
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let cache_key = images::cache_key(name, media_type, &data_b64);
        Ok(Attachment {
            name: name.to_owned(),
            media_type: media_type.to_string(),
            data_b64,
            byte_size,
            cache_key,
        })
    }

    /// Map to the wire `ImageData`; dimensions and hash are unknown here and
    /// left `None` for the daemon to fill in.
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

/// Open a native file picker for a single supported image. Returns the chosen
/// path, `Ok(None)` when the user cancels, or `Err` with a display reason.
fn pick_image_file() -> Result<Option<PathBuf>, String> {
    Ok(rfd::FileDialog::new()
        .add_filter("Images", &["png", "jpeg", "jpg", "webp", "gif"])
        .set_title("Attach an image")
        .pick_file())
}

/// First line of `text`, truncated to a compact one-line queue row.
fn one_line(text: &str) -> String {
    const MAX: usize = 120;
    let first = text.lines().next().unwrap_or("");
    if first.chars().count() <= MAX {
        return first.to_string();
    }
    let end = first
        .char_indices()
        .nth(MAX)
        .map(|(offset, _)| offset)
        .unwrap_or(first.len());
    format!("{}…", &first[..end])
}

/// A UI action a tab's composer requested (e.g. `/stats`). Dialogs live on
/// `DesktopApp`, so the tab queues the request and the app applies it on the
/// next `drain_local`.
enum UiRequest {
    OpenStats,
    OpenJob(String),
    OpenSetup,
    OpenConfig,
    OpenCatalog,
    /// `/catalog install|remove NAME`; the raw argument string is parsed by the
    /// app (which owns the catalog snapshot).
    CatalogAction(String),
    SaveModel(String),
    OpenProvider,
    SwitchProvider(String),
}

/// A large pasted blob held out of the visible composer. `token` is the short
/// placeholder shown in the composer; `content` is the real text spliced back in
/// on submit via [`Tab::expanded_composer`]. Mirrors the TUI's `PasteBlob`.
struct PasteBlob {
    token: String,
    content: String,
}

struct Draft {
    text: String,
    pastes: Vec<PasteBlob>,
    attachments: Vec<Attachment>,
}

struct Tab {
    id: u64,
    /// Seeded transcript used when no daemon is available (BONE_DESKTOP_DEMO).
    demo: bool,
    /// Authoritative conversation id once attached/created; `None` until the
    /// first `conversation_loaded` pins it for a New tab.
    conversation_id: Option<i64>,
    connected: bool,
    connecting: bool,
    /// User requested close; drop the tab once the socket acknowledges.
    closing: bool,
    /// Marked for removal at the next prune (Disconnected while closing, or a
    /// socket that never connected).
    remove: bool,
    /// True while the app-level auto-connect coordinator may reconnect this
    /// tab. Cleared by an explicit user Disconnect; set by any Connect.
    auto: bool,
    /// Set when the most recent connect attempt was refused (peer active). The
    /// coordinator consumes the flag each frame to decide whether to spawn or
    /// retry a local daemon.
    connect_failed_refused: bool,
    /// Set when the most recent connect attempt failed for another reason
    /// (timeout, DNS, malformed address). Lets the coordinator give up with a
    /// helpful message instead of retrying.
    connect_failed_other: bool,
    /// Set by a `Disconnected` event on an already-connected socket (mid-session
    /// drop). Consumed by the coordinator to start a bounded reconnect.
    mid_drop: bool,
    /// `host_api_version` from the daemon's `frontend_state` for this socket,
    /// 0 until observed. Compared against this client's constant for a mismatch
    /// notice.
    host_api_version: u16,
    /// Daemon-reported working directory, never inferred from the client cwd.
    workspace: String,
    endpoint: String,
    connection_status: String,
    state: State,
    composer: String,
    saved_title: Option<(i64, String)>,
    /// Large pastes collapsed to `[Pasted text #N +M chars]` placeholder tokens
    /// in `composer`; the real text is spliced back in on submit.
    pastes: Vec<PasteBlob>,
    /// Monotonic counter for paste-placeholder numbering within this tab.
    paste_seq: usize,
    /// Images staged in the composer to send with the next prompt.
    attachments: Vec<Attachment>,
    stick_to_bottom: bool,
    jump_to_latest: bool,
    has_new_output: bool,
    transcript: transcript::Cache,
    live_pane: live_pane::LivePane,
    image_cache: images::ImageCache,
    repair_at: Instant,
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
    /// Host responses observed on this socket since the last drain, correlated
    /// by request id at the app level and drained by `DesktopApp::drain_all`.
    /// Kept as a buffer (not a single slot) so a broadcast response from
    /// another client cannot clobber a pending one before it is matched.
    host_responses: Vec<(u64, HostResponse)>,
    /// Daemon config snapshots/rejections observed since the last drain,
    /// lifted to the app level by `drain_all` for the provider/model picker.
    /// Each snapshot carries the daemon schema and whether it was flagged
    /// restart-required.
    config_snapshots: Vec<(ConfigSchema, ConfigSnapshot, bool)>,
    config_rejections: Vec<String>,
    /// Keymap dispatch results observed since the last drain, lifted to the app
    /// level by `drain_all` so the resolved action can be applied there.
    keymap_dispatched: Vec<(Option<u64>, KeymapDispatchKind)>,
    /// Request id of the in-flight daemon-run slash command, used to correlate
    /// the matching `CommandComplete` so another client's result is ignored.
    pending_command: Option<u64>,
    /// The `/name arg` echo for the in-flight command, pushed as a user row when
    /// its `CommandComplete` reports `submit`.
    pending_command_echo: Option<String>,
    /// Inline `/` command autocomplete for this tab's composer; `None` when the
    /// buffer is not a slash-command name being typed.
    autocomplete: Option<commands::AutocompleteState>,
    /// egui context, cloned so client-local worker threads can request repaints.
    ctx: egui::Context,
    /// Sender for client-local task results (`/update`, inline shell).
    local_tx: std::sync::mpsc::Sender<LocalResult>,
    /// Prompts typed while a turn is busy, drained in order when it finishes.
    queue: std::collections::VecDeque<String>,
    /// Recovered prompts wait for an explicit resume after a connection/load change.
    queue_paused: bool,
    /// Submitted prompt history for Up/Down recall (oldest first).
    history: Vec<String>,
    /// Cursor into `history` while recalling; `None` when editing a fresh line.
    history_index: Option<usize>,
    /// `/edit` modal editor open flag.
    show_editor: bool,
    /// Working text of the `/edit` modal; applied to the composer on save.
    editor_text: String,
    /// Set by `:q`/`:q!` to request this tab close on the next drain.
    close_request: bool,
    /// App-level UI actions requested by client-side built-ins (`/stats`,
    /// `/setup`, `/catalog`, `/model`, `/provider`), drained by
    /// `DesktopApp::drain_local`.
    pending_ui: Vec<UiRequest>,
    /// Keyboard selection for the first pending approval (0 = Approve,
    /// 1 = Deny), mirroring the TUI prompt's `selected`.
    approval_selected: usize,
    /// Peek mode for the first pending approval's command preview, mirroring
    /// the TUI prompt's `peek_mode`.
    approval_peek: bool,
    /// Approval id the nav state above refers to; reset when the first pending
    /// approval changes so a new prompt starts at the default selection.
    approval_nav_id: Option<u64>,
}

impl Tab {
    fn new(
        id: u64,
        intent: Intent,
        ctx: &egui::Context,
        local_tx: std::sync::mpsc::Sender<LocalResult>,
    ) -> Self {
        let (commands, events) = connection::spawn(ctx.clone());
        let mut tab = Self {
            id,
            demo: false,
            conversation_id: match intent {
                Intent::Load(id) => Some(id),
                Intent::New => None,
            },
            connected: false,
            connecting: false,
            closing: false,
            remove: false,
            auto: true,
            connect_failed_refused: false,
            connect_failed_other: false,
            mid_drop: false,
            host_api_version: 0,
            workspace: String::new(),
            endpoint: String::new(),
            connection_status: "Disconnected".into(),
            image_cache: images::ImageCache::default(),
            state: State::default(),
            composer: String::new(),
            saved_title: None,
            pastes: Vec::new(),
            paste_seq: 0,
            attachments: Vec::new(),
            stick_to_bottom: true,
            jump_to_latest: false,
            has_new_output: false,
            transcript: transcript::Cache::new(),
            live_pane: live_pane::LivePane::default(),
            repair_at: Instant::now(),
            commands,
            events,
            host_responses: Vec::new(),
            config_snapshots: Vec::new(),
            config_rejections: Vec::new(),
            pending_command: None,
            pending_command_echo: None,
            autocomplete: None,
            ctx: ctx.clone(),
            local_tx,
            queue: std::collections::VecDeque::new(),
            queue_paused: false,
            history: Vec::new(),
            history_index: None,
            show_editor: false,
            editor_text: String::new(),
            close_request: false,
            pending_ui: Vec::new(),
            keymap_dispatched: Vec::new(),
            approval_selected: 0,
            approval_peek: false,
            approval_nav_id: None,
        };
        tab.reset_for_attach();
        tab
    }

    /// Re-arm the reducer for a (re)connect: New tabs skip the default-actor
    /// replay; Load/reconnect tabs filter the replay by their conversation id.
    fn reset_for_attach(&mut self) {
        match self.conversation_id {
            Some(id) => self.state.reset(Some(id)),
            None => self.state.reset_new(),
        }
        self.transcript.reset();
        self.image_cache.clear();
        self.pending_command = None;
        self.pending_command_echo = None;
        self.autocomplete = None;
    }

    /// Retry the authoritative load after a conversation-load failure without
    /// mutating or deleting any daemon-owned history.
    fn retry_failed_load(&mut self) -> bool {
        let Some(id) = self.state.expected_id else {
            return false;
        };
        self.state.last_error = None;
        self.state.status = "Loading conversation…".into();
        self.command(RuntimeCommand::LoadConversation { id })
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

    /// Send a daemon-run slash command (`name` + `input`). The daemon answers
    /// with a correlated `CommandComplete`, applied by `handle_event`.
    /// Returns false when not connected.
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

    /// Send the composer. Built-in slash commands are handled client-side (the
    /// daemon only runs Lua commands and protects built-ins); unknown names run
    /// as daemon slash commands; everything else submits a normal prompt.
    fn submit_composer(&mut self) {
        let expanded = self.expanded_composer();
        let text = expanded.trim().to_string();
        if let Some(command) = text.strip_prefix(':').or_else(|| text.strip_prefix('!')) {
            let command = command.trim();
            if command == "q" || command == "q!" {
                self.composer.clear();
                self.autocomplete = None;
                self.close_request = true;
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

    /// Queue an app-level UI request and repaint so `drain_local` applies it.
    fn request_ui(&mut self, request: UiRequest) {
        self.pending_ui.push(request);
        self.ctx.request_repaint();
    }

    /// Reset the composer and its transient input state after a command
    /// consumed the line (attachments, autocomplete, recall cursor).
    fn clear_input(&mut self) {
        self.composer.clear();
        self.pastes.clear();
        self.attachments.clear();
        self.autocomplete = None;
        self.history_index = None;
    }

    /// The composer with every paste placeholder substituted back to its real
    /// content. Equal to `composer` when no large pastes are pending.
    fn expanded_composer(&self) -> String {
        if self.pastes.is_empty() {
            return self.composer.clone();
        }
        let mut out = self.composer.clone();
        for blob in &self.pastes {
            out = out.replace(&blob.token, &blob.content);
        }
        out
    }

    /// Collapse a large paste into a `[Pasted text #N +M chars]` placeholder at
    /// the given char index, keeping the real text for [`expanded_composer`].
    /// Returns the placeholder's char length so callers can advance the caret.
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

    /// Run the image picker and stage the chosen file as a composer attachment.
    /// Shared by the attach button and the configurable `paste_image` binding.
    fn attach_image_dialog(&mut self) {
        match pick_image_file() {
            Ok(Some(path)) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                match std::fs::read(&path) {
                    Ok(bytes) => match Attachment::from_file(&name, bytes) {
                        Ok(attachment) => {
                            self.attachments.push(attachment);
                            self.state.last_error = None;
                        }
                        Err(reason) => self.state.last_error = Some(reason),
                    },
                    Err(error) => {
                        self.state.last_error = Some(format!("Could not read {path:?}: {error}"));
                    }
                }
            }
            Ok(None) => {} // User cancelled the picker.
            Err(error) => self.state.last_error = Some(error),
        }
    }

    /// Handle a built-in slash command client-side. Returns true when `name` is
    /// a built-in this client owns, so `submit_composer` does not also send it
    /// to the daemon (which would answer `unknown command`). Returns false for
    /// unknown names, which fall through to a daemon/Lua slash command.
    fn handle_builtin_command(&mut self, name: &str, arg: &str) -> bool {
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
                self.state.push_row("system", text);
            }
            "clear" | "new" => {
                self.clear_input();
                self.state.clear_conversation();
                if self.command(RuntimeCommand::NewConversation) {
                    self.state.push_row("system", "Chat cleared.");
                } else {
                    self.state
                        .push_row("system", "Not connected to the daemon.");
                }
            }
            "tools" => {
                self.clear_input();
                let input = if arg.trim() == "reload" {
                    "tools reload"
                } else {
                    "tools"
                };
                if !self.run_command("config", input) {
                    self.state
                        .push_row("system", "Not connected to the daemon.");
                }
            }
            "config" => {
                self.clear_input();
                if arg.trim().is_empty() {
                    self.request_ui(UiRequest::OpenConfig);
                } else if !self.run_command("config", arg) {
                    self.state
                        .push_row("system", "Not connected to the daemon.");
                }
            }
            "update" => {
                self.clear_input();
                self.start_update();
            }
            "edit" | "e" => {
                self.clear_input();
                self.open_editor();
            }
            "incognito" => {
                self.clear_input();
                let enabled = match arg.trim() {
                    "" => !self.state.snapshot.incognito,
                    "on" => true,
                    "off" => false,
                    other => {
                        self.state.push_row(
                            "system",
                            format!("Unknown option `{other}` — usage: /incognito [on|off]"),
                        );
                        return true;
                    }
                };
                if self.command(RuntimeCommand::SetIncognito { enabled }) {
                    self.state.push_row(
                        "system",
                        format!("Incognito {}.", if enabled { "on" } else { "off" }),
                    );
                } else {
                    self.state
                        .push_row("system", "Not connected to the daemon.");
                }
            }
            "model" => {
                self.clear_input();
                let arg = arg.trim();
                if arg.is_empty() {
                    let text = format!(
                        "{} ({})",
                        self.state.snapshot.provider_model, self.state.snapshot.provider_id
                    );
                    self.state.push_row("system", text);
                } else {
                    self.request_ui(UiRequest::SaveModel(arg.to_string()));
                }
            }
            "provider" => {
                self.clear_input();
                let arg = arg.trim();
                if arg.is_empty() {
                    self.request_ui(UiRequest::OpenProvider);
                } else {
                    self.request_ui(UiRequest::SwitchProvider(arg.to_string()));
                }
            }
            "stats" => {
                self.clear_input();
                self.request_ui(UiRequest::OpenStats);
            }
            "setup" => {
                self.clear_input();
                self.request_ui(UiRequest::OpenSetup);
            }
            "catalog" => {
                self.clear_input();
                let arg = arg.trim();
                if arg.is_empty() {
                    self.request_ui(UiRequest::OpenCatalog);
                } else {
                    self.request_ui(UiRequest::CatalogAction(arg.to_string()));
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

    /// Client-local `/update`: run `bone update` in a background process and
    /// report its exit code as a system row when it finishes.
    fn start_update(&mut self) {
        self.start_update_with(daemon::resolve_binary());
    }

    /// `/update` with an explicit resolved binary (split out so tests can drive
    /// the no-binary path without touching the environment or spawning).
    fn start_update_with(&mut self, binary: Option<PathBuf>) {
        match binary {
            Some(binary) => {
                self.state.status = "Checking for updates…".into();
                local::spawn_update(self.ctx.clone(), self.local_tx.clone(), self.id, binary);
            }
            None => {
                self.state.push_row("system", local::update_reply(None));
            }
        }
    }

    /// Client-local `/edit`: open the native modal editor seeded with the current
    /// draft. The GUI cannot suspend to a terminal, so this replaces the TUI's
    /// external-editor handoff with an in-app text area (documented deviation).
    fn open_editor(&mut self) {
        self.editor_text = String::new();
        self.show_editor = true;
        self.history_index = None;
    }

    /// Client-local `:`/`!`: run the command in a shell on this machine and show
    /// its output as a local tool row, folded into the daemon transcript.
    fn start_inline_shell(&mut self, command: &str) {
        self.composer.clear();
        self.attachments.clear();
        self.autocomplete = None;
        self.state.status = "Running inline command…".into();
        local::spawn_shell(
            self.ctx.clone(),
            self.local_tx.clone(),
            self.id,
            command.to_string(),
        );
    }

    /// Render a finished inline-shell command: a local tool row plus the folded
    /// `$ cmd\n<output>` message the daemon keeps in the conversation.
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
            label: Some(local::shell_label(command)),
            show_result: None,
            eager: None,
        });
        let _ = self.command(RuntimeCommand::AppendMessage {
            role: "user".into(),
            content: local::shell_transcript_text(command, output),
        });
    }

    /// The `/edit` modal editor. Applying replaces the composer draft with the
    /// edited text (empty text keeps the previous draft, matching the TUI).
    fn editor_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_editor {
            return;
        }
        let mut open = self.show_editor;
        let mut apply = false;
        let mut cancel = false;
        crate::surface::Surface::new("Edit message", "Apply your edits to the current draft.")
            .size(680.0, 440.0)
            .body_scroll(true)
            .show(ctx, &mut open, |ui| {
                ui.label("Edit the draft, then Apply to place it in the composer.");
                ui.add(
                    egui::TextEdit::multiline(&mut self.editor_text)
                        .desired_width(f32::INFINITY)
                        .desired_rows(12)
                        .code_editor(),
                );
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if apply {
            let text = self.editor_text.trim_end_matches(['\r', '\n']).to_string();
            if !text.trim().is_empty() {
                self.composer = text;
            }
            self.show_editor = false;
        } else if cancel || !open {
            self.show_editor = false;
        }
    }

    /// Recompute the inline `/` autocomplete from the current composer buffer.
    /// Keeps the existing selection when the query and command set are
    /// unchanged, so per-frame calls do not fight arrow-key navigation.
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
        let stale = self
            .autocomplete
            .as_ref()
            .map(|ac| ac.all_commands() != all.as_slice())
            .unwrap_or(true);
        if stale {
            self.autocomplete = Some(commands::AutocompleteState::new(all));
        }
        if let Some(ac) = self.autocomplete.as_mut() {
            ac.update(query);
        }
    }

    /// Accept the highlighted autocomplete command: fill the composer with
    /// `/name` and close the popup.
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
            self.composer.clear();
            self.pastes.clear();
            self.attachments.clear();
            self.autocomplete = None;
            self.state.busy = true;
            self.state.status = "Sending…".into();
            // Started, not the click, creates the authoritative user row.
        }
    }

    /// Apply the result of an in-flight daemon slash command. Ignored unless the
    /// request id correlates with the one this tab sent, so another client's
    /// broadcast never renders here.
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

        // A reply-bearing action (config_action) replaces the displayed output
        // with its status reply; submit is false in that case (daemon-enforced).
        if let Some(action) = action
            && let Some(reply) = self.apply_command_action(action)
        {
            output = reply;
        }

        if submit && !output.is_empty() {
            // The daemon already pushed `output` as the user message and runs the
            // turn; show the `/cmd` echo and render it. Never SubmitPrompt.
            if let Some(echo) = echo {
                self.state.push_row("user", echo);
            }
            self.state.busy = true;
            self.state.status = "Working…".into();
        } else if !output.is_empty() {
            let role = if display_role.as_deref() == Some("assistant") {
                "assistant"
            } else {
                "system"
            };
            self.state.push_row(role, output);
        }
    }

    /// Send the daemon command(s) a frontend action requests. Returns a status
    /// reply string when the action is config-related.
    fn apply_command_action(&mut self, action: CommandAction) -> Option<String> {
        if let Some(messages) = action.conversation_replace {
            self.command(RuntimeCommand::ReplaceConversation { messages });
        }
        if let Some(load) = action.conversation_load
            && let Some(id) = load.conversation_id
        {
            self.command(RuntimeCommand::LoadConversation { id });
        }
        action
            .config_action
            .map(|action| self.apply_config_action(action))
    }

    /// Apply a config/runtime mutation requested by a command. Native has no
    /// config-await machinery yet, so it fires the command and reports a status
    /// reply (the daemon's ConfigChanged/ConfigSnapshot updates the picker).
    fn apply_config_action(&mut self, action: ConfigAction) -> String {
        match action {
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
        }
    }

    /// Returns true when the tab's conversation pin changed (a New tab got
    /// attached to a daemon conversation), which affects the restart layout.
    fn drain_events(&mut self, ctx: &egui::Context) -> bool {
        let mut layout_changed = false;
        for _ in 0..MAX_EVENTS_PER_FRAME {
            let Ok(event) = self.events.try_recv() else {
                break;
            };
            if self.handle_event(event) {
                layout_changed = true;
            }
        }
        if !self.events.is_empty() {
            ctx.request_repaint();
        }
        // Only repair a busy/lagged attachment. No idle polling/repaint timer.
        if self.connected && self.state.repairing {
            if Instant::now() >= self.repair_at {
                let command = self.state.synchronize();
                self.command(command);
                self.repair_at = Instant::now() + Duration::from_millis(500);
            }
            ctx.request_repaint_after(self.repair_at.saturating_duration_since(Instant::now()));
        }
        layout_changed
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
                let command = match self.conversation_id {
                    Some(id) => RuntimeCommand::LoadConversation { id },
                    None => RuntimeCommand::NewConversation,
                };
                if !self.command(command) {
                    self.connection_status =
                        "Connection worker stopped before attach could be sent".into();
                }
                false
            }
            Event::Disconnected(reason) => {
                if self.closing {
                    self.remove = true;
                    return false;
                }
                // A drop of an already-established socket (not a failed connect
                // attempt) is a mid-session drop; the app-level coordinator
                // consumes it to start a bounded reconnect.
                self.mid_drop = self.connected && daemon::is_mid_session_drop(&reason);
                self.connected = false;
                self.connecting = false;
                self.state.ready = false;
                // Interactive requests cannot be answered after the socket is
                // gone. Drop both approval and key-capture UI state so a stale
                // modal cannot consume input or offer a reply to a dead peer.
                self.state.approvals.clear();
                self.state.pending_key = None;
                self.state.busy = false;
                self.connection_status = reason.clone();
                self.queue_paused |= !self.queue.is_empty();
                if reason.starts_with("Connect failed") {
                    // Only a failed connect attempt is a daemon-reachability
                    // signal; mid-session drops use a different message and
                    // must never auto-restart anything.
                    if daemon::is_refused(&reason) {
                        self.connect_failed_refused = true;
                    } else {
                        self.connect_failed_other = true;
                    }
                }
                false
            }
            Event::Runtime(event) => {
                // Host responses are app-level (conversation picker); buffer
                // every one until `drain_all` (the tab's reducer ignores them).
                if let RuntimeEvent::HostResponse {
                    request_id,
                    response,
                } = &event
                {
                    self.host_responses.push((*request_id, response.clone()));
                }
                // Daemon config updates are app-level too (provider/model
                // picker); buffer them until `drain_all`. Command completions
                // are captured for post-reduce rendering (below).
                let mut command_complete: Option<CommandCompletion> = None;
                match &event {
                    RuntimeEvent::ConfigSnapshot {
                        schema, snapshot, ..
                    } => {
                        self.config_snapshots
                            .push((schema.clone(), snapshot.clone(), false));
                    }
                    RuntimeEvent::ConfigChanged {
                        schema,
                        snapshot,
                        restart_required,
                        ..
                    } => {
                        self.config_snapshots.push((
                            schema.clone(),
                            snapshot.clone(),
                            *restart_required,
                        ));
                    }
                    RuntimeEvent::ConfigMutationRejected { error, .. } => {
                        self.config_rejections.push(error.clone());
                    }
                    RuntimeEvent::FrontendState {
                        host_api_version,
                        cwd,
                        ..
                    } => {
                        self.host_api_version = *host_api_version;
                        self.workspace = cwd.clone().unwrap_or_default();
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
                // Unsent prompts are local drafts. Preserve them across loads,
                // but require review before sending into a recovered/new session.
                let conversation_loaded = matches!(event, RuntimeEvent::ConversationLoaded { .. });
                if let Some(command) = self.state.reduce(event) {
                    self.command(command);
                }
                if !self.queue.is_empty()
                    && (conversation_loaded
                        || before.is_some() && self.state.snapshot.conversation_id != before)
                {
                    self.queue_paused = true;
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

    /// Whether the composer can be queued for after the current turn: connected,
    /// ready, busy, and holding non-empty text. Images are not preserved in the
    /// queue, matching the TUI (which resets the input including images).
    fn can_queue(&self) -> bool {
        self.connected && self.state.ready && self.state.busy && !self.composer.trim().is_empty()
    }

    /// Queue the current composer text to send once the turn finishes, then clear
    /// the composer. Mirrors the TUI's busy-submit path: images are dropped.
    fn enqueue_composer(&mut self) {
        if !self.can_queue() {
            return;
        }
        if self.queue.is_empty() {
            self.queue_paused = false;
        }
        self.queue
            .push_back(self.expanded_composer().trim().to_string());
        self.composer.clear();
        self.pastes.clear();
        self.attachments.clear();
        self.autocomplete = None;
        self.history_index = None;
    }

    /// Move a queued prompt up one position (no-op at the top).
    fn move_queued_up(&mut self, index: usize) {
        if index > 0 && index < self.queue.len() {
            self.queue.swap(index, index - 1);
        }
    }

    /// Move a queued prompt down one position (no-op at the bottom).
    fn move_queued_down(&mut self, index: usize) {
        if index + 1 < self.queue.len() {
            self.queue.swap(index, index + 1);
        }
    }

    /// Send a queued prompt before the others by moving it to the front.
    fn send_queued_next(&mut self, index: usize) {
        if index < self.queue.len()
            && let Some(text) = self.queue.remove(index)
        {
            self.queue.push_front(text);
        }
    }

    /// Pull a queued prompt back into the composer for editing, dropping its
    /// attachments (they were never preserved) and resetting autocomplete/history.
    fn edit_queued(&mut self, index: usize) {
        if index < self.queue.len()
            && let Some(text) = self.queue.remove(index)
        {
            self.composer = text;
            self.attachments.clear();
            self.autocomplete = None;
            self.history_index = None;
        }
    }

    /// Remove a queued prompt at `index`.
    fn remove_queued(&mut self, index: usize) {
        if index < self.queue.len() {
            self.queue.remove(index);
        }
    }

    /// Whether the composer can steer a running turn: connected, ready, busy, and
    /// holding non-empty text. Steer is text-only, so images are not required.
    fn can_steer(&self) -> bool {
        self.connected && self.state.ready && self.state.busy && !self.composer.trim().is_empty()
    }

    /// Steer the agent mid-turn, mirroring the TUI's Ctrl/Alt+Enter when busy: the
    /// turn continues and the text is injected into the transcript. Images are
    /// dropped (steer is text-only), and the text is recorded for history recall.
    fn steer_composer(&mut self) {
        if !self.can_steer() {
            return;
        }
        let text = self.expanded_composer().trim().to_string();
        if self.command(RuntimeCommand::Steer { text }) {
            self.record_history();
            self.composer.clear();
            self.pastes.clear();
            self.attachments.clear();
            self.autocomplete = None;
            self.history_index = None;
            self.state.status = "Steering…".into();
        }
    }

    fn resume_queue(&mut self) -> bool {
        if !self.connected || !self.state.ready || self.closing {
            return false;
        }
        self.queue_paused = false;
        self.ctx.request_repaint();
        true
    }

    /// Send the next queued prompt once the tab is idle and its composer is
    /// empty, mirroring the TUI's `drain_queue_when_input_empty`. One prompt per
    /// call: `submit_composer` re-marks the tab busy, so the next item waits for
    /// the following turn.
    fn drain_queue(&mut self) {
        if self.queue.is_empty()
            || self.queue_paused
            || self.closing
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

    /// Remember the submitted composer text for Ctrl/Cmd+Up recall, deduping an
    /// existing equal entry and capping the list, matching the TUI's history.
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

    /// Recall the previous submitted prompt into the composer (Ctrl/Cmd+Up).
    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_index = Some(index);
        self.composer = self.history[index].clone();
        self.autocomplete = None;
    }

    /// Recall the next submitted prompt, or clear to a fresh line past the newest
    /// (Ctrl/Cmd+Down), matching the TUI's `history_down`.
    fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.composer = self.history[index + 1].clone();
        } else {
            self.history_index = None;
            self.composer.clear();
        }
        self.autocomplete = None;
    }

    /// Composer keyboard shortcuts, mirroring the TUI's input actions. Called
    /// while the editor has focus. Ctrl+U/Ctrl+W word/line deletion is handled
    /// natively by egui's `TextEdit`; Escape never discards a desktop draft.
    fn handle_input_shortcuts(&mut self, ui: &mut egui::Ui) {
        // Ctrl/Cmd+Up/Down recall submitted prompts; plain Up/Down keep moving
        // the caret in the multiline editor.
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowUp)) {
            self.history_prev();
        } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowDown)) {
            self.history_next();
        }
    }

    /// Attach any image files dropped into the window to this tab's composer.
    /// Non-image or oversized files surface a notice in the composer error slot.
    fn apply_drops(&mut self, dropped: &[egui::DroppedFileHandle]) {
        if self.demo {
            return;
        }
        for handle in dropped {
            let path = handle.path().to_path_buf();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let bytes = match handle.bytes() {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.state.last_error =
                        Some(format!("Could not read dropped file {name}: {error}"));
                    continue;
                }
            };
            match Attachment::from_file(&name, bytes) {
                Ok(attachment) => {
                    self.attachments.push(attachment);
                    self.state.last_error = None;
                }
                Err(reason) => self.state.last_error = Some(reason),
            }
        }
    }

    /// Sidebar status indicator and its color for this tab's row: an animated
    /// spinner while the turn is live and running, a check while it is waiting on
    /// the user (e.g. a tool approval), and static glyphs for other states.
    fn status_indicator(&self) -> (RowIndicator, egui::Color32) {
        let (_, indicator, color) = self.navigation_status();
        (indicator, color)
    }

    fn navigation_status(&self) -> (&'static str, RowIndicator, egui::Color32) {
        let muted = egui::Color32::from_rgb(150, 150, 150);
        let attention = egui::Color32::from_rgb(235, 190, 80);
        if self.demo {
            return (
                "Demo",
                RowIndicator::Glyph("◆"),
                egui::Color32::from_rgb(140, 170, 255),
            );
        }
        if self.state.last_error.is_some() {
            return (
                "Error",
                RowIndicator::Glyph("✕"),
                egui::Color32::from_rgb(235, 90, 90),
            );
        }
        if self.closing {
            return ("Closing", RowIndicator::None, muted);
        }
        if self.connecting {
            return ("Connecting", RowIndicator::Spinner, attention);
        }
        if !self.connected {
            return ("Disconnected", RowIndicator::Glyph("○"), muted);
        }
        if self.state.needs_approval() {
            return ("Needs approval", RowIndicator::Glyph("!"), attention);
        }
        if self.state.pending_key.is_some() {
            return ("Waiting for input", RowIndicator::Glyph("?"), attention);
        }
        if self.state.busy {
            return (
                "Running",
                RowIndicator::Spinner,
                egui::Color32::from_rgb(110, 200, 120),
            );
        }
        if self.queue_paused && !self.queue.is_empty() {
            return ("Queue paused", RowIndicator::Glyph("!"), attention);
        }
        if self.has_new_output {
            return (
                "Unread output",
                RowIndicator::Glyph("•"),
                egui::Color32::from_rgb(79, 156, 249),
            );
        }
        if !self.composer.is_empty() || !self.attachments.is_empty() {
            return ("Draft", RowIndicator::Glyph("•"), muted);
        }
        ("Ready", RowIndicator::None, muted)
    }

    fn title(&self) -> String {
        if self.demo {
            return "Demo".into();
        }
        if let Some((id, title)) = &self.saved_title
            && self.conversation_id == Some(*id)
            && !title.trim().is_empty()
        {
            return one_line(title);
        }
        let title = self.state.short_title();
        // Unattached tabs without history all read "New conversation"; derive
        // the label from the composer draft instead so open tabs stay
        // distinguishable. Daemon titles and renamed conversations are never
        // touched — this only replaces the default fallback.
        if title == "New conversation" && !self.composer.trim().is_empty() {
            let draft: String = self
                .composer
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let mut shortened: String = draft.chars().take(46).collect();
            if shortened != draft {
                shortened.push('…');
            }
            if !shortened.is_empty() {
                return shortened;
            }
        }
        title
    }
}

struct DesktopApp {
    /// Demo mode: self-contained seeded transcript, no daemon, no persistence.
    demo: bool,
    address: String,
    tabs: Vec<Tab>,
    /// Cached index of the focused tab in the window currently being rendered.
    /// Workspace owns selection; conversation commands retain stable tab IDs.
    selected: usize,
    workspace: workspace::Workspace,
    window_ui: workspace_ui::WindowUi,
    sidebar_notice: String,
    history_search: String,
    last_pointer: Option<egui::Pos2>,
    next_tab_id: u64,
    /// Where the restart layout file lives; `None` disables persistence
    /// (demo mode or no writable home).
    layout_path: Option<PathBuf>,
    /// Set when a layout-affecting change is pending a debounced save.
    layout_dirty_since: Option<Instant>,
    /// Local-daemon lifecycle used by the auto-connect coordinator.
    daemon_phase: daemon::Phase,
    /// Next frame at which the coordinator retries connecting auto tabs.
    retry_at: Option<Instant>,
    /// Daemon binary override (tests inject a path; otherwise resolved from
    /// the environment when first needed).
    daemon_bin: Option<PathBuf>,
    /// Pid of a daemon this app spawned, shown in the Server dialog.
    daemon_pid: Option<u32>,
    /// Transient coordinator message (e.g. why auto-start gave up).
    daemon_notice: String,
    /// Server dialog open flag.
    show_server: bool,
    /// Most recent daemon conversation list for the sidebar picker.
    conversations: Vec<ConversationMeta>,
    /// (tab id, request id) of the in-flight `Conversations` request; `None`
    /// while none is pending.
    conversations_request: Option<(u64, u64)>,
    /// When the conversation list request was sent, so a wedged daemon cannot
    /// leave the sidebar on "Loading tasks…" forever.
    conversations_request_at: Option<Instant>,
    /// True once a conversation list has been received (or the daemon reported
    /// an error); a manual refresh clears it to refetch.
    conversations_loaded: bool,
    /// Latest daemon config snapshot for the provider/model picker. `GetConfig`
    /// responses are broadcast (not request-correlated), so any connected tab
    /// can refresh it; a rejected mutation clears it to force a refetch.
    config: Option<ConfigSnapshot>,
    /// Locally requested or daemon-confirmed Danger state. This bridges the
    /// short window where another connection can deliver a request evaluated
    /// under Safe before the daemon processes `SetApprovalMode`.
    danger_mode_active: bool,
    /// Latest daemon config schema (pages/fields) paired with `config`, used by
    /// the schema-driven config dialog. Refreshed alongside `config`.
    config_schema: Option<ConfigSchema>,
    /// Id of the tab that issued the pending `GetConfig`; `None` while none is
    /// in flight. Abandoned if that tab loses its socket.
    config_request: Option<u64>,
    /// Schema-driven config dialog open flag.
    show_config: bool,
    /// Per-path text buffers; dirty drafts survive daemon snapshot refreshes.
    config_edits: std::collections::HashMap<String, String>,
    config_ui: config_view::SettingsUi,
    /// Inline status/error line under the config dialog.
    config_notice: String,
    /// Last `ViewDiff::SetTheme` payload applied to egui visuals, so the style
    /// is only rebuilt when the daemon's theme actually changes.
    applied_theme: Option<serde_json::Value>,
    /// Configurable keymap resolved from the daemon's `FrontendState` settings.
    /// Empty until the first settings snapshot; native's hardcoded Ctrl
    /// shortcuts remain the fallback when no binding matches.
    keymap: keymap::Keymap,
    /// `revision` of the settings payload the current `keymap` was parsed from,
    /// so the bindings are only reparsed when the daemon's config changes.
    keymap_revision: Option<u64>,
    /// (target tab id, request id) of an in-flight `KeymapDispatch`, used to
    /// correlate the matching `KeymapDispatched` and route the result back to the
    /// tab that requested it (another client's result is ignored).
    pending_keymap: Option<(u64, u64)>,
    /// Background process/job viewer open flag.
    palette: palette::Palette,
    command_input: Option<task_ui::CommandInput>,
    review: review::Review,
    show_activity: bool,
    /// Conversation whose daemon-owned processes/jobs are shown in Activity.
    activity_tab: Option<u64>,
    /// Fullscreen live-output process viewer state; `None` when closed.
    process_view: Option<activity::ProcessViewer>,
    /// Full job-transcript viewer state; `None` when closed.
    job_view: Option<activity::JobViewer>,
    /// Keyboard-selected row in the Activity dialog (flat index: processes
    /// first, then jobs), mirroring the TUI's selectable-list model.
    activity_selected: usize,
    /// Whether the daemon-owned float panes are shown (toggled by the
    /// `toggle_panes` keymap action, mirroring the TUI's `panes_visible`).
    panes_visible: bool,
    /// The float pane currently keyboard-focused, as `(tab id, float id)`.
    active_pane: Option<(u64, live_pane::PageId)>,
    /// Frontend scroll offsets added to a float's daemon scroll, keyed by
    /// `(tab id, float id)`. Lets the keyboard scroll a pane without the daemon.
    pane_scroll: std::collections::HashMap<(u64, String), i64>,
    /// Token-stats dashboard open flag (Phase 6).
    show_stats: bool,
    /// Latest daemon usage snapshot for the stats dashboard.
    stats: Option<UsageStatsSnapshot>,
    /// (tab id, request id) of the in-flight `Stats` request; `None` while none
    /// is pending. Abandoned if that tab loses its socket.
    stats_request: Option<(u64, u64)>,
    /// When the in-flight `Stats` request was sent, used to time it out so a
    /// daemon that accepts the socket but never replies surfaces an error.
    stats_request_at: Option<Instant>,
    /// Earliest time to auto-retry stats after a timeout, so the error notice
    /// stays visible instead of being cleared by the next frame's retry.
    stats_retry_at: Option<Instant>,
    /// Frontend-local usage view index (0 today … 4 all time).
    stats_mode: usize,
    /// Applied custom date range, if any. When set, the dashboard renders the
    /// daemon's range reply instead of the view index buckets.
    stats_custom: Option<DateRange>,
    /// When the last stats snapshot was received, for the "refreshed Ns ago" label.
    stats_refreshed: Option<Instant>,
    /// Editable `(start, end)` fields of the custom date-range picker.
    stats_picker: Option<(String, String)>,
    /// Inline status/error line under the stats dialog.
    stats_notice: String,
    /// Catalog browser open flag (Phase 6).
    show_catalog: bool,
    /// Latest daemon catalog snapshot.
    catalog: Option<CatalogSnapshot>,
    /// (tab id, request id) of the in-flight `Catalog`/`CatalogApply` request;
    /// `None` while none is pending. Abandoned if that tab loses its socket.
    catalog_request: Option<(u64, u64)>,
    /// When the in-flight catalog request was sent, used to time it out so a
    /// daemon that accepts the socket but never replies surfaces an error.
    catalog_request_at: Option<Instant>,
    /// Earliest time to auto-retry the catalog after a timeout, so the error
    /// notice stays visible instead of being cleared by the next retry.
    catalog_retry_at: Option<Instant>,
    /// Inline status/error line under the catalog dialog.
    catalog_notice: String,
    /// A `/catalog install|remove` waiting for the first snapshot so it can be
    /// applied against a known revision: `(requesting tab, action)`.
    pending_catalog_action: Option<(u64, CatalogAction)>,
    /// Persistent multi-select state for the catalog browser (checked rows,
    /// touched set, per-item apply outcomes, result-phase banner).
    catalog_view: catalog::CatalogView,
    /// Provider/model dialog open flag.
    show_provider: bool,
    /// Stable id of the tab that opened the provider/model chooser. Commands
    /// remain pinned to this tab even if selection changes while the chooser is open.
    provider_origin_tab: Option<u64>,
    /// Provider onboarding dialog open flag.
    show_setup: bool,
    /// State machine for the provider onboarding dialog (snapshot + editable
    /// key, wiped on submit/close).
    setup_ui: setup::SetupUi,
    /// (tab id, request id) of the in-flight setup `Setup`/`SetupApply`
    /// request; `None` while none is pending. Abandoned if that tab loses its
    /// socket (which also aborts the machine).
    setup_request: Option<(u64, u64)>,
    /// Set once the onboarding dialog has been auto-offered for this session,
    /// so an actionable snapshot opens it a single time (not every frame).
    setup_offered: bool,
    /// CLI `--provider`: activate this provider once the config snapshot loads.
    cli_provider: Option<String>,
    /// True while the CLI provider switch is in flight, so it is not re-sent
    /// every frame before the daemon's config broadcast confirms it.
    cli_provider_sent: bool,
    /// CLI `--model`: set the active provider's model once that provider is
    /// active (so `--provider X --model Y` targets X, not the old provider).
    cli_model: Option<String>,
    /// Editable model text in the provider dialog.
    model_field: String,
    /// Provider id the `model_field` currently displays; a different active
    /// provider resyncs the field from the snapshot.
    model_field_provider: String,
    model_picker: models::Picker,
    /// Transient message inside the provider dialog (switch in flight, a
    /// daemon rejection, or a restart-required flag).
    provider_notice: String,
    /// Conversation id being renamed inline in the sidebar; `None` when idle.
    rename_target: Option<i64>,
    /// Working title shown in the sidebar while `rename_target` is set.
    rename_field: String,
    /// (id, title) of the conversation awaiting the inline delete confirmation.
    delete_target: Option<(i64, String)>,
    pending_delete: Option<i64>,
    /// Live display settings, mirrored into the restart layout on change.
    display: layout::Preferences,
    /// Id of the conversation a pending rename/delete mutation targets, so a
    /// failed response can be reported as an update error, not a list error.
    mutation_target: Option<i64>,
    /// Remaining reconnect rounds: None before recovery, Some(0) when exhausted.
    /// A successful tab must not reset the budget of other disconnected tabs.
    reconnect_budget: Option<u32>,
    /// The app-spawned daemon process, kept so its death can be detected and a
    /// new one respawned; `None` unless this app started it.
    daemon_child: Option<std::process::Child>,
    /// Non-empty when a connected daemon reports a `host_api_version` that
    /// differs from this client's constant.
    version_notice: String,
    /// Stable tab id awaiting explicit stop/discard confirmation.
    close_target: Option<u64>,
    /// Sender half of the client-local result channel; cloned into each tab.
    local_tx: std::sync::mpsc::Sender<LocalResult>,
    /// Receiver half; drained once per frame in `drain_local`.
    local_rx: std::sync::mpsc::Receiver<LocalResult>,
}

impl DesktopApp {
    fn new(ctx: egui::Context, cli: cli::Cli) -> Self {
        let demo = std::env::var_os("BONE_DESKTOP_DEMO").is_some();
        let layout_path = if demo { None } else { layout::state_path() };
        let mut app = Self::open(ctx.clone(), demo, layout_path);
        app.apply_cli(cli);
        app.begin();
        app
    }

    /// Build the app from an explicit layout path (`None` disables persistence),
    /// restoring the saved tabs/drafts/order when present. Split out so tests
    /// can drive the restore without mutating process-global environment.
    fn open(ctx: egui::Context, demo: bool, layout_path: Option<PathBuf>) -> Self {
        let (local_tx, local_rx) = std::sync::mpsc::channel();
        let mut app = Self {
            demo,
            address: daemon::DEFAULT_ADDRESS.into(),
            tabs: Vec::new(),
            selected: 0,
            workspace: workspace::Workspace::default(),
            window_ui: workspace_ui::WindowUi::default(),
            sidebar_notice: String::new(),
            history_search: String::new(),
            last_pointer: None,
            next_tab_id: 1,
            layout_path,
            layout_dirty_since: None,
            daemon_phase: daemon::Phase::Probe,
            retry_at: None,
            daemon_bin: None,
            daemon_pid: None,
            daemon_notice: String::new(),
            show_server: false,
            conversations: Vec::new(),
            conversations_request: None,
            conversations_request_at: None,
            conversations_loaded: false,
            config: None,
            danger_mode_active: false,
            config_schema: None,
            config_request: None,
            show_config: false,
            config_edits: std::collections::HashMap::new(),
            config_ui: config_view::SettingsUi::default(),
            config_notice: String::new(),
            applied_theme: None,
            keymap: keymap::Keymap::default(),
            keymap_revision: None,
            pending_keymap: None,
            palette: palette::Palette::default(),
            command_input: None,
            review: review::Review::default(),
            show_activity: false,
            activity_tab: None,
            process_view: None,
            job_view: None,
            activity_selected: 0,
            panes_visible: true,
            active_pane: None,
            pane_scroll: std::collections::HashMap::new(),
            show_stats: false,
            stats: None,
            stats_request: None,
            stats_request_at: None,
            stats_retry_at: None,
            stats_mode: 1,
            stats_custom: None,
            stats_refreshed: None,
            stats_picker: None,
            stats_notice: String::new(),
            show_catalog: false,
            catalog: None,
            catalog_request: None,
            catalog_request_at: None,
            catalog_retry_at: None,
            catalog_notice: String::new(),
            pending_catalog_action: None,
            catalog_view: catalog::CatalogView::default(),
            show_provider: false,
            provider_origin_tab: None,
            show_setup: false,
            setup_ui: setup::SetupUi::default(),
            setup_request: None,
            setup_offered: false,
            cli_provider: None,
            cli_provider_sent: false,
            cli_model: None,
            model_field: String::new(),
            model_field_provider: String::new(),
            model_picker: models::Picker::default(),
            provider_notice: String::new(),
            rename_target: None,
            rename_field: String::new(),
            delete_target: None,
            pending_delete: None,
            display: layout::Preferences::default(),
            mutation_target: None,
            reconnect_budget: None,
            daemon_child: None,
            version_notice: String::new(),
            close_target: None,
            local_tx,
            local_rx,
        };
        // Install the native visual baseline before daemon theme snapshots arrive.
        // A later resolved snapshot replaces these defaults through `apply_theme`,
        // preserving canonical theme precedence.
        theme::install_fonts(&ctx);
        ctx.set_style_of(ctx.theme(), theme::ThemeSettings::default().style());
        if demo {
            app.add_demo_tab(&ctx);
            return app;
        }
        match &app.layout_path {
            // No layout file yet: a fresh single New tab.
            None => app.add_tab(Intent::New, &ctx),
            Some(path) => match layout::load(path) {
                Ok(None) => app.add_tab(Intent::New, &ctx),
                Err(error) => {
                    app.add_tab(Intent::New, &ctx);
                    app.sidebar_notice = format!("Could not restore layout: {error}");
                }
                Ok(Some(layout)) => {
                    app.address = layout.address;
                    for tab in layout.tabs {
                        let intent = match tab.conversation_id {
                            Some(id) => Intent::Load(id),
                            None => Intent::New,
                        };
                        app.add_tab(intent, &ctx);
                        // Drafts are restored programmatically, so they never
                        // mark the layout dirty on their own (editor.changed()
                        // stays false for non-typed text).
                        app.tabs.last_mut().expect("tab just added").composer = tab.draft;
                    }
                    if app.tabs.is_empty() {
                        // Restored with every tab closed: show the empty state.
                        app.selected = 0;
                    } else {
                        app.selected = layout.selected.min(app.tabs.len() - 1);
                    }
                    // Old layouts become one or two independent tab groups.
                    let prefs = layout.preferences;
                    let legacy_split = layout.legacy_split;
                    ctx.set_zoom_factor((prefs.zoom_percent as f32 / 100.0).clamp(0.75, 2.0));
                    app.display = prefs;
                    let ids: Vec<_> = app.tabs.iter().map(|tab| tab.id).collect();
                    app.workspace = layout.workspace.unwrap_or_else(|| {
                        workspace::Workspace::from_legacy(
                            &ids,
                            app.selected,
                            legacy_split.active,
                            legacy_split.tab,
                        )
                    });
                    app.workspace.normalize(&ids);
                    app.sync_window_selection(app.workspace.active_window);
                }
            },
        }
        app
    }

    fn add_tab(&mut self, intent: Intent, ctx: &egui::Context) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs
            .push(Tab::new(id, intent, ctx, self.local_tx.clone()));
        self.workspace.add_tab(self.window_ui.rendering, id);
        self.selected = self.tabs.len() - 1;
    }

    /// Seed a self-contained transcript so the renderer can be exercised (and
    /// screenshotted) without a running daemon.
    fn add_demo_tab(&mut self, ctx: &egui::Context) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let (commands, events) = connection::spawn(ctx.clone());
        let mut tab = Tab {
            queue_paused: false,
            saved_title: None,
            id,
            demo: true,
            conversation_id: None,
            connected: false,
            connecting: false,
            closing: false,
            remove: false,
            auto: false,
            connect_failed_refused: false,
            connect_failed_other: false,
            mid_drop: false,
            host_api_version: 0,
            workspace: "Demo workspace".into(),
            endpoint: String::new(),
            connection_status: "Demo mode".into(),
            image_cache: images::ImageCache::default(),
            state: State::default(),
            composer: String::new(),
            pastes: Vec::new(),
            paste_seq: 0,
            attachments: Vec::new(),
            stick_to_bottom: true,
            jump_to_latest: false,
            has_new_output: false,
            transcript: transcript::Cache::new(),
            live_pane: live_pane::LivePane::default(),
            repair_at: Instant::now(),
            commands,
            events,
            host_responses: Vec::new(),
            config_snapshots: Vec::new(),
            config_rejections: Vec::new(),
            keymap_dispatched: Vec::new(),
            pending_command: None,
            pending_command_echo: None,
            autocomplete: None,
            ctx: ctx.clone(),
            local_tx: self.local_tx.clone(),
            queue: std::collections::VecDeque::new(),
            history: Vec::new(),
            history_index: None,
            show_editor: false,
            editor_text: String::new(),
            close_request: false,
            pending_ui: Vec::new(),
            approval_selected: 0,
            approval_peek: false,
            approval_nav_id: None,
        };
        tab.state.push_row("user", "Show me a Markdown sample.");
        tab.state.push_row(
            "assistant",
            "# Bone Desktop\n\
             \n\
             Here is some **bold**, *italic*, ~~strikethrough~~ and `inline code`.\n\
             \n\
             ## Features\n\
             - Markdown rendering\n\
             - Code blocks with copy\n\
             - [Safe links](https://example.com) open in the browser\n\
             \n\
             > A block quote, indented to the side.\n\
             \n\
             1. Ordered\n\
             2. List\n\
             \n\
             ```rust\n\
             fn main() {\n\
                 println!(\"Hello, Bone Desktop!\");\n\
             }\n\
             ```",
        );
        let tool = tab.state.push_row(
            "tool: shell",
            "exit code: 0\nstdout:\n$ cargo test -p bone-desktop\nall tests passed\n",
        );
        tab.state.toolcards[tool] = Some(ToolCard {
            name: "shell".into(),
            state: ToolState::Done,
            args: Some("cargo test -p bone-desktop".into()),
            label: Some("cargo test -p bone-desktop".into()),
            show_result: None,
            eager: None,
        });
        tab.state.ready = true;
        for (id, name, arguments, content) in [
            (
                "demo-read",
                "read_file",
                serde_json::json!({"path": "src/greeting.rs"}),
                "File: src/greeting.rs\nRange: 1-3\n1 | fn greeting() {\n2 |     println!(\"Hello\");\n3 | }",
            ),
            (
                "demo-create",
                "create_file",
                serde_json::json!({"path": "notes/review.txt", "content": "Reviewed the tool display."}),
                "wrote notes/review.txt (26 bytes, 1 line)",
            ),
            (
                "demo-edit",
                "edit_file",
                serde_json::json!({"path": "src/greeting.rs", "old_text": "    println!(\"Hello\");", "new_text": "    println!(\"Hello, Bone!\");"}),
                "\n    edit_file src/greeting.rs (-1 | +1)\n    1   fn greeting() {\n    2 -     println!(\"Hello\");\n    2 +     println!(\"Hello, Bone!\");\n    3   }",
            ),
        ] {
            tab.state.reduce(bone_protocol::RuntimeEvent::ToolCall {
                id: id.into(),
                name: name.into(),
                summary: String::new(),
                arguments,
            });
            tab.state.reduce(bone_protocol::RuntimeEvent::ToolResult {
                call_id: id.into(),
                name: name.into(),
                content: content.into(),
                is_error: false,
            });
        }
        tab.state.push_row(
            "assistant",
            "File changes stay visible. Expand a call for details, or choose More → Tool calls → Verbose.",
        );
        tab.state
            .view
            .components
            .push(bone_protocol::Component::float_from_pane_content(
                &bone_protocol::PaneContent {
                    source: "task_list".into(),
                    title: "Tasks (1/3)".into(),
                    visible_rows: 8,
                    scroll: 0,
                    lines: vec![
                        bone_protocol::PaneLineSpec::Spans {
                            spans: vec![bone_protocol::PaneSpanSpec {
                                text: "✓ Inspect the current tool display".into(),
                                fg: Some("muted".into()),
                                modifiers: vec!["strike".into()],
                            }],
                            bg: None,
                        },
                        bone_protocol::PaneLineSpec::Plain("◐ Build reusable live panes".into()),
                        bone_protocol::PaneLineSpec::Plain("○ Verify layout and navigation".into()),
                    ],
                },
            ));
        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        tab.state.jobs = vec![
            bone_protocol::JobSnapshot {
                id: "demo-researcher".into(),
                agent: "Researcher".into(),
                task: "Review tool presentation".into(),
                title: "Review tool presentation".into(),
                status: bone_protocol::JobStatus::Running,
                started_at,
                token_sent: 1400,
                token_received: 320,
                provider: "Demo".into(),
                activity: Some("Inspecting the desktop chat components".into()),
                events: vec![bone_protocol::JobEventSnapshot::TextDelta {
                    text: "Reviewing task rows and live-pane layout.".into(),
                }],
            },
            bone_protocol::JobSnapshot {
                id: "demo-reviewer".into(),
                agent: "Reviewer".into(),
                task: "Check layout and keyboard navigation".into(),
                title: "Check layout and keyboard navigation".into(),
                status: bone_protocol::JobStatus::Queued,
                started_at,
                token_sent: 0,
                token_received: 0,
                provider: "Demo".into(),
                activity: None,
                events: vec![],
            },
        ];
        tab.state.status = "Demo mode (BONE_DESKTOP_DEMO)".into();
        self.workspace.add_tab(self.window_ui.rendering, tab.id);
        self.tabs.push(tab);
        self.selected = self.tabs.len() - 1;
    }

    /// Remove acknowledged-closed tabs. Returns whether any tab was removed so
    /// the caller can mark the restart layout dirty.
    fn prune_closed(&mut self) -> bool {
        let removed: Vec<_> = self
            .tabs
            .iter()
            .filter(|tab| tab.remove)
            .map(|tab| tab.id)
            .collect();
        if removed.is_empty() {
            return false;
        }
        for id in removed {
            self.workspace.remove_tab(id);
        }
        self.tabs.retain(|tab| !tab.remove);
        self.sync_window_selection(self.window_ui.rendering);
        true
    }

    fn drain_all(&mut self, ctx: &egui::Context) {
        let mut layout_changed = false;
        let mut host_responses: Vec<(u64, u64, HostResponse)> = Vec::new();
        let mut config_snapshots: Vec<(ConfigSchema, ConfigSnapshot, bool)> = Vec::new();
        let mut config_rejections: Vec<String> = Vec::new();
        let mut keymap_dispatched: Vec<(Option<u64>, KeymapDispatchKind)> = Vec::new();
        for tab in &mut self.tabs {
            layout_changed |= tab.drain_events(ctx);
            for (request_id, response) in tab.host_responses.drain(..) {
                host_responses.push((tab.id, request_id, response));
            }
            for dispatched in tab.keymap_dispatched.drain(..) {
                keymap_dispatched.push(dispatched);
            }
            if !tab.demo {
                for snapshot in tab.config_snapshots.drain(..) {
                    config_snapshots.push(snapshot);
                }
                for rejection in tab.config_rejections.drain(..) {
                    config_rejections.push(rejection);
                }
            }
        }
        // Drop request-correlation slots whose sending tab lost its socket, so a
        // stale id can never match a later response.
        if let Some((tab_id, _)) = self.stats_request
            && !self
                .tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.connected)
        {
            self.stats_request = None;
            self.stats_request_at = None;
        }
        if let Some((tab_id, _)) = self.catalog_request
            && !self
                .tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.connected)
        {
            self.catalog_request = None;
            self.catalog_request_at = None;
        }
        // Apply replies already buffered by the transport before expiring their
        // correlation slots: a slow render must not turn a valid reply into a timeout.
        for (tab_id, request_id, response) in host_responses {
            if self
                .review
                .pending
                .is_some_and(|(t, id, _)| t == tab_id && id == request_id)
            {
                self.review.apply(response);
            } else if self
                .conversations_request
                .is_some_and(|(t, id)| t == tab_id && id == request_id)
            {
                self.conversations_request = None;
                self.conversations_request_at = None;
                self.apply_host_response(response);
            } else if self
                .setup_request
                .is_some_and(|(t, id)| t == tab_id && id == request_id)
            {
                self.setup_request = None;
                self.apply_setup_response(response);
            } else if self
                .stats_request
                .is_some_and(|(t, id)| t == tab_id && id == request_id)
            {
                self.stats_request = None;
                self.stats_request_at = None;
                self.apply_stats_response(response);
            } else if self
                .catalog_request
                .is_some_and(|(t, id)| t == tab_id && id == request_id)
            {
                self.catalog_request = None;
                self.catalog_request_at = None;
                self.apply_catalog_response(response);
            }
        }
        // Only unanswered requests can time out. Keep failures visible rather
        // than leaving a spinner forever or continuously retrying a stalled daemon.
        let now = Instant::now();
        if self.conversations_request.is_some_and(|_| {
            self.conversations_request_at
                .is_some_and(|at| now >= at + HOST_REQUEST_TIMEOUT)
        }) {
            self.conversations_request = None;
            self.conversations_request_at = None;
            self.conversations_loaded = true;
            self.sidebar_notice = if self.mutation_target.take().is_some() {
                "Conversation update was not confirmed: the daemon did not reply.".into()
            } else {
                format!(
                    "No response from the daemon after {}s; the task list may be unavailable.",
                    HOST_REQUEST_TIMEOUT.as_secs()
                )
            };
        }
        if self.stats_request.is_some_and(|_| {
            self.stats_request_at
                .is_some_and(|at| now >= at + HOST_REQUEST_TIMEOUT)
        }) {
            self.stats_request = None;
            self.stats_request_at = None;
            self.stats_retry_at = Some(now + HOST_REQUEST_RETRY_DELAY);
            self.stats_notice = format!(
                "No response from the daemon after {}s; the connection may be stalled. Use Refresh to retry.",
                HOST_REQUEST_TIMEOUT.as_secs()
            );
        }
        if self.catalog_request.is_some_and(|_| {
            self.catalog_request_at
                .is_some_and(|at| now >= at + HOST_REQUEST_TIMEOUT)
        }) {
            self.catalog_request = None;
            self.catalog_request_at = None;
            self.catalog_retry_at = Some(now + HOST_REQUEST_RETRY_DELAY);
            self.catalog_notice = format!(
                "No response from the daemon after {}s; the connection may be stalled. Use Refresh to retry.",
                HOST_REQUEST_TIMEOUT.as_secs()
            );
        }
        for snapshot in config_snapshots {
            self.apply_config_snapshot(snapshot);
        }
        for rejection in config_rejections {
            self.apply_config_rejection(rejection);
        }
        // Events are reduced per tab before config broadcasts are applied here.
        // Resolve only after those broadcasts so an authoritative Safe update
        // wins over a stale local Danger request in the same drain.
        self.resolve_danger_approvals();
        for (request_id, kind) in keymap_dispatched {
            self.apply_keymap_dispatched(ctx, request_id, kind);
        }
        self.sync_keymap();
        self.sync_task_titles();
        if layout_changed {
            self.note_layout_change(ctx);
        }
        self.poll_conversations();
        self.poll_config();
        self.poll_setup();
        self.check_host_api_versions();
    }

    /// Drain client-local task results (`/update`, inline shell), drain queued
    /// prompts for idle tabs, and act on `:q`/`:q!` close requests. Called once
    /// per frame after daemon events so the UI reflects both sources in order.
    fn drain_local(&mut self) {
        while let Ok(result) = self.local_rx.try_recv() {
            match result {
                LocalResult::Update { tab_id, reply } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                        tab.state.push_row("system", reply);
                        tab.state.status = "Ready".into();
                    }
                }
                LocalResult::Shell {
                    tab_id,
                    command,
                    output,
                    is_error,
                } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
                        tab.append_shell_result(&command, &output, is_error);
                    }
                }
            }
        }
        let close_ids: Vec<u64> = self
            .tabs
            .iter()
            .filter(|tab| tab.close_request)
            .map(|tab| tab.id)
            .collect();
        for id in close_ids {
            if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
                self.tabs[index].close_request = false;
                self.close_tab(index);
            }
        }
        // Apply app-level UI requests queued by client-side built-ins.
        let ui_requests: Vec<(u64, UiRequest)> = self
            .tabs
            .iter_mut()
            .flat_map(|tab| {
                let id = tab.id;
                tab.pending_ui.drain(..).map(move |request| (id, request))
            })
            .collect();
        for (tab_id, request) in ui_requests {
            self.apply_ui_request(tab_id, request);
        }
        // Send the next queued prompt for any tab that has gone idle.
        for tab in &mut self.tabs {
            tab.drain_queue();
        }
    }

    /// Compare the `host_api_version` reported by a connected daemon against
    /// this client's constant and surface a warning label on a mismatch.
    fn check_host_api_versions(&mut self) {
        let observed = self
            .tabs
            .iter()
            .find(|tab| !tab.demo && tab.host_api_version > 0)
            .map(|tab| tab.host_api_version);
        self.version_notice = match observed {
            Some(version) if version != bone_protocol::HOST_API_VERSION => {
                format!(
                    "Daemon host API version {version} does not match this client ({}); \
                     some features may not work.",
                    bone_protocol::HOST_API_VERSION
                )
            }
            _ => String::new(),
        };
    }

    /// Apply a daemon config broadcast to the provider/model picker state.
    fn apply_config_snapshot(
        &mut self,
        (schema, snapshot, restart_required): (ConfigSchema, ConfigSnapshot, bool),
    ) {
        self.danger_mode_active = snapshot
            .values
            .pointer("/general/approval")
            .and_then(|value| value.as_str())
            == Some("danger");
        self.config_schema = Some(schema);
        self.config = Some(snapshot);
        // A fresh authoritative snapshot resolves any in-flight fetch and
        // clears transient "switching…" notices; a restart-required flag wins.
        self.config_request = None;
        self.provider_notice = if restart_required {
            "Applied. The daemon flagged this change as restart-required.".into()
        } else {
            String::new()
        };
    }

    /// Apply a daemon config rejection: show it, drop the local snapshot so
    /// the next poll refetches the authoritative revision.
    fn apply_config_rejection(&mut self, message: String) {
        self.provider_notice = format!("Config change rejected: {message}");
        // Roll a local mode transition back to the last confirmed snapshot
        // before dropping it for the authoritative refetch.
        self.danger_mode_active = self
            .config
            .as_ref()
            .and_then(|config| config.values.pointer("/general/approval"))
            .and_then(|value| value.as_str())
            == Some("danger");
        self.config = None;
        self.config_request = None;
        // A rejected CLI override will never take effect; stop retrying it.
        self.cli_provider = None;
        self.cli_provider_sent = false;
        self.cli_model = None;
    }

    /// Apply command-line options: address override, dialog shortcuts, and
    /// pending provider/model switches resolved once a tab connects.
    fn apply_cli(&mut self, cli: cli::Cli) {
        if let Some(address) = cli.address {
            self.address = address;
        }
        self.cli_provider = cli.provider;
        self.cli_model = cli.model;
        self.show_setup |= cli.open_setup;
        self.show_catalog |= cli.open_catalog;
        self.show_stats |= cli.open_stats;
    }

    /// Apply `--provider`/`--model` once the daemon's config snapshot is known.
    /// The provider switch is issued first; the model is written only after the
    /// requested provider is active, so `--provider X --model Y` targets X.
    fn apply_cli_overrides(&mut self) {
        if let Some(id) = self.cli_provider.clone() {
            let Some((active, known)) = self.config.as_ref().map(|config| {
                (
                    config.active_provider.clone(),
                    config.providers.iter().any(|provider| provider.id == id),
                )
            }) else {
                return;
            };
            if active == id {
                self.cli_provider = None;
                self.cli_provider_sent = false;
            } else if !known {
                self.provider_notice = format!("Unknown provider `{id}`.");
                self.cli_provider = None;
                self.cli_provider_sent = false;
            } else if !self.cli_provider_sent && self.switch_provider(None, &id) {
                self.cli_provider_sent = true;
            }
        }
        if self.cli_provider.is_some() {
            return;
        }
        if let Some(model) = self.cli_model.clone() {
            let Some(current) = self.config.as_ref().and_then(|config| {
                config
                    .providers
                    .iter()
                    .find(|provider| provider.id == config.active_provider)
                    .map(|provider| provider.model.clone())
            }) else {
                self.cli_model = None;
                return;
            };
            if current == model {
                self.cli_model = None;
            } else {
                let provider_id = self
                    .config
                    .as_ref()
                    .map(|config| config.active_provider.clone());
                if self.save_model_for_provider(None, provider_id.as_deref(), model) {
                    self.cli_model = None;
                }
            }
        }
    }

    /// Fetch data for dialogs opened before a tab connected (e.g. via the CLI),
    /// retrying each frame until a connected tab can serve the request.
    fn ensure_open_dialog_data(&mut self) {
        if self.demo {
            return;
        }
        if self.show_stats
            && self.stats.is_none()
            && self.stats_retry_at.is_none_or(|at| Instant::now() >= at)
        {
            let _ = self.refresh_stats();
        }
        if self.show_catalog
            && self.catalog.is_none()
            && self.catalog_retry_at.is_none_or(|at| Instant::now() >= at)
        {
            let _ = self.request_catalog(false);
        }
    }

    /// Keep one `GetConfig` fetch in flight: issue it once any tab is
    /// connected and abandon it if its sending tab loses its socket.
    fn poll_config(&mut self) {
        if self.demo {
            return;
        }
        if let Some(tab_id) = self.config_request {
            if !self
                .tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.connected)
            {
                self.config_request = None;
            }
            return;
        }
        if self.config.is_some() {
            return;
        }
        // A no-op while no tab is connected; the connect event's repaint
        // re-runs this poll.
        let _ = self.request_config();
    }

    /// Send `GetConfig` through the first connected tab.
    fn request_config(&mut self) -> bool {
        if self.demo || self.config_request.is_some() || self.config.is_some() {
            return false;
        }
        let Some(index) = self.first_connected_tab_index() else {
            return false;
        };
        let sent = self.tabs[index].command(RuntimeCommand::GetConfig);
        if sent {
            let tab_id = self.tabs[index].id;
            self.config_request = Some(tab_id);
        }
        sent
    }

    /// Resolve a command target without silently falling back when a chooser was
    /// opened by a tab that has since disconnected or closed.
    fn provider_target_index(&self, origin: Option<u64>) -> Option<usize> {
        match origin {
            Some(id) => self
                .tabs
                .iter()
                .position(|tab| tab.id == id && tab.connected && !tab.demo),
            None => self.first_connected_tab_index(),
        }
    }

    /// Persistently switch the daemon's active provider (TUI `/provider`
    /// equivalent) using the latest snapshot's revision for conflict checks.
    fn switch_provider(&mut self, origin: Option<u64>, id: &str) -> bool {
        let Some(config) = self.config.as_ref() else {
            return false;
        };
        let Some(index) = self.provider_target_index(origin) else {
            return false;
        };
        if self.tabs[index].state.snapshot.provider_id == id {
            return false;
        }
        let sent = self.tabs[index].command(RuntimeCommand::SetActiveProvider {
            id: id.to_owned(),
            expected_revision: config.revision,
            request_id: None,
        });
        if sent {
            self.provider_notice = "Switching provider…".into();
        }
        sent
    }

    /// Persist the edited model on the provider reported by the target tab's
    /// snapshot, preserving every other configured field.
    #[cfg(test)]
    fn save_model(&mut self, origin: Option<u64>, model: String) -> bool {
        self.save_model_for_provider(origin, None, model)
    }

    fn save_model_for_provider(
        &mut self,
        origin: Option<u64>,
        provider_id_override: Option<&str>,
        model: String,
    ) -> bool {
        let Some(config) = self.config.as_ref().cloned() else {
            return false;
        };
        let Some(index) = self.provider_target_index(origin) else {
            return false;
        };
        let provider_id = provider_id_override
            .map(str::to_owned)
            .unwrap_or_else(|| self.tabs[index].state.snapshot.provider_id.clone());
        let Some(provider) = config
            .providers
            .iter()
            .find(|p| p.id == provider_id)
            .cloned()
        else {
            return false;
        };
        if provider.model == model {
            return false;
        }
        let update = ProviderUpdate {
            id: provider.id,
            label: provider.label,
            base_url: provider.base_url,
            model,
            endpoint: provider.endpoint,
            handler: provider.handler,
            context_window_tokens: provider.context_window_tokens,
            max_concurrency: provider.max_concurrency,
            reasoning_effort: provider.reasoning_effort,
            fast_mode: None,
            supports_prompt_cache_key: None,
            stream_usage: None,
            api_key: None,
        };
        let sent = self.tabs[index].command(RuntimeCommand::UpsertProvider {
            provider: update,
            expected_revision: config.revision,
            request_id: None,
        });
        if sent {
            self.provider_notice = "Saving model…".into();
        }
        sent
    }

    /// Persist one config value against the latest snapshot revision (TUI
    /// `/config set`). Returns whether the command was queued.
    fn set_config_value(&mut self, path: &str, value: serde_json::Value) -> bool {
        let Some(revision) = self.config.as_ref().map(|config| config.revision) else {
            self.config_notice = "Configuration is still loading; try again shortly.".into();
            return false;
        };
        let Some(index) = self.first_connected_tab_index() else {
            self.config_notice = "No connected conversation to send the change.".into();
            return false;
        };
        let sent = self.tabs[index].command(RuntimeCommand::SetConfigValue {
            path: path.to_owned(),
            value,
            expected_revision: revision,
            request_id: None,
        });
        self.config_notice = if sent {
            format!("Saving {path}…")
        } else {
            "Configuration daemon is unavailable.".into()
        };
        sent
    }

    /// Reset one config value to its default (TUI `/config reset`).
    fn reset_config_value(&mut self, path: &str) -> bool {
        let Some(revision) = self.config.as_ref().map(|config| config.revision) else {
            self.config_notice = "Configuration is still loading; try again shortly.".into();
            return false;
        };
        let Some(index) = self.first_connected_tab_index() else {
            self.config_notice = "No connected conversation to send the change.".into();
            return false;
        };
        let sent = self.tabs[index].command(RuntimeCommand::ResetConfigValue {
            path: path.to_owned(),
            expected_revision: revision,
            request_id: None,
        });
        self.config_notice = if sent {
            format!("Resetting {path}…")
        } else {
            "Configuration daemon is unavailable.".into()
        };
        sent
    }

    /// Enable/disable a tool or command (native-only toggle; the TUI does not
    /// expose this). Enablement lives in the snapshot's disabled lists rather
    /// than `values`, so it uses the dedicated commands.
    fn set_enabled(&mut self, namespace: &str, name: &str, enabled: bool) -> bool {
        let Some(revision) = self.config.as_ref().map(|config| config.revision) else {
            self.config_notice = "Configuration is still loading; try again shortly.".into();
            return false;
        };
        let Some(index) = self.first_connected_tab_index() else {
            self.config_notice = "No connected conversation to send the change.".into();
            return false;
        };
        let command = if namespace == "tools" {
            RuntimeCommand::SetToolEnabled {
                name: name.to_owned(),
                enabled,
                expected_revision: revision,
                request_id: None,
            }
        } else {
            RuntimeCommand::SetCommandEnabled {
                name: name.to_owned(),
                enabled,
                expected_revision: revision,
                request_id: None,
            }
        };
        let sent = self.tabs[index].command(command);
        self.config_notice = if sent {
            let verb = if enabled { "Enabling" } else { "Disabling" };
            format!("{verb} {name}…")
        } else {
            "Configuration daemon is unavailable.".into()
        };
        sent
    }

    /// Current approval mode from the config snapshot (`"safe"` unless the
    /// daemon reports otherwise).
    fn approval_mode(&self) -> String {
        self.config
            .as_ref()
            .and_then(|config| config.values.pointer("/general/approval"))
            .and_then(|value| value.as_str())
            .unwrap_or("safe")
            .to_owned()
    }

    /// Whether any tab can carry a command to the daemon.
    fn has_connected_tab(&self) -> bool {
        self.first_connected_tab_index().is_some()
    }

    /// Select the connection used for app-wide requests that do not belong to
    /// a particular conversation. Keeping this policy in one place prevents
    /// each dialog from growing its own subtly different first-tab scan.
    fn first_connected_tab_index(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.connected && !tab.demo)
    }

    /// Push the authoritative approval mode to the daemon (`SharedApprovalMode`),
    /// which is what actually gates tool calls. Entering Danger also resolves
    /// non-blocked approvals already waiting on their owning connections.
    fn set_approval_mode(&mut self, mode: &str) -> bool {
        let Some(index) = self.first_connected_tab_index() else {
            self.config_notice = "No connected conversation to send the change.".into();
            return false;
        };
        let sent = self.tabs[index].command(RuntimeCommand::SetApprovalMode {
            mode: mode.to_owned(),
        });
        if sent {
            self.danger_mode_active = mode == "danger";
            self.resolve_danger_approvals();
        }
        self.config_notice = if sent {
            format!("Approval mode: {mode}…")
        } else {
            "Configuration daemon is unavailable.".into()
        };
        sent
    }

    /// Approve non-blocked calls waiting while local or authoritative state is
    /// Danger. Each reply must use the connection that owns the request.
    fn resolve_danger_approvals(&mut self) {
        if !self.danger_mode_active {
            return;
        }
        for tab in &mut self.tabs {
            if !tab.connected || tab.demo {
                continue;
            }
            let pending: Vec<u64> = tab
                .state
                .approvals
                .iter()
                .filter(|approval| approval.blocked.is_none())
                .map(|approval| approval.id)
                .collect();
            for id in pending {
                if tab.command(RuntimeCommand::ApprovalReply {
                    id,
                    outcome: CallOutcome::Approve,
                }) {
                    tab.state.answered(id);
                }
            }
        }
    }

    /// Toggle session-scoped incognito mode (no durable writes while on).
    fn set_incognito(&mut self, enabled: bool) -> bool {
        let Some(tab) = self
            .tabs
            .get_mut(self.selected)
            .filter(|tab| tab.connected && !tab.demo)
        else {
            self.config_notice = "Connect this task before changing its privacy setting.".into();
            return false;
        };
        let sent = tab.command(RuntimeCommand::SetIncognito { enabled });
        self.config_notice = if sent {
            format!("Incognito {}…", if enabled { "on" } else { "off" })
        } else {
            "Configuration daemon is unavailable.".into()
        };
        sent
    }

    /// Ask the daemon for an immediate process/job snapshot when the Activity
    /// window is opened. The daemon also pushes snapshots on its own timer, so
    /// this is only a first-paint convenience; it is a no-op without a
    /// connected tab.
    fn refresh_activity(&mut self) {
        if let Some(index) = self.activity_tab_index() {
            let _ = self.tabs[index].command(RuntimeCommand::GetProcesses);
            let _ = self.tabs[index].command(RuntimeCommand::GetJobs);
        }
    }

    /// Resolve the conversation owning the Activity dialog. Once the dialog
    /// is open, keep it bound to its origin tab even if the user switches the
    /// focused conversation; falling back to another tab would make ids and
    /// cancellation commands target the wrong daemon session.
    fn activity_tab_index(&self) -> Option<usize> {
        if let Some(tab_id) = self.activity_tab {
            return self
                .tabs
                .iter()
                .position(|tab| tab.id == tab_id && tab.connected && !tab.demo);
        }
        self.tabs
            .get(self.selected)
            .filter(|tab| tab.connected && !tab.demo)
            .map(|_| self.selected)
            .or_else(|| self.first_connected_tab_index())
    }

    /// Catalog update count reported by the selected tab's frontend state.
    fn catalog_updates(&self) -> usize {
        self.tabs
            .get(self.selected)
            .and_then(|tab| tab.state.frontend.as_ref())
            .map(|frontend| frontend.catalog_updates)
            .unwrap_or(0)
    }

    /// Send `HostRequest::Stats` through the first connected tab (Phase 6).
    /// Returns whether a request was queued; a no-op while one is in flight.
    fn refresh_stats(&mut self) -> bool {
        if self.demo || self.stats_request.is_some() {
            return false;
        }
        let Some(index) = self.first_connected_tab_index() else {
            return false;
        };
        let request_id = self.tabs[index].state.next_id();
        let sent = self.tabs[index].command(RuntimeCommand::HostRequest {
            request_id,
            request: HostRequest::Stats { range: None },
        });
        if sent {
            let tab_id = self.tabs[index].id;
            self.stats_request = Some((tab_id, request_id));
            self.stats_request_at = Some(Instant::now());
            self.stats_retry_at = None;
            self.stats_notice.clear();
        }
        sent
    }

    fn request_review(&mut self) {
        if self.review.pending.is_some() {
            return;
        }
        let Some(tab) = self
            .tabs
            .get_mut(self.selected)
            .filter(|t| t.connected && t.state.ready && !t.demo)
        else {
            self.review.notice = "Connect to a daemon before reviewing workspace changes.".into();
            return;
        };
        if tab.host_api_version < 2 {
            self.review.notice = "Workspace review requires an updated daemon (host API 2).".into();
            return;
        }
        let request_id = tab.state.next_id();
        if tab.command(RuntimeCommand::HostRequest {
            request_id,
            request: HostRequest::WorkspaceReview,
        }) {
            self.review.pending = Some((tab.id, request_id, Instant::now()));
            self.review.notice.clear();
        } else {
            self.review.notice = "Could not send workspace review request.".into();
        }
    }

    /// Send `HostRequest::Catalog` through the first connected tab (Phase 6).
    /// `refresh` asks the daemon to re-fetch the remote manifest.
    fn request_catalog(&mut self, refresh: bool) -> bool {
        if self.demo || self.catalog_request.is_some() {
            return false;
        }
        let Some(index) = self.first_connected_tab_index() else {
            return false;
        };
        let request_id = self.tabs[index].state.next_id();
        let sent = self.tabs[index].command(RuntimeCommand::HostRequest {
            request_id,
            request: HostRequest::Catalog { refresh },
        });
        if sent {
            let tab_id = self.tabs[index].id;
            self.catalog_request = Some((tab_id, request_id));
            self.catalog_request_at = Some(Instant::now());
            self.catalog_retry_at = None;
            self.catalog_notice.clear();
        }
        sent
    }

    /// Apply one install/remove action against the latest catalog revision
    /// (`HostRequest::CatalogApply`). The daemon replies with `CatalogApplied`,
    /// which refreshes the snapshot and reports the outcome.
    fn apply_catalog_actions(&mut self, actions: Vec<CatalogAction>) -> bool {
        if self.demo || self.catalog_request.is_some() || actions.is_empty() {
            return false;
        }
        let Some(revision) = self
            .catalog
            .as_ref()
            .map(|catalog| catalog.revision.clone())
        else {
            self.catalog_notice = "Catalog is still loading; try again shortly.".into();
            return false;
        };
        let Some(index) = self.first_connected_tab_index() else {
            self.catalog_notice = "No connected conversation to send the change.".into();
            return false;
        };
        let request_id = self.tabs[index].state.next_id();
        let sent = self.tabs[index].command(RuntimeCommand::HostRequest {
            request_id,
            request: HostRequest::CatalogApply {
                expected_revision: revision,
                actions,
            },
        });
        if sent {
            let tab_id = self.tabs[index].id;
            self.catalog_request = Some((tab_id, request_id));
            self.catalog_request_at = Some(Instant::now());
            self.catalog_retry_at = None;
            self.catalog_notice = "Applying catalog changes…".into();
        }
        sent
    }

    /// Apply a correlated `Stats` response to the stats dashboard state.
    fn apply_stats_response(&mut self, response: HostResponse) {
        match response {
            HostResponse::Stats(snapshot) => {
                self.stats = Some(*snapshot);
                self.stats_notice.clear();
                self.stats_refreshed = Some(Instant::now());
            }
            HostResponse::Error { message, .. } => {
                self.stats_notice = format!("Usage stats unavailable: {message}");
            }
            _ => {}
        }
    }

    /// Apply a correlated `Catalog`/`CatalogApplied` response to the catalog
    /// browser state.
    fn apply_catalog_response(&mut self, response: HostResponse) {
        match response {
            HostResponse::Catalog(snapshot) => {
                self.catalog = Some(snapshot);
                self.catalog_notice.clear();
                // A `/catalog install|remove` queued before the first snapshot
                // can now be applied against the known revision.
                if let Some((tab_id, action)) = self.pending_catalog_action.take() {
                    let name = action.name.clone();
                    if self.apply_catalog_actions(vec![action]) {
                        self.reply_to_tab(tab_id, format!("Applying catalog change for {name}…"));
                    } else {
                        self.reply_to_tab(
                            tab_id,
                            format!("Catalog change for {name} could not be sent."),
                        );
                    }
                }
            }
            HostResponse::CatalogApplied(result) => {
                self.catalog_notice = catalog::applied_summary(&result);
                self.catalog_view.apply_result(&result);
                self.catalog = Some(result.snapshot);
            }
            HostResponse::Error { message, .. } => {
                self.catalog_notice = format!("Catalog unavailable: {message}");
                if let Some((tab_id, action)) = self.pending_catalog_action.take() {
                    self.reply_to_tab(
                        tab_id,
                        format!("Catalog change for {} failed: {message}", action.name),
                    );
                }
            }
            _ => {}
        }
    }

    /// Apply an app-level UI request queued by a tab's client-side built-in.
    fn apply_ui_request(&mut self, tab_id: u64, request: UiRequest) {
        match request {
            UiRequest::OpenJob(id) => {
                if self
                    .tabs
                    .iter()
                    .any(|tab| tab.id == tab_id && tab.state.jobs.iter().any(|job| job.id == id))
                {
                    self.job_view = Some(activity::JobViewer::new(tab_id, id));
                }
            }
            UiRequest::OpenStats => {
                self.show_stats = true;
                if self.stats.is_none() {
                    self.refresh_stats();
                }
            }
            UiRequest::OpenConfig => {
                self.show_config = true;
                if self.config.is_none() {
                    self.request_config();
                }
            }
            UiRequest::OpenSetup => {
                self.show_setup = true;
            }
            UiRequest::OpenCatalog => {
                self.show_catalog = true;
                if self.catalog.is_none() {
                    self.request_catalog(false);
                }
            }
            UiRequest::CatalogAction(arg) => self.apply_catalog_command(tab_id, &arg),
            UiRequest::SaveModel(model) => {
                self.provider_origin_tab = Some(tab_id);
                self.apply_model_command(tab_id, &model);
            }
            UiRequest::OpenProvider => {
                self.provider_origin_tab = Some(tab_id);
                self.show_provider = true;
            }
            UiRequest::SwitchProvider(id) => self.apply_provider_command(tab_id, &id),
        }
    }

    /// Push a client-side command reply into a specific tab's transcript.
    fn reply_to_tab(&mut self, tab_id: u64, text: impl Into<String>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) {
            tab.state.push_row("system", text);
        }
    }

    /// `/model <name>`: select the model for this task, reporting the
    /// outcome in the requesting tab (the provider dialog may not be open).
    fn apply_model_command(&mut self, tab_id: u64, model: &str) {
        if self.config.is_none() {
            self.reply_to_tab(
                tab_id,
                "Model change unavailable: configuration is still loading; try again shortly.",
            );
            return;
        }
        let current = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| tab.state.snapshot.provider_model.clone());
        if current.as_deref() == Some(model) {
            self.reply_to_tab(tab_id, format!("No change — model is already {model}."));
            return;
        }
        let provider = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| tab.state.snapshot.provider_id.clone())
            .unwrap_or_default();
        if self.choose_task_model(tab_id, &provider, model) {
            self.reply_to_tab(tab_id, format!("Selecting model {model} for this task…"));
        } else {
            self.reply_to_tab(tab_id, self.provider_notice.clone());
        }
    }

    /// `/provider <id>`: select a configured provider for this task, reporting the
    /// outcome in the requesting tab.
    fn apply_provider_command(&mut self, tab_id: u64, id: &str) {
        if self.config.is_none() {
            self.reply_to_tab(
                tab_id,
                "Provider switch unavailable: configuration is still loading; try again shortly.",
            );
            return;
        }
        if self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .is_some_and(|tab| tab.state.snapshot.provider_id == id)
        {
            self.reply_to_tab(tab_id, format!("No change — already using {id}."));
            return;
        }
        let model = self
            .config
            .as_ref()
            .and_then(|config| config.providers.iter().find(|p| p.id == id))
            .map(|p| p.model.clone())
            .unwrap_or_default();
        if self.choose_task_model(tab_id, id, &model) {
            self.reply_to_tab(tab_id, format!("Switching provider to {id}…"));
        } else {
            self.reply_to_tab(tab_id, self.provider_notice.clone());
        }
    }

    /// `/catalog install|remove NAME`: parse and apply a single catalog action,
    /// fetching the catalog first when no snapshot is loaded yet.
    fn apply_catalog_command(&mut self, tab_id: u64, arg: &str) {
        let mut parts = arg.split_whitespace();
        let action = parts.next();
        let name = parts.next();
        let (Some(action), Some(name)) = (action, name) else {
            self.reply_to_tab(tab_id, "Usage: /catalog install|remove NAME");
            return;
        };
        if parts.next().is_some() || !matches!(action, "install" | "remove") {
            self.reply_to_tab(tab_id, "Usage: /catalog install|remove NAME");
            return;
        }
        let action = if action == "install" {
            CatalogActionKind::Install
        } else {
            CatalogActionKind::Remove
        };
        let item = CatalogAction {
            name: name.to_string(),
            action,
        };
        if self.catalog.is_some() {
            if self.apply_catalog_actions(vec![item]) {
                self.reply_to_tab(tab_id, format!("Applying catalog change for {name}…"));
            } else {
                self.reply_to_tab(
                    tab_id,
                    format!("Catalog change for {name} could not be sent."),
                );
            }
            return;
        }
        self.pending_catalog_action = Some((tab_id, item));
        if self.catalog_request.is_none() && !self.request_catalog(true) {
            self.pending_catalog_action = None;
            self.reply_to_tab(tab_id, "Catalog unavailable: no connected conversation.");
        }
    }

    /// Latest theme payload from the selected tab (falling back to any tab that
    /// has received one), as broadcast by `ViewDiff::SetTheme`.
    fn theme_value(&self) -> Option<&serde_json::Value> {
        self.tabs
            .get(self.selected)
            .and_then(|tab| tab.state.theme.as_ref())
            .or_else(|| self.tabs.iter().find_map(|tab| tab.state.theme.as_ref()))
    }

    /// Resolved palette of the currently applied daemon theme, used to color
    /// declarative panes/status lines. Falls back to palette defaults when no
    /// theme payload has arrived yet.
    fn palette(&self) -> theme::Palette {
        self.theme_value()
            .and_then(|value| serde_json::from_value::<theme::ThemeSettings>(value.clone()).ok())
            .map(|settings| settings.palette)
            .unwrap_or_default()
    }

    /// Resolved render-role colors of the currently applied daemon theme, used
    /// by the transcript and Markdown renderers. Falls back to palette-derived
    /// defaults when no theme payload has arrived yet.
    fn theme_colors(&self) -> theme::ThemeColors {
        self.theme_value()
            .and_then(|value| serde_json::from_value::<theme::ThemeSettings>(value.clone()).ok())
            .unwrap_or_default()
            .colors()
    }

    /// Apply the daemon's resolved theme to egui's visuals. Only rebuilds the
    /// style when the payload changes, so this is cheap on the common frame.
    fn apply_theme(&mut self, ctx: &egui::Context) {
        let Some(value) = self.theme_value().cloned() else {
            return;
        };
        if self.applied_theme.as_ref() == Some(&value) {
            return;
        }
        if let Ok(settings) = serde_json::from_value::<theme::ThemeSettings>(value.clone()) {
            ctx.set_style_of(ctx.theme(), settings.style());
        }
        self.applied_theme = Some(value);
    }

    /// Refresh the configurable keymap from the daemon's resolved frontend
    /// settings. Mirrors `theme_value`/`apply_theme`: the selected tab's payload
    /// wins, any tab is a fallback, and an unchanged settings `revision` skips
    /// the reparse so this is cheap on the common frame.
    fn sync_keymap(&mut self) {
        let Some(value) = self
            .tabs
            .get(self.selected)
            .and_then(|tab| tab.state.frontend.as_ref())
            .map(|frontend| frontend.settings.clone())
            .or_else(|| {
                self.tabs
                    .iter()
                    .find_map(|tab| tab.state.frontend.as_ref().map(|f| f.settings.clone()))
            })
        else {
            return;
        };
        let revision = value.get("revision").and_then(serde_json::Value::as_u64);
        if revision.is_some() && revision == self.keymap_revision {
            return;
        }
        self.keymap = keymap::Keymap::parse(&value);
        self.keymap_revision = revision;
    }

    /// Consume a user-configured keybinding before the hardcoded shortcuts, so a
    /// config binding always wins. The matched action string is sent to the
    /// daemon for classification (`KeymapDispatched`), mirroring the TUI.
    fn handle_keymap(&mut self, ui: &mut egui::Ui) {
        if self.demo
            || self.modal_open()
            || file_refs::is_open(ui.ctx())
            || self.keymap.bindings.is_empty()
        {
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
            self.dispatch_keymap_action(action);
        }
    }

    /// Send a matched binding to the daemon for classification. The reply
    /// (`KeymapDispatched`) carries the same request id, so a stale or another
    /// client's result is ignored in `apply_keymap_dispatched`.
    fn dispatch_keymap_action(&mut self, action: String) {
        let Some(index) = self
            .tabs
            .get(self.selected)
            .filter(|tab| tab.connected && !tab.demo)
            .map(|_| self.selected)
        else {
            self.config_notice =
                "Select a connected conversation to dispatch the keybinding.".into();
            return;
        };
        let tab_id = self.tabs[index].id;
        let request_id = self.tabs[index].state.next_id();
        if self.tabs[index].command(RuntimeCommand::KeymapDispatch {
            request_id: Some(request_id),
            action,
        }) {
            self.pending_keymap = Some((tab_id, request_id));
        } else {
            self.config_notice = "Configuration daemon is unavailable.".into();
        }
    }

    /// Apply a daemon-classified keybinding. Correlate the request id so a stale
    /// reply (or another client's) is ignored; a legacy `None` id applies to the
    /// selected tab unconditionally.
    fn apply_keymap_dispatched(
        &mut self,
        ctx: &egui::Context,
        request_id: Option<u64>,
        kind: KeymapDispatchKind,
    ) {
        let tab_id = match request_id {
            Some(id) => match self.pending_keymap {
                Some((tab_id, pending)) if pending == id => {
                    self.pending_keymap = None;
                    Some(tab_id)
                }
                _ => return,
            },
            None => None,
        };
        self.apply_keymap_kind(ctx, tab_id, kind);
    }

    /// Apply a classified keybinding. Builtins act on the app (panes, approval
    /// mode, image paste, composer caret); command/prompt text seeds the target
    /// tab's composer and submits it, mirroring the TUI.
    fn apply_keymap_kind(
        &mut self,
        ctx: &egui::Context,
        tab_id: Option<u64>,
        kind: KeymapDispatchKind,
    ) {
        match kind {
            KeymapDispatchKind::Noop => {}
            KeymapDispatchKind::Builtin { action } => {
                self.apply_keymap_builtin(ctx, tab_id, &action)
            }
            KeymapDispatchKind::Command { text } | KeymapDispatchKind::Prompt { text } => {
                self.apply_keymap_text(tab_id, text);
            }
        }
    }

    /// The tab a keymap action targets: an explicit id from the dispatched reply,
    /// otherwise the selected tab.
    fn keymap_target_index(&self, tab_id: Option<u64>) -> Option<usize> {
        match tab_id {
            Some(id) => self.tabs.iter().position(|tab| tab.id == id),
            None => self.tabs.get(self.selected).map(|_| self.selected),
        }
    }

    fn apply_keymap_builtin(&mut self, ctx: &egui::Context, tab_id: Option<u64>, action: &str) {
        match action {
            "toggle_panes" => {
                self.panes_visible = !self.panes_visible;
            }
            "cycle_approval_mode" => {
                let next = if self.approval_mode() == "safe" {
                    "danger"
                } else {
                    "safe"
                };
                self.set_approval_mode(next);
            }
            "paste_image" => {
                if let Some(index) = self.keymap_target_index(tab_id)
                    && !self.tabs[index].demo
                {
                    self.tabs[index].attach_image_dialog();
                }
            }
            "cursor_to_start" => self.move_composer_cursor(ctx, tab_id, true),
            "cursor_to_end" => self.move_composer_cursor(ctx, tab_id, false),
            _ => {}
        }
    }

    /// Seed the target tab's composer with a command/prompt and submit it, the
    /// native equivalent of the TUI applying `Command`/`Prompt` to its input.
    fn apply_keymap_text(&mut self, tab_id: Option<u64>, text: String) {
        let Some(index) = self.keymap_target_index(tab_id) else {
            return;
        };
        let tab = &mut self.tabs[index];
        if tab.demo {
            return;
        }
        tab.composer = text;
        tab.attachments.clear();
        tab.autocomplete = None;
        tab.submit_composer();
    }

    /// Move the target tab's composer caret to the start or end of its text,
    /// mirroring the TUI's `cursor_to_start`/`cursor_to_end`.
    fn move_composer_cursor(&mut self, ctx: &egui::Context, tab_id: Option<u64>, to_start: bool) {
        let Some(index) = self.keymap_target_index(tab_id) else {
            return;
        };
        let tab = &self.tabs[index];
        if tab.demo {
            return;
        }
        let editor_id = egui::Id::new((tab.id, "composer-editor"));
        let char_index = if to_start {
            0
        } else {
            tab.composer.chars().count()
        };
        if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, editor_id) {
            let cursor = egui::text::CCursor::new(char_index);
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(cursor)));
            state.store(ctx, editor_id);
            ctx.request_repaint();
        }
    }

    /// Apply a mutation requested by the config dialog.
    fn apply_config_ui_action(&mut self, action: config_view::ConfigUiAction) {
        match action {
            config_view::ConfigUiAction::Set { path, value } => {
                self.set_config_value(&path, value);
            }
            config_view::ConfigUiAction::Reset { path } => {
                self.reset_config_value(&path);
            }
            config_view::ConfigUiAction::SetEnabled {
                namespace,
                name,
                enabled,
            } => {
                self.set_enabled(&namespace, &name, enabled);
            }
        }
    }

    /// Apply a correlated host response to the conversation picker state.
    fn apply_host_response(&mut self, response: HostResponse) {
        match response {
            HostResponse::Conversations(conversations) => {
                self.conversations = conversations;
                self.sync_task_titles();
                self.conversations_loaded = true;
                self.mutation_target = None;
                self.sidebar_notice.clear();
            }
            HostResponse::Error { message, .. } => {
                // Latch the failure like a successful list: otherwise
                // `poll_conversations` re-issues the request on every error
                // round trip while the daemon stays broken. The snapshot is
                // refetched automatically once a reconnect succeeds.
                self.conversations_loaded = true;
                let was_mutation = self.mutation_target.is_some();
                self.mutation_target = None;
                self.sidebar_notice = if was_mutation {
                    format!("Conversation update failed: {message}")
                } else {
                    format!("Conversation list unavailable: {message}")
                };
            }
            _ => {} // Other host responses are not consumed by this app.
        }
    }

    /// Keep one `Conversations` request in flight: issue it once any tab is
    /// connected and abandon it if its sending tab loses its socket.
    fn poll_conversations(&mut self) {
        if self.demo {
            return;
        }
        if let Some((tab_id, _)) = self.conversations_request {
            if !self
                .tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.connected)
            {
                self.conversations_request = None;
                self.conversations_request_at = None;
                self.conversations_loaded = false;
                if self.mutation_target.take().is_some() {
                    self.sidebar_notice = "Conversation update was not confirmed: the \
                                           connection dropped before the daemon replied."
                        .into();
                }
            }
            return;
        }
        if self.conversations_loaded {
            return;
        }
        // A no-op while no tab is connected; the connect event's repaint
        // re-runs this poll.
        let _ = self.request_conversations();
    }

    /// Route a correlated setup response into the onboarding machine and
    /// surface any outcome as a transient notice.
    fn apply_setup_response(&mut self, response: HostResponse) {
        let outcome = self.setup_ui.handle_response(response);
        match outcome {
            setup::Outcome::Snapshot => {
                // First (or refreshed) snapshot arrived: offer onboarding once
                // when the daemon says something actually needs setup.
                if !self.setup_offered && self.setup_ui.actionable() {
                    self.setup_offered = true;
                    self.show_setup = true;
                }
            }
            setup::Outcome::Applied(result) => {
                if !result.message.is_empty() {
                    self.sidebar_notice = result.message.clone();
                }
                // The save changed the daemon's config; drop the cached snapshot
                // so the next poll refetches the authoritative revision for the
                // provider/model picker.
                self.config = None;
                self.config_request = None;
            }
            setup::Outcome::Failed { code, message, .. } => {
                self.sidebar_notice = format!("Setup failed ({code:?}): {message}");
            }
            setup::Outcome::Ignored => {}
        }
    }

    /// Keep one setup request in flight: re-issue the snapshot request when
    /// `poll` asks for it and abandon it (aborting the machine) if its sending
    /// tab loses its socket.
    fn poll_setup(&mut self) {
        if self.demo {
            return;
        }
        if let Some((tab_id, _)) = self.setup_request {
            if !self
                .tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.connected)
            {
                self.setup_request = None;
                self.setup_ui.abort();
            }
            return;
        }
        // `poll` returns a fresh `Setup` request only while no snapshot is
        // stored (or a stale re-request is armed); otherwise it is `None`.
        let Some(request) = self.setup_ui.poll() else {
            return;
        };
        if !self.send_setup_request(request) {
            self.setup_ui.abort();
        }
    }

    /// Send a setup `HostRequest` through the first connected tab, recording
    /// the correlation slot. Returns `false` when it could not be sent (no
    /// connected tab or the socket dropped), so the caller can abort.
    fn send_setup_request(&mut self, request: HostRequest) -> bool {
        if self.demo || self.setup_request.is_some() {
            return false;
        }
        let Some(index) = self.first_connected_tab_index() else {
            return false;
        };
        let request_id = self.tabs[index].state.next_id();
        let sent = self.tabs[index].command(RuntimeCommand::HostRequest {
            request_id,
            request,
        });
        if sent {
            let tab_id = self.tabs[index].id;
            self.setup_request = Some((tab_id, request_id));
        }
        sent
    }

    /// Send `HostRequest::Conversations` through the first connected tab.
    fn request_conversations(&mut self) -> bool {
        if self.demo || self.conversations_request.is_some() || self.conversations_loaded {
            return false;
        }
        let Some(index) = self.first_connected_tab_index() else {
            return false;
        };
        let request_id = self.tabs[index].state.next_id();
        let sent = self.tabs[index].command(RuntimeCommand::HostRequest {
            request_id,
            request: HostRequest::Conversations { limit: 0 },
        });
        if sent {
            let tab_id = self.tabs[index].id;
            self.conversations_request = Some((tab_id, request_id));
            self.conversations_request_at = Some(Instant::now());
        }
        sent
    }

    /// Send a conversation rename/delete through the first connected tab,
    /// reusing the picker's in-flight slot so the daemon's refreshed
    /// `Conversations` list is correlated and applied like a plain fetch.
    fn request_conversation_mutation(&mut self, request: HostRequest) -> bool {
        if self.demo || self.conversations_request.is_some() {
            return false;
        }
        let Some(index) = self.first_connected_tab_index() else {
            return false;
        };
        let request_id = self.tabs[index].state.next_id();
        let sent = self.tabs[index].command(RuntimeCommand::HostRequest {
            request_id,
            request: request.clone(),
        });
        if sent {
            let tab_id = self.tabs[index].id;
            self.conversations_request = Some((tab_id, request_id));
            self.conversations_request_at = Some(Instant::now());
            self.mutation_target = match &request {
                HostRequest::ConversationRename { id, .. }
                | HostRequest::ConversationDelete { id, .. } => Some(*id),
                _ => None,
            };
        }
        sent
    }

    /// Select the tab already attached to `id`, or open (and connect) a fresh
    /// Load tab for it.
    fn open_conversation(&mut self, id: i64, ctx: &egui::Context) {
        if let Some(existing) = self
            .tabs
            .iter()
            .position(|tab| !tab.demo && tab.conversation_id == Some(id))
        {
            self.focus_conversation(self.tabs[existing].id, ctx);
        } else {
            self.add_tab(Intent::Load(id), ctx);
            if !self.demo {
                self.connect_index(self.selected);
            }
        }
        self.sidebar_notice.clear();
        self.note_layout_change(ctx);
    }

    /// Snapshot the open tabs as a restart layout. Demo tabs never appear; they
    /// only exist in demo mode, where persistence is disabled entirely.
    fn current_layout(&self) -> layout::Layout {
        let saved: Vec<_> = self.tabs.iter().filter(|tab| !tab.demo).collect();
        let mut workspace = self.workspace.clone();
        workspace.remap_tabs(|id| {
            saved
                .iter()
                .position(|tab| tab.id == id)
                .map(|i| i as u64 + 1)
        });
        layout::Layout {
            address: self.address.clone(),
            selected: self.selected.min(self.tabs.len().saturating_sub(1)),
            tabs: self
                .tabs
                .iter()
                .filter(|tab| !tab.demo)
                .map(|tab| layout::TabState {
                    conversation_id: tab.conversation_id,
                    draft: tab.expanded_composer(),
                })
                .collect(),
            preferences: self.display,
            workspace: Some(workspace),
            legacy_split: layout::LegacySplit::default(),
        }
    }

    /// Mirror live display state (context zoom and resolved sidebar width) into
    /// the persisted preferences, marking the layout dirty on change. A `None`
    /// width leaves the previously persisted value untouched.
    fn sync_display(
        &mut self,
        ctx: &egui::Context,
        sidebar_width: Option<f32>,
        sidebar_manually_resized: bool,
    ) {
        let mut next = self.display;
        next.zoom_percent =
            ((ctx.zoom_factor() * 100.0).round() as u16).clamp(layout::ZOOM_MIN, layout::ZOOM_MAX);
        if let Some(width) = sidebar_width {
            let width = width.clamp(
                layout::SIDEBAR_WIDTH_MIN as f32,
                layout::SIDEBAR_WIDTH_MAX as f32,
            );
            next.sidebar_width_manual |= sidebar_manually_resized;
            next.sidebar_width =
                (width.round() as u16).clamp(layout::SIDEBAR_WIDTH_MIN, layout::SIDEBAR_WIDTH_MAX);
        }
        if next != self.display {
            self.display = next;
            self.note_layout_change(ctx);
        }
    }

    /// Responsively cap a resizable panel's egui-persisted width so a saved
    /// wide sidebar or split pane cannot crowd the transcripts in a narrow
    /// window (e.g. a 500px sidebar in a 1000px window). Dragging within the
    /// cap still works; the cap only re-clamps when the window shrinks.
    fn cap_panel_width(ctx: &egui::Context, id: egui::Id, cap: f32) {
        let Some(state) = egui::PanelState::load(ctx, id) else {
            return;
        };
        let width = state.outer_rect.width();
        if width > cap {
            ctx.data_mut(|data| {
                data.insert_persisted(
                    id,
                    egui::PanelState {
                        outer_rect: egui::Rect::from_min_size(
                            state.outer_rect.min,
                            egui::vec2(cap, state.outer_rect.height()),
                        ),
                    },
                )
            });
        }
    }

    fn set_panel_width(ctx: &egui::Context, id: egui::Id, width: f32) {
        let Some(state) = egui::PanelState::load(ctx, id) else {
            return;
        };
        if (state.outer_rect.width() - width).abs() > 0.5 {
            ctx.data_mut(|data| {
                data.insert_persisted(
                    id,
                    egui::PanelState {
                        outer_rect: egui::Rect::from_min_size(
                            state.outer_rect.min,
                            egui::vec2(width, state.outer_rect.height()),
                        ),
                    },
                )
            });
        }
    }

    /// Record a layout-affecting change (tab open/close, selection, draft,
    /// address, conversation pin) and schedule the debounced flush. Waking the
    /// app for the flush matters: the UI sleeps while idle, so the pending save
    /// would otherwise never run after the last edit.
    fn note_layout_change(&mut self, ctx: &egui::Context) {
        if self.layout_path.is_none() {
            return;
        }
        if self.layout_dirty_since.is_none() {
            ctx.request_repaint_after(Duration::from_millis(LAYOUT_SAVE_DEBOUNCE_MS));
        }
        self.layout_dirty_since.get_or_insert_with(Instant::now);
    }

    /// Write the layout file once the debounce has elapsed. Kept dirty (with a
    /// retry wake) when the write fails so the state is not silently dropped.
    fn flush_layout(&mut self, ctx: &egui::Context) {
        let Some(path) = self.layout_path.clone() else {
            self.layout_dirty_since = None;
            return;
        };
        let Some(since) = self.layout_dirty_since else {
            return;
        };
        let debounce = Duration::from_millis(LAYOUT_SAVE_DEBOUNCE_MS);
        if since.elapsed() < debounce {
            // Not ready yet: wake when the debounce elapses so the pending
            // save still runs even though the UI is idle.
            ctx.request_repaint_after(since + debounce - Instant::now());
            return;
        }
        let snapshot = self.current_layout();
        match layout::save(&path, &snapshot) {
            Ok(()) => self.layout_dirty_since = None,
            Err(error) => {
                self.sidebar_notice = format!("Layout save failed: {error}");
                self.layout_dirty_since = Some(Instant::now());
                ctx.request_repaint_after(debounce);
            }
        }
    }

    /// Address used for connects/spawns: the saved address, normalized to carry
    /// a port so bare hostnames still work.
    fn effective_address(&self) -> String {
        daemon::ensure_port(self.address.trim())
    }

    /// Begin the user-visible session: mark every tab auto and connect them to
    /// the daemon address. The coordinator observes failures and starts a local
    /// daemon when the target is loopback and nothing is listening.
    fn begin(&mut self) {
        if self.demo {
            return;
        }
        for i in 0..self.tabs.len() {
            self.connect_index(i);
        }
    }

    fn connect_index(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let address = self.effective_address();
        let tab = &mut self.tabs[index];
        if tab.demo || tab.connected || tab.connecting {
            return;
        }
        tab.endpoint = address.clone();
        tab.workspace.clear();
        tab.auto = true;
        tab.connecting = true;
        tab.connect_failed_refused = false;
        tab.connect_failed_other = false;
        tab.connection_status = "Connecting…".into();
        if tab.commands.send(Command::Connect(address)).is_err() {
            tab.connecting = false;
            tab.connection_status = "Connection worker stopped".into();
        }
    }

    fn connect_selected(&mut self) {
        self.connect_index(self.selected);
    }

    fn disconnect_selected(&mut self) {
        if self.selected >= self.tabs.len() {
            return;
        }
        let tab = &mut self.tabs[self.selected];
        if tab.demo || !tab.connected {
            return;
        }
        tab.auto = false; // An explicit disconnect stops auto-reconnect.
        let _ = tab.commands.send(Command::Disconnect);
        tab.connected = false;
        tab.connecting = true; // wait for the worker's acknowledgement
        tab.state.ready = false;
        tab.connection_status = "Disconnecting…".into();
    }

    /// Schedule the next auto-reconnect round at least `delay` from now. Waking
    /// the app matters because the UI sleeps while idle.
    fn schedule_retry(&mut self, ctx: &egui::Context, delay: Duration) {
        let due = Instant::now() + delay;
        let earliest = match self.retry_at {
            Some(at) if at <= due => at,
            _ => due,
        };
        self.retry_at = Some(earliest);
        ctx.request_repaint_after(earliest.saturating_duration_since(Instant::now()));
    }

    /// Whether the app should wake at least once a second to refresh live
    /// background elapsed times. True while any process is running, any job
    /// exists, or an activity/viewer dialog is open.
    fn background_tick_due(&self) -> bool {
        if self.show_activity || self.process_view.is_some() || self.job_view.is_some() {
            return true;
        }
        self.tabs.iter().any(|tab| {
            tab.state.processes.iter().any(|process| process.running) || !tab.state.jobs.is_empty()
        })
    }

    /// Live pages followed by explicit overlays, scoped to the focused conversation.
    fn active_tab_float_ids(&self) -> Vec<live_pane::PageId> {
        self.tabs
            .get(self.selected)
            .map(|tab| {
                let mut ids = live_pane::page_ids(&tab.state.view, &tab.state.jobs);
                ids.extend(tab.state.view.components.iter().filter_map(
                    |component| match component {
                        bone_protocol::Component::Float {
                            id,
                            presentation: bone_protocol::PanePresentation::Overlay,
                            ..
                        } => Some(live_pane::PageId::Extension(id.clone())),
                        _ => None,
                    },
                ));
                ids
            })
            .unwrap_or_default()
    }

    fn cycle_active_pane(&mut self) {
        let ids = self.active_tab_float_ids();
        let Some(tab) = self.tabs.get_mut(self.selected) else {
            self.active_pane = None;
            return;
        };
        if ids.is_empty() {
            self.active_pane = None;
            return;
        }
        let current = self
            .active_pane
            .as_ref()
            .filter(|(id, _)| *id == tab.id)
            .and_then(|(_, pane)| ids.iter().position(|candidate| candidate == pane));
        let next = ids[current.map_or(0, |index| (index + 1) % ids.len())].clone();
        if live_pane::page_ids(&tab.state.view, &tab.state.jobs).contains(&next) {
            tab.live_pane.select(next.clone());
        }
        self.active_pane = Some((tab.id, next));
    }

    fn scroll_active_pane(&mut self, delta: i64) {
        let Some((tab_id, id)) = self.active_pane.clone() else {
            return;
        };
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        if live_pane::page_ids(&tab.state.view, &tab.state.jobs).contains(&id) {
            tab.live_pane.select(id);
            tab.live_pane.scroll_by(delta);
        } else if let live_pane::PageId::Extension(id) = id {
            let offset = self.pane_scroll.entry((tab_id, id)).or_insert(0);
            *offset = (*offset + delta).max(0);
        }
    }

    /// Consume pane-navigation keys: Tab cycles the focused float and
    /// PageUp/PageDown (and Ctrl+Up/Down) scroll it. Only acts when the panes
    /// are visible, nothing is focused, and no modal dialog is open, so typing
    /// and other shortcuts are never stolen.
    fn handle_pane_keys(&mut self, ui: &egui::Ui) {
        if !self.panes_visible || self.modal_open() || file_refs::is_open(ui.ctx()) {
            return;
        }
        if ui.memory(|memory| memory.focused()).is_some() {
            return;
        }
        if self.active_tab_float_ids().is_empty() {
            self.active_pane = None;
            return;
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Tab)) {
            self.cycle_active_pane();
        }
        const PAGE: i64 = 5;
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown)) {
            self.scroll_active_pane(PAGE);
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp)) {
            self.scroll_active_pane(-PAGE);
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowDown)) {
            self.scroll_active_pane(1);
        }
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::ArrowUp)) {
            self.scroll_active_pane(-1);
        }
    }

    /// Consume Activity-dialog navigation keys while the Activity window is
    /// open: Up/Down move the selection, Home/End jump, Enter opens the
    /// selected viewer, and `k` cancels the selected running process/job.
    /// Returns any viewer/cancel actions for the caller to apply alongside
    /// mouse-driven actions. `process_ids`/`job_ids` carry `(id, running)`.
    fn handle_activity_keys(
        &mut self,
        ctx: &egui::Context,
        process_ids: &[(String, bool)],
        job_ids: &[(String, bool)],
    ) -> Vec<activity::ActivityAction> {
        if !self.show_activity {
            return Vec::new();
        }
        if ctx.memory(|memory| memory.focused()).is_some() {
            return Vec::new();
        }
        let total = process_ids.len() + job_ids.len();
        if total == 0 {
            self.activity_selected = 0;
            return Vec::new();
        }
        self.activity_selected = self.activity_selected.min(total - 1);
        let mut actions = Vec::new();
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
            self.activity_selected = (self.activity_selected + 1).min(total - 1);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
            self.activity_selected = self.activity_selected.saturating_sub(1);
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Home)) {
            self.activity_selected = 0;
        }
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::End)) {
            self.activity_selected = total - 1;
        }
        let selected = self.activity_selected;
        let target = if selected < process_ids.len() {
            let (id, running) = &process_ids[selected];
            (id.clone(), *running, true)
        } else {
            let (id, running) = &job_ids[selected - process_ids.len()];
            (id.clone(), *running, false)
        };
        let (id, running, is_process) = target;
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            if is_process {
                actions.push(activity::ActivityAction::OpenProcess(id.clone()));
            } else {
                actions.push(activity::ActivityAction::OpenJob(id.clone()));
            }
        }
        if running && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::K))
        {
            if is_process {
                actions.push(activity::ActivityAction::CancelProcess(id));
            } else {
                actions.push(activity::ActivityAction::CancelJob(id));
            }
        }
        actions
    }

    /// Whether a modal dialog that owns keyboard input is currently open.
    fn modal_open(&self) -> bool {
        self.shared_dialog_open()
            || self.pending_key_request().is_some()
            || self.close_target.is_some()
            || self.window_ui.closing_window()
            || self.tabs.iter().any(|tab| {
                tab.show_editor
                    && self
                        .workspace
                        .tab_location(tab.id)
                        .map(|(window, _)| window)
                        == Some(self.window_ui.rendering)
            })
    }

    fn shared_dialog_open(&self) -> bool {
        self.palette.open
            || self.command_input.is_some()
            || self.rename_target.is_some()
            || self.delete_target.is_some()
            || self.review.open
            || self.show_provider
            || self.show_setup
            || self.show_server
            || self.show_config
            || self.stats_picker.is_some()
            || self.show_activity
            || self.process_view.is_some()
            || self.job_view.is_some()
            || self.show_stats
            || self.show_catalog
    }

    /// Spawn `bone serve` bound to the effective address, if possible. On
    /// success moves to `Starting` and schedules the first reconnect round.
    /// Returns false (and records a `Stopped` reason) when no binary is found
    /// or the process cannot start.
    fn start_local_daemon(&mut self, ctx: &egui::Context) -> bool {
        let address = match daemon::local_endpoint(&self.effective_address()) {
            Ok(endpoint) => endpoint.to_string(),
            Err(message) => {
                self.daemon_phase = daemon::Phase::Stopped(message.clone());
                self.daemon_notice = message;
                return false;
            }
        };
        let bin = match self.daemon_bin.clone().or_else(daemon::resolve_binary) {
            Some(bin) => bin,
            None => {
                let message =
                    "Could not find the `bone` daemon binary (set BONE_DESKTOP_DAEMON or install bone)."
                        .to_owned();
                self.daemon_phase = daemon::Phase::Stopped(message.clone());
                self.daemon_notice = message;
                return false;
            }
        };
        let state_dir = self.layout_path.as_deref().and_then(|p| p.parent());
        let log = daemon::daemon_log_path(state_dir);
        match daemon::spawn_daemon(&bin, &address, &log) {
            Ok(child) => {
                self.daemon_pid = Some(child.id());
                self.daemon_child = Some(child);
                self.daemon_phase = daemon::Phase::Starting { attempts: 0 };
                self.daemon_notice.clear();
                self.schedule_retry(ctx, Duration::from_millis(daemon::DAEMON_RETRY_DELAY_MS));
                true
            }
            Err(error) => {
                let message =
                    format!("Could not start the local daemon: {error} (see the Server dialog)");
                self.daemon_phase = daemon::Phase::Stopped(message.clone());
                self.daemon_notice = message;
                false
            }
        }
    }

    /// Reconnect every tab the coordinator still owns that is not already
    /// connected or mid-connect. Explicitly disconnected tabs are skipped.
    fn reconnect_auto_tabs(&mut self) {
        for i in 0..self.tabs.len() {
            let should = {
                let tab = &self.tabs[i];
                tab.auto
                    && !tab.demo
                    && !tab.connected
                    && !tab.connecting
                    && !tab.closing
                    && !tab.remove
            };
            if should {
                self.connect_index(i);
            }
        }
    }

    /// Detect the death of a daemon this app spawned. `try_wait` is a no-op
    /// while the process runs. Once it has exited the sockets are physically
    /// dead: clear the connected flags (a stale `Ready` must not re-flip), and
    /// either respawn (a live session was lost) or stop with a message (it
    /// crashed while booting).
    fn poll_daemon_child(&mut self, ctx: &egui::Context) {
        let Some(status) = self
            .daemon_child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
        else {
            return;
        };
        let pid = self.daemon_pid.unwrap_or(0);
        self.daemon_child = None;
        self.reconnect_budget = None;
        for tab in &mut self.tabs {
            tab.connected = false;
        }
        if matches!(self.daemon_phase, daemon::Phase::Ready) {
            // A live session was lost: try a fresh daemon in its place.
            let restarted = self.start_local_daemon(ctx);
            if restarted {
                self.daemon_notice = format!(
                    "The local daemon (pid {pid}) exited ({status}); a new one is starting."
                );
            }
        } else {
            let message = format!(
                "The local daemon (pid {pid}) exited before accepting connections ({status}). \
                 Use Server… to start it."
            );
            self.daemon_phase = daemon::Phase::Stopped(message.clone());
            self.daemon_notice = message;
        }
    }

    /// Frame pump for the daemon lifecycle. Runs after events are drained:
    /// - Any tab connected means the daemon is reachable: mark Ready.
    /// - A refused connect on a loopback address with no daemon yet starts one.
    /// - Retry ticks reconnect auto tabs while the spawned daemon boots.
    /// - A mid-session drop (an established socket lost) drives a bounded
    ///   reconnect; when the budget is exhausted the coordinator stops and
    ///   defers to the Server dialog.
    fn pump_daemon(&mut self, ctx: &egui::Context) {
        self.poll_daemon_child(ctx);
        if self.demo || self.tabs.is_empty() {
            return;
        }
        // Reachability is not recovery: one live tab must not cancel retries
        // for other automatic tabs, including attempts still in flight.
        let reachable = self.tabs.iter().any(|tab| tab.connected);
        if reachable && self.daemon_phase != daemon::Phase::Ready {
            self.daemon_phase = daemon::Phase::Ready;
            self.daemon_notice.clear();
        }
        let mut unresolved = false;
        let mut refused = false;
        let mut other_failure = false;
        let mut mid_drop = false;
        for tab in &mut self.tabs {
            let eligible = tab.auto && !tab.demo && !tab.connected && !tab.closing && !tab.remove;
            unresolved |= eligible;
            // Always consume stale flags, but only recover coordinator-owned tabs.
            refused |= std::mem::take(&mut tab.connect_failed_refused) && eligible;
            other_failure |= std::mem::take(&mut tab.connect_failed_other) && eligible;
            mid_drop |= std::mem::take(&mut tab.mid_drop) && eligible;
        }
        if !unresolved {
            self.retry_at = None;
            if self.reconnect_budget.take().is_some() {
                self.daemon_notice.clear();
            }
            return;
        }
        // Carry a pending startup retry into partial startup success as a
        // bounded recovery round instead of silently discarding it.
        if self.daemon_phase == daemon::Phase::Ready
            && self.retry_at.is_some()
            && self.reconnect_budget.is_none()
        {
            self.reconnect_budget = Some(daemon::MAX_RECONNECT_ROUNDS);
        }
        // A scheduled retry tick has come due: run the next reconnect round.
        if let Some(at) = self.retry_at
            && Instant::now() >= at
        {
            self.retry_at = None;
            if let daemon::Phase::Starting { attempts } = self.daemon_phase {
                if attempts + 1 > daemon::MAX_DAEMON_RETRIES {
                    let message =
                            "Started the local daemon but it never accepted connections (see Server → logs)."
                                .to_owned();
                    self.daemon_phase = daemon::Phase::Stopped(message.clone());
                    self.daemon_notice = message;
                    return;
                }
                self.daemon_phase = daemon::Phase::Starting {
                    attempts: attempts + 1,
                };
                self.reconnect_auto_tabs();
            } else if let Some(remaining @ 1..) = self.reconnect_budget {
                // Mid-session reconnect round: spend one budget unit.
                self.reconnect_auto_tabs();
                self.reconnect_budget = Some(remaining - 1);
            }
        }
        // A drop of an established session — or a fresh connect failure while
        // Ready — starts a bounded reconnect reusing the retry machinery.
        // The daemon owns the picker's task list, so a dropped session makes
        // the cached snapshot stale: clear it and let the reconnect refetch
        // (there is no manual refresh control).
        if mid_drop {
            self.conversations_loaded = false;
        }
        let reconnect_signal = mid_drop
            || (matches!(self.daemon_phase, daemon::Phase::Ready) && (refused || other_failure));
        if reconnect_signal && self.retry_at.is_none() {
            if self.reconnect_budget.is_none() || (mid_drop && self.reconnect_budget == Some(0)) {
                // Only a new episode can allocate a budget, not repeated failures.
                self.reconnect_budget = Some(daemon::MAX_RECONNECT_ROUNDS);
            }
            if self.reconnect_budget.is_some_and(|remaining| remaining > 0) {
                self.schedule_retry(ctx, Duration::from_millis(daemon::RECONNECT_RETRY_DELAY_MS));
            } else {
                // Budget exhausted: retain reachability for live peers, but
                // leave the notice and exhausted budget until manual recovery.
                let message = format!(
                    "Lost the daemon connection and could not restore it after {} attempts. \
                     Reconnect manually in the Server dialog.",
                    daemon::MAX_RECONNECT_ROUNDS
                );
                if !reachable {
                    self.daemon_phase = daemon::Phase::Stopped(message.clone());
                }
                self.daemon_notice = message;
            }
        }
        // React to a fresh failure only when no retry round is already pending
        // (multiple tabs fail in the same burst; one decision is enough).
        if (refused || other_failure) && self.retry_at.is_none() {
            match &self.daemon_phase {
                daemon::Phase::Ready => {}
                daemon::Phase::Stopped(_) => {} // Keep the message; manual action only.
                daemon::Phase::Probe => {
                    let address = self.effective_address();
                    if refused
                        && daemon::local_endpoint(&address)
                            .is_ok_and(|endpoint| endpoint.port() == 7878)
                    {
                        let _ = self.start_local_daemon(ctx);
                    } else {
                        let message = format!(
                            "No daemon responding at {address}. Check the local daemon or SSH tunnel in Connection settings. Custom ports never auto-start a daemon."
                        );
                        self.daemon_phase = daemon::Phase::Stopped(message.clone());
                        self.daemon_notice = message;
                    }
                }
                daemon::Phase::Starting { .. } => {
                    self.schedule_retry(ctx, Duration::from_millis(daemon::DAEMON_RETRY_DELAY_MS));
                }
            }
        }
    }

    /// Toolbar pill for the active provider. Returns a compact visible label
    /// (provider name only), the full "label · model" text for hover purposes,
    /// and a status color.
    fn model_pill(&self, tab_index: usize) -> (String, String, egui::Color32) {
        let Some(tab) = self.tabs.get(tab_index) else {
            return (
                "Model …".into(),
                "No conversation tab".into(),
                egui::Color32::GRAY,
            );
        };
        let provider_id = &tab.state.snapshot.provider_id;
        let model = &tab.state.snapshot.provider_model;
        if provider_id.is_empty() || model.is_empty() {
            return (
                "Model …".into(),
                "Waiting for this conversation's runtime snapshot".into(),
                egui::Color32::from_rgb(235, 190, 80),
            );
        }
        let label = self
            .config
            .as_ref()
            .and_then(|config| config.providers.iter().find(|p| p.id == *provider_id))
            .map(|p| p.label.as_str())
            .unwrap_or(provider_id.as_str());
        (
            format!("Model: {model}"),
            format!("{label} · {model}"),
            egui::Color32::from_rgb(110, 200, 120),
        )
    }

    /// Toolbar pill describing the daemon target.
    fn daemon_pill(&self) -> (String, egui::Color32) {
        if self.demo {
            return ("Demo".into(), egui::Color32::from_rgb(140, 170, 255));
        }
        match &self.daemon_phase {
            daemon::Phase::Ready => (
                format!(
                    "Connected · {}",
                    self.tabs
                        .get(self.selected)
                        .map(|tab| tab.endpoint.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(&self.address)
                ),
                egui::Color32::from_rgb(110, 200, 120),
            ),
            daemon::Phase::Probe | daemon::Phase::Starting { .. } => (
                "… Starting daemon".into(),
                egui::Color32::from_rgb(235, 190, 80),
            ),
            daemon::Phase::Stopped(_) => (
                "Daemon offline".into(),
                egui::Color32::from_rgb(235, 90, 90),
            ),
        }
    }

    fn close_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.state.busy
            || !tab.composer.is_empty()
            || !tab.attachments.is_empty()
            || !tab.queue.is_empty()
        {
            self.close_target = Some(tab.id);
            return;
        }
        self.finish_close_tab(index);
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        let Some(id) = self.close_target else {
            return;
        };
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            self.close_target = None;
            return;
        };
        let busy = self.tabs[index].state.busy;
        let draft = !self.tabs[index].composer.is_empty()
            || !self.tabs[index].attachments.is_empty()
            || !self.tabs[index].queue.is_empty();
        let response =
            crate::surface::modal(ctx, egui::Id::new("confirm-close-tab")).show(ctx, |ui| {
                ui.heading("Close conversation?");
                ui.label(self.tabs[index].title());
                if busy {
                    ui.label(
                        "This conversation is still working. Closing will stop its current turn.",
                    );
                }
                if draft {
                    ui.label(
                        "Your unsent draft, attachments and queued messages will be discarded.",
                    );
                }
                ui.label("Saved messages will remain in history.");
                ui.horizontal(|ui| {
                    if ui.button("Keep open").clicked() {
                        self.close_target = None;
                    }
                    if ui
                        .button(if busy {
                            "Stop and close"
                        } else {
                            "Discard draft and close"
                        })
                        .clicked()
                    {
                        self.close_target = None;
                        self.finish_close_tab(index);
                    }
                });
            });
        if response.should_close() {
            self.close_target = None;
        }
    }

    fn finish_close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let (busy, connected, connecting) = {
            let tab = &mut self.tabs[index];
            tab.closing = true;
            tab.connection_status = "Closing…".into();
            (tab.state.busy, tab.connected, tab.connecting)
        };
        if connected {
            if busy {
                // Stop any running turn before dropping the socket.
                let _ = self.tabs[index]
                    .commands
                    .send(Command::Send(RuntimeCommand::Cancel));
            }
            let _ = self.tabs[index].commands.send(Command::Disconnect);
            // Remove when the worker acknowledges with Disconnected.
        } else if connecting {
            // In-flight connect: dropping both channel ends stops the worker.
            self.tabs[index].remove = true;
        } else {
            // Never connected: nothing to tear down.
            self.tabs[index].remove = true;
        }
    }

    /// Apply one Ctrl-gated shortcut (the modifier is checked by the caller).
    /// Returns whether the key was handled.
    fn apply_shortcut(&mut self, key: egui::Key, ctx: &egui::Context) -> bool {
        match key {
            egui::Key::T => {
                self.add_tab(Intent::New, ctx);
                if !self.demo {
                    self.connect_index(self.selected);
                }
                self.sidebar_notice.clear();
                true
            }
            egui::Key::W => {
                if self.tabs.get(self.selected).is_none() {
                    return false;
                }
                self.close_tab(self.selected);
                true
            }
            egui::Key::PageUp => self.cycle_group_tab(-1, ctx),
            egui::Key::PageDown => self.cycle_group_tab(1, ctx),
            key @ (egui::Key::Num1
            | egui::Key::Num2
            | egui::Key::Num3
            | egui::Key::Num4
            | egui::Key::Num5
            | egui::Key::Num6
            | egui::Key::Num7
            | egui::Key::Num8
            | egui::Key::Num9) => {
                let index = match key {
                    egui::Key::Num1 => 0,
                    egui::Key::Num2 => 1,
                    egui::Key::Num3 => 2,
                    egui::Key::Num4 => 3,
                    egui::Key::Num5 => 4,
                    egui::Key::Num6 => 5,
                    egui::Key::Num7 => 6,
                    egui::Key::Num8 => 7,
                    _ => 8, // Digit9
                };
                self.select_group_tab(index, ctx)
            }
            egui::Key::Backslash => {
                if let Some(tab) = self.tabs.get(self.selected) {
                    self.split_conversation(tab.id, workspace::Axis::Horizontal, ctx);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Consume the Ctrl+key shortcuts (new tab, close tab, tab selection,
    /// split toggle) regardless of which widget has focus.
    fn handle_shortcuts(&mut self, ui: &mut egui::Ui) {
        if self.modal_open() || !ui.input(|i| i.modifiers.command) {
            return;
        }
        if self.palette.open || file_refs::is_open(ui.ctx()) {
            return;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
            self.palette
                .open(true, self.tabs.get(self.selected).map_or(0, |t| t.id));
            return;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::P)) {
            self.palette
                .open(false, self.tabs.get(self.selected).map_or(0, |t| t.id));
            return;
        }
        if ui.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::N,
            )
        }) {
            self.new_native_window(ui.ctx());
            return;
        }
        if ui.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Backslash,
            )
        }) {
            if let Some(tab) = self.tabs.get(self.selected) {
                self.split_conversation(tab.id, workspace::Axis::Vertical, ui.ctx());
            }
            return;
        }
        const KEYS: [egui::Key; 14] = [
            egui::Key::T,
            egui::Key::W,
            egui::Key::PageUp,
            egui::Key::PageDown,
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
            egui::Key::Backslash,
        ];
        for key in KEYS {
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, key))
                && self.apply_shortcut(key, ui.ctx())
            {
                return;
            }
        }
    }

    fn navigation_palette(&mut self, ctx: &egui::Context) {
        if !self.palette.open {
            return;
        }
        let mut entries = Vec::new();
        if self.palette.commands {
            if let Some(tab) = self.tabs.iter().find(|tab| tab.id == self.palette.tab) {
                let advertised = tab
                    .state
                    .frontend
                    .as_ref()
                    .map(|f| f.commands.as_slice())
                    .unwrap_or(&[]);
                entries.extend(
                    commands::merge_commands(advertised)
                        .into_iter()
                        .filter(|(name, _)| {
                            !matches!(name.as_str(), "e" | "clear" | "provider" | "exit")
                        })
                        .map(|(name, detail)| palette::Entry {
                            label: task_ui::command_label(&name),
                            detail,
                            action: palette::Action::Command { tab: tab.id, name },
                        }),
                );
            }
        } else {
            entries.push(palette::Entry {
                label: "New task".into(),
                detail: "Start a new conversation".into(),
                action: palette::Action::New,
            });
            entries.extend(self.tabs.iter().map(|tab| palette::Entry {
                label: tab.title(),
                detail: tab.workspace.clone(),
                action: palette::Action::Task(tab.id),
            }));
            entries.extend(
                self.conversations
                    .iter()
                    .filter(|meta| {
                        !self
                            .tabs
                            .iter()
                            .any(|tab| tab.conversation_id == Some(meta.id))
                    })
                    .map(|meta| palette::Entry {
                        label: if meta.title.trim().is_empty() {
                            format!("Task {}", meta.id)
                        } else {
                            meta.title.clone()
                        },
                        detail: format!(
                            "{} · {} messages",
                            task_ui::relative_date(&meta.updated_at, &meta.updated_at_local),
                            meta.message_count
                        ),
                        action: palette::Action::Recent(meta.id),
                    }),
            );
        }
        match self.palette.show(ctx, &entries) {
            Some(palette::Action::Task(id)) => self.focus_conversation(id, ctx),
            Some(palette::Action::Recent(id)) => self.open_conversation(id, ctx),
            Some(palette::Action::New) => {
                self.apply_shortcut(egui::Key::T, ctx);
            }
            Some(palette::Action::Command { tab, name }) => {
                self.run_palette_action(tab, &name, ctx);
            }
            None => {}
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        // A panel's child min-content width otherwise becomes a competing
        // constraint: one long task title can make egui grow the resizable
        // panel far past its default width. The sidebar is deliberately a
        // clipped, responsive surface; rows truncate instead of expanding it.
        ui.set_min_width(0.0);
        ui.set_max_width(ui.available_width());
        ui.add_space(8.0);
        let ctx = ui.ctx().clone();
        // One flat hierarchy: a prominent New task action, a search box, then
        // the compact Open and Recent headings with their task lists below.
        if task_ui::new_task_button(ui).clicked() {
            self.add_tab(Intent::New, &ctx);
            self.sidebar_notice.clear();
            self.note_layout_change(&ctx);
            if !self.demo {
                self.connect_index(self.selected);
            }
        }
        ui.add_space(2.0);
        task_ui::task_search(ui, &mut self.history_search);
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("sidebar-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| self.sidebar_lists(ui));
    }

    fn conversation_pane(&mut self, index: usize, ui: &mut egui::Ui) -> bool {
        let palette = self.palette();
        let theme_colors = self.theme_colors();
        let inside = self
            .last_pointer
            .is_some_and(|pos| ui.available_rect_before_wrap().contains(pos));
        let blocked = self.modal_open() || file_refs::is_open(ui.ctx());
        if inside && !blocked {
            if ui.input(|i| i.pointer.any_pressed()) {
                self.focus_conversation(self.tabs[index].id, ui.ctx());
            }
            if ui.input(|i| !i.raw.hovered_files.is_empty()) {
                ui.label("Drop images into this conversation");
            }
            let dropped = ui.input(|i| i.raw.dropped_files.clone());
            if !dropped.is_empty() {
                self.tabs[index].apply_drops(&dropped);
            }
        }
        let model_selector = (!self.demo).then(|| self.model_pill(index));
        let danger = self.approval_mode() == "danger";
        let approval_keyboard_enabled = !blocked
            && ui.is_enabled()
            && self.workspace.active_tab(self.window_ui.rendering) == Some(self.tabs[index].id);
        let tab = &mut self.tabs[index];
        tab.transcript
            .set_tool_verbosity(self.display.tool_verbosity, &tab.state.toolcards);
        let colors = theme_colors;
        let previous_page = tab.live_pane.selected.clone();
        let changed = ui
            .push_id(tab.id, |ui| {
                file_refs::set_workspace(ui.ctx(), &tab.workspace);
                let changed = tab.body(
                    ui,
                    &colors,
                    self.panes_visible.then_some(&palette),
                    model_selector,
                    danger,
                    approval_keyboard_enabled,
                );
                file_refs::set_workspace(ui.ctx(), "");
                changed
            })
            .inner;
        if self.selected == index && tab.live_pane.selected != previous_page {
            self.active_pane = tab.live_pane.selected.clone().map(|page| (tab.id, page));
        }
        // Render this pane's larger image preview, if one is selected. The id
        // is pane-specific so the left and split previews cannot collide.
        tab.image_cache
            .show_preview(ui.ctx(), ("preview", tab.id, index));
        // Daemon-owned float overlays (Lua `api.ui.open_float`), anchored and
        // sized by `FloatRect`. Salted by tab id so split panes don't collide.
        // Keyboard scrolling adds a frontend offset; the active float is
        // outlined; `toggle_panes` hides them entirely.
        let pane_offsets: std::collections::HashMap<String, i64> = self
            .pane_scroll
            .iter()
            .filter(|&((tab_id, _id), _offset)| *tab_id == tab.id)
            .map(|((_tab_id, id), offset)| (id.clone(), *offset))
            .collect();
        let active = self
            .active_pane
            .as_ref()
            .filter(|(tab_id, _)| *tab_id == tab.id)
            .and_then(|(_, id)| match id {
                live_pane::PageId::Extension(id) => Some(id.as_str()),
                live_pane::PageId::Agents => None,
            });
        panes::render_floats(
            ui.ctx(),
            tab.id,
            &tab.state.view,
            &palette,
            self.panes_visible && tab.state.pending_key.is_none(),
            active,
            &pane_offsets,
        );
        changed
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if icons::button(ui, icons::Icon::Sidebar, "Toggle task sidebar").clicked() {
                ui.ctx().data_mut(|data| {
                    let id = egui::Id::new(("sidebar-hidden", self.window_ui.rendering));
                    let hidden = data.get_temp::<bool>(id).unwrap_or(false);
                    data.insert_temp(id, !hidden);
                });
            }
            let (pill, color) = self.daemon_pill();
            let (dot, response) =
                ui.allocate_exact_size(egui::vec2(12.0, 20.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.0, color);
            response.on_hover_text(&pill);
            if !self.demo && !matches!(self.daemon_phase, daemon::Phase::Ready) {
                ui.label(egui::RichText::new(pill).small());
            }
            // A text-only menu bar in the spirit of File/Edit/View: the labels
            // carry no button chrome and drop their menus on click. The active
            // conversation's title stays in the tab strip below, not up here.
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("Layout", |ui| self.window_menu(ui))
                    .response
                    .on_hover_text("Windows and split layout");
                if !self.demo {
                    self.permissions_menu(ui);
                }
                if ui
                    .button("Changes")
                    .on_hover_text("Review changed files in this workspace")
                    .clicked()
                {
                    self.review.open = true;
                    self.request_review();
                }
                ui.menu_button("More", |ui| self.toolbar_menu(ui))
                    .response
                    .on_hover_text("View & settings");
            });
        });
        if let Some(tab) = self.tabs.get(self.selected) {
            let palette = self.palette();
            let running = tab
                .state
                .processes
                .iter()
                .filter(|process| process.state == bone_protocol::ProcessState::Running)
                .count();
            let jobs = tab.state.jobs.len();
            // The live running command, token usage, and turn timing left the
            // toolbar: the command renders inside the chat and usage/timing sit
            // under the composer. Only background work and errors stay up here.
            if tab.state.last_error.is_some() || running > 0 || jobs > 0 {
                ui.horizontal_wrapped(|ui| {
                    let mut first = true;
                    if running > 0 || jobs > 0 {
                        if !first {
                            ui.separator();
                        }
                        ui.label(format!(
                            "{running} running · {jobs} job{}",
                            if jobs == 1 { "" } else { "s" }
                        ))
                        .on_hover_text("Background processes and jobs (see Activity)");
                        first = false;
                    }
                    if let Some(error) = &tab.state.last_error {
                        if !first {
                            ui.separator();
                        }
                        ui.colored_label(egui::Color32::from_rgb(235, 90, 90), brief_error(error));
                    }
                });
            }
            // Daemon-owned status lines (Lua `api.ui.set_statusline`) render
            // beneath the native status row, mirroring the TUI's status bar.
            for component in &tab.state.view.components {
                if let bone_protocol::Component::StatusLine { segments, .. } = component {
                    panes::render_status_line(ui, segments, &tab.state.view.highlights, &palette);
                }
            }
        }
        if !self.demo && !self.daemon_notice.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(235, 90, 90),
                format!("Daemon: {}", self.daemon_notice),
            );
        }
        if !self.config_notice.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(235, 190, 80), &self.config_notice);
        }
        if !self.provider_notice.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(235, 90, 90), &self.provider_notice);
        }
        if !self.demo && !self.version_notice.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(235, 190, 80),
                self.version_notice.clone(),
            );
        }
    }

    /// Secondary toolbar actions and view settings, collapsed into one menu so
    /// the top bar stays a compact single row.
    fn toolbar_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("Switch task…  Ctrl/Cmd+P").clicked() {
            self.palette
                .open(false, self.tabs.get(self.selected).map_or(0, |t| t.id));
            ui.close();
        }
        if ui.button("Workspace changes…").clicked() {
            self.review.open = true;
            self.request_review();
            ui.close();
        }
        if ui.button("Commands…  Ctrl/Cmd+K").clicked() {
            self.palette
                .open(true, self.tabs.get(self.selected).map_or(0, |t| t.id));
            ui.close();
        }
        ui.separator();
        if self.has_connected_tab() {
            let incognito = self
                .tabs
                .get(self.selected)
                .is_some_and(|tab| tab.state.snapshot.incognito);
            if ui
                .add_enabled(
                    self.tabs
                        .get(self.selected)
                        .is_some_and(|tab| tab.connected && !tab.demo),
                    egui::Button::selectable(incognito, "Incognito for this task"),
                )
                .on_hover_text(
                    "Session-scoped: while on, nothing is written to the conversation database",
                )
                .clicked()
            {
                self.set_incognito(!incognito);
                ui.close();
            }
            let (process_count, job_count) = self
                .tabs
                .get(self.selected)
                .map(|tab| (tab.state.processes.len(), tab.state.jobs.len()))
                .unwrap_or((0, 0));
            let active = process_count + job_count;
            let label = if active > 0 {
                format!("Activity ({active})")
            } else {
                "Activity".to_string()
            };
            if ui
                .add_enabled(true, egui::Button::selectable(self.show_activity, label))
                .on_hover_text("Background processes and jobs")
                .clicked()
            {
                self.show_activity = !self.show_activity;
                if self.show_activity {
                    self.activity_tab = self
                        .tabs
                        .get(self.selected)
                        .filter(|tab| tab.connected && !tab.demo)
                        .map(|tab| tab.id)
                        .or_else(|| {
                            self.tabs
                                .iter()
                                .find(|tab| tab.connected && !tab.demo)
                                .map(|tab| tab.id)
                        });
                    self.refresh_activity();
                } else {
                    self.activity_tab = None;
                }
                ui.close();
            }
            if ui
                .add_enabled(true, egui::Button::selectable(self.show_stats, "Usage"))
                .on_hover_text("Token usage statistics")
                .clicked()
            {
                self.show_stats = !self.show_stats;
                if self.show_stats && self.stats.is_none() {
                    self.refresh_stats();
                }
                ui.close();
            }
            let updates = self.catalog_updates();
            let catalog_label = if updates > 0 {
                format!("Catalog ({updates})")
            } else {
                "Catalog".to_string()
            };
            if ui
                .add_enabled(
                    true,
                    egui::Button::selectable(self.show_catalog, catalog_label),
                )
                .on_hover_text("Browse and install extensions")
                .clicked()
            {
                self.show_catalog = !self.show_catalog;
                if self.show_catalog {
                    self.request_catalog(true);
                }
                ui.close();
            }
        }
        ui.separator();
        if !self.demo && ui.button("Server & connection…").clicked() {
            self.show_server = true;
            ui.close();
        }
        if !self.demo && ui.button("Provider setup…").clicked() {
            self.show_setup = true;
            ui.close();
        }
        ui.menu_button("Tool calls", |ui| {
            let before = self.display.tool_verbosity;
            ui.selectable_value(
                &mut self.display.tool_verbosity,
                layout::ToolVerbosity::Concise,
                "Concise",
            )
            .on_hover_text(
                "Compact summaries with filenames and commands; edit diffs stay visible",
            );
            ui.selectable_value(
                &mut self.display.tool_verbosity,
                layout::ToolVerbosity::Verbose,
                "Verbose",
            )
            .on_hover_text("Expand tool arguments and output; edit diffs stay visible");
            if before != self.display.tool_verbosity {
                self.note_layout_change(ui.ctx());
            }
        });
        ui.horizontal(|ui| {
            ui.label("Zoom");
            let zoom = ui.ctx().zoom_factor();
            let mut next = zoom;
            if ui.button("−").clicked() {
                next = (zoom - 0.1).max(0.75);
            }
            if ui
                .button(format!("{:.0}%", zoom * 100.0))
                .on_hover_text("Reset zoom")
                .clicked()
            {
                next = 1.0;
            }
            if ui.button("+").clicked() {
                next = (zoom + 0.1).min(2.0);
            }
            if (next - zoom).abs() > f32::EPSILON {
                ui.ctx().set_zoom_factor(next);
            }
        });
        ui.separator();
        if let Some(tab) = self.tabs.get(self.selected) {
            ui.label(format!("Socket: {}", tab.connection_status));
            ui.label(format!("Turn: {}", tab.state.status));
            ui.label(format!("Host API: {}", tab.host_api_version));
        }
        ui.weak("Ctrl/Cmd+T: new · W: close focused pane");
        ui.weak("Ctrl/Cmd+PageUp/PageDown: switch conversation");
        ui.weak("Ctrl/Cmd+P: switch task · K: command picker");
    }

    /// Advanced connection dialog: address editing, daemon status, and manual
    /// connect/disconnect. The local (loopback) case is normally automatic, so
    /// this is only for remote daemons and manual control.
    fn server_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || !self.show_server {
            return;
        }
        let mut open = self.show_server;
        crate::surface::Surface::new("Connection", "Manage your connection to the Bone daemon.")
            .size(560.0, 600.0)
            .body_scroll(true)
            .show(ctx, &mut open, |ui| {
                ui.label(
                    "Connections are restricted to loopback because Bone TCP has no \
                     authentication or encryption. For remote use, first create an SSH \
                     tunnel, then enter 127.0.0.1:<forwarded-port>. The standard local \
                     address can start a missing daemon automatically.",
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Address");
                    let field =
                        ui.add(egui::TextEdit::singleline(&mut self.address).desired_width(180.0));
                    if field.changed() {
                        self.note_layout_change(ctx);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Daemon");
                    let (pill, color) = self.daemon_pill();
                    ui.colored_label(color, pill);
                    if let Some(pid) = self.daemon_pid {
                        ui.weak(format!("(pid {pid})"));
                    }
                });
                if !self.daemon_notice.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(235, 90, 90), &self.daemon_notice);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let selected = self.tabs.get(self.selected);
                    let connected = selected
                        .map(|tab| tab.connected && !tab.demo)
                        .unwrap_or(false);
                    let connecting = selected
                        .map(|tab| tab.connecting && !tab.demo)
                        .unwrap_or(false);
                    if ui
                        .add_enabled(!connected && !connecting, egui::Button::new("Connect"))
                        .clicked()
                    {
                        self.connect_selected();
                    }
                    if ui
                        .add_enabled(connected, egui::Button::new("Disconnect"))
                        .clicked()
                    {
                        self.disconnect_selected();
                    }
                    let can_start = daemon::is_loopback(&self.effective_address())
                        && !matches!(
                            self.daemon_phase,
                            daemon::Phase::Ready | daemon::Phase::Starting { .. }
                        );
                    if ui
                        .add_enabled(can_start, egui::Button::new("Start local daemon"))
                        .clicked()
                    {
                        self.start_local_daemon(ctx);
                    }
                });
                ui.add_space(2.0);
                ui.weak(
                    "A daemon this app started keeps running after the app closes \
                     and is shared with other Bone clients.",
                );
            });
        if !open {
            self.show_server = false;
        }
    }

    /// Background process/job viewer (Phase 5). Renders the origin tab's
    /// daemon snapshots and sends cancel commands for running items.
    fn activity_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || !self.show_activity {
            return;
        }
        let palette = self.palette();
        let mut open = self.show_activity;
        let mut actions = Vec::new();
        // Snapshot the origin tab's ids/running flags so keyboard nav can
        // borrow `self` mutably, then hand the selection to the renderer.
        let (process_ids, job_ids) = match self.activity_tab_index() {
            Some(index) => {
                let state = &self.tabs[index].state;
                let process_ids: Vec<(String, bool)> = state
                    .processes
                    .iter()
                    .map(|process| {
                        (
                            process.id.clone(),
                            process.state == bone_protocol::ProcessState::Running,
                        )
                    })
                    .collect();
                let job_ids: Vec<(String, bool)> = state
                    .jobs
                    .iter()
                    .map(|job| {
                        (
                            job.id.clone(),
                            job.status == bone_protocol::JobStatus::Running,
                        )
                    })
                    .collect();
                (process_ids, job_ids)
            }
            None => (Vec::new(), Vec::new()),
        };
        actions.extend(self.handle_activity_keys(ctx, &process_ids, &job_ids));
        let selected = self.activity_selected;
        crate::surface::Surface::new("Activity", "Background processes and agent jobs.")
            .size(860.0, 640.0)
            .body_scroll(true)
            .show(ctx, &mut open, |ui| {
                let Some(index) = self.activity_tab_index() else {
                    ui.weak("No connected conversation.");
                    return;
                };
                egui::ScrollArea::vertical()
                    .max_height(440.0)
                    .show(ui, |ui| {
                        actions.extend(activity::render(
                            ui,
                            &self.tabs[index].state.processes,
                            &self.tabs[index].state.jobs,
                            &palette,
                            Some(selected),
                        ));
                    });
            });
        self.show_activity = open;
        if !open {
            self.activity_tab = None;
        }
        let Some(index) = self.activity_tab_index() else {
            return;
        };
        for action in actions {
            match action {
                activity::ActivityAction::CancelProcess(id) => {
                    self.tabs[index].command(RuntimeCommand::CancelProcess { id });
                }
                activity::ActivityAction::CancelJob(id) => {
                    self.tabs[index].command(RuntimeCommand::CancelJob { id });
                }
                activity::ActivityAction::OpenProcess(id) => {
                    self.process_view = Some(activity::ProcessViewer::new(self.tabs[index].id, id));
                }
                activity::ActivityAction::OpenJob(id) => {
                    self.job_view = Some(activity::JobViewer::new(self.tabs[index].id, id));
                }
                activity::ActivityAction::Select(row) => {
                    self.activity_selected = row;
                }
            }
        }
    }

    /// Fullscreen live-output process viewer (Phase 5, parity with the TUI's
    /// `process_view`). Renders the origin tab's snapshot for the open process
    /// id and sends a cancel command through that same tab. Closes itself once
    /// the process is no longer in the snapshot.
    fn process_view_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || self.process_view.is_none() {
            return;
        }
        let palette = self.palette();
        let viewer = self.process_view.as_mut().expect("checked above");
        let mut open = true;
        let mut actions = Vec::new();
        crate::surface::Surface::new("Process output", "Live output from the selected process.")
            .size(960.0, 680.0)
            .body_scroll(false)
            .show(ctx, &mut open, |ui| {
                let Some(index) = self
                    .tabs
                    .iter()
                    .position(|tab| tab.id == viewer.tab_id && tab.connected && !tab.demo)
                else {
                    ui.weak("No connected conversation.");
                    return;
                };
                let Some(process) = self.tabs[index]
                    .state
                    .processes
                    .iter()
                    .find(|process| process.id == viewer.id)
                else {
                    ui.weak("Process is no longer tracked.");
                    return;
                };
                egui::ScrollArea::vertical()
                    .id_salt(("process-view-scroll", viewer.id.as_str()))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        actions = activity::render_process_viewer(ui, process, viewer, &palette);
                    });
            });
        if !open {
            self.process_view = None;
            return;
        }
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.id == viewer.tab_id && tab.connected && !tab.demo)
        else {
            return;
        };
        // Drop the viewer once the process leaves the snapshot.
        let gone = !self.tabs[index]
            .state
            .processes
            .iter()
            .any(|process| process.id == viewer.id);
        for action in actions {
            if let activity::ActivityAction::CancelProcess(id) = action {
                self.tabs[index].command(RuntimeCommand::CancelProcess { id });
            }
        }
        if gone {
            self.process_view = None;
        }
    }

    /// Full job-transcript viewer (Phase 5, parity with the TUI's `open_job`).
    /// Reads and acts on the originating conversation, even after tab switches.
    /// Closes when the origin or its job leaves the current snapshot.
    fn job_view_dialog(&mut self, ctx: &egui::Context) {
        if self.job_view.is_none() {
            return;
        }
        let palette = self.palette();
        let viewer = self.job_view.as_mut().expect("checked above");
        let Some(index) = self.tabs.iter().position(|tab| tab.id == viewer.tab_id) else {
            self.job_view = None;
            return;
        };
        let mut open = true;
        let mut actions = Vec::new();
        crate::surface::Surface::new(
            "Job transcript",
            "Progress and messages from the selected job.",
        )
        .size(960.0, 680.0)
        .body_scroll(false)
        .show(ctx, &mut open, |ui| {
            let Some(job) = self.tabs[index]
                .state
                .jobs
                .iter()
                .find(|job| job.id == viewer.id)
            else {
                ui.weak("Job is no longer tracked.");
                return;
            };
            egui::ScrollArea::vertical()
                .id_salt(("job-view-scroll", viewer.tab_id, viewer.id.as_str()))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    actions = ui
                        .add_enabled_ui(self.tabs[index].connected, |ui| {
                            activity::render_job_viewer(ui, job, viewer, &palette)
                        })
                        .inner;
                });
        });
        if !open {
            self.job_view = None;
            return;
        }
        let gone = !self.tabs[index]
            .state
            .jobs
            .iter()
            .any(|job| job.id == viewer.id);
        for action in actions {
            if let activity::ActivityAction::CancelJob(id) = action {
                self.tabs[index].command(RuntimeCommand::CancelJob { id });
            }
        }
        if gone {
            self.job_view = None;
        }
    }

    fn open_utility(&mut self, destination: &str) {
        self.show_config = destination == "Settings";
        self.show_catalog = destination == "Catalog";
        self.show_stats = destination == "Usage";
        self.stats_picker = None;
        if self.show_catalog && self.catalog.is_none() {
            self.request_catalog(true);
        }
        if self.show_stats && self.stats.is_none() {
            self.refresh_stats();
        }
    }

    /// Token usage dashboard (Phase 6). Renders the most recent
    /// `UsageStatsSnapshot` and requests a refresh when the user asks.
    fn stats_heat_scale(&self) -> stats::HeatScale {
        let (low, high, empty) = self
            .theme_value()
            .and_then(|value| serde_json::from_value::<theme::ThemeSettings>(value.clone()).ok())
            .unwrap_or_default()
            .heat_colors();
        stats::HeatScale::new(low, high, empty)
    }

    /// Token usage dashboard (Phase 6). Renders the most recent
    /// `UsageStatsSnapshot` and requests a refresh when the user asks.
    fn stats_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || !self.show_stats {
            return;
        }
        let palette = self.palette();
        let heat = self.stats_heat_scale();
        let refreshed_secs = self.stats_refreshed.map(|at| at.elapsed().as_secs());
        let mut open = self.show_stats;
        let mut destination = None;
        let mut action = None;
        crate::surface::Surface::new(
            "Usage",
            "Explore token usage, models, and activity over time.",
        )
        .size(960.0, 680.0)
        .body_scroll(false)
        .show(ctx, &mut open, |ui| {
            destination = surface::navigation(ui, "Usage", |_| {});
            self.stats_picker_controls(ui);
            if !self.stats_notice.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(220, 120, 90), &self.stats_notice);
                ui.separator();
            }
            match self.stats.as_ref() {
                Some(snapshot) => {
                    action = stats::render(
                        ui,
                        snapshot,
                        &stats::StatsView {
                            mode: self.stats_mode,
                            custom: self.stats_custom.as_ref(),
                            refreshed_secs,
                            heat: &heat,
                        },
                        &palette,
                    );
                }
                None => {
                    // A notice (timeout/error) already explains the empty
                    // state; only show the loading hint while it is pending.
                    if self.stats_notice.is_empty() {
                        ui.spinner();
                        ui.weak("Loading usage stats…");
                    } else if ui.button("Try again").clicked() {
                        action = Some(stats::StatsAction::Refresh);
                    }
                }
            }
        });
        self.show_stats = open;
        if let Some(destination) = destination {
            self.open_utility(destination);
        }
        if !open {
            self.stats_picker = None;
        }
        match action {
            Some(stats::StatsAction::Refresh) => {
                self.refresh_stats();
            }
            Some(stats::StatsAction::SetMode(mode)) => {
                self.stats_mode = mode;
                self.stats_picker = None;
                if self.stats_custom.take().is_some() {
                    self.refresh_stats();
                }
            }
            Some(stats::StatsAction::OpenDatePicker) => {
                let (start, end) = self
                    .stats_custom
                    .as_ref()
                    .map(|range| {
                        (
                            if range.start == "0000-01-01" {
                                String::new()
                            } else {
                                range.start.clone()
                            },
                            if range.end == "9999-12-31" {
                                String::new()
                            } else {
                                range.end.clone()
                            },
                        )
                    })
                    .unwrap_or_default();
                self.stats_picker = Some((start, end));
            }
            None => {}
        }
    }

    /// Inline range editor: invalid dates stay visible and never reach the daemon.
    fn stats_picker_controls(&mut self, ui: &mut egui::Ui) {
        let Some((mut start, mut end)) = self.stats_picker.clone() else {
            return;
        };
        let mut apply = false;
        let mut cancel = false;
        egui::Frame::group(ui.style())
            .inner_margin(14)
            .corner_radius(8)
            .show(ui, |ui| {
                ui.strong("Custom date range");
                ui.horizontal_wrapped(|ui| {
                    ui.label("From");
                    ui.add(
                        egui::TextEdit::singleline(&mut start)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(120.0),
                    );
                    ui.label("To");
                    ui.add(
                        egui::TextEdit::singleline(&mut end)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(120.0),
                    );
                });
                ui.weak("Leave either date empty for an open range.");
                let range = stats::custom_range(&start, &end);
                if let Err(error) = &range {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.horizontal(|ui| {
                    apply = ui
                        .add_enabled(range.is_ok(), egui::Button::new("Apply range"))
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if apply {
            if let Ok(range) = stats::custom_range(&start, &end) {
                self.stats_custom = Some(range);
                self.stats_picker = None;
                self.refresh_stats();
            }
        } else if cancel {
            self.stats_picker = None;
        } else {
            self.stats_picker = Some((start, end));
        }
    }

    /// Extension catalog browser (Phase 6). Renders the most recent
    /// `CatalogSnapshot` and sends install/remove `CatalogApply` requests.
    fn catalog_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || !self.show_catalog {
            return;
        }
        let palette = self.palette();
        let mut open = self.show_catalog;
        let mut destination = None;
        let mut actions = Vec::new();
        let mut refresh = false;
        crate::surface::Surface::new("Catalog", "Find and manage extensions for Bone.")
            .size(1040.0, 700.0)
            .body_scroll(false)
            .show(ctx, &mut open, |ui| {
                destination = surface::navigation(ui, "Catalog", |ui| {
                    refresh = ui
                        .add_enabled(self.catalog_request.is_none(), egui::Button::new("Refresh"))
                        .clicked();
                });
                if !self.catalog_notice.is_empty() {
                    ui.weak(&self.catalog_notice);
                }
                match self.catalog.as_ref() {
                    Some(snapshot) => {
                        actions = ui
                            .add_enabled_ui(self.catalog_request.is_none(), |ui| {
                                catalog::render(ui, snapshot, &mut self.catalog_view, &palette)
                            })
                            .inner;
                    }
                    None => {
                        // A notice (timeout/error) already explains the empty
                        // state; only show the loading hint while it is pending.
                        if self.catalog_notice.is_empty() {
                            ui.weak("Loading catalog…");
                        }
                    }
                }
            });
        self.show_catalog = open;
        if let Some(destination) = destination {
            self.open_utility(destination);
        }
        if refresh {
            // Drop any stale in-flight slot so the refresh is not a no-op.
            self.catalog_request = None;
            self.catalog_request_at = None;
            self.catalog_retry_at = None;
            self.request_catalog(true);
        }
        if !actions.is_empty() {
            let mut mapped = Vec::new();
            for action in actions {
                match action {
                    catalog::CatalogUiAction::Apply(changes) => {
                        for (name, install) in changes {
                            mapped.push(CatalogAction {
                                name,
                                action: if install {
                                    CatalogActionKind::Install
                                } else {
                                    CatalogActionKind::Remove
                                },
                            });
                        }
                    }
                }
            }
            self.apply_catalog_actions(mapped);
        }
    }

    /// Schema-driven configuration dialog: renders the daemon's config pages and
    /// sends `SetConfigValue`/`ResetConfigValue` for edits (TUI `/config`).
    fn config_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || !self.show_config {
            return;
        }
        let view = config_view::ConfigView::new(self.config_schema.clone(), self.config.clone());
        let mut open = self.show_config;
        let mut destination = None;
        let mut action: Option<config_view::ConfigUiAction> = None;
        let mut refetch = false;
        crate::surface::Surface::new("Settings", "Customize how Bone looks and works.")
            .size(980.0, 680.0)
            .body_scroll(false)
            .show(ctx, &mut open, |ui| {
                destination = surface::navigation(ui, "Settings", |_| {});
                if view.schema.is_none() || view.snapshot.is_none() {
                    ui.weak("Waiting for the daemon…");
                    if ui.small_button("↻ Refetch").clicked() {
                        refetch = true;
                    }
                    return;
                }
                if !self.config_notice.is_empty() {
                    ui.weak(&self.config_notice);
                }
                config_view::render_pages(
                    ui,
                    &view,
                    &mut self.config_edits,
                    &mut self.config_ui,
                    &mut action,
                );
            });
        self.show_config = open;
        if let Some(destination) = destination {
            self.open_utility(destination);
        }
        if refetch {
            self.config = None;
            self.config_schema = None;
            self.config_request = None;
            self.config_notice.clear();
        }
        if let Some(action) = action {
            self.apply_config_ui_action(action);
        }
    }

    fn pending_key_request(&self) -> Option<(usize, u64)> {
        self.tabs.iter().enumerate().find_map(|(index, tab)| {
            if tab.demo
                || self
                    .workspace
                    .tab_location(tab.id)
                    .map(|(window, _)| window)
                    != Some(self.window_ui.rendering)
            {
                None
            } else {
                tab.state.pending_key.map(|id| (index, id))
            }
        })
    }

    /// Interactive `ctx.ui.key()` capture. Render the owning tab's authoritative
    /// float panes inside the modal, where the backdrop cannot obscure them.
    /// Lua still owns the question, options, selection, and key semantics.
    fn key_capture_dialog(&mut self, ctx: &egui::Context) {
        let Some((index, id)) = self.pending_key_request() else {
            return;
        };
        // Surrender text focus so the next key press reaches this modal instead
        // of being typed into the composer (which draws after the dialogs).
        ctx.memory_mut(|memory| memory.stop_text_input());
        let captured = ctx.input_mut(|input| {
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
                // Printable keys also produce Text events; don't leak those to
                // an editor that requests focus later in this frame.
                input
                    .events
                    .retain(|event| !matches!(event, egui::Event::Text(_)));
            }
            captured
        });
        let view = &self.tabs[index].state.view;
        let palette = self.palette();
        let screen = ctx.content_rect();
        crate::surface::modal(ctx, egui::Id::new("key-capture")).show(ctx, |ui| {
            ui.set_width((screen.width() - 64.0).clamp(1.0, 720.0));
            egui::ScrollArea::vertical()
                .id_salt(("key-capture-panes", self.tabs[index].id))
                .max_height((screen.height() - 64.0).max(1.0))
                .show(ui, |ui| {
                    let mut has_content = false;
                    for component in &view.components {
                        let bone_protocol::Component::Float {
                            title,
                            lines,
                            scroll,
                            ..
                        } = component
                        else {
                            continue;
                        };
                        if lines.is_empty() {
                            continue;
                        }
                        if has_content {
                            ui.separator();
                        }
                        has_content = true;
                        if !title.is_empty() {
                            ui.heading(title);
                        }
                        let start = (*scroll).min(lines.len());
                        panes::render_lines(ui, &lines[start..], &view.highlights, &palette);
                    }
                    if !has_content {
                        ui.heading("Interactive key input");
                        ui.label("A tool is waiting for a key press.");
                        ui.label("Press any key to send it.");
                        ui.separator();
                        ui.weak("Ctrl / Alt / Shift are forwarded with the key.");
                    }
                });
        });

        if let Some((key, modifiers)) = captured {
            let event = keys::key_event(key, modifiers);
            self.tabs[index].state.answer_key(id);
            if !self.tabs[index].command(RuntimeCommand::KeyReply { id, key: event }) {
                self.tabs[index].state.last_error =
                    Some("Could not send the key reply; the connection is closed.".into());
            }
        }
    }

    /// Provider onboarding dialog: renders the `SetupUi` form and sends the
    /// `SetupApply` plan it returns from Save. Dismissing the window (or
    /// clicking Done/Cancel/Close, which call [`setup::SetupUi::close`]) wipes
    /// the in-memory key and drops any in-flight request.
    fn setup_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || !self.show_setup {
            return;
        }
        let mut open = self.show_setup;
        let mut close_requested = false;
        crate::surface::Surface::new(
            "Provider setup",
            "Connect a provider and get ready to work.",
        )
        .size(680.0, 650.0)
        .body_scroll(true)
        .show(ctx, &mut open, |ui| {
            ui.set_min_width(360.0);
            let outcome = self.setup_ui.render(ui);
            close_requested = outcome.close;
            if let Some(request) = outcome.request
                && !self.send_setup_request(request)
            {
                self.setup_ui.abort();
            }
        });
        if close_requested {
            open = false;
        }
        self.show_setup = open;
        if !open {
            // Still actionable and never applied: the user dismissed onboarding
            // without finishing. Nudge them to reopen it later.
            let skipped = self.setup_ui.actionable();
            let completed = self.setup_ui.completed();
            self.setup_ui.close();
            self.setup_request = None;
            if skipped && !completed {
                self.sidebar_notice =
                    "Setup skipped — reopen from View & settings → Provider setup… or /setup."
                        .into();
            }
        }
    }
}

impl Tab {
    /// Intercept autocomplete navigation before the multiline editor sees the
    /// keys, so Enter accepts a suggestion instead of inserting a newline.
    fn handle_autocomplete_keys(&mut self, ui: &mut egui::Ui) {
        let has_matches = self
            .autocomplete
            .as_ref()
            .map(|ac| !ac.matches.is_empty())
            .unwrap_or(false);
        if !has_matches {
            return;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
            if let Some(ac) = self.autocomplete.as_mut() {
                ac.down();
            }
        } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
            if let Some(ac) = self.autocomplete.as_mut() {
                ac.up();
            }
        } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Tab))
            || ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        {
            self.accept_autocomplete();
        }
    }

    /// Keep the approval nav state pinned to the first pending approval. When
    /// that approval changes (answered, replaced, or a new one arrives), reset
    /// to the default selection so a fresh prompt never inherits stale state.
    fn sync_approval_nav(&mut self) {
        let first = self.state.approvals.first().map(|approval| approval.id);
        if first != self.approval_nav_id {
            self.approval_nav_id = first;
            self.approval_selected = 0;
            self.approval_peek = false;
        }
    }

    /// Keyboard navigation for the first pending approval, mirroring the TUI
    /// prompt: Up/Down (and PageUp/PageDown) move the selection, `p` toggles the
    /// command preview, Enter activates the selected option. Returns the reply
    /// to send when Enter confirms; the caller dispatches it. Only called when
    /// no widget holds focus, so it never steals typing or button activation.
    fn handle_approval_keys(&mut self, ui: &mut egui::Ui) -> Option<(u64, CallOutcome)> {
        let first = self.state.approvals.first()?;
        let id = first.id;
        let can_approve = self.connected && first.blocked.is_none();
        let can_deny = self.connected;
        let has_preview = first.preview.is_some();
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
            || ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp))
        {
            self.approval_selected = self.approval_selected.saturating_sub(1);
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
            || ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown))
        {
            self.approval_selected = (self.approval_selected + 1).min(1);
        }
        if has_preview && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::P)) {
            self.approval_peek = !self.approval_peek;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            let outcome = match self.approval_selected {
                0 if can_approve => Some(CallOutcome::Approve),
                1 if can_deny => Some(CallOutcome::Denied),
                _ => None,
            };
            if let Some(outcome) = outcome {
                return Some((id, outcome));
            }
        }
        None
    }

    /// Draw the `/` autocomplete dropdown just below the editor. Returns true
    /// when a row was clicked, which rewrites the composer buffer.
    fn autocomplete_popup(&mut self, ui: &mut egui::Ui, editor: &egui::Response) -> bool {
        if !editor.has_focus() {
            return false;
        }
        let Some(ac) = self.autocomplete.as_ref() else {
            return false;
        };
        if ac.matches.is_empty() {
            return false;
        }
        let rows: Vec<(String, String)> = ac
            .matches
            .iter()
            .skip(ac.scroll_offset)
            .take(commands::MAX_VISIBLE)
            .cloned()
            .collect();
        let selected = ac.selected.saturating_sub(ac.scroll_offset);
        let more = ac.more_count();
        let mut clicked: Option<String> = None;
        egui::Area::new(editor.id.with("autocomplete"))
            .order(egui::Order::Foreground)
            .fixed_pos(editor.rect.left_bottom())
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(editor.rect.width().max(240.0));
                    for (index, (name, description)) in rows.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(index == selected, format!("/{name}"))
                                .clicked()
                            {
                                clicked = Some(name.clone());
                            }
                            ui.weak(description);
                        });
                    }
                    if more > 0 {
                        ui.weak(format!("+{more} more"));
                    }
                });
            });
        match clicked {
            Some(name) => {
                self.composer = format!("/{name}");
                self.autocomplete = None;
                true
            }
            None => false,
        }
    }

    fn composer_panel(
        &mut self,
        ui: &mut egui::Ui,
        model_selector: Option<(String, String, egui::Color32)>,
        _danger: bool,
        approval_keyboard_enabled: bool,
    ) -> bool {
        let editor_id = egui::Id::new((self.id, "composer-editor"));
        self.sync_approval_nav();
        // Only the focused pane may consume approval keys. Disabled panes must
        // also leave modal/dialog input alone, even when no editor has focus.
        let approval_keyboard = approval_keyboard_enabled
            && ui.is_enabled()
            && !self.state.approvals.is_empty()
            && ui.memory(|memory| memory.focused()).is_none();
        let keyboard_reply = if approval_keyboard {
            self.handle_approval_keys(ui)
        } else {
            None
        };
        egui::ScrollArea::vertical()
            .id_salt(("approvals", self.id, self.approval_nav_id))
            .max_height(150.0)
            .show(ui, |ui| {
                let mut reply = keyboard_reply;
                for (index, approval) in self.state.approvals.iter().enumerate() {
                    // Only the first approval is keyboard-targeted, matching the
                    // TUI's single blocking prompt.
                    let targeted = approval_keyboard && index == 0;
                    let selected = if targeted {
                        self.approval_selected
                    } else {
                        usize::MAX
                    };
                    ui.group(|ui| {
                        ui.strong(format!("Approval required: {}", approval.name));
                        // Keep the decisions ahead of arbitrarily long details,
                        // so they are immediately reachable without scrolling.
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled(
                                    self.connected && approval.blocked.is_none(),
                                    egui::Button::new("Approve").selected(selected == 0),
                                )
                                .clicked()
                            {
                                reply = Some((approval.id, CallOutcome::Approve));
                            }
                            if ui
                                .add_enabled(
                                    self.connected,
                                    egui::Button::new("Deny").selected(selected == 1),
                                )
                                .clicked()
                            {
                                reply = Some((approval.id, CallOutcome::Denied));
                            }
                            if targeted {
                                ui.weak("↑/↓ select · Enter confirm");
                                if approval.preview.is_some() {
                                    ui.weak("· p preview");
                                }
                            }
                        });
                        ui.label(&approval.summary);
                        if let Some(blocked) = &approval.blocked {
                            ui.colored_label(egui::Color32::YELLOW, blocked);
                        }
                        if let Some(preview) = &approval.preview {
                            if targeted && self.approval_peek {
                                ui.strong(format!("Preview #{}", approval.id));
                                ui.monospace(preview);
                            } else {
                                ui.collapsing(format!("Preview #{}", approval.id), |ui| {
                                    ui.monospace(preview);
                                });
                            }
                        }
                    });
                }
                if let Some((id, outcome)) = reply
                    && self.command(RuntimeCommand::ApprovalReply { id, outcome })
                {
                    self.state.answered(id);
                }
            });
        if let Some(error) = &self.state.last_error {
            ui.colored_label(egui::Color32::from_rgb(235, 90, 90), brief_error(error));
        }
        // Image attachments: a row of removable chips above the editor.
        let mut attachment_removed = false;
        if !self.attachments.is_empty() {
            let mut remove: Option<usize> = None;
            egui::ScrollArea::horizontal()
                .id_salt("staged-images")
                .max_height(160.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (index, attachment) in self.attachments.iter().enumerate() {
                            ui.vertical(|ui| {
                                self.image_cache.show(
                                    ui,
                                    &attachment.cache_key,
                                    &attachment.name,
                                    &attachment.media_type,
                                    &attachment.data_b64,
                                );
                                ui.label(format!("🖼 {}", attachment.name));
                                ui.weak(format!("({} KB)", attachment.byte_size / 1024));
                                if ui
                                    .small_button("✕")
                                    .on_hover_text("Remove attachment")
                                    .clicked()
                                {
                                    remove = Some(index);
                                }
                            });
                        }
                    })
                });
            if let Some(index) = remove {
                self.attachments.remove(index);
                attachment_removed = true;
            }
            ui.separator();
        }
        // Queued prompts: shown above the editor, sent one per idle turn.
        if !self.queue.is_empty() {
            let mut remove: Option<usize> = None;
            let mut clear_all = false;
            let mut move_up: Option<usize> = None;
            let mut move_down: Option<usize> = None;
            let mut send_next: Option<usize> = None;
            let mut edit: Option<usize> = None;
            let len = self.queue.len();
            if self.queue_paused {
                ui.colored_label(ui.visuals().warn_fg_color, "Queued messages are paused");
                ui.label(
                    "Review these messages before resuming after a connection or chat change.",
                );
                if ui
                    .add_enabled(
                        self.connected && self.state.ready && !self.closing,
                        egui::Button::new("Resume queue"),
                    )
                    .clicked()
                {
                    self.resume_queue();
                }
            }
            ui.horizontal(|ui| {
                ui.strong(format!("Queued ({len})"))
                    .on_hover_text("Each prompt is sent when the task next goes idle");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("Clear all")
                        .on_hover_text("Remove every queued prompt (Ctrl/Cmd+D)")
                        .clicked()
                    {
                        clear_all = true;
                    }
                });
            });
            egui::ScrollArea::vertical()
                .id_salt(("queue", self.id))
                .max_height(140.0)
                .show(ui, |ui| {
                    for (index, item) in self.queue.iter().enumerate() {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak(format!("{}.", index + 1));
                            ui.add(egui::Label::new(one_line(item)).truncate())
                                .on_hover_text(item.as_str());
                            if ui
                                .add_enabled(index > 0, egui::Button::new("Move up").small())
                                .on_hover_text("Send earlier: move this prompt up one")
                                .clicked()
                            {
                                move_up = Some(index);
                            }
                            if ui
                                .add_enabled(
                                    index + 1 < len,
                                    egui::Button::new("Move down").small(),
                                )
                                .on_hover_text("Send later: move this prompt down one")
                                .clicked()
                            {
                                move_down = Some(index);
                            }
                            if ui
                                .add_enabled(index > 0, egui::Button::new("Send next").small())
                                .on_hover_text("Send this prompt before the others")
                                .clicked()
                            {
                                send_next = Some(index);
                            }
                            if ui
                                .small_button("Edit")
                                .on_hover_text("Pull this prompt back into the composer")
                                .clicked()
                            {
                                edit = Some(index);
                            }
                            if ui
                                .small_button("Remove")
                                .on_hover_text("Delete this prompt from the queue")
                                .clicked()
                            {
                                remove = Some(index);
                            }
                        });
                    }
                });
            if clear_all {
                self.queue.clear();
                self.queue_paused = false;
            } else if let Some(index) = move_up {
                self.move_queued_up(index);
            } else if let Some(index) = move_down {
                self.move_queued_down(index);
            } else if let Some(index) = send_next {
                self.send_queued_next(index);
            } else if let Some(index) = edit {
                self.edit_queued(index);
            } else if let Some(index) = remove {
                self.remove_queued(index);
            }
            ui.separator();
        }
        let editor_focused = ui.memory(|memory| memory.has_focus(editor_id));
        self.refresh_autocomplete();
        if editor_focused {
            self.handle_autocomplete_keys(ui);
        }
        // Collapse large pastes into a placeholder token, mirroring the TUI.
        // The event is removed so the TextEdit below does not also insert the
        // whole blob. Only when focused, so we never swallow another widget's
        // paste.
        let mut paste_changed = false;
        if editor_focused {
            let large: Vec<String> = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Paste(text)
                            if text.chars().count() > PASTE_PLACEHOLDER_THRESHOLD =>
                        {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect()
            });
            if !large.is_empty() {
                ui.input_mut(|i| {
                    i.events.retain(|event| {
                        !matches!(
                            event,
                            egui::Event::Paste(text)
                                if text.chars().count() > PASTE_PLACEHOLDER_THRESHOLD
                        )
                    });
                });
                let mut char_index = egui::text_edit::TextEditState::load(ui.ctx(), editor_id)
                    .and_then(|state| state.cursor.char_range())
                    .map(|range| range.primary.index.0)
                    .unwrap_or_else(|| self.composer.chars().count());
                for text in large {
                    char_index += self.insert_paste_placeholder(&text, char_index);
                }
                // Place the caret after the inserted placeholder(s).
                if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), editor_id) {
                    let cursor = egui::text::CCursor::new(char_index);
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(cursor)));
                    state.store(ui.ctx(), editor_id);
                }
                paste_changed = true;
            }
        }
        let input_style = self.state.input_style.clone();
        let pad_h = (input_style.horizontal_padding.min(8) as i8) * 4;
        let pad_v = (input_style.vertical_padding.min(8) as i8) * 4;
        let mut frame = egui::Frame::default().inner_margin(egui::Margin {
            left: pad_h,
            right: pad_h,
            top: pad_v,
            bottom: pad_v,
        });
        match input_style.preset {
            state::InputPreset::Lines => {}
            state::InputPreset::Box => {
                frame = frame
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                    .corner_radius(4.0);
            }
            state::InputPreset::Filled => {
                frame = frame.fill(ui.visuals().extreme_bg_color).corner_radius(4.0);
            }
        }
        let editor = egui::ScrollArea::vertical()
            .id_salt((self.id, "composer-scroll"))
            .max_height(180.0)
            .show(ui, |ui| {
                ui.style_mut()
                    .text_styles
                    .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
                frame
                    .show(ui, |ui| {
                        if input_style.prefix.is_empty() {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.composer)
                                    .id(editor_id)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(2)
                                    .frame(egui::Frame::NONE)
                                    // Enter is an action, not a newline: the
                                    // return-key handler below sends, steers,
                                    // or queues instead of inserting a `\n`.
                                    .return_key(None)
                                    .hint_text("Ask anything, or / for commands"),
                            )
                        } else {
                            ui.horizontal_top(|ui| {
                                ui.label(&input_style.prefix);
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.composer)
                                        .id(editor_id)
                                        .desired_width(ui.available_width())
                                        .desired_rows(2)
                                        .frame(egui::Frame::NONE)
                                        .return_key(None)
                                        .hint_text("Ask anything, or / for commands"),
                                )
                            })
                            .inner
                        }
                    })
                    .inner
            })
            .inner;
        let mut changed = editor.changed() || attachment_removed || paste_changed;
        if editor.changed() {
            self.refresh_autocomplete();
        }
        if self.autocomplete_popup(ui, &editor) {
            changed = true;
        }
        // Return-key actions. The composer's TextEdit has no return key, so
        // these events are still available to consume here:
        //   Enter       — send; queue while a turn is running
        //   Ctrl+Enter  — steer the running turn (send when idle)
        //   Shift+Enter — queue for after the running turn (send when idle)
        // Consume the most specific modifiers first: `consume_key` ignores extra
        // Shift/Alt, so a Shift+Enter event also matches the plain-Enter pattern.
        let editor_focused = editor.has_focus();
        let steer_shortcut = editor_focused
            && (ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
                // Alt+Enter mirrors the TUI's terminal fallback for Ctrl+Enter.
                || ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::Enter)));
        let queue_shortcut = editor_focused
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter));
        let send_shortcut = editor_focused
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        // Ctrl/Cmd+D clears the queue, mirroring the TUI's ClearQueue action.
        // Gated on an empty composer so it never shadows text editing.
        if editor.has_focus()
            && self.composer.is_empty()
            && !self.queue.is_empty()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::D))
        {
            self.queue.clear();
            self.queue_paused = false;
            changed = true;
        }
        // Copy/Cut retain the platform editor behavior. Stop is an explicit action.
        if editor.has_focus() {
            self.handle_input_shortcuts(ui);
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().interact_size.y = 28.0;
            ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
            if self.state.snapshot.incognito {
                ui.label(egui::RichText::new("Incognito").small())
                    .on_hover_text("This task is not being saved to conversation history");
            }
            if let Some((model_label, model_detail, model_color)) = model_selector
                && ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(model_label.trim_start_matches("Model: "))
                                .small()
                                .color(model_color),
                        )
                        .frame(false),
                    )
                    .on_hover_text(format!("Choose provider and model: {model_detail}"))
                    .clicked()
            {
                self.request_ui(UiRequest::OpenProvider);
            }
            // Image attach button, next to the model name. Mirrors the
            // `paste_image` keybinding and drag-and-drop paths.
            if !self.demo
                && icons::button(
                    ui,
                    icons::Icon::Plus,
                    "Attach an image (png, jpeg, webp, gif)",
                )
                .clicked()
            {
                self.attach_image_dialog();
                changed = true;
            }
            let actions_width = if self.state.busy { 192.0 } else { 60.0 };
            ui.add_space((ui.available_size_before_wrap().x - actions_width).max(0.0));
            let primary = |ui: &egui::Ui, label: &str| {
                egui::Button::new(egui::RichText::new(label).color(ui.visuals().panel_fill))
                    .fill(ui.visuals().text_color())
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(4.0)
                    .min_size(egui::vec2(60.0, 28.0))
            };
            if self.state.busy {
                if ui
                    .add_enabled(
                        self.can_steer(),
                        egui::Button::new("Steer")
                            .frame(false)
                            .min_size(egui::vec2(56.0, 28.0)),
                    )
                    .on_hover_text("Inject this text into the running turn (Ctrl+Enter)")
                    .clicked()
                    || steer_shortcut
                {
                    self.steer_composer();
                    changed = true;
                }
                if ui
                    .add_enabled(
                        self.can_queue(),
                        egui::Button::new("Queue")
                            .frame(false)
                            .min_size(egui::vec2(60.0, 28.0)),
                    )
                    .on_hover_text("Send this prompt when the current turn finishes (Shift+Enter)")
                    .clicked()
                    || queue_shortcut
                    || send_shortcut
                {
                    self.enqueue_composer();
                    changed = true;
                }
            } else if ui
                .add_enabled(self.can_send(), primary(ui, "Send"))
                .clicked()
                || send_shortcut
                || queue_shortcut
                || steer_shortcut
            {
                self.submit_composer();
                changed = true;
            }
            // Show exactly one primary action: Stop replaces Send while a turn
            // is running (Steer/Queue stay available above as busy alternatives).
            if self.state.busy
                && ui
                    .add_enabled(self.connected && self.state.ready, primary(ui, "Stop"))
                    .on_hover_text("Cancel the current turn")
                    .clicked()
            {
                self.command(RuntimeCommand::Cancel);
                self.state.status = "Cancelling…".into();
            }
        });
        changed
    }

    /// Returns true when the composer draft changed this frame (typed or sent),
    /// so the caller can mark the restart layout dirty.
    /// Egui persists panel rectangles by id. A bottom panel lives inside the
    /// central pane, so its restored x/width must follow that pane every frame;
    /// otherwise a previous full-window rect leaves a black gutter after the
    /// sidebar is resized or restored.
    fn fit_bottom_panel(ctx: &egui::Context, id: egui::Id, available: egui::Rect) {
        let Some(state) = egui::PanelState::load(ctx, id) else {
            return;
        };
        let height = state.outer_rect.height().min(available.height()).max(0.0);
        let rect = egui::Rect::from_min_max(
            egui::pos2(
                available.left(),
                (available.bottom() - height).max(available.top()),
            ),
            egui::pos2(available.right(), available.bottom()),
        );
        if state.outer_rect != rect {
            ctx.data_mut(|data| data.insert_persisted(id, egui::PanelState { outer_rect: rect }));
        }
    }
    fn body(
        &mut self,
        ui: &mut egui::Ui,
        colors: &theme::ThemeColors,
        pane_palette: Option<&theme::Palette>,
        model_selector: Option<(String, String, egui::Color32)>,
        danger: bool,
        approval_keyboard_enabled: bool,
    ) -> bool {
        // Reserve the composer area before laying out history so long
        // transcripts cannot push it out of the window. The id is per-tab so
        // the split view never registers two bottom panels under one id.
        let live_height = ui.available_height() / 3.0;
        self.live_pane
            .sync(&live_pane::page_ids(&self.state.view, &self.state.jobs));
        let composer_id = egui::Id::new(("composer", self.id));
        Self::fit_bottom_panel(ui.ctx(), composer_id, ui.available_rect_before_wrap());
        let composer_changed = egui::Panel::bottom(composer_id)
            .frame(egui::Frame::NONE.fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                let column_width = ui.available_width().min(theme::CHAT_WIDTH);
                let inset =
                    (ui.available_width() - column_width) * 0.5 + f32::from(theme::CHAT_PADDING);
                ui.add_space(8.0);
                let changed = ui
                    .horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.add_space(inset);
                        ui.vertical(|ui| {
                            ui.set_width(
                                (column_width - 2.0 * f32::from(theme::CHAT_PADDING)).max(1.0),
                            );
                            if self.state.pending_key.is_none()
                                && let Some(palette) = pane_palette
                                && let Some(id) = self.live_pane.render(
                                    ui,
                                    &self.state.view,
                                    &self.state.jobs,
                                    palette,
                                    live_height,
                                )
                            {
                                self.pending_ui.push(UiRequest::OpenJob(id));
                            }
                            let changed = egui::Frame::default()
                                .fill(ui.visuals().faint_bg_color)
                                .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                                .corner_radius(6.0)
                                .inner_margin(egui::Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    self.composer_panel(
                                        ui,
                                        model_selector,
                                        danger,
                                        approval_keyboard_enabled,
                                    )
                                })
                                .inner;
                            // Token usage and turn timing live below the chat
                            // box, not in the toolbar, so they read as composer
                            // detail.
                            let usage = self.state.token_usage.as_ref().map(|usage| {
                                format!(
                                    "curr {} · in {} · out {} · total {}",
                                    activity::format_tokens(usage.context_length),
                                    activity::format_tokens(usage.sent),
                                    activity::format_tokens(usage.received),
                                    activity::format_tokens(usage.sent + usage.received),
                                )
                            });
                            let elapsed = self.state.work_elapsed_ms.map(|elapsed_ms| {
                                format!("worked for {}", state::format_elapsed_ms(elapsed_ms))
                            });
                            let detail = match (usage, elapsed) {
                                (Some(usage), Some(elapsed)) => {
                                    Some(format!("{usage} · {elapsed}"))
                                }
                                (usage, elapsed) => usage.or(elapsed),
                            };
                            if let Some(detail) = detail {
                                ui.add_space(4.0);
                                ui.vertical_centered(|ui| {
                                    ui.label(egui::RichText::new(detail).small().weak());
                                });
                            }
                            changed
                        })
                        .inner
                    })
                    .inner;
                ui.add_space(8.0);
                changed
            })
            .inner;

        ui.painter().rect_filled(
            ui.available_rect_before_wrap(),
            0.0,
            ui.visuals().panel_fill,
        );
        if !self.state.images.is_empty() {
            egui::CollapsingHeader::new(format!(
                "Conversation images ({})",
                self.state.images.len()
            ))
            .id_salt((self.id, "history-images"))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("image-gallery")
                    .max_height(150.0)
                    .show_rows(ui, 130.0, self.state.images.len(), |ui, range| {
                        for index in range {
                            let image = &self.state.images[index];
                            self.image_cache.show(
                                ui,
                                &self.state.image_keys[index],
                                &format!("Image {}", index + 1),
                                &image.media_type,
                                &image.data,
                            );
                        }
                    });
            });
        }
        if let Some(error) = self.state.last_error.clone()
            && !self.state.ready
        {
            ui.group(|ui| {
                ui.strong(brief_error(&error));
                ui.label("Retry this task or start a new one; saved data is unchanged.");
                ui.horizontal(|ui| {
                    if ui.button("Retry load").clicked() {
                        self.retry_failed_load();
                    }
                    if ui.button("Start a new task").clicked() {
                        self.state.reset_new();
                        self.transcript.reset();
                        self.command(RuntimeCommand::NewConversation);
                    }
                });
                ui.collapsing("Technical details", |ui| {
                    ui.monospace(&error);
                });
            });
        }
        if self.state.ready && self.state.rows.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("Ready for a new task");
                ui.weak("Describe what you want to do below.");
            });
        }
        // Rows and tool cards are only borrowed while this call lays out the
        // visible band; cached measurements live in self.transcript.
        // The running command is surfaced inside the chat (after the last
        // message) instead of the toolbar; it is transparent to idle tabs.
        self.transcript.set_status(
            (self.state.busy && !self.state.status.is_empty()).then(|| self.state.status.clone()),
        );
        let response = self.transcript.show(
            ui,
            self.id,
            self.stick_to_bottom,
            &self.state.rows,
            &self.state.toolcards,
            colors,
        );
        // Keep following the bottom until the user scrolls away from it.
        let max_scroll = (response.content_size.y - response.inner_rect.height()).max(0.0);
        let at_bottom = (max_scroll - response.state.offset.y).abs() < 8.0;
        self.stick_to_bottom = at_bottom;
        if !self.stick_to_bottom {
            // Overlay the transcript without allocating space in the parent.
            // Adding a composer row changes its bottom panel's height one frame
            // late, clipping the editor and shifting the viewport as we scroll.
            let mut overlay = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("jump-latest", self.id))
                    .max_rect(response.inner_rect.shrink(8.0))
                    .layout(egui::Layout::bottom_up(egui::Align::Center)),
            );
            overlay.set_clip_rect(ui.clip_rect().intersect(response.inner_rect));
            if overlay
                .add(
                    egui::Button::new(if self.has_new_output {
                        "↓ New output · Jump to latest"
                    } else {
                        "↓ Jump to latest"
                    })
                    .fill(ui.visuals().widgets.inactive.bg_fill),
                )
                .on_hover_text("Reading earlier messages")
                .clicked()
            {
                self.jump_to_latest = true;
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
        composer_changed
    }
}

impl eframe::App for DesktopApp {
    fn on_exit(&mut self) {
        if !self.save_workspace_now() {
            eprintln!("{}", self.sidebar_notice);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.flush_layout(ui.ctx());
        if self.prune_closed() {
            self.note_layout_change(ui.ctx());
        }
        self.poll_task_delete(ui.ctx());
        self.drain_all(ui.ctx());
        self.drain_local();
        self.apply_cli_overrides();
        self.ensure_open_dialog_data();
        // Keep the host-request lifecycle moving even when the daemon is silent:
        // wake at the next timeout/retry deadline so a stalled connection
        // surfaces its notice (and the throttled retry fires) without input.
        let next_host_deadline = [
            self.conversations_request_at
                .map(|at| at + HOST_REQUEST_TIMEOUT),
            self.stats_request_at.map(|at| at + HOST_REQUEST_TIMEOUT),
            self.catalog_request_at.map(|at| at + HOST_REQUEST_TIMEOUT),
            self.stats_retry_at,
            self.catalog_retry_at,
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(deadline) = next_host_deadline {
            ui.ctx()
                .request_repaint_after(deadline.saturating_duration_since(Instant::now()));
        }
        self.apply_theme(ui.ctx());
        if self.prune_closed() {
            self.note_layout_change(ui.ctx());
        }
        // Coordinate the local-daemon lifecycle against connect outcomes and
        // retry ticks collected while draining events above.
        self.pump_daemon(ui.ctx());
        // Reconcile each tab's virtualization cache with its authoritative row
        // vector before anything draws (changed rows are drained per frame).
        for tab in &mut self.tabs {
            let rows_len = tab.state.rows.len();
            let changed = std::mem::take(&mut tab.state.changed_rows);
            if !tab.stick_to_bottom && !changed.is_empty() {
                tab.has_new_output = true;
            }
            tab.transcript
                .sync_with_cards(rows_len, &changed, &tab.state.toolcards);
        }

        self.render_workspace(ui);
        if self.background_tick_due() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
    }
}

fn main() -> eframe::Result {
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
        cli::Command::Gui => {
            // Compiling the bundled syntect grammars takes long enough to stall
            // a frame, so warm them off the UI thread before the first code
            // block appears.
            std::thread::Builder::new()
                .name("markdown-prewarm".to_string())
                .spawn(markdown::prewarm_code_highlighting)
                .ok();
        }
    }
    eframe::run_native(
        "Bone Desktop",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1000.0, 720.0])
                .with_min_inner_size([520.0, 400.0]),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(DesktopApp::new(cc.egui_ctx.clone(), cli)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn errors_have_brief_user_text_without_losing_details() {
        let technical =
            "database conversation 7 not found at /very/long/private/path/messages.sqlite";
        assert_eq!(brief_error(technical), "Conversation could not be loaded");
        assert!(!brief_error(technical).contains("/very/long"));
        assert_eq!(
            brief_error("permission denied"),
            "The requested operation could not be completed"
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bone-desktop-app-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_layout() -> layout::Layout {
        layout::Layout {
            address: "127.0.0.1:17878".into(),
            selected: 2,
            tabs: vec![
                layout::TabState {
                    conversation_id: Some(7),
                    draft: "hello".into(),
                },
                layout::TabState {
                    conversation_id: None,
                    draft: "draft line one\n\nline three ✓ and trailing newline\n".into(),
                },
                layout::TabState {
                    conversation_id: Some(9001),
                    draft: String::new(),
                },
            ],
            preferences: layout::Preferences::default(),
            workspace: None,
            legacy_split: layout::LegacySplit::default(),
        }
    }

    #[test]
    fn open_restores_tabs_drafts_order_and_selection() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("layout.txt");
        layout::save(&path, &sample_layout()).unwrap();

        let app = DesktopApp::open(egui::Context::default(), false, Some(path.clone()));
        assert_eq!(app.address, "127.0.0.1:17878");
        assert_eq!(app.tabs.len(), 3);
        let ids: Vec<_> = app.tabs.iter().map(|t| t.conversation_id).collect();
        assert_eq!(ids, vec![Some(7), None, Some(9001)]);
        let drafts: Vec<_> = app.tabs.iter().map(|t| t.composer.as_str()).collect();
        assert_eq!(
            drafts,
            vec![
                "hello",
                "draft line one\n\nline three ✓ and trailing newline\n",
                ""
            ]
        );
        assert_eq!(app.selected, 2);
        // Restoring must not mark the freshly built layout dirty: the first
        // idle run must not rewrite what we just read.
        assert!(app.layout_dirty_since.is_none());
        assert!(app.sidebar_notice.is_empty());
        assert_eq!(app.layout_path, Some(path.clone()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_restores_display_preferences() {
        let dir = temp_dir("prefs");
        let mut layout = sample_layout();
        layout.preferences = layout::Preferences {
            tool_verbosity: layout::ToolVerbosity::Concise,
            zoom_percent: 125,
            sidebar_width: 300,
            sidebar_width_manual: true,
        };
        let path = dir.join("layout.txt");
        layout::save(&path, &layout).unwrap();

        let ctx = egui::Context::default();
        let app = DesktopApp::open(ctx.clone(), false, Some(path.clone()));
        assert_eq!(app.display, layout.preferences);
        // A saved v4 file has no split flag, so the workspace is a single pane.
        assert_eq!(app.workspace.window(0).unwrap().root.panes().len(), 1);
        // egui applies a pending zoom at the start of the next pass, so run
        // one headless pass before reading the factor back.
        ctx.begin_pass(egui::RawInput::default());
        let output = ctx.end_pass();
        output.drop_without_applying_deltas();
        assert!((ctx.zoom_factor() - 1.25).abs() < f32::EPSILON);
        // Restoring prefs must not mark the freshly built layout dirty.
        assert!(app.layout_dirty_since.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_migrates_a_legacy_split_file_into_two_panes() {
        // Byte-for-byte shape of the real on-disk v2 file: eight tabs, the
        // seventh selected, and the eighth shown in a right-hand pane. This
        // exercises the one-time split -> workspace migration.
        let dir = temp_dir("legacy-split");
        let path = dir.join("layout.txt");
        let mut bytes = String::from("bone-desktop-layout v2\n");
        bytes.push_str("address 11 127.0.0.1:1\n");
        bytes.push_str("selected 6\n");
        for id in [4676, 4679, 4682, 4685, 4686, 4687, 4689, 4692] {
            bytes.push_str(&format!("tab load {id}\n"));
        }
        bytes.push_str("display 100 1 7 349 1 676\n");
        std::fs::write(&path, bytes).unwrap();

        let app = DesktopApp::open(egui::Context::default(), false, Some(path.clone()));
        assert_eq!(app.tabs.len(), 8);
        assert_eq!(app.selected, 6);
        assert_eq!(app.display.zoom_percent, 100);
        assert_eq!(app.display.sidebar_width, 349);
        assert!(app.display.sidebar_width_manual);
        let panes = app.workspace.window(0).unwrap().root.panes();
        assert_eq!(panes.len(), 2);
        // The legacy split tab (index 7) is active in the second pane; the
        // selected tab (index 6) stays focused.
        assert_eq!(panes[1].active, Some(app.tabs[7].id));
        assert_eq!(app.workspace.active_tab(0), Some(app.tabs[6].id));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_migrates_a_legacy_file_without_split_into_one_pane() {
        let dir = temp_dir("legacy-nosplit");
        let path = dir.join("layout.txt");
        std::fs::write(
            &path,
            b"bone-desktop-layout v1\naddress 11 127.0.0.1:1\nselected 0\ntab load 5\n\
              display 100 0 0 480 1 380\n",
        )
        .unwrap();
        let app = DesktopApp::open(egui::Context::default(), false, Some(path.clone()));
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.workspace.window(0).unwrap().root.panes().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_clamps_selection_and_accepts_empty_layout() {
        let dir = temp_dir("clamp");
        let mut layout = sample_layout();
        layout.selected = 99; // Beyond the restored tab count.
        let path = dir.join("layout.txt");
        layout::save(&path, &layout).unwrap();
        let app = DesktopApp::open(egui::Context::default(), false, Some(path.clone()));
        assert_eq!(app.selected, app.tabs.len() - 1);

        // Every tab closed before exit: restore to the empty state.
        layout::save(&path, &layout::Layout::default()).unwrap();
        let app = DesktopApp::open(egui::Context::default(), false, Some(path.clone()));
        assert!(app.tabs.is_empty());
        assert!(app.tabs.get(app.selected).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_corrupt_layout_falls_back_to_new_tab_with_notice() {
        let dir = temp_dir("corrupt");
        let path = dir.join("layout.txt");
        std::fs::write(&path, b"bone-desktop-layout v1\nselected not-a-number\n").unwrap();
        let app = DesktopApp::open(egui::Context::default(), false, Some(path.clone()));
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].conversation_id, None);
        assert_eq!(app.selected, 0);
        assert!(
            app.sidebar_notice.contains("Could not restore layout"),
            "notice: {}",
            app.sidebar_notice
        );
        assert!(app.layout_dirty_since.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_without_layout_file_starts_fresh_new_tab() {
        let dir = temp_dir("missing");
        let path = dir.join("layout.txt"); // Never written.
        let app = DesktopApp::open(egui::Context::default(), false, Some(path.clone()));
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].conversation_id, None);
        assert_eq!(app.address, "127.0.0.1:7878");
        assert!(app.sidebar_notice.is_empty());
        assert!(app.layout_dirty_since.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn debounced_flush_writes_current_layout_once_elapsed() {
        let dir = temp_dir("flush");
        let path = dir.join("layout.txt");
        let ctx = egui::Context::default();
        let mut app = DesktopApp::open(ctx.clone(), false, Some(path.clone()));
        assert_eq!(app.tabs.len(), 1);
        assert!(!path.exists());

        // Edit the composer draft, then mark the layout dirty (as the UI does
        // on each typed character / selection change).
        app.tabs[0].composer = "pending draft ✓\nsecond line".into();
        app.note_layout_change(&ctx);
        assert!(app.layout_dirty_since.is_some());

        // Before the debounce elapses nothing is written yet.
        app.flush_layout(&ctx);
        assert!(!path.exists());
        assert!(app.layout_dirty_since.is_some());

        // Once the debounce elapses the flush writes the file and clears dirty.
        std::thread::sleep(Duration::from_millis(LAYOUT_SAVE_DEBOUNCE_MS + 100));
        app.flush_layout(&ctx);
        assert!(app.layout_dirty_since.is_none());
        assert_eq!(layout::load(&path).unwrap(), Some(app.current_layout()));
        let restored = layout::load(&path).unwrap().unwrap();
        assert_eq!(restored.tabs.len(), 1);
        assert_eq!(restored.tabs[0].draft, "pending draft ✓\nsecond line");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // A helper that yields a one-tab, non-demo app whose layout file does not
    // exist yet (fresh New tab), keeping daemon logs under a temp dir.
    fn fresh_app(ctx: &egui::Context, name: &str) -> (DesktopApp, PathBuf) {
        let dir = temp_dir(name);
        let app = DesktopApp::open(ctx.clone(), false, Some(dir.join("layout.txt")));
        assert_eq!(app.tabs.len(), 1);
        (app, dir)
    }

    #[test]
    fn pump_refused_on_loopback_with_unspawnable_bin_goes_stopped() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "pump-nobindir");
        app.daemon_bin = Some(dir.join("definitely-missing-daemon"));
        app.tabs[0].connect_failed_refused = true;
        app.pump_daemon(&ctx);
        match &app.daemon_phase {
            daemon::Phase::Stopped(message) => {
                assert!(
                    message.contains("Could not start the local daemon"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected Stopped, got {other:?}"),
        }
        assert!(app.daemon_pid.is_none());
        assert!(app.retry_at.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pump_remote_address_refusal_never_spawns() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "pump-remote");
        app.address = "10.0.0.5:8080".into();
        app.daemon_bin = Some(dir.join("definitely-missing-daemon")); // must stay unused
        app.tabs[0].connect_failed_refused = true;
        app.pump_daemon(&ctx);
        match &app.daemon_phase {
            daemon::Phase::Stopped(message) => {
                assert!(
                    message.contains("No daemon responding at 10.0.0.5:8080"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected Stopped, got {other:?}"),
        }
        assert!(app.daemon_pid.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pump_connected_tab_flips_phase_to_ready() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "pump-ready");
        app.daemon_phase = daemon::Phase::Starting { attempts: 5 };
        app.retry_at = Some(Instant::now());
        app.tabs[0].connected = true;
        app.pump_daemon(&ctx);
        assert_eq!(app.daemon_phase, daemon::Phase::Ready);
        assert!(app.retry_at.is_none());
        assert!(app.daemon_notice.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pump_starting_retry_tick_increments_and_reconnects() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "pump-retry");
        app.address = "127.0.0.1:17000".into(); // closed port: instant refusal
        app.daemon_phase = daemon::Phase::Starting { attempts: 2 };
        app.retry_at = Some(Instant::now() - Duration::from_millis(1)); // already due
        app.tabs[0].auto = true;
        app.pump_daemon(&ctx);
        assert_eq!(app.daemon_phase, daemon::Phase::Starting { attempts: 3 });
        assert!(app.retry_at.is_none());
        assert!(
            app.tabs[0].connecting,
            "reconnect_auto_tabs should have started a connect"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pump_exhausted_starting_rounds_goes_stopped() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "pump-exhaust");
        app.daemon_phase = daemon::Phase::Starting {
            attempts: daemon::MAX_DAEMON_RETRIES,
        };
        app.retry_at = Some(Instant::now() - Duration::from_millis(1));
        app.pump_daemon(&ctx);
        match &app.daemon_phase {
            daemon::Phase::Stopped(message) => {
                assert!(
                    message.contains("never accepted connections"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected Stopped, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pump_stopped_phase_ignores_new_refusals() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "pump-stopped");
        app.daemon_phase = daemon::Phase::Stopped("manual stop".into());
        app.daemon_notice = "manual stop".into();
        app.tabs[0].connect_failed_refused = true;
        app.pump_daemon(&ctx);
        assert_eq!(
            app.daemon_phase,
            daemon::Phase::Stopped("manual stop".into())
        );
        assert_eq!(app.daemon_notice, "manual stop");
        assert!(app.daemon_pid.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pump_is_a_noop_in_demo_mode() {
        let ctx = egui::Context::default();
        let dir = temp_dir("pump-demo");
        let mut app = DesktopApp::open(ctx.clone(), true, Some(dir.join("layout.txt")));
        app.tabs[0].connect_failed_refused = true;
        app.pump_daemon(&ctx);
        assert_eq!(app.daemon_phase, daemon::Phase::Probe);
        assert!(app.daemon_pid.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    pub(super) fn sample_meta(id: i64, title: &str) -> ConversationMeta {
        ConversationMeta {
            id,
            title: title.into(),
            full_title: title.into(),
            updated_at: "2026-09-07T12:30:00Z".into(),
            updated_at_local: "2026-09-07T08:30:00".into(),
            message_count: 4,
            provider: "desktop-fixture".into(),
            model: "mock".into(),
        }
    }

    #[test]
    fn review_requests_require_new_api_and_are_correlated() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "reviewreq");
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;
        app.tabs[0].connected = true;
        app.tabs[0].state.ready = true;
        app.tabs[0].host_api_version = 1;
        app.request_review();
        assert!(app.review.pending.is_none());
        assert!(rx.try_recv().is_err());
        app.tabs[0].host_api_version = 2;
        app.request_review();
        let (tab, id, _) = app.review.pending.unwrap();
        assert_eq!(tab, app.tabs[0].id);
        assert!(
            matches!(rx.try_recv(), Ok(Command::Send(RuntimeCommand::HostRequest { request_id, request: HostRequest::WorkspaceReview })) if request_id == id)
        );
        app.request_review();
        assert!(
            rx.try_recv().is_err(),
            "pending requests are not duplicated"
        );
        app.review.check_pending(false);
        assert!(app.review.pending.is_none());
        assert!(!app.review.notice.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conversations_request_goes_out_on_first_connected_tab_once() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "convreq");
        // No tab connected yet: nothing is sent.
        assert!(!app.request_conversations());
        assert!(app.conversations_request.is_none());
        app.tabs[0].connected = true;
        assert!(app.request_conversations());
        let pending = app.conversations_request;
        // While pending, neither another poll nor a direct call double-sends.
        assert!(!app.request_conversations());
        app.poll_conversations();
        assert_eq!(app.conversations_request, pending);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conversations_response_populates_picker_and_open_reuses_tab() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "convlist");
        app.tabs[0].connected = true;
        assert!(app.request_conversations());
        let (tab_id, request_id) = app.conversations_request.unwrap();
        assert_eq!(tab_id, app.tabs[0].id);
        // Simulate the daemon's correlated response arriving on that socket.
        let meta = sample_meta(7, "first prompt");
        app.tabs[0].host_responses.push((
            request_id,
            bone_protocol::HostResponse::Conversations(vec![meta.clone()]),
        ));
        app.drain_all(&ctx);
        assert_eq!(app.conversations, vec![meta.clone()]);
        assert!(app.conversations_loaded);
        assert!(app.conversations_request.is_none());

        // Opening a listed conversation with no matching tab loads it fresh.
        app.open_conversation(7, &ctx);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.tabs.last().unwrap().conversation_id, Some(7));
        // Opening it again reuses the existing tab.
        app.open_conversation(7, &ctx);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.selected, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conversations_request_abandoned_when_sending_tab_disconnects() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "convgone");
        app.tabs[0].connected = true;
        assert!(app.request_conversations());
        app.tabs[0].connected = false; // socket dropped before any response
        app.poll_conversations();
        assert!(app.conversations_request.is_none());
        assert!(!app.conversations_loaded);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn abandoned_mutation_reports_unconfirmed_notice() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "mutgone");
        app.tabs[0].connected = true;
        app.conversations = vec![sample_meta(7, "first prompt")];
        app.start_rename(7);
        app.rename_field = "new title".into();
        app.commit_rename();
        assert_eq!(app.mutation_target, Some(7));
        app.tabs[0].connected = false; // socket dropped before any response
        app.poll_conversations();
        assert!(app.conversations_request.is_none());
        assert!(app.mutation_target.is_none());
        assert!(
            app.sidebar_notice.contains("not confirmed"),
            "notice: {}",
            app.sidebar_notice
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conversations_error_response_latches_and_does_not_resend() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "converr");
        app.tabs[0].connected = true;
        assert!(app.request_conversations());
        let (_, request_id) = app.conversations_request.unwrap();
        app.tabs[0].host_responses.push((
            request_id,
            bone_protocol::HostResponse::Error {
                code: bone_protocol::HostErrorCode::Unavailable,
                message: "cannot open conversations.db".into(),
            },
        ));
        app.drain_all(&ctx);
        assert!(app.conversations_loaded, "error must latch like a list");
        assert!(app.conversations_request.is_none());
        assert!(app.sidebar_notice.contains("cannot open conversations.db"));
        // With the latch set, the poll must not re-issue the request (no
        // retry loop while the daemon stays broken).
        assert!(!app.request_conversations());
        app.poll_conversations();
        assert!(app.conversations_request.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conversations_response_survives_later_broadcast_in_same_batch() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "convclobber");
        app.tabs[0].connected = true;
        assert!(app.request_conversations());
        let (_, request_id) = app.conversations_request.unwrap();
        let meta = sample_meta(3, "survivor");
        // The picker's response arrives first, then an unrelated host
        // response from another client (e.g. a TUI stats request) lands on
        // the same socket in the same drain batch.
        app.tabs[0].host_responses.push((
            request_id,
            bone_protocol::HostResponse::Conversations(vec![meta.clone()]),
        ));
        app.tabs[0].host_responses.push((
            999_999,
            bone_protocol::HostResponse::Error {
                code: bone_protocol::HostErrorCode::Unavailable,
                message: "unrelated client's request failed".into(),
            },
        ));
        app.drain_all(&ctx);
        assert_eq!(app.conversations, vec![meta.clone()]);
        assert!(app.conversations_loaded);
        assert!(app.conversations_request.is_none());
        // The unrelated response must not be applied to our picker.
        assert!(app.sidebar_notice.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conversations_poll_is_a_noop_in_demo_mode() {
        let ctx = egui::Context::default();
        let dir = temp_dir("convdemo");
        let mut app = DesktopApp::open(ctx.clone(), true, Some(dir.join("layout.txt")));
        app.tabs[0].connected = true;
        app.poll_conversations();
        assert!(app.conversations_request.is_none());
        assert!(!app.conversations_loaded);
        assert!(app.conversations.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn sample_setup_snapshot(
        revision: u64,
        active: &str,
        providers: &[(&str, &str, bool)],
        needs_onboarding: bool,
    ) -> bone_protocol::SetupSnapshot {
        bone_protocol::SetupSnapshot {
            config_revision: revision,
            providers: providers
                .iter()
                .map(|(id, label, configured)| bone_protocol::ProviderChoice {
                    id: (*id).into(),
                    label: (*label).into(),
                    api_key_configured: *configured,
                })
                .collect(),
            active_provider: active.into(),
            init_exists: false,
            needs_onboarding,
            catalog: bone_protocol::CatalogSnapshot {
                revision: "0".into(),
                items: Vec::new(),
            },
        }
    }

    #[test]
    fn setup_request_goes_out_and_actionable_snapshot_opens_dialog() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "setupopen");
        app.tabs[0].connected = true;
        app.poll_setup();
        assert!(
            app.setup_request.is_some(),
            "Setup must be sent on the first connected tab"
        );
        let (_, request_id) = app.setup_request.unwrap();
        let snapshot = sample_setup_snapshot(5, "openai", &[("openai", "OpenAI", false)], true);
        app.tabs[0]
            .host_responses
            .push((request_id, bone_protocol::HostResponse::Setup(snapshot)));
        app.drain_all(&ctx);
        assert!(app.setup_request.is_none());
        assert!(
            app.show_setup,
            "an actionable snapshot must open the onboarding dialog"
        );
        assert!(app.setup_offered);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn setup_snapshot_request_is_abandoned_when_tab_disconnects() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "setupgone");
        app.tabs[0].connected = true;
        app.poll_setup();
        assert!(app.setup_request.is_some());
        app.tabs[0].connected = false;
        app.poll_setup();
        assert!(app.setup_request.is_none());
        // The machine must be re-armed so a reconnected tab can re-request.
        app.tabs[0].connected = true;
        app.poll_setup();
        assert!(
            app.setup_request.is_some(),
            "an aborted poll must re-issue the snapshot request"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn setup_apply_response_clears_cached_config_and_reports_message() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "setupapply");
        app.tabs[0].connected = true;
        app.config = Some(sample_config(1, "openai", &[("openai", "gpt-4")]));
        let tab_id = app.tabs[0].id;
        app.setup_request = Some((tab_id, 42));
        let catalog =
            sample_setup_snapshot(5, "openai", &[("openai", "OpenAI", true)], false).catalog;
        app.tabs[0].host_responses.push((
            42,
            bone_protocol::HostResponse::SetupApplied(bone_protocol::SetupApplyResult {
                config_revision: 6,
                catalog: bone_protocol::CatalogApplyResult {
                    snapshot: catalog,
                    results: Vec::new(),
                    changed: true,
                    extensions_reloaded: false,
                },
                restart_required: false,
                message: "Provider openai configured".into(),
            }),
        ));
        app.drain_all(&ctx);
        assert!(app.setup_request.is_none());
        assert!(
            app.config.is_none(),
            "a successful save must force a config refetch"
        );
        assert!(app.sidebar_notice.contains("Provider openai configured"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    pub(super) fn sample_config(
        revision: u64,
        active: &str,
        providers: &[(&str, &str)],
    ) -> ConfigSnapshot {
        ConfigSnapshot {
            revision,
            values: serde_json::json!({}),
            providers: providers
                .iter()
                .map(|(id, model)| bone_protocol::ProviderConfig {
                    id: (*id).into(),
                    label: (*id).into(),
                    base_url: "http://127.0.0.1:11434".into(),
                    model: (*model).into(),
                    endpoint: "/v1/chat/completions".into(),
                    handler: "openai_compat".into(),
                    context_window_tokens: None,
                    max_concurrency: None,
                    reasoning_effort: "medium".into(),
                    fast_mode: false,
                    supports_prompt_cache_key: false,
                    stream_usage: "auto".into(),
                    api_key_configured: true,
                })
                .collect(),
            active_provider: active.into(),
            disabled_tools: vec![],
            disabled_commands: vec![],
        }
    }

    fn sample_schema() -> ConfigSchema {
        ConfigSchema {
            pages: vec![bone_protocol::ConfigPage {
                namespace: "general".into(),
                title: "General".into(),
                fields: vec![bone_protocol::SettingDefinition {
                    path: "general.approval".into(),
                    key: "approval".into(),
                    label: "Approval mode".into(),
                    value_type: "enum".into(),
                    options: vec!["safe".into(), "danger".into()],
                    default: serde_json::json!("safe"),
                    value: None,
                    integer: None,
                    min: None,
                    max: None,
                    reload_behavior: "none".into(),
                }],
                pages: vec![],
            }],
        }
    }

    #[test]
    fn config_fetch_goes_out_on_first_connected_tab_once() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfgreq");
        // No tab connected yet: nothing is sent.
        assert!(!app.request_config());
        assert!(app.config_request.is_none());
        app.tabs[0].connected = true;
        assert!(app.request_config());
        let pending = app.config_request;
        // While pending, neither another poll nor a direct call double-sends.
        assert!(!app.request_config());
        app.poll_config();
        assert_eq!(app.config_request, pending);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_broadcast_populates_picker_and_switch_succeeds() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfgsnap");
        app.tabs[0].connected = true;
        assert!(app.request_config());
        // Simulate the daemon's broadcast snapshot arriving on that socket.
        app.tabs[0].config_snapshots.push((
            sample_schema(),
            sample_config(3, "local", &[("local", "qwen3"), ("openai", "gpt-5")]),
            false,
        ));
        app.drain_all(&ctx);
        assert!(app.config.is_some());
        app.tabs[0].state.snapshot.provider_id = "local".into();
        assert!(
            app.config_request.is_none(),
            "fetch resolved by the broadcast"
        );
        // Switching to the other provider is accepted while connected.
        assert!(app.switch_provider(None, "openai"));
        assert!(app.provider_notice.contains("Switching provider"));
        // Switching to the already-active provider is a no-op.
        assert!(!app.switch_provider(None, "local"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_cli_sets_address_and_opens_dialogs() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cli-open");
        app.apply_cli(cli::Cli {
            address: Some("127.0.0.1:9001".into()),
            open_setup: true,
            open_catalog: true,
            open_stats: true,
            ..Default::default()
        });
        assert_eq!(app.address, "127.0.0.1:9001");
        assert!(app.show_setup);
        assert!(app.show_catalog);
        assert!(app.show_stats);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_cli_overrides_switches_provider_then_sets_model() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cli-overrides");
        app.tabs[0].connected = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;
        app.apply_config_snapshot((
            sample_schema(),
            sample_config(1, "local", &[("local", "qwen3"), ("openai", "gpt-5")]),
            false,
        ));
        app.cli_provider = Some("openai".into());
        app.cli_model = Some("gpt-5.1".into());

        // Frame 1: switch the provider; the model waits until it is active.
        app.apply_cli_overrides();
        match rx.try_recv().expect("provider switch sent") {
            Command::Send(RuntimeCommand::SetActiveProvider { id, .. }) => {
                assert_eq!(id, "openai");
            }
            _other => panic!("expected SetActiveProvider"),
        }
        assert!(rx.try_recv().is_err(), "model must wait for the switch");
        assert!(app.cli_provider.is_some());

        // Daemon confirms the switch with a fresh snapshot: now set the model.
        app.apply_config_snapshot((
            sample_schema(),
            sample_config(2, "openai", &[("local", "qwen3"), ("openai", "gpt-5")]),
            false,
        ));
        // The runtime snapshot may still be one frame behind the config broadcast;
        // CLI ordering must still save the requested provider.
        app.tabs[0].state.snapshot.provider_id = "local".into();
        app.tabs[0].state.snapshot.provider_model = "qwen3".into();
        app.apply_cli_overrides();
        match rx.try_recv().expect("model upsert sent") {
            Command::Send(RuntimeCommand::UpsertProvider { provider, .. }) => {
                assert_eq!(provider.id, "openai");
                assert_eq!(provider.model, "gpt-5.1");
            }
            _other => panic!("expected UpsertProvider"),
        }
        assert!(app.cli_provider.is_none());
        assert!(app.cli_model.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_cli_overrides_reports_unknown_provider_once() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cli-unknown");
        app.tabs[0].connected = true;
        app.apply_config_snapshot((
            sample_schema(),
            sample_config(1, "local", &[("local", "qwen3")]),
            false,
        ));
        app.cli_provider = Some("nope".into());
        app.apply_cli_overrides();
        assert!(app.provider_notice.contains("Unknown provider `nope`"));
        assert!(app.cli_provider.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_rejection_clears_snapshot_and_shows_notice() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfgrej");
        app.tabs[0].connected = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;
        app.apply_config_snapshot((
            sample_schema(),
            sample_config(1, "local", &[("local", "m")]),
            false,
        ));
        assert!(app.set_approval_mode("danger"));
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::SetApprovalMode { ref mode })) if mode == "danger"
        ));
        app.tabs[0].state.approvals.push(state::Approval {
            id: 71,
            name: "shell".into(),
            summary: "run a command".into(),
            preview: None,
            blocked: None,
        });
        app.tabs[0]
            .config_rejections
            .push("revision mismatch".into());
        app.drain_all(&ctx);
        assert!(
            app.config.is_none(),
            "rejected mutation must force a refetch"
        );
        assert!(app.provider_notice.contains("revision mismatch"));
        assert!(
            !app.danger_mode_active,
            "a rejected Danger transition must restore confirmed Safe mode"
        );
        assert_eq!(app.tabs[0].state.approvals.len(), 1);
        assert_eq!(app.tabs[0].state.approvals[0].id, 71);
        // The drain's own poll already re-issued the fetch; no double-send.
        assert!(app.config_request.is_some());
        let commands: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            commands
                .iter()
                .any(|command| matches!(command, Command::Send(RuntimeCommand::GetConfig)))
        );
        assert!(!commands.iter().any(|command| matches!(
            command,
            Command::Send(RuntimeCommand::ApprovalReply { id: 71, .. })
        )));
        assert!(!app.request_config());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_dialog_set_and_reset_send_with_snapshot_revision() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfgset");
        app.tabs[0].connected = true;
        app.tabs[0].state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;
        app.apply_config_snapshot((
            sample_schema(),
            sample_config(9, "local", &[("local", "m")]),
            false,
        ));

        assert!(app.set_config_value("general.approval", serde_json::json!("danger")));
        match rx.try_recv().expect("set sent") {
            Command::Send(RuntimeCommand::SetConfigValue {
                path,
                value,
                expected_revision,
                request_id,
            }) => {
                assert_eq!(path, "general.approval");
                assert_eq!(value, serde_json::json!("danger"));
                assert_eq!(expected_revision, 9, "uses the snapshot revision");
                assert!(request_id.is_none());
            }
            _ => panic!("expected SetConfigValue"),
        }

        assert!(app.reset_config_value("general.approval"));
        match rx.try_recv().expect("reset sent") {
            Command::Send(RuntimeCommand::ResetConfigValue {
                path,
                expected_revision,
                ..
            }) => {
                assert_eq!(path, "general.approval");
                assert_eq!(expected_revision, 9);
            }
            _ => panic!("expected ResetConfigValue"),
        }

        // A disconnected app queues nothing and explains why.
        app.tabs[0].connected = false;
        assert!(!app.set_config_value("general.approval", serde_json::json!("safe")));
        assert!(app.config_notice.contains("No connected"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tool_and_command_enable_toggles_send_dedicated_commands() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfgtoggle");
        app.tabs[0].connected = true;
        app.tabs[0].state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;
        let mut config = sample_config(9, "local", &[("local", "m")]);
        config.disabled_tools = vec!["shell".into()];
        config.disabled_commands = vec!["/model".into()];
        app.apply_config_snapshot((sample_schema(), config, false));

        app.apply_config_ui_action(config_view::ConfigUiAction::SetEnabled {
            namespace: "tools".into(),
            name: "shell".into(),
            enabled: false,
        });
        match rx.try_recv().expect("tool toggle sent") {
            Command::Send(RuntimeCommand::SetToolEnabled {
                name,
                enabled,
                expected_revision,
                request_id,
            }) => {
                assert_eq!(name, "shell");
                assert!(!enabled);
                assert_eq!(expected_revision, 9);
                assert!(request_id.is_none());
            }
            _ => panic!("expected SetToolEnabled"),
        }
        assert!(app.config_notice.contains("Disabling shell"));

        app.apply_config_ui_action(config_view::ConfigUiAction::SetEnabled {
            namespace: "commands".into(),
            name: "/model".into(),
            enabled: true,
        });
        match rx.try_recv().expect("command toggle sent") {
            Command::Send(RuntimeCommand::SetCommandEnabled {
                name,
                enabled,
                expected_revision,
                ..
            }) => {
                assert_eq!(name, "/model");
                assert!(enabled);
                assert_eq!(expected_revision, 9);
            }
            _ => panic!("expected SetCommandEnabled"),
        }

        // A disconnected app queues nothing and explains why.
        app.tabs[0].connected = false;
        app.apply_config_ui_action(config_view::ConfigUiAction::SetEnabled {
            namespace: "tools".into(),
            name: "shell".into(),
            enabled: true,
        });
        assert!(app.config_notice.contains("No connected"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn approval_and_incognito_toggles_send_commands() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfgmode");
        app.tabs[0].connected = true;
        app.tabs[0].state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;
        let mut config = sample_config(9, "local", &[("local", "m")]);
        config.values = serde_json::json!({ "general": { "approval": "danger" } });
        app.apply_config_snapshot((sample_schema(), config, false));
        assert_eq!(app.approval_mode(), "danger");

        assert!(app.set_approval_mode("safe"));
        match rx.try_recv().expect("approval mode sent") {
            Command::Send(RuntimeCommand::SetApprovalMode { mode }) => {
                assert_eq!(mode, "safe");
            }
            _ => panic!("expected SetApprovalMode"),
        }
        assert!(app.config_notice.contains("safe"));

        assert!(app.set_incognito(true));
        match rx.try_recv().expect("incognito sent") {
            Command::Send(RuntimeCommand::SetIncognito { enabled }) => {
                assert!(enabled);
            }
            _ => panic!("expected SetIncognito"),
        }
        assert!(app.config_notice.contains("on"));

        // A disconnected app queues nothing and explains why.
        app.tabs[0].connected = false;
        assert!(!app.set_approval_mode("danger"));
        assert!(!app.set_incognito(false));
        assert!(app.config_notice.contains("Connect this task"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn danger_mode_approves_pending_calls_on_their_owning_tabs() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "danger-pending");
        app.add_tab(Intent::New, &ctx);
        for tab in &mut app.tabs {
            tab.connected = true;
            tab.state.ready = true;
        }
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = first_tx;
        let (second_tx, mut second_rx) = mpsc::unbounded_channel();
        app.tabs[1].commands = second_tx;
        app.tabs[0].state.approvals.extend([
            state::Approval {
                id: 41,
                name: "shell".into(),
                summary: "run a command".into(),
                preview: None,
                blocked: None,
            },
            state::Approval {
                id: 42,
                name: "shell".into(),
                summary: "blocked command".into(),
                preview: None,
                blocked: Some("blocked by hook".into()),
            },
        ]);
        app.tabs[1].state.approvals.push(state::Approval {
            id: 51,
            name: "edit_file".into(),
            summary: "edit a file".into(),
            preview: Some("diff".into()),
            blocked: None,
        });

        assert!(app.set_approval_mode("danger"));
        assert!(matches!(
            first_rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::SetApprovalMode { ref mode })) if mode == "danger"
        ));
        assert!(matches!(
            first_rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::ApprovalReply {
                id: 41,
                outcome: CallOutcome::Approve,
            }))
        ));
        assert!(first_rx.try_recv().is_err());
        assert!(matches!(
            second_rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::ApprovalReply {
                id: 51,
                outcome: CallOutcome::Approve,
            }))
        ));
        assert!(second_rx.try_recv().is_err());
        assert_eq!(app.tabs[0].state.approvals.len(), 1);
        assert_eq!(app.tabs[0].state.approvals[0].id, 42);
        assert!(app.tabs[1].state.approvals.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn danger_transition_approves_stale_safe_request_until_safe_snapshot() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "danger-transition");
        app.add_tab(Intent::New, &ctx);
        for tab in &mut app.tabs {
            tab.connected = true;
            tab.state.ready = true;
        }
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = first_tx;
        let (second_tx, mut second_rx) = mpsc::unbounded_channel();
        app.tabs[1].commands = second_tx;
        let mut safe = sample_config(9, "local", &[("local", "m")]);
        safe.values = serde_json::json!({ "general": { "approval": "safe" } });
        app.apply_config_snapshot((sample_schema(), safe.clone(), false));

        assert!(app.set_approval_mode("danger"));
        assert!(matches!(
            first_rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::SetApprovalMode { ref mode })) if mode == "danger"
        ));

        // This request was evaluated on another connection before the daemon
        // processed SetApprovalMode, so its policy bit still says Safe.
        app.tabs[1].handle_event(Event::Runtime(RuntimeEvent::ApprovalRequest {
            id: 61,
            call_id: "call-stale-safe".into(),
            name: "shell".into(),
            summary: "run a command".into(),
            arguments: serde_json::json!({"command": "echo ok"}),
            blocked: None,
            auto_allows: false,
            preview: None,
        }));
        app.drain_all(&ctx);
        assert!(matches!(
            second_rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::ApprovalReply {
                id: 61,
                outcome: CallOutcome::Approve,
            }))
        ));
        assert!(app.tabs[1].state.approvals.is_empty());

        // An authoritative Safe update in the same drain must end the local
        // Danger transition before newly pending calls are resolved.
        safe.revision = 10;
        app.tabs[0]
            .config_snapshots
            .push((sample_schema(), safe, false));
        app.tabs[1].handle_event(Event::Runtime(RuntimeEvent::ApprovalRequest {
            id: 62,
            call_id: "call-safe".into(),
            name: "shell".into(),
            summary: "run another command".into(),
            arguments: serde_json::json!({"command": "echo prompt"}),
            blocked: None,
            auto_allows: false,
            preview: None,
        }));
        app.drain_all(&ctx);
        assert!(second_rx.try_recv().is_err());
        assert_eq!(app.tabs[1].state.approvals.len(), 1);
        assert_eq!(app.tabs[1].state.approvals[0].id, 62);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn steer_composer_sends_mid_turn_text_and_clears_input() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "steer");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        tab.state.busy = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        tab.composer = "  change direction  ".into();
        tab.attachments.clear();

        assert!(tab.can_steer());
        tab.steer_composer();
        match rx.try_recv().expect("steer sent") {
            Command::Send(RuntimeCommand::Steer { text }) => {
                assert_eq!(text, "change direction");
            }
            _ => panic!("expected Steer"),
        }
        assert!(tab.composer.is_empty());
        assert_eq!(tab.state.status, "Steering…");
        // The steered text is recalled via history like a normal send.
        assert_eq!(
            tab.history.last().map(String::as_str),
            Some("change direction")
        );

        // Idle or empty composer cannot steer.
        tab.state.busy = false;
        tab.composer = "queued text".into();
        assert!(!tab.can_steer());
        tab.steer_composer();
        assert!(rx.try_recv().is_err());
        tab.state.busy = true;
        tab.composer.clear();
        assert!(!tab.can_steer());
        tab.steer_composer();
        assert!(rx.try_recv().is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn composer_synthetic_events_cover_stop_editor_steer_queue_and_escape() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "composer-events");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        tab.state.busy = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        let editor_id = egui::Id::new((tab.id, "composer-editor"));
        let frame = |tab: &mut Tab, event: egui::Event| {
            ctx.memory_mut(|memory| memory.request_focus(editor_id));
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 500.0),
                )),
                ..Default::default()
            };
            input.events.push(event);
            ctx.run_ui(input, |ui| {
                tab.composer_panel(ui, None, false, true);
            })
            .textures_delta
            .clear();
        };

        let click = |tab: &mut Tab, position: egui::Pos2| {
            ctx.memory_mut(|memory| memory.request_focus(editor_id));
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 500.0),
                )),
                ..Default::default()
            };
            input.events.extend([
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            ctx.run_ui(input, |ui| {
                tab.composer_panel(ui, None, false, true);
            })
            .textures_delta
            .clear();
        };

        tab.composer = "stop by button".into();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 500.0),
                )),
                ..Default::default()
            },
            |ui| {
                tab.composer_panel(ui, None, false, true);
            },
        );
        let stop_position = output.shapes.iter().find_map(|shape| {
            if let egui::epaint::Shape::Text(text) = &shape.shape {
                (text.galley.text() == "Stop").then(|| text.pos + text.galley.size() * 0.5)
            } else {
                None
            }
        });
        output.textures_delta.clear();
        click(tab, stop_position.expect("Stop is visible"));
        assert!(
            matches!(rx.try_recv(), Ok(Command::Send(RuntimeCommand::Cancel))),
            "synthetic pointer input activates Stop"
        );
        assert_eq!(tab.state.status, "Cancelling…");
        tab.state.busy = true;

        tab.composer = "stop this turn".into();
        frame(tab, egui::Event::Copy);
        assert!(rx.try_recv().is_err(), "Copy must not stop work");

        tab.state.busy = false;
        tab.composer = "open editor".into();
        frame(tab, egui::Event::Cut);
        assert!(
            !tab.show_editor,
            "Cut must retain standard text editing behavior"
        );

        tab.show_editor = false;
        tab.state.busy = true;
        tab.composer = "steer now".into();
        frame(
            tab,
            egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::ALT,
            },
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::Steer { ref text })) if text == "steer now"
        ));
        assert!(tab.composer.is_empty());

        tab.queue.extend(["one".into(), "two".into()]);
        tab.composer.clear();
        frame(
            tab,
            egui::Event::Key {
                key: egui::Key::D,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            },
        );
        assert!(tab.queue.is_empty(), "Ctrl/Cmd+D clears queued prompts");

        tab.composer = "discard me".into();
        frame(
            tab,
            egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        );
        assert_eq!(tab.composer, "discard me", "Escape preserves the draft");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn composer_return_key_bindings_send_steer_and_queue() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "composer-return");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        let editor_id = egui::Id::new((tab.id, "composer-editor"));
        let press = |tab: &mut Tab, modifiers: egui::Modifiers| {
            ctx.memory_mut(|memory| memory.request_focus(editor_id));
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 500.0),
                )),
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                tab.composer_panel(ui, None, false, true);
            })
            .textures_delta
            .clear();
        };

        // Idle + Enter sends the prompt verbatim: the composer's TextEdit has no
        // return key, so Enter must not insert a newline into the submitted text.
        tab.state.busy = false;
        tab.composer = "hello".into();
        press(tab, egui::Modifiers::NONE);
        assert!(
            matches!(
                rx.try_recv(),
                Ok(Command::Send(RuntimeCommand::SubmitPrompt { ref text, .. })) if text == "hello"
            ),
            "idle Enter sends the prompt without a trailing newline"
        );
        assert!(tab.composer.is_empty());
        assert!(tab.queue.is_empty());

        // Busy + Enter queues instead of sending.
        tab.state.busy = true;
        tab.composer = "queued".into();
        press(tab, egui::Modifiers::NONE);
        assert!(rx.try_recv().is_err(), "busy Enter must not send a command");
        assert_eq!(tab.queue.len(), 1);
        assert_eq!(tab.queue[0], "queued");
        assert!(tab.composer.is_empty());

        // Busy + Shift+Enter also queues.
        tab.composer = "shifted".into();
        press(tab, egui::Modifiers::SHIFT);
        assert!(rx.try_recv().is_err());
        assert_eq!(tab.queue.len(), 2);
        assert_eq!(tab.queue[1], "shifted");

        // Busy + Ctrl+Enter steers the running turn.
        tab.composer = "steer me".into();
        press(tab, egui::Modifiers::COMMAND);
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::Steer { ref text })) if text == "steer me"
        ));
        assert!(tab.composer.is_empty());
        assert_eq!(tab.queue.len(), 2, "steer must not enqueue");

        // Idle + Ctrl+Enter falls back to sending.
        tab.state.busy = false;
        tab.composer = "send via ctrl".into();
        press(tab, egui::Modifiers::COMMAND);
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::SubmitPrompt { ref text, .. })) if text == "send via ctrl"
        ));
        assert!(tab.composer.is_empty());
        assert_eq!(
            tab.queue.len(),
            2,
            "an idle send leaves the queue untouched"
        );

        // Idle + Shift+Enter falls back to sending.
        tab.state.busy = false;
        tab.composer = "send via shift".into();
        press(tab, egui::Modifiers::SHIFT);
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::SubmitPrompt { ref text, .. })) if text == "send via shift"
        ));
        assert!(tab.composer.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn approval_keyboard_nav_selects_preview_and_confirms() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "approvalnav");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        tab.state.approvals.push(state::Approval {
            id: 42,
            name: "run_shell".into(),
            summary: "Run `ls -la`".into(),
            preview: Some("ls -la".into()),
            blocked: None,
        });
        tab.sync_approval_nav();
        assert_eq!(tab.approval_selected, 0);
        assert!(!tab.approval_peek);

        fn press(key: egui::Key) -> egui::RawInput {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            input
        }
        let drive = |tab: &mut Tab, key: egui::Key| {
            let mut reply = None;
            let mut output = ctx.run_ui(press(key), |ui| {
                reply = tab.handle_approval_keys(ui);
            });
            output.textures_delta.clear();
            reply
        };

        // Down selects Deny; Up clamps back to Approve.
        assert!(drive(tab, egui::Key::ArrowDown).is_none());
        assert_eq!(tab.approval_selected, 1);
        assert!(drive(tab, egui::Key::ArrowUp).is_none());
        assert_eq!(tab.approval_selected, 0);
        // PageDown jumps to the last option (Deny), PageUp back to the first.
        assert!(drive(tab, egui::Key::PageDown).is_none());
        assert_eq!(tab.approval_selected, 1);
        assert!(drive(tab, egui::Key::PageUp).is_none());
        assert_eq!(tab.approval_selected, 0);
        // `p` toggles the command preview while one exists.
        assert!(drive(tab, egui::Key::P).is_none());
        assert!(tab.approval_peek);
        assert!(drive(tab, egui::Key::P).is_none());
        assert!(!tab.approval_peek);
        // Enter confirms the selected option (Deny here) and returns the reply.
        assert!(drive(tab, egui::Key::ArrowDown).is_none());
        assert_eq!(
            drive(tab, egui::Key::Enter),
            Some((42, CallOutcome::Denied))
        );
        assert!(
            rx.try_recv().is_err(),
            "handle_approval_keys returns the reply; dispatch is the caller's job"
        );

        // Approve is unreachable while blocked, so Enter does nothing.
        tab.state.approvals[0].blocked = Some("blocked by policy".into());
        tab.approval_selected = 0;
        assert!(drive(tab, egui::Key::Enter).is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn key_request_capture_sends_key_reply() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "keycapture");
        app.tabs[0].connected = true;
        app.tabs[0].state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;
        // Approval and key IDs overlap; approval must not suppress menu input.
        app.tabs[0].state.answered(77);
        app.tabs[0]
            .state
            .reduce(bone_protocol::RuntimeEvent::KeyRequest { id: 77 });

        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key {
            key: egui::Key::ArrowUp,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        ctx.run_ui(input, |ui| app.key_capture_dialog(ui.ctx()))
            .textures_delta
            .clear();

        match rx.try_recv().expect("key reply sent") {
            Command::Send(RuntimeCommand::KeyReply { id, key }) => {
                assert_eq!(id, 77);
                assert_eq!(key.code, "Up");
            }
            _ => panic!("expected KeyReply"),
        }
        assert!(app.tabs[0].state.pending_key.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn generic_interactive_menu_sequences_forward_select_multi_text_and_cancel_keys() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "interactive-menu");
        app.tabs[0].connected = true;
        app.tabs[0].state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;

        let send = |app: &mut DesktopApp, id: u64, key: egui::Key| {
            app.tabs[0].state.pending_key = Some(id);
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            ctx.run_ui(input, |ui| app.key_capture_dialog(ui.ctx()))
                .textures_delta
                .clear();
        };

        // ui.menu.select: Down, Enter.
        send(&mut app, 1, egui::Key::ArrowDown);
        send(&mut app, 2, egui::Key::Enter);
        // ui.menu.multi_select: Space, Down, Space, Enter.
        send(&mut app, 3, egui::Key::Space);
        send(&mut app, 4, egui::Key::ArrowDown);
        send(&mut app, 5, egui::Key::Space);
        send(&mut app, 6, egui::Key::Enter);
        // ui.menu.text_input: printable text then Enter.
        send(&mut app, 7, egui::Key::A);
        send(&mut app, 8, egui::Key::Enter);
        // Esc is the shared cancel path for every menu kind.
        send(&mut app, 9, egui::Key::Escape);

        let mut replies = Vec::new();
        while let Ok(Command::Send(RuntimeCommand::KeyReply { id, key })) = rx.try_recv() {
            replies.push((id, key));
        }
        assert_eq!(replies.len(), 9);
        assert_eq!(replies[0].1.code, "Down");
        assert_eq!(replies[1].1.code, "Enter");
        assert_eq!(replies[2].1.char.as_deref(), Some(" "));
        assert_eq!(replies[6].1.char.as_deref(), Some("a"));
        assert_eq!(replies[8].1.code, "Esc");
        assert!(app.tabs[0].state.pending_key.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn key_capture_ignores_modifier_press_for_chord() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "keycapturemod");
        app.tabs[0].connected = true;
        app.tabs[0].state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;
        app.tabs[0].state.pending_key = Some(78);

        let mut input = egui::RawInput::default();
        // A chord arrives as the modifier's own key event first, then the key.
        input.events.push(egui::Event::Key {
            key: egui::Key::ControlLeft,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        });
        input.events.push(egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        });
        ctx.run_ui(input, |ui| app.key_capture_dialog(ui.ctx()))
            .textures_delta
            .clear();

        match rx.try_recv().expect("key reply sent") {
            Command::Send(RuntimeCommand::KeyReply { id, key }) => {
                assert_eq!(id, 78);
                assert_eq!(key.code, "Char");
                assert_eq!(key.char.as_deref(), Some("c"));
                assert!(key.ctrl);
            }
            _ => panic!("expected KeyReply"),
        }
        assert!(app.tabs[0].state.pending_key.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refresh_activity_requests_processes_and_jobs() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "activityrefresh");
        app.tabs[0].connected = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;

        app.refresh_activity();

        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::GetProcesses))
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(Command::Send(RuntimeCommand::GetJobs))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refresh_activity_without_connection_is_noop() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "activitynoop");
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;

        app.refresh_activity();

        assert!(rx.try_recv().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn activity_dialog_renders_snapshots() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "activityrender");
        app.show_activity = true;
        app.tabs[0].connected = true;
        app.tabs[0].state.processes = vec![bone_protocol::ProcessSnapshot {
            id: "p1".into(),
            command: "cargo test".into(),
            owner: "conversation".into(),
            running: true,
            state: bone_protocol::ProcessState::Running,
            started_at: 1_000,
            finished_at: None,
            stdout: "running 5 tests".into(),
            stderr: String::new(),
            exit_code: None,
            signal: None,
            error: None,
        }];
        app.tabs[0].state.jobs = vec![bone_protocol::JobSnapshot {
            id: "j1".into(),
            agent: "explore".into(),
            task: "find the thing".into(),
            title: "Locate code".into(),
            status: bone_protocol::JobStatus::Running,
            started_at: 1_000,
            token_sent: 1_200,
            token_received: 3_400,
            provider: "anthropic".into(),
            activity: Some("reading files".into()),
            events: Vec::new(),
        }];

        ctx.run_ui(egui::RawInput::default(), |ui| {
            app.activity_dialog(ui.ctx())
        })
        .textures_delta
        .clear();

        assert!(app.show_activity);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn activity_stays_bound_to_its_origin_tab() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "activity-origin");
        app.tabs[0].connected = true;
        let first_id = app.tabs[0].id;
        app.add_tab(Intent::New, &ctx);
        app.tabs[1].connected = true;
        let second_id = app.tabs[1].id;

        // Opening from the focused conversation chooses that conversation.
        app.selected = 1;
        assert_eq!(app.activity_tab_index(), Some(1));
        // Once opened, switching focus must not retarget the dialog.
        app.activity_tab = Some(first_id);
        app.selected = 1;
        assert_eq!(app.activity_tab_index(), Some(0));
        assert_ne!(first_id, second_id);
        // A disconnected origin must not silently fall back to another tab.
        app.tabs[0].connected = false;
        assert_eq!(app.activity_tab_index(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn process_and_job_viewers_render_and_self_close() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "viewerrender");
        app.tabs[0].connected = true;
        app.tabs[0].state.processes = vec![bone_protocol::ProcessSnapshot {
            id: "p1".into(),
            command: "cargo test".into(),
            owner: "conversation".into(),
            running: true,
            state: bone_protocol::ProcessState::Running,
            started_at: 1_000,
            finished_at: None,
            stdout: "running 5 tests".into(),
            stderr: String::new(),
            exit_code: None,
            signal: None,
            error: None,
        }];
        app.tabs[0].state.jobs = vec![bone_protocol::JobSnapshot {
            id: "j1".into(),
            agent: "explore".into(),
            task: "find the thing".into(),
            title: "Locate code".into(),
            status: bone_protocol::JobStatus::Running,
            started_at: 1_000,
            token_sent: 1_200,
            token_received: 3_400,
            provider: "anthropic".into(),
            activity: Some("reading files".into()),
            events: Vec::new(),
        }];

        // Open both viewers for tracked items and render a frame: they stay open.
        app.process_view = Some(activity::ProcessViewer::new(app.tabs[0].id, "p1"));
        app.job_view = Some(activity::JobViewer::new(app.tabs[0].id, "j1"));
        ctx.run_ui(egui::RawInput::default(), |ui| {
            app.process_view_dialog(ui.ctx());
            app.job_view_dialog(ui.ctx());
        })
        .textures_delta
        .clear();
        assert!(app.process_view.is_some());
        assert!(app.job_view.is_some());

        // A viewer whose item left the snapshot closes itself.
        app.process_view = Some(activity::ProcessViewer::new(app.tabs[0].id, "gone"));
        app.job_view = Some(activity::JobViewer::new(app.tabs[0].id, "gone"));
        ctx.run_ui(egui::RawInput::default(), |ui| {
            app.process_view_dialog(ui.ctx());
            app.job_view_dialog(ui.ctx());
        })
        .textures_delta
        .clear();
        assert!(app.process_view.is_none());
        assert!(app.job_view.is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pane_keys_cycle_and_scroll_floats() {
        use bone_protocol::{Component, FloatRect, PaneLineSpec};
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "panekeys");
        let float = |id: &str| Component::Float {
            presentation: bone_protocol::PanePresentation::Overlay,
            id: id.into(),
            title: id.into(),
            lines: vec![PaneLineSpec::Plain("hi".into())],
            rect: FloatRect {
                anchor: Default::default(),
                width: 10,
                height: 3,
                col: 0,
                row: 0,
            },
            z: 0,
            border: true,
            scroll: 0,
        };
        app.tabs[0].state.view.components = vec![float("a"), float("b")];
        let tab_id = app.tabs[0].id;
        let drive = |app: &mut DesktopApp, key: egui::Key| {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            ctx.run_ui(input, |ui| app.handle_pane_keys(ui))
                .textures_delta
                .clear();
        };
        assert!(app.panes_visible);
        // Tab focuses the first float, then cycles (wrapping).
        drive(&mut app, egui::Key::Tab);
        assert_eq!(app.active_pane, Some((tab_id, "a".into())));
        drive(&mut app, egui::Key::Tab);
        assert_eq!(app.active_pane, Some((tab_id, "b".into())));
        drive(&mut app, egui::Key::Tab);
        assert_eq!(app.active_pane, Some((tab_id, "a".into())));
        // PageDown scrolls the focused float by a page; PageUp clamps at the top.
        drive(&mut app, egui::Key::PageDown);
        assert_eq!(app.pane_scroll.get(&(tab_id, "a".into())).copied(), Some(5));
        drive(&mut app, egui::Key::PageUp);
        assert_eq!(app.pane_scroll.get(&(tab_id, "a".into())).copied(), Some(0));
        // Hidden panes never steal keys.
        app.panes_visible = false;
        drive(&mut app, egui::Key::Tab);
        assert_eq!(app.active_pane, Some((tab_id, "a".into())));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn activity_keys_navigate_select_and_open() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "activitykeys");
        app.show_activity = true;
        let process_ids = vec![("p1".to_string(), true), ("p2".to_string(), true)];
        let job_ids = vec![("j1".to_string(), true)];
        let drive = |app: &mut DesktopApp, key: egui::Key| -> Vec<activity::ActivityAction> {
            let mut input = egui::RawInput::default();
            input.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
            let mut actions = Vec::new();
            ctx.run_ui(input, |ui| {
                actions = app.handle_activity_keys(ui.ctx(), &process_ids, &job_ids);
            })
            .textures_delta
            .clear();
            actions
        };
        assert_eq!(app.activity_selected, 0);
        // Down moves selection across the flat process-then-job list.
        drive(&mut app, egui::Key::ArrowDown);
        assert_eq!(app.activity_selected, 1);
        // End jumps to the last row (the job).
        drive(&mut app, egui::Key::End);
        assert_eq!(app.activity_selected, 2);
        assert_eq!(
            drive(&mut app, egui::Key::Enter),
            vec![activity::ActivityAction::OpenJob("j1".into())]
        );
        // Home returns to the first process; Enter opens its viewer.
        drive(&mut app, egui::Key::Home);
        assert_eq!(app.activity_selected, 0);
        assert_eq!(
            drive(&mut app, egui::Key::Enter),
            vec![activity::ActivityAction::OpenProcess("p1".into())]
        );
        // `k` cancels the selected running process.
        assert_eq!(
            drive(&mut app, egui::Key::K),
            vec![activity::ActivityAction::CancelProcess("p1".into())]
        );
        // ArrowUp clamps at the top and a non-running row ignores `k`.
        drive(&mut app, egui::Key::ArrowUp);
        assert_eq!(app.activity_selected, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn sample_stats() -> UsageStatsSnapshot {
        UsageStatsSnapshot {
            started_at: None,
            ended_at: None,
            total: bone_protocol::UsageSummary::default(),
            by_model_today: Vec::new(),
            by_model_7d: Vec::new(),
            by_model_4w: Vec::new(),
            by_model_all: Vec::new(),
            daily: Vec::new(),
            weekly: Vec::new(),
            monthly: Vec::new(),
            all_time: Vec::new(),
            yearly: Vec::new(),
            hourly_today: Vec::new(),
            hourly_7d: Vec::new(),
            hourly_4w: Vec::new(),
            hourly_all: Vec::new(),
            daily_activity: Vec::new(),
        }
    }

    fn sample_catalog(revision: &str) -> CatalogSnapshot {
        CatalogSnapshot {
            revision: revision.into(),
            items: vec![bone_protocol::CatalogItem {
                name: "demo".into(),
                kind: "tool".into(),
                description: "demo tool".into(),
                installed: false,
                update_available: false,
                ..bone_protocol::CatalogItem::default()
            }],
        }
    }

    #[test]
    fn refresh_stats_sends_stats_request() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "statsrefresh");
        app.tabs[0].connected = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;

        assert!(app.refresh_stats());
        match rx.try_recv().expect("stats request sent") {
            Command::Send(RuntimeCommand::HostRequest { request, .. }) => {
                assert!(matches!(request, HostRequest::Stats { range: None }));
            }
            _ => panic!("expected HostRequest::Stats"),
        }
        // A second call while one is in flight is a no-op.
        assert!(!app.refresh_stats());
        assert!(rx.try_recv().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refresh_stats_without_connection_is_noop() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "statsnoop");
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;

        assert!(!app.refresh_stats());
        assert!(rx.try_recv().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn request_catalog_sends_catalog_request() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "catalogrefresh");
        app.tabs[0].connected = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;

        assert!(app.request_catalog(true));
        match rx.try_recv().expect("catalog request sent") {
            Command::Send(RuntimeCommand::HostRequest { request, .. }) => {
                assert!(matches!(request, HostRequest::Catalog { refresh: true }));
            }
            _ => panic!("expected HostRequest::Catalog"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn buffered_host_responses_win_over_expired_deadlines() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "bufferedhost");
        app.tabs[0].connected = true;
        let tab_id = app.tabs[0].id;
        let stale = Instant::now() - (HOST_REQUEST_TIMEOUT + Duration::from_secs(1));
        app.conversations_request = Some((tab_id, 6));
        app.conversations_request_at = Some(stale);
        app.stats_request = Some((tab_id, 7));
        app.stats_request_at = Some(stale);
        app.catalog_request = Some((tab_id, 8));
        app.catalog_request_at = Some(stale);
        let conversations = vec![sample_meta(42, "saved task")];
        app.tabs[0].host_responses.extend([
            (6, HostResponse::Conversations(conversations.clone())),
            (7, HostResponse::Stats(Box::new(sample_stats()))),
            (8, HostResponse::Catalog(sample_catalog("buffered"))),
        ]);

        app.drain_all(&ctx);

        assert_eq!(app.conversations, conversations);
        assert!(app.conversations_loaded);
        assert!(app.conversations_request.is_none());
        assert!(app.conversations_request_at.is_none());
        assert!(app.sidebar_notice.is_empty());
        assert!(app.stats.is_some());
        assert!(app.stats_request.is_none());
        assert!(app.stats_retry_at.is_none());
        assert!(app.stats_notice.is_empty());
        assert_eq!(app.catalog.as_ref().unwrap().revision, "buffered");
        assert!(app.catalog_request.is_none());
        assert!(app.catalog_retry_at.is_none());
        assert!(app.catalog_notice.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conversation_timeout_and_successful_retry_clear_pending_state() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "conversationtimeout");
        app.tabs[0].connected = true;
        assert!(
            app.request_conversation_mutation(HostRequest::ConversationRename {
                id: 42,
                title: "test title".into(),
                limit: 0,
            })
        );
        assert!(
            app.conversations_request_at.is_some(),
            "mutations must also time out"
        );
        app.conversations_request_at =
            Some(Instant::now() - (HOST_REQUEST_TIMEOUT + Duration::from_secs(1)));
        app.drain_all(&ctx);
        assert!(app.conversations_request.is_none());
        assert!(app.conversations_request_at.is_none());
        assert!(app.mutation_target.is_none());
        assert!(
            app.conversations_loaded,
            "do not retry mutations automatically"
        );
        assert!(app.sidebar_notice.contains("update was not confirmed"));

        app.conversations_loaded = false;
        assert!(app.request_conversations());
        app.conversations_request_at =
            Some(Instant::now() - (HOST_REQUEST_TIMEOUT + Duration::from_secs(1)));
        app.drain_all(&ctx);
        assert!(app.conversations_request.is_none());
        assert!(app.sidebar_notice.contains("No response from the daemon"));

        app.conversations_loaded = false;
        assert!(app.request_conversations());
        let (_, request_id) = app.conversations_request.unwrap();
        app.tabs[0]
            .host_responses
            .push((request_id, HostResponse::Conversations(vec![])));
        app.drain_all(&ctx);
        assert!(
            app.sidebar_notice.is_empty(),
            "successful recovery clears the error"
        );
        assert!(app.conversations_loaded);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn host_request_timeout_surfaces_notice_and_throttles_retry() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "hosttimeout");
        app.tabs[0].connected = true;
        let tab_id = app.tabs[0].id;
        // A request sent long ago that the daemon never answered (wedged socket).
        let stale = Instant::now() - (HOST_REQUEST_TIMEOUT + Duration::from_secs(1));
        app.stats_request = Some((tab_id, 7));
        app.stats_request_at = Some(stale);
        app.catalog_request = Some((tab_id, 8));
        app.catalog_request_at = Some(stale);

        app.drain_all(&ctx);

        // Both in-flight slots are abandoned, an error notice replaces the
        // perpetual spinner, and auto-retry is throttled so it stays visible.
        assert!(app.stats_request.is_none());
        assert!(app.stats_request_at.is_none());
        assert!(app.stats_retry_at.is_some_and(|at| at > Instant::now()));
        assert!(app.stats_notice.contains("No response from the daemon"));
        assert!(app.catalog_request.is_none());
        assert!(app.catalog_request_at.is_none());
        assert!(app.catalog_retry_at.is_some_and(|at| at > Instant::now()));
        assert!(app.catalog_notice.contains("No response from the daemon"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_catalog_actions_sends_catalog_apply_with_revision() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "catalogapply");
        app.tabs[0].connected = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx;
        app.catalog = Some(sample_catalog("rev-9"));

        assert!(app.apply_catalog_actions(vec![CatalogAction {
            name: "demo".into(),
            action: CatalogActionKind::Install,
        }]));
        match rx.try_recv().expect("catalog apply sent") {
            Command::Send(RuntimeCommand::HostRequest { request, .. }) => match request {
                HostRequest::CatalogApply {
                    expected_revision,
                    actions,
                } => {
                    assert_eq!(expected_revision, "rev-9");
                    assert_eq!(actions.len(), 1);
                    assert_eq!(actions[0].name, "demo");
                    assert_eq!(actions[0].action, CatalogActionKind::Install);
                }
                _ => panic!("expected HostRequest::CatalogApply"),
            },
            _ => panic!("expected HostRequest"),
        }
        // Without a loaded snapshot there is nothing to apply against.
        app.catalog_request = None;
        app.catalog = None;
        assert!(!app.apply_catalog_actions(vec![CatalogAction {
            name: "demo".into(),
            action: CatalogActionKind::Remove,
        }]));
        assert!(app.catalog_notice.contains("still loading"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_stats_response_stores_snapshot_and_error() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "statsresp");

        app.apply_stats_response(HostResponse::Stats(Box::new(sample_stats())));
        assert!(app.stats.is_some());
        assert!(app.stats_notice.is_empty());

        app.apply_stats_response(HostResponse::Error {
            code: bone_protocol::HostErrorCode::Unavailable,
            message: "down".into(),
        });
        assert!(app.stats_notice.contains("down"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_catalog_response_stores_snapshot_and_applied() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "catalogresp");

        app.apply_catalog_response(HostResponse::Catalog(sample_catalog("r1")));
        assert_eq!(app.catalog.as_ref().unwrap().revision, "r1");
        assert!(app.catalog_notice.is_empty());

        app.apply_catalog_response(HostResponse::CatalogApplied(
            bone_protocol::CatalogApplyResult {
                snapshot: sample_catalog("r2"),
                results: vec![bone_protocol::CatalogItemResult {
                    name: "demo".into(),
                    outcome: bone_protocol::CatalogItemOutcome::Installed,
                }],
                changed: true,
                extensions_reloaded: true,
            },
        ));
        assert_eq!(app.catalog.as_ref().unwrap().revision, "r2");
        assert_eq!(app.catalog_notice, "Catalog item installed: demo");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn catalog_updates_reads_selected_tab_frontend() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "catalogcount");
        assert_eq!(app.catalog_updates(), 0);
        app.tabs[0].state.frontend = Some(state::FrontendState {
            catalog_updates: 3,
            ..state::FrontendState::default()
        });
        assert_eq!(app.catalog_updates(), 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sync_keymap_parses_settings_and_tracks_revision() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "keymapsync");
        app.tabs[0].state.frontend = Some(state::FrontendState {
            settings: serde_json::json!({
                "revision": 5,
                "keymaps": {"bindings": [{"key": "<C-p>", "action": "toggle_panes"}]},
            }),
            ..state::FrontendState::default()
        });
        app.sync_keymap();
        assert_eq!(app.keymap.bindings.len(), 1);
        assert_eq!(app.keymap.bindings[0].action, "toggle_panes");
        assert_eq!(app.keymap_revision, Some(5));
        // The same revision is skipped, so a cleared keymap is left untouched.
        app.keymap = keymap::Keymap::default();
        app.sync_keymap();
        assert!(
            app.keymap.bindings.is_empty(),
            "unchanged revision must not reparse"
        );
        // A new revision reparses.
        app.tabs[0].state.frontend = Some(state::FrontendState {
            settings: serde_json::json!({
                "revision": 6,
                "keymaps": {"bindings": [
                    {"key": "<C-p>", "action": "toggle_panes"},
                    {"key": "<S-Tab>", "action": "/help"},
                ]},
            }),
            ..state::FrontendState::default()
        });
        app.sync_keymap();
        assert_eq!(app.keymap.bindings.len(), 2);
        assert_eq!(app.keymap_revision, Some(6));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_keymap_dispatched_correlates_and_routes() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "keymapcorr");
        let tab_id = app.tabs[0].id;
        // A reply with a stale request id is ignored and leaves the pending slot.
        app.pending_keymap = Some((tab_id, 7));
        app.apply_keymap_dispatched(
            &ctx,
            Some(8),
            KeymapDispatchKind::Prompt {
                text: "stale".into(),
            },
        );
        assert_eq!(app.pending_keymap, Some((tab_id, 7)));
        assert!(app.tabs[0].composer.is_empty());
        // The matching id routes to the target tab and clears the pending slot.
        app.apply_keymap_dispatched(
            &ctx,
            Some(7),
            KeymapDispatchKind::Prompt {
                text: "hello".into(),
            },
        );
        assert_eq!(app.pending_keymap, None);
        assert_eq!(app.tabs[0].composer, "hello");
        // A legacy `None` id applies to the selected tab without correlation.
        app.tabs[0].composer.clear();
        app.apply_keymap_dispatched(
            &ctx,
            None,
            KeymapDispatchKind::Prompt {
                text: "legacy".into(),
            },
        );
        assert_eq!(app.tabs[0].composer, "legacy");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_keymap_builtin_toggles_panes() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "keymapbuiltin");
        app.add_tab(Intent::New, &ctx); // two tabs so the split can stay on
        app.focus_conversation(app.tabs[0].id, &ctx);
        app.panes_visible = false;
        app.apply_keymap_dispatched(
            &ctx,
            None,
            KeymapDispatchKind::Builtin {
                action: "toggle_panes".into(),
            },
        );
        assert!(app.panes_visible, "toggle_panes shows the float panes");
        app.apply_keymap_dispatched(
            &ctx,
            None,
            KeymapDispatchKind::Builtin {
                action: "toggle_panes".into(),
            },
        );
        assert!(!app.panes_visible, "toggle_panes toggles them back off");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stats_and_catalog_dialogs_render_headless() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "phase6render");
        app.tabs[0].connected = true;
        app.show_stats = true;
        app.stats = Some(sample_stats());
        app.show_catalog = true;
        app.catalog = Some(sample_catalog("r1"));

        ctx.run_ui(egui::RawInput::default(), |ui| {
            app.stats_dialog(ui.ctx());
            app.catalog_dialog(ui.ctx());
        })
        .textures_delta
        .clear();

        assert!(app.show_stats);
        assert!(app.show_catalog);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stats_picker_dialog_renders_headless_and_persists_edits() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "statspicker");
        app.tabs[0].connected = true;
        app.show_stats = true;
        app.stats = Some(sample_stats());
        app.stats_custom = Some(DateRange {
            start: "2026-01-01".into(),
            end: "2026-01-31".into(),
        });
        app.stats_picker = Some(("2026-01-01".into(), "2026-01-31".into()));

        ctx.run_ui(egui::RawInput::default(), |ui| {
            app.stats_dialog(ui.ctx());
            app.stats_picker_controls(ui);
        })
        .textures_delta
        .clear();

        // Without Apply/Cancel the picker keeps its (possibly edited) fields.
        assert_eq!(
            app.stats_picker,
            Some(("2026-01-01".to_string(), "2026-01-31".to_string()))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_theme_sets_visuals_from_tab_theme() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfgtheme");
        // Unknown daemon fields (shell/syntax/highlights) are ignored.
        app.tabs[0].state.theme = Some(serde_json::json!({
            "name": "dark",
            "palette": { "bg": "#101014", "fg": "#e0e0e0", "accent": "#4f9cf9" },
            "syntax": { "keyword": "#ff79c6" },
            "highlights": { "user_msg": "#8be9fd" }
        }));
        app.apply_theme(&ctx);
        let visuals = ctx.style_of(ctx.theme()).visuals.clone();
        assert!(visuals.dark_mode);
        assert_eq!(
            visuals.panel_fill,
            egui::Color32::from_rgb(0x10, 0x10, 0x14)
        );
        assert_eq!(app.applied_theme, app.tabs[0].state.theme);

        // No theme payload: leave visuals untouched and mark nothing applied.
        app.tabs[0].state.theme = None;
        app.applied_theme = None;
        app.apply_theme(&ctx);
        assert!(app.applied_theme.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_request_abandoned_when_sending_tab_disconnects() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfggone");
        app.tabs[0].connected = true;
        assert!(app.request_config());
        app.tabs[0].connected = false; // socket dropped before any snapshot
        app.poll_config();
        assert!(app.config_request.is_none());
        assert!(app.config.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn provider_switch_and_model_save_refuse_without_connection_or_snapshot() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfgnoop");
        // No snapshot: nothing can be sent.
        assert!(!app.switch_provider(None, "openai"));
        assert!(!app.save_model(None, "gpt-5".into()));
        app.config = Some(sample_config(7, "local", &[("local", "qwen3")]));
        // Snapshot but no connected tab: still refused.
        assert!(!app.switch_provider(None, "local"));
        assert!(!app.save_model(None, "other".into()));
        app.tabs[0].connected = true;
        app.tabs[0].state.snapshot.provider_id = "local".into();
        app.tabs[0].state.snapshot.provider_model = "qwen3".into();
        // Unchanged model is a no-op even when connected.
        assert!(!app.save_model(None, "qwen3".into()));
        // A changed model goes out.
        assert!(app.save_model(None, "qwen3-fast".into()));
        assert!(app.provider_notice.contains("Saving model"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn provider_actions_stay_with_origin_tab_and_runtime_display() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfg-origin");
        app.add_tab(Intent::New, &ctx);
        let (tx0, mut rx0) = mpsc::unbounded_channel();
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        app.tabs[0].commands = tx0;
        app.tabs[1].commands = tx1;
        for tab in &mut app.tabs {
            tab.connected = true;
        }
        app.tabs[0].state.snapshot.provider_id = "local".into();
        app.tabs[0].state.snapshot.provider_model = "qwen3".into();
        app.tabs[1].state.snapshot.provider_id = "openai".into();
        app.tabs[1].state.snapshot.provider_model = "gpt-5".into();
        app.config = Some(sample_config(
            4,
            "local",
            &[("local", "qwen3"), ("openai", "gpt-5")],
        ));
        assert_eq!(app.model_pill(0).0, "Model: qwen3");
        assert_eq!(app.model_pill(1).0, "Model: gpt-5");

        let origin = app.tabs[1].id;
        app.selected = 0;
        assert!(app.save_model(Some(origin), "gpt-5.1".into()));
        assert!(
            matches!(rx1.try_recv(), Ok(Command::Send(RuntimeCommand::UpsertProvider { provider, .. })) if provider.id == "openai" && provider.model == "gpt-5.1")
        );
        assert!(
            rx0.try_recv().is_err(),
            "save must not fall back to selected tab"
        );

        app.tabs[1].connected = false;
        assert!(!app.switch_provider(Some(origin), "openai"));
        assert!(
            rx0.try_recv().is_err(),
            "disconnected origin must not use another tab"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn config_poll_is_a_noop_in_demo_mode() {
        let ctx = egui::Context::default();
        let dir = temp_dir("cfgdemo");
        let mut app = DesktopApp::open(ctx.clone(), true, Some(dir.join("layout.txt")));
        app.tabs[0].connected = true;
        app.poll_config();
        assert!(app.config_request.is_none());
        assert!(app.config.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- Stage 5 slice 2: image attachments -------------------------------

    #[test]
    fn attach_image_encodes_base64_and_sets_media_type() {
        let bytes = vec![1, 2, 3]; // STANDARD base64 "AQID"
        let attachment = Attachment::from_file("pic.png", bytes.clone()).unwrap();
        assert_eq!(attachment.name, "pic.png");
        assert_eq!(attachment.media_type, "image/png");
        assert_eq!(attachment.byte_size, 3);
        assert_eq!(attachment.data_b64, "AQID");
        // Round-trip: the stored string decodes back to the original bytes.
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&attachment.data_b64)
                .unwrap(),
            bytes
        );
        // Extension mapping is case-insensitive and covers the supported set.
        assert_eq!(media_type_for("photo.JPG"), Some("image/jpeg"));
        assert_eq!(media_type_for("photo.jpeg"), Some("image/jpeg"));
        assert_eq!(media_type_for("a.webp"), Some("image/webp"));
        assert_eq!(media_type_for("a.GIF"), Some("image/gif"));
    }

    #[test]
    fn attach_image_rejects_unknown_extension_and_oversized() {
        let err = Attachment::from_file("notes.txt", vec![1, 2, 3]).unwrap_err();
        assert!(err.contains("not a supported image"), "got: {err}");
        // A file with no extension at all is rejected too.
        assert!(Attachment::from_file("README", vec![1]).is_err());
        // One byte over the cap is rejected; exactly at the cap is allowed.
        let err =
            Attachment::from_file("big.png", vec![0u8; MAX_ATTACHMENT_BYTES + 1]).unwrap_err();
        assert!(err.contains("over the"), "got: {err}");
        assert!(Attachment::from_file("edge.png", vec![0u8; MAX_ATTACHMENT_BYTES]).is_ok());
    }

    #[test]
    fn can_send_allows_attachments_with_empty_text() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "attcansend");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        assert!(!tab.can_send(), "empty text + no attachments must not send");
        tab.attachments
            .push(Attachment::from_file("a.png", vec![1, 2, 3]).unwrap());
        assert!(tab.can_send(), "an attachment alone must be sendable");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn composer_slash_opens_autocomplete_and_accepts() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "slashac");
        let tab = &mut app.tabs[0];
        tab.composer = "/he".into();
        tab.refresh_autocomplete();
        let ac = tab
            .autocomplete
            .as_ref()
            .expect("autocomplete open for /he");
        assert_eq!(ac.selected_command(), Some("help"));
        assert!(tab.accept_autocomplete(), "accept fills the buffer");
        assert_eq!(tab.composer, "/help");
        assert!(tab.autocomplete.is_none(), "popup closes after accept");
        // Arguments end the command-name query, so the popup stays closed.
        tab.composer = "/help me".into();
        tab.refresh_autocomplete();
        assert!(tab.autocomplete.is_none());
        // Non-slash text closes it too.
        tab.composer = "hello".into();
        tab.refresh_autocomplete();
        assert!(tab.autocomplete.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn autocomplete_merges_daemon_advertised_commands() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "slashlua");
        let tab = &mut app.tabs[0];
        tab.state.frontend = Some(state::FrontendState {
            commands: vec![("summarize".into(), "summarize the chat".into())],
            ..Default::default()
        });
        tab.composer = "/sum".into();
        tab.refresh_autocomplete();
        let ac = tab
            .autocomplete
            .as_ref()
            .expect("autocomplete open for /sum");
        assert_eq!(ac.selected_command(), Some("summarize"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn send_prompt_clears_composer_and_attachments() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "attsend");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        tab.composer = "describe this".into();
        tab.attachments
            .push(Attachment::from_file("a.png", vec![1, 2, 3]).unwrap());
        tab.send_prompt();
        assert!(tab.composer.is_empty(), "composer must clear on send");
        assert!(tab.attachments.is_empty(), "attachments must clear on send");
        assert!(tab.state.busy, "send must mark the turn busy");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn run_command_records_pending_id_and_correlates_complete() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "runcmd");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        assert!(tab.run_command("help", ""));
        let pending = tab.pending_command.expect("pending id recorded");
        // A non-matching CommandComplete leaves the pending id intact.
        tab.handle_event(Event::Runtime(RuntimeEvent::CommandComplete {
            request_id: Some(pending + 1),
            output: "other".into(),
            submit: false,
            display_role: None,
            action: None,
        }));
        assert_eq!(tab.pending_command, Some(pending));
        // The matching result clears the marker and displays the output.
        tab.handle_event(Event::Runtime(RuntimeEvent::CommandComplete {
            request_id: Some(pending),
            output: "help text".into(),
            submit: false,
            display_role: Some("assistant".into()),
            action: None,
        }));
        assert!(tab.pending_command.is_none());
        assert!(
            tab.state
                .rows
                .iter()
                .any(|row| row.0 == "assistant" && row.1 == "help text")
        );
        // Disconnect/reconnect must drop a stale in-flight marker.
        tab.pending_command = Some(99);
        tab.reset_for_attach();
        assert!(tab.pending_command.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn submit_composer_routes_slash_and_plain_text() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "subcomp");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        // Capture outgoing commands instead of the spawned worker.
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;

        // `/help` is client-side: it clears the composer, sends nothing to the
        // daemon, and renders a command/shortcut reference as a system row.
        tab.composer = "/help".into();
        tab.submit_composer();
        assert!(tab.composer.is_empty(), "slash command clears the composer");
        assert!(
            tab.pending_command.is_none(),
            "/help is not sent to the daemon"
        );
        assert!(rx.try_recv().is_err(), "/help sends no command");
        let (role, text) = tab.state.rows.last().expect("a help row was pushed");
        assert_eq!(role, "system");
        assert!(text.contains("Commands"), "help lists commands: {text}");
        assert!(text.contains("/help"), "help lists /help: {text}");

        // An unknown slash command still routes to the daemon as RunCommand.
        tab.composer = "/custom arg".into();
        tab.submit_composer();
        assert!(
            tab.composer.is_empty(),
            "unknown slash command clears composer"
        );
        assert_eq!(tab.pending_command_echo.as_deref(), Some("/custom arg"));
        match rx.try_recv().expect("a command was sent") {
            Command::Send(RuntimeCommand::RunCommand {
                name,
                input,
                request_id,
                ..
            }) => {
                assert_eq!(name, "custom");
                assert_eq!(input, "arg");
                assert!(request_id.is_some());
            }
            _ => panic!("expected RunCommand"),
        }
        tab.pending_command = None;

        // Plain text routes to a normal prompt turn.
        tab.composer = "hello there".into();
        tab.submit_composer();
        assert!(tab.state.busy, "plain text marks the turn busy");
        match rx.try_recv().expect("a command was sent") {
            Command::Send(RuntimeCommand::SubmitPrompt { text, .. }) => {
                assert_eq!(text, "hello there");
            }
            _ => panic!("expected SubmitPrompt"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clear_command_clears_transcript_and_sends_new_conversation() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "clearcmd");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        tab.state.push_row("user", "old question");
        tab.composer = "/clear".into();
        tab.submit_composer();
        assert!(tab.composer.is_empty(), "composer cleared");
        match rx.try_recv().expect("NewConversation sent") {
            Command::Send(RuntimeCommand::NewConversation) => {}
            _ => panic!("expected NewConversation"),
        }
        assert_eq!(tab.state.rows.len(), 1, "old transcript dropped");
        assert_eq!(tab.state.rows[0].0, "system");
        assert_eq!(tab.state.rows[0].1, "Chat cleared.");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn model_command_shows_active_or_queues_save() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "modelcmd");
        let tab = &mut app.tabs[0];
        tab.state.snapshot.provider_id = "openai".into();
        tab.state.snapshot.provider_model = "gpt-4".into();
        // `/model` with no argument reports the active provider/model.
        tab.composer = "/model".into();
        tab.submit_composer();
        assert!(tab.pending_ui.is_empty(), "bare /model opens nothing");
        let (role, text) = tab.state.rows.last().expect("a model row was pushed");
        assert_eq!(role, "system");
        assert!(
            text.contains("gpt-4") && text.contains("openai"),
            "got {text}"
        );
        // `/model <name>` queues a save for the app to apply.
        tab.composer = "/model gpt-5".into();
        tab.submit_composer();
        assert!(tab.composer.is_empty());
        match tab.pending_ui.as_slice() {
            [UiRequest::SaveModel(model)] => assert_eq!(model, "gpt-5"),
            other => panic!("expected SaveModel, got {} requests", other.len()),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn provider_command_queues_dialog_or_switch() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "provcmd");
        let tab = &mut app.tabs[0];
        tab.composer = "/provider".into();
        tab.submit_composer();
        assert!(matches!(
            tab.pending_ui.as_slice(),
            [UiRequest::OpenProvider]
        ));
        tab.pending_ui.clear();
        tab.composer = "/provider anthropic".into();
        tab.submit_composer();
        match tab.pending_ui.as_slice() {
            [UiRequest::SwitchProvider(id)] => assert_eq!(id, "anthropic"),
            other => panic!("expected SwitchProvider, got {} requests", other.len()),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn incognito_command_sends_toggle_and_rejects_bad_option() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "incogcmd");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        tab.composer = "/incognito on".into();
        tab.submit_composer();
        match rx.try_recv().expect("SetIncognito sent") {
            Command::Send(RuntimeCommand::SetIncognito { enabled }) => assert!(enabled),
            _ => panic!("expected SetIncognito"),
        }
        assert_eq!(tab.state.rows.last().unwrap().1, "Incognito on.");
        // An invalid argument is reported and sends nothing.
        tab.composer = "/incognito maybe".into();
        tab.submit_composer();
        assert!(rx.try_recv().is_err(), "invalid option sends nothing");
        let text = &tab.state.rows.last().unwrap().1;
        assert!(text.contains("Unknown option"), "got {text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stats_setup_and_catalog_queue_ui_requests() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "uicmd");
        let tab = &mut app.tabs[0];
        tab.composer = "/stats".into();
        tab.submit_composer();
        assert!(matches!(tab.pending_ui.as_slice(), [UiRequest::OpenStats]));
        tab.pending_ui.clear();
        tab.composer = "/setup".into();
        tab.submit_composer();
        assert!(matches!(tab.pending_ui.as_slice(), [UiRequest::OpenSetup]));
        tab.pending_ui.clear();
        tab.composer = "/catalog".into();
        tab.submit_composer();
        assert!(matches!(
            tab.pending_ui.as_slice(),
            [UiRequest::OpenCatalog]
        ));
        tab.pending_ui.clear();
        tab.composer = "/catalog install foo".into();
        tab.submit_composer();
        match tab.pending_ui.as_slice() {
            [UiRequest::CatalogAction(arg)] => assert_eq!(arg, "install foo"),
            other => panic!("expected CatalogAction, got {} requests", other.len()),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_ui_request_opens_the_right_dialog() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "uiopen");
        let id = app.tabs[0].id;
        app.apply_ui_request(id, UiRequest::OpenStats);
        assert!(app.show_stats);
        app.apply_ui_request(id, UiRequest::OpenSetup);
        assert!(app.show_setup);
        app.apply_ui_request(id, UiRequest::OpenConfig);
        assert!(app.show_config);
        app.apply_ui_request(id, UiRequest::OpenCatalog);
        assert!(app.show_catalog);
        app.apply_ui_request(id, UiRequest::OpenProvider);
        assert!(app.show_provider);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_command_opens_modal_client_local() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "editcmd");
        let tab = &mut app.tabs[0];
        // Not connected/ready: `/edit` is client-local and must still work.
        tab.composer = "/edit".into();
        tab.submit_composer();
        assert!(tab.show_editor, "modal opens");
        assert!(
            tab.editor_text.is_empty(),
            "editor opens empty like the TUI"
        );
        assert!(tab.composer.is_empty(), "the /edit command text is cleared");
        assert!(tab.pending_command.is_none(), "no daemon command");
        // `/e` is an alias.
        tab.show_editor = false;
        tab.composer = "/e".into();
        tab.submit_composer();
        assert!(tab.show_editor, "/e opens the modal");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inline_quit_requests_close() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "quitcmd");
        let tab = &mut app.tabs[0];
        tab.composer = ":q".into();
        tab.submit_composer();
        assert!(tab.close_request, ":q requests a close");
        assert!(tab.composer.is_empty(), "composer cleared");
        assert!(tab.pending_command.is_none(), "no daemon command");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_without_binary_reports_fallback() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "updatenone");
        let tab = &mut app.tabs[0];
        tab.start_update_with(None);
        let (role, text) = tab.state.rows.last().expect("a reply row");
        assert_eq!(role, "system");
        assert_eq!(text, &local::update_reply(None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn update_with_binary_sets_status() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "updatebin");
        let tab = &mut app.tabs[0];
        // A missing binary still spawns a worker that fails harmlessly.
        tab.start_update_with(Some(dir.join("missing-bone")));
        assert_eq!(tab.state.status, "Checking for updates…");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inline_shell_routes_both_prefixes() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "shellroute");
        let tab = &mut app.tabs[0];
        // Not connected/ready: inline shell is client-local and must still run.
        tab.composer = ": true".into();
        tab.submit_composer();
        assert!(tab.composer.is_empty(), "composer cleared");
        assert_eq!(tab.state.status, "Running inline command…");
        assert!(tab.pending_command.is_none(), "no daemon command");
        // `!` is a documented alias for `:`.
        tab.composer = "! true".into();
        tab.submit_composer();
        assert!(tab.composer.is_empty(), "composer cleared for ! too");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn append_shell_result_pushes_tool_row_and_folds_message() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "shellres");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;

        tab.append_shell_result("echo hi", "exit code: 0\nstdout:\nhi", false);
        let i = tab.state.rows.len() - 1;
        assert_eq!(tab.state.rows[i].0, "tool: shell");
        let card = tab.state.toolcards[i].as_ref().expect("shell tool card");
        assert_eq!(card.name, "shell");
        assert_eq!(card.state, ToolState::Done);
        assert_eq!(card.label.as_deref(), Some("shell echo hi"));
        match rx.try_recv().expect("append message sent") {
            Command::Send(RuntimeCommand::AppendMessage { role, content }) => {
                assert_eq!(role, "user");
                assert!(content.starts_with("$ echo hi\n"), "{content}");
            }
            _ => panic!("expected AppendMessage"),
        }

        // A failing command renders as an error card.
        tab.append_shell_result("false", "exit code: 1\nstdout:\n", true);
        let i = tab.state.rows.len() - 1;
        assert_eq!(tab.state.rows[i].0, "tool: shell (error)");
        assert_eq!(
            tab.state.toolcards[i].as_ref().unwrap().state,
            ToolState::Error
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn queue_composer_requires_busy_and_text() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "queuebusy");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        tab.composer = "next".into();
        // Idle: cannot queue.
        assert!(!tab.can_queue());
        tab.enqueue_composer();
        assert!(tab.queue.is_empty(), "nothing queued while idle");
        // Busy: queues the trimmed text and clears the composer.
        tab.state.busy = true;
        assert!(tab.can_queue());
        tab.enqueue_composer();
        assert_eq!(tab.queue.len(), 1);
        assert_eq!(tab.queue[0], "next");
        assert!(tab.composer.is_empty(), "composer cleared after queueing");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn queue_reorder_send_next_and_edit() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "queueedit");
        let tab = &mut app.tabs[0];
        for text in ["a", "b", "c"] {
            tab.queue.push_back(text.into());
        }
        // Reorder up/down with clamping at the ends.
        tab.move_queued_up(0);
        assert_eq!(tab.queue.iter().collect::<Vec<_>>(), vec!["a", "b", "c"]);
        tab.move_queued_up(2);
        assert_eq!(tab.queue.iter().collect::<Vec<_>>(), vec!["a", "c", "b"]);
        tab.move_queued_down(2);
        assert_eq!(tab.queue.iter().collect::<Vec<_>>(), vec!["a", "c", "b"]);
        tab.move_queued_down(0);
        assert_eq!(tab.queue.iter().collect::<Vec<_>>(), vec!["c", "a", "b"]);
        // Send next moves the chosen prompt to the front.
        tab.send_queued_next(2);
        assert_eq!(tab.queue.iter().collect::<Vec<_>>(), vec!["b", "c", "a"]);
        // Edit pulls the prompt into the composer and removes it from the queue.
        tab.composer = "leftover".into();
        tab.edit_queued(1);
        assert_eq!(tab.composer, "c");
        assert_eq!(tab.queue.iter().collect::<Vec<_>>(), vec!["b", "a"]);
        // Remove deletes a single entry; out-of-range indices are ignored.
        tab.remove_queued(5);
        assert_eq!(tab.queue.len(), 2);
        tab.remove_queued(0);
        assert_eq!(tab.queue.iter().collect::<Vec<_>>(), vec!["a"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paste_placeholder_round_trips_and_expands() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "paste");
        let tab = &mut app.tabs[0];
        assert_eq!(tab.expanded_composer(), "");
        // A large paste collapses to a token and expands back on submit.
        let blob = "x".repeat(600);
        let len = tab.insert_paste_placeholder(&blob, 0);
        assert_eq!(tab.composer, "[Pasted text #1 +600 chars]");
        assert_eq!(len, tab.composer.chars().count());
        assert_eq!(tab.expanded_composer(), blob);
        // Insert at a char index mid-buffer and normalize CRLF line endings.
        tab.composer = "ab".into();
        tab.pastes.clear();
        tab.paste_seq = 0;
        tab.insert_paste_placeholder("line1\r\nline2", 1);
        assert_eq!(tab.composer, "a[Pasted text #1 +11 chars]b");
        assert_eq!(tab.expanded_composer(), "aline1\nline2b");
        // An out-of-range index appends at the end.
        tab.composer = "z".into();
        tab.pastes.clear();
        tab.paste_seq = 0;
        tab.insert_paste_placeholder("blob", 99);
        assert_eq!(tab.composer, "z[Pasted text #1 +4 chars]");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn drain_queue_sends_one_prompt_when_idle() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "queuedrain");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        tab.queue.push_back("first".into());
        tab.queue.push_back("second".into());
        // Busy: drain waits.
        tab.state.busy = true;
        tab.drain_queue();
        assert_eq!(tab.queue.len(), 2, "no drain while busy");
        // Idle: send exactly one; the send re-marks the tab busy.
        tab.state.busy = false;
        tab.drain_queue();
        assert_eq!(tab.queue.len(), 1);
        assert_eq!(tab.queue[0], "second");
        assert!(tab.state.busy, "send marks the tab busy");
        match rx.try_recv().expect("SubmitPrompt sent") {
            Command::Send(RuntimeCommand::SubmitPrompt { text, .. }) => {
                assert_eq!(text, "first");
            }
            _ => panic!("expected SubmitPrompt"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn drain_queue_waits_for_empty_composer() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "queuedraft");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, _rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        tab.queue.push_back("queued".into());
        tab.composer = "draft".into();
        tab.drain_queue();
        assert_eq!(tab.queue.len(), 1, "draft keeps the queue waiting");
        assert_eq!(tab.composer, "draft");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn conversation_loaded_preserves_and_pauses_queue() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "queueload");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, _rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        tab.queue.push_back("stale".into());
        tab.handle_event(Event::Runtime(RuntimeEvent::ConversationLoaded {
            messages: vec![],
            snapshot: bone_protocol::SessionSnapshot::default(),
            busy: false,
        }));
        assert_eq!(tab.queue.front().map(String::as_str), Some("stale"));
        assert!(
            tab.queue_paused,
            "a conversation load pauses unsent messages"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn record_history_dedupes_and_caps() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "histdedupe");
        let tab = &mut app.tabs[0];
        // Blank drafts are ignored.
        tab.composer = "   ".into();
        tab.record_history();
        assert!(tab.history.is_empty(), "blank draft not recorded");
        // Re-recording the same text moves it to the newest slot, no duplicate.
        for text in ["alpha", "beta", "alpha"] {
            tab.composer = text.into();
            tab.record_history();
        }
        assert_eq!(tab.history, vec!["beta".to_string(), "alpha".to_string()]);
        // The list is capped, dropping the oldest entries.
        for i in 0..MAX_HISTORY + 5 {
            tab.composer = format!("item-{i}");
            tab.record_history();
        }
        assert_eq!(tab.history.len(), MAX_HISTORY);
        assert_eq!(
            tab.history.last().unwrap(),
            &format!("item-{}", MAX_HISTORY + 4)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn history_prev_next_traversal() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "histnav");
        let tab = &mut app.tabs[0];
        for text in ["a", "b", "c"] {
            tab.composer = text.into();
            tab.record_history();
        }
        // Ctrl+Up walks newest to oldest and clamps at the oldest entry.
        tab.history_prev();
        assert_eq!(tab.composer, "c");
        assert_eq!(tab.history_index, Some(2));
        tab.history_prev();
        assert_eq!(tab.composer, "b");
        tab.history_prev();
        assert_eq!(tab.composer, "a");
        tab.history_prev();
        assert_eq!(tab.composer, "a", "clamped at the oldest entry");
        // Ctrl+Down walks forward; past the newest it clears to a fresh line.
        tab.history_next();
        assert_eq!(tab.composer, "b");
        tab.history_next();
        assert_eq!(tab.composer, "c");
        tab.history_next();
        assert!(tab.composer.is_empty(), "cleared past the newest entry");
        assert_eq!(tab.history_index, None);
        // Down with no active recall is a no-op.
        tab.history_next();
        assert!(tab.composer.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn send_prompt_records_history() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "histsend");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        tab.composer = "hello world".into();
        tab.send_prompt();
        assert_eq!(tab.history, vec!["hello world".to_string()]);
        assert!(tab.composer.is_empty());
        match rx.try_recv().expect("SubmitPrompt sent") {
            Command::Send(RuntimeCommand::SubmitPrompt { text, .. }) => {
                assert_eq!(text, "hello world");
            }
            _ => panic!("expected SubmitPrompt"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_command_complete_renders_only_matching_id() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cmdcomplete");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, _rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        assert!(tab.run_command("help", ""));
        let pending = tab.pending_command.expect("pending id");

        // A non-matching broadcast is ignored (no rows, marker intact).
        tab.handle_event(Event::Runtime(RuntimeEvent::CommandComplete {
            request_id: Some(pending + 1),
            output: "other client".into(),
            submit: false,
            display_role: None,
            action: None,
        }));
        assert_eq!(tab.pending_command, Some(pending));
        assert!(tab.state.rows.is_empty(), "foreign result renders nothing");

        // The matching result renders by display role and clears the markers.
        tab.handle_event(Event::Runtime(RuntimeEvent::CommandComplete {
            request_id: Some(pending),
            output: "help text".into(),
            submit: false,
            display_role: Some("assistant".into()),
            action: None,
        }));
        assert!(tab.pending_command.is_none());
        assert!(tab.pending_command_echo.is_none());
        assert_eq!(
            tab.state.rows.last().unwrap(),
            &("assistant".to_string(), "help text".to_string())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn apply_command_complete_submit_pushes_echo_and_busy() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cmdsubmit");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, _rx) = mpsc::unbounded_channel();
        tab.commands = tx;
        assert!(tab.run_command("do", "the thing"));
        let pending = tab.pending_command.expect("pending id");
        tab.handle_event(Event::Runtime(RuntimeEvent::CommandComplete {
            request_id: Some(pending),
            output: "the thing".into(),
            submit: true,
            display_role: None,
            action: None,
        }));
        assert!(tab.state.busy, "submit keeps the turn busy");
        assert_eq!(
            tab.state.rows.last().unwrap(),
            &("user".to_string(), "/do the thing".to_string())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn command_action_replace_and_switch_provider_send_commands() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cmdaction");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        tab.commands = tx;

        // conversation_replace forwards ReplaceConversation.
        assert!(tab.run_command("compact", ""));
        let pending = tab.pending_command.expect("pending id");
        let _ = rx.try_recv(); // drain the RunCommand
        tab.handle_event(Event::Runtime(RuntimeEvent::CommandComplete {
            request_id: Some(pending),
            output: String::new(),
            submit: false,
            display_role: None,
            action: Some(CommandAction {
                conversation_replace: Some(Vec::new()),
                ..Default::default()
            }),
        }));
        match rx.try_recv().expect("ReplaceConversation sent") {
            Command::Send(RuntimeCommand::ReplaceConversation { messages }) => {
                assert!(messages.is_empty());
            }
            _ => panic!("expected ReplaceConversation"),
        }

        // SwitchProvider fires the command and replaces output with the reply.
        assert!(tab.run_command("model", "fast"));
        let pending = tab.pending_command.expect("pending id");
        let _ = rx.try_recv(); // drain the RunCommand
        tab.handle_event(Event::Runtime(RuntimeEvent::CommandComplete {
            request_id: Some(pending),
            output: "ignored".into(),
            submit: false,
            display_role: None,
            action: Some(CommandAction {
                config_action: Some(ConfigAction::SwitchProvider { id: "fast".into() }),
                ..Default::default()
            }),
        }));
        match rx.try_recv().expect("SwitchProvider sent") {
            Command::Send(RuntimeCommand::SwitchProvider { provider_id }) => {
                assert_eq!(provider_id, "fast");
            }
            _ => panic!("expected SwitchProvider"),
        }
        assert_eq!(
            tab.state.rows.last().unwrap(),
            &(
                "system".to_string(),
                "Switching provider to fast…".to_string()
            )
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- Stage 5 slice 3: rename/delete conversations ------------------------

    #[test]
    fn rename_commit_sends_and_response_updates_list() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "rensend");
        app.tabs[0].connected = true;
        app.conversations = vec![sample_meta(7, "first prompt")];
        app.conversations_loaded = true;
        app.start_rename(7);
        app.rename_field = "  new title  ".into(); // trimmed on commit
        app.commit_rename();
        assert!(app.rename_target.is_none(), "field must close on success");
        let (_, request_id) = app.conversations_request.unwrap();
        // The daemon answers with the refreshed list (LIMIT 100 semantics).
        let renamed = sample_meta(7, "new title");
        app.tabs[0].host_responses.push((
            request_id,
            bone_protocol::HostResponse::Conversations(vec![renamed.clone()]),
        ));
        app.drain_all(&ctx);
        assert_eq!(app.conversations, vec![renamed]);
        assert!(app.conversations_request.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_empty_title_is_refused_locally() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "renblank");
        app.conversations = vec![sample_meta(7, "first prompt")];
        app.start_rename(7);
        app.rename_field = "   ".into();
        app.commit_rename();
        assert!(app.conversations_request.is_none(), "nothing may go out");
        assert_eq!(app.rename_target, Some(7), "the field stays open");
        assert!(app.sidebar_notice.contains("blank"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_and_delete_are_refused_without_connection() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "rennoop");
        app.conversations = vec![sample_meta(7, "first prompt")];
        app.start_rename(7);
        app.rename_field = "new title".into();
        app.commit_rename();
        assert!(app.conversations_request.is_none());
        assert_eq!(app.rename_target, Some(7), "the field stays open");
        assert!(!app.sidebar_notice.is_empty());
        app.cancel_rename();
        app.start_delete(7);
        assert!(app.delete_target.is_some());
        app.commit_delete();
        assert!(app.conversations_request.is_none(), "delete refused too");
        assert!(!app.sidebar_notice.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_error_response_reports_update_failure() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "renerr");
        app.tabs[0].connected = true;
        app.conversations = vec![sample_meta(7, "first prompt")];
        app.start_rename(7);
        app.rename_field = "new title".into();
        app.commit_rename();
        let (_, request_id) = app.conversations_request.unwrap();
        app.tabs[0].host_responses.push((
            request_id,
            bone_protocol::HostResponse::Error {
                code: bone_protocol::HostErrorCode::Invalid,
                message: "no such conversation".into(),
            },
        ));
        app.drain_all(&ctx);
        assert!(
            app.sidebar_notice.contains("no such conversation"),
            "notice: {}",
            app.sidebar_notice
        );
        assert!(
            app.sidebar_notice.contains("update failed"),
            "a mutation error must not read as a list error. notice: {}",
            app.sidebar_notice
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn deleting_an_open_task_requires_explicit_confirmation() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "delopen");
        app.tabs[0].conversation_id = Some(7); // attached
        app.conversations = vec![sample_meta(7, "first prompt")];
        app.start_delete(7);
        assert_eq!(app.delete_target.as_ref().map(|t| t.0), Some(7));
        assert!(
            !app.tabs[0].closing,
            "opening the confirmation must not close the task"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn delete_commit_updates_list() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "delsend");
        app.tabs[0].connected = true;
        app.conversations = vec![sample_meta(7, "keep"), sample_meta(9, "gone")];
        app.conversations_loaded = true;
        app.start_delete(9);
        assert_eq!(app.delete_target, Some((9, "gone".into())));
        app.commit_delete();
        assert!(
            app.delete_target.is_none(),
            "confirmation closes on success"
        );
        let (_, request_id) = app.conversations_request.unwrap();
        let kept = sample_meta(7, "keep");
        app.tabs[0].host_responses.push((
            request_id,
            bone_protocol::HostResponse::Conversations(vec![kept.clone()]),
        ));
        app.drain_all(&ctx);
        assert_eq!(app.conversations, vec![kept]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- Stage 5 slice 4: keyboard shortcuts --------------------------------

    #[test]
    fn shortcuts_select_tabs_add_close_and_toggle_split() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "shortcuts");
        app.address = "127.0.0.1:17000".into(); // closed port: instant refusal
        app.add_tab(Intent::New, &ctx);
        app.add_tab(Intent::New, &ctx);
        assert_eq!(app.tabs.len(), 3);
        assert_eq!(app.selected, 2);
        // Ctrl+1 jumps to the first tab; Ctrl+9 is out of range.
        assert!(app.apply_shortcut(egui::Key::Num1, &ctx));
        assert_eq!(app.selected, 0);
        assert!(!app.apply_shortcut(egui::Key::Num9, &ctx));
        assert_eq!(app.selected, 0);
        // PageUp wraps from the first tab to the last; PageDown wraps forward.
        assert!(app.apply_shortcut(egui::Key::PageUp, &ctx));
        assert_eq!(app.selected, 2);
        assert!(app.apply_shortcut(egui::Key::PageDown, &ctx));
        assert_eq!(app.selected, 0);
        // Backslash moves the selected conversation into its own group.
        assert_eq!(app.workspace.window(0).unwrap().root.panes().len(), 1);
        assert!(app.apply_shortcut(egui::Key::Backslash, &ctx));
        assert_eq!(app.workspace.window(0).unwrap().root.panes().len(), 2);
        assert_eq!(app.workspace.active_tab(0), Some(app.tabs[0].id));
        assert!(!app.apply_shortcut(egui::Key::Num2, &ctx));
        // Ctrl+T adds a tab and connects it; Ctrl+W closes the selected one.
        assert!(app.apply_shortcut(egui::Key::T, &ctx));
        assert_eq!(app.tabs.len(), 4);
        assert!(app.tabs[app.selected].connecting);
        assert!(app.apply_shortcut(egui::Key::W, &ctx));
        assert!(app.tabs[app.selected].remove, "never-connected tab removed");
        // Unknown keys are not handled.
        assert!(!app.apply_shortcut(egui::Key::A, &ctx));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- Stage 5 slice 5: split views ----------------------------------------

    #[test]
    fn pane_selection_is_independent_and_empty_groups_collapse_on_close() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "splitrecon");
        app.add_tab(Intent::New, &ctx);
        app.add_tab(Intent::New, &ctx);
        let third = app.tabs[2].id;
        app.split_conversation(third, workspace::Axis::Horizontal, &ctx);
        let right = app.workspace.tab_location(third).unwrap().1;
        app.focus_conversation(app.tabs[0].id, &ctx);
        assert_eq!(app.workspace.pane(right).unwrap().active, Some(third));
        app.tabs[2].remove = true;
        assert!(app.prune_closed());
        assert!(app.workspace.pane(right).is_none());
        assert_eq!(app.workspace.window(0).unwrap().root.panes().len(), 1);
        assert_eq!(app.workspace.active_tab(0), Some(app.tabs[0].id));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- Stage 6 slice A: reconnect and daemon hardening ---------------------

    #[test]
    fn disconnect_drops_pending_interaction_state() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "disconnect-interaction");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        tab.state.busy = true;
        tab.state.pending_key = Some(9);
        tab.state.approvals.push(state::Approval {
            id: 10,
            name: "shell".into(),
            summary: "run command".into(),
            preview: None,
            blocked: None,
        });

        tab.handle_event(Event::Disconnected("Connection lost".into()));

        assert!(!tab.connected);
        assert!(!tab.state.busy);
        assert!(tab.state.pending_key.is_none());
        assert!(tab.state.approvals.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tab_mid_drop_only_when_connected_socket_is_lost() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "midclass");
        let tab = &mut app.tabs[0];
        // Never connected: init and connect failures are not mid-session drops.
        tab.handle_event(Event::Disconnected(
            "Runtime initialization failed: boom".into(),
        ));
        assert!(!tab.mid_drop, "unconnected init failure is not a drop");
        tab.handle_event(Event::Disconnected(
            "Connect failed: Connection refused (os error 111)".into(),
        ));
        assert!(!tab.mid_drop);
        assert!(tab.connect_failed_refused);
        // Established: losing the socket is a mid-session drop.
        tab.connected = true;
        tab.handle_event(Event::Disconnected(
            "Connection lost. Delivery may be uncertain; reconnect manually. Prompts are never \
             resent automatically."
                .into(),
        ));
        assert!(!tab.connected);
        assert!(
            tab.mid_drop,
            "established socket loss must flag a mid-session drop"
        );
        // Plain worker teardown never flags one either.
        tab.connected = true;
        tab.handle_event(Event::Disconnected("Disconnected".into()));
        assert!(
            !tab.mid_drop,
            "plain Disconnected is not a mid-session drop"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mid_session_drop_drives_bounded_reconnect_rounds() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "midbounded");
        app.address = "127.0.0.1:17000".into(); // closed port: instant refusal
        // Establish a session first.
        app.tabs[0].connected = true;
        app.pump_daemon(&ctx);
        assert_eq!(app.daemon_phase, daemon::Phase::Ready);
        // The daemon goes away: the established socket drops mid-session.
        app.tabs[0].connected = false;
        app.tabs[0].mid_drop = true;
        app.pump_daemon(&ctx);
        assert!(
            app.retry_at.is_some(),
            "the first drop schedules a reconnect"
        );
        assert_eq!(
            app.reconnect_budget,
            Some(daemon::MAX_RECONNECT_ROUNDS),
            "the first drop allocates the full budget"
        );
        // Each due tick spends one budget unit; the final failure gives up.
        for round in 0..daemon::MAX_RECONNECT_ROUNDS {
            app.retry_at = Some(Instant::now() - Duration::from_millis(1)); // due
            app.pump_daemon(&ctx);
            assert!(
                app.tabs[0].connecting,
                "round {round} must reconnect the auto tab"
            );
            // The attempt is refused (the port is still closed).
            app.tabs[0].connecting = false;
            app.tabs[0].connect_failed_refused = true;
            app.pump_daemon(&ctx);
            if round + 1 < daemon::MAX_RECONNECT_ROUNDS {
                assert!(
                    app.retry_at.is_some(),
                    "round {round} failure must schedule the next one"
                );
            }
        }
        match &app.daemon_phase {
            daemon::Phase::Stopped(message) => {
                assert!(
                    message.contains("could not restore it"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected Stopped, got {other:?}"),
        }
        assert!(app.retry_at.is_none());
        assert!(app.daemon_notice.contains("could not restore it"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mid_session_reconnect_restores_tabs_and_clears_budget() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "midrestore");
        app.add_tab(Intent::Load(7), &ctx);
        app.add_tab(Intent::Load(9), &ctx);
        for tab in &mut app.tabs {
            tab.connected = true;
        }
        app.pump_daemon(&ctx);
        assert_eq!(app.daemon_phase, daemon::Phase::Ready);
        // Every socket drops at once (the daemon went away).
        for tab in &mut app.tabs {
            tab.connected = false;
            tab.mid_drop = true;
        }
        app.pump_daemon(&ctx);
        assert!(app.retry_at.is_some());
        assert_eq!(app.reconnect_budget, Some(daemon::MAX_RECONNECT_ROUNDS));
        // The due tick reconnects every auto tab in one round.
        app.retry_at = Some(Instant::now() - Duration::from_millis(1));
        app.pump_daemon(&ctx);
        assert!(
            app.tabs.iter().all(|tab| tab.connecting),
            "every auto tab must reconnect"
        );
        // They all come back: the session is restored and the budget is spent.
        for tab in &mut app.tabs {
            tab.connected = true;
            tab.connecting = false;
        }
        app.pump_daemon(&ctx);
        assert_eq!(app.daemon_phase, daemon::Phase::Ready);
        assert_eq!(app.reconnect_budget, None);
        assert!(app.retry_at.is_none());
        assert_eq!(
            app.tabs
                .iter()
                .map(|tab| tab.conversation_id)
                .collect::<Vec<_>>(),
            vec![None, Some(7), Some(9)],
            "conversation pins survive the reconnect"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidebar_task_title_accepts_clicks_without_selecting_text() {
        for title in ["Short task".to_string(), "Long task title ".repeat(20)] {
            for button in [egui::PointerButton::Primary, egui::PointerButton::Secondary] {
                let ctx = egui::Context::default();
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(230.0, 100.0));
                let mut clicked = false;
                let mut secondary_clicked = false;
                let mut draw = |events| {
                    ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(screen),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            ui.style_mut().interaction.selectable_labels = true;
                            let response = task_row(
                                ui,
                                RowIndicator::None,
                                egui::Color32::GRAY,
                                egui::RichText::new(&title),
                                false,
                            );
                            clicked |= response.clicked();
                            secondary_clicked |= response.secondary_clicked();
                        },
                    )
                };
                draw(vec![]).textures_delta.clear();
                let mut output = draw(vec![]);
                let pos = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text)
                            if text.galley.text().starts_with(&title[..5]) =>
                        {
                            Some(text.pos + egui::vec2(15.0, text.galley.size().y / 2.0))
                        }
                        _ => None,
                    })
                    .expect("task title was rendered");
                output.textures_delta.clear();
                draw(vec![egui::Event::PointerMoved(pos)])
                    .textures_delta
                    .clear();
                for pressed in [true, false] {
                    draw(vec![egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    }])
                    .textures_delta
                    .clear();
                }
                let _ = draw;
                assert_eq!(clicked, button == egui::PointerButton::Primary);
                assert_eq!(secondary_clicked, button == egui::PointerButton::Secondary);
            }
        }
    }

    #[test]
    fn sidebar_scroll_reaches_recent_rows_and_tab_controls() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "sidebar-scroll");
        app.conversations_loaded = true;
        app.conversations = (1..=100)
            .map(|id| sample_meta(id, &format!("Recent {id}")))
            .collect();
        let mut scroll_id = egui::Id::NULL;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 300.0));
        let mut draw = |app: &mut DesktopApp| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| {
                    egui::Panel::left("sidebar")
                        .default_size(230.0)
                        .show(ui, |ui| {
                            scroll_id = ui.make_persistent_id(egui::IdSalt::new("sidebar-scroll"));
                            app.sidebar(ui);
                            assert!(ui.min_rect().bottom() <= screen.bottom());
                        });
                },
            )
        };
        draw(&mut app).textures_delta.clear();
        let mut initial = draw(&mut app);
        initial.textures_delta.clear();
        assert!(
            initial.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::epaint::Shape::Text(text)
                if text.galley.text().contains("New conversation") && shape.clip_rect.intersect(screen)
                    .intersects(egui::Rect::from_min_size(text.pos, text.galley.size())))
            }),
            "open-tab row appears before history without scrolling"
        );
        let _ = draw;
        let mut state = egui::scroll_area::State::load(&ctx, scroll_id)
            .expect("Recent and open tabs share a scroll area");
        assert_eq!(state.offset.y, 0.0);
        state.offset.y = 100_000.0;
        state.store(&ctx, scroll_id);
        let mut output = None;
        for _ in 0..3 {
            output = Some(ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| {
                    egui::Panel::left("sidebar")
                        .default_size(230.0)
                        .show(ui, |ui| app.sidebar(ui));
                },
            ));
            output.as_mut().unwrap().textures_delta.clear();
        }
        let state = egui::scroll_area::State::load(&ctx, scroll_id).unwrap();
        assert!(state.offset.y > screen.height());
        assert!(state.offset.y < 100_000.0, "offset is clamped to content");
        let visible_text = |needle: &str| {
            output.as_ref().unwrap().shapes.iter().any(|shape| {
                if let egui::epaint::Shape::Text(text) = &shape.shape {
                    text.galley.text().contains(needle)
                        && shape
                            .clip_rect
                            .intersect(screen)
                            .intersects(egui::Rect::from_min_size(text.pos, text.galley.size()))
                } else {
                    false
                }
            })
        };
        assert!(visible_text("Recent 100"), "last Recent row is reachable");
        assert!(
            !visible_text("✕"),
            "open tabs are above history, not below it"
        );
        assert!(visible_text("New task"), "New stays fixed");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn narrow_render_keeps_sidebar_near_twenty_percent_and_left_aligns_long_task() {
        let ctx = egui::Context::default();
        let dir = temp_dir("sidebar-geometry");
        let path = dir.join("layout.txt");
        std::fs::write(
            &path,
            b"bone-desktop-layout v1\ndisplay 100 0 0 480 1 380\n",
        )
        .unwrap();
        let mut app = DesktopApp::open(ctx.clone(), false, Some(path));
        assert!(!app.display.sidebar_width_manual);
        app.conversations_loaded = true;
        app.conversations = vec![
            sample_meta(1, &format!("{} task", "a very long title ".repeat(20))),
            sample_meta(2, "hi"),
        ];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(840.0, 400.0));
        let mut panel_rect = egui::Rect::NOTHING;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                panel_rect = egui::Panel::left("sidebar-geometry")
                    .default_size(layout::default_sidebar_width(840.0))
                    .min_size(layout::SIDEBAR_WIDTH_MIN as f32)
                    .max_size(260.0)
                    .show(ui, |ui| app.sidebar(ui))
                    .response
                    .rect;
            },
        );
        let title = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text().contains("a very long") => {
                Some(text)
            }
            _ => None,
        });
        assert!(
            panel_rect.width() <= 260.0,
            "sidebar expanded: {panel_rect:?}"
        );
        let title = title.expect("long task title was rendered");
        assert!(
            title.pos.x <= panel_rect.left() + 12.0,
            "title is not left aligned: x={} left={}",
            title.pos.x,
            panel_rect.left()
        );
        let short_title = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "hi" => Some(text),
                _ => None,
            })
            .expect("short task title was rendered");
        assert!(
            (short_title.pos.x - title.pos.x).abs() < 1.0,
            "short and long titles must share the same left edge"
        );
        assert!(output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.text().contains('…'))
        }), "long title should be truncated");
        output.textures_delta.clear();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn restored_bottom_panel_tracks_central_pane_after_sidebar_restore() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "central-geometry");
        app.tabs[0].state.ready = true;
        let composer_id = egui::Id::new(("composer", app.tabs[0].id));
        ctx.data_mut(|data| {
            data.insert_persisted(
                composer_id,
                egui::PanelState {
                    outer_rect: egui::Rect::from_min_size(
                        egui::pos2(370.0, 280.0),
                        egui::vec2(470.0, 120.0),
                    ),
                },
            )
        });
        let central = egui::Rect::from_min_max(egui::pos2(168.0, 0.0), egui::pos2(840.0, 400.0));
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(840.0, 400.0),
                )),
                ..Default::default()
            },
            |ui| {
                ui.scope_builder(egui::UiBuilder::new().max_rect(central), |ui| {
                    app.conversation_pane(0, ui);
                });
            },
        );
        output.textures_delta.clear();
        let state = egui::PanelState::load(&ctx, composer_id).expect("composer panel state");
        assert_eq!(state.outer_rect.left(), 168.0);
        assert_eq!(state.outer_rect.right(), 840.0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn approval_actions_stay_visible_and_clickable_with_long_details() {
        for width in [320.0, 672.0, 1200.0] {
            for outcome in [CallOutcome::Approve, CallOutcome::Denied] {
                let ctx = egui::Context::default();
                let (mut app, dir) = fresh_app(&ctx, "approval-layout");
                let tab = &mut app.tabs[0];
                tab.connected = true;
                tab.state.ready = true;
                tab.state.busy = true;
                let (tx, mut rx) = mpsc::unbounded_channel();
                tab.commands = tx;
                let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 600.0));
                let render = |tab: &mut Tab, events| {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(screen),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            tab.body(ui, &theme::ThemeColors::default(), None, None, false, true);
                        },
                    );
                    output.textures_delta.clear();
                    output
                };
                // Start with the compact composer, then receive an approval.
                for _ in 0..3 {
                    render(tab, Vec::new());
                }
                tab.state.approvals.push(state::Approval {
                    id: 42,
                    name: "shell".into(),
                    summary: "A long command needing explicit approval. ".repeat(40),
                    preview: Some("echo harmless preview\n".repeat(80)),
                    blocked: None,
                });
                tab.sync_approval_nav();
                tab.approval_peek = true;
                for _ in 0..5 {
                    render(tab, Vec::new());
                }
                let output = render(tab, Vec::new());
                let mut target = None;
                for label in ["Approve", "Deny", "Stop"] {
                    let (rect, clip) = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.text() == label => Some((
                                text.galley.rect.translate(text.pos.to_vec2()),
                                shape.clip_rect,
                            )),
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("missing {label} at width {width}"));
                    assert!(
                        screen.contains_rect(rect),
                        "{label} outside window: {rect:?}"
                    );
                    assert!(
                        clip.contains_rect(rect),
                        "{label} clipped at width {width}: {rect:?}, clip={clip:?}"
                    );
                    if label
                        == if outcome == CallOutcome::Approve {
                            "Approve"
                        } else {
                            "Deny"
                        }
                    {
                        target = Some(rect.center());
                    }
                }
                let pos = target.unwrap();
                for pressed in [true, false] {
                    render(
                        tab,
                        vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    );
                }
                assert!(
                    matches!(rx.try_recv(), Ok(Command::Send(RuntimeCommand::ApprovalReply { id: 42, outcome: actual })) if actual == outcome)
                );
                assert!(tab.state.approvals.is_empty());
                std::fs::remove_dir_all(dir).unwrap();
            }
        }
    }

    #[test]
    fn next_approval_starts_with_visible_actions_after_scrolling_details() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "approval-scroll-reset");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.state.ready = true;
        tab.state.busy = true;
        for id in [42, 43] {
            tab.state.approvals.push(state::Approval {
                id,
                name: "shell".into(),
                summary: "A long command needing explicit approval. ".repeat(40),
                preview: Some("echo harmless preview\n".repeat(80)),
                blocked: None,
            });
        }
        let render = |tab: &mut Tab, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(320.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    tab.body(ui, &theme::ThemeColors::default(), None, None, false, true);
                },
            );
            output.textures_delta.clear();
            output
        };
        let visible_action = |output: &egui::FullOutput, label| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    shape.clip_rect.contains_rect(rect).then_some(rect)
                }
                _ => None,
            })
        };
        for _ in 0..5 {
            render(tab, Vec::new());
        }
        let output = render(tab, Vec::new());
        let pos = visible_action(&output, "Approve")
            .expect("initial approval action")
            .center();
        render(
            tab,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -300.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        for _ in 0..60 {
            render(tab, Vec::new());
        }
        let output = render(tab, Vec::new());
        assert!(
            visible_action(&output, "Approve").is_none(),
            "details actually scrolled past actions"
        );
        // The first request resolves while the next one remains pending.
        tab.state.answered(42);
        for _ in 0..5 {
            render(tab, Vec::new());
        }
        let output = render(tab, Vec::new());
        assert_eq!(tab.approval_nav_id, Some(43));
        assert!(visible_action(&output, "Approve").is_some());
        assert!(visible_action(&output, "Deny").is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn jump_to_latest_keeps_composer_stable_and_resumes_following() {
        for width in [320.0, 672.0, 1200.0] {
            let ctx = egui::Context::default();
            let (mut app, dir) = fresh_app(&ctx, "jump-latest-layout");
            let tab = &mut app.tabs[0];
            tab.state.ready = true;
            tab.connected = true;
            tab.composer = "Keep my draft".into();
            tab.state.rows = (0..80)
                .map(|i| ("assistant".into(), format!("History message {i}")))
                .collect();
            // An inset pane also exercises split/sidebar geometry.
            let pane = egui::Rect::from_min_size(egui::pos2(168.0, 0.0), egui::vec2(width, 700.0));
            let mut frame = 0;
            let mut render = |tab: &mut Tab, events| {
                frame += 1;
                let mut scroll_id = egui::Id::NULL;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_max(egui::Pos2::ZERO, pane.max)),
                        time: Some(frame as f64 / 60.0),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        ui.scope_builder(egui::UiBuilder::new().max_rect(pane), |ui| {
                            scroll_id =
                                ui.make_persistent_id(egui::IdSalt::new(("transcript", tab.id)));
                            tab.body(ui, &theme::ThemeColors::default(), None, None, false, true);
                        });
                    },
                );
                output.textures_delta.clear();
                let panel = egui::PanelState::load(&ctx, egui::Id::new(("composer", tab.id)))
                    .unwrap()
                    .outer_rect;
                let scroll = egui::scroll_area::State::load(&ctx, scroll_id)
                    .unwrap()
                    .offset
                    .y;
                (output, panel, scroll)
            };
            let text_rect = |output: &egui::FullOutput, label: &str| {
                output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        let rect = text.galley.rect.translate(text.pos.to_vec2());
                        assert!(
                            shape.clip_rect.expand(0.5).contains_rect(rect),
                            "{label} was clipped: rect={rect:?}, clip={:?}",
                            shape.clip_rect
                        );
                        Some(rect)
                    }
                    _ => None,
                })
            };
            for _ in 0..8 {
                render(tab, vec![]);
            }
            let (output, panel, bottom_offset) = render(tab, vec![]);
            let editor = text_rect(&output, "Keep my draft").unwrap();
            assert!(tab.stick_to_bottom);
            assert!(bottom_offset > 500.0);
            assert!(text_rect(&output, "↓ Jump to latest").is_none());

            let pointer = egui::pos2(pane.center().x, 200.0);
            render(tab, vec![egui::Event::PointerMoved(pointer)]);
            let mut jump = None;
            for frame in 0..30 {
                let events = if frame == 0 {
                    vec![egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, 240.0),
                        modifiers: egui::Modifiers::NONE,
                        phase: egui::TouchPhase::Move,
                    }]
                } else {
                    vec![]
                };
                let (output, current_panel, offset) = render(tab, events);
                assert_eq!(
                    current_panel, panel,
                    "jump button moved the panel at width {width}"
                );
                assert_eq!(text_rect(&output, "Keep my draft"), Some(editor));
                assert!(offset < bottom_offset);
                jump = text_rect(&output, "↓ Jump to latest").or(jump);
            }
            let jump = jump.expect("scrolling up reveals Jump to latest");
            assert!(pane.contains_rect(jump));
            assert!(!jump.intersects(editor));
            assert!(!tab.stick_to_bottom);
            let (_, _, reading_offset) = render(tab, vec![]);

            tab.state
                .rows
                .push(("assistant".into(), "New output while reading".into()));
            tab.has_new_output = true;
            let (output, current_panel, offset) = render(tab, vec![]);
            assert_eq!(current_panel, panel);
            assert_eq!(text_rect(&output, "Keep my draft"), Some(editor));
            assert!((offset - reading_offset).abs() < 1.0);
            let jump = text_rect(&output, "↓ New output · Jump to latest").unwrap();
            assert!(pane.contains_rect(jump));
            let pos = jump.center();
            for pressed in [true, false] {
                let (output, current_panel, _) = render(
                    tab,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
                assert_eq!(current_panel, panel);
                assert_eq!(text_rect(&output, "Keep my draft"), Some(editor));
            }
            for _ in 0..3 {
                let (output, current_panel, offset) = render(tab, vec![]);
                assert!(
                    tab.stick_to_bottom,
                    "clicking Jump to latest resumes following"
                );
                assert!(!tab.has_new_output);
                assert_eq!(current_panel, panel);
                assert_eq!(text_rect(&output, "Keep my draft"), Some(editor));
                assert!(text_rect(&output, "↓ Jump to latest").is_none());
                assert!(text_rect(&output, "↓ New output · Jump to latest").is_none());
                assert!(offset > bottom_offset);
            }
            tab.state
                .rows
                .push(("assistant".into(), "Following new output again".into()));
            for _ in 0..3 {
                render(tab, vec![]);
            }
            assert!(tab.stick_to_bottom);
            assert_eq!(tab.composer, "Keep my draft");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn composer_surface_is_centered_bounded_and_keeps_primary_actions() {
        for width in [320.0, 672.0, 1200.0] {
            for busy in [false, true] {
                let ctx = egui::Context::default();
                let (mut app, dir) = fresh_app(&ctx, "composer-surface");
                let tab = &mut app.tabs[0];
                tab.state.ready = true;
                tab.connected = true;
                tab.state.busy = busy;
                tab.composer = "Review the current changes".into();
                let colors = theme::ThemeColors::default();
                for frame in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 700.0),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            tab.body(
                                ui,
                                &colors,
                                None,
                                Some((
                                    "Model: Example model".into(),
                                    "Example".into(),
                                    colors.tool_call,
                                )),
                                false,
                                true,
                            );
                        },
                    );
                    output.textures_delta.clear();
                    // Bottom panels settle their content-measured height on the next frame.
                    if frame < 2 {
                        continue;
                    }
                    let surface = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Rect(rect)
                                if rect.corner_radius == egui::CornerRadius::same(6) =>
                            {
                                assert_eq!(rect.stroke.width, 1.0);
                                Some(rect.rect)
                            }
                            _ => None,
                        })
                        .expect("rounded composer surface");
                    let expected =
                        width.min(theme::CHAT_WIDTH) - 2.0 * f32::from(theme::CHAT_PADDING);
                    assert!(
                        (surface.width() - expected).abs() < 1.0,
                        "{surface:?}, width={width}, busy={busy}"
                    );
                    assert!((surface.center().x - width * 0.5).abs() < 1.0);
                    let texts: Vec<_> = output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) => Some(text.galley.text()),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(texts.contains(&"Send"), !busy);
                    assert_eq!(texts.contains(&"Stop"), busy);
                    assert_eq!(texts.contains(&"Steer"), busy);
                    assert_eq!(texts.contains(&"Queue"), busy);
                    let panel =
                        egui::PanelState::load(&ctx, egui::Id::new(("composer", tab.id))).unwrap();
                    assert_eq!(panel.outer_rect.right(), width);
                    let max_height = if width >= 672.0 { 140.0 } else { 240.0 };
                    assert!(
                        panel.outer_rect.height() < max_height,
                        "composer should stay compact: {:?}, width={width}, busy={busy}",
                        panel.outer_rect
                    );
                }
                std::fs::remove_dir_all(dir).unwrap();
            }
        }
    }

    /// The live running command, token usage, and turn timing moved out of the
    /// toolbar: the command renders inside the transcript and usage/timing sit
    /// below the composer on the same line.
    #[test]
    fn running_command_and_token_usage_leave_the_toolbar() {
        fn screen(frame: usize) -> egui::RawInput {
            egui::RawInput {
                time: Some(frame as f64 * 0.016),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                ..Default::default()
            }
        }
        fn texts(output: &egui::FullOutput) -> Vec<String> {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.text().to_string()),
                    _ => None,
                })
                .collect()
        }

        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "header-activity");
        let colors = theme::ThemeColors::default();
        {
            let tab = &mut app.tabs[0];
            tab.state.ready = true;
            tab.connected = true;
            tab.state.busy = true;
            tab.state.status = "running shell: cargo test".into();
            tab.state.work_elapsed_ms = Some(65_000);
            tab.state.token_usage = Some(state::TokenUsage {
                sent: 1_200,
                received: 800,
                context_length: 4_000,
            });
        }
        // The live command, usage, and turn timing all left the header.
        let mut header = Vec::new();
        for frame in 0..3 {
            let mut out = ctx.run_ui(screen(frame), |ui| {
                egui::Panel::top("toolbar").show(ui, |ui| app.toolbar(ui));
            });
            out.textures_delta.clear();
            header = texts(&out);
        }
        assert!(
            !header.iter().any(|t| t.contains("worked for")),
            "turn timing left the header: {header:?}"
        );
        assert!(
            !header.iter().any(|t| t.contains("running shell")),
            "the live command left the header: {header:?}"
        );
        assert!(
            !header.iter().any(|t| t.contains("curr ")),
            "token usage left the header: {header:?}"
        );
        // Usage and turn timing now render below the composer box inside the
        // bottom panel, on the same line.
        let mut out = None;
        for frame in 0..3 {
            let mut frame_out = ctx.run_ui(screen(frame), |ui| {
                app.tabs[0].body(ui, &colors, None, None, false, true);
            });
            frame_out.textures_delta.clear();
            out = Some(frame_out);
        }
        let out = out.unwrap();
        let mut composer_bottom = f32::NEG_INFINITY;
        let mut detail_y = None;
        let mut detail_text = None;
        for shape in &out.shapes {
            match &shape.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.corner_radius == egui::CornerRadius::same(6) =>
                {
                    composer_bottom = composer_bottom.max(rect.rect.bottom());
                }
                egui::epaint::Shape::Text(text) if text.galley.text().starts_with("curr ") => {
                    detail_y = Some(text.pos.y + text.galley.size().y * 0.5);
                    detail_text = Some(text.galley.text().to_string());
                }
                _ => {}
            }
        }
        let detail_text = detail_text
            .unwrap_or_else(|| panic!("token usage renders below the composer: {:?}", texts(&out)));
        assert!(
            detail_text.contains("worked for 1:05"),
            "turn timing shares the usage line: {detail_text:?}"
        );
        let detail_y = detail_y.expect("usage line renders below the composer box");
        assert!(
            detail_y > composer_bottom,
            "usage must sit below the composer box ({detail_y} vs {composer_bottom})"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The header is a text-only menu bar: it names the menus and no longer
    /// shows the active conversation's title or its workspace folder.
    #[test]
    fn header_is_a_menu_bar_without_task_title_or_workspace() {
        fn screen() -> egui::RawInput {
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                ..Default::default()
            }
        }
        fn texts(output: &egui::FullOutput) -> Vec<String> {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.text().to_string()),
                    _ => None,
                })
                .collect()
        }

        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "header-menu-bar");
        app.tabs[0].conversation_id = Some(7);
        app.tabs[0].workspace = "/home/user/project".into();
        app.tabs[0].saved_title = Some((7, "Refactor the parser".into()));
        let mut header = Vec::new();
        for _ in 0..3 {
            let mut out = ctx.run_ui(screen(), |ui| {
                egui::Panel::top("toolbar").show(ui, |ui| app.toolbar(ui));
            });
            out.textures_delta.clear();
            header = texts(&out);
        }
        for menu in ["Layout", "Tools: ask", "Changes", "More"] {
            assert!(
                header.iter().any(|t| t == menu),
                "menu bar shows `{menu}`: {header:?}"
            );
        }
        assert!(
            !header.iter().any(|t| t.contains("Refactor the parser")),
            "the task title left the header: {header:?}"
        );
        assert!(
            !header.iter().any(|t| t.contains("project")),
            "the workspace folder left the header: {header:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The text-only header menus must actually drop their contents when their
    /// labels are clicked. This guards the `MenuBar` wiring: a label that only
    /// highlights on hover (dead click) would leave the dropdowns unusable.
    #[test]
    fn header_menu_labels_open_their_dropdowns() {
        fn screen() -> egui::RawInput {
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                ..Default::default()
            }
        }
        fn texts(output: &egui::FullOutput) -> Vec<String> {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.text().to_string()),
                    _ => None,
                })
                .collect()
        }
        fn label_center(output: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
        }

        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "header-menu-open");
        let render = |app: &mut DesktopApp, events: Vec<egui::Event>| {
            let mut input = screen();
            input.events = events;
            let mut out = ctx.run_ui(input, |ui| {
                egui::Panel::top("toolbar").show(ui, |ui| app.toolbar(ui));
            });
            out.textures_delta.clear();
            out
        };
        let click = |app: &mut DesktopApp, pos: egui::Pos2| {
            render(
                app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
        };

        // Settle the header so label positions are stable before clicking.
        let mut settled = render(&mut app, Vec::new());
        for _ in 0..2 {
            settled = render(&mut app, Vec::new());
        }
        let closed = texts(&settled);
        assert!(
            !closed.iter().any(|t| t.contains("New window"))
                && !closed.iter().any(|t| t.contains("Tool permissions")),
            "menus start closed before any click: {closed:?}"
        );

        // `Layout` drops the window/split menu.
        let layout = label_center(&settled, "Layout").expect("Layout label is visible");
        click(&mut app, layout);
        let mut open = Vec::new();
        for _ in 0..3 {
            open = texts(&render(&mut app, vec![egui::Event::PointerMoved(layout)]));
            if open.iter().any(|t| t.contains("New window")) {
                break;
            }
        }
        assert!(
            open.iter().any(|t| t.contains("New window")),
            "clicking Layout opens its menu: {open:?}"
        );

        // `Tools` drops the shared tool-permission menu.
        let tools = label_center(&settled, "Tools: ask").expect("Tools label is visible");
        click(&mut app, tools);
        let mut open = Vec::new();
        for _ in 0..3 {
            open = texts(&render(&mut app, vec![egui::Event::PointerMoved(tools)]));
            if open.iter().any(|t| t.contains("Tool permissions")) {
                break;
            }
        }
        assert!(
            open.iter().any(|t| t.contains("Tool permissions")),
            "clicking Tools opens its menu: {open:?}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn full_desktop_layout_keeps_central_pane_at_sidebar_edge() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "full-central-geometry");
        app.tabs[0].state.ready = true;
        app.tabs[0].state.push_row("user", "mixed transcript row");
        app.conversations_loaded = true;
        app.conversations = vec![
            sample_meta(7, "Readable task list entry"),
            sample_meta(8, "hi"),
            sample_meta(9, &"Long task title ".repeat(30)),
        ];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(840.0, 400.0));
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                egui::Panel::top("toolbar").show(ui, |ui| app.toolbar(ui));
                let plan = layout::responsive_plan(840.0);
                let sidebar = egui::Panel::left("sidebar")
                    .resizable(true)
                    .default_size(layout::default_sidebar_width(840.0))
                    .min_size(layout::SIDEBAR_WIDTH_MIN as f32)
                    .max_size(plan.sidebar_cap)
                    .show(ui, |ui| app.sidebar(ui));
                app.conversation_pane(0, ui);
                let composer =
                    egui::PanelState::load(ui.ctx(), egui::Id::new(("composer", app.tabs[0].id)))
                        .expect("composer panel state");
                assert!(
                    (sidebar.response.rect.width() - layout::default_sidebar_width(840.0)).abs()
                        < 1.0,
                    "sidebar contents expanded beyond its painted width: {:?}",
                    sidebar.response.rect
                );
                assert_eq!(composer.outer_rect.left(), sidebar.response.rect.right());
            },
        );
        output.textures_delta.clear();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sidebar_shows_open_and_recent_task_hierarchy() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "flat-sidebar");
        app.tabs[0].conversation_id = Some(7);
        app.conversations_loaded = true;
        app.conversations = vec![sample_meta(7, "Duplicate open row"), sample_meta(9, "")];
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 400.0));
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                egui::Panel::left("sidebar")
                    .default_size(230.0)
                    .show(ui, |ui| app.sidebar(ui));
            },
        );
        output.textures_delta.clear();
        let texts: Vec<String> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect();
        assert!(
            texts.iter().any(|t| t == "New task"),
            "prominent New task action is rendered: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Search tasks")),
            "search box is rendered: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "Open") && texts.iter().any(|t| t == "Recent"),
            "task sections are rendered: {texts:?}"
        );
        assert_eq!(
            texts
                .iter()
                .filter(|t| t.contains("Duplicate open row"))
                .count(),
            1,
            "open title appears once: {texts:?}"
        );
        assert!(texts.iter().any(|t| t == "Task 9"), "{texts:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_task_indicator_spins_while_running_and_warns_when_awaiting_user() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "status-indicator");
        let tab = &mut app.tabs[0];
        tab.connected = true;
        tab.connecting = false;
        tab.state.last_error = None;
        assert_eq!(tab.status_indicator().0, RowIndicator::None);
        // Live and running: an animated spinner instead of a static dot.
        tab.state.busy = true;
        assert_eq!(tab.status_indicator().0, RowIndicator::Spinner);
        // Awaiting the user (a pending tool approval): a warning, even while the
        // turn still reports itself busy.
        tab.state.approvals.push(state::Approval {
            id: 1,
            name: "run".into(),
            summary: "wants to run a command".into(),
            preview: None,
            blocked: None,
        });
        assert!(tab.state.busy);
        assert_eq!(
            tab.status_indicator().0,
            RowIndicator::Glyph("!"),
            "a waiting conversation shows an attention indicator"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sidebar_draws_spinner_while_running_and_warning_when_awaiting_user() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "status-render");
        app.tabs[0].connected = true;
        app.tabs[0].connecting = false;
        app.tabs[0].state.busy = true;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(320.0, 400.0));
        let render = |app: &mut DesktopApp| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| {
                    egui::Panel::left("sidebar")
                        .default_size(230.0)
                        .show(ui, |ui| app.sidebar(ui));
                },
            )
        };
        let texts = |output: &egui::FullOutput| -> Vec<String> {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.text().to_string()),
                    _ => None,
                })
                .collect()
        };
        // A running conversation animates a spinner (a multi-point open path)
        // instead of the old static "●" dot.
        let mut output = render(&mut app);
        output.textures_delta.clear();
        assert!(
            output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::epaint::Shape::Path(path) if path.points.len() >= 8 && !path.closed)),
            "running conversation draws a spinner path"
        );
        assert!(
            !texts(&output).iter().any(|t| t.contains('●')),
            "the static running dot is replaced"
        );
        // Awaiting the user: a warning replaces the spinner.
        app.tabs[0].state.approvals.push(state::Approval {
            id: 1,
            name: "run".into(),
            summary: "wants to run a command".into(),
            preview: None,
            blocked: None,
        });
        let mut output = render(&mut app);
        output.textures_delta.clear();
        assert!(
            texts(&output).iter().any(|t| t.contains('!')),
            "awaiting conversation shows an attention indicator: {:?}",
            texts(&output)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_tab_title_derives_from_composer_draft() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "draft-title");
        // A bare new tab keeps the default label.
        assert_eq!(app.tabs[0].title(), "New conversation");
        // A draft derives a meaningful, whitespace-collapsed label.
        app.tabs[0].composer = "  fix\nthe login  page ".into();
        assert_eq!(app.tabs[0].title(), "fix the login page");
        // Long drafts truncate like history-derived titles.
        app.tabs[0].composer = "x".repeat(80);
        let title = app.tabs[0].title();
        assert_eq!(title.chars().count(), 47);
        assert!(title.ends_with('…'));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reconnect_with_live_peer_stays_bounded() {
        for partial_recovery in [false, true] {
            let ctx = egui::Context::default();
            let (mut app, dir) = fresh_app(&ctx, &format!("partial-{partial_recovery}"));
            app.address = "127.0.0.1:17000".into();
            app.add_tab(Intent::Load(7), &ctx);
            for tab in &mut app.tabs {
                tab.connected = true;
                tab.connecting = false;
            }
            app.pump_daemon(&ctx);
            app.tabs[1].connected = false;
            app.tabs[1].mid_drop = true;
            if partial_recovery {
                app.tabs[0].connected = false;
                app.tabs[0].mid_drop = true;
            }
            app.pump_daemon(&ctx);
            assert_eq!(app.reconnect_budget, Some(daemon::MAX_RECONNECT_ROUNDS));
            assert!(app.retry_at.is_some());
            for round in 1..=daemon::MAX_RECONNECT_ROUNDS {
                app.retry_at = Some(Instant::now() - Duration::from_millis(1));
                app.pump_daemon(&ctx);
                assert!(app.tabs[1].connecting);
                if partial_recovery && round == 1 {
                    assert!(app.tabs[0].connecting);
                    app.tabs[0].connected = true;
                    app.tabs[0].connecting = false;
                }
                assert!(!app.tabs[0].connecting, "live peers are not reconnected");
                // A success while the other attempt is still in flight must
                // neither clear nor replenish the remaining budget.
                app.pump_daemon(&ctx);
                assert_eq!(
                    app.reconnect_budget,
                    Some(daemon::MAX_RECONNECT_ROUNDS - round)
                );
                app.tabs[1].connecting = false;
                app.tabs[1].connect_failed_refused = true;
                app.pump_daemon(&ctx);
                assert_eq!(app.retry_at.is_some(), round < daemon::MAX_RECONNECT_ROUNDS);
            }
            assert_eq!(app.daemon_phase, daemon::Phase::Ready);
            assert!(app.daemon_notice.contains("could not restore it"));
            for _ in 0..10 {
                app.tabs[1].connect_failed_refused = true;
                app.pump_daemon(&ctx);
                assert_eq!(app.reconnect_budget, Some(0));
                assert!(app.retry_at.is_none());
                assert!(!app.daemon_notice.is_empty());
            }
            app.tabs[1].connected = true;
            app.pump_daemon(&ctx);
            assert_eq!(app.reconnect_budget, None);
            assert!(app.daemon_notice.is_empty());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn reconnect_ignores_excluded_tabs_and_stale_failures() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "excluded-recovery");
        app.tabs[0].connected = true;
        for id in 1..=4 {
            app.add_tab(Intent::Load(id), &ctx);
        }
        app.tabs[1].auto = false;
        app.tabs[2].demo = true;
        app.tabs[3].closing = true;
        app.tabs[4].remove = true;
        for tab in &mut app.tabs {
            tab.mid_drop = true;
            tab.connect_failed_refused = true;
            tab.connect_failed_other = true;
        }
        app.pump_daemon(&ctx);
        assert_eq!(app.daemon_phase, daemon::Phase::Ready);
        assert_eq!(app.reconnect_budget, None);
        assert!(app.retry_at.is_none());
        assert!(
            app.tabs.iter().all(|tab| !tab.mid_drop
                && !tab.connect_failed_refused
                && !tab.connect_failed_other)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn partial_startup_success_preserves_pending_retry() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "partial-startup");
        app.address = "127.0.0.1:17000".into();
        app.add_tab(Intent::Load(7), &ctx);
        app.daemon_phase = daemon::Phase::Starting { attempts: 1 };
        app.retry_at = Some(Instant::now() - Duration::from_millis(1));
        app.tabs[0].connected = true;
        app.tabs[1].connecting = false;
        app.pump_daemon(&ctx);
        assert_eq!(app.daemon_phase, daemon::Phase::Ready);
        assert!(app.tabs[1].connecting);
        assert_eq!(app.reconnect_budget, Some(daemon::MAX_RECONNECT_ROUNDS - 1));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn idle_ready_app_never_schedules_retries() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "mididle");
        app.tabs[0].connected = true;
        for _ in 0..10 {
            app.pump_daemon(&ctx);
            assert!(
                app.retry_at.is_none(),
                "an idle Ready session must never arm a retry"
            );
            assert_eq!(app.daemon_phase, daemon::Phase::Ready);
            assert_eq!(app.reconnect_budget, None);
        }
        // A deliberate disconnect (no mid-drop flag) stays quiet too.
        app.tabs[0].connected = false;
        app.tabs[0].auto = false;
        app.pump_daemon(&ctx);
        assert!(app.retry_at.is_none());
        assert!(!app.tabs[0].connecting);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A child process that exits immediately, standing in for a crashed
    /// app-spawned daemon.
    fn spawn_dead_process() -> std::process::Child {
        let child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/c", "exit", "0"])
                .spawn()
        } else {
            std::process::Command::new("sh")
                .args(["-c", "exit 0"])
                .spawn()
        };
        child.expect("spawn a dead process for the death tests")
    }

    #[test]
    fn spawned_daemon_death_after_ready_triggers_respawn() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "deathready");
        app.daemon_bin = Some(dir.join("definitely-missing-daemon"));
        app.daemon_phase = daemon::Phase::Ready;
        app.daemon_pid = Some(424242);
        app.daemon_child = Some(spawn_dead_process());
        app.tabs[0].connected = true;
        std::thread::sleep(Duration::from_millis(200)); // let it exit
        app.pump_daemon(&ctx);
        assert!(app.daemon_child.is_none(), "the dead child must be reaped");
        assert!(!app.tabs[0].connected, "a dead daemon's sockets are dead");
        // The respawn attempt used the (missing) injected binary and failed
        // cleanly into Stopped.
        match &app.daemon_phase {
            daemon::Phase::Stopped(message) => {
                assert!(
                    message.contains("Could not start the local daemon"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected Stopped, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn spawned_daemon_death_before_ready_is_stopped() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "deathboot");
        app.daemon_phase = daemon::Phase::Starting { attempts: 3 };
        app.daemon_pid = Some(424242);
        app.daemon_child = Some(spawn_dead_process());
        std::thread::sleep(Duration::from_millis(200)); // let it exit
        app.pump_daemon(&ctx);
        assert!(app.daemon_child.is_none());
        match &app.daemon_phase {
            daemon::Phase::Stopped(message) => {
                assert!(
                    message.contains("exited before accepting connections"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected Stopped, got {other:?}"),
        }
        assert!(app.retry_at.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn host_api_version_mismatch_surfaces_notice_and_match_clears() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "apiver");
        // Matching version: no notice.
        app.tabs[0].host_api_version = bone_protocol::HOST_API_VERSION;
        app.check_host_api_versions();
        assert!(app.version_notice.is_empty());
        // A different version: a warning naming the daemon's version.
        app.tabs[0].host_api_version = bone_protocol::HOST_API_VERSION + 1;
        app.check_host_api_versions();
        assert!(
            app.version_notice
                .contains(&(bone_protocol::HOST_API_VERSION + 1).to_string()),
            "mismatch must name the daemon version: {}",
            app.version_notice
        );
        // No version observed yet (0): no notice.
        app.tabs[0].host_api_version = 0;
        app.check_host_api_versions();
        assert!(app.version_notice.is_empty());
        // A FrontendState event records the daemon's version on the tab.
        app.tabs[0].handle_event(Event::Runtime(RuntimeEvent::FrontendState {
            banner: String::new(),
            settings: serde_json::json!({}),
            commands: vec![],
            tool_defs: vec![],
            tool_display: serde_json::json!({}),
            subagents: vec![],
            host_api_version: 7,
            catalog_updates: 0,
            cwd: Some("/workspace/project".into()),
        }));
        assert_eq!(app.tabs[0].workspace, "/workspace/project");
        assert_eq!(app.tabs[0].host_api_version, 7);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
