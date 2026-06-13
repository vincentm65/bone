pub mod stream;

use crate::chat::Message;
use crate::config::{self, UserConfig};
use crate::llm::{ChatMessage, LlmProvider, TokenStats, providers};
use crate::session_db::SessionDb;

use crate::ext::ExtensionManager;
use crate::tools::registry::ToolHandler;
use crate::tools::{ApprovalMode, ToolCall};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::collections::VecDeque;
use std::io;
use std::time::Instant;
use tokio::time::{self, Duration};

use super::autocomplete::AutocompleteState;
use super::commands;
use super::input::{InputAction, InputState};
use super::pane_page::PanePage;
use super::prompt::{Decision, Prompt};
use super::render::{
    BoneTerminal, MAX_PANE_ROWS, MIN_ROWS, PaneDraw, Renderer, StatusInfo,
    clamped_pane_visible_rows,
};

pub struct App {
    pub messages: Vec<Message>,
    pub transcript: Vec<ChatMessage>,
    pub input: InputState,
    pub streaming: bool,
    pub provider: String,
    pub model: String,
    pub llm: Box<dyn LlmProvider>,
    pub should_quit: bool,
    pub renderer: Renderer,
    pub user_config: UserConfig,
    pub custom_configs: config::custom::CustomConfigs,
    pub queue: VecDeque<String>,
    pub tools: ToolHandler,

    pub approval_mode: ApprovalMode,
    pub active_prompt: Option<Prompt>,
    /// Set to `true` to abort the current streaming response.
    pub cancel_streaming: bool,
    /// Timestamp of the last Ctrl+C press (for double-tap quit).
    pub last_ctrl_c: Option<Instant>,
    /// Cumulative token usage stats.
    pub token_stats: TokenStats,

    /// Active pane pages displayed between input and status bar.
    pub pages: Vec<PanePage>,
    /// Index of the currently visible pane page.
    pub active_page: usize,
    /// Whether pane pages are shown in the bottom pane.
    pub panes_visible: bool,

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
        let provider = format!("{} ({})", llm.name(), llm.id());
        let model = llm.model().to_string();
        let approval_mode = user_config.approval_mode;
        // Boot Lua extension system and build tool handler.
        let booted = crate::ext::boot_with_tools(
            &crate::config::bone_dir(),
            &std::env::current_dir().unwrap_or_default(),
            &mut custom_configs,
            true,
            crate::ext::BootOptions::default(),
        );
        let extensions = booted.manager;
        let tools = booted.tools;

        // Create renderer with Lua theme applied over defaults.
        let mut renderer = Renderer::new();
        extensions.theme_snapshot().apply_to(&mut renderer.theme);

        // Apply Lua config snapshot — overrides YAML config values.
        apply_lua_config_snapshot(&mut user_config, extensions.config_snapshot());

        // Capture keymap snapshot before `extensions` is moved into the struct.
        let lua_keymap = extensions.keymap_snapshot().clone();

        let messages = vec![Message::system(
            "bone v0.1.0 — type /help for commands. Ctrl+C twice to quit.",
        )];

        Ok(Self {
            messages,
            transcript: Vec::new(),
            input: InputState::default(),
            streaming: false,
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
            cancel_streaming: false,
            last_ctrl_c: None,
            token_stats: TokenStats::new(),
            pages: Vec::new(),
            active_page: 0,
            panes_visible: true,

            session_db: None,
            conversation_id: None,
            session_seq: 0,
            turn_start: None,
            turn_paused_duration: std::time::Duration::ZERO,
            turn_pause_start: None,
            autocomplete: None,
            extensions,
            lua_keymap,
            shown_tool_rows: std::collections::HashSet::new(),
            subagent_seen_version: u64::MAX,
            subagent_last_refresh: std::time::Instant::now(),
            quit_despite_jobs: false,
        })
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
    /// Append an assistant message to the session database.
    pub(crate) fn append_assistant_to_db(&mut self, content: &str, tool_calls_json: Option<&str>) {
        if let Some(ref db) = self.session_db
            && let Some(conv_id) = self.conversation_id
        {
            self.session_seq += 1;
            db.append_message(
                conv_id,
                "assistant",
                content,
                None,
                None,
                tool_calls_json,
                self.session_seq,
            )
            .ok();
        }
    }

