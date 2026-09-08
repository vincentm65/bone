//! Multi-conversation desktop frontend: each tab owns one conversation socket,
//! its own reducer state, composer, and virtualized transcript cache. Only the
//! daemon is
//! authoritative; this UI renders RuntimeEvents and sends RuntimeCommands.
mod connection;
mod daemon;
mod layout;
mod markdown;
#[cfg(test)]
mod perf_tests;
mod state;
mod transcript;

use bone_protocol::RuntimeCommand;
use bone_protocol::tools::CallOutcome;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    New,
    Load(i64),
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
    connection_status: String,
    state: State,
    composer: String,
    stick_to_bottom: bool,
    transcript: transcript::Cache,
    repair_at: Instant,
    commands: mpsc::UnboundedSender<Command>,
    events: mpsc::Receiver<Event>,
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
            connection_status: "Disconnected".into(),
            state: State::default(),
            composer: String::new(),
            stick_to_bottom: true,
            transcript: transcript::Cache::new(),
            repair_at: Instant::now(),
            commands,
            events,
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
        if self.command(RuntimeCommand::SubmitPrompt {
            request_id: Some(request_id),
            text: self.composer.clone(),
            images: vec![],
        }) {
            self.composer.clear();
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
        self.connected && self.state.ready && !self.state.busy && !self.composer.trim().is_empty()
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
    load_field: String,
    sidebar_notice: String,
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
            load_field: String::new(),
            sidebar_notice: String::new(),
            next_tab_id: 1,
            layout_path,
            layout_dirty_since: None,
            daemon_phase: daemon::Phase::Probe,
            retry_at: None,
            daemon_bin: None,
            daemon_pid: None,
            daemon_notice: String::new(),
            show_server: false,
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
            connection_status: "Demo mode".into(),
            state: State::default(),
            composer: String::new(),
            stick_to_bottom: true,
            transcript: transcript::Cache::new(),
            repair_at: Instant::now(),
            commands,
            events,
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
        for tab in &mut self.tabs {
            layout_changed |= tab.drain_events(ctx);
        }
        if layout_changed {
            self.note_layout_change(ctx);
        }
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
        let address = self.effective_address();
        let bin = match self
            .daemon_bin
            .clone()
            .or_else(daemon::resolve_binary)
        {
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
            Ok(pid) => {
                self.daemon_pid = Some(pid);
                self.daemon_phase = daemon::Phase::Starting { attempts: 0 };
                self.daemon_notice.clear();
                self.schedule_retry(
                    ctx,
                    Duration::from_millis(daemon::DAEMON_RETRY_DELAY_MS),
                );
                true
            }
            Err(error) => {
                let message = format!(
                    "Could not start the local daemon: {error} (see the Server dialog)"
                );
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
                tab.auto && !tab.demo && !tab.connected && !tab.connecting && !tab.closing && !tab.remove
            };
            if should {
                self.connect_index(i);
            }
        }
    }

    /// Frame pump for the daemon lifecycle. Runs after events are drained:
    /// - Any tab connected means the daemon is reachable: mark Ready.
    /// - A refused connect on a loopback address with no daemon yet starts one.
    /// - Retry ticks reconnect auto tabs while the spawned daemon boots.
    fn pump_daemon(&mut self, ctx: &egui::Context) {
        if self.demo || self.tabs.is_empty() {
            return;
        }
        // Ready as soon as any tab connects; that also cancels pending retries.
        if self.tabs.iter().any(|tab| tab.connected) {
            if self.daemon_phase != daemon::Phase::Ready {
                self.daemon_phase = daemon::Phase::Ready;
                self.daemon_notice.clear();
            }
            self.retry_at = None;
            return;
        }
        // Consume failure flags raised while draining events.
        let mut refused = false;
        let mut other_failure = false;
        for tab in &mut self.tabs {
            refused |= std::mem::take(&mut tab.connect_failed_refused);
            other_failure |= std::mem::take(&mut tab.connect_failed_other);
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
                    self.daemon_phase = daemon::Phase::Starting { attempts: attempts + 1 };
                    self.reconnect_auto_tabs();
                }
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
                    if daemon::is_loopback(&address) {
                        let _ = self.start_local_daemon(ctx);
                    } else {
                        let message = format!(
                            "No daemon responding at {address}. Use Server… to connect to a remote daemon."
                        );
                        self.daemon_phase = daemon::Phase::Stopped(message.clone());
                        self.daemon_notice = message;
                    }
                }
                daemon::Phase::Starting { .. } => {
                    self.schedule_retry(
                        ctx,
                        Duration::from_millis(daemon::DAEMON_RETRY_DELAY_MS),
                    );
                }
            }
        }
    }

    /// Toolbar pill describing the daemon target.
    fn daemon_pill(&self) -> (String, egui::Color32) {
        if self.demo {
            return ("Demo".into(), egui::Color32::from_rgb(140, 170, 255));
        }
        match &self.daemon_phase {
            daemon::Phase::Ready => ("● Local daemon".into(), egui::Color32::from_rgb(110, 200, 120)),
            daemon::Phase::Probe | daemon::Phase::Starting { .. } => (
                "… Starting daemon".into(),
                egui::Color32::from_rgb(235, 190, 80),
            ),
            daemon::Phase::Stopped(_) => ("Daemon offline".into(), egui::Color32::from_rgb(235, 90, 90)),
        }
    }

    fn close_tab(&mut self, index: usize) {
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
        ui.horizontal(|ui| {
            ui.label("Load");
            ui.add(
                egui::TextEdit::singleline(&mut self.load_field)
                    .desired_width(64.0)
                    .hint_text("ID"),
            );
            if ui.button("Open").clicked() {
                match self.load_field.trim().parse::<i64>() {
                    Ok(id) if id > 0 => {
                        self.add_tab(Intent::Load(id), &ctx);
                        self.sidebar_notice.clear();
                        self.note_layout_change(&ctx);
                        if !self.demo {
                            self.connect_index(self.selected);
                        }
                    }
                    _ => self.sidebar_notice = "Conversation ID must be a positive integer".into(),
                }
            }
        });
        if !self.sidebar_notice.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(235, 90, 90), &self.sidebar_notice);
        }
        ui.separator();
        let mut open: Option<usize> = None;
        let mut close: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("tab-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
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
            });
        if let Some(i) = open {
            self.selected = i;
            self.note_layout_change(&ctx);
        }
        if let Some(i) = close {
            self.close_tab(i);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("Bone Desktop");
            ui.separator();
            let (pill, color) = self.daemon_pill();
            ui.colored_label(color, pill);
            if !self.demo {
                ui.separator();
                if ui.button("Server…").clicked() {
                    self.show_server = true;
                }
            }
            ui.separator();
            ui.label("Zoom");
            let mut zoom = ui.ctx().zoom_factor();
            ui.add(egui::Slider::new(&mut zoom, 0.75..=2.0).fixed_decimals(2));
            if (zoom - ui.ctx().zoom_factor()).abs() > f32::EPSILON {
                ui.ctx().set_zoom_factor(zoom);
            }
        });
        // Per-tab status line.
        if let Some(tab) = self.tabs.get(self.selected) {
            ui.horizontal_wrapped(|ui| {
                let mut label = format!("Socket: {}", tab.connection_status);
                ui.label(&label);
                if !tab.demo {
                    label = format!("Turn: {}", tab.state.status);
                    ui.label(&label);
                }
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
                    "Bone Desktop talks to a `bone serve` daemon over TCP. For a \
                     local (127.0.0.1 / localhost) address, a missing daemon is \
                     started automatically. A remote address is never auto-started.",
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Address");
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.address).desired_width(180.0),
                    );
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
                    ui.colored_label(
                        egui::Color32::from_rgb(235, 90, 90),
                        &self.daemon_notice,
                    );
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
}

