//! Main TUI application: event loop, state, and turn orchestration.

mod editor;
mod keymap;
mod paste;
pub mod stream;

use paste::{apply_input_key_with_paste_burst, collect_paste_burst, is_paste_burst, plain_char};

use crate::chat::Message;
use crate::config::{self, UserConfig};
use crate::llm::{ChatMessage, LlmProvider};

use crate::tools::{ApprovalMode, CallOutcome, ToolCall, ToolResult};
use crate::ui::tool_display::build_tool_row;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::Instant;
use tokio::time::Duration;

use super::autocomplete::AutocompleteState;
use super::commands;
use super::input::{InputAction, InputState};
use super::pane_page::PanePage;
use super::prompt::{Decision, Prompt};
use super::render::{BoneTerminal, MAX_PANE_ROWS, MIN_ROWS, PaneDraw, Renderer, StatusInfo};

fn should_open_agent_log(input: &InputState) -> bool {
    input.buffer.trim().is_empty()
}

fn background_pane_needs_refresh(
    processes_running: bool,
    processes_pane_visible: bool,
    agent_jobs_tick_due: bool,
) -> bool {
    processes_running || processes_pane_visible || agent_jobs_tick_due
}

fn finish_queue_edit(
    queue: &mut VecDeque<String>,
    queue_selected: &mut usize,
    queue_editing: &mut Option<(usize, String)>,
    input: &mut InputState,
    save: bool,
) -> bool {
    let Some((index, original)) = queue_editing.take() else {
        return false;
    };
    let edited = input.expanded().trim().to_string();
    let text = if save && !edited.is_empty() {
        edited
    } else {
        original
    };
    queue.insert(index.min(queue.len()), text);
    *queue_selected = index.min(queue.len() - 1);
    input.clear_buffer();
    true
}

/// Result of an agents-pane key handled by [`apply_agents_nav_key`].
pub(crate) enum AgentsKeyResult {
    Unhandled,
    SelectionChanged,
    OpenJob,
    Cancelled(String),
}

/// Shared queue-pane navigation for idle `handle_key` and streaming `drain_keys`.
/// Mutates queue/selection/edit state; returns true when the key was consumed.
pub(crate) fn apply_queue_nav_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    queue: &mut VecDeque<String>,
    queue_selected: &mut usize,
    queue_editing: &mut Option<(usize, String)>,
    input: &mut InputState,
) -> bool {
    if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return false;
    }
    let index = (*queue_selected).min(queue.len().saturating_sub(1));
    match code {
        KeyCode::Up if modifiers.contains(KeyModifiers::SHIFT) && index > 0 => {
            queue.swap(index, index - 1);
            *queue_selected = index - 1;
            true
        }
        KeyCode::Down if modifiers.contains(KeyModifiers::SHIFT) && index + 1 < queue.len() => {
            queue.swap(index, index + 1);
            *queue_selected = index + 1;
            true
        }
        KeyCode::Up if modifiers.is_empty() => {
            *queue_selected = index.saturating_sub(1);
            true
        }
        KeyCode::Down if modifiers.is_empty() => {
            *queue_selected = (index + 1).min(queue.len().saturating_sub(1));
            true
        }
        KeyCode::Enter if modifiers.is_empty() && input.buffer.trim().is_empty() => {
            if let Some(text) = queue.remove(index) {
                queue.push_front(text);
                *queue_selected = 0;
            }
            true
        }
        KeyCode::F(2) if modifiers.is_empty() && input.buffer.is_empty() => {
            if let Some(text) = queue.remove(index) {
                input.buffer = text.clone();
                input.cursor_pos = input.buffer.chars().count();
                *queue_editing = Some((index, text));
            }
            true
        }
        KeyCode::Delete if modifiers.is_empty() => {
            queue.remove(index);
            *queue_selected = index.min(queue.len().saturating_sub(1));
            true
        }
        _ => false,
    }
}

/// Shared agents/jobs-pane navigation for idle `handle_key` and streaming `drain_keys`.
pub(crate) fn apply_agents_nav_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    selected_job_id: &mut Option<String>,
    input: &InputState,
) -> AgentsKeyResult {
    if !modifiers.is_empty() {
        return AgentsKeyResult::Unhandled;
    }
    let jobs = crate::ext::jobs::registry().running_jobs();
    let current = selected_job_id
        .as_deref()
        .and_then(|id| jobs.iter().position(|j| j.id == id))
        .unwrap_or(0);
    match code {
        KeyCode::Up if !jobs.is_empty() => {
            *selected_job_id = Some(jobs[current.saturating_sub(1)].id.clone());
            AgentsKeyResult::SelectionChanged
        }
        KeyCode::Down if !jobs.is_empty() => {
            *selected_job_id = Some(jobs[(current + 1).min(jobs.len() - 1)].id.clone());
            AgentsKeyResult::SelectionChanged
        }
        KeyCode::Enter if should_open_agent_log(input) => AgentsKeyResult::OpenJob,
        KeyCode::Char('k') => selected_job_id
            .clone()
            .map_or(AgentsKeyResult::Unhandled, AgentsKeyResult::Cancelled),
        _ => AgentsKeyResult::Unhandled,
    }
}

/// Shared pane Tab / PageUp / PageDown / Ctrl+Up / Ctrl+Down navigation.
/// Returns true when the key was consumed.
pub(crate) fn apply_pane_nav_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    pages: &mut [PanePage],
    active_page: &mut usize,
) -> bool {
    if pages.is_empty() {
        return false;
    }
    *active_page = (*active_page).min(pages.len() - 1);
    match (code, modifiers) {
        (KeyCode::Tab, m) if m.is_empty() => {
            *active_page = (*active_page + 1) % pages.len();
            true
        }
        (KeyCode::PageUp, m) if m.is_empty() => {
            let page = &mut pages[*active_page];
            page.scroll = page.scroll.saturating_sub(MAX_PANE_ROWS);
            true
        }
        (KeyCode::PageDown, m) if m.is_empty() => {
            let page = &mut pages[*active_page];
            page.scroll = (page.scroll + MAX_PANE_ROWS).min(page.max_scroll());
            true
        }
        (KeyCode::Up, m) if m.contains(KeyModifiers::CONTROL) => {
            let page = &mut pages[*active_page];
            page.scroll = page.scroll.saturating_sub(1);
            true
        }
        (KeyCode::Down, m) if m.contains(KeyModifiers::CONTROL) => {
            let page = &mut pages[*active_page];
            page.scroll = (page.scroll + 1).min(page.max_scroll());
            true
        }
        _ => false,
    }
}

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

fn lua_config_available(commands: &[(String, String)]) -> bool {
    commands.iter().any(|(name, _)| name == "config")
}

fn configured_input_style(
    snapshot: &crate::ext::snapshots::InputStyleSnapshot,
    preset: Option<&str>,
) -> super::render::InputStyle {
    let mut snapshot = snapshot.clone();
    if let Some(preset) = preset {
        snapshot.preset = Some(preset.to_string());
    }
    super::render::InputStyle::from_snapshot(&snapshot)
}

#[derive(Default)]
struct ConfigView {
    schema: Option<bone_protocol::ConfigSchema>,
    snapshot: Option<bone_protocol::ConfigSnapshot>,
}

impl ConfigView {
    fn revision(&self) -> u64 {
        self.snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.revision)
    }

    fn field(&self, path: &str) -> Option<&bone_protocol::SettingDefinition> {
        fn find<'a>(
            pages: &'a [bone_protocol::ConfigPage],
            path: &str,
        ) -> Option<&'a bone_protocol::SettingDefinition> {
            pages.iter().find_map(|page| {
                page.fields
                    .iter()
                    .find(|field| field.path == path)
                    .or_else(|| find(&page.pages, path))
            })
        }
        self.schema
            .as_ref()
            .and_then(|schema| find(&schema.pages, path))
    }

    fn value(&self, field: &bone_protocol::SettingDefinition) -> serde_json::Value {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| {
                field
                    .path
                    .split('.')
                    .try_fold(&snapshot.values, |value, part| value.get(part))
                    .cloned()
            })
            .or_else(|| field.value.clone())
            .unwrap_or_else(|| field.default.clone())
    }

    fn disabled_commands(&self) -> &[String] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.disabled_commands.as_slice())
    }
}

