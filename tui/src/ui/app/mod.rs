//! Main TUI application: event loop, state, and turn orchestration.

mod editor;
mod keymap;
mod paste;
pub mod stream;

use paste::{apply_input_key_with_paste_burst, collect_paste_burst, is_paste_burst, plain_char};

use crate::chat::Message;
use crate::config::{self, UserConfig};
use crate::llm::{ChatMessage, LlmProvider};

use crate::ext::ExtensionManager;
use crate::tools::{ApprovalMode, CallOutcome, ToolCall, ToolResult};
use crate::ui::tool_display::build_tool_row;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::collections::VecDeque;
use std::io;
use std::time::Instant;
use tokio::time::Duration;

use super::autocomplete::AutocompleteState;
use super::commands;
use super::input::{InputAction, InputState};
use super::pane_page::PanePage;
use super::prompt::{Decision, Prompt};
use super::render::{BoneTerminal, MAX_PANE_ROWS, MIN_ROWS, PaneDraw, Renderer, StatusInfo};

/// A tool-call approval awaiting a user decision. Held in `App` while the
/// bottom-pane prompt is shown so the streaming loop keeps pumping (spinner,
/// events, subagent panes) instead of blocking on a nested poll loop. The
/// `id` routes the decision back to the daemon through `RuntimeCommand`.
pub struct PendingApproval {
    /// The `RuntimeEvent::ApprovalRequest` id this prompt answers.
    id: u64,
    /// `true` once the user picked "Advise" and is typing free-form advice.
    advising: bool,
}

/// Tool metadata the frontend needs to render — display configs (for tool rows)
/// and definitions (for the context-size estimate) — fed from the daemon's
/// `FrontendState`. The client never executes tools (the daemon does), so this
/// is the VM-less replacement for the render concerns of the local `ToolHandler`.
#[derive(Default)]
pub struct WireTools {
    defs: Vec<crate::tools::ToolDefinition>,
    display: std::collections::HashMap<String, crate::tools::types::ToolDisplayConfig>,
}

impl WireTools {
    /// Build from the local VM's tool handler (the in-process / boot path, before
    /// any `FrontendState` arrives).
    fn from_handler(handler: &crate::tools::registry::ToolHandler) -> Self {
        Self {
            defs: handler.definitions(),
            display: handler.display_map().clone(),
        }
    }

    /// Custom display config for a tool call, if the tool registered one.
    pub fn display_for_call(
        &self,
        call: &crate::tools::ToolCall,
    ) -> Option<&crate::tools::types::ToolDisplayConfig> {
        self.display.get(&call.name)
    }

    /// Enabled tool definitions (for the local context-size estimate).
    pub fn definitions(&self) -> &[crate::tools::ToolDefinition] {
        &self.defs
    }
}

pub struct App {
    pub messages: Vec<Message>,
    /// Channel to send commands (SubmitPrompt, lifecycle, approvals) to the
    /// in-process daemon that owns the RuntimeSession.
    pub command_tx: tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeCommand>,
    /// In-process hand-off for `ReloadExtensions`: the TUI boots the extensions
    /// once and drops the cloned result here, so the daemon (which shares the
    /// Lua VM) adopts it instead of re-booting a second VM from disk. `None` in
    /// remote (`--connect`) mode — the remote daemon has its own VM and
    /// disk-boots on reload.
    pub reload_inbox: Option<std::sync::Arc<std::sync::Mutex<Option<crate::ext::BootedTools>>>>,
    /// Broadcast receiver for RuntimeEvents from the daemon.
    pub events_rx: tokio::sync::broadcast::Receiver<crate::runtime::RuntimeEvent>,
    /// Keeps a remote socket bridge alive for exactly as long as the App. Its
    /// `Drop` implementation terminates the bridge's forwarding/writer tasks.
    _remote_client: Option<crate::rpc::RemoteClient>,
    /// Read-only session DB handle for stats queries (the daemon owns the
    /// authoritative connection through its own RuntimeSession).
    pub session_db: Option<crate::session_db::SessionDb>,
    /// Read-only clone of the tool handler for display metadata and
    /// definitions (the daemon owns the authoritative, mutable copy).
    pub tools: crate::tools::registry::ToolHandler,
    pub input: InputState,
    pub streaming: bool,
    /// A live Lua command is running through `drive_live`. This needs the same
    /// cancellation plumbing as streaming, but it is not a model turn and
    /// should not show the thinking spinner.
    pub live_command: bool,
    pub should_quit: bool,
    pub renderer: Renderer,
    pub user_config: UserConfig,
    pub custom_configs: config::custom::CustomConfigs,
    pub queue: VecDeque<String>,

    pub approval_mode: ApprovalMode,
    pub active_prompt: Option<Prompt>,
    /// Tool-call approval awaiting a decision, resolved inside the main stream
    /// loop. `Some` only while `active_prompt` shows an approval prompt.
    pub pending_approval: Option<PendingApproval>,
    /// Set to `true` to abort the current streaming response.
    pub cancel_streaming: bool,
    /// Timestamp of the last Ctrl+C press (for double-tap quit).
    pub last_ctrl_c: Option<Instant>,
    /// Live, running estimate of `received` (completion) tokens during a turn.
    /// `Some` while a turn streams — ticked up on each text/tool delta and
    /// rebaselined to the authoritative count on `TokenUsage`; `None` when idle
    /// so the status bar shows `token_stats.received` directly.
    pub stream_estimated_received: Option<u64>,
    /// Frontend mirror of cumulative session state (token totals, conversation
    /// id/seq, active provider). Populated from `StateSnapshot` events after
    /// each turn / lifecycle change.
    pub view: crate::runtime::SessionSnapshot,
    /// Active conversation id, synced from `StateSnapshot` events so
    /// `app_ctx_state` can build Lua context without reading the session.
    pub conversation_id: Option<i64>,
    /// Accumulated token stats, synced from `StateSnapshot` events.
    pub token_stats: crate::llm::TokenStats,
    /// Accumulated per-provider usage, synced from `StateSnapshot` events.
    pub usage_by_provider: Vec<crate::ext::ctx::UsageProviderContext>,

    /// Active pane pages displayed between input and status bar.
    pub pages: Vec<PanePage>,
    /// Index of the currently visible pane page.
    pub active_page: usize,
    /// Whether pane pages are shown in the bottom pane.
    pub panes_visible: bool,
    /// Bounded tail of the current turn's reasoning text, rendered into the
    /// live "thinking" pane while a reasoning model streams (only when
    /// `user_config.show_thinking`). Cleared when the answer starts / turn ends.
    pub thinking_tail: String,
    /// When the thinking pane first appeared this turn — anchors the minimum
    /// 1s on-screen retention so a quick reasoning burst doesn't flash away.
    pub thinking_first_shown: Option<std::time::Instant>,
    /// Deferred teardown deadline for the thinking pane: set when the answer
    /// starts but the retention window hasn't elapsed; the pump tick clears it.
    pub thinking_clear_at: Option<std::time::Instant>,

    /// Wall-clock start of the current agent turn (set when streaming begins).
    turn_start: Option<Instant>,
    /// Accumulated time spent paused for user approvals during this turn.
    turn_paused_duration: std::time::Duration,
    /// Instant when the current approval pause started.
    turn_pause_start: Option<Instant>,
    /// Active autocomplete state (shown when typing `/`).
    autocomplete: Option<AutocompleteState>,
    /// Lua extension manager.
    extensions: ExtensionManager,
    /// Lua keymap snapshot for custom bindings.
    lua_keymap: crate::ext::snapshots::LuaKeymapSnapshot,
    /// Slash commands `(name, description)` the daemon advertised via
    /// `FrontendState`, for autocomplete. Empty until a remote daemon sends them;
    /// when empty, autocomplete falls back to the local VM's `commands()`.
    wire_commands: Vec<(String, String)>,
    /// Tool definitions + display configs the daemon advertised via
    /// `FrontendState`, used to render tool rows + estimate context size. Seeded
    /// from the local VM at boot and overwritten when a remote daemon sends its.
    wire_tools: WireTools,
    /// Lua status lines keyed by id (`bone.api.ui.set_statusline`), in
    /// registration order. Each id's segments are appended to the native
    /// status bar; re-setting the same id updates it in place.
    lua_status: Vec<(String, Vec<crate::runtime::view::StatusSegment>)>,
    /// Call IDs that already have a tool row in chat (to avoid duplicates).
    shown_tool_rows: std::collections::HashSet<String>,
    /// Last-seen job-registry version (forces first-tick render).
    jobs_seen_version: u64,
    /// Last wall-clock jobs-pane refresh (drives the ~1s live ticker).
    jobs_last_refresh: std::time::Instant,
    /// Set after the user was warned that quitting kills running sub-agent
    /// jobs; the next quit request goes through.
    quit_despite_jobs: bool,
    /// True when attached to a remote `bone serve` daemon (`bone --connect`).
    /// Slash commands then run on the daemon's Lua VM over the protocol
    /// (`RunCommand`) instead of the TUI's local VM; in-process mode (`false`)
    /// runs them locally via `run_lua_command`.
    is_remote: bool,
}

/// Where the App's turn-running daemon lives.
pub enum DaemonSource {
    /// Spawn an in-process daemon that owns the session (default TUI mode).
    InProcess,
    /// Attach to a remote `bone serve` daemon over a connected bridge
    /// (`bone --connect <addr>`). The local App keeps its own Lua VM for
    /// display + interactive slash commands; turns run on the remote daemon.
    Remote(crate::rpc::RemoteClient),
}

impl App {
    /// Construct the TUI with an in-process daemon (the normal entry point).
    pub fn new(
        llm: Box<dyn LlmProvider>,
        user_config: UserConfig,
        custom_configs: config::custom::CustomConfigs,
    ) -> io::Result<Self> {
        Self::with_daemon(llm, user_config, custom_configs, DaemonSource::InProcess)
    }

