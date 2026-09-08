//! Multi-conversation desktop frontend: each tab owns one conversation socket,
//! its own reducer state, composer, and virtualized transcript cache. Only the
//! daemon is
//! authoritative; this UI renders RuntimeEvents and sends RuntimeCommands.
mod connection;
mod daemon;
mod images;
mod layout;
mod markdown;
#[cfg(test)]
mod perf_tests;
mod setup;
mod state;
mod transcript;
#[cfg(test)]
mod ux_tests;

use base64::Engine;
use bone_protocol::tools::CallOutcome;
use bone_protocol::{
    ConfigSnapshot, ConversationMeta, HostRequest, HostResponse, ImageData, ProviderUpdate,
    RuntimeCommand, RuntimeEvent,
};
use connection::{Command, Event};
use eframe::egui;
use state::State;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Bound event-drain work per tab per frame while the worker's bounded channel
/// applies backpressure.
const MAX_EVENTS_PER_FRAME: usize = 256;

/// Minimum idle time after a layout-affecting change before the restart
/// layout file is rewritten.
const LAYOUT_SAVE_DEBOUNCE_MS: u64 = 400;

/// Maximum size of a single image attachment (bytes).
const MAX_ATTACHMENT_BYTES: usize = 15 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    New,
    Load(i64),
}

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
    fn to_image_data(self) -> ImageData {
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
    /// Images staged in the composer to send with the next prompt.
    attachments: Vec<Attachment>,
    stick_to_bottom: bool,
    jump_to_latest: bool,
    has_new_output: bool,
    transcript: transcript::Cache,
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
    /// Each snapshot carries whether the daemon flagged it restart-required.
    config_snapshots: Vec<(ConfigSnapshot, bool)>,
    config_rejections: Vec<String>,
}

impl Tab {
    fn new(id: u64, intent: Intent, ctx: &egui::Context) -> Self {
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
            attachments: Vec::new(),
            stick_to_bottom: true,
            jump_to_latest: false,
            has_new_output: false,
            transcript: transcript::Cache::new(),
            repair_at: Instant::now(),
            commands,
            events,
            host_responses: Vec::new(),
            config_snapshots: Vec::new(),
            config_rejections: Vec::new(),
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
        self.image_cache.reset();
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

    fn send_prompt(&mut self) {
        if !self.can_send() {
            return;
        }
        let request_id = self.state.next_id();
        let images: Vec<ImageData> = self
            .attachments
            .iter()
            .cloned()
            .map(Attachment::to_image_data)
            .collect();
        if self.command(RuntimeCommand::SubmitPrompt {
            request_id: Some(request_id),
            text: self.composer.clone(),
            images,
        }) {
            self.composer.clear();
            self.attachments.clear();
            self.state.busy = true;
            self.state.status = "Sending…".into();
            // Started, not the click, creates the authoritative user row.
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
                self.state.approvals.clear();
                self.connection_status = reason.clone();
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
                // picker); buffer them until `drain_all`.
                match &event {
                    RuntimeEvent::ConfigSnapshot { snapshot, .. } => {
                        self.config_snapshots.push((snapshot.clone(), false));
                    }
                    RuntimeEvent::ConfigChanged {
                        snapshot,
                        restart_required,
                        ..
                    } => {
                        self.config_snapshots
                            .push((snapshot.clone(), *restart_required));
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
                    _ => {}
                }
                let before = self.conversation_id;
                if let Some(command) = self.state.reduce(event) {
                    self.command(command);
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

    /// Sidebar status glyph and its color for this tab's row.
    fn status_glyph(&self) -> (&'static str, egui::Color32) {
        if self.demo {
            return ("◆", egui::Color32::from_rgb(140, 170, 255));
        }
        if self.state.last_error.is_some() {
            return ("✕", egui::Color32::from_rgb(235, 90, 90));
        }
        if self.connecting {
            return ("…", egui::Color32::from_rgb(235, 190, 80));
        }
        if !self.connected {
            return ("", egui::Color32::GRAY);
        }
        if self.state.needs_approval() {
            return ("⚠", egui::Color32::from_rgb(235, 190, 60));
        }
        if self.state.busy {
            return ("●", egui::Color32::from_rgb(110, 200, 120));
        }
        ("✓", egui::Color32::from_rgb(150, 150, 150))
    }

    fn title(&self) -> String {
        if self.demo {
            return "Demo".into();
        }
        self.state.short_title()
    }
}

struct DesktopApp {
    /// Demo mode: self-contained seeded transcript, no daemon, no persistence.
    demo: bool,
    address: String,
    tabs: Vec<Tab>,
    selected: usize,
    sidebar_notice: String,
    history_search: String,
    last_pointer: Option<egui::Pos2>,
    focused_pane: Option<u64>,
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
    /// True once a conversation list has been received (or the daemon reported
    /// an error); a manual refresh clears it to refetch.
    conversations_loaded: bool,
    /// Latest daemon config snapshot for the provider/model picker. `GetConfig`
    /// responses are broadcast (not request-correlated), so any connected tab
    /// can refresh it; a rejected mutation clears it to force a refetch.
    config: Option<ConfigSnapshot>,
    /// Id of the tab that issued the pending `GetConfig`; `None` while none is
    /// in flight. Abandoned if that tab loses its socket.
    config_request: Option<u64>,
    /// Provider/model dialog open flag.
    show_provider: bool,
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
    /// Editable model text in the provider dialog.
    model_field: String,
    /// Provider id the `model_field` currently displays; a different active
    /// provider resyncs the field from the snapshot.
    model_field_provider: String,
    /// Transient message inside the provider dialog (switch in flight, a
    /// daemon rejection, or a restart-required flag).
    provider_notice: String,
    /// Conversation id being renamed inline in the sidebar; `None` when idle.
    rename_target: Option<i64>,
    /// Working title shown in the sidebar while `rename_target` is set.
    rename_field: String,
    /// (id, title) of the conversation awaiting the inline delete confirmation.
    delete_target: Option<(i64, String)>,
    /// Whether the split view (second tab pane on the right) is active.
    split: bool,
    /// Tab index rendered in the split pane; never the left-selected tab.
    split_tab: usize,
    /// Live display settings (zoom, split, pane widths), mirrored into the
    /// restart layout on change and restored from it on launch.
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
}

impl DesktopApp {
    fn new(ctx: egui::Context) -> Self {
        let demo = std::env::var_os("BONE_DESKTOP_DEMO").is_some();
        let layout_path = if demo { None } else { layout::state_path() };
        let mut app = Self::open(ctx.clone(), demo, layout_path);
        app.begin();
        app
    }

    /// Build the app from an explicit layout path (`None` disables persistence),
    /// restoring the saved tabs/drafts/order when present. Split out so tests
    /// can drive the restore without mutating process-global environment.
    fn open(ctx: egui::Context, demo: bool, layout_path: Option<PathBuf>) -> Self {
        let mut app = Self {
            demo,
            address: daemon::DEFAULT_ADDRESS.into(),
            tabs: Vec::new(),
            selected: 0,
            sidebar_notice: String::new(),
            history_search: String::new(),
            last_pointer: None,
            focused_pane: None,
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
            conversations_loaded: false,
            config: None,
            config_request: None,
            show_provider: false,
            show_setup: false,
            setup_ui: setup::SetupUi::default(),
            setup_request: None,
            setup_offered: false,
            model_field: String::new(),
            model_field_provider: String::new(),
            provider_notice: String::new(),
            rename_target: None,
            rename_field: String::new(),
            delete_target: None,
            split: false,
            split_tab: 0,
            display: layout::Preferences::default(),
            mutation_target: None,
            reconnect_budget: None,
            daemon_child: None,
            version_notice: String::new(),
            close_target: None,
        };
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
                    // Restore display settings: zoom applies to the egui
                    // context, pane widths seed the panel default sizes, and
                    // the split view is re-validated against the tab list.
                    let prefs = layout.preferences;
                    ctx.set_zoom_factor((prefs.zoom_percent as f32 / 100.0).clamp(0.75, 2.0));
                    app.display = prefs;
                    app.split = prefs.split;
                    app.split_tab = prefs.split_tab;
                    app.reconcile_split();
                }
            },
        }
        app
    }

    fn add_tab(&mut self, intent: Intent, ctx: &egui::Context) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab::new(id, intent, ctx));
        self.selected = self.tabs.len() - 1;
    }