fn parse_config_value(
    field: &bone_protocol::SettingDefinition,
    input: &str,
) -> Result<serde_json::Value, String> {
    fn validate_bounds(field: &bone_protocol::SettingDefinition, value: f64) -> Result<(), String> {
        if field.min.is_some_and(|min| value < min) {
            return Err(format!("must be at least {}", field.min.unwrap()));
        }
        if field.max.is_some_and(|max| value > max) {
            return Err(format!("must be at most {}", field.max.unwrap()));
        }
        Ok(())
    }

    match field.value_type.as_str() {
        "bool" => match input.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" => Ok(serde_json::Value::Bool(true)),
            "false" | "off" | "no" => Ok(serde_json::Value::Bool(false)),
            _ => Err("expected true/false, on/off, or yes/no".into()),
        },
        "number" => {
            if field.integer == Some(true) {
                let value = input
                    .parse::<i64>()
                    .map_err(|_| "expected an integer".to_string())?;
                validate_bounds(field, value as f64)?;
                Ok(serde_json::Value::from(value))
            } else {
                let value = input
                    .parse::<f64>()
                    .map_err(|_| "expected a number".to_string())?;
                if !value.is_finite() {
                    return Err("expected a finite number".into());
                }
                validate_bounds(field, value)?;
                Ok(serde_json::Value::from(value))
            }
        }
        "enum" => {
            if field.options.iter().any(|option| option == input) {
                Ok(serde_json::Value::String(input.into()))
            } else {
                Err(format!("expected one of: {}", field.options.join(", ")))
            }
        }
        "string" => Ok(serde_json::Value::String(input.into())),
        other => Err(format!("unsupported setting type `{other}`")),
    }
}

fn take_pending_config(
    pending: &mut BTreeMap<String, String>,
    request_id: Option<String>,
) -> Option<String> {
    request_id.and_then(|request_id| pending.remove(&request_id))
}

fn config_rejection_message(path: Option<String>, error: &str) -> String {
    let context = path.map_or(String::new(), |path| format!(" for {path}"));
    format!("Configuration change{context} rejected: {error}")
}

fn idle_state_needs_redraw(
    replaced_scrollback: bool,
    messages_before: usize,
    messages_after: usize,
    config_revision_before: u64,
    config_revision_after: u64,
) -> bool {
    replaced_scrollback
        || messages_after != messages_before
        || config_revision_after != config_revision_before
}

fn render_config_page(view: &ConfigView, requested: Option<&str>) -> Result<String, String> {
    fn collect<'a>(
        pages: &'a [bone_protocol::ConfigPage],
        requested: Option<&str>,
        out: &mut Vec<&'a bone_protocol::ConfigPage>,
    ) {
        for page in pages {
            if requested.is_none_or(|name| page.namespace == name) {
                out.push(page);
            }
            collect(&page.pages, requested, out);
        }
    }

    let schema = view
        .schema
        .as_ref()
        .ok_or_else(|| "Configuration is still loading; try again shortly.".to_string())?;
    let mut pages = Vec::new();
    collect(&schema.pages, requested, &mut pages);
    if pages.is_empty() {
        return Err(format!(
            "Unknown configuration page `{}`.",
            requested.unwrap_or_default()
        ));
    }
    let mut lines = Vec::new();
    for page in pages {
        if page.fields.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(page.title.clone());
        for field in &page.fields {
            let value = view.value(field);
            let rendered = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            let options = if field.options.is_empty() {
                String::new()
            } else {
                format!(" [{}]", field.options.join(" | "))
            };
            lines.push(format!(
                "  {} = {}{} — {}",
                field.path, rendered, options, field.label
            ));
        }
    }
    lines.push(String::new());
    lines.push("Set: /config set <path> <value>".into());
    lines.push("Reset: /config reset <path>".into());
    Ok(lines.join("\n"))
}

pub struct App {
    pub messages: Vec<Message>,
    /// Channel to send commands (SubmitPrompt, lifecycle, approvals) to the
    /// in-process daemon that owns the RuntimeSession.
    pub command_tx: tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeCommand>,
    /// Broadcast receiver for RuntimeEvents from the daemon.
    pub events_rx: tokio::sync::broadcast::Receiver<crate::runtime::RuntimeEvent>,
    /// Keeps the socket bridge to the daemon alive for the App's lifetime. Its
    /// `Drop` terminates the bridge's forwarding/writer tasks.
    _remote_client: Option<crate::rpc::RemoteClient>,
    /// Read-only session DB handle for stats queries (the daemon owns the
    /// authoritative connection through its own RuntimeSession).
    pub session_db: Option<crate::session_db::SessionDb>,
    pub input: InputState,
    pub streaming: bool,
    /// A live slash command is running through `run_remote_command`. This needs
    /// the same cancellation plumbing as streaming, but it is not a model turn
    /// and should not show the thinking spinner.
    pub live_command: bool,
    pub should_quit: bool,
    pub renderer: Renderer,
    pub user_config: UserConfig,
    pub custom_configs: config::custom::CustomConfigs,
    /// Latest daemon-owned typed schema/snapshot and correlated mutations.
    config_view: ConfigView,
    pending_config: BTreeMap<String, String>,
    next_config_request: u64,
    pub queue: VecDeque<String>,
    /// Selected row in the native input-queue pane.
    pub queue_selected: usize,
    /// Queue item temporarily removed for in-place editing: (original index, text).
    pub queue_editing: Option<(usize, String)>,

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
    /// Resolved key bindings supplied by the daemon's `FrontendState`.
    keymaps: crate::config::settings::KeymapSettings,
    /// Resolved input customization supplied by the daemon.
    lua_input_style: crate::ext::snapshots::InputStyleSnapshot,
    /// Slash commands `(name, description)` the daemon advertised via
    /// `FrontendState`, for autocomplete.
    wire_commands: Vec<(String, String)>,
    /// Tool definitions + display configs the daemon advertised via
    /// `FrontendState`, used to render tool rows + estimate context size.
    wire_tools: WireTools,
    /// Whether the boot banner (daemon `bone.banner()` + client hints) has been
    /// shown — it arrives with the first `FrontendState`, not at construction.
    banner_shown: bool,
    /// Lua status lines keyed by id (`bone.api.ui.set_statusline`), in
    /// registration order. Each id's segments are appended to the native
    /// status bar; re-setting the same id updates it in place.
    lua_status: Vec<(String, Vec<crate::runtime::view::StatusSegment>)>,
    /// Call IDs that already have a tool row in chat (to avoid duplicates).
    shown_tool_rows: std::collections::HashSet<String>,
    /// In-flight shell commands (call_id, formatted label, start time), shown as
    /// a transient strip above the input while running.
    running_shells: Vec<(String, String, std::time::Instant)>,
    /// Shell calls waiting for a display threshold before promotion to
    /// `running_shells`, so sub-second commands don't flash the strip.
    pending_shells: Vec<(String, String, std::time::Instant)>,
    /// Last-seen job-registry version (forces first-tick render).
    jobs_seen_version: u64,
    /// Last wall-clock jobs-pane refresh (drives the ~1s live ticker).
    jobs_last_refresh: std::time::Instant,
    /// Job selected in the native Agents pane.
    selected_job_id: Option<String>,
    /// Set after the user was warned that quitting kills running sub-agent
    /// jobs; the next quit request goes through.
    quit_despite_jobs: bool,
    /// True after OSC 11 changed the emulator background; reset on TUI handoff/exit.
    terminal_bg_set: bool,
}