    /// Construct the TUI against a chosen [`DaemonSource`]. The Lua VM, renderer,
    /// banner, and view-model are identical across modes; only how the App
    /// reaches the daemon (in-process spawn vs. remote bridge) differs.
    pub fn with_daemon(
        llm: Box<dyn LlmProvider>,
        mut user_config: UserConfig,
        mut custom_configs: config::custom::CustomConfigs,
        daemon: DaemonSource,
    ) -> io::Result<Self> {
        // The provider is shared (via Arc) with the per-turn runtime Driver.
        let llm: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::from(llm);
        let provider = format!("{} ({})", llm.name(), llm.id());
        let model = llm.model().to_string();
        let approval_mode = user_config.approval_mode;
        // Boot Lua extension system and build tool handler.
        let opts = crate::ext::BootOptions {
            agent_depth: 0,
            headless: false,
            model: model.clone(),
            provider: provider.clone(),
            tool_allowlist: None,
        };
        let booted = crate::ext::boot_with_tools(
            &crate::config::bone_dir(),
            &std::env::current_dir().unwrap_or_default(),
            &mut custom_configs,
            true,
            opts,
            &model,
            &provider,
        );
        let extensions = booted.manager;
        let tools = booted.tools;
        // Seed the render-time tool metadata from the boot VM; a remote daemon
        // overwrites it via `FrontendState`. Built before `tools` is moved.
        let wire_tools = WireTools::from_handler(&tools);

        // Set model/provider on the Lua table (for banner and other Lua code).
        let lua = extensions.lua_handle();
        let lua = lua.lock().unwrap_or_else(|e| e.into_inner());
        let bone = lua.globals().get::<mlua::Table>("bone").ok();
        if let Some(bone) = bone {
            let _ = bone.set("model", model.clone());
            let _ = bone.set("provider", provider.clone());
        }
        drop(lua);

        // Collect banner text from `bone.banner()` (empty if undefined).
        let banner = Self::collect_banner(&extensions);

        // Create renderer with Lua theme applied over defaults.
        let mut renderer = Renderer::new();
        renderer.theme.apply_snapshot(extensions.theme_snapshot());

        // Apply Lua config snapshot — overrides YAML config values.
        apply_lua_config_snapshot(&mut user_config, extensions.config_snapshot());

        // Capture keymap snapshot before `extensions` is moved into the struct.
        let lua_keymap = extensions.keymap_snapshot().clone();

        let mut messages = Vec::new();
        if !banner.is_empty() {
            messages.push(Message::system(banner));
        }
        messages.push(Message::system(format!(
            "bone v{} — type /help for commands. Ctrl+C twice to quit.",
            env!("CARGO_PKG_VERSION")
        )));

        // ── Connect to the daemon (in-process spawn or remote bridge) ──
        // Both arms yield the same channel pair the App's event loop consumes:
        // a command sender and a broadcast event receiver.
        let is_remote = matches!(daemon, DaemonSource::Remote(_));
        let (command_tx, events_rx, initial_conversation_id, reload_inbox, remote_client) =
            match daemon {
                DaemonSource::InProcess => {
                    let runtime = std::sync::Arc::new(std::sync::Mutex::new(
                        crate::runtime::RuntimeSession::new(tools.clone()),
                    ));
                    let (hub, commands_rx) = crate::rpc::Hub::new();
                    let command_tx = hub.command_sender();
                    let events_rx = hub.subscribe();
                    let publisher = hub.publisher();

                    // Init the session DB inside the runtime. Snapshot the
                    // conversation_id before moving the runtime into the daemon.
                    if let Some(warning) = runtime.lock().unwrap().init_db(&*llm) {
                        messages.push(Message::system(warning));
                    }
                    let initial_conversation_id = runtime.lock().unwrap().conversation_id;

                    let reload_inbox: std::sync::Arc<
                        std::sync::Mutex<Option<crate::ext::BootedTools>>,
                    > = std::sync::Arc::new(std::sync::Mutex::new(None));
                    tokio::spawn(crate::rpc::run_daemon(
                        publisher,
                        commands_rx,
                        llm.clone(),
                        extensions.clone(),
                        runtime,
                        approval_mode,
                        Some(reload_inbox.clone()),
                        // In-process: the TUI shares this VM and drains the UiState
                        // itself, so the daemon must not also drain/forward.
                        false,
                    ));
                    (
                        command_tx,
                        events_rx,
                        initial_conversation_id,
                        Some(reload_inbox),
                        None,
                    )
                }
                DaemonSource::Remote(client) => {
                    // Subscribe before any `.await` so the daemon's on-connect
                    // StateSnapshot isn't missed; the conversation id arrives with
                    // it. No reload inbox — the remote daemon owns its own Lua VM.
                    let command_tx = client.command_sender();
                    let events_rx = client.subscribe();
                    // Sync our configured approval mode to the daemon up front.
                    // `bone serve` boots its gate at `Safe` regardless of config,
                    // and the client otherwise only pushes the mode when the user
                    // *cycles* it — so a `danger`-configured client would display
                    // Danger while the daemon silently gated at Safe until the
                    // first toggle. The in-process path doesn't need this: it
                    // hands `approval_mode` straight to `run_daemon`.
                    let _ = command_tx.send(crate::runtime::RuntimeCommand::SetApprovalMode {
                        mode: match approval_mode {
                            crate::tools::ApprovalMode::Danger => "danger",
                            crate::tools::ApprovalMode::Safe => "safe",
                        }
                        .to_string(),
                    });
                    (command_tx, events_rx, None, None, Some(client))
                }
            };

        // Read-only DB handle for the App's stats queries (sqlite WAL supports
        // concurrent readers). For a local daemon this is the same file the
        // daemon writes; for a remote host stats reflect the local DB.
        let session_db = match crate::session_db::SessionDb::open(&crate::session_db::db_path()) {
            Ok(db) => Some(db),
            Err(err) => {
                messages.push(Message::system(format!(
                    "warning: failed to open session database: {err}"
                )));
                None
            }
        };

        // Seed the frontend view from the empty session + active provider.
        let view = crate::runtime::SessionSnapshot {
            provider_id: llm.id().to_string(),
            provider_model: model.clone(),
            conversation_id: initial_conversation_id,
            ..Default::default()
        };

        Ok(Self {
            messages,
            command_tx,
            reload_inbox,
            events_rx,
            _remote_client: remote_client,
            session_db,
            tools,
            input: InputState::default(),
            streaming: false,
            live_command: false,
            should_quit: false,
            renderer,
            user_config,
            custom_configs,
            queue: VecDeque::new(),

            approval_mode,
            active_prompt: None,
            pending_approval: None,
            cancel_streaming: false,
            last_ctrl_c: None,
            stream_estimated_received: None,
            view,
            conversation_id: initial_conversation_id,
            token_stats: crate::llm::TokenStats::default(),
            usage_by_provider: Vec::new(),
            pages: Vec::new(),
            active_page: 0,
            panes_visible: true,
            thinking_tail: String::new(),
            thinking_first_shown: None,
            thinking_clear_at: None,

            turn_start: None,
            turn_paused_duration: std::time::Duration::ZERO,
            turn_pause_start: None,
            autocomplete: None,
            extensions,
            lua_keymap,
            wire_commands: Vec::new(),
            wire_tools,
            lua_status: Vec::new(),
            shown_tool_rows: std::collections::HashSet::new(),
            jobs_seen_version: u64::MAX,
            jobs_last_refresh: std::time::Instant::now(),
            quit_despite_jobs: false,
            is_remote,
        })
    }
    /// Collect banner text from `bone.banner()` Lua function.
    /// Returns lines joined with newlines, or empty if undefined/nothing.
    fn collect_banner(extensions: &crate::ext::ExtensionManager) -> String {
        let mut lines = Vec::new();
        if let Ok(g) = extensions.lua_handle().lock()
            && let Ok(bone) = g.globals().get::<mlua::Table>("bone")
            && let Ok(banner_fn) = bone.get::<mlua::Function>("banner")
        {
            match banner_fn.call::<mlua::Table>(()) {
                Ok(tbl) => {
                    for item in tbl.sequence_values::<mlua::String>() {
                        if let Ok(item_str) = item
                            && let Ok(s) = item_str.to_str()
                        {
                            lines.push(s.to_string());
                        }
                    }
                }
                Err(e) => {
                    eprintln!("bone: warning: banner() call failed: {e}");
                }
            }
        }

        // Append a release hint if a newer version was seen (cached, local
        // read — never blocks on network). Channel-agnostic: the releases
        // page covers every install method.
        if crate::update_check::update_available()
            && let Some(latest) = crate::update_check::latest_seen()
        {
            lines.push(format!(
                "bone {latest} available — https://github.com/vincentm65/bone/releases"
            ));
        }

        // Append a catalog-update hint (cached/local read — never blocks).
        let updates = crate::ext::catalog::updates_available();
        if updates > 0 {
            lines.push(format!(
                "{updates} catalog update{} available — run /catalog",
                if updates == 1 { "" } else { "s" }
            ));
        }

        lines.join("\n")
    }

    /// Drain daemon events during the idle loop (between turns). Updates local
    /// view-model fields from authoritative daemon snapshots.
    fn apply_idle_event(&mut self, ev: crate::runtime::RuntimeEvent) {
        use crate::runtime::RuntimeEvent;
        match ev {
            RuntimeEvent::StateSnapshot { snapshot } => {
                self.apply_snapshot(snapshot);
            }
            RuntimeEvent::ConversationLoaded { messages, snapshot } => {
                self.apply_snapshot(snapshot);
                self.messages.clear();
                let rows = self.rebuild_scrollback_from_transcript(&messages);
                self.messages.extend(rows);
                self.renderer.scrollback_cursor = 0;
            }
            RuntimeEvent::Status { message } => {
                self.messages.push(Message::system(message));
            }
            // The daemon VM's boot-time display state. Adopt it so the frontend
            // renders the daemon's theme/keymap/config/commands rather than its
            // own local VM's — the step toward a VM-less client. Sent on attach
            // and after a remote `ReloadExtensions`. The local VM (still present)
            // remains the fallback for anything not carried here.
            RuntimeEvent::FrontendState {
                theme, keymap, config, commands, tool_defs, tool_display, ..
            } => {
                self.apply_frontend_state(
                    theme, keymap, config, commands, tool_defs, tool_display,
                );
            }
            // Pane/UI diff from a remote daemon (e.g. a command's pane between
            // turns). In-process these come from the shared UiState drain.
            RuntimeEvent::ViewDiff { diff } => {
                self.apply_view_diff(diff);
            }
            // All other events are turn-scoped and ignored in idle.
            _ => {}
        }
    }

    /// Adopt the daemon VM's display state (theme/keymap/config/commands) from a
    /// `FrontendState` event. The blobs arrive as JSON (the protocol crate has no
    /// Lua snapshot types); deserialize back into the `Lua*Snapshot` shapes and
    /// apply them exactly as the boot path does. A blob that fails to decode is
    /// skipped so a partial/garbled field can't blank the others.
    #[allow(clippy::too_many_arguments)]
    fn apply_frontend_state(
        &mut self,
        theme: serde_json::Value,
        keymap: serde_json::Value,
        config: serde_json::Value,
        commands: Vec<(String, String)>,
        tool_defs: Vec<crate::tools::ToolDefinition>,
        tool_display: serde_json::Value,
    ) {
        if let Ok(snap) =
            serde_json::from_value::<crate::ext::snapshots::LuaThemeSnapshot>(theme)
        {
            self.renderer.theme.apply_snapshot(&snap);
        }
        if let Ok(snap) =
            serde_json::from_value::<crate::ext::snapshots::LuaKeymapSnapshot>(keymap)
        {
            self.lua_keymap = snap;
        }
        if let Ok(snap) =
            serde_json::from_value::<crate::ext::snapshots::LuaConfigSnapshot>(config)
        {
            apply_lua_config_snapshot(&mut self.user_config, &snap);
        }
        self.wire_commands = commands;
        // Tool render metadata: definitions arrive typed; display configs as an
        // opaque JSON map. A malformed display map is skipped (keeps the defs).
        self.wire_tools = WireTools {
            defs: tool_defs,
            display: serde_json::from_value(tool_display).unwrap_or_default(),
        };
    }

    /// Dispatch a `session_end` event to Lua handlers.
    pub fn dispatch_session_end(&self) {
        if self.is_remote {
            let _ = self.command_tx.send(crate::runtime::RuntimeCommand::DispatchHook {
                name: "session_end".into(),
                payload: serde_json::json!({}),
            });
        } else {
            self.extensions
                .dispatch_simple("session_end", serde_json::json!({}));
        }
    }