    /// Append a tool result to the session database.
    pub(crate) fn append_tool_result_to_db(&mut self, name: &str, call_id: &str, content: &str) {
        if let Some(ref db) = self.session_db
            && let Some(conv_id) = self.conversation_id
        {
            self.session_seq += 1;
            db.append_message(
                conv_id,
                "tool",
                content,
                Some(name),
                Some(call_id),
                None,
                self.session_seq,
            )
            .ok();
        }
    }
    /// Start a new conversation in the database (used by /clear, /new).
    /// Apply a generic action returned by a Lua command or hook.
    pub(crate) fn apply_lua_action(
        &mut self,
        action: crate::ext::types::LuaReturnAction,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        if let Some(new_messages) = action.conversation_replace {
            // Validate: all messages must have valid roles and content.
            for msg in &new_messages {
                if msg.content.is_empty() {
                    // Allow empty content for tool messages, skip validation.
                }
            }

            // Replace the active transcript.
            self.transcript = new_messages;

            // Rebuild the visible chat messages minimally:
            // keep only a system note for this replacement since we can't
            // reconstruct the full prior UI from ChatMessages alone.
            // Existing messages stay in scrollback; add a compact marker.
            self.messages.push(Message::system(
                "Context compacted. Summary in transcript; recent messages preserved.",
            ));
            self.renderer
                .flush_new_to_scrollback(&self.messages, term)?;

            // Recompute context_length estimate from the new transcript.
            let history = crate::chat::build_chat_history(&self.transcript, None);
            let prompt_chars =
                crate::ui::app::App::estimate_context_chars(&history, &self.tools.definitions());
            self.token_stats.set_context_estimate(prompt_chars);
        }
        Ok(())
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
        self.cancel_streaming = true;
        self.pages.clear();
        self.active_page = 0;
        self.tools.state_map.clear();
        self.subagent_seen_version = u64::MAX;

        let summary = if self.token_stats.request_count > 0 {
            format!("Session: {}. Chat cleared.", self.token_stats.one_liner())
        } else {
            "Chat cleared.".to_string()
        };

        self.start_new_conversation();
        self.token_stats.reset();

        self.messages.clear();
        self.transcript.clear();
        self.messages.push(Message::system(
            "bone v0.1.0 — type /help for commands. Ctrl+C twice to quit.",
        ));
        self.messages.push(Message::system(summary));
        self.renderer.scrollback_cursor = self.messages.len();
        self.renderer
            .render_banner(term, &self.provider, &self.model)?;
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
        self.renderer
            .render_banner(&mut terminal, &self.provider, &self.model)?;
        self.refresh_subagent_pane();
        self.force_redraw(&mut terminal)?;

        while !self.should_quit {
            if event::poll(std::time::Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.handle_key(key.code, key.modifiers, &mut terminal)
                            .await?;
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
        let width = terminal.size()?.width;
        let desired = Renderer::desired_height(
            &self.input,
            self.active_prompt.as_ref(),
            width,
            self.visible_pages(),
            self.active_page,
            self.autocomplete.as_ref(),
        );

        if desired != self.renderer.viewport_height {
            Renderer::resize_viewport(terminal, desired)?;
            self.renderer.viewport_height = desired;
        }

        terminal.draw(|frame| self.draw(frame))?;
        Ok(())
    }

    /// Redraw from scratch, updating the tracked terminal size.
    /// Used after resize or stale-size detection.
    fn force_redraw(&mut self, terminal: &mut BoneTerminal) -> io::Result<()> {
        self.ensure_viewport_and_draw(terminal)?;
        self.renderer.last_size = Some(crossterm::terminal::size()?);
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
        let mut elapsed = start.elapsed();
        // If currently paused, don't add the ongoing pause
        // If NOT currently paused, subtract accumulated pause time
        if self.turn_pause_start.is_none() {
            elapsed = elapsed.saturating_sub(self.turn_paused_duration);
        } else {
            // Currently paused: subtract accumulated + current pause so far
            // But we don't add to turn_paused_duration until resume,
            // so just subtract what we've accumulated so far
            elapsed = elapsed.saturating_sub(self.turn_paused_duration);
        }
        let total_secs = elapsed.as_secs();
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        Some(format!("{}:{:02}", mins, secs))
    }

    pub(crate) fn status_info(&self) -> StatusInfo {
        self.stream_status_info_with_tokens(None)
    }

    /// Build a [`StatusInfo`] for the streaming spinner wait, with an optional
    /// live cumulative output-token estimate.
    fn stream_status_info_with_tokens(&self, estimated_tokens: Option<u64>) -> StatusInfo {
        let elapsed = self.timer_elapsed();
        stream_status_info_with_token_stats(
            estimated_tokens,
            &self.model,
            &self.token_stats,
            self.streaming,
            self.approval_mode,
            self.queue.len(),
            &self.user_config,
            elapsed,
        )
    }

    /// Look up a keymap binding for the given key combo.
    /// Returns the action name if found in the current mode.
    fn lookup_keymap(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
        let mode = if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT {
            "n"
        } else {
            "i"
        };

        let bindings = match mode {
            "n" => &self.lua_keymap.normal,
            "i" => &self.lua_keymap.insert,
            _ => return None,
        };

        for binding in bindings {
            if key_matches(&binding.key, code, modifiers) {
                return Some(binding.action.clone());
            }
        }
        None
    }

    /// Execute a keymap action.
    async fn handle_keymap_action(
        &mut self,
        action: String,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        match action.as_str() {
            "toggle_panes" => {
                self.panes_visible = !self.panes_visible;
                self.redraw(term)
            }
            "cycle_approval_mode" => {
                self.approval_mode = self.approval_mode.cycle();
                self.user_config.approval_mode = self.approval_mode;
                self.persist_runtime_config();
                self.redraw(term)
            }
            "cursor_to_start" => {
                self.input.cursor_to_start();
                self.redraw(term)
            }
            "cursor_to_end" => {
                self.input.cursor_to_end();
                self.redraw(term)
            }
            other => {
                eprintln!("bone-lua warn: unknown keymap action '{other}'; ignoring");
                self.redraw(term)
            }
        }
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
        self.refresh_subagent_pane();
        self.subagent_seen_version = version;
        self.subagent_last_refresh = std::time::Instant::now();
        true
    }

    /// Refresh the subagent live-pane from the job registry.
    ///
    /// Rendered natively in Rust (no Lua) so the pane stays live even while
    /// a Lua tool blocks the VM (e.g. a long `ctx.agent.wait`).
    fn refresh_subagent_pane(&mut self) {
        let agents = self.extensions.subagent_names();
        if agents.is_empty() {
            return;
        }
        let jobs = crate::ext::jobs::registry().all_jobs();
        if let Some(page) = crate::ui::subagent_pane::render(agents, &jobs) {
            let (_, new_active) = PanePage::upsert(&mut self.pages, self.active_page, page);
            self.active_page = new_active;
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
            let status_sym = match job.status {
                crate::ext::jobs::JobStatus::Done => "✓",
                crate::ext::jobs::JobStatus::Error => "✗",
                crate::ext::jobs::JobStatus::Running => "◐",
            };
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
            .map(|j| {
                let sym = match j.status {
                    crate::ext::jobs::JobStatus::Done => "✓",
                    crate::ext::jobs::JobStatus::Error => "✗",
                    crate::ext::jobs::JobStatus::Running => "◐",
                };
                format!("{} {}", j.agent, sym)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let display_text = format!("[subagent results: {}]", display);
        Some((turn_text, display_text))
    }
}

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
    StatusInfo {
        model: model.to_string(),
        token_stats: token_stats.clone(),
        streaming_completion_tokens,
        streaming,
        approval_mode,
        queue_len,
        status_show: cfg.status_show.clone(),
        elapsed,
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
            self.active_prompt.as_ref(),
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

    async fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        // ── Interactive pane key interception ─────────────────────────
        // dispatch_interaction_key handles submit/cancel cleanup (removing the
        // pane when the interaction goes inactive, except the shared "interact"
        // page).
        if self.panes_visible
            && PanePage::dispatch_interaction_key(
                &mut self.pages,
                &mut self.active_page,
                code,
                modifiers,
            )
        {
            return self.redraw(term);
        }

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
                    let max_scroll = page
                        .content
                        .len()
                        .saturating_sub(clamped_pane_visible_rows(page.visible_rows));
                    page.scroll = (page.scroll + MAX_PANE_ROWS).min(max_scroll);
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
                let max_scroll = page
                    .content
                    .len()
                    .saturating_sub(clamped_pane_visible_rows(page.visible_rows));
                page.scroll = (page.scroll + 1).min(max_scroll);
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

        match self.input.apply_key(code, modifiers) {
            InputAction::Cancel => self.handle_ctrl_c(term),
            InputAction::Submit => {
                self.autocomplete = None;
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

    /// Cancel any active interaction on the current pane.
    /// Returns true if an interaction was cancelled.
    fn cancel_active_interaction(&mut self) -> bool {
        if !self.panes_visible {
            return false;
        }
        let active_idx = self.active_page.min(self.pages.len().saturating_sub(1));
        self.pages.get(active_idx).is_some_and(|page| {
            page.interaction.as_ref().is_some_and(|i| {
                if i.is_active() {
                    i.cancel();
                    true
                } else {
                    false
                }
            })
        })
    }

    /// Handle Ctrl+C: cancel streaming response, or quit on double-tap.
    fn handle_ctrl_c(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        // If an interactive pane is active, cancel it instead of quitting.
        if self.cancel_active_interaction() {
            return self.redraw(term);
        }

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

        if self.streaming {
            self.cancel_streaming = true;
        }
        self.queue.clear();

        self.redraw(term)?;
        Ok(())
    }

    /// Show a blocking prompt for a tool call that needs approval.
    /// Renders the prompt options in the fixed bottom pane, waits for a choice,
    /// then restores the normal input/status display.
    pub(crate) fn prompt_and_wait(
        &mut self,
        call: &ToolCall,
        term: &mut BoneTerminal,
    ) -> io::Result<Decision> {
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

        let mut prompt = if call.name == "shell" {
            let title = call.arguments["display_label"]
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
                });
            Prompt::new(
                format!("{} — {}", call.name, title),
                vec!["Accept", "Advise", "Cancel"],
            )
        } else {
            Prompt::new(
                format!("{} — {}", call.name, summary),
                vec!["Accept", "Advise", "Cancel"],
            )
        };
        prompt.full_command = if call.name == "shell" {
            call.arguments["command"].as_str().map(String::from)
        } else {
            None
        };
        self.active_prompt = Some(prompt);

        self.redraw(term)?;

        let mut advising = false;

        let decision = loop {
            if event::poll(std::time::Duration::from_millis(50))? {
                let event = event::read()?;
                if let Event::Paste(text) = event {
                    if advising {
                        self.input.insert_paste(&text);
                        self.redraw(term)?;
                    }
                    continue;
                }
                let Event::Key(key) = event else {
                    continue;
                };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if advising {
                    match self.input.apply_key(key.code, key.modifiers) {
                        InputAction::Submit => {
                            let advice = self.input.buffer.trim().to_string();
                            self.input.reset();
                            break Decision::Advise(advice);
                        }
                        InputAction::Cancel | InputAction::Escape => {
                            self.input.clear_buffer();
                            break Decision::Cancel;
                        }
                        InputAction::Redraw => self.redraw(term)?,
                        InputAction::None if key.code == KeyCode::Enter => {
                            break Decision::Advise(String::new());
                        }
                        _ => {}
                    }
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
                        if let Some(prompt) = self.active_prompt.as_ref() {
                            let decision = prompt.decision();
                            if matches!(decision, Decision::Advise(_)) {
                                self.active_prompt = None;
                                advising = true;
                                self.redraw(term)?;
                                continue;
                            }
                            break decision;
                        }
                        break Decision::Cancel;
                    }
                    KeyCode::Esc => {
                        break Decision::Cancel;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break Decision::Cancel;
                    }
                    KeyCode::Char(c)
                        if key.modifiers.is_empty()
                            && self
                                .active_prompt
                                .as_ref()
                                .is_some_and(|prompt| prompt.selected == 1) =>
                    {
                        self.input.insert_char(c);
                        self.active_prompt = None;
                        advising = true;
                        self.redraw(term)?;
                    }
                    _ => {}
                }
            }
        };

        self.active_prompt = None;
        self.redraw(term)?;

        Ok(decision)
    }

    pub(super) async fn handle_command(
        &mut self,
        input: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        let cmd = parts[0].to_string();
        let arg = parts.get(1).copied().unwrap_or("").to_string();

        if cmd == "provider" && arg.is_empty() {
            return self.config_picker(term, Some("providers")).await;
        }
        if cmd == "tools" {
            return self.handle_tools_command(&arg, term).await;
        }
        if cmd == "config" {
            return self.config_picker(term, None).await;
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
            if enabled.contains(&cmd) {
                if let Some(_reply) = self.run_lua_command(&cmd, &arg, term).await {
                    return Ok(());
                }
            }
        }

        if matches!(cmd.as_str(), "clear" | "new") {
            return self.clear_chat(term);
        }
        if cmd == "stats" {
            return self.open_stats_dashboard(term);
        }

        let prev_provider = self.llm.id().to_string();
        let prev_model = self.llm.model().to_string();

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
    async fn run_lua_command(
        &mut self,
        cmd: &str,
        arg: &str,
        term: &mut BoneTerminal,
    ) -> Option<()> {
        let lua = self.extensions.lua_handle();
        let cmd_owned = cmd.to_string();
        let arg_owned = arg.to_string();
        let session_id = self.conversation_id;
        let provider = self.llm.id().to_string();
        let model = self.llm.model().to_string();
        let approval_mode = self.approval_mode;
        let tools = self.tools.clone();
        let defs = self.tools.definitions();
        let schema_json = serde_json::to_string(&defs).unwrap_or_default();
        let schema_chars = schema_json.len() as u64;
        let schema_tokens = (schema_chars as f64 / 3.8).ceil() as u64;
        let sys = crate::llm::prompts::system_prompt();
        let sys_chars = sys.len() as u64;
        let sys_tokens = (sys_chars as f64 / 3.8).ceil() as u64;
        let by_provider = self
            .session_db
            .as_ref()
            .and_then(|db| session_id.and_then(|id| db.usage_by_provider(id).ok()))
            .unwrap_or_default()
            .into_iter()
            .map(|p| crate::ext::ctx::UsageProviderContext {
                provider: p.provider,
                model: p.model,
                prompt_tokens: p.prompt_tokens.max(0) as u64,
                completion_tokens: p.completion_tokens.max(0) as u64,
                cached_tokens: p.cached_tokens.max(0) as u64,
                cost: p.cost,
                request_count: p.request_count.max(0) as u64,
            })
            .collect();
        let usage = crate::ext::ctx::UsageContext {
            request_count: self.token_stats.request_count,
            sent: self.token_stats.sent,
            received: self.token_stats.received,
            cached: self.token_stats.cached,
            cost: self.token_stats.cost,
            context_length: self.token_stats.context_length,
            tool_count: defs.len() as u64,
            tool_schema_chars: schema_chars,
            tool_schema_tokens: schema_tokens,
            system_prompt_chars: sys_chars,
            system_prompt_tokens: sys_tokens,
            by_provider,
        };

        let transcript_snapshot = self.transcript.clone();

        let mut task = tokio::task::spawn_blocking(move || {
            let lua = lua.lock().unwrap_or_else(|e| e.into_inner());

            // Find the command handler using the shared lookup.
            let handler = match crate::ext::ops_commands::find_handler(&lua, &cmd_owned) {
                Some(f) => f,
                None => return Some(None),
            };

            let config_dir = crate::config::bone_dir().to_string_lossy().to_string();
            let shared_state: crate::ext::ctx::SharedState =
                std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
            let mut ctx_cfg = crate::ext::ctx::CtxConfig::new(config_dir, shared_state);
            ctx_cfg.tool_handler = Some(tools);
            ctx_cfg.approval_mode = approval_mode;
            ctx_cfg.session_id = session_id;
            ctx_cfg.provider = Some(provider);
            ctx_cfg.model = Some(model);
            ctx_cfg.usage = Some(usage);
            ctx_cfg.conversation_history = Some(transcript_snapshot.clone());
            let ctx_table = crate::ext::ctx::create_ctx_table(&lua, &ctx_cfg).ok()?;

            // Release the project Lua mutex before calling into Lua: a nested
            // LuaTool invocation via ctx.tools.call runs inline on this thread
            // and must re-acquire it (std::sync::Mutex is not reentrant).
            drop(lua);

            let result = handler.call::<mlua::Value>((arg_owned, ctx_table));

            let reply = match result {
                Ok(value) => crate::ext::types::parse_lua_command_return(value)
                    .map(|ret| (ret.output, ret.submit, ret.action)),
                Err(e) => {
                    eprintln!("bone-lua error: command '{cmd_owned}': {e}");
                    Some((format!("Lua command error: {e}"), false, None))
                }
            };
            Some(reply)
        });

        self.streaming = true;
        self.turn_start = Some(Instant::now());
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;
        self.redraw(term).ok();

        let mut spinner = time::interval(Duration::from_millis(90));
        let reply = loop {
            tokio::select! {
                result = &mut task => {
                    self.streaming = false;
                    self.turn_start = None;
                    break result.ok().flatten();
                }
                _ = spinner.tick() => {
                    let status_info = self.status_info();
                    let pages: &[PanePage] = if self.panes_visible { &self.pages } else { &[] };
                    self.renderer.tick_spinner(term, &PaneDraw {
                        input: &self.input,
                        status_info: &status_info,
                        pages,
                        active_page: self.active_page,
                        autocomplete: None,
                    }).ok();
                }
            }
        };

        if let Some(Some((reply, submit, action))) = reply {
            if let Some(action) = action {
                self.apply_lua_action(action, term).ok();
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
                self.show_reply(reply, term).ok();
            }
        } else {
            self.redraw(term).ok();
        }

        Some(())
    }

    fn panel_key(&mut self, term: &mut BoneTerminal) -> io::Result<(KeyCode, KeyModifiers)> {
        loop {
            if event::poll(std::time::Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        return Ok((key.code, key.modifiers));
                    }
                    Event::Resize(_, _) => self.force_redraw(term)?,
                    _ => {}
                }
            }
        }
    }

    fn close_panel(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        self.active_prompt = None;
        self.redraw(term)
    }

    fn show_reply(&mut self, reply: impl Into<String>, term: &mut BoneTerminal) -> io::Result<()> {
        self.messages.push(Message::system(reply.into()));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)
    }

    fn mask_secret(value: &str) -> String {
        if value.is_empty() {
            "(empty)".to_string()
        } else {
            "*".repeat(value.chars().count().clamp(4, 12))
        }
    }

    fn edit_value(
        &mut self,
        label: &str,
        initial: &str,
        secret: bool,
        term: &mut BoneTerminal,
    ) -> io::Result<Option<String>> {
        self.input.buffer = if secret {
            String::new()
        } else {
            initial.to_string()
        };
        self.input.cursor_pos = self.input.buffer.chars().count();
        loop {
            let value = if secret {
                Self::mask_secret(&self.input.buffer)
            } else {
                self.input.buffer.clone()
            };
            let mut prompt =
                Prompt::new(format!("Edit {label}"), vec![format!("{label}: {value}")]);
            prompt.hint = Some("Enter save value  Esc cancel".to_string());
            self.active_prompt = Some(prompt);
            self.redraw(term)?;
            match event::read()? {
                Event::Paste(text) => self.input.insert_paste(&text),
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.code == KeyCode::Enter {
                        let value = self.input.buffer.clone();
                        self.input.clear_buffer();
                        return Ok(Some(value));
                    }
                    if key.code == KeyCode::Esc
                        || (key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL))
                    {
                        self.input.clear_buffer();
                        return Ok(None);
                    }
                    let _ = self.input.apply_key(key.code, key.modifiers);
                }
                _ => {}
            }
        }
    }