impl App {
    /// Construct the TUI as a pure client attached to a runtime over in-process
    /// channels. The runtime (daemon) owns the Lua VM, tools, and session; the
    /// TUI pushes [`RuntimeCommand`]s and renders [`RuntimeEvent`]s. For a remote
    /// daemon, pass `daemon_client` to keep the socket bridge alive; the in-process
    /// path passes `None`. `provider` seeds the initial `SessionSnapshot` display
    /// strings.
    pub fn with_runtime_client(
        provider: std::sync::Arc<dyn LlmProvider>,
        user_config: UserConfig,
        custom_configs: config::custom::CustomConfigs,
        command_tx: tokio::sync::mpsc::UnboundedSender<crate::runtime::RuntimeCommand>,
        events_rx: tokio::sync::broadcast::Receiver<crate::runtime::RuntimeEvent>,
        daemon_client: Option<crate::rpc::RemoteClient>,
    ) -> io::Result<Self> {
        let model = provider.model().to_string();
        let approval_mode = user_config.approval_mode;

        // Renderer starts on the default theme; the daemon's `FrontendState`
        // applies the user's theme over the wire on attach.
        let renderer = Renderer::new();
        let mut messages = Vec::new();

        // Read-only DB handle for the App's stats queries (sqlite WAL supports
        // concurrent readers); the daemon owns the authoritative connection.
        let session_db = match crate::session_db::SessionDb::open(&crate::session_db::db_path()) {
            Ok(db) => Some(db),
            Err(err) => {
                messages.push(Message::system(format!(
                    "warning: failed to open session database: {err}"
                )));
                None
            }
        };

        // Seed the frontend view from the active provider; the daemon's
        // `StateSnapshot` fills in the conversation id and token totals.
        let view = crate::runtime::SessionSnapshot {
            provider_id: provider.id().to_string(),
            provider_model: model.clone(),
            conversation_id: None,
            ..Default::default()
        };

        let _ = command_tx.send(crate::runtime::RuntimeCommand::GetConfig);
        Ok(Self {
            messages,
            command_tx,
            events_rx,
            _remote_client: daemon_client,
            session_db,
            input: InputState::default(),
            streaming: false,
            live_command: false,
            should_quit: false,
            renderer,
            user_config,
            custom_configs,
            config_view: ConfigView::default(),
            pending_config: BTreeMap::new(),
            next_config_request: 0,
            queue: VecDeque::new(),
            queue_selected: 0,
            queue_editing: None,

            approval_mode,
            active_prompt: None,
            pending_approval: None,
            cancel_streaming: false,
            last_ctrl_c: None,
            stream_estimated_received: None,
            view,
            conversation_id: None,
            token_stats: crate::llm::TokenStats::default(),
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
            keymaps: crate::config::settings::KeymapSettings::default(),
            lua_input_style: crate::ext::snapshots::InputStyleSnapshot::default(),
            wire_commands: Vec::new(),
            wire_tools: WireTools::default(),
            banner_shown: false,
            lua_status: Vec::new(),
            shown_tool_rows: std::collections::HashSet::new(),
            running_shells: Vec::new(),
            pending_shells: Vec::new(),
            jobs_seen_version: u64::MAX,
            jobs_last_refresh: std::time::Instant::now(),
            selected_job_id: None,
            quit_despite_jobs: false,
            terminal_bg_set: false,
        })
    }

    /// Construct the TUI as a pure client of a `bone serve` daemon reached over
    /// `client`. The daemon is the sole Lua-VM owner; the TUI boots no VM and
    /// renders the daemon's state from the wire. Delegates to
    /// [`with_runtime_client`] after extracting the in-process channel handles
    /// from the remote client.
    pub fn with_daemon(
        llm: Box<dyn LlmProvider>,
        user_config: UserConfig,
        custom_configs: config::custom::CustomConfigs,
        client: crate::rpc::RemoteClient,
    ) -> io::Result<Self> {
        let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::from(llm);
        // Subscribe before any `.await` so the on-connect `FrontendState` /
        // `StateSnapshot` aren't missed.
        let command_tx = client.command_sender();
        let events_rx = client.subscribe();
        Self::with_runtime_client(
            provider,
            user_config,
            custom_configs,
            command_tx,
            events_rx,
            Some(client),
        )
    }