    /// Apply a generic action returned by a Lua command or hook.
    pub(crate) async fn apply_lua_action(
        &mut self,
        action: crate::ext::types::LuaReturnAction,
        term: &mut BoneTerminal,
    ) -> io::Result<Option<String>> {
        let mut action_reply = None;
        if let Some(new_messages) = action.conversation_replace {
            // Send the replacement to the daemon (idempotent).
            let _ = self.command_tx.send(crate::runtime::RuntimeCommand::ReplaceConversation {
                messages: new_messages,
            });
        }

        if let Some(load) = action.conversation_load {
            self.load_conversation(load, term)?;
        }

        if let Some(config_action) = action.config_action {
            action_reply = Some(self.apply_config_action(config_action).await);
        }
        Ok(action_reply)
    }

    async fn apply_config_action(&mut self, action: crate::ext::types::ConfigAction) -> String {
        match action {
            crate::ext::types::ConfigAction::Apply => {
                let custom = config::custom::CustomConfigs::load();
                self.apply_custom_configs_to_runtime(custom);
                // Notify the daemon to rebuild its provider from updated config.
                let active_id = self.view.provider_id.clone();
                let _ = self.command_tx.send(crate::runtime::RuntimeCommand::SwitchProvider {
                    provider_id: active_id.clone(),
                });
                // Await the daemon's StateSnapshot with new provider info.
                self.await_state_snapshot().await;
                "Configuration applied.".to_string()
            }
            crate::ext::types::ConfigAction::ReloadTools => self.reload_extensions(),
            crate::ext::types::ConfigAction::SwitchProvider { id } => {
                let mut custom = config::custom::CustomConfigs::load();
                custom.set_last_provider(&id);
                self.custom_configs = custom;
                // Tell the daemon to switch providers.
                let _ = self.command_tx.send(crate::runtime::RuntimeCommand::SwitchProvider {
                    provider_id: id.clone(),
                });
                // Await the daemon's StateSnapshot with new provider info.
                self.await_state_snapshot().await;
                format!("Switched to {} ({})", self.view.provider_model, self.view.provider_id)
            }
        }
    }

    /// Adopt a daemon `SessionSnapshot` as the local view-model: the single
    /// place the frontend mirrors authoritative state. Moves the snapshot in to
    /// avoid cloning the whole struct (only `usage_by_provider` is duplicated,
    /// since `to_token_stats` borrows the snapshot before it lands in `view`).
    pub(crate) fn apply_snapshot(&mut self, snapshot: crate::runtime::SessionSnapshot) {
        self.conversation_id = snapshot.conversation_id;
        self.token_stats = snapshot.to_token_stats();
        self.usage_by_provider = snapshot.usage_by_provider.clone();
        self.view = snapshot;
    }

    /// Block until the daemon publishes a StateSnapshot, then adopt it.
    async fn await_state_snapshot(&mut self) {
        loop {
            // Bind first so the recv future temporary is dropped before
            // `apply_snapshot` borrows `self` again.
            let ev = self.events_rx.recv().await;
            match ev {
                Ok(crate::runtime::RuntimeEvent::StateSnapshot { snapshot }) => {
                    self.apply_snapshot(snapshot);
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }

    /// Load a past conversation as the active chat (the `conversation.load`
    /// action, used by `/history`). Clears the current scrollback/transcript and
    /// resumes the selected conversation in place so future messages append to
    /// it rather than doubling up on the previous conversation.
    fn load_conversation(
        &mut self,
        load: crate::ext::types::ConversationLoad,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        self.reset_transient_ui_state();

        // Build the rendered scrollback from the loaded messages directly
        // (no runtime access needed — ConversationLoad carries the messages).
        self.conversation_id = load.conversation_id;
        self.messages.clear();
        let rows = self.rebuild_scrollback_from_transcript(&load.messages);
        self.messages.extend(rows);
        self.renderer.scrollback_cursor = 0;

        // Update local token estimate from the loaded transcript. Reuse the
        // core estimator (the driver's authoritative one) so the status-bar
        // estimate can't drift from the daemon's.
        self.token_stats.reset();
        let history = crate::chat::build_chat_history(&load.messages, None);
        let tool_defs_json_chars = serde_json::to_string(self.wire_tools.definitions())
            .map(|json| json.chars().count())
            .unwrap_or(0);
        let prompt_chars = crate::agent::estimate_context_chars(&history, tool_defs_json_chars);
        self.token_stats.set_context_estimate(prompt_chars);

        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.cancel_streaming = false;
        self.redraw(term)?;

        // Tell the daemon (idempotent — shares the same session).
        // The daemon publishes ConversationLoaded with authoritative state,
        // which the idle loop picks up on the next poll.
        if let Some(conv_id) = load.conversation_id {
            let _ = self.command_tx.send(crate::runtime::RuntimeCommand::LoadConversation { id: conv_id });
        }

        Ok(())
    }

    /// Convert the loaded transcript into rendered scrollback rows, reusing the
    /// live render path so a restored conversation looks identical to one built
    /// turn-by-turn: tool rows are relabelled from the originating call's
    /// arguments via [`build_tool_row`], and `edit_file` renders its diff
    /// preview (the diff is embedded in the persisted result content).
    fn rebuild_scrollback_from_transcript(&self, transcript: &[ChatMessage]) -> Vec<Message> {
        use crate::llm::ChatRole;
        // Map each tool_call_id to its originating call so a tool-result row can
        // be relabelled from the call's `arguments`, matching the live path.
        let calls: std::collections::HashMap<&str, &ToolCall> = transcript
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .map(|c| (c.id.as_str(), c))
            .collect();
        let mut rows = Vec::new();
        for msg in transcript {
            match msg.role {
                ChatRole::User => rows.push(Message::user_with_images(
                    msg.content.clone(),
                    msg.images.len(),
                )),
                ChatRole::Assistant => {
                    if !msg.content.trim().is_empty() {
                        rows.push(Message::assistant(msg.content.clone()));
                    }
                }
                // A *successful* `edit_file` shows its diff (no separate tool
                // row) live; the diff is embedded in the result content after
                // the summary line, and a System message starting with `\n`
                // renders as a preview. Failures (no diff) fall through to the
                // generic tool-row path below so the error is still visible.
                ChatRole::Tool
                    if msg.name.as_deref() == Some("edit_file")
                        && !msg.is_error
                        && msg.content.starts_with("edited file (") =>
                {
                    if let Some(nl) = msg.content.find('\n') {
                        rows.push(Message::system(msg.content[nl..].to_string()));
                    }
                }
                ChatRole::Tool => {
                    let row = match msg.tool_call_id.as_deref().and_then(|id| calls.get(id)) {
                        Some(call) => build_tool_row(
                            call,
                            &ToolResult {
                                content: msg.content.clone(),
                                images: msg.images.clone(),
                                is_error: msg.is_error,
                                ..Default::default()
                            },
                            self.wire_tools.display_for_call(call),
                        ),
                        None => {
                            let label = msg.name.clone().unwrap_or_else(|| "tool".to_string());
                            let mut row = Message::tool_row(label, msg.is_error);
                            row.image_count = msg.images.len();
                            row
                        }
                    };
                    rows.push(row);
                }
                ChatRole::System => {}
            }
        }
        rows
    }

    /// Reset transient per-turn UI state before switching conversations:
    /// abort any in-flight stream, drop panes, queued input, and any pending
    /// tool-approval prompt. Shared by `/clear` and conversation load.
    fn reset_transient_ui_state(&mut self) {
        self.cancel_streaming = true;
        self.pages.clear();
        self.active_page = 0;
        self.tools.state_map.clear();
        self.jobs_seen_version = u64::MAX;
        self.queue.clear();
        self.active_prompt = None;
        self.pending_approval = None;
    }

    async fn start_new_conversation(&mut self) {
        let _ = self.command_tx.send(crate::runtime::RuntimeCommand::NewConversation);
        // Await the daemon's StateSnapshot for authority.
        self.await_state_snapshot().await;
    }

    /// Clear chat history, end the current DB conversation, start a fresh one,
    /// and display a usage summary.
    async fn clear_chat(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        self.reset_transient_ui_state();

        let summary = if self.token_stats.request_count > 0 {
            format!(
                "Session: {}. Chat cleared.",
                self.token_stats.one_liner()
            )
        } else {
            "Chat cleared.".to_string()
        };

        // Tell the daemon to start a new conversation. It publishes a
        // StateSnapshot with the new conversation_id and reset stats.
        let _ = self.command_tx.send(crate::runtime::RuntimeCommand::NewConversation);

        // Await the daemon's StateSnapshot for authority.
        self.await_state_snapshot().await;

        self.messages.clear();
        self.messages.push(Message::system(format!(
            "bone v{} — type /help for commands. Ctrl+C twice to quit.",
            env!("CARGO_PKG_VERSION")
        )));
        self.messages.push(Message::system(summary));
        self.renderer.scrollback_cursor = 0;
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.cancel_streaming = false;
        self.redraw(term)?;
        Ok(())
    }

    fn persist_runtime_config(&mut self) {
        let mode = match self.user_config.approval_mode {
            crate::tools::ApprovalMode::Danger => "danger",
            crate::tools::ApprovalMode::Safe => "safe",
        };
        self.custom_configs
            .set_value("general", "approval_mode", mode.to_string());
        if self.is_remote {
            let _ = self.command_tx.send(crate::runtime::RuntimeCommand::DispatchHook {
                name: "mode_change".into(),
                payload: serde_json::json!({ "mode": mode }),
            });
        } else {
            self.extensions
                .dispatch_simple("mode_change", serde_json::json!({ "mode": mode }));
        }
        // Push the mode to the daemon's authoritative `SharedApprovalMode` — the
        // atomic the gate actually reads. Without this, cycling Safe/Danger only
        // changes the UI while the daemon keeps gating at its startup mode.
        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::SetApprovalMode {
                mode: mode.to_string(),
            });
    }

    fn apply_custom_configs_to_runtime(&mut self, custom: config::custom::CustomConfigs) {
        self.user_config.apply_custom_configs(&custom);
        self.approval_mode = self.user_config.approval_mode;
        self.custom_configs = custom;
    }

    pub async fn run(&mut self) -> io::Result<()> {
        let mut terminal = Renderer::init_terminal(MIN_ROWS)?;

        self.renderer
            .flush_new_to_scrollback(&self.messages, &mut terminal)?;
        self.refresh_jobs_pane();
        self.force_redraw(&mut terminal)?;

        while !self.should_quit {
            if event::poll(std::time::Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        // Coalesce a non-bracketed paste burst (Windows conhost
                        // delivers a paste as a flood of Char events) into one
                        // insert_paste so large pastes collapse to a placeholder
                        // and cost a single redraw.
                        if let Some(c) = plain_char(&key)
                            && self.active_prompt.is_none()
                        {
                            let burst = collect_paste_burst(c)?;
                            if is_paste_burst(&burst.text) {
                                self.input.history_index = None;
                                self.input.insert_paste(&burst.text);
                                self.update_autocomplete();
                                self.redraw(&mut terminal)?;
                            } else {
                                self.handle_key(key.code, key.modifiers, &mut terminal)
                                    .await?;
                            }
                            if let Some(trailing) = burst.trailing {
                                self.handle_trailing_input_event(trailing, &mut terminal)
                                    .await?;
                            }
                        } else {
                            self.handle_key(key.code, key.modifiers, &mut terminal)
                                .await?;
                        }
                    }
                    Event::Paste(text) => {
                        self.input.insert_paste(&text);
                        self.update_autocomplete();
                        self.redraw(&mut terminal)?;
                    }
                    Event::Resize(_, _) | Event::Key(_) => {
                        // Resize or non-press key: force a full redraw to
                        // re-sync the inline viewport position.
                        self.force_redraw(&mut terminal)?;
                    }
                    _ => {}
                }
            }

            // P1: Detect stale terminal size (e.g. after tmux detach/reattach
            // where SIGWINCH may not fire). If the dimensions changed out from
            // under us, force a redraw.
            if let Ok(size) = crossterm::terminal::size()
                && self.renderer.last_size != Some(size)
            {
                self.force_redraw(&mut terminal)?;
            }

            // Drain daemon events between turns (StateSnapshot, Status, etc.).
            while let Ok(ev) = self.events_rx.try_recv() {
                self.apply_idle_event(ev);
            }

            // Tick background jobs: refresh pane + auto-inject finished results.
            self.tick_jobs(&mut terminal).await?;

            // Drain prompts queued by Lua via `bone.api.submit`.
            self.tick_inbox(&mut terminal).await?;
        }

        // Finalize any in-progress streaming message before clearing the
        // viewport, so the user sees "[cancelled]" and the last partial
        // line in scrollback rather than losing them.
        if self.streaming {
            if let Some(msg) = self.messages.last_mut()
                && (msg.content.is_empty() || !msg.content.ends_with("\n[cancelled]"))
            {
                msg.content.push_str("\n[cancelled]");
            }
            self.renderer.finalize_streaming_message(
                self.messages
                    .last()
                    .map(|m| m.content.as_str())
                    .unwrap_or(""),
                &mut terminal,
            )?;
            self.renderer
                .flush_new_to_scrollback(&self.messages, &mut terminal)?;
        }

        self.dispatch_session_end();

        Renderer::prepare_exit(&mut terminal)?;
        Renderer::shutdown_terminal()?;
        Ok(())
    }