    /// Seed a self-contained transcript so the renderer can be exercised (and
    /// screenshotted) without a running daemon.
    fn add_demo_tab(&mut self, ctx: &egui::Context) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let (commands, events) = connection::spawn(ctx.clone());
        let mut tab = Tab {
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
            attachments: Vec::new(),
            stick_to_bottom: true,
            jump_to_latest: false,
            has_new_output: false,
            transcript: transcript::Cache::new(),
            repair_at: Instant::now(),
            commands,
            events,
            host_responses: Vec::new(),
            config_snapshots: Vec::new(),
            config_rejections: Vec::new(),
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
        tab.state.ready = true;
        tab.state.status = "Demo mode (BONE_DESKTOP_DEMO)".into();
        self.tabs.push(tab);
        self.selected = self.tabs.len() - 1;
    }

    /// Remove acknowledged-closed tabs. Returns whether any tab was removed so
    /// the caller can mark the restart layout dirty.
    fn prune_closed(&mut self) -> bool {
        let mut remove_at = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            if tab.remove {
                remove_at.push(i);
            }
        }
        if remove_at.is_empty() {
            return false;
        }
        let mut removed_before_selected = 0;
        for i in &remove_at {
            if *i < self.selected {
                removed_before_selected += 1;
            }
        }
        self.tabs.retain(|tab| !tab.remove);
        if self.tabs.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.saturating_sub(removed_before_selected);
            if self.selected >= self.tabs.len() {
                self.selected = self.tabs.len() - 1;
            }
        }
        true
    }

    fn drain_all(&mut self, ctx: &egui::Context) {
        let mut layout_changed = false;
        let mut host_responses: Vec<(u64, u64, HostResponse)> = Vec::new();
        let mut config_snapshots: Vec<(ConfigSnapshot, bool)> = Vec::new();
        let mut config_rejections: Vec<String> = Vec::new();
        for tab in &mut self.tabs {
            layout_changed |= tab.drain_events(ctx);
            for (request_id, response) in tab.host_responses.drain(..) {
                host_responses.push((tab.id, request_id, response));
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
        for (tab_id, request_id, response) in host_responses {
            if self
                .conversations_request
                .is_some_and(|(t, id)| t == tab_id && id == request_id)
            {
                self.conversations_request = None;
                self.apply_host_response(response);
            } else if self
                .setup_request
                .is_some_and(|(t, id)| t == tab_id && id == request_id)
            {
                self.setup_request = None;
                self.apply_setup_response(response);
            }
        }
        for snapshot in config_snapshots {
            self.apply_config_snapshot(snapshot);
        }
        for rejection in config_rejections {
            self.apply_config_rejection(rejection);
        }
        if layout_changed {
            self.note_layout_change(ctx);
        }
        self.poll_conversations();
        self.poll_config();
        self.poll_setup();
        self.check_host_api_versions();
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
    fn apply_config_snapshot(&mut self, (snapshot, restart_required): (ConfigSnapshot, bool)) {
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
        self.config = None;
        self.config_request = None;
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
        let Some(index) = self.tabs.iter().position(|tab| tab.connected && !tab.demo) else {
            return false;
        };
        let sent = self.tabs[index].command(RuntimeCommand::GetConfig);
        if sent {
            let tab_id = self.tabs[index].id;
            self.config_request = Some(tab_id);
        }
        sent
    }

    /// Persistently switch the daemon's active provider (TUI `/provider`
    /// equivalent) using the latest snapshot's revision for conflict checks.
    fn switch_provider(&mut self, id: &str) -> bool {
        let Some(config) = self.config.as_ref() else {
            return false;
        };
        if config.active_provider == id {
            return false;
        }
        let Some(index) = self.tabs.iter().position(|tab| tab.connected && !tab.demo) else {
            return false;
        };
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

    /// Persist the edited model on the active provider, preserving every other
    /// configured field (omitted `ProviderUpdate` options keep their values).
    fn save_model(&mut self, model: String) -> bool {
        let Some(config) = self.config.as_ref().cloned() else {
            return false;
        };
        let Some(provider) = config
            .providers
            .iter()
            .find(|p| p.id == config.active_provider)
            .cloned()
        else {
            return false;
        };
        if provider.model == model {
            return false;
        }
        let Some(index) = self.tabs.iter().position(|tab| tab.connected && !tab.demo) else {
            return false;
        };
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
            fast_mode: None, // Omitted: preserve current value.
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

    /// Apply a correlated host response to the conversation picker state.
    fn apply_host_response(&mut self, response: HostResponse) {
        match response {
            HostResponse::Conversations(conversations) => {
                self.conversations = conversations;
                self.conversations_loaded = true;
                self.mutation_target = None;
            }
            HostResponse::Error { message, .. } => {
                // Latch the failure like a successful list: otherwise
                // `poll_conversations` re-issues the request on every error
                // round trip while the daemon stays broken. ↻ clears this.
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
        let Some(index) = self.tabs.iter().position(|tab| tab.connected && !tab.demo) else {
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
        let Some(index) = self.tabs.iter().position(|tab| tab.connected && !tab.demo) else {
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
        let Some(index) = self.tabs.iter().position(|tab| tab.connected && !tab.demo) else {
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
            self.mutation_target = match &request {
                HostRequest::ConversationRename { id, .. }
                | HostRequest::ConversationDelete { id, .. } => Some(*id),
                _ => None,
            };
        }
        sent
    }

    /// Begin an inline rename of conversation `id`, seeded with its current
    /// title. Cancels any pending delete confirmation.
    fn start_rename(&mut self, id: i64) {
        let Some(title) = self
            .conversations
            .iter()
            .find(|m| m.id == id)
            .map(|m| {
                if m.full_title.is_empty() {
                    m.title.clone()
                } else {
                    m.full_title.clone()
                }
            })
        else {
            return;
        };
        self.rename_field = title;
        self.delete_target = None;
        self.rename_target = Some(id);
        self.sidebar_notice.clear();
    }

    fn cancel_rename(&mut self) {
        self.rename_target = None;
        self.rename_field.clear();
    }

    /// Commit the inline rename. A blank title is refused locally; a refused
    /// send (no connection or a fetch already in flight) keeps the field open.
    fn commit_rename(&mut self) {
        let Some(id) = self.rename_target else {
            return;
        };
        let title = self.rename_field.trim().to_owned();
        if title.is_empty() {
            self.sidebar_notice = "A conversation title cannot be blank.".into();
            return;
        }
        if self.request_conversation_mutation(HostRequest::ConversationRename {
            id,
            title,
            limit: 0,
        }) {
            self.rename_target = None;
            self.rename_field.clear();
        } else {
            self.sidebar_notice = "Cannot rename right now: no daemon connection or a \
                                  conversation update is already in flight."
                .into();
        }
    }

    /// Arm the inline delete confirmation for conversation `id`. Refused while
    /// a tab still has that conversation open.
    fn start_delete(&mut self, id: i64) {
        if self
            .tabs
            .iter()
            .any(|tab| !tab.demo && tab.conversation_id == Some(id))
        {
            self.sidebar_notice = "Close the tab for that conversation before deleting it.".into();
            return;
        }
        let title = self
            .conversations
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.title.clone())
            .unwrap_or_default();
        self.rename_target = None;
        self.rename_field.clear();
        self.delete_target = Some((id, title));
        self.sidebar_notice.clear();
    }

    fn cancel_delete(&mut self) {
        self.delete_target = None;
    }

    /// Commit the inline delete confirmation.
    fn commit_delete(&mut self) {
        let Some((id, _)) = self.delete_target.take() else {
            return;
        };
        if !self.request_conversation_mutation(HostRequest::ConversationDelete { id, limit: 0 }) {
            self.sidebar_notice = "Cannot delete right now: no daemon connection or a \
                                  conversation update is already in flight."
                .into();
        }
    }

    /// Select the tab already attached to `id`, or open (and connect) a fresh
    /// Load tab for it.
    fn open_conversation(&mut self, id: i64, ctx: &egui::Context) {
        if let Some(existing) = self
            .tabs
            .iter()
            .position(|tab| !tab.demo && tab.conversation_id == Some(id))
        {
            self.selected = existing;
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
        layout::Layout {
            address: self.address.clone(),
            selected: self.selected.min(self.tabs.len().saturating_sub(1)),
            tabs: self
                .tabs
                .iter()
                .filter(|tab| !tab.demo)
                .map(|tab| layout::TabState {
                    conversation_id: tab.conversation_id,
                    draft: tab.composer.clone(),
                })
                .collect(),
            preferences: self.display,
        }
    }

    /// Mirror live display state (context zoom, split, resolved pane widths)
    /// into the persisted preferences, marking the layout dirty on change.
    /// A `None` pane width keeps the previously persisted value (the split
    /// pane is not rendered while the split view is off).
    fn sync_display(
        &mut self,
        ctx: &egui::Context,
        sidebar_width: Option<f32>,
        split_width: Option<f32>,
    ) {
        let mut next = self.display;
        next.zoom_percent =
            ((ctx.zoom_factor() * 100.0).round() as u16).clamp(layout::ZOOM_MIN, layout::ZOOM_MAX);
        next.split = self.split;
        next.split_tab = self.split_tab;
        if let Some(width) = sidebar_width {
            next.sidebar_width = (width
                .clamp(
                    layout::SIDEBAR_WIDTH_MIN as f32,
                    layout::SIDEBAR_WIDTH_MAX as f32,
                )
                .round() as u16)
                .clamp(layout::SIDEBAR_WIDTH_MIN, layout::SIDEBAR_WIDTH_MAX);
        }
        if let Some(width) = split_width {
            next.split_width = (width
                .clamp(
                    layout::SPLIT_WIDTH_MIN as f32,
                    layout::SPLIT_WIDTH_MAX as f32,
                )
                .round() as u16)
                .clamp(layout::SPLIT_WIDTH_MIN, layout::SPLIT_WIDTH_MAX);
        }
        if next != self.display {
            self.display = next;
            self.note_layout_change(ctx);
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
        if let Some(at) = self.retry_at {
            if Instant::now() >= at {
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
        }
        // A drop of an established session — or a fresh connect failure while
        // Ready — starts a bounded reconnect reusing the retry machinery.
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

    /// Toolbar pill describing the active provider and model.
    fn model_pill(&self) -> (String, egui::Color32) {
        let Some(config) = self.config.as_ref() else {
            return ("Model …".into(), egui::Color32::from_rgb(235, 190, 80));
        };
        match config
            .providers
            .iter()
            .find(|p| p.id == config.active_provider)
        {
            Some(provider) => (
                format!("Default model · {} · {}", provider.label, provider.model),
                egui::Color32::from_rgb(110, 200, 120),
            ),
            None => (
                "⚙ No active provider".into(),
                egui::Color32::from_rgb(235, 90, 90),
            ),
        }
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
        if tab.state.busy || !tab.composer.is_empty() || !tab.attachments.is_empty() {
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
        let draft =
            !self.tabs[index].composer.is_empty() || !self.tabs[index].attachments.is_empty();
        let response = egui::Modal::new(egui::Id::new("confirm-close-tab")).show(ctx, |ui| {
            ui.heading("Close conversation?");
            ui.label(self.tabs[index].title());
            if busy {
                ui.label("This conversation is still working. Closing will stop its current turn.");
            }
            if draft {
                ui.label("Your unsent draft and attachments will be discarded.");
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
                if self.tabs.is_empty() {
                    false
                } else {
                    let index = self
                        .focused_pane
                        .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
                        .filter(|index| {
                            *index == self.selected || (self.split && *index == self.split_tab)
                        })
                        .unwrap_or(self.selected);
                    self.close_tab(index);
                    true
                }
            }
            egui::Key::PageUp => {
                if self.tabs.is_empty() {
                    return false;
                }
                // Wrap: from the first tab jump to the last.
                self.selected = if self.selected == 0 {
                    self.tabs.len() - 1
                } else {
                    self.selected - 1
                };
                true
            }
            egui::Key::PageDown => {
                if self.tabs.is_empty() {
                    return false;
                }
                // Wrap: from the last tab jump to the first.
                self.selected = (self.selected + 1) % self.tabs.len();
                true
            }
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
                if index < self.tabs.len() {
                    self.selected = index;
                    true
                } else {
                    false
                }
            }
            egui::Key::Backslash => {
                self.split = !self.split;
                self.reconcile_split();
                true
            }
            _ => false,
        }
    }

    /// Consume the Ctrl+key shortcuts (new tab, close tab, tab selection,
    /// split toggle) regardless of which widget has focus.
    fn handle_shortcuts(&mut self, ui: &mut egui::Ui) {
        if self.close_target.is_some()
            || self.show_provider
            || self.show_server
            || self.show_setup
            || !ui.input(|i| i.modifiers.command)
        {
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

    /// Keep the split pane valid: clamp its index, never point it at the
    /// left-selected tab (one transcript cache must not render in two places
    /// at once), and disable the split when fewer than two tabs remain.
    fn reconcile_split(&mut self) {
        if self.tabs.len() < 2 {
            self.split = false;
            return;
        }
        if self.selected >= self.tabs.len() {
            self.selected = self.tabs.len() - 1;
        }
        if self.split_tab >= self.tabs.len() {
            self.split_tab = 0;
        }
        if self.split_tab == self.selected {
            self.split_tab = (self.split_tab + 1) % self.tabs.len();
        }
    }

    /// Tab strip at the top of the split pane: choose which tab renders
    /// there. The left-selected tab is skipped because its transcript is
    /// already rendered in the left pane.
    fn split_header(&mut self, ui: &mut egui::Ui) {
        let mut pick: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            for i in 0..self.tabs.len() {
                if i == self.selected {
                    continue;
                }
                let title = self.tabs[i].title();
                if ui
                    .selectable_label(self.split_tab == i, egui::RichText::new(title).small())
                    .clicked()
                {
                    pick = Some(i);
                }
            }
        });
        if let Some(i) = pick {
            self.split_tab = i;
        }
        ui.separator();
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading("Conversations");
        let ctx = ui.ctx().clone();
        if ui.button("+ New conversation").clicked() {
            self.add_tab(Intent::New, &ctx);
            self.sidebar_notice.clear();
            self.note_layout_change(&ctx);
            if !self.demo {
                self.connect_index(self.selected);
            }
        }
        egui::ScrollArea::vertical()
            .id_salt("sidebar-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| self.sidebar_lists(ui));
    }

    fn sidebar_lists(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.open_tabs(ui);
        ui.separator();
        if !self.demo {
            ui.horizontal(|ui| {
                ui.strong("Recent");
                if ui
                    .small_button("↻")
                    .on_hover_text("Refresh the conversation list")
                    .clicked()
                {
                    self.conversations_request = None;
                    self.conversations_loaded = false;
                }
            });
            ui.add(
                egui::TextEdit::singleline(&mut self.history_search)
                    .hint_text("Search conversation titles")
                    .desired_width(f32::INFINITY),
            );
            let search = self.history_search.trim().to_lowercase();
            let mut open_conversation: Option<i64> = None;
            match (self.conversations_request, self.conversations_loaded) {
                (Some((_, request_id)), _) => {
                    ui.weak(format!("Loading… (request #{request_id})"));
                }
                (None, false) => {
                    ui.weak("Waiting for the daemon…");
                }
                (None, true) if self.conversations.is_empty() => {
                    ui.weak("No conversations yet.");
                }
                (None, true) => {
                    let mut rename_start: Option<i64> = None;
                    let mut rename_commit = false;
                    let mut rename_cancel = false;
                    let mut delete_start: Option<i64> = None;
                    let mut delete_commit = false;
                    let mut delete_cancel = false;
                    for meta in &self.conversations {
                        if !meta.title.to_lowercase().contains(&search)
                            && self.rename_target != Some(meta.id)
                            && !self.delete_target.as_ref().is_some_and(|t| t.0 == meta.id)
                        {
                            continue;
                        }
                        if self.rename_target == Some(meta.id) {
                            // Inline rename: an edit field plus save/cancel.
                            ui.horizontal(|ui| {
                                let field = ui.add(
                                    egui::TextEdit::singleline(&mut self.rename_field)
                                        .desired_width(150.0)
                                        .hint_text("Title"),
                                );
                                if ui
                                    .small_button("✓")
                                    .on_hover_text("Save the new title (Enter)")
                                    .clicked()
                                {
                                    rename_commit = true;
                                }
                                if ui
                                    .small_button("✕")
                                    .on_hover_text("Cancel the rename (Escape)")
                                    .clicked()
                                {
                                    rename_cancel = true;
                                }
                                // Enter/Escape fire on the frame the field
                                // loses focus, so both keys commit/cancel.
                                if field.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                {
                                    rename_commit = true;
                                } else if field.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Escape))
                                {
                                    rename_cancel = true;
                                }
                            });
                        } else if self.delete_target.as_ref().is_some_and(|t| t.0 == meta.id) {
                            // Inline delete confirmation replaces the row.
                            let title = self
                                .delete_target
                                .as_ref()
                                .map(|(_, t)| t.clone())
                                .unwrap_or_default();
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    egui::Color32::from_rgb(235, 90, 90),
                                    format!("Delete \"{title}\"?"),
                                );
                                if ui
                                    .small_button("Yes")
                                    .on_hover_text(
                                        "Delete this conversation and all of its messages",
                                    )
                                    .clicked()
                                {
                                    delete_commit = true;
                                }
                                if ui.small_button("No").clicked() {
                                    delete_cancel = true;
                                }
                            });
                        } else {
                            let open = self
                                .tabs
                                .iter()
                                .any(|tab| !tab.demo && tab.conversation_id == Some(meta.id));
                            let glyph = if open { "✓ " } else { "" };
                            ui.horizontal(|ui| {
                                if ui
                                    .selectable_label(
                                        false,
                                        egui::RichText::new(format!("{glyph}{}", meta.title)),
                                    )
                                    .clicked()
                                {
                                    open_conversation = Some(meta.id);
                                }
                                ui.push_id(meta.id, |ui| {
                                    ui.menu_button("…", |ui| {
                                        if ui.button("Rename…").clicked() {
                                            rename_start = Some(meta.id);
                                            ui.close();
                                        }
                                        if ui.button("Delete…").clicked() {
                                            delete_start = Some(meta.id);
                                            ui.close();
                                        }
                                    });
                                });
                            });
                            let when: String = meta.updated_at.chars().take(16).collect();
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} messages · {when}",
                                    meta.message_count
                                ))
                                .small()
                                .weak(),
                            );
                        }
                    }
                    // Apply the row actions after the loop so the borrows of
                    // `self.conversations`/`self.tabs` above have ended.
                    if let Some(id) = rename_start {
                        self.start_rename(id);
                    } else if rename_cancel {
                        self.cancel_rename();
                    } else if rename_commit {
                        self.commit_rename();
                    }
                    if let Some(id) = delete_start {
                        self.start_delete(id);
                    } else if delete_cancel {
                        self.cancel_delete();
                    } else if delete_commit {
                        self.commit_delete();
                    }
                }
            }
            if let Some(id) = open_conversation {
                self.open_conversation(id, &ctx);
            }
        }
        if !self.sidebar_notice.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(235, 90, 90), &self.sidebar_notice);
        }
    }

    fn open_tabs(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.strong("Open conversations");
        let mut open: Option<usize> = None;
        let mut close: Option<usize> = None;
        for i in 0..self.tabs.len() {
            let tab = &self.tabs[i];
            let (glyph, color) = tab.status_glyph();
            let glyph = if glyph.is_empty() {
                String::new()
            } else {
                format!("{glyph} ")
            };
            let title = tab.title();
            ui.horizontal(|ui| {
                let selected = self.selected == i;
                let label = egui::RichText::new(format!("{glyph}{title}")).color(color);
                if ui.selectable_label(selected, label).clicked() {
                    open = Some(i);
                }
                if !tab.demo && ui.small_button("✕").clicked() {
                    close = Some(i);
                }
            });
        }
        if let Some(i) = open {
            self.selected = i;
            self.note_layout_change(&ctx);
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
    }

    fn conversation_pane(&mut self, index: usize, ui: &mut egui::Ui) -> bool {
        let inside = self
            .last_pointer
            .is_some_and(|pos| ui.available_rect_before_wrap().contains(pos));
        let blocked = self.close_target.is_some()
            || self.show_server
            || self.show_provider
            || self.show_setup;
        if inside && !blocked {
            if ui.input(|i| i.pointer.any_pressed()) {
                self.focused_pane = Some(self.tabs[index].id);
            }
            if ui.input(|i| !i.raw.hovered_files.is_empty()) {
                ui.label("Drop images into this conversation");
            }
            let dropped = ui.input(|i| i.raw.dropped_files.clone());
            if !dropped.is_empty() {
                self.tabs[index].apply_drops(&dropped);
            }
        }
        let tab = &mut self.tabs[index];
        let changed = ui.push_id(tab.id, |ui| tab.body(ui)).inner;
        // Render this pane's larger image preview, if one is selected. The id
        // is pane-specific so the left and split previews cannot collide.
        tab.image_cache
            .show_preview(ui.ctx(), ("preview", tab.id, index));
        changed
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Bone Desktop");
            ui.separator();
            let (pill, color) = self.daemon_pill();
            ui.colored_label(color, pill);
            if !self.demo {
                ui.separator();
                let (pill, color) = self.model_pill();
                if ui
                    .button(egui::RichText::new(pill).color(color))
                    .on_hover_text("Choose the provider and model")
                    .clicked()
                {
                    self.show_provider = true;
                }
                ui.separator();
                if ui
                    .add_enabled(
                        self.tabs.len() >= 2,
                        egui::Button::selectable(self.split, "⧉ Split"),
                    )
                    .on_hover_text("Show a second conversation beside this one (Ctrl+\\)")
                    .clicked()
                {
                    self.split = !self.split;
                    self.reconcile_split();
                }
            }
            ui.separator();
            ui.menu_button("View & settings", |ui| {
                if !self.demo && ui.button("Server & connection…").clicked() {
                    self.show_server = true;
                    ui.close();
                }
                if !self.demo && ui.button("Provider setup…").clicked() {
                    self.show_setup = true;
                    ui.close();
                }
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
            });
        });
        if let Some(tab) = self.tabs.get(self.selected) {
            ui.horizontal_wrapped(|ui| {
                ui.strong(tab.title());
                let workspace = if tab.workspace.is_empty() { "Workspace unknown — waiting for daemon" } else { &tab.workspace };
                ui.label(workspace).on_hover_text("Daemon working directory. Conversations on this daemon share this workspace; use separate worktrees for concurrent edits.");
            });
        }
        // Per-tab status line.
        if let Some(tab) = self.tabs.get(self.selected) {
            ui.horizontal_wrapped(|ui| {
                ui.label(&tab.state.status);
                if let Some(error) = &tab.state.last_error {
                    ui.colored_label(
                        egui::Color32::from_rgb(235, 90, 90),
                        format!("Error: {error}"),
                    );
                }
            });
        }
        if !self.demo && !self.daemon_notice.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(235, 90, 90),
                format!("Daemon: {}", self.daemon_notice),
            );
        }
        if !self.demo && !self.version_notice.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(235, 190, 80),
                self.version_notice.clone(),
            );
        }
    }

    /// Advanced connection dialog: address editing, daemon status, and manual
    /// connect/disconnect. The local (loopback) case is normally automatic, so
    /// this is only for remote daemons and manual control.
    fn server_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || !self.show_server {
            return;
        }
        let mut open = self.show_server;
        egui::Window::new("Server")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::RIGHT_TOP, [-12.0, 8.0])
            .show(ctx, |ui| {
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

    /// Provider/model dialog: lists the daemon's providers (active one
    /// highlighted), persists a provider switch, and edits the active
    /// provider's model. Changes apply daemon-wide, like TUI `/provider`.
    fn provider_dialog(&mut self, ctx: &egui::Context) {
        if self.demo || !self.show_provider {
            return;
        }
        let mut open = self.show_provider;
        egui::Window::new("Model / Provider")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::RIGHT_TOP, [-12.0, 8.0])
            .show(ctx, |ui| {
                ui.label(
                    "Pick the provider the daemon uses for new turns. Changes \
                     are saved to the daemon's configuration and apply to all \
                     Bone clients.",
                );
                ui.add_space(6.0);
                // Clone the snapshot so the dialog can mutate `self` (switch,
                // save, refetch) while still reading the list.
                let Some(config) = self.config.clone() else {
                    ui.weak("Waiting for the daemon…");
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("↻")
                            .on_hover_text("Refetch the configuration")
                            .clicked()
                        {
                            self.config_request = None;
                        }
                    });
                    if !self.provider_notice.is_empty() {
                        ui.colored_label(
                            egui::Color32::from_rgb(235, 90, 90),
                            &self.provider_notice,
                        );
                    }
                    return;
                };
                if config.providers.is_empty() {
                    ui.weak("No providers configured on this daemon.");
                }
                for provider in &config.providers {
                    let active = provider.id == config.active_provider;
                    let marker = if active { "● " } else { "○ " };
                    if ui
                        .selectable_label(
                            active,
                            egui::RichText::new(format!(
                                "{marker}{} — {}",
                                provider.label, provider.model
                            )),
                        )
                        .on_hover_text(if provider.api_key_configured {
                            "API key configured"
                        } else {
                            "API key not configured"
                        })
                        .clicked()
                    {
                        if !active {
                            self.switch_provider(&provider.id);
                        }
                    }
                }
                if let Some(provider) = config
                    .providers
                    .iter()
                    .find(|p| p.id == config.active_provider)
                {
                    ui.separator();
                    // Resync the edit field when the active provider changes.
                    if self.model_field_provider != provider.id {
                        self.model_field_provider = provider.id.clone();
                        self.model_field = provider.model.clone();
                    }
                    let any_connected = self.tabs.iter().any(|tab| tab.connected && !tab.demo);
                    ui.horizontal(|ui| {
                        ui.label("Model");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.model_field).desired_width(180.0),
                        );
                        let dirty = !self.model_field.trim().is_empty()
                            && self.model_field != provider.model;
                        if ui
                            .add_enabled(dirty && any_connected, egui::Button::new("Save model"))
                            .clicked()
                        {
                            self.save_model(self.model_field.clone());
                        }
                    });
                }
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.weak(format!("Config rev {}", config.revision));
                    if ui
                        .small_button("↻")
                        .on_hover_text("Refetch the configuration")
                        .clicked()
                    {
                        self.config = None;
                        self.config_request = None;
                    }
                });
                if !self.provider_notice.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(235, 90, 90), &self.provider_notice);
                }
            });
        if !open {
            self.show_provider = false;
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
        egui::Window::new("Provider setup")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 40.0])
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                let outcome = self.setup_ui.render(ui);
                close_requested = outcome.close;
                if let Some(request) = outcome.request {
                    if !self.send_setup_request(request) {
                        self.setup_ui.abort();
                    }
                }
            });
        if close_requested {
            open = false;
        }
        self.show_setup = open;
        if !open {
            self.setup_ui.close();
            self.setup_request = None;
        }
    }
}

