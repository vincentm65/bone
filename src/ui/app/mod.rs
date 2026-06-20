mod editor;
mod keymap;
mod paste;
pub mod stream;

use paste::{
    apply_input_key_with_paste_burst, collect_non_bracketed_paste_burst, is_paste_burst, plain_char,
};

use crate::chat::Message;
use crate::config::{self, UserConfig};
use crate::llm::{ChatMessage, LlmProvider, TokenStats, providers};
use crate::session_db::SessionDb;

use crate::ext::ExtensionManager;
use crate::tools::registry::ToolHandler;
use crate::tools::{ApprovalMode, CallOutcome, ToolCall};
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
/// `reply` resolves the `ChannelApprovalGate` that the running tool awaits.
pub struct PendingApproval {
    reply: tokio::sync::oneshot::Sender<CallOutcome>,
    /// `true` once the user picked "Advise" and is typing free-form advice.
    advising: bool,
}

pub struct App {
    pub messages: Vec<Message>,
    pub transcript: Vec<ChatMessage>,
    pub input: InputState,
    pub streaming: bool,
    /// A live Lua command is running through `drive_live`. This needs the same
    /// cancellation plumbing as streaming, but it is not a model turn and
    /// should not show the thinking spinner.
    pub live_command: bool,
    pub provider: String,
    pub model: String,
    pub llm: std::sync::Arc<dyn LlmProvider>,
    pub should_quit: bool,
    pub renderer: Renderer,
    pub user_config: UserConfig,
    pub custom_configs: config::custom::CustomConfigs,
    pub queue: VecDeque<String>,
    pub tools: ToolHandler,

    pub approval_mode: ApprovalMode,
    pub active_prompt: Option<Prompt>,
    /// Tool-call approval awaiting a decision, resolved inside the main stream
    /// loop. `Some` only while `active_prompt` shows an approval prompt.
    pub pending_approval: Option<PendingApproval>,
    /// Set to `true` to abort the current streaming response.
    pub cancel_streaming: bool,
    /// Timestamp of the last Ctrl+C press (for double-tap quit).
    pub last_ctrl_c: Option<Instant>,
    /// Cumulative token usage stats.
    pub token_stats: TokenStats,
    /// Live, running estimate of `received` (completion) tokens during a turn.
    /// `Some` while a turn streams — ticked up on each text/tool delta and
    /// rebaselined to the authoritative count on `TokenUsage`; `None` when idle
    /// so the status bar shows `token_stats.received` directly.
    pub stream_estimated_received: Option<u64>,

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

    /// SQLite session database for conversation persistence and usage tracking.
    session_db: Option<SessionDb>,
    /// Current conversation ID in the session database.
    conversation_id: Option<i64>,
    /// Message sequence counter for DB ordering.
    session_seq: i64,
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
    /// Lua status lines keyed by id (`bone.api.ui.set_statusline`), in
    /// registration order. Each id's segments are appended to the native
    /// status bar; re-setting the same id updates it in place.
    lua_status: Vec<(String, Vec<crate::runtime::view::StatusSegment>)>,
    /// Call IDs that already have a tool row in chat (to avoid duplicates).
    shown_tool_rows: std::collections::HashSet<String>,
    /// Last-seen subagent job-registry version (forces first-tick render).
    subagent_seen_version: u64,
    /// Last wall-clock subagent pane refresh (drives the ~1s live ticker).
    subagent_last_refresh: std::time::Instant,
    /// Set after the user was warned that quitting kills running sub-agent
    /// jobs; the next quit request goes through.
    quit_despite_jobs: bool,
}