    /// Ensure the viewport is the right size, then draw.
    fn ensure_viewport_and_draw(&mut self, terminal: &mut BoneTerminal) -> io::Result<()> {
        // Apply any Lua-driven UI updates (floats from bone.api.ui, ctx.ui.pane,
        // or ctx.emit_pane) before measuring, so a newly opened float is
        // counted in the viewport height.
        self.apply_view_diffs();
        let size = terminal.size()?;
        // Publish the live terminal width so Lua panes (`ctx.ui.width`) can wrap
        // text to the current width. Re-read each frame so it tracks resizes.
        if self.is_remote {
            let _ = self.command_tx.send(crate::runtime::RuntimeCommand::SetTerminalWidth {
                width: size.width,
            });
        } else if let Ok(mut ui) = self.extensions.ui_handle().lock() {
            ui.terminal_width = size.width;
        }
        let desired = Renderer::desired_height(
            &self.input,
            // Approval prompt is a pane now (counted via `visible_pages`), so the
            // input slot is sized normally — pass no prompt.
            None,
            size.width,
            self.visible_pages(),
            self.active_page,
            self.autocomplete.as_ref(),
        )
        // The inline viewport can never be taller than the terminal — crossterm
        // can't reserve more rows than exist, and an oversized inline viewport
        // scrolls and overlaps (duplicated tab bars, status text bleeding into
        // a tall /config menu over a subagent pane). The draw code already
        // truncates content to its area, so clamping degrades gracefully. We
        // reserve one row above it (`max_viewport_height`) so the viewport never
        // fills the whole screen and `insert_before` keeps using its robust
        // partial-screen scroll path.
        .min(crate::ui::render::max_viewport_height(size.height));

        let old_height = self.renderer.viewport_height;
        if desired != old_height {
            Renderer::resize_viewport(terminal, old_height, desired)?;
            self.renderer.viewport_height = desired;
        }

        terminal.draw(|frame| self.draw(frame))?;
        Ok(())
    }

    /// Redraw from scratch, updating the tracked terminal size.
    /// Used after resize or stale-size detection.
    fn force_redraw(&mut self, terminal: &mut BoneTerminal) -> io::Result<()> {
        // On a physical terminal resize (cols/rows changed), the emulator has
        // already reflowed both scrollback and the inline viewport. ratatui's
        // built-in inline autoresize re-anchors the viewport by scrolling
        // (`append_lines`), which leaves the old, reflowed viewport rows (the
        // separators above/below the input, wrapped to an unknown height) stuck
        // in scrollback as glitchy duplicates. There's no reliable in-place
        // erase, so rebuild from scratch: wipe screen + scrollback and re-flush
        // all history at the new width — the same way scrollback is built from
        // empty on startup.
        let size = crossterm::terminal::size()?;
        if self.renderer.last_size.is_some_and(|last| last != size) {
            self.rebuild_scrollback_after_resize(terminal)?;
            self.renderer.last_size = Some(size);
        }
        self.ensure_viewport_and_draw(terminal)?;
        self.renderer.last_size = Some(size);
        Ok(())
    }

    /// Wipe the screen and native scrollback, then re-render all message history
    /// into scrollback at the current terminal width. Used after a physical
    /// resize, where reflowed/duplicated viewport rows can't be erased in place.
    fn rebuild_scrollback_after_resize(&mut self, terminal: &mut BoneTerminal) -> io::Result<()> {
        Renderer::hard_reset_viewport(terminal, self.renderer.viewport_height)?;
        self.renderer.reset_scrollback_state();

        // The in-progress streamed assistant message (if any) is flushed via the
        // streaming path rather than as a committed message, and `scrollback_cursor`
        // counts it as already accounted for. Re-flush committed messages up to
        // it, then replay the streamed portion so the counters line up.
        let stream_idx = if self.streaming {
            self.messages.len().checked_sub(1)
        } else {
            None
        };
        let committed_end = stream_idx.unwrap_or(self.messages.len());
        self.renderer
            .flush_new_to_scrollback(&self.messages[..committed_end], terminal)?;

        if let Some(idx) = stream_idx {
            self.renderer.scrollback_cursor += 1;
            self.renderer.streaming_source_flushed = 0;
            self.renderer
                .flush_streaming_message(&self.messages[idx].content, terminal)?;
        }
        Ok(())
    }

    fn redraw(&mut self, terminal: &mut BoneTerminal) -> io::Result<()> {
        self.ensure_viewport_and_draw(terminal)
    }

    /// Pause the turn timer (call before entering approval prompt).
    pub(crate) fn timer_pause(&mut self) {
        if self.turn_start.is_some() && self.turn_pause_start.is_none() {
            self.turn_pause_start = Some(Instant::now());
        }
    }

    /// Resume the turn timer (call after approval prompt returns).
    pub(crate) fn timer_resume(&mut self) {
        if let Some(pause_start) = self.turn_pause_start.take() {
            self.turn_paused_duration += pause_start.elapsed();
        }
    }

    /// Compute the active elapsed time for the current turn, formatted as M:SS.
    pub(crate) fn timer_elapsed(&self) -> Option<String> {
        let start = self.turn_start?;
        // Subtract pause time accumulated so far. While currently paused, the
        // ongoing pause isn't added to `turn_paused_duration` until resume, so
        // the same subtraction is correct in both states.
        let elapsed = start.elapsed().saturating_sub(self.turn_paused_duration);
        let total_secs = elapsed.as_secs();
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        Some(format!("{}:{:02}", mins, secs))
    }

    /// Raw active elapsed milliseconds of the current turn (for spinner frames).
    pub(crate) fn timer_elapsed_ms(&self) -> u64 {
        let Some(start) = self.turn_start else {
            return 0;
        };
        start
            .elapsed()
            .saturating_sub(self.turn_paused_duration)
            .as_millis() as u64
    }

    pub(crate) fn status_info(&self) -> StatusInfo {
        self.stream_status_info_with_tokens(self.stream_estimated_received)
    }

    /// Build a [`StatusInfo`] for the streaming spinner wait, with an optional
    /// live cumulative output-token estimate.
    fn stream_status_info_with_tokens(&self, estimated_tokens: Option<u64>) -> StatusInfo {
        // Don't show the "thinking" spinner / timer while an interactive pane
        // owns the UI. The pane may be briefly inactive after submitting a
        // navigation value and before Lua upserts the replacement pane; treating
        // that handoff as interactive avoids a one-frame spinner flash.
        // A pending tool-approval is interactive too — it now lives in the pane
        // region like Lua menus, so pause the spinner/timer the same way.
        let interacting = self.has_lua_menu_pane() || self.active_prompt.is_some();
        // A live Lua command (e.g. /shotgun) runs through `drive_live` without
        // setting `streaming`, but it's still working — keep the spinner/timer
        // alive so the UI doesn't look frozen during long multi-model runs.
        let spinner_active = (self.streaming || self.live_command) && !interacting;
        let elapsed = if interacting {
            None
        } else {
            self.timer_elapsed()
        };
        let mut info = stream_status_info_with_token_stats(
            estimated_tokens,
            &self.view.provider_model,
            &self.view.to_token_stats(),
            spinner_active,
            self.approval_mode,
            self.queue.len(),
            &self.user_config,
            elapsed,
        );
        info.lua_status = self
            .lua_status
            .iter()
            .flat_map(|(_, segs)| segs.iter().cloned())
            .collect();
        info.spinner_elapsed_ms = self.timer_elapsed_ms();
        info
    }

    /// True when Lua is showing its shared menu pane.
    fn has_lua_menu_pane(&self) -> bool {
        self.pages.iter().any(|p| p.source == "interact")
    }

    /// Tick background-job status: refresh pane if needed, auto-inject
    /// finished results when the TUI is idle.
    async fn tick_jobs(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        // 1. Pane refresh: version change, or ~1s ticker while jobs run.
        if self.maybe_refresh_jobs_pane() {
            self.redraw(term)?;
        }

        // 2. Auto-injection: only when idle. Peek first; mark the jobs
        //    consumed only after the injection actually went through.
        if self.active_prompt.is_none() && !self.streaming && self.queue.is_empty() {
            let finished = crate::ext::jobs::registry().peek_finished_unconsumed();
            if let Some((text, display)) = Self::format_job_results(&finished) {
                let ids: Vec<String> = finished.iter().map(|j| j.id.clone()).collect();
                let draft = std::mem::take(&mut self.input.buffer);
                let draft_cursor = self.input.cursor_pos;
                self.submit_user_turn(text, Some(display), Vec::new(), term)
                    .await?;
                crate::ext::jobs::registry().mark_consumed(&ids);
                if !draft.is_empty() {
                    self.input.buffer = draft;
                    self.input.cursor_pos = draft_cursor.min(self.input.buffer.chars().count());
                    self.redraw(term)?;
                }
                // Drain any queued messages left after injection.
                while let Some(queued) = self.queue.pop_front() {
                    self.input.buffer = queued;
                    self.input.cursor_pos = self.input.buffer.chars().count();
                    self.send_message(term).await?;
                }
            }
        }

        Ok(())
    }