impl Tab {
    fn composer_panel(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.stick_to_bottom {
            ui.horizontal(|ui| {
                if ui
                    .button(if self.has_new_output {
                        "↓ New output · Jump to latest"
                    } else {
                        "↓ Jump to latest"
                    })
                    .clicked()
                {
                    self.jump_to_latest = true;
                }
                ui.weak("Reading earlier messages");
            });
        }
        egui::ScrollArea::vertical()
            .id_salt(("approvals", self.id))
            .max_height(150.0)
            .show(ui, |ui| {
                let mut reply = None;
                for approval in &self.state.approvals {
                    ui.group(|ui| {
                        ui.strong(format!("Approval required: {}", approval.name));
                        ui.label(&approval.summary);
                        if let Some(blocked) = &approval.blocked {
                            ui.colored_label(egui::Color32::YELLOW, blocked);
                        }
                        if let Some(preview) = &approval.preview {
                            ui.collapsing(format!("Preview #{}", approval.id), |ui| {
                                ui.monospace(preview);
                            });
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    self.connected && approval.blocked.is_none(),
                                    egui::Button::new("Approve"),
                                )
                                .clicked()
                            {
                                reply = Some((approval.id, CallOutcome::Approve));
                            }
                            if ui
                                .add_enabled(self.connected, egui::Button::new("Deny"))
                                .clicked()
                            {
                                reply = Some((approval.id, CallOutcome::Denied));
                            }
                        });
                    });
                }
                if let Some((id, outcome)) = reply {
                    if self.command(RuntimeCommand::ApprovalReply { id, outcome }) {
                        self.state.answered(id);
                    }
                }
            });
        if let Some(error) = &self.state.last_error {
            ui.colored_label(egui::Color32::from_rgb(235, 90, 90), error);
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
        let editor = ui.add(
            egui::TextEdit::multiline(&mut self.composer)
                .desired_width(f32::INFINITY)
                .desired_rows(3)
                .hint_text("Message (Ctrl/Cmd+Enter to send) · 📎 or drop an image"),
        );
        let mut changed = editor.changed() || attachment_removed;
        let shortcut = editor.has_focus()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        ui.horizontal(|ui| {
            if !self.demo
                && ui
                    .add_enabled(!self.state.busy, egui::Button::new("📎"))
                    .on_hover_text("Attach an image (png, jpeg, webp, gif)")
                    .clicked()
            {
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
                                self.state.last_error =
                                    Some(format!("Could not read {path:?}: {error}"));
                            }
                        }
                    }
                    Ok(None) => {} // User cancelled the picker.
                    Err(error) => self.state.last_error = Some(error),
                }
            }
            if ui
                .add_enabled(self.can_send(), egui::Button::new("Send"))
                .clicked()
                || shortcut
            {
                self.send_prompt();
                changed = true;
            }
            if ui
                .add_enabled(
                    self.connected && self.state.ready && self.state.busy,
                    egui::Button::new("Stop generating"),
                )
                .clicked()
            {
                self.command(RuntimeCommand::Cancel);
                self.state.status = "Cancelling…".into();
            }
            ui.weak(format!(
                "Conversation {}",
                self.conversation_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "new…".into())
            ));
        });
        changed
    }

    /// Returns true when the composer draft changed this frame (typed or sent),
    /// so the caller can mark the restart layout dirty.
    fn body(&mut self, ui: &mut egui::Ui) -> bool {
        // Reserve the composer area before laying out history so long
        // transcripts cannot push it out of the window.
        let composer_changed = egui::Panel::bottom("composer")
            .show(ui, |ui| self.composer_panel(ui))
            .inner;

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
        let available_height = ui.available_size().y;
        // Rows and tool cards are only borrowed while this call lays out the
        // visible band; cached measurements live in self.transcript.
        let response = self.transcript.show(
            ui,
            self.id,
            self.stick_to_bottom,
            &self.state.rows,
            &self.state.toolcards,
        );
        // Keep following the bottom until the user scrolls away from it.
        let max_scroll = (response.content_size.y - available_height).max(0.0);
        let at_bottom = (max_scroll - response.state.offset.y).abs() < 8.0;
        self.stick_to_bottom = at_bottom;
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.flush_layout(ui.ctx());
        if self.prune_closed() {
            self.note_layout_change(ui.ctx());
        }
        self.drain_all(ui.ctx());
        if self.prune_closed() {
            self.note_layout_change(ui.ctx());
        }
        // Coordinate the local-daemon lifecycle against connect outcomes and
        // retry ticks collected while draining events above.
        self.pump_daemon(ui.ctx());
        // Ctrl-shortcuts act on the tabs as they stand after the drain; the
        // split pane stays valid across the tab opens/closes they may cause.
        self.handle_shortcuts(ui);
        self.reconcile_split();

        // Reconcile each tab's virtualization cache with its authoritative row
        // vector before anything draws (changed rows are drained per frame).
        for tab in &mut self.tabs {
            let rows_len = tab.state.rows.len();
            let changed = std::mem::take(&mut tab.state.changed_rows);
            if !tab.stick_to_bottom && !changed.is_empty() {
                tab.has_new_output = true;
            }
            tab.transcript.sync(rows_len, &changed);
        }

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        let sidebar = egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(self.display.sidebar_width as f32)
            .min_size(150.0)
            .show(ui, |ui| self.sidebar(ui));
        let sidebar_width = sidebar.response.rect.max.x;

        self.server_dialog(ui.ctx());
        self.provider_dialog(ui.ctx());
        self.setup_dialog(ui.ctx());
        self.close_dialog(ui.ctx());

        if self.tabs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("Create or load a conversation from the sidebar.");
            });
            self.sync_display(ui.ctx(), Some(sidebar_width), None);
            return;
        }
        if self.selected >= self.tabs.len() {
            self.selected = self.tabs.len() - 1;
        }
        // Retain the last drag position: some platforms clear it on the drop frame.
        if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
            self.last_pointer = Some(pos);
        }
        let split_width = if self.split {
            // Reserve and render the right pane first so the left transcript
            // measures its area against the remaining central space.
            let split_response = egui::Panel::right("split-pane")
                .resizable(true)
                .default_size(self.display.split_width as f32)
                .min_size(220.0)
                .show(ui, |ui| {
                    self.split_header(ui);
                    self.conversation_pane(self.split_tab, ui)
                });
            let split_changed = split_response.inner;
            let changed = self.conversation_pane(self.selected, ui);
            if changed || split_changed {
                self.note_layout_change(ui.ctx());
            }
            Some(split_response.response.rect.width())
        } else {
            let changed = self.conversation_pane(self.selected, ui);
            if changed {
                self.note_layout_change(ui.ctx());
            }
            None
        };
        self.sync_display(ui.ctx(), Some(sidebar_width), split_width);
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "Bone Desktop",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1000.0, 720.0])
                .with_min_inner_size([520.0, 400.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(DesktopApp::new(cc.egui_ctx.clone())))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    /// Isolated state dir per test (tests run in parallel threads).
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
    fn open_restores_display_preferences_and_split() {
        let dir = temp_dir("prefs");
        let mut layout = sample_layout();
        layout.preferences = layout::Preferences {
            zoom_percent: 125,
            split: true,
            split_tab: 1,
            sidebar_width: 300,
            split_width: 500,
        };
        let path = dir.join("layout.txt");
        layout::save(&path, &layout).unwrap();

        let ctx = egui::Context::default();
        let app = DesktopApp::open(ctx.clone(), false, Some(path.clone()));
        assert_eq!(app.display, layout.preferences);
        assert!(app.split);
        assert_eq!(app.split_tab, 1);
        // egui applies a pending zoom at the start of the next pass, so run
        // one headless pass before reading the factor back.
        ctx.begin_pass(egui::RawInput::default());
        let output = ctx.end_pass();
        output.drop_without_applying_deltas();
        assert!((ctx.zoom_factor() - 1.25).abs() < f32::EPSILON);
        // Restoring prefs must not mark the freshly built layout dirty.
        assert!(app.layout_dirty_since.is_none());

        // A restored split with fewer than two tabs is disabled again.
        layout.tabs.truncate(1);
        layout.selected = 0;
        layout.preferences.split_tab = 0;
        layout::save(&path, &layout).unwrap();
        let app = DesktopApp::open(egui::Context::default(), false, Some(path.clone()));
        assert!(!app.split);
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
        assert_eq!(app.selected, 0);
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

    fn sample_meta(id: i64, title: &str) -> ConversationMeta {
        ConversationMeta {
            id,
            title: title.into(),
            full_title: title.into(),
            updated_at: "2026-09-07T12:30:00Z".into(),
            message_count: 4,
            provider: "desktop-fixture".into(),
            model: "mock".into(),
        }
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

    fn sample_config(revision: u64, active: &str, providers: &[(&str, &str)]) -> ConfigSnapshot {
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
            sample_config(3, "local", &[("local", "qwen3"), ("openai", "gpt-5")]),
            false,
        ));
        app.drain_all(&ctx);
        assert!(app.config.is_some());
        assert!(
            app.config_request.is_none(),
            "fetch resolved by the broadcast"
        );
        // Switching to the other provider is accepted while connected.
        assert!(app.switch_provider("openai"));
        assert!(app.provider_notice.contains("Switching provider"));
        // Switching to the already-active provider is a no-op.
        assert!(!app.switch_provider("local"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_rejection_clears_snapshot_and_shows_notice() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "cfgrej");
        app.tabs[0].connected = true;
        app.apply_config_snapshot((sample_config(1, "local", &[("local", "m")]), false));
        app.tabs[0]
            .config_rejections
            .push("revision mismatch".into());
        app.drain_all(&ctx);
        assert!(
            app.config.is_none(),
            "rejected mutation must force a refetch"
        );
        assert!(app.provider_notice.contains("revision mismatch"));
        // The drain's own poll already re-issued the fetch; no double-send.
        assert!(app.config_request.is_some());
        assert!(!app.request_config());
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
        assert!(!app.switch_provider("openai"));
        assert!(!app.save_model("gpt-5".into()));
        app.config = Some(sample_config(7, "local", &[("local", "qwen3")]));
        // Snapshot but no connected tab: still refused.
        assert!(!app.switch_provider("local"));
        assert!(!app.save_model("other".into()));
        app.tabs[0].connected = true;
        // Unchanged model is a no-op even when connected.
        assert!(!app.save_model("qwen3".into()));
        // A changed model goes out.
        assert!(app.save_model("qwen3-fast".into()));
        assert!(app.provider_notice.contains("Saving model"));
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
    fn delete_refused_while_a_tab_has_the_conversation_open() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "delopen");
        app.tabs[0].conversation_id = Some(7); // attached
        app.conversations = vec![sample_meta(7, "first prompt")];
        app.start_delete(7);
        assert!(app.delete_target.is_none(), "no confirmation armed");
        assert!(app.sidebar_notice.contains("Close the tab"));
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
        // Backslash toggles the split (needs at least two tabs).
        assert!(!app.split);
        assert!(app.apply_shortcut(egui::Key::Backslash, &ctx));
        assert!(app.split);
        assert_ne!(app.split_tab, app.selected);
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
    fn reconcile_split_stays_valid_across_selection_and_closes() {
        let ctx = egui::Context::default();
        let (mut app, dir) = fresh_app(&ctx, "splitrecon");
        app.add_tab(Intent::New, &ctx); // 2 tabs, selected 1
        app.split = true;
        app.reconcile_split();
        assert_eq!(
            app.split_tab, 0,
            "split tab must differ from the selected one"
        );
        // Selecting the other tab pushes the split tab to a different index.
        app.selected = 0;
        app.reconcile_split();
        assert_eq!(app.split_tab, 1);
        // An out-of-range index is clamped back into place.
        app.split_tab = 99;
        app.reconcile_split();
        assert_eq!(app.split_tab, 1);
        // Below two tabs the split is force-disabled.
        app.tabs.pop();
        app.selected = 0;
        app.reconcile_split();
        assert!(!app.split);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- Stage 6 slice A: reconnect and daemon hardening ---------------------

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
                if text.galley.text() == "✕" && shape.clip_rect.intersect(screen)
                    .intersects(egui::Rect::from_min_size(text.pos, text.galley.size())))
            }),
            "open-tab close control appears before history without scrolling"
        );
        drop(draw);
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
        assert!(visible_text("+ New conversation"), "New stays fixed");
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
            app.version_notice.contains("2"),
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
