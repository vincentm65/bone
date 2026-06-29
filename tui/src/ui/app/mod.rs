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
    /// A live Lua command is running through `run_remote_command`. This needs
    /// the same cancellation plumbing as streaming, but it is not a model turn
    /// and should not show the thinking spinner.
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
    /// Keymap snapshot for custom bindings, supplied by the daemon's
    /// `FrontendState`.
    lua_keymap: crate::ext::snapshots::LuaKeymapSnapshot,
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
    /// Last-seen job-registry version (forces first-tick render).
    jobs_seen_version: u64,
    /// Last wall-clock jobs-pane refresh (drives the ~1s live ticker).
    jobs_last_refresh: std::time::Instant,
    /// Set after the user was warned that quitting kills running sub-agent
    /// jobs; the next quit request goes through.
    quit_despite_jobs: bool,
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

        // Sync our configured approval mode to the runtime up front: it boots
        // its gate at `Safe` regardless of config, and the client otherwise
        // only pushes the mode when the user *cycles* it.
        let _ = command_tx.send(crate::runtime::RuntimeCommand::SetApprovalMode {
            mode: match approval_mode {
                crate::tools::ApprovalMode::Danger => "danger",
                crate::tools::ApprovalMode::Safe => "safe",
            }
            .to_string(),
        });

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
            queue: VecDeque::new(),

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
            lua_keymap: crate::ext::snapshots::LuaKeymapSnapshot::default(),
            wire_commands: Vec::new(),
            wire_tools: WireTools::default(),
            banner_shown: false,
            lua_status: Vec::new(),
            shown_tool_rows: std::collections::HashSet::new(),
            jobs_seen_version: u64::MAX,
            jobs_last_refresh: std::time::Instant::now(),
            quit_despite_jobs: false,
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
        if crate::update_check::update_available()
            && let Some(latest) = crate::update_check::latest_seen()
        {
            lines.push(format!(
                "bone {latest} available — https://github.com/vincentm65/bone/releases"
            ));
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
                banner, theme, keymap, config, commands, tool_defs, tool_display,
            } => {
                self.apply_frontend_state(
                    banner, theme, keymap, config, commands, tool_defs, tool_display,
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
        banner: String,
        theme: serde_json::Value,
        keymap: serde_json::Value,
        config: serde_json::Value,
        commands: Vec<(String, String)>,
        tool_defs: Vec<crate::tools::ToolDefinition>,
        tool_display: serde_json::Value,
    ) {
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

    /// Dispatch a `session_end` hook on the daemon's Lua VM.
    pub fn dispatch_session_end(&self) {
        let _ = self.command_tx.send(crate::runtime::RuntimeCommand::DispatchHook {
            name: "session_end".into(),
            payload: serde_json::json!({}),
        });
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
    /// place the frontend mirrors authoritative state.
    pub(crate) fn apply_snapshot(&mut self, snapshot: crate::runtime::SessionSnapshot) {
        self.conversation_id = snapshot.conversation_id;
        self.token_stats = snapshot.to_token_stats();
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
        let _ = self.command_tx.send(crate::runtime::RuntimeCommand::DispatchHook {
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

            // Drain daemon events between turns (StateSnapshot, Status,
            // FrontendState, etc.).
            let before = self.messages.len();
            while let Ok(ev) = self.events_rx.try_recv() {
                self.apply_idle_event(ev);
            }
            // Commit any scrollback an idle event added (banner, Status notices)
            // so it renders promptly rather than waiting for the next keystroke.
            if self.messages.len() != before {
                self.renderer
                    .flush_new_to_scrollback(&self.messages, &mut terminal)?;
                self.redraw(&mut terminal)?;
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
        let size = terminal.size()?;
        // Publish the live terminal width to the daemon so its Lua panes
        // (`ctx.ui.width`) wrap text to the current width. Re-read each frame so
        // it tracks resizes.
        let _ = self.command_tx.send(crate::runtime::RuntimeCommand::SetTerminalWidth {
            width: size.width,
        });
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

    /// Collect all available slash commands with descriptions: native builtins
    /// plus the daemon-advertised Lua commands (`wire_commands` from
    /// `FrontendState`).
    fn collect_commands(&self) -> Vec<(String, String)> {
        let mut cmds = crate::ui::autocomplete::builtin_commands();
        cmds.extend(self.wire_commands.iter().cloned());
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
        let has_lua_config = self.wire_commands.iter().any(|(name, _)| name == "config");

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
        if !commands::is_protected_builtin(cmd.as_str()) && !self.wire_commands.is_empty() {
            // Check if the lua command is enabled in commands config.
            // If the commands page is absent or empty, treat all registered commands as enabled
            // (same fallback semantics as tools).
            let all_command_names: Vec<String> = self
                .wire_commands
                .iter()
                .map(|(name, _)| name.clone())
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

        let lua_cmds: Vec<(String, String)> = self.wire_commands.clone();

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

    /// Run a Lua slash command on the daemon's VM over the protocol. Returns
    /// `Some(())` if handled, `None` if the daemon reported the command unknown.
    async fn run_lua_command(
        &mut self,
        cmd: &str,
        arg: &str,
        term: &mut BoneTerminal,
    ) -> Option<()> {
        self.run_remote_command(cmd, arg, term).await
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

        self.force_redraw(term)?;
        if let Err(err) = result {
            return self.show_reply(format!("Stats dashboard failed: {err}"), term);
        }
        Ok(())
    }

    /// Ask the daemon to rebuild its Lua VM and tool registry from disk (the
    /// `/tools reload` and post-`/catalog` hot-reload path). The daemon disk-boots
    /// and broadcasts a fresh `FrontendState` (new theme/keymap/commands/tools)
    /// plus a `Status` summary, which the event loop applies. Also refresh the
    /// local config-derived state from disk so settings tracked client-side
    /// (e.g. approval mode) stay in sync.
    fn reload_extensions(&mut self) -> String {
        let custom = config::custom::CustomConfigs::load();
        self.user_config.apply_custom_configs(&custom);
        self.approval_mode = self.user_config.approval_mode;
        self.custom_configs = custom;
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