    /// Drain prompts queued by Lua (`bone.api.submit`) and feed them through the
    /// normal input path. When idle they submit immediately; mid-turn they wait
    /// in `self.queue` and drain when the active turn ends (the same path typed
    /// input uses), so the status bar's `Q:` count reflects them.
    async fn tick_inbox(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let texts = crate::ext::inbox::drain();
        if texts.is_empty() {
            return Ok(());
        }
        for text in texts {
            self.queue.push_back(text);
        }
        if self.active_prompt.is_none() && !self.streaming {
            while let Some(queued) = self.queue.pop_front() {
                self.input.buffer = queued;
                self.input.cursor_pos = self.input.buffer.chars().count();
                self.send_message(term).await?;
            }
        } else {
            self.redraw(term)?;
        }
        Ok(())
    }

    /// Refresh the jobs pane when the registry version changed or, while
    /// jobs are running, at least once per second so elapsed time and token
    /// counters stay live. Returns `true` when the pane was refreshed.
    pub(crate) fn maybe_refresh_jobs_pane(&mut self) -> bool {
        let registry = crate::ext::jobs::registry();
        let version = registry.version();
        let periodic = self.jobs_last_refresh.elapsed() >= std::time::Duration::from_secs(1)
            && !registry.running_ids().is_empty();
        if version == self.jobs_seen_version && !periodic {
            return false;
        }
        // Unhide the pane when a new job starts while hidden.
        if version != self.jobs_seen_version && !registry.running_ids().is_empty() {
            self.panes_visible = true;
        }
        self.refresh_jobs_pane();
        self.jobs_seen_version = version;
        self.jobs_last_refresh = std::time::Instant::now();
        true
    }

    /// Apply a single `ViewDiff` to the app state. Shared by the render-tick
    /// drain of the standalone `UiState` handle (both `bone.api.ui.*` and
    /// `ctx.ui.pane` push into it). Returns `true` when the diff caused a
    /// visible change.
    pub(crate) fn apply_view_diff(&mut self, diff: crate::runtime::view::ViewDiff) -> bool {
        use crate::runtime::view::{Component, ViewDiff};
        match diff {
            // A Lua status line is appended to the native status bar.
            ViewDiff::Upsert {
                component: Component::StatusLine { id, segments },
            } => {
                match self.lua_status.iter_mut().find(|(i, _)| *i == id) {
                    Some(slot) => slot.1 = segments,
                    None => self.lua_status.push((id, segments)),
                }
                true
            }
            ViewDiff::Upsert { component } => {
                if let Some(pc) = component.as_pane_content() {
                    if pc.is_empty() {
                        self.active_page =
                            PanePage::remove(&mut self.pages, &pc.source, self.active_page);
                    } else {
                        let page = PanePage::from_content(&pc);
                        let (_, new_active) =
                            PanePage::upsert(&mut self.pages, self.active_page, page);
                        self.active_page = new_active;
                        self.panes_visible = true;
                    }
                    true
                } else {
                    false
                }
            }
            ViewDiff::Remove { id } => {
                let before = self.lua_status.len();
                self.lua_status.retain(|(i, _)| i != &id);
                if self.lua_status.len() == before {
                    self.active_page = PanePage::remove(&mut self.pages, &id, self.active_page);
                }
                true
            }
            ViewDiff::SetHighlight { name, fg } => {
                self.renderer.theme.set_highlight(&name, fg.as_deref())
            }
        }
    }

    /// Drain UI diffs emitted by `bone.api.ui.*` and apply them. Mirrors
    /// `maybe_refresh_jobs_pane`: called on the render tick and the live
    /// ticks so Lua UI appears and updates. `Float` components map to panes via
    /// `Component::as_pane_content`; `StatusLine` segments append to the native
    /// status bar; `SetHighlight` recolors the live theme. Returns `true` when
    /// anything changed (so the caller redraws).
    pub(crate) fn apply_view_diffs(&mut self) -> bool {
        let diffs = self.extensions.drain_view_diffs();
        if diffs.is_empty() {
            return false;
        }
        let mut changed = false;
        for diff in diffs {
            if self.apply_view_diff(diff) {
                changed = true;
            }
        }
        changed
    }

    /// Refresh the background-jobs live-pane from the job registry.
    ///
    /// Rendered natively in Rust (no Lua) so the pane stays live even while
    /// a Lua tool blocks the VM (e.g. a long `ctx.agent.wait`). The pane is
    /// driven entirely by the generic job registry — it has no knowledge of
    /// which tool (sub-agent, shotgun, …) dispatched a given job.
    /// Only shows when there are running jobs; hides when all are idle.
    fn refresh_jobs_pane(&mut self) {
        let jobs = crate::ext::jobs::registry().all_jobs();
        let has_running = jobs
            .iter()
            .any(|j| j.status == crate::ext::jobs::JobStatus::Running);
        if has_running {
            if let Some(page) = crate::ui::jobs_pane::render(&jobs) {
                let (_, new_active) = PanePage::upsert(&mut self.pages, self.active_page, page);
                self.active_page = new_active;
                self.panes_visible = true;
            }
        } else {
            // No running jobs — hide the pane.
            self.active_page = PanePage::remove(
                &mut self.pages,
                crate::ui::jobs_pane::PANE_SOURCE,
                self.active_page,
            );
        }
    }

    /// Format finished background-job results for auto-injection. Operates on
    /// the generic job registry, independent of which tool dispatched them.
    /// Returns `(turn_text, display_text)` or `None` when no finished jobs.
    fn format_job_results(jobs: &[crate::ext::jobs::Job]) -> Option<(String, String)> {
        if jobs.is_empty() {
            return None;
        }
        let mut lines = Vec::with_capacity(jobs.len());
        for job in jobs {
            let status_sym = job_status_sym(job.status);
            let mut truncated = crate::ext::jobs::truncate_for_injection(
                job.result.as_deref().unwrap_or(""),
                crate::ext::jobs::MAX_INJECT_CHARS,
            );
            if let Some(file) = &job.result_file {
                truncated.push_str(&format!("\n[full output saved to: {file}]"));
            }
            lines.push(format!(
                "## {} ({}) — {}\n{}",
                job.agent, job.id, status_sym, truncated
            ));
        }
        let still_running = crate::ext::jobs::registry().running_jobs();
        if !still_running.is_empty() {
            let names: Vec<String> = still_running
                .iter()
                .map(|j| format!("{} ({})", j.agent, j.id))
                .collect();
            lines.push(format!(
                "Note: still running: {}. Their results will arrive automatically in a later message — do not assume their outcome.",
                names.join(", ")
            ));
        }
        let turn_text = format!(
            "[automated message] Results from background jobs you dispatched earlier are now ready. \
             Review them and continue the task they were dispatched for; if nothing remains to be done, \
             summarize the outcomes for the user.\n\n{}",
            lines.join("\n\n")
        );
        let display: String = jobs
            .iter()
            .map(|j| format!("{} {}", j.agent, job_status_sym(j.status)))
            .collect::<Vec<_>>()
            .join(", ");
        let display_text = format!("[job results: {}]", display);
        Some((turn_text, display_text))
    }
}

/// Status glyph for a subagent job (done / error / running).
fn job_status_sym(status: crate::ext::jobs::JobStatus) -> &'static str {
    match status {
        crate::ext::jobs::JobStatus::Done => "✓",
        crate::ext::jobs::JobStatus::Error => "✗",
        crate::ext::jobs::JobStatus::Running => "◑",
    }
}

/// Built-in spinner used when no Lua preset resolves, so the streaming spinner
/// is never blank. Mirrors the bundled `braille` preset.
const FALLBACK_SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const FALLBACK_SPINNER_SPEED_MS: u64 = 80;

/// Build a [`StatusInfo`] with a live streaming cumulative output-token estimate.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_status_info_with_token_stats(
    streaming_completion_tokens: Option<u64>,
    model: &str,
    token_stats: &crate::llm::TokenStats,
    streaming: bool,
    approval_mode: crate::tools::ApprovalMode,
    queue_len: usize,
    cfg: &crate::config::UserConfig,
    elapsed: Option<String>,
) -> StatusInfo {
    // Resolve the selected spinner style + speed override. Fall back to a
    // built-in spinner if the configured style has no matching preset (e.g. the
    // `ui.spinners` lib failed to load, or the YAML enum drifted from presets)
    // so the streaming spinner never silently disappears.
    let (mut spinner_frames, mut spinner_speed_ms) = cfg
        .spinner_styles
        .iter()
        .find(|s| s.name == cfg.spinner_style)
        .map(|s| {
            let speed = if cfg.spinner_speed > 0 {
                cfg.spinner_speed
            } else {
                s.speed
            };
            (s.frames.clone(), speed)
        })
        .unwrap_or_default();
    if spinner_frames.is_empty() {
        spinner_frames = FALLBACK_SPINNER_FRAMES
            .iter()
            .map(|s| s.to_string())
            .collect();
        if spinner_speed_ms == 0 {
            spinner_speed_ms = FALLBACK_SPINNER_SPEED_MS;
        }
    }
    let spinner_texts = if cfg.spinner_text_custom.trim().is_empty() {
        cfg.spinner_texts
            .iter()
            .find(|t| t.name == cfg.spinner_text)
            .map(|t| t.phrases.clone())
            .unwrap_or_default()
    } else {
        cfg.spinner_text_custom
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };

    StatusInfo {
        model: model.to_string(),
        token_stats: token_stats.clone(),
        streaming_completion_tokens,
        streaming,
        approval_mode,
        queue_len,
        status_show: cfg.status_show.clone(),
        elapsed,
        lua_status: Vec::new(),
        spinner_frames,
        spinner_speed_ms,
        spinner_texts,
        spinner_text_rotate: cfg.spinner_text_rotate,
        spinner_text_speed_ms: cfg.spinner_text_speed,
        spinner_elapsed_ms: 0,
    }
}

impl App {
    /// Render the bottom pane (input, status bar, panes, autocomplete).
    fn draw(&self, frame: &mut ratatui::Frame) {
        self.renderer.draw_bottom_pane(
            frame,
            &PaneDraw {
                input: &self.input,
                status_info: &self.status_info(),
                pages: self.visible_pages(),
                active_page: self.active_page,
                autocomplete: self.autocomplete.as_ref(),
            },
            // The tool-approval prompt renders as a live pane (source
            // `"approval"`) in the pane region, not the input slot, so the input
            // field stays usable (e.g. for typing free-form advice).
            None,
        );
    }

    fn visible_pages(&self) -> &[PanePage] {
        if self.panes_visible { &self.pages } else { &[] }
    }

    /// Update autocomplete state based on current input buffer.
    /// Shows autocomplete when buffer starts with `/`, hides otherwise.
    fn update_autocomplete(&mut self) {
        // Don't open autocomplete while navigating history — prevents the
        // dropdown from reopening on every arrow press when a history entry
        // starts with '/'.
        if self.input.history_index.is_some() {
            self.autocomplete = None;
            return;
        }
        let buf = &self.input.buffer;
        if let Some(query) = buf.strip_prefix('/') {
            // Don't show if there's a space (user typed args already)
            if query.contains(' ') {
                self.autocomplete = None;
                return;
            }
            if self.autocomplete.is_none() {
                self.autocomplete = Some(AutocompleteState::new(self.collect_commands()));
            }
            if let Some(ref mut ac) = self.autocomplete {
                ac.update(query);
            }
        } else {
            self.autocomplete = None;
        }
    }