    fn provider_editor(&mut self, id: String, term: &mut BoneTerminal) -> io::Result<()> {
        let entry = self
            .custom_configs
            .get_provider_entry("providers", &id)
            .ok_or_else(|| io::Error::other(format!("unknown provider `{id}`")))?;
        let mut entry = entry;
        let mut selected = 0usize;
        loop {
            let options = vec![
                format!("label · {}", entry.label),
                format!("model · {}", entry.model),
                format!("base_url · {}", entry.base_url),
                format!("endpoint · {}", entry.endpoint),
                format!("handler · {}", entry.handler),
                format!("api_key · {}", Self::mask_secret(&entry.api_key)),
                "Save changes".to_string(),
            ];
            let mut prompt = Prompt::new(format!("Edit provider: {id}"), options);
            prompt.set_selected(selected);
            prompt.hint = Some("Enter edit/select  Esc back".to_string());
            self.active_prompt = Some(prompt);
            self.redraw(term)?;
            let (code, modifiers) = self.panel_key(term)?;
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(());
            }
            if self.navigate_prompt(code, false, term)? {
                selected = self.active_prompt.as_ref().unwrap().selected;
                continue;
            }
            match code {
                KeyCode::Esc => return Ok(()),
                KeyCode::Enter => {
                    let selected = self.active_prompt.as_ref().unwrap().selected;
                    match selected {
                        0 => {
                            if let Some(value) =
                                self.edit_value("label", &entry.label, false, term)?
                            {
                                entry.label = value;
                            }
                        }
                        1 => {
                            if let Some(value) =
                                self.edit_value("model", &entry.model, false, term)?
                            {
                                entry.model = value;
                            }
                        }
                        2 => {
                            if let Some(value) =
                                self.edit_value("base_url", &entry.base_url, false, term)?
                            {
                                entry.base_url = value;
                            }
                        }
                        3 => {
                            if let Some(value) =
                                self.edit_value("endpoint", &entry.endpoint, false, term)?
                            {
                                entry.endpoint = value;
                            }
                        }
                        4 => {
                            entry.handler = if entry.handler == "codex" {
                                "openai".to_string()
                            } else {
                                "codex".to_string()
                            };
                        }
                        5 => {
                            if let Some(value) = self.edit_value("api_key", "", true, term)? {
                                entry.api_key = value;
                            }
                        }
                        6 => {
                            self.custom_configs
                                .set_provider_entry("providers", &id, &entry);
                            let reply = if self.llm.id() == id {
                                match providers::create_provider_with_config(
                                    &id,
                                    &self.custom_configs.derive_providers_config(),
                                ) {
                                    Ok(provider) => {
                                        self.provider =
                                            format!("{} ({})", provider.name(), provider.id());
                                        self.model = provider.model().to_string();
                                        self.llm = provider;
                                        format!("Saved and reloaded provider {id}.")
                                    }
                                    Err(err) => format!(
                                        "Saved provider {id}, but active reload failed: {err}"
                                    ),
                                }
                            } else {
                                format!("Saved provider {id}.")
                            };
                            self.show_reply(reply, term)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    async fn handle_tools_command(&mut self, arg: &str, term: &mut BoneTerminal) -> io::Result<()> {
        let mut parts = arg.split_whitespace();
        let action = parts.next().unwrap_or("");
        match action {
            "reload" => {
                // Full reload: re-boot extensions and rebuild tool handler.
                let config_dir = crate::config::bone_dir();
                let cwd = std::env::current_dir().unwrap_or_default();
                let booted = crate::ext::boot_with_tools(
                    &config_dir,
                    &cwd,
                    &mut self.custom_configs,
                    true,
                    crate::ext::BootOptions::default(),
                );
                self.extensions = booted.manager;
                self.tools = booted.tools;

                self.user_config.enabled_tools = self.tools.enabled_names();
                let count = self.tools.definitions().len();
                self.show_reply(
                    format!("Tools and Lua extensions reloaded. {count} tools enabled."),
                    term,
                )
            }
            _ => self.config_picker(term, Some("tools")).await,
        }
    }

    async fn config_picker(
        &mut self,
        term: &mut BoneTerminal,
        start_tab: Option<&str>,
    ) -> io::Result<()> {
        let mut custom = config::custom::CustomConfigs::load();

        let mut tabs: Vec<String> = Vec::new();
        let mut namespaces: Vec<String> = Vec::new();
        for ns in ["general", "providers", "tools"] {
            if let Some((_, page)) = custom.pages.iter().find(|(page_ns, _)| page_ns == ns) {
                tabs.push(page.title.clone());
                namespaces.push(ns.to_string());
            }
        }
        for (ns, page) in &custom.pages {
            if namespaces.iter().any(|existing| existing == ns) {
                continue;
            }
            tabs.push(page.title.clone());
            namespaces.push(ns.clone());
        }
        let providers_tab_idx = namespaces
            .iter()
            .position(|ns| ns == "providers")
            .unwrap_or(0);
        let num_tabs = tabs.len();

        let mut active = if let Some(tab) = start_tab {
            if tab == "providers" {
                providers_tab_idx
            } else {
                namespaces.iter().position(|ns| ns == tab).unwrap_or(0)
            }
        } else {
            0
        };
        let mut selected = 0usize;

        loop {
            let options = if active == providers_tab_idx {
                // Providers tab: list providers like the old provider_picker
                let providers_config = custom.derive_providers_config();
                let mut ids: Vec<String> = providers_config.providers.keys().cloned().collect();
                ids.sort();
                ids.iter()
                    .map(|id| {
                        let entry = &providers_config.providers[id];
                        let active_marker = if id == self.llm.id() { "●" } else { "○" };
                        let kind = if entry.handler.is_empty() {
                            "openai"
                        } else {
                            entry.handler.as_str()
                        };
                        format!(
                            "{active_marker} {id} · {} · {} · {kind}",
                            entry.model, entry.label
                        )
                    })
                    .collect()
            } else if active < namespaces.len() {
                let ns = &namespaces[active];
                let page_idx = custom
                    .pages
                    .iter()
                    .position(|(page_ns, _)| page_ns == ns)
                    .unwrap();
                let page = &custom.pages[page_idx].1;
                page.fields
                    .iter()
                    .map(|field| {
                        let label = field.label.as_deref().unwrap_or(&field.key);
                        let value = custom.get_value(ns, &field.key);
                        let display = match field.field_type {
                            config::custom::ConfigFieldType::Bool => {
                                if value == "true" {
                                    "●".to_string()
                                } else {
                                    "○".to_string()
                                }
                            }
                            _ => value,
                        };
                        if matches!(field.field_type, config::custom::ConfigFieldType::Bool) {
                            format!("{display} {label}")
                        } else {
                            format!("{:<30} {}", label, display)
                        }
                    })
                    .collect()
            } else {
                vec![]
            };

            let mut prompt = Prompt::new(tabs[active].clone(), options);
            prompt.set_selected(selected);
            prompt.tabs = tabs.clone();
            prompt.active_tab = active;
            let hint = if active == providers_tab_idx {
                "Enter select  e edit  Esc close".to_string()
            } else {
                "Tab switch  Enter edit/cycle  Esc close".to_string()
            };
            prompt.hint = Some(hint);
            self.active_prompt = Some(prompt);
            self.redraw(term)?;

            let (code, modifiers) = self.panel_key(term)?;
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                return self.close_panel(term);
            }
            if self.navigate_prompt(code, false, term)? {
                selected = self.active_prompt.as_ref().unwrap().selected;
                continue;
            }

            match code {
                KeyCode::Esc => return self.close_panel(term),
                KeyCode::Tab => {
                    active = (active + 1) % num_tabs;
                    selected = 0;
                    continue;
                }
                KeyCode::BackTab => {
                    active = if active == 0 {
                        num_tabs - 1
                    } else {
                        active - 1
                    };
                    selected = 0;
                    continue;
                }
                KeyCode::Enter => {
                    if active == providers_tab_idx {
                        // Providers tab: select provider
                        let providers_config = custom.derive_providers_config();
                        let mut ids: Vec<String> =
                            providers_config.providers.keys().cloned().collect();
                        ids.sort();
                        let Some(id) = ids.get(self.active_prompt.as_ref().unwrap().selected)
                        else {
                            continue;
                        };
                        let id = id.clone();
                        let reply =
                            match providers::create_provider_with_config(&id, &providers_config) {
                                Ok(new_provider) => match new_provider.validate().await {
                                    Ok(()) => {
                                        self.provider = format!(
                                            "{} ({})",
                                            new_provider.name(),
                                            new_provider.id()
                                        );
                                        self.model = new_provider.model().to_string();
                                        self.llm = new_provider;
                                        custom.set_last_provider(&id);
                                        self.custom_configs = custom.clone();
                                        format!("Switched to {} ({})", self.model, self.provider)
                                    }
                                    Err(err) => format!("Provider validation failed: {err}"),
                                },
                                Err(err) => err.to_string(),
                            };
                        self.close_panel(term)?;
                        return self.show_reply(reply, term);
                    }
                    if active >= namespaces.len() {
                        continue;
                    }
                    let ns = namespaces[active].clone();
                    let page_idx = custom
                        .pages
                        .iter()
                        .position(|(page_ns, _)| page_ns == &ns)
                        .unwrap();
                    let page = &custom.pages[page_idx].1;
                    let idx = self.active_prompt.as_ref().unwrap().selected;

                    if idx >= page.fields.len() {
                        continue;
                    }
                    let field = page.fields[idx].clone();
                    let current = custom.get_value(&ns, &field.key);
                    match field.field_type {
                        config::custom::ConfigFieldType::Bool
                        | config::custom::ConfigFieldType::Enum => {
                            if let Some(next) = custom.cycle_field(&ns, &field.key, &current) {
                                custom.set_value(&ns, &field.key, next.clone());
                                self.apply_custom_configs_to_runtime(custom.clone());
                                if ns == "tools" {
                                    self.tools.set_enabled(&field.key, next == "true");
                                }
                            }
                        }
                        _ => {
                            let label = field.label.as_deref().unwrap_or(&field.key).to_string();
                            if let Some(val) = self.edit_value(&label, &current, false, term)? {
                                custom.set_value(&ns, &field.key, val.trim().to_string());
                                self.apply_custom_configs_to_runtime(custom.clone());
                            }
                        }
                    }
                }
                KeyCode::Char('e') | KeyCode::Char('E') if active == providers_tab_idx => {
                    let providers_config = custom.derive_providers_config();
                    let mut ids: Vec<String> = providers_config.providers.keys().cloned().collect();
                    ids.sort();
                    if let Some(id) = ids.get(self.active_prompt.as_ref().unwrap().selected) {
                        self.provider_editor(id.clone(), term)?;
                        custom = config::custom::CustomConfigs::load();
                    }
                }
                _ => {}
            }
        }
    }

    fn open_stats_dashboard(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        if std::env::var_os("TMUX").is_some()
            && std::process::Command::new("tmux")
                .arg("display-message")
                .arg("-p")
                .arg("ok")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        {
            let exe = std::env::current_exe()?;
            let cmd = format!("{} stats-popup", shell_quote(&exe.to_string_lossy()));
            let result = std::process::Command::new("tmux")
                .arg("display-popup")
                .arg("-E")
                .arg("-w")
                .arg("96%")
                .arg("-h")
                .arg("92%")
                .arg(cmd)
                .status();
            self.force_redraw(term)?;
            if result.is_ok_and(|s| s.success()) {
                return Ok(());
            }
        }

        let Some(ref db) = self.session_db else {
            return self.show_reply("Stats database is not available.".to_string(), term);
        };

        let result = crate::ui::stats::run(|| {
            db.usage_stats_snapshot()
                .map_err(|err| io::Error::other(err.to_string()))
        });

        self.force_redraw(term)?;
        if let Err(err) = result {
            return self.show_reply(format!("Stats dashboard failed: {err}"), term);
        }
        Ok(())
    }

    async fn open_editor(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        Renderer::prepare_exit(term)?;
        Renderer::shutdown_terminal()?;
        let tmp = std::env::temp_dir().join("bone-edit.txt");
        std::fs::write(&tmp, "")?;
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "nano".to_string());
        let _ = tokio::process::Command::new(&editor)
            .arg(&tmp)
            .spawn()?
            .wait()
            .await;
        let text = std::fs::read_to_string(&tmp)?;
        std::fs::remove_file(&tmp).ok();
        let text = text.trim_end_matches(['\r', '\n']).to_string();
        if !text.trim().is_empty() {
            self.input.buffer = text;
            self.input.cursor_pos = self.input.buffer.chars().count();
        }

        *term = Renderer::init_terminal(MIN_ROWS)?;
        self.renderer.viewport_height = MIN_ROWS;
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.force_redraw(term)
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Match a Lua key string (e.g. "<C-p>", "<S-Tab>") against a KeyCode + modifiers.
fn key_matches(key_str: &str, code: KeyCode, modifiers: KeyModifiers) -> bool {
    let key_str = key_str.trim();
    let mut expected_mods = KeyModifiers::NONE;
    let mut key_part = key_str;

    if key_str.starts_with('<') && key_str.ends_with('>') {
        key_part = &key_str[1..key_str.len() - 1];
        let parts: Vec<&str> = key_part.split('-').collect();
        for part in &parts {
            match *part {
                "C" | "Ctrl" => expected_mods |= KeyModifiers::CONTROL,
                "S" | "Shift" => expected_mods |= KeyModifiers::SHIFT,
                "A" | "Alt" => expected_mods |= KeyModifiers::ALT,
                _ => {}
            }
        }
        key_part = parts.last().copied().unwrap_or(&key_part);
    }

    if modifiers != expected_mods {
        return false;
    }

    match key_part {
        "Tab" => code == KeyCode::Tab,
        "BackTab" | "Backtab" => code == KeyCode::BackTab,
        "Enter" => code == KeyCode::Enter,
        "Esc" | "Escape" => code == KeyCode::Esc,
        "Space" => code == KeyCode::Char(' '),
        "Backspace" => code == KeyCode::Backspace,
        "Delete" => code == KeyCode::Delete,
        "Insert" => code == KeyCode::Insert,
        "Home" => code == KeyCode::Home,
        "End" => code == KeyCode::End,
        "PageUp" => code == KeyCode::PageUp,
        "PageDown" => code == KeyCode::PageDown,
        "Up" => code == KeyCode::Up,
        "Down" => code == KeyCode::Down,
        "Left" => code == KeyCode::Left,
        "Right" => code == KeyCode::Right,
        "F1" | "F2" | "F3" | "F4" | "F5" | "F6" | "F7" | "F8" | "F9" | "F10" | "F11" | "F12" => {
            if let Some(n) = key_part[1..].parse::<u8>().ok() {
                code == KeyCode::F(n)
            } else {
                false
            }
        }
        _ if key_part.len() == 1 => {
            if let Some(ch) = key_part.chars().next() {
                code == KeyCode::Char(ch)
            } else {
                false
            }
        }
        _ => false,
    }
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
}