impl App {
    pub fn new(
        llm: Box<dyn LlmProvider>,
        mut user_config: UserConfig,
        mut custom_configs: config::custom::CustomConfigs,
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

        Ok(Self {
            messages,
            transcript: Vec::new(),
            input: InputState::default(),
            streaming: false,
            live_command: false,
            provider,
            model,
            llm,
            should_quit: false,
            renderer,
            user_config,
            custom_configs,
            queue: VecDeque::new(),
            tools,

            approval_mode,
            active_prompt: None,
            pending_approval: None,
            cancel_streaming: false,
            last_ctrl_c: None,
            token_stats: TokenStats::new(),
            stream_estimated_received: None,
            pages: Vec::new(),
            active_page: 0,
            panes_visible: true,
            thinking_tail: String::new(),
            thinking_first_shown: None,
            thinking_clear_at: None,

            session_db: None,
            conversation_id: None,
            session_seq: 0,
            turn_start: None,
            turn_paused_duration: std::time::Duration::ZERO,
            turn_pause_start: None,
            autocomplete: None,
            extensions,
            lua_keymap,
            lua_status: Vec::new(),
            shown_tool_rows: std::collections::HashSet::new(),
            subagent_seen_version: u64::MAX,
            subagent_last_refresh: std::time::Instant::now(),
            quit_despite_jobs: false,
        })
    }
    /// Collect banner text from `bone.banner()` Lua function.
    /// Returns lines joined with newlines, or empty if undefined/nothing.
    fn collect_banner(extensions: &crate::ext::ExtensionManager) -> String {
        let lua = extensions.lua_handle();
        let lua = match lua.lock() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("bone: warning: Lua mutex poisoned: {e}");
                return String::new();
            }
        };
        let bone = match lua.globals().get::<mlua::Table>("bone") {
            Ok(b) => b,
            Err(_) => return String::new(),
        };
        let banner_fn: mlua::Function = match bone.get("banner") {
            Ok(f) => f,
            Err(_) => return String::new(),
        };
        match banner_fn.call::<mlua::Table>(()) {
            Ok(tbl) => {
                let mut lines = Vec::new();
                for item in tbl.sequence_values::<mlua::String>() {
                    if let Ok(item_str) = item {
                        if let Ok(s) = item_str.to_str() {
                            lines.push(s.to_string());
                        }
                    }
                }
                lines.join("\n")
            }
            Err(e) => {
                eprintln!("bone: warning: banner() call failed: {e}");
                String::new()
            }
        }
    }

    /// Dispatch a `session_end` event to Lua handlers.
    pub fn dispatch_session_end(&self) {
        self.extensions
            .dispatch_simple("session_end", serde_json::json!({}));
    }

    /// Initialize or open the session database.
    fn init_session_db(&mut self) -> Option<String> {
        if self.session_db.is_some() {
            return None;
        }
        let db_path = crate::session_db::db_path();
        match SessionDb::open(&db_path) {
            Ok(db) => match db.create_conversation(self.llm.id(), self.llm.model()) {
                Ok(conv_id) => {
                    self.conversation_id = Some(conv_id);
                    self.session_db = Some(db);
                    None
                }
                Err(err) => Some(format!("warning: failed to create conversation: {err}")),
            },
            Err(err) => Some(format!("warning: failed to open session database: {err}")),
        }
    }
    /// Append a message to the session database under the active conversation,
    /// allocating the next sequence number. No-op when no db/conversation is
    /// open. Shared by the assistant and tool-result append helpers.
    fn append_db_message(
        &mut self,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        call_id: Option<&str>,
        tool_calls_json: Option<&str>,
    ) {
        let Some(conv_id) = self.conversation_id else {
            return;
        };
        let Some(db) = self.session_db.as_ref() else {
            return;
        };
        self.session_seq += 1;
        db.append_message(
            conv_id,
            role,
            content,
            tool_name,
            call_id,
            tool_calls_json,
            self.session_seq,
        )
        .ok();
    }

    /// Append an assistant message to the session database.
    pub(crate) fn append_assistant_to_db(&mut self, content: &str, tool_calls_json: Option<&str>) {
        self.append_db_message("assistant", content, None, None, tool_calls_json);
    }

    /// Append a tool result to the session database.
    pub(crate) fn append_tool_result_to_db(&mut self, name: &str, call_id: &str, content: &str) {
        self.append_db_message("tool", content, Some(name), Some(call_id), None);
    }
    /// Record a token-usage event for the active conversation. The Driver runs
    /// with a `NullSessionSink` (the TUI owns `session_seq`), so usage events it
    /// reports are returned in the `DriverOutcome` and written here instead.
    pub(crate) fn record_usage_to_db(&mut self, usage: &crate::runtime::UsageRecord) {
        if let Some(ref db) = self.session_db
            && let Some(conv_id) = self.conversation_id
        {
            db.record_usage(
                conv_id,
                &usage.provider,
                &usage.model,
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.cached_tokens,
                usage.cost,
                usage.is_estimated,
            )
            .ok();
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
            // Replace the active transcript. The caller (e.g. compact.lua's
            // `display` return) already surfaces the compaction summary and
            // savings to the user, so no separate marker is pushed here.
            self.transcript = new_messages;

            // Recompute context_length estimate from the new transcript.
            let history = crate::chat::build_chat_history(&self.transcript, None);
            let prompt_chars =
                crate::ui::app::App::estimate_context_chars(&history, &self.tools.definitions());
            self.token_stats.set_context_estimate(prompt_chars);
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
                "Configuration applied.".to_string()
            }
            crate::ext::types::ConfigAction::ReloadTools => {
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
                        model: self.model.clone(),
                        provider: self.provider.clone(),
                        tool_allowlist: None,
                    },
                    &self.model,
                    &self.provider,
                );
                self.extensions = booted.manager;
                self.tools = booted.tools;
                self.user_config.apply_custom_configs(&custom);
                self.approval_mode = self.user_config.approval_mode;
                self.custom_configs = custom;
                self.user_config.enabled_tools = self.tools.enabled_names();
                let count = self.tools.definitions().len();
                format!("Tools and Lua extensions reloaded. {count} tools enabled.")
            }
            crate::ext::types::ConfigAction::SwitchProvider { id } => {
                let mut custom = config::custom::CustomConfigs::load();
                let providers_config = custom.derive_providers_config();
                match providers::create_provider_with_config(&id, &providers_config) {
                    Ok(new_provider) => match new_provider.validate().await {
                        Ok(()) => {
                            self.provider =
                                format!("{} ({})", new_provider.name(), new_provider.id());
                            self.model = new_provider.model().to_string();
                            self.llm = std::sync::Arc::from(new_provider);
                            custom.set_last_provider(&id);
                            self.custom_configs = custom;
                            format!("Switched to {} ({})", self.model, self.provider)
                        }
                        Err(err) => format!("Provider validation failed: {err}"),
                    },
                    Err(err) => err.to_string(),
                }
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

        // Adopt the loaded conversation id so new messages append to it. End the
        // previous conversation we're leaving (if different), and clear the
        // target's `ended_at` so it's no longer marked closed.
        if let (Some(db), Some(target)) = (&self.session_db, load.conversation_id) {
            if let Some(old_id) = self.conversation_id
                && old_id != target
            {
                db.end_conversation(old_id).ok();
            }
            db.reopen_conversation(target).ok();
            self.session_seq = db.max_message_seq(target).unwrap_or(0);
            self.conversation_id = Some(target);
        }

        // Swap in the loaded transcript and rebuild the rendered scrollback from
        // it (clearing the previous conversation's messages).
        self.transcript = load.messages;
        self.messages.clear();
        self.messages
            .extend(Self::rebuild_scrollback_from_transcript(&self.transcript));
        self.renderer.scrollback_cursor = 0;

        // Update token counts: reset cumulative stats and estimate context from
        // the loaded transcript.
        self.token_stats.reset();
        let history = crate::chat::build_chat_history(&self.transcript, None);
        let prompt_chars = Self::estimate_context_chars(&history, &self.tools.definitions());
        self.token_stats.set_context_estimate(prompt_chars);

        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.cancel_streaming = false;
        self.redraw(term)?;
        Ok(())
    }

    /// Convert a loaded transcript into rendered scrollback rows. Tool-call-only
    /// assistant messages (empty content) are skipped; tool results render as a
    /// compact tool row.
    fn rebuild_scrollback_from_transcript(transcript: &[crate::llm::ChatMessage]) -> Vec<Message> {
        use crate::llm::ChatRole;
        let mut rows = Vec::new();
        for msg in transcript {
            match msg.role {
                ChatRole::User => rows.push(Message::user(msg.content.clone())),
                ChatRole::Assistant => {
                    if !msg.content.trim().is_empty() {
                        rows.push(Message::assistant(msg.content.clone()));
                    }
                }
                ChatRole::Tool => {
                    let label = msg.name.clone().unwrap_or_else(|| "tool".to_string());
                    rows.push(Message::tool_row(label, false));
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
        self.subagent_seen_version = u64::MAX;
        self.queue.clear();
        self.active_prompt = None;
        self.pending_approval = None;
    }

    fn start_new_conversation(&mut self) {
        if let Some(ref db) = self.session_db {
            if let Some(conv_id) = self.conversation_id {
                db.end_conversation(conv_id).ok();
            }
            match db.create_conversation(self.llm.id(), self.llm.model()) {
                Ok(conv_id) => {
                    self.conversation_id = Some(conv_id);
                    self.session_seq = 0;
                }
                Err(err) => {
                    eprintln!("warning: failed to create conversation: {err}");
                }
            }
        }
    }

    /// Clear chat history, end the current DB conversation, start a fresh one,
    /// and display a usage summary.
    fn clear_chat(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        self.reset_transient_ui_state();

        let summary = if self.token_stats.request_count > 0 {
            format!("Session: {}. Chat cleared.", self.token_stats.one_liner())
        } else {
            "Chat cleared.".to_string()
        };

        self.start_new_conversation();
        self.token_stats.reset();

        self.messages.clear();
        self.transcript.clear();
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
        self.extensions
            .dispatch_simple("mode_change", serde_json::json!({ "mode": mode }));
    }

    fn apply_custom_configs_to_runtime(&mut self, custom: config::custom::CustomConfigs) {
        self.user_config.apply_custom_configs(&custom);
        self.approval_mode = self.user_config.approval_mode;
        self.custom_configs = custom;
    }

    pub async fn run(&mut self) -> io::Result<()> {
        let mut terminal = Renderer::init_terminal(MIN_ROWS)?;

        if let Some(warning) = self.init_session_db() {
            self.messages.push(Message::system(warning));
        }

        self.renderer
            .flush_new_to_scrollback(&self.messages, &mut terminal)?;
        self.refresh_subagent_pane();
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
                            let burst = collect_non_bracketed_paste_burst(c)?;
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

            // Tick subagent jobs: refresh pane + auto-inject finished results.
            self.tick_subagents(&mut terminal).await?;

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
        // truncates content to its area, so clamping degrades gracefully.
        .min(size.height.max(1));

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
            self.renderer.streaming_lines_flushed = 0;
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
            &self.model,
            &self.token_stats,
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

    /// Tick subagent job status: refresh pane if needed, auto-inject
    /// finished results when the TUI is idle.
    async fn tick_subagents(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        // 1. Pane refresh: version change, or ~1s ticker while jobs run.
        if self.maybe_refresh_subagent_pane() {
            self.redraw(term)?;
        }

        // 2. Auto-injection: only when idle. Peek first; mark the jobs
        //    consumed only after the injection actually went through.
        if self.active_prompt.is_none() && !self.streaming && self.queue.is_empty() {
            let finished = crate::ext::jobs::registry().peek_finished_unconsumed();
            if let Some((text, display)) = Self::format_subagent_results(&finished) {
                let ids: Vec<String> = finished.iter().map(|j| j.id.clone()).collect();
                let draft = std::mem::take(&mut self.input.buffer);
                let draft_cursor = self.input.cursor_pos;
                self.submit_user_turn(text, Some(display), term).await?;
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

    /// Refresh the subagent pane when the registry version changed or, while
    /// jobs are running, at least once per second so elapsed time and token
    /// counters stay live. Returns `true` when the pane was refreshed.
    pub(crate) fn maybe_refresh_subagent_pane(&mut self) -> bool {
        let registry = crate::ext::jobs::registry();
        let version = registry.version();
        let periodic = self.subagent_last_refresh.elapsed() >= std::time::Duration::from_secs(1)
            && !registry.running_ids().is_empty();
        if version == self.subagent_seen_version && !periodic {
            return false;
        }
        // Unhide the pane when a new subagent job starts while hidden.
        if version != self.subagent_seen_version && !registry.running_ids().is_empty() {
            self.panes_visible = true;
        }
        self.refresh_subagent_pane();
        self.subagent_seen_version = version;
        self.subagent_last_refresh = std::time::Instant::now();
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
    /// `maybe_refresh_subagent_pane`: called on the render tick and the live
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

    /// Refresh the subagent live-pane from the job registry.
    ///
    /// Rendered natively in Rust (no Lua) so the pane stays live even while
    /// a Lua tool blocks the VM (e.g. a long `ctx.agent.wait`).
    /// Only shows when there are running jobs; hides when all are idle.
    fn refresh_subagent_pane(&mut self) {
        let agents = self.extensions.subagent_names();
        let jobs = crate::ext::jobs::registry().all_jobs();
        let has_running = jobs
            .iter()
            .any(|j| j.status == crate::ext::jobs::JobStatus::Running);
        if has_running {
            if let Some(page) = crate::ui::subagent_pane::render(agents, &jobs) {
                let (_, new_active) = PanePage::upsert(&mut self.pages, self.active_page, page);
                self.active_page = new_active;
                self.panes_visible = true;
            }
        } else {
            // No running jobs — hide the pane.
            self.active_page = PanePage::remove(
                &mut self.pages,
                crate::ui::subagent_pane::PANE_SOURCE,
                self.active_page,
            );
        }
    }

    /// Format subagent results for auto-injection.
    /// Returns `(turn_text, display_text)` or `None` when no finished jobs.
    fn format_subagent_results(jobs: &[crate::ext::jobs::Job]) -> Option<(String, String)> {
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
            "[automated message] Results from background sub-agent jobs you dispatched earlier are now ready. \
             Review them and continue the task they were dispatched for; if nothing remains to be done, \
             summarize the outcomes for the user.\n\n{}",
            lines.join("\n\n")
        );
        let display: String = jobs
            .iter()
            .map(|j| format!("{} {}", j.agent, job_status_sym(j.status)))
            .collect::<Vec<_>>()
            .join(", ");
        let display_text = format!("[subagent results: {}]", display);
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
    fn collect_commands(&self) -> Vec<(String, String)> {
        let mut cmds = crate::ui::autocomplete::builtin_commands();
        if self.extensions.is_available() {
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
        reply: tokio::sync::oneshot::Sender<CallOutcome>,
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
            reply,
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
            let _ = pending.reply.send(resolved);
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

        if has_lua_config && cmd == "provider" && arg.is_empty() {
            if self
                .run_lua_command("config", "providers", term)
                .await
                .is_some()
            {
                return Ok(());
            }
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
        if has_lua_config && cmd == "config" {
            if self.run_lua_command("config", &arg, term).await.is_some() {
                return Ok(());
            }
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
            return self.clear_chat(term);
        }
        if cmd == "stats" {
            return self.open_stats_dashboard(term);
        }
        if cmd == "setup" {
            return self.open_setup_wizard(term);
        }

        let prev_provider = self.llm.id().to_string();
        let prev_model = self.llm.model().to_string();

        let lua_cmds: Vec<(String, String)> = if self.extensions.is_available() {
            self.extensions
                .commands()
                .iter()
                .map(|c| (c.name.clone(), c.description.clone()))
                .collect()
        } else {
            Vec::new()
        };

        let result = commands::handle(
            &cmd,
            &arg,
            &mut self.messages,
            &mut self.transcript,
            &mut self.token_stats,
            &mut self.renderer,
            term,
            &mut self.llm,
            &mut self.provider,
            &mut self.model,
            &mut self.custom_configs,
            &lua_cmds,
        )
        .await?;

        let provider_changed = self.llm.id() != prev_provider;
        let model_changed = self.llm.model() != prev_model;
        if provider_changed || model_changed {
            self.start_new_conversation();
        }

        match result {
            commands::CommandResult::Quit => {
                if let Some(notice) = self.request_quit() {
                    self.messages.push(Message::system(notice));
                    self.renderer
                        .flush_new_to_scrollback(&self.messages, term)?;
                    self.redraw(term)?;
                }
            }
            commands::CommandResult::Continue { reply } => {
                self.messages.push(Message::system(reply));
                self.renderer
                    .flush_new_to_scrollback(&self.messages, term)?;
                self.redraw(term)?;
            }
            commands::CommandResult::OpenEditor => self.open_editor(term).await?,
        }
        Ok(())
    }

    /// Run a Lua-registered command. Returns `Some(())` if the command was found and handled.
    /// Snapshot the app-derived `ctx` fields for the current conversation.
    /// Shared by the command runner, the tool dispatch path, and `before_turn`
    /// so every Lua entry point sees an identical `ctx`.
    pub(super) fn app_ctx_state(&self) -> crate::ext::ctx::AppCtxState {
        let by_provider = crate::ext::ctx::usage_by_provider_context(
            self.session_db.as_ref(),
            self.conversation_id,
        );
        crate::ext::ctx::AppCtxState::new(
            &self.tools,
            &self.token_stats,
            &self.approval_mode,
            self.conversation_id,
            self.llm.id(),
            self.llm.model(),
            by_provider,
            self.transcript.clone(),
        )
    }

    async fn run_lua_command(
        &mut self,
        cmd: &str,
        arg: &str,
        term: &mut BoneTerminal,
    ) -> Option<()> {
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

        if let Some(Some((mut reply, submit, action, display_role))) = reply {
            if let Some(action) = action {
                if let Ok(Some(action_reply)) = self.apply_lua_action(action, term).await {
                    reply = action_reply;
                }
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
                self.submit_user_turn(reply, Some(display), term).await.ok();
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
        if let Some(status) = self.try_tmux_popup("stats-popup", "96%", "92%", term)?
            && status.success()
        {
            return Ok(());
        }

        let Some(ref db) = self.session_db else {
            return self.show_reply("Stats database is not available.".to_string(), term);
        };

        let result = crate::ui::stats::run(|range| match range {
            None => db
                .usage_stats_snapshot()
                .map_err(|err| io::Error::other(err.to_string())),
            Some(r) => db
                .usage_stats_range(&r.start, &r.end)
                .map_err(|err| io::Error::other(err.to_string())),
        });

        self.force_redraw(term)?;
        if let Err(err) = result {
            return self.show_reply(format!("Stats dashboard failed: {err}"), term);
        }
        Ok(())
    }

    fn open_setup_wizard(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let mut ran = false;
        if let Some(status) = self.try_tmux_popup("setup", "80%", "80%", term)? {
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