    /// Collect all available slash commands with descriptions (builtins + Lua).
    /// Prefers the daemon-advertised list (`wire_commands`) when present — the
    /// authoritative source for a remote host — and otherwise falls back to the
    /// local VM's registered commands.
    fn collect_commands(&self) -> Vec<(String, String)> {
        let mut cmds = crate::ui::autocomplete::builtin_commands();
        if !self.wire_commands.is_empty() {
            cmds.extend(self.wire_commands.iter().cloned());
        } else if self.extensions.is_available() {
            for cmd in self.extensions.commands() {
                cmds.push((cmd.name.clone(), cmd.description.clone()));
            }
        }
        cmds
    }

    async fn handle_trailing_input_event(
        &mut self,
        event: Event,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.handle_key(key.code, key.modifiers, term).await
            }
            Event::Paste(text) => {
                self.input.insert_paste(&text);
                self.update_autocomplete();
                self.redraw(term)
            }
            Event::Resize(_, _) => self.force_redraw(term),
            _ => Ok(()),
        }
    }

    async fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        // Handle page keybindings before input processing
        if code == KeyCode::Char('t') && modifiers.contains(KeyModifiers::CONTROL) {
            self.panes_visible = !self.panes_visible;
            return self.redraw(term);
        }

        if self.panes_visible && !self.pages.is_empty() && modifiers.is_empty() {
            match code {
                KeyCode::Tab => {
                    self.active_page = (self.active_page + 1) % self.pages.len();
                    return self.redraw(term);
                }
                KeyCode::BackTab => {
                    self.active_page = if self.active_page == 0 {
                        self.pages.len() - 1
                    } else {
                        self.active_page - 1
                    };
                    return self.redraw(term);
                }
                KeyCode::PageUp => {
                    let page = &mut self.pages[self.active_page];
                    page.scroll = page.scroll.saturating_sub(MAX_PANE_ROWS);
                    return self.redraw(term);
                }
                KeyCode::PageDown => {
                    let page = &mut self.pages[self.active_page];
                    page.scroll = (page.scroll + MAX_PANE_ROWS).min(page.max_scroll());
                    return self.redraw(term);
                }
                _ => {}
            }
        }

        // Ctrl+Up / Ctrl+Down: scroll pane pages
        if self.panes_visible && !self.pages.is_empty() {
            if matches!(code, KeyCode::Up) && modifiers.contains(KeyModifiers::CONTROL) {
                let page = &mut self.pages[self.active_page];
                page.scroll = page.scroll.saturating_sub(1);
                return self.redraw(term);
            }
            if matches!(code, KeyCode::Down) && modifiers.contains(KeyModifiers::CONTROL) {
                let page = &mut self.pages[self.active_page];
                page.scroll = (page.scroll + 1).min(page.max_scroll());
                return self.redraw(term);
            }
        }

        if self.active_prompt.is_some() {
            return self.handle_prompt_key(code, term);
        }

        // Autocomplete key interception (before input.apply_key)
        if let Some(ref mut ac) = self.autocomplete {
            // Windows Console reports arrow keys with extra modifier/state bits,
            // so an exact `modifiers.is_empty()` check fails there and menu
            // navigation goes dead. Only CTRL/ALT should disqualify plain
            // navigation; SHIFT and enhanced-key state are benign.
            let nav_mods = !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
            match code {
                // Arrow Up/Down: if buffer is a complete command (exact match),
                // dismiss autocomplete and fall through to history navigation.
                // Otherwise scroll the suggestion list.
                KeyCode::Up | KeyCode::Down if nav_mods => {
                    let buf = &self.input.buffer;
                    let complete = buf.starts_with('/')
                        && ac
                            .matches
                            .iter()
                            .any(|(name, _)| *buf == format!("/{}", name));
                    if complete {
                        self.autocomplete = None;
                        // fall through to apply_key for history
                    } else {
                        if code == KeyCode::Up {
                            ac.up();
                        } else {
                            ac.down();
                        }
                        self.redraw(term)?;
                        return Ok(());
                    }
                }
                KeyCode::Tab | KeyCode::Enter if nav_mods => {
                    if let Some(cmd) = ac.selected_command() {
                        self.input.buffer = format!("/{}", cmd);
                        self.input.cursor_pos = self.input.buffer.chars().count();
                        if code == KeyCode::Enter {
                            self.autocomplete = None;
                            self.send_message(term).await?;
                            while let Some(queued) = self.queue.pop_front() {
                                self.input.buffer = queued;
                                self.input.cursor_pos = self.input.buffer.chars().count();
                                self.send_message(term).await?;
                            }
                        } else {
                            self.autocomplete = None;
                            self.redraw(term)?;
                        }
                        return Ok(());
                    }
                    self.autocomplete = None;
                    return self.redraw(term);
                }
                KeyCode::Esc => {
                    self.autocomplete = None;
                    return self.redraw(term);
                }
                _ => {}
            }
        }

        // Check Lua keymap bindings before default input handling.
        if let Some(action) = self.lookup_keymap(code, modifiers) {
            return self.handle_keymap_action(action, term).await;
        }

        // Detect a non-bracketed paste flood: if more key events are already
        // buffered behind this one, we're mid-paste (e.g. Windows conhost).
        self.input.paste_mode = event::poll(std::time::Duration::from_millis(0)).unwrap_or(false);
        match self.input.apply_key(code, modifiers) {
            InputAction::Cancel => self.handle_ctrl_c(term),
            InputAction::Submit => {
                self.autocomplete = None;
                // Remove the old interact page before clearing the input so
                // the screen is clean — avoids a flash where the stale
                // interact page is visible with an empty input buffer.
                if self.panes_visible && !self.pages.is_empty() {
                    self.active_page =
                        PanePage::remove(&mut self.pages, "interact", self.active_page);
                }
                self.send_message(term).await?;
                while let Some(queued) = self.queue.pop_front() {
                    self.input.buffer = queued;
                    self.input.cursor_pos = self.input.buffer.chars().count();
                    self.send_message(term).await?;
                }
                Ok(())
            }
            InputAction::ClearQueue => {
                self.queue.clear();
                self.redraw(term)
            }
            InputAction::CycleMode => {
                self.approval_mode = self.approval_mode.cycle();
                self.user_config.approval_mode = self.approval_mode;
                self.persist_runtime_config();
                self.redraw(term)
            }
            InputAction::Redraw | InputAction::Escape => {
                // Mid-burst (non-bracketed paste flood on Windows conhost):
                // the buffer insert already happened in apply_key; defer the
                // expensive autocomplete recompute + redraw until the final
                // buffered event so a large paste costs one redraw instead of
                // one per character. paste_mode is set iff more events are
                // already queued behind this one.
                if self.input.paste_mode {
                    return Ok(());
                }
                self.update_autocomplete();
                self.redraw(term)
            }
            InputAction::OpenEditor => self.open_editor(term).await,
            InputAction::None => Ok(()),
        }
    }

    /// Handle a keypress while a blocking prompt is displayed.
    fn handle_prompt_key(&mut self, code: KeyCode, term: &mut BoneTerminal) -> io::Result<()> {
        self.navigate_prompt(code, true, term).map(|_| ())
    }

    fn navigate_prompt(
        &mut self,
        code: KeyCode,
        allow_peek: bool,
        term: &mut BoneTerminal,
    ) -> io::Result<bool> {
        let Some(prompt) = self.active_prompt.as_mut() else {
            return Ok(false);
        };
        match code {
            KeyCode::Up => prompt.up(),
            KeyCode::Down => prompt.down(),
            KeyCode::PageUp => prompt.page_up(),
            KeyCode::PageDown => prompt.page_down(),
            KeyCode::Char('p') | KeyCode::Char('P') if allow_peek => prompt.toggle_peek(),
            _ => return Ok(false),
        }
        self.refresh_approval_pane(term);
        self.redraw(term)?;
        Ok(true)
    }

    /// Request app exit. When sub-agent jobs are still running, the first
    /// request is blocked and returns a warning notice; a repeated request
    /// quits anyway (jobs are detached tasks and die with the process).
    fn request_quit(&mut self) -> Option<String> {
        let running = crate::ext::jobs::registry().running_jobs();
        if !running.is_empty() && !self.quit_despite_jobs {
            self.quit_despite_jobs = true;
            let names: Vec<String> = running
                .iter()
                .map(|j| format!("{} ({})", j.agent, j.id))
                .collect();
            return Some(format!(
                "{} sub-agent job(s) still running: {}. Quit again to exit anyway (they will be terminated).",
                running.len(),
                names.join(", ")
            ));
        }
        // Best-effort end conversation in DB
        if let Some(ref db) = self.session_db
            && let Some(conv_id) = self.conversation_id
        {
            db.end_conversation(conv_id).ok();
        }
        self.should_quit = true;
        None
    }

    /// Handle Ctrl+C: cancel streaming response, or quit on double-tap.
    fn handle_ctrl_c(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let now = Instant::now();
        let double_tap = self
            .last_ctrl_c
            .is_some_and(|prev| now.duration_since(prev) < Duration::from_secs(1));

        if double_tap {
            if let Some(notice) = self.request_quit() {
                self.messages.push(Message::system(notice));
                self.renderer
                    .flush_new_to_scrollback(&self.messages, term)?;
                self.last_ctrl_c = Some(now);
                return self.redraw(term);
            }
            return Ok(());
        }

        self.last_ctrl_c = Some(now);

        if self.streaming || self.live_command {
            self.cancel_streaming = true;
        }
        self.queue.clear();

        self.redraw(term)?;
        Ok(())
    }

    fn handle_advising_input_event(
        &mut self,
        event: Event,
        term: &mut BoneTerminal,
    ) -> io::Result<Option<Decision>> {
        let mut next = Some(event);
        while let Some(event) = next {
            next = None;
            match event {
                Event::Paste(text) => {
                    self.input.insert_paste(&text);
                    self.redraw(term)?;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let result = apply_input_key_with_paste_burst(&mut self.input, key)?;
                    next = result.trailing;
                    match result.action {
                        InputAction::Submit => {
                            let advice = self.input.expanded().trim().to_string();
                            self.input.reset();
                            return Ok(Some(Decision::Advise(advice)));
                        }
                        InputAction::Cancel | InputAction::Escape => {
                            self.input.clear_buffer();
                            return Ok(Some(Decision::Cancel));
                        }
                        InputAction::Redraw => self.redraw(term)?,
                        InputAction::None if key.code == KeyCode::Enter => {
                            return Ok(Some(Decision::Advise(String::new())));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// Show an approval prompt for a tool call in the fixed bottom pane and
    /// store the gate's `reply` channel. The decision is collected later by
    /// [`Self::drain_approval_keys`] from inside the main stream loop, so the
    /// spinner, streaming events, and subagent panes stay live while the user
    /// decides (no nested blocking event loop).
    pub(crate) fn begin_approval(
        &mut self,
        call: &ToolCall,
        id: u64,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let summary = match call.name.as_str() {
            "read_file" | "write_file" | "edit_file" => {
                call.arguments["path"].as_str().unwrap_or("?").to_string()
            }
            "shell" => call.arguments["command"]
                .as_str()
                .unwrap_or("?")
                .to_string(),
            _ => call.name.clone(),
        };

        let is_shell = call.name == "shell";
        let title = if is_shell {
            call.arguments["display_label"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| {
                    call.arguments["command"]
                        .as_str()
                        .unwrap_or("?")
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(80)
                        .collect::<String>()
                })
        } else {
            summary
        };
        let mut prompt = Prompt::new(
            format!("{} — {}", call.name, title),
            vec!["Accept", "Advise", "Cancel"],
        );
        prompt.full_command = if is_shell {
            call.arguments["command"].as_str().map(String::from)
        } else {
            None
        };
        self.active_prompt = Some(prompt);
        self.pending_approval = Some(PendingApproval {
            id,
            advising: false,
        });
        self.timer_pause();
        self.refresh_approval_pane(term);
        self.redraw(term)
    }

    /// (Re)build the tool-approval prompt as a live pane (source `"approval"`)
    /// in the pane region, so it renders alongside reasoning/tool/subagent panes
    /// — the same place `/config` and other interactive menus live. Called on
    /// every state change (selection, peek toggle, entering advise mode) so the
    /// pane stays in sync. No-op when no approval is pending.
    pub(crate) fn refresh_approval_pane(&mut self, term: &BoneTerminal) {
        let Some(prompt) = self.active_prompt.as_ref() else {
            return;
        };
        let advising = self.pending_approval.as_ref().is_some_and(|p| p.advising);
        let width = term.size().map(|s| s.width).unwrap_or(80);
        let content =
            crate::ui::render::approval_pane_lines(&self.renderer.theme, prompt, advising, width);
        let visible_rows = content.len();
        let page = PanePage {
            source: "approval".to_string(),
            title: "approval".to_string(),
            content,
            visible_rows,
            scroll: 0,
        };
        self.panes_visible = true;
        let (_, active) = PanePage::upsert(&mut self.pages, self.active_page, page);
        self.active_page = active;
    }

    /// Remove the live approval pane. Idempotent.
    pub(crate) fn clear_approval_pane(&mut self) {
        self.active_page = PanePage::remove(&mut self.pages, "approval", self.active_page);
    }

    /// Drain pending terminal events into the active approval prompt. Called
    /// once per main-loop iteration while `pending_approval` is `Some`. On a
    /// final choice it resolves the gate and clears the prompt; otherwise it
    /// only updates selection/advice and returns, leaving the loop to pump.
    pub(crate) fn drain_approval_keys(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        while self.pending_approval.is_some() && event::poll(std::time::Duration::from_millis(0))? {
            let event = event::read()?;
            let advising = self.pending_approval.as_ref().is_some_and(|p| p.advising);
            if advising {
                if let Some(decision) = self.handle_advising_input_event(event, term)? {
                    self.resolve_approval(decision, term)?;
                }
                continue;
            }
            if let Event::Paste(_) = event {
                continue;
            }
            let Event::Key(key) = event else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Char('p')
                | KeyCode::Char('P') => {
                    self.navigate_prompt(key.code, true, term)?;
                }
                KeyCode::Enter => {
                    let decision = self
                        .active_prompt
                        .as_ref()
                        .map_or(Decision::Cancel, Prompt::decision);
                    if matches!(decision, Decision::Advise(_)) {
                        // Switch to free-form advice entry; stay pending. Keep
                        // `active_prompt` set so the approval pane keeps showing
                        // the tool context above the "type advice" instruction.
                        if let Some(p) = self.pending_approval.as_mut() {
                            p.advising = true;
                        }
                        self.refresh_approval_pane(term);
                        self.redraw(term)?;
                    } else {
                        self.resolve_approval(decision, term)?;
                    }
                }
                KeyCode::Esc => self.resolve_approval(Decision::Cancel, term)?,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.resolve_approval(Decision::Cancel, term)?;
                }
                KeyCode::Char(c)
                    if key.modifiers.is_empty()
                        && self
                            .active_prompt
                            .as_ref()
                            .is_some_and(|prompt| prompt.selected == 1) =>
                {
                    self.input.insert_char(c);
                    if let Some(p) = self.pending_approval.as_mut() {
                        p.advising = true;
                    }
                    self.refresh_approval_pane(term);
                    self.redraw(term)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Send the final [`CallOutcome`] to the waiting tool, clear prompt state,
    /// and resume the turn timer. A `Cancel` aborts the streaming turn; the
    /// loop's top-of-iteration check propagates the cancel to running tools.
    fn resolve_approval(&mut self, decision: Decision, term: &mut BoneTerminal) -> io::Result<()> {
        if let Some(pending) = self.pending_approval.take() {
            let resolved = match decision {
                Decision::Accept => CallOutcome::Approve,
                Decision::Advise(advice) => CallOutcome::Blocked(format!(
                    "[exit_code=1] Tool not executed. User advice: {advice}"
                )),
                Decision::Cancel => {
                    self.cancel_streaming = true;
                    CallOutcome::Denied
                }
            };
            let _ = self
                .command_tx
                .send(crate::runtime::RuntimeCommand::ApprovalReply {
                    id: pending.id,
                    outcome: resolved,
                });
        }
        self.active_prompt = None;
        self.clear_approval_pane();
        self.timer_resume();
        self.redraw(term)
    }

    pub(super) async fn handle_command(
        &mut self,
        input: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        let cmd = parts[0].to_string();
        let arg = parts.get(1).copied().unwrap_or("").to_string();
        let has_lua_config = self
            .extensions
            .commands()
            .iter()
            .any(|registered| registered.name == "config");

        if has_lua_config
            && cmd == "provider"
            && arg.is_empty()
            && self
                .run_lua_command("config", "providers", term)
                .await
                .is_some()
        {
            return Ok(());
        }
        if has_lua_config && cmd == "tools" {
            let config_arg = if arg.trim() == "reload" {
                "tools reload"
            } else {
                "tools"
            };
            if self
                .run_lua_command("config", config_arg, term)
                .await
                .is_some()
            {
                return Ok(());
            }
        }
        if has_lua_config
            && cmd == "config"
            && self.run_lua_command("config", &arg, term).await.is_some()
        {
            return Ok(());
        }

        // Protected built-ins always win over Lua commands.
        if !commands::is_protected_builtin(cmd.as_str()) && self.extensions.is_available() {
            // Check if the lua command is enabled in commands config.
            // If the commands page is absent or empty, treat all registered commands as enabled
            // (same fallback semantics as tools).
            let all_command_names: Vec<String> = self
                .extensions
                .commands()
                .iter()
                .map(|c| c.name.clone())
                .collect();
            let enabled_commands = self.custom_configs.enabled_command_names();
            let enabled = if enabled_commands.is_empty() {
                all_command_names
            } else {
                enabled_commands
            };
            if enabled.contains(&cmd)
                && let Some(_reply) = self.run_lua_command(&cmd, &arg, term).await
            {
                return Ok(());
            }
        }

        if matches!(cmd.as_str(), "clear" | "new") {
            return self.clear_chat(term).await;
        }
        if cmd == "stats" {
            return self.open_stats_dashboard(term);
        }
        if cmd == "setup" {
            return self.open_setup_wizard(term);
        }
        if cmd == "catalog" {
            return self.open_catalog(term);
        }

        let prev_provider = self.view.provider_id.clone();
        let prev_model = self.view.provider_model.clone();

        let lua_cmds: Vec<(String, String)> = if self.extensions.is_available() {
            self.extensions
                .commands()
                .iter()
                .map(|c| (c.name.clone(), c.description.clone()))
                .collect()
        } else {
            Vec::new()
        };

        // Handle /provider and /model by telling the daemon to switch, then
        // reading the authoritative provider info from the StateSnapshot.
        let reply = match cmd.as_str() {
            "model" => {
                if arg.is_empty() {
                    format!("{} ({})", self.view.provider_model, self.view.provider_id)
                } else {
                    let provider_id = self.view.provider_id.clone();
                    // Update the model in custom config for this provider.
                    if let Some(mut entry) = self.custom_configs.get_provider_entry("providers", &provider_id) {
                        entry.model = arg.to_string();
                        self.custom_configs.set_provider_entry("providers", &provider_id, &entry);
                    }
                    let _ = self.command_tx.send(crate::runtime::RuntimeCommand::SwitchProvider {
                        provider_id,
                    });
                    self.await_state_snapshot().await;
                    format!("Switched to {} ({})", self.view.provider_model, self.view.provider_id)
                }
            }
            "provider" => {
                if arg.is_empty() {
                    let mut lines = vec![format!("Current: {} ({})", self.view.provider_model, self.view.provider_id)];
                    let providers = self.custom_configs.derive_providers_config().providers;
                    if providers.is_empty() {
                        lines.push("No providers configured. Edit ~/.bone-rust/config/providers.yaml".to_string());
                    } else {
                        lines.push("Available:".to_string());
                        for (id, entry) in &providers {
                            let marker = if id == &self.view.provider_id { " *" } else { "" };
                            lines.push(format!("  {} — {} ({}){}", id, entry.label, entry.model, marker));
                        }
                    }
                    lines.join("\n")
                } else {
                    self.custom_configs.set_last_provider(&arg);
                    let _ = self.command_tx.send(crate::runtime::RuntimeCommand::SwitchProvider {
                        provider_id: arg.to_string(),
                    });
                    self.await_state_snapshot().await;
                    format!("Switched to {} ({})", self.view.provider_model, self.view.provider_id)
                }
            }
            "help" => commands::help(&lua_cmds),
            "quit" | "exit" => {
                if let Some(notice) = self.request_quit() {
                    self.messages.push(Message::system(notice));
                    self.renderer.flush_new_to_scrollback(&self.messages, term)?;
                    self.redraw(term)?;
                }
                return Ok(());
            }
            "edit" | "e" => {
                return self.open_editor(term).await;
            }
            _ => format!("Unknown command: /{cmd}. Type /help for available commands."),
        };

        // If provider or model identity changed, start a new conversation.
        if self.view.provider_id != prev_provider || self.view.provider_model != prev_model {
            self.start_new_conversation().await;
        }

        self.messages.push(Message::system(reply));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)?;
        Ok(())
    }

    /// Run a Lua-registered command. Returns `Some(())` if the command was found and handled.
    /// Snapshot the app-derived `ctx` fields for the current conversation.
    /// Shared by the command runner, the tool dispatch path, and `before_turn`
    /// so every Lua entry point sees an identical `ctx`.
    pub(super) fn app_ctx_state(&self) -> crate::ext::ctx::AppCtxState {
        crate::ext::ctx::AppCtxState::new(
            &self.tools,
            &self.token_stats,
            &self.approval_mode,
            self.conversation_id,
            &self.view.provider_id,
            &self.view.provider_model,
            self.usage_by_provider.clone(),
            self.messages
                .iter()
                .map(|m| ChatMessage::new(m.role, &m.content))
                .collect(),
        )
    }

    async fn run_lua_command(
        &mut self,
        cmd: &str,
        arg: &str,
        term: &mut BoneTerminal,
    ) -> Option<()> {
        // Attached to a remote daemon: run the command on its Lua VM over the
        // protocol instead of the local one. Every `run_lua_command` call site
        // in `handle_command` routes through here, so this is the single switch.
        if self.is_remote {
            return self.run_remote_command(cmd, arg, term).await;
        }
        let lua = self.extensions.lua_handle();
        let shared_ui = self.extensions.ui_handle();
        let cmd_owned = cmd.to_string();
        let arg_owned = arg.to_string();
        let app_state = self.app_ctx_state();
        // Shared cancel flag: wired into the command's ctx (so Lua can observe
        // cancellation) and flipped by `drive_live` when the user hits Esc.
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        self.live_command = true;
        self.cancel_streaming = false;
        // Seed the turn timer so the status bar shows elapsed time while the
        // command works (mirrors a normal streamed turn).
        self.turn_start = Some(std::time::Instant::now());
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;

        // Run the command through the shared live-driver loop so it gets the
        // same capabilities as tools, including `ctx.ui.key()`, which needs
        // the loop to pump pane events and deliver keystrokes to Lua.
        let cancel_for_ctx = cancel.clone();
        let reply = self
            .drive_live(
                move |events| async move {
                    tokio::task::spawn_blocking(move || {
                        let lua = lua.lock().unwrap_or_else(|e| e.into_inner());

                        // Find the command handler using the shared lookup.
                        let handler = match crate::ext::ops_commands::find_handler(&lua, &cmd_owned)
                        {
                            Some(f) => f,
                            None => return Some(None),
                        };

                        let config_dir = crate::config::bone_dir().to_string_lossy().to_string();
                        let shared_state = crate::ext::ctx::process_shared_state();
                        let mut ctx_cfg = crate::ext::ctx::CtxConfig::new(config_dir, shared_state);
                        app_state.apply_to(&mut ctx_cfg);
                        ctx_cfg.pane_sender = Some(events);
                        ctx_cfg.ui = Some(shared_ui.clone());
                        ctx_cfg.cancelled = Some(cancel_for_ctx);
                        let ctx_table = crate::ext::ctx::create_ctx_table(&lua, &ctx_cfg).ok()?;

                        // Release the project Lua mutex before calling into Lua: a
                        // nested LuaTool invocation via ctx.tools.call runs inline on
                        // this thread and must re-acquire it (std::sync::Mutex is not
                        // reentrant).
                        drop(lua);

                        let result = handler.call::<mlua::Value>((arg_owned, ctx_table));

                        let reply = match result {
                            Ok(value) => crate::ext::types::parse_lua_command_return(value)
                                .map(|ret| (ret.output, ret.submit, ret.action, ret.display_role)),
                            Err(e) => {
                                eprintln!("bone-lua error: command '{cmd_owned}': {e}");
                                Some((format!("Lua command error: {e}"), false, None, None))
                            }
                        };
                        Some(reply)
                    })
                    .await
                    .ok()
                    .flatten()
                },
                term,
                cancel.clone(),
                || None,
            )
            .await
            .ok()
            .flatten();

        self.live_command = false;
        self.cancel_streaming = false;
        // Tear down the turn timer seeded above; otherwise the status bar keeps
        // ticking after the command returns (mirrors the streaming teardown).
        self.turn_start = None;
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;

        if let Some(Some((mut reply, mut submit, action, display_role))) = reply {
            // A reply-bearing action (config_action) yields a status reply
            // that must be displayed, not submitted as a user turn. Force
            // submit=false so the local path can't diverge from RPC.
            submit &= action
                .as_ref()
                .and_then(|a| a.config_action.as_ref())
                .is_none();
            if let Some(action) = action
                && let Ok(Some(action_reply)) = self.apply_lua_action(action, term).await
            {
                reply = action_reply;
            }
            if submit {
                let display = format!(
                    "/{cmd}{}",
                    if arg.is_empty() {
                        "".to_string()
                    } else {
                        format!(" {arg}")
                    }
                );
                self.submit_user_turn(reply, Some(display), Vec::new(), term)
                    .await
                    .ok();
            } else {
                if display_role.as_deref() == Some("assistant") {
                    self.show_assistant_reply(reply, term).ok();
                } else {
                    self.show_reply(reply, term).ok();
                }
            }
        } else {
            self.redraw(term).ok();
        }

        Some(())
    }

    fn show_reply(&mut self, reply: impl Into<String>, term: &mut BoneTerminal) -> io::Result<()> {
        self.messages.push(Message::system(reply.into()));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)
    }

    fn show_assistant_reply(
        &mut self,
        reply: impl Into<String>,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        self.messages.push(Message::assistant(reply.into()));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)
    }

    /// Launch `<bone-exe> <subcommand>` in a tmux `display-popup` sized
    /// `width`×`height`, redrawing afterward. Returns the popup's exit status,
    /// or `None` when not in a responsive tmux or the popup failed to launch (so
    /// the caller falls back to its inline fullscreen path). Shared by the stats
    /// dashboard and setup wizard, the two popup-capable fullscreen entries.
    fn try_tmux_popup(
        &mut self,
        subcommand: &str,
        width: &str,
        height: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<Option<std::process::ExitStatus>> {
        let tmux_ok = std::env::var_os("TMUX").is_some()
            && std::process::Command::new("tmux")
                .args(["display-message", "-p", "ok"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
        if !tmux_ok {
            return Ok(None);
        }
        let exe = std::env::current_exe()?;
        let cmd = format!("{} {subcommand}", shell_quote(&exe.to_string_lossy()));
        let result = std::process::Command::new("tmux")
            .args(["display-popup", "-E", "-w", width, "-h", height])
            .arg(cmd)
            .status();
        self.force_redraw(term)?;
        Ok(result.ok())
    }

    fn open_stats_dashboard(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        if self.is_remote {
            return self.show_reply(
                "Stats dashboard is not available in remote mode.".to_string(),
                term,
            );
        }
        if let Some(status) = self.try_tmux_popup("stats-popup", "96%", "92%", term)?
            && status.success()
        {
            return Ok(());
        }

        if self.session_db.is_none() {
            return self.show_reply("Stats database is not available.".to_string(), term);
        }
        let result = {
            let db = self.session_db.as_ref().unwrap();
            crate::ui::stats::run(|range| match range {
                None => db
                    .usage_stats_snapshot()
                    .map_err(|err| io::Error::other(err.to_string())),
                Some(r) => db
                    .usage_stats_range(&r.start, &r.end)
                    .map_err(|err| io::Error::other(err.to_string())),
            })
        };

        self.force_redraw(term)?;
        if let Err(err) = result {
            return self.show_reply(format!("Stats dashboard failed: {err}"), term);
        }
        Ok(())
    }

    /// Rebuild the Lua VM and tool registry from disk in place (the
    /// `/tools reload` and post-`/catalog` hot-reload path). Returns a summary
    /// line. Lets catalog installs/removes take effect without restarting bone.
    fn reload_extensions(&mut self) -> String {
        let config_dir = crate::config::bone_dir();
        let cwd = std::env::current_dir().unwrap_or_default();
        let mut custom = config::custom::CustomConfigs::load();
        let booted = crate::ext::boot_with_tools(
            &config_dir,
            &cwd,
            &mut custom,
            true,
            crate::ext::BootOptions {
                agent_depth: 0,
                headless: false,
                model: self.view.provider_model.clone(),
                provider: self.view.provider_id.clone(),
                tool_allowlist: None,
            },
            &self.view.provider_model,
            &self.view.provider_id,
        );
        self.extensions = booted.manager;
        self.tools = booted.tools;
        self.user_config.apply_custom_configs(&custom);
        self.approval_mode = self.user_config.approval_mode;
        self.custom_configs = custom;
        self.user_config.enabled_tools = self.tools.enabled_names();
        let count = self.tools.definitions().len();
        // In-process: hand the daemon a clone of what we just booted (shared
        // Lua VM) via the inbox so it adopts it instead of re-reading disk. In
        // remote mode there's no inbox — the remote daemon disk-boots on the
        // command (it's a separate process with its own VM).
        if let Some(inbox) = &self.reload_inbox {
            *inbox.lock().unwrap_or_else(|e| e.into_inner()) = Some(crate::ext::BootedTools {
                manager: self.extensions.clone(),
                tools: self.tools.clone(),
            });
        }
        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::ReloadExtensions);
        format!("Tools and Lua extensions reloaded. {count} tools enabled.")
    }

    fn open_catalog(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        // Prefer a tmux popup (same as /setup); fall back to an inline takeover.
        // `bone catalog` exits 0 when something changed, 2 when nothing did —
        // both are completed runs, so only a failed launch falls through.
        if let Some(status) = self.try_tmux_popup("catalog", "96%", "92%", term)? {
            match status.code() {
                Some(0) => {
                    // Files were written by the subprocess; hot-reload them here
                    // so the changes take effect without a restart.
                    let reloaded = self.reload_extensions();
                    return self.show_reply(format!("Catalog updated. {reloaded}"), term);
                }
                Some(2) => return self.show_reply("Catalog: no changes.".to_string(), term),
                _ => {} // popup failed to launch — fall through to inline
            }
        }

        let result = crate::ui::catalog::run();
        self.force_redraw(term)?;
        match result {
            Ok(outcome) => {
                let msg = if outcome.changed {
                    let reloaded = self.reload_extensions();
                    format!("{} {reloaded}", outcome.message)
                } else {
                    outcome.message
                };
                self.show_reply(msg, term)
            }
            Err(err) => self.show_reply(format!("Catalog failed: {err}"), term),
        }
    }

    fn open_setup_wizard(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let mut ran = false;
        if let Some(status) = self.try_tmux_popup("setup", "96%", "92%", term)? {
            match status {
                s if s.success() => ran = true,
                // `bone setup` exits 2 when the user cancels the wizard.
                s if s.code() == Some(2) => {
                    return self.show_reply("Setup cancelled.".to_string(), term);
                }
                // Popup failed to launch — fall through to the inline wizard.
                _ => {}
            }
        }

        if !ran {
            let result = crate::ui::setup::run(false);
            self.force_redraw(term)?;
            match result {
                Ok(true) => {}
                Ok(false) => return self.show_reply("Setup cancelled.".to_string(), term),
                Err(err) => {
                    return self.show_reply(format!("Setup wizard failed: {err}"), term);
                }
            }
        }

        self.show_reply(
            format!(
                "Setup saved to {}. Restart bone to load the new tools and commands.",
                crate::config::bone_dir().display()
            ),
            term,
        )
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Apply a Lua config snapshot to the Rust `UserConfig`.
/// Lua values override YAML config values.
fn apply_lua_config_snapshot(
    cfg: &mut crate::config::UserConfig,
    snapshot: &crate::ext::snapshots::LuaConfigSnapshot,
) {
    if let Some(ref approval_mode) = snapshot.approval_mode {
        cfg.approval_mode = match approval_mode.as_str() {
            "danger" => crate::tools::ApprovalMode::Danger,
            _ => crate::tools::ApprovalMode::Safe,
        };
    }

    // Merge status_show — Lua values override, missing keys keep defaults.
    if !snapshot.status_show.is_empty() {
        for (k, v) in &snapshot.status_show {
            cfg.status_show.insert(k.clone(), *v);
        }
    }

    // Spinner/text presets from the seeded ui.spinners lib.
    if !snapshot.spinners.is_empty() {
        cfg.spinner_styles = snapshot.spinners.clone();
    }
    if !snapshot.texts.is_empty() {
        cfg.spinner_texts = snapshot.texts.clone();
    }
}