    /// Client-side banner hints (release + catalog update notices) appended below
    /// the daemon's `bone.banner()` text. These are local cached reads, not Lua.
    fn banner_client_hints() -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(notice) = crate::update_check::notice() {
            lines.push(notice);
        }
        let updates = crate::ext::catalog::updates_available();
        if updates > 0 {
            lines.push(format!(
                "{updates} catalog update{} available — run /catalog",
                if updates == 1 { "" } else { "s" }
            ));
        }
        lines
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
                self.reset_transient_ui_state(true);
                self.cancel_streaming = false;
                self.apply_snapshot(snapshot);
                self.messages.clear();
                let rows = self.rebuild_scrollback_from_transcript(&messages);
                self.messages.extend(rows);
                self.renderer.scrollback_cursor = 0;
            }
            RuntimeEvent::Status { message }
            | RuntimeEvent::ConversationLoadFailed { message, .. } => {
                self.messages.push(Message::system(message));
            }
            // The daemon's boot-time display state. Adopt it so the frontend
            // renders the daemon's theme/keymap/config/commands. Sent on attach
            // and after a remote `ReloadExtensions`.
            RuntimeEvent::FrontendState {
                banner,
                settings,
                commands,
                tool_defs,
                tool_display,
                subagents: _,
            } => {
                self.apply_frontend_state(banner, settings, commands, tool_defs, tool_display);
            }
            RuntimeEvent::ConfigSnapshot { schema, snapshot } => {
                self.apply_config_snapshot(schema, snapshot);
            }
            RuntimeEvent::ConfigChanged {
                schema,
                snapshot,
                restart_required,
                request_id,
                ..
            } => {
                let completed = self.finish_config_change(schema, snapshot, request_id);
                if let Some(path) = completed {
                    self.messages
                        .push(Message::system(format!("Configuration updated: {path}.")));
                }
                if restart_required {
                    self.messages.push(Message::system(
                        "Configuration saved. Restart required to apply this change.",
                    ));
                }
            }
            RuntimeEvent::ConfigMutationRejected {
                current_revision,
                error,
                request_id,
            } => {
                let path = self.reject_config_change(current_revision, request_id);
                self.messages
                    .push(Message::system(config_rejection_message(path, &error)));
            }
            // Pane/UI diff from the daemon (e.g. a command's pane between turns).
            // Both in-process and remote clients receive these via the event bus
            // when `forward_view_diffs` is enabled.
            RuntimeEvent::ViewDiff { diff } => {
                self.apply_view_diff(diff);
            }
            // All other events are turn-scoped and ignored in idle.
            _ => {}
        }
    }

    fn apply_config_snapshot(
        &mut self,
        schema: bone_protocol::ConfigSchema,
        snapshot: bone_protocol::ConfigSnapshot,
    ) {
        let settings_value = snapshot.values.clone();
        self.config_view = ConfigView {
            schema: Some(schema),
            snapshot: Some(snapshot),
        };
        if let Ok(settings) =
            serde_json::from_value::<crate::config::settings::BoneSettings>(settings_value)
        {
            self.renderer.theme.apply_snapshot(&settings.theme);
            apply_settings_to_user_config(&mut self.user_config, &settings);
            self.keymaps = settings.keymaps;
            self.approval_mode = self.user_config.approval_mode;
        }
    }

    fn finish_config_change(
        &mut self,
        schema: bone_protocol::ConfigSchema,
        snapshot: bone_protocol::ConfigSnapshot,
        request_id: Option<String>,
    ) -> Option<String> {
        self.apply_config_snapshot(schema, snapshot);
        take_pending_config(&mut self.pending_config, request_id)
    }

    fn reject_config_change(
        &mut self,
        current_revision: u64,
        request_id: Option<String>,
    ) -> Option<String> {
        if let Some(snapshot) = self.config_view.snapshot.as_mut() {
            snapshot.revision = current_revision;
        }
        // The daemon rejected the mutation (e.g. config.yaml unwritable), so
        // revert local approval mode to what the (pre-change) config snapshot
        // still holds.  Without this, `cycle_approval_mode` / `persist_mode`
        // updates `self.approval_mode` before the daemon acknowledges, and a
        // rejection leaves the TUI permanently showing the wrong mode.
        self.sync_approval_mode_from_config();
        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::GetConfig);
        take_pending_config(&mut self.pending_config, request_id)
    }

    /// Re-read `approval_mode` from the current config-view snapshot so the TUI
    /// reflects the daemon's authoritative persisted value after a rejection.
    fn sync_approval_mode_from_config(&mut self) {
        let Some(snapshot) = &self.config_view.snapshot else {
            return;
        };
        let Some(mode_str) = snapshot
            .values
            .pointer("/general/approval")
            .and_then(|v| v.as_str())
        else {
            return;
        };
        if let Ok(parsed) = crate::tools::ApprovalMode::parse(mode_str) {
            self.approval_mode = parsed;
            self.user_config.approval_mode = parsed;
        }
    }

    fn begin_config_change(&mut self, path: impl Into<String>) -> String {
        self.next_config_request = self.next_config_request.saturating_add(1);
        let request_id = format!("tui-{}-{}", std::process::id(), self.next_config_request);
        self.pending_config.insert(request_id.clone(), path.into());
        request_id
    }

    fn config_command(&mut self, arg: &str) -> String {
        let arg = arg.trim();
        if arg.is_empty() {
            return render_config_page(&self.config_view, None).unwrap_or_else(|error| error);
        }
        if let Some(path) = arg.strip_prefix("reset ").map(str::trim) {
            if self.config_view.field(path).is_none() {
                return format!("Unknown configuration setting `{path}`.");
            }
            let request_id = self.begin_config_change(path);
            if self
                .command_tx
                .send(crate::runtime::RuntimeCommand::ResetConfigValue {
                    path: path.into(),
                    expected_revision: self.config_view.revision(),
                    request_id: Some(request_id.clone()),
                })
                .is_err()
            {
                self.pending_config.remove(&request_id);
                return "Configuration daemon is unavailable.".into();
            }
            return format!("Resetting {path}…");
        }
        if let Some(rest) = arg.strip_prefix("set ") {
            let Some((path, raw)) = rest.trim().split_once(char::is_whitespace) else {
                return "Usage: /config set <path> <value>".into();
            };
            let raw = raw.trim();
            let Some(field) = self.config_view.field(path) else {
                return format!("Unknown configuration setting `{path}`.");
            };
            let value = match parse_config_value(field, raw) {
                Ok(value) => value,
                Err(error) => return format!("Invalid value for {path}: {error}."),
            };
            let request_id = self.begin_config_change(path);
            if self
                .command_tx
                .send(crate::runtime::RuntimeCommand::SetConfigValue {
                    path: path.into(),
                    value,
                    expected_revision: self.config_view.revision(),
                    request_id: Some(request_id.clone()),
                })
                .is_err()
            {
                self.pending_config.remove(&request_id);
                return "Configuration daemon is unavailable.".into();
            }
            return format!("Saving {path}…");
        }
        render_config_page(&self.config_view, Some(arg)).unwrap_or_else(|error| error)
    }

    /// Adopt the daemon-owned resolved settings and display metadata from a
    /// `FrontendState` event. Invalid settings leave the current frontend state
    /// untouched while command/tool metadata is still refreshed.
    fn apply_frontend_state(
        &mut self,
        banner: String,
        settings: serde_json::Value,
        commands: Vec<(String, String)>,
        tool_defs: Vec<crate::tools::ToolDefinition>,
        tool_display: serde_json::Value,
    ) {
        let mut theme_changed = false;
        // First receipt carries the boot banner — the daemon's `bone.banner()`
        // text plus client-side update/catalog hints — followed by the greeting.
        // (The VM-less client can't build the banner itself; it arrives here.)
        if !self.banner_shown {
            self.banner_shown = true;
            let mut lines: Vec<String> = Vec::new();
            if !banner.is_empty() {
                lines.push(banner.clone());
            }
            lines.extend(Self::banner_client_hints());
            if !lines.is_empty() {
                self.messages.push(Message::system(lines.join("\n")));
            }
            self.messages.push(Message::system(format!(
                "bone v{} — type /help for commands. Ctrl+C twice to quit.",
                env!("CARGO_PKG_VERSION")
            )));
        }
        if let Ok(resolved) =
            serde_json::from_value::<crate::ext::snapshots::ResolvedFrontendSettings>(settings)
        {
            let settings = resolved.settings;
            self.renderer.theme.apply_snapshot(&settings.theme);
            theme_changed = true;
            self.lua_input_style = input_style_snapshot(&settings.ui.input);
            apply_settings_to_user_config(&mut self.user_config, &settings);
            self.user_config.spinner_styles = resolved.spinner_styles;
            self.user_config.spinner_texts = resolved.spinner_texts;
            self.keymaps = settings.keymaps;
            self.approval_mode = self.user_config.approval_mode;
            self.renderer.input_style =
                configured_input_style(&self.lua_input_style, settings.ui.input.preset.as_deref());
        }
        if theme_changed {
            self.apply_terminal_background();
        }
        self.wire_commands = commands;
        // Tool render metadata: definitions arrive typed; display configs as an
        // opaque JSON map. A malformed display map is skipped (keeps the defs).
        self.wire_tools = WireTools {
            defs: tool_defs,
            display: serde_json::from_value(tool_display).unwrap_or_default(),
        };
    }

    fn apply_terminal_background(&mut self) {
        if self.renderer.theme.palette.bg.is_some() {
            if Renderer::apply_terminal_background(self.renderer.theme.palette.bg).is_ok() {
                self.terminal_bg_set = true;
            }
        } else {
            self.reset_terminal_background();
        }
    }

    fn reset_terminal_background(&mut self) {
        if self.terminal_bg_set {
            let _ = Renderer::reset_terminal_background();
            self.terminal_bg_set = false;
        }
    }

    /// Dispatch a hook on the daemon and wait briefly for its acknowledgement.
    /// For `session_end`, the daemon also closes the authoritative DB conversation.
    pub async fn dispatch_session_end(&mut self) {
        let command = crate::runtime::RuntimeCommand::DispatchHook {
            name: "session_end".into(),
            payload: serde_json::json!({}),
        };
        // A user hook must not leave the terminal in raw mode forever on exit.
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            self.send_and_await_snapshot(command),
        )
        .await;
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
            let _ = self
                .command_tx
                .send(crate::runtime::RuntimeCommand::ReplaceConversation {
                    messages: new_messages,
                });
        }

        if let Some(load) = action.conversation_load {
            self.load_conversation(load, term).await?;
        }

        if let Some(config_action) = action.config_action {
            action_reply = Some(self.apply_config_action(config_action).await);
        }
        Ok(action_reply)
    }

    async fn apply_config_action(&mut self, action: crate::ext::types::ConfigAction) -> String {
        match action {
            crate::ext::types::ConfigAction::Apply => {
                let _ = self
                    .command_tx
                    .send(crate::runtime::RuntimeCommand::ReloadSettings);
                let active_id = self.view.provider_id.clone();
                self.send_and_await_snapshot(crate::runtime::RuntimeCommand::SwitchProvider {
                    provider_id: active_id,
                })
                .await;
                "Configuration applied.".to_string()
            }
            crate::ext::types::ConfigAction::ApplyRestartRequired => {
                let _ = self
                    .command_tx
                    .send(crate::runtime::RuntimeCommand::ReloadSettings);
                let active_id = self.view.provider_id.clone();
                self.send_and_await_snapshot(crate::runtime::RuntimeCommand::SwitchProvider {
                    provider_id: active_id,
                })
                .await;
                "Configuration saved. Restart required for tool/command changes.".to_string()
            }
            crate::ext::types::ConfigAction::ReloadTools => self.reload_extensions(),
            crate::ext::types::ConfigAction::SwitchProvider { id } => {
                let prev = self.view.provider_id.clone();
                let request_id = self.begin_config_change(format!("providers.active ({id})"));
                self.send_and_await_snapshot(crate::runtime::RuntimeCommand::SetActiveProvider {
                    id,
                    expected_revision: self.config_view.revision(),
                    request_id: Some(request_id),
                })
                .await;
                if self.view.provider_id == prev {
                    format!(
                        "No change — still {} ({}). Provider may not be valid.",
                        self.view.provider_model, self.view.provider_id
                    )
                } else {
                    format!(
                        "Switched to {} ({})",
                        self.view.provider_model, self.view.provider_id
                    )
                }
            }
        }
    }

    /// Adopt a daemon `SessionSnapshot` as the local view-model: the single
    /// place the frontend mirrors authoritative state.
    pub(crate) fn apply_snapshot(&mut self, snapshot: crate::runtime::SessionSnapshot) {
        self.conversation_id = snapshot.conversation_id;
        self.token_stats = snapshot.to_token_stats();
        self.view = snapshot;
    }

    /// Block until the daemon publishes a `StateSnapshot`, then adopt it.
    ///
    /// Other events seen while waiting (notably an error `Status` the daemon
    /// emits *before* the snapshot — e.g. "failed to switch provider: …") are
    /// applied via [`apply_idle_event`], not discarded: a swallowed `Status`
    /// makes a failed switch look like a successful one and the post-failure
    /// snapshot (which keeps the *old* provider) the only thing the caller sees.
    /// A `Lagged` receiver is retried rather than treated as terminal.
    async fn await_state_snapshot(&mut self) {
        loop {
            // Bind first so the recv future temporary is dropped before
            // `apply_snapshot` / `apply_idle_event` borrows `self` again.
            let ev = self.events_rx.recv().await;
            match ev {
                Ok(crate::runtime::RuntimeEvent::StateSnapshot { snapshot }) => {
                    self.apply_snapshot(snapshot);
                    break;
                }
                Ok(other) => self.apply_idle_event(other),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    /// Drain buffered events, send a state-mutating command, then block for the
    /// daemon's authoritative `StateSnapshot` reply.
    ///
    /// Draining *before* the send is what fixes the "one switch behind" bug:
    /// every command (and turn) ends with a published snapshot, and one can
    /// still be sitting in the broadcast buffer (not yet drained by the idle
    /// loop) when the next `/provider` / `/model` / `/new` runs. Without the
    /// pre-drain, `await_state_snapshot` would adopt that *previous* snapshot
    /// and return before the daemon ever processed this command — so the view
    /// always trailed by one. The daemon publishes nothing until it receives
    /// `cmd`, so after draining + sending, the only snapshot that can arrive is
    /// this command's. There is no await point between the drain and the send,
    /// so the daemon cannot slip a publish in between.
    async fn send_and_await_snapshot(&mut self, cmd: crate::runtime::RuntimeCommand) {
        while let Ok(ev) = self.events_rx.try_recv() {
            self.apply_idle_event(ev);
        }
        let _ = self.command_tx.send(cmd);
        self.await_state_snapshot().await;
    }

    /// Load a past conversation as one atomic frontend operation. Waiting here
    /// yields to the in-process daemon and prevents the reply from racing with
    /// the next submitted user message.
    async fn load_conversation(
        &mut self,
        load: crate::ext::types::ConversationLoad,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let Some(id) = load.conversation_id else {
            return Ok(());
        };

        // A response from an earlier command must not satisfy this request.
        while let Ok(ev) = self.events_rx.try_recv() {
            self.apply_idle_event(ev);
        }
        self.command_tx
            .send(crate::runtime::RuntimeCommand::LoadConversation { id })
            .map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "runtime command channel closed")
            })?;

        loop {
            match self.events_rx.recv().await {
                Ok(crate::runtime::RuntimeEvent::ConversationLoaded { messages, snapshot })
                    if snapshot.conversation_id == Some(id) =>
                {
                    self.apply_idle_event(crate::runtime::RuntimeEvent::ConversationLoaded {
                        messages,
                        snapshot,
                    });
                    Renderer::hard_reset_viewport(term, self.renderer.viewport_height)?;
                    self.renderer.reset_scrollback_state();
                    self.renderer
                        .flush_new_to_scrollback(&self.messages, term)?;
                    self.redraw(term)?;
                    return Ok(());
                }
                Ok(crate::runtime::RuntimeEvent::ConversationLoadFailed {
                    id: failed_id,
                    message,
                }) if failed_id == id => {
                    self.messages.push(Message::system(message));
                    self.renderer
                        .flush_new_to_scrollback(&self.messages, term)?;
                    self.redraw(term)?;
                    return Ok(());
                }
                Ok(other) => self.apply_idle_event(other),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "runtime event channel closed while loading conversation",
                    ));
                }
            }
        }
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
                ChatRole::Tool => {
                    if let Some(diff) = edit_diff_message(
                        msg.name.as_deref().unwrap_or_default(),
                        msg.is_error,
                        &msg.content,
                    ) {
                        rows.push(diff);
                        continue;
                    }
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
    /// abort any in-flight stream, drop panes, and any pending tool-approval
    /// prompt. Shared by `/clear` and conversation load.
    ///
    /// When `clear_queue` is true (conversation load), drop stacked input —
    /// those prompts targeted the previous conversation. When false (`/clear`
    /// / `/new`), preserve the queue so later stacked messages still run
    /// (e.g. `msg1`, `/clear`, `msg2`). Explicit queue wipe remains Ctrl+D.
    fn reset_transient_ui_state(&mut self, clear_queue: bool) {
        self.cancel_streaming = true;
        self.pages.clear();
        self.active_page = 0;
        self.jobs_seen_version = u64::MAX;
        if clear_queue {
            self.queue.clear();
            self.queue_selected = 0;
            self.queue_editing = None;
        } else {
            // Pane UI was wiped; drop any in-progress queue edit state.
            self.queue_editing = None;
            if !self.queue.is_empty() {
                self.queue_selected = self.queue_selected.min(self.queue.len() - 1);
            } else {
                self.queue_selected = 0;
            }
        }
        self.active_prompt = None;
        self.pending_approval = None;
    }

    async fn start_new_conversation(&mut self) {
        self.send_and_await_snapshot(crate::runtime::RuntimeCommand::NewConversation)
            .await;
    }

    /// Clear chat history, end the current DB conversation, start a fresh one,
    /// and display a usage summary.
    async fn clear_chat(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        // Keep remaining stacked messages — `/clear` itself may be one item in
        // the queue, with more work intentionally queued after it.
        self.reset_transient_ui_state(false);

        let summary = if self.token_stats.request_count > 0 {
            format!("Session: {}. Chat cleared.", self.token_stats.one_liner())
        } else {
            "Chat cleared.".to_string()
        };

        // Tell the daemon to start a new conversation. It publishes a
        // StateSnapshot with the new conversation_id and reset stats.
        self.send_and_await_snapshot(crate::runtime::RuntimeCommand::NewConversation)
            .await;

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
        // Pages were cleared above; re-show the queue pane if items remain.
        self.refresh_queue_pane();
        self.redraw(term)?;
        Ok(())
    }

    fn persist_runtime_config(&mut self) {
        let mode = match self.user_config.approval_mode {
            crate::tools::ApprovalMode::Danger => "danger",
            crate::tools::ApprovalMode::Safe => "safe",
        };
        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::DispatchHook {
                name: "mode_change".into(),
                payload: serde_json::json!({ "mode": mode }),
            });
        // Push the mode to the daemon's authoritative `SharedApprovalMode` — the
        // atomic the gate actually reads. Without this, cycling Safe/Danger only
        // changes the UI while the daemon keeps gating at its startup mode.
        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::SetApprovalMode {
                mode: mode.to_string(),
            });
    }

    pub async fn run(&mut self) -> io::Result<()> {
        // Restore the terminal (disable raw mode + bracketed paste) before the
        // default hook prints the panic. Without this, a panic in the event loop
        // below unwinds with raw mode still on, leaving the user's shell without
        // echo or line editing until they blind-type `reset`. `shutdown_terminal`
        // only touches process-global stdout, so it is safe from a hook.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = Renderer::shutdown_terminal();
            prev_hook(info);
        }));

        let mut terminal = Renderer::init_terminal(MIN_ROWS)?;

        self.renderer
            .flush_new_to_scrollback(&self.messages, &mut terminal)?;
        self.refresh_jobs_pane();
        self.force_redraw(&mut terminal)?;

        while !self.should_quit {
            if event::poll(std::time::Duration::from_millis(50))? {
                if self.pending_approval.is_some() {
                    self.drain_approval_keys(&mut terminal)?;
                } else {
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
            }

            // P1: Detect stale terminal size (e.g. after tmux detach/reattach
            // where SIGWINCH may not fire). If the dimensions changed out from
            // under us, force a redraw.
            if let Ok(size) = crossterm::terminal::size()
                && self.renderer.last_size != Some(size)
            {
                self.force_redraw(&mut terminal)?;
            }

            // Drain daemon events between turns (StateSnapshot, Status,
            // FrontendState, etc.).
            let before = self.messages.len();
            let before_config_revision = self.config_view.revision();
            let mut replaced_scrollback = false;
            while let Ok(ev) = self.events_rx.try_recv() {
                replaced_scrollback |=
                    matches!(&ev, crate::runtime::RuntimeEvent::ConversationLoaded { .. });
                match ev {
                    crate::runtime::RuntimeEvent::Started { task, display, .. } => {
                        self.adopt_daemon_turn(task, display, &mut terminal).await?;
                    }
                    crate::runtime::RuntimeEvent::ApprovalRequest {
                        id,
                        call_id,
                        name,
                        arguments,
                        auto_allows,
                        preview,
                        ..
                    } => {
                        self.handle_approval_request(
                            id,
                            call_id,
                            name,
                            arguments,
                            auto_allows,
                            preview.as_deref(),
                            &mut terminal,
                        )?;
                    }
                    ev => self.apply_idle_event(ev),
                }
            }
            // Loading a conversation replaces native terminal scrollback; a
            // cursor reset alone would leave the command's old "Loading..."
            // line visible and make a successful load look stuck.
            if replaced_scrollback {
                Renderer::hard_reset_viewport(&mut terminal, self.renderer.viewport_height)?;
                self.renderer.reset_scrollback_state();
            }
            // Commit any scrollback an idle event added or replaced so it
            // renders promptly rather than waiting for the next keystroke.
            // Also redraw when the config_view changed (e.g. from a remote
            // client's mutation) even if no message was appended, so the
            // status bar and config page update without keyboard input.
            if idle_state_needs_redraw(
                replaced_scrollback,
                before,
                self.messages.len(),
                before_config_revision,
                self.config_view.revision(),
            ) {
                self.renderer
                    .flush_new_to_scrollback(&self.messages, &mut terminal)?;
                self.redraw(&mut terminal)?;
            }

            // Keep native process/sub-agent panes live; turn injection is daemon-owned.
            if self.maybe_refresh_jobs_pane() {
                self.redraw(&mut terminal)?;
            }
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

        self.dispatch_session_end().await;

        self.reset_terminal_background();
        Renderer::prepare_exit(&mut terminal)?;
        Renderer::shutdown_terminal()?;
        Ok(())
    }

    /// Ensure the viewport is the right size, then draw.
    fn ensure_viewport_and_draw(&mut self, terminal: &mut BoneTerminal) -> io::Result<()> {
        // Apply any Lua-driven UI updates (floats from bone.api.ui, ctx.ui.pane,
        // or ctx.ui.pane) before measuring, so a newly opened float is
        // counted in the viewport height.
        let size = terminal.size()?;
        // Publish the live terminal width to the daemon so its Lua panes
        // (`ctx.ui.width`) wrap text to the current width. Re-read each frame so
        // it tracks resizes.
        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::SetTerminalWidth { width: size.width });
        let desired = self
            .renderer
            .desired_height(
                &self.input,
                // Approval prompt is a pane now (counted via `visible_pages`), so the
                // input slot is sized normally — pass no prompt.
                None,
                size.width,
                self.visible_pages(),
                self.active_page,
                self.autocomplete.as_ref(),
                self.running_shells.len(),
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
        self.apply_terminal_background();
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

    /// Recreate the inline viewport after another terminal UI temporarily owned
    /// the screen. A normal redraw can retain ratatui's stale viewport anchor,
    /// causing the input rows restored by the takeover to overlap the next draw.
    fn restore_after_takeover(&mut self, terminal: &mut BoneTerminal) -> io::Result<()> {
        let height = self.renderer.viewport_height;
        Renderer::resize_viewport(terminal, height, height)?;
        self.force_redraw(terminal)
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
        // A live Lua command (e.g. /shotgun) runs through `run_remote_command`
        // without setting `streaming`, but it's still working — keep spinner/timer
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

    /// Submit queued turns without overwriting text typed while a turn runs.
    /// Draining pauses as soon as the input contains a draft; the idle inbox
    /// tick resumes it after that draft is submitted or cleared.
    async fn drain_queue_when_input_empty(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        while !self.queue.is_empty()
            && self.input.buffer.is_empty()
            && !self.input.has_pastes()
            && !self.input.has_images()
        {
            let queued = self.queue.pop_front().expect("queue checked non-empty");
            self.queue_selected = self.queue_selected.min(self.queue.len().saturating_sub(1));
            self.refresh_queue_pane();
            self.input.buffer = queued;
            self.input.cursor_pos = self.input.buffer.chars().count();
            self.send_message(term).await?;
        }
        self.refresh_queue_pane();
        Ok(())
    }

    /// Refresh the jobs pane when the registry version changed or, while
    /// jobs are running, at least once per second so elapsed time and token
    /// counters stay live. Returns `true` when the pane was refreshed.
    pub(crate) fn maybe_refresh_jobs_pane(&mut self) -> bool {
        let registry = crate::ext::jobs::registry();
        let version = registry.version();
        let processes_running = crate::processes::registry()
            .list(None)
            .iter()
            .any(|p| p.running);
        let processes_pane_visible = self
            .pages
            .iter()
            .any(|page| page.source == crate::ui::processes_pane::PANE_SOURCE);
        // Keep refreshing for one final tick after the last process exits so
        // refresh_jobs_pane can remove the now-empty processes pane.
        let agent_jobs_tick_due = self.jobs_last_refresh.elapsed()
            >= std::time::Duration::from_secs(1)
            && !registry.running_ids().is_empty();
        let periodic = background_pane_needs_refresh(
            processes_running,
            processes_pane_visible,
            agent_jobs_tick_due,
        );
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
                let changed = self.renderer.theme.set_highlight(&name, fg.as_deref());
                if changed && name == "bg" {
                    self.apply_terminal_background();
                }
                changed
            }
        }
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
            let active: Vec<_> = jobs.iter().filter(|j| !j.is_finished()).collect();
            if !active
                .iter()
                .any(|j| Some(j.id.as_str()) == self.selected_job_id.as_deref())
            {
                self.selected_job_id = active.first().map(|j| j.id.clone());
            }
            if let Some(page) =
                crate::ui::jobs_pane::render_selected(&jobs, self.selected_job_id.as_deref())
            {
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
        let processes = crate::processes::registry().list(None);
        if let Some(page) = crate::ui::processes_pane::render(&processes) {
            let (_, new_active) = PanePage::upsert(&mut self.pages, self.active_page, page);
            self.active_page = new_active;
            self.panes_visible = true;
        } else {
            self.active_page = PanePage::remove(
                &mut self.pages,
                crate::ui::processes_pane::PANE_SOURCE,
                self.active_page,
            );
        }
    }
}

fn edit_diff_message(name: &str, is_error: bool, content: &str) -> Option<Message> {
    if name != "edit_file" || is_error || !content.starts_with("Edited: ") {
        return None;
    }
    let newline = content.find('\n')?;
    Some(Message::system(content[newline..].to_string()))
}

/// Render a point-in-time view of a running job from its bounded runtime-event log.
fn job_snapshot_messages(job: &crate::ext::jobs::Job, wire_tools: &WireTools) -> Vec<Message> {
    let mut rows = vec![Message::user(job.task.clone())];
    let mut answer = String::new();
    let mut calls = std::collections::HashMap::new();
    let mut shown_edit_previews = std::collections::HashSet::new();
    for job_event in &job.events {
        match &job_event.event {
            crate::runtime::RuntimeEvent::TextDelta { text } => answer.push_str(text),
            crate::runtime::RuntimeEvent::ReasoningDelta { text } => {
                if !text.is_empty() {
                    rows.push(Message::system(format!("thinking: {text}")));
                }
            }
            crate::runtime::RuntimeEvent::ToolCall {
                id,
                name,
                arguments,
                ..
            } => {
                if !answer.trim().is_empty() {
                    rows.push(Message::assistant(std::mem::take(&mut answer)));
                }
                calls.insert(
                    id.clone(),
                    ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    },
                );
                if let Some(diff) = &job_event.edit_preview {
                    rows.push(Message::system(diff.clone()));
                    shown_edit_previews.insert(id.clone());
                }
            }
            crate::runtime::RuntimeEvent::ToolOutput { .. } => {}
            crate::runtime::RuntimeEvent::ToolResult {
                name,
                call_id,
                content,
                is_error,
            } => {
                if shown_edit_previews.contains(call_id) && !is_error {
                    continue;
                }
                if let Some(diff) = edit_diff_message(name, *is_error, content) {
                    rows.push(diff);
                    continue;
                }
                let result = ToolResult {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    ..Default::default()
                };
                let row = match calls.get(call_id) {
                    Some(call) => build_tool_row(call, &result, wire_tools.display_for_call(call)),
                    None => Message::tool_row(name.clone(), *is_error),
                };
                rows.push(row);
            }
            crate::runtime::RuntimeEvent::Failed { message } => {
                rows.push(Message::system(format!("failed: {message}")))
            }
            _ => {}
        }
    }
    if !answer.trim().is_empty() {
        rows.push(Message::assistant(answer));
    }
    if rows.len() == 1 {
        let status = job.activity.as_deref().unwrap_or("starting");
        rows.push(Message::system(format!("{} — {status}", job.id)));
    }
    rows
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
                running: &self.running_shells,
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

    /// Collect all available slash commands with descriptions: native builtins
    /// plus the daemon-advertised Lua commands (`wire_commands` from
    /// `FrontendState`).
    fn collect_commands(&self) -> Vec<(String, String)> {
        commands::merge_commands(&self.wire_commands)
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

    pub(super) fn open_transcript_view(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let result = crate::ui::transcript_view::run(&self.messages, &self.renderer.theme);
        self.force_redraw(term)?;
        result
    }

    fn queue_pane_active(&self) -> bool {
        self.panes_visible
            && self
                .pages
                .get(self.active_page)
                .is_some_and(|p| p.source == crate::ui::queue_pane::PANE_SOURCE)
    }

    fn refresh_queue_pane(&mut self) {
        if self.queue.is_empty() {
            self.queue_selected = 0;
            self.active_page = PanePage::remove(
                &mut self.pages,
                crate::ui::queue_pane::PANE_SOURCE,
                self.active_page,
            );
            return;
        }
        self.queue_selected = self.queue_selected.min(self.queue.len() - 1);
        if let Some(page) = crate::ui::queue_pane::render(&self.queue, self.queue_selected) {
            let (_, active) = PanePage::upsert(&mut self.pages, self.active_page, page);
            self.active_page = active;
            self.panes_visible = true;
        }
    }

    fn finish_queue_edit(&mut self, save: bool) {
        if finish_queue_edit(
            &mut self.queue,
            &mut self.queue_selected,
            &mut self.queue_editing,
            &mut self.input,
            save,
        ) {
            self.refresh_queue_pane();
        }
    }

    fn agents_pane_active(&self) -> bool {
        self.panes_visible
            && self
                .pages
                .get(self.active_page)
                .is_some_and(|p| p.source == crate::ui::jobs_pane::PANE_SOURCE)
    }

    fn open_selected_job(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let Some(id) = self.selected_job_id.as_deref() else {
            return Ok(());
        };
        let Some(job) = crate::ext::jobs::registry()
            .all_jobs()
            .into_iter()
            .find(|j| j.id == id)
        else {
            return Ok(());
        };
        let messages = if let Some(transcript) = &job.transcript {
            self.rebuild_scrollback_from_transcript(transcript)
        } else {
            job_snapshot_messages(&job, &self.wire_tools)
        };
        let result = crate::ui::transcript_view::run_collapsed(&messages, &self.renderer.theme);
        self.force_redraw(term)?;
        result
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

        if code == KeyCode::Char('o') && modifiers.contains(KeyModifiers::CONTROL) {
            return self.open_transcript_view(term);
        }

        if self.queue_editing.is_some() && modifiers.is_empty() {
            match code {
                KeyCode::Enter => {
                    self.finish_queue_edit(true);
                    return self.redraw(term);
                }
                KeyCode::Esc => {
                    self.finish_queue_edit(false);
                    return self.redraw(term);
                }
                _ => {}
            }
        }

        if self.queue_pane_active()
            && self.queue_editing.is_none()
            && apply_queue_nav_key(
                code,
                modifiers,
                &mut self.queue,
                &mut self.queue_selected,
                &mut self.queue_editing,
                &mut self.input,
            )
        {
            self.refresh_queue_pane();
            return self.redraw(term);
        }

        if self.agents_pane_active() {
            match apply_agents_nav_key(code, modifiers, &mut self.selected_job_id, &self.input) {
                AgentsKeyResult::Unhandled => {}
                AgentsKeyResult::SelectionChanged => {
                    self.refresh_jobs_pane();
                    return self.redraw(term);
                }
                AgentsKeyResult::Cancelled(id) => {
                    let _ = self
                        .command_tx
                        .send(crate::runtime::RuntimeCommand::CancelJob { id });
                    self.refresh_jobs_pane();
                    return self.redraw(term);
                }
                AgentsKeyResult::OpenJob => {
                    return self.open_selected_job(term);
                }
            }
        }

        // BackTab is reserved for approval-mode cycle (see CycleMode below).
        if self.panes_visible
            && apply_pane_nav_key(code, modifiers, &mut self.pages, &mut self.active_page)
        {
            return self.redraw(term);
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
                            self.drain_queue_when_input_empty(term).await?;
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
                self.drain_queue_when_input_empty(term).await?;
                Ok(())
            }
            InputAction::ClearQueue => {
                self.queue.clear();
                self.queue_editing = None;
                self.refresh_queue_pane();
                self.redraw(term)
            }
            InputAction::CycleMode => self.cycle_approval_mode(term),
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
            // The pump (run_event_pump / remote command loop) sends Cancel,
            // which the daemon now also routes to its background sub-agents.
            self.cancel_streaming = true;
        } else {
            // Idle: no turn to cancel, but background sub-agent jobs may be
            // running. Ask the daemon to terminate them (a no-op if none).
            let _ = self.command_tx.send(crate::runtime::RuntimeCommand::Cancel);
        }
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

    /// Handle a daemon `ApprovalRequest` from any event pump (idle, turn, or
    /// command). Shows the edit preview if present, auto-approves when the
    /// gate says so or the UI is in Danger mode (reasserting Danger on desync),
    /// otherwise raises the interactive prompt.
    pub(crate) fn handle_approval_request(
        &mut self,
        id: u64,
        call_id: String,
        name: String,
        arguments: serde_json::Value,
        auto_allows: bool,
        preview: Option<&str>,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let call = ToolCall {
            id: call_id,
            name,
            arguments,
        };
        if let Some(preview) = preview {
            self.pump_show_edit_preview(&call.id, preview, term)?;
        }
        // Danger UI means every tool is allowed. Even if the daemon still sent
        // a prompt (mode desync), auto-accept and reassert Danger so the gate
        // catches up for subsequent calls.
        if auto_allows || matches!(self.approval_mode, ApprovalMode::Danger) {
            if !auto_allows {
                self.user_config.approval_mode = ApprovalMode::Danger;
                self.persist_runtime_config();
            }
            let _ = self
                .command_tx
                .send(crate::runtime::RuntimeCommand::ApprovalReply {
                    id,
                    outcome: CallOutcome::Approve,
                });
        } else {
            self.begin_approval(&call, id, term)?;
        }
        Ok(())
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
            // BackTab always cycles approval mode — even mid-prompt / mid-advise —
            // and auto-accepts when the result is Danger (see cycle_approval_mode).
            if let Event::Key(key) = &event
                && key.kind == KeyEventKind::Press
                && key.code == KeyCode::BackTab
            {
                self.cycle_approval_mode(term)?;
                continue;
            }
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

    /// Cycle Safe ↔ Danger, push the new mode to the daemon, and — when a tool
    /// approval is still waiting — auto-accept if the result is Danger. Danger
    /// means every tool is allowed, so the prompt is obsolete. If the UI was
    /// already Danger while still pending (UI/daemon desync), reassert Danger
    /// and accept instead of flipping to Safe.
    fn cycle_approval_mode(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let pending = self.pending_approval.is_some();
        if pending && matches!(self.approval_mode, ApprovalMode::Danger) {
            self.user_config.approval_mode = ApprovalMode::Danger;
            self.persist_runtime_config();
            return self.resolve_approval(Decision::Accept, term);
        }
        self.approval_mode = self.approval_mode.cycle();
        self.user_config.approval_mode = self.approval_mode;
        self.persist_runtime_config();
        if pending && matches!(self.approval_mode, ApprovalMode::Danger) {
            return self.resolve_approval(Decision::Accept, term);
        }
        self.redraw(term)
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
        let has_lua_config = lua_config_available(&self.wire_commands);

        if has_lua_config
            && cmd == "provider"
            && arg.is_empty()
            && self
                .run_remote_command("config", "providers", term)
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
                .run_remote_command("config", config_arg, term)
                .await
                .is_some()
            {
                return Ok(());
            }
        }
        // `/config` is protected from arbitrary Lua overrides, but the bundled
        // Lua command still owns its interactive UI. Keep the native schema
        // renderer only as a fallback when that command is unavailable.
        if cmd == "config" {
            if has_lua_config
                && self
                    .run_remote_command("config", &arg, term)
                    .await
                    .is_some()
            {
                return Ok(());
            }
            let reply = self.config_command(&arg);
            return self.show_reply(reply, term);
        }

        // Protected built-ins always win over Lua commands.
        if !commands::is_protected_builtin(cmd.as_str()) && !self.wire_commands.is_empty() {
            // Check if the lua command is enabled in commands config.
            // If the commands page is absent or empty, treat all registered commands as enabled
            // (same fallback semantics as tools).
            let enabled = !self
                .config_view
                .disabled_commands()
                .iter()
                .any(|name| name == &cmd);
            if enabled && let Some(_reply) = self.run_remote_command(&cmd, &arg, term).await {
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
        if cmd == "update" {
            return self.open_update(term);
        }

        let prev_provider = self.view.provider_id.clone();
        let prev_model = self.view.provider_model.clone();

        // Handle /provider and /model by telling the daemon to switch, then
        // reading the authoritative provider info from the StateSnapshot.
        let reply = match cmd.as_str() {
            "model" => {
                if arg.is_empty() {
                    format!("{} ({})", self.view.provider_model, self.view.provider_id)
                } else {
                    let provider_id = self.view.provider_id.clone();
                    let provider = self
                        .config_view
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| {
                            snapshot
                                .providers
                                .iter()
                                .find(|provider| provider.id == provider_id)
                        })
                        .cloned();
                    if let Some(provider) = provider {
                        let update = bone_protocol::ProviderUpdate {
                            id: provider.id.clone(),
                            label: provider.label.clone(),
                            base_url: provider.base_url.clone(),
                            model: arg.to_string(),
                            endpoint: provider.endpoint.clone(),
                            handler: provider.handler.clone(),
                            context_window_tokens: provider.context_window_tokens,
                            reasoning_effort: provider.reasoning_effort.clone(),
                            api_key: None,
                        };
                        let request_id =
                            self.begin_config_change(format!("providers.{}.model", provider.id));
                        self.send_and_await_snapshot(
                            crate::runtime::RuntimeCommand::UpsertProvider {
                                provider: update,
                                expected_revision: self.config_view.revision(),
                                request_id: Some(request_id),
                            },
                        )
                        .await;
                    }
                    // The snapshot is authoritative: if the model didn't actually
                    // change, the switch failed (the daemon also emits the precise
                    // reason as a Status, surfaced by `await_state_snapshot`).
                    if self.view.provider_model == prev_model {
                        format!(
                            "No change — model is still {} ({}). '{arg}' may not be valid for this provider.",
                            self.view.provider_model, self.view.provider_id
                        )
                    } else {
                        format!(
                            "Switched to {} ({})",
                            self.view.provider_model, self.view.provider_id
                        )
                    }
                }
            }
            "provider" => {
                if arg.is_empty() {
                    let mut lines = vec![format!(
                        "Current: {} ({})",
                        self.view.provider_model, self.view.provider_id
                    )];
                    let providers = self
                        .config_view
                        .snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.providers.as_slice())
                        .unwrap_or_default();
                    if providers.is_empty() {
                        lines.push("No providers configured.".to_string());
                    } else {
                        lines.push("Available:".to_string());
                        for entry in providers {
                            let id = &entry.id;
                            let marker = if id == &self.view.provider_id {
                                " *"
                            } else {
                                ""
                            };
                            lines.push(format!(
                                "  {} — {} ({}){}",
                                id, entry.label, entry.model, marker
                            ));
                        }
                    }
                    lines.join("\n")
                } else {
                    let request_id = self.begin_config_change(format!("providers.active ({arg})"));
                    self.send_and_await_snapshot(
                        crate::runtime::RuntimeCommand::SetActiveProvider {
                            id: arg.to_string(),
                            expected_revision: self.config_view.revision(),
                            request_id: Some(request_id),
                        },
                    )
                    .await;
                    // If the provider id didn't change, the switch failed (e.g.
                    // unknown id); the daemon's Status carries the exact reason.
                    if self.view.provider_id == prev_provider {
                        format!(
                            "No change — still {} ({}). '{arg}' is not an available provider; run /provider to list them.",
                            self.view.provider_model, self.view.provider_id
                        )
                    } else {
                        format!(
                            "Switched to {} ({})",
                            self.view.provider_model, self.view.provider_id
                        )
                    }
                }
            }
            "help" => commands::help(&self.wire_commands),
            "quit" | "exit" => {
                if let Some(notice) = self.request_quit() {
                    self.messages.push(Message::system(notice));
                    self.renderer
                        .flush_new_to_scrollback(&self.messages, term)?;
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
        self.restore_after_takeover(term)?;
        Ok(result.ok())
    }

    fn open_stats_dashboard(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        // Reads the local session DB — the same file the local daemon writes.
        // (A truly remote `--connect` host would reflect the local DB instead;
        // acceptable, and no worse than the previous "unavailable" block.)
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

        self.restore_after_takeover(term)?;
        if let Err(err) = result {
            return self.show_reply(format!("Stats dashboard failed: {err}"), term);
        }
        Ok(())
    }

    /// Ask the daemon to rebuild its Lua VM and tool registry from disk (the
    /// `/tools reload` and post-`/catalog` hot-reload path). The daemon disk-boots
    /// and broadcasts a fresh `FrontendState` (new theme/keymap/commands/tools)
    /// plus a `Status` summary, which the event loop applies. Frontend state is
    /// driven entirely by the daemon's `FrontendState`.
    fn reload_extensions(&mut self) -> String {
        let _ = self
            .command_tx
            .send(crate::runtime::RuntimeCommand::ReloadExtensions);
        "Reloading tools and Lua extensions…".to_string()
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
        self.restore_after_takeover(term)?;
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

    fn open_update(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        if let Some(status) = self.try_tmux_popup("update", "96%", "60%", term)? {
            return match status.code() {
                Some(0) => self.show_reply(
                    "Update applied. Restart bone to use the new binary.".to_string(),
                    term,
                ),
                Some(2) => self.show_reply("Update: no changes.".to_string(), term),
                _ => self.show_reply("Update failed.".to_string(), term),
            };
        }

        self.show_reply(
            "Run `bone update` from your shell to update bone.".to_string(),
            term,
        )
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
            self.restore_after_takeover(term)?;
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

fn input_style_snapshot(
    input: &crate::config::settings::UiInputSettings,
) -> crate::ext::snapshots::InputStyleSnapshot {
    crate::ext::snapshots::InputStyleSnapshot {
        preset: input.preset.clone(),
        prefix: input.prefix.clone(),
        show_prefix: Some(input.show_prefix),
        horizontal_padding: input.horizontal_padding,
        vertical_padding: input.vertical_padding,
        fill: input.fill,
        border: crate::ext::snapshots::InputBorderSnapshot {
            horizontal: input.border.horizontal.clone(),
            vertical: input.border.vertical.clone(),
            top_left: input.border.top_left.clone(),
            top_right: input.border.top_right.clone(),
            bottom_left: input.border.bottom_left.clone(),
            bottom_right: input.border.bottom_right.clone(),
        },
    }
}

fn apply_settings_to_user_config(
    cfg: &mut crate::config::UserConfig,
    settings: &crate::config::settings::BoneSettings,
) {
    cfg.approval_mode =
        crate::tools::ApprovalMode::parse_lenient(settings.general.approval.as_str());
    cfg.show_thinking = settings.general.show_reasoning;
    cfg.input_preset = settings.ui.input.preset.clone();
    for (key, value) in [
        ("status_show_model", settings.ui.status_show_model),
        ("status_show_approval", settings.ui.status_show_approval),
        (
            "status_show_tokens_curr",
            settings.ui.status_show_tokens_curr,
        ),
        ("status_show_tokens_in", settings.ui.status_show_tokens_in),
        ("status_show_tokens_out", settings.ui.status_show_tokens_out),
        (
            "status_show_tokens_total",
            settings.ui.status_show_tokens_total,
        ),
        ("status_show_queue", settings.ui.status_show_queue),
        ("status_show_spinner", settings.ui.status_show_spinner),
        ("status_show_timer", settings.ui.status_show_timer),
    ] {
        cfg.status_show.insert(key.to_string(), value);
    }
    cfg.spinner_style = settings.ui.spinner_style.clone();
    cfg.spinner_text = settings.ui.spinner_text.clone();
    cfg.spinner_speed = settings.ui.spinner_speed;
    cfg.spinner_text_rotate = settings.ui.spinner_text_rotate;
    cfg.spinner_text_speed = settings.ui.spinner_text_speed;
    cfg.spinner_text_custom = settings.ui.spinner_custom.clone();
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