impl Tab {
    fn composer_panel(&mut self, ui: &mut egui::Ui) -> bool {
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
        let editor = ui.add(
            egui::TextEdit::multiline(&mut self.composer)
                .desired_width(f32::INFINITY)
                .desired_rows(3)
                .hint_text("Message (Ctrl+Enter to send)"),
        );
        let mut changed = editor.changed();
        let shortcut = editor.has_focus()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter));
        ui.horizontal(|ui| {
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
                    egui::Button::new("Cancel"),
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

        // Reconcile each tab's virtualization cache with its authoritative row
        // vector before anything draws (changed rows are drained per frame).
        for tab in &mut self.tabs {
            let rows_len = tab.state.rows.len();
            let changed = std::mem::take(&mut tab.state.changed_rows);
            tab.transcript.sync(rows_len, &changed);
        }

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(230.0)
            .min_size(150.0)
            .show(ui, |ui| self.sidebar(ui));

        self.server_dialog(ui.ctx());

        if self.tabs.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("Create or load a conversation from the sidebar.");
            });
            return;
        }
        if self.selected >= self.tabs.len() {
            self.selected = self.tabs.len() - 1;
        }
        let changed = {
            let tab = &mut self.tabs[self.selected];
            tab.body(ui)
        };
        if changed {
            self.note_layout_change(ui.ctx());
        }
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
}
