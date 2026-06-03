pub mod stream;

use crate::chat::{COMPACT_NOTICE, DEFAULT_KEEP_MESSAGES, Message, compact_transcript};
use crate::config::{self, ProvidersConfig, UserConfig};
use crate::llm::{ChatMessage, LlmProvider, TokenStats, format_tokens, providers};
use crate::skills::SkillStore;
use crate::skills::types::Skill;
use crate::tools::script_runner::{ScriptRequest, run_script};
use crate::tools::{ApprovalMode, ToolCall, ToolHandler};
use crate::session_db::SessionDb;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::collections::VecDeque;
use std::io;
use std::time::Instant;
use tokio::time::Duration;

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
    pub providers_config: ProvidersConfig,
    pub user_config: UserConfig,
    pub custom_configs: config::custom::CustomConfigs,
    pub queue: VecDeque<String>,
    pub tools: ToolHandler,
    pub skills: SkillStore,
    pub approval_mode: ApprovalMode,
    pub active_prompt: Option<Prompt>,
    /// Set to `true` to abort the current streaming response.
    pub cancel_streaming: bool,
    /// Timestamp of the last Ctrl+C press (for double-tap quit).
    pub last_ctrl_c: Option<Instant>,
    /// Cumulative token usage stats.
    pub token_stats: TokenStats,
    /// Cached set of dynamic tool names that use interaction: select.
    interaction_tools: std::collections::HashSet<String>,
    /// Active pane pages displayed between input and status bar.
    pub pages: Vec<PanePage>,
    /// Index of the currently visible pane page.
    pub active_page: usize,
    /// Whether pane pages are shown in the bottom pane.
    pub panes_visible: bool,
    /// Map from dynamic tool name to its script (shown in approval prompt).
    dynamic_scripts: std::collections::HashMap<String, String>,
    /// SQLite session database for conversation persistence and usage tracking.
    session_db: Option<SessionDb>,
    /// Current conversation ID in the session database.
    conversation_id: Option<i64>,
    /// Message sequence counter for DB ordering.
    session_seq: i64,
    /// Last measured output tokens/sec from the most recent stream.
    last_tokens_per_sec: Option<f64>,
}

impl App {
    pub fn new(
        llm: Box<dyn LlmProvider>,
        providers_config: ProvidersConfig,
        user_config: UserConfig,
        custom_configs: config::custom::CustomConfigs,
    ) -> io::Result<Self> {
        let provider = format!("{} ({})", llm.name(), llm.id());
        let model = llm.model().to_string();
        let approval_mode = user_config.approval_mode;
        let loaded = crate::tools::load_tools();
        // Sync tools page with registry, then rebuild enabled list
        let all_tool_names: Vec<String> = loaded
            .registry
            .definitions()
            .iter()
            .map(|d| d.name.clone())
            .collect();
        let mut custom_configs = custom_configs;
        custom_configs.sync_tools_from_registry(&all_tool_names);
        let enabled = custom_configs.enabled_tool_names();
        let enabled = if enabled.is_empty() {
            all_tool_names
        } else {
            enabled
        };
        let tools = ToolHandler::with_enabled_safety_and_display(
            loaded.registry,
            &enabled,
            loaded.dynamic_safety,
            loaded.dynamic_display,
        );
        let skills = SkillStore::load()?;
        let mut messages = vec![Message::system(
            "bone v0.1.0 — type /help for commands. Ctrl+C twice to quit.",
        )];
        for warning in skills.warnings() {
            messages.push(Message::system(format!("skill warning: {warning}")));
        }

        Ok(Self {
            messages,
            transcript: Vec::new(),
            input: InputState::default(),
            streaming: false,
            provider,
            model,
            llm,
            should_quit: false,
            renderer: Renderer::new(),
            providers_config,
            user_config,
            custom_configs,
            queue: VecDeque::new(),
            tools,
            skills,
            approval_mode,
            active_prompt: None,
            cancel_streaming: false,
            last_ctrl_c: None,
            token_stats: TokenStats::new(),
            pages: Vec::new(),
            active_page: 0,
            panes_visible: true,
            interaction_tools: loaded.interaction_tools,
            dynamic_scripts: loaded.dynamic_scripts,
            session_db: None,
            conversation_id: None,
            session_seq: 0,
            last_tokens_per_sec: None,
        })
    }
    /// Initialize or open the session database.
    fn init_session_db(&mut self) -> Option<String> {
        if self.session_db.is_some() {
            return None;
        }
        let db_path = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".bone-rust")
            .join("data")
            .join("conversations.db");
        match SessionDb::open(&db_path) {
            Ok(db) => {
                match db.create_conversation(&self.llm.id(), self.llm.model()) {
                    Ok(conv_id) => {
                        self.conversation_id = Some(conv_id);
                        self.session_db = Some(db);
                        None
                    }
                    Err(err) => Some(format!("warning: failed to create conversation: {err}")),
                }
            }
            Err(err) => Some(format!("warning: failed to open session database: {err}")),
        }
    }
    /// Append an assistant message to the session database.
    pub(crate) fn append_assistant_to_db(&mut self, content: &str, tool_calls_json: Option<&str>) {
        if let Some(ref db) = self.session_db {
            if let Some(conv_id) = self.conversation_id {
                self.session_seq += 1;
                db.append_message(conv_id, "assistant", content, None, None, tool_calls_json, self.session_seq).ok();
            }
        }
    }

    /// Append a tool result to the session database.
    pub(crate) fn append_tool_result_to_db(&mut self, name: &str, call_id: &str, content: &str) {
        if let Some(ref db) = self.session_db {
            if let Some(conv_id) = self.conversation_id {
                self.session_seq += 1;
                db.append_message(conv_id, "tool", content, Some(name), Some(call_id), None, self.session_seq).ok();
            }
        }
    }
    /// Start a new conversation in the database (used by /clear, /new).
    fn start_new_conversation(&mut self) {
        if let Some(ref db) = self.session_db {
            if let Some(conv_id) = self.conversation_id {
                db.end_conversation(conv_id).ok();
            }
            match db.create_conversation(&self.llm.id(), self.llm.model()) {
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
        self.renderer.render_banner(term, &self.provider, &self.model)?;
        self.renderer.flush_new_to_scrollback(&self.messages, term)?;
        self.cancel_streaming = false;
        self.redraw(term)?;
        Ok(())
    }

    fn persist_runtime_config(&mut self) {
        let mode = match self.user_config.approval_mode {
            crate::tools::ApprovalMode::Danger => "danger",
            crate::tools::ApprovalMode::Edits => "edit",
            crate::tools::ApprovalMode::Safe => "safe",
        };
        self.custom_configs
            .set_value("general", "approval_mode", mode.to_string());
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

    pub(crate) fn status_info(&self) -> StatusInfo {
        self.stream_status_info_with_tokens(None, self.last_tokens_per_sec)
    }

    /// Build a [`StatusInfo`] for the streaming spinner wait, with an optional
    /// live cumulative output-token estimate.
    fn stream_status_info_with_tokens(&self, estimated_tokens: Option<u64>, tokens_per_sec: Option<f64>) -> StatusInfo {
        stream_status_info_with_token_stats(
            estimated_tokens,
            tokens_per_sec,
            &self.model,
            &self.token_stats,
            self.streaming,
            self.approval_mode,
            self.queue.len(),
            self.user_config.show_token_metrics,
        )
    }
}

/// Build a [`StatusInfo`] with a live streaming cumulative output-token estimate.
pub(crate) fn stream_status_info_with_token_stats(
    streaming_completion_tokens: Option<u64>,
    tokens_per_sec: Option<f64>,
    model: &str,
    token_stats: &crate::llm::TokenStats,
    streaming: bool,
    approval_mode: crate::tools::ApprovalMode,
    queue_len: usize,
    show_token_metrics: bool,
) -> StatusInfo {
    StatusInfo {
        model: model.to_string(),
        token_stats: token_stats.clone(),
        streaming_completion_tokens,
        tokens_per_sec,
        streaming,
        approval_mode,
        queue_len,
        show_token_metrics,
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
                pane_toggle_hint: self.pane_toggle_hint(),
            },
            self.active_prompt.as_ref(),
        );
    }

    fn visible_pages(&self) -> &[PanePage] {
        if self.panes_visible { &self.pages } else { &[] }
    }

    fn pane_toggle_hint(&self) -> Option<&'static str> {
        if self.pages.is_empty() {
            None
        } else if self.panes_visible {
            Some("Ctrl+T hide panel  ──  Ctrl+↑↓")
        } else {
            Some("Ctrl+T show panel  ──  Ctrl+↑↓")
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

        match self.input.apply_key(code, modifiers) {
            InputAction::Cancel => self.handle_ctrl_c(term),
            InputAction::Submit => {
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
            InputAction::Redraw | InputAction::Escape => self.redraw(term),
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

    /// Handle Ctrl+C: cancel streaming response, or quit on double-tap.
    fn handle_ctrl_c(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let now = Instant::now();
        let double_tap = self
            .last_ctrl_c
            .is_some_and(|prev| now.duration_since(prev) < Duration::from_secs(1));

        if double_tap {
            // Best-effort end conversation in DB
            if let Some(ref db) = self.session_db {
                if let Some(conv_id) = self.conversation_id {
                    db.end_conversation(conv_id).ok();
                }
            }
            self.should_quit = true;
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
            self.dynamic_scripts.get(&call.name).cloned()
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
        if cmd == "skills" {
            return self.handle_skills_command(&arg, term);
        }
        if cmd == "compact" {
            return self.compact_chat(term);
        }
        if !matches!(
            cmd.as_str(),
            "help"
                | "clear"
                | "new"
                | "context"
                | "model"
                | "provider"
                | "quit"
                | "exit"
                | "edit"
                | "e"
                | "usage"
                | "recall"
        ) {
            if let Err(err) = self.skills.reload() {
                return self.show_reply(format!("Failed to refresh skills: {err}"), term);
            }
            if let Some(skill) = self.skills.get_enabled(&cmd).cloned() {
                return self.invoke_skill(skill, &arg, term).await;
            }
        }

        if matches!(cmd.as_str(), "clear" | "new") {
            return self.clear_chat(term);
        }
        if cmd == "usage" {
            let mut reply = self.token_stats.summary();
            // Tool schema token estimate
            let defs = self.tools.definitions();
            let schema_json = serde_json::to_string(&defs).unwrap_or_default();
            let schema_chars = schema_json.len();
            let schema_tokens = (schema_chars as f64 / 3.8).ceil() as u64;
            reply.push_str(&format!(
                "\n  Tools:     {} tools, ~{} tokens ({} chars)",
                defs.len(),
                crate::llm::format_tokens(schema_tokens),
                crate::llm::format_tokens(schema_chars as u64)
            ));

            // System prompt token estimate
            let sys = crate::llm::prompts::system_prompt();
            let sys_chars = sys.len();
            let sys_tokens = (sys_chars as f64 / 3.8).ceil() as u64;
            reply.push_str(&format!(
                "\n  Sys prompt: ~{} tokens ({} chars)",
                crate::llm::format_tokens(sys_tokens),
                crate::llm::format_tokens(sys_chars as u64)
            ));
            if let Some(ref db) = self.session_db {
                if let Some(conv_id) = self.conversation_id {
                    if let Ok(by_provider) = db.usage_by_provider(conv_id) {
                        if by_provider.len() > 1 {
                            reply.push_str("\n\nBy provider/model");
                            for p in &by_provider {
                                reply.push_str(&format!(
                                    "\n  {} / {}\t{} in / {} out",
                                    p.provider, p.model,
                                    crate::llm::format_tokens(p.prompt_tokens as u64),
                                    crate::llm::format_tokens(p.completion_tokens as u64),
                                ));
                                if p.cached_tokens > 0 {
                                    reply.push_str(&format!(" / {} cached", crate::llm::format_tokens(p.cached_tokens as u64)));
                                }
                                if p.cost > 0.0 {
                                    reply.push_str(&format!(" / ${:.4}", p.cost));
                                }
                            }
                        }
                    }
                }
            }
            return self.show_reply(reply, term);
        }

        if cmd == "recall" {
            let query = arg.trim();
            if query.is_empty() {
                return self.show_reply("Usage: /recall <query>", term);
            }
            let reply = if let Some(ref db) = self.session_db {
                match db.search(query, 5) {
                    Ok(hits) => {
                        if hits.is_empty() {
                            format!("No results for \"{query}\".")
                        } else {
                            let mut lines = vec![format!("Recall results for \"{query}\"")];
                            for hit in &hits {
                                lines.push(format!(
                                    "  {} {}: {}",
                                    hit.created_at, hit.role, hit.snippet
                                ));
                            }
                            lines.join("\n")
                        }
                    }
                    Err(err) => format!("Search error: {err}"),
                }
            } else {
                "Session database not available.".to_string()
            };
            return self.show_reply(reply, term);
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
            &mut self.providers_config,
        )
        .await?;

        let provider_changed = self.llm.id() != prev_provider;
        let model_changed = self.llm.model() != prev_model;
        if provider_changed || model_changed {
            self.start_new_conversation();
        }

        match result {
            commands::CommandResult::Quit => {
                // Best-effort end conversation in DB
                if let Some(ref db) = self.session_db {
                    if let Some(conv_id) = self.conversation_id {
                        db.end_conversation(conv_id).ok();
                    }
                }
                self.should_quit = true;
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

    fn handle_skills_command(&mut self, arg: &str, term: &mut BoneTerminal) -> io::Result<()> {
        let mut parts = arg.split_whitespace();
        let action = parts.next().unwrap_or("list");
        if action != "reload"
            && let Err(err) = self.skills.reload()
        {
            return self.show_reply(format!("Failed to refresh skills: {err}"), term);
        }
        let reply = match action {
            "list" => {
                let mut lines = vec!["Skills:".to_string()];
                lines.extend(self.skills.list().map(|skill| {
                    let status = if skill.enabled { "enabled" } else { "disabled" };
                    format!("  /{} [{status}] — {}", skill.name, skill.description)
                }));
                if lines.len() == 1 {
                    lines.push("  (none)".to_string());
                }
                lines.join("\n")
            }
            "enable" | "disable" => match parts.next() {
                Some(name) => {
                    let enabled = action == "enable";
                    match self.skills.set_enabled(name, enabled) {
                        Ok(()) => format!(
                            "Skill /{name} {}.",
                            if enabled { "enabled" } else { "disabled" }
                        ),
                        Err(err) => err,
                    }
                }
                None => format!("Usage: /skills {action} <name>"),
            },
            "reload" => match self.skills.reload() {
                Ok(()) => {
                    let mut lines = vec!["Skills reloaded.".to_string()];
                    lines.extend(
                        self.skills
                            .warnings()
                            .iter()
                            .map(|warning| format!("warning: {warning}")),
                    );
                    lines.join("\n")
                }
                Err(err) => format!("Failed to reload skills: {err}"),
            },
            _ => "Usage: /skills [list|enable <name>|disable <name>|reload]".to_string(),
        };
        self.show_reply(reply, term)
    }

    async fn invoke_skill(
        &mut self,
        skill: Skill,
        args: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let script_output = if let Some(script) = skill.script.as_ref() {
            let approval_call = ToolCall {
                id: format!("skill:{}", skill.name),
                name: "shell".to_string(),
                arguments: serde_json::json!({
                    "command": script,
                    "classification": "danger",
                    "display_label": format!("skill: /{}", skill.name),
                }),
            };
            if !self.approval_mode.allows_call(&approval_call) {
                match self.prompt_and_wait(&approval_call, term)? {
                    Decision::Accept => {}
                    Decision::Cancel => {
                        return self.show_reply(
                            format!("Skill /{} cancelled; script was not executed.", skill.name),
                            term,
                        );
                    }
                    Decision::Advise(advice) => {
                        let suffix = if advice.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" Advice: {}", advice.trim())
                        };
                        return self.show_reply(
                            format!("Skill /{} not executed.{suffix}", skill.name),
                            term,
                        );
                    }
                }
            }
            let output = match run_script(ScriptRequest {
                command: script.clone(),
                env: vec![("BONE_ARGS".to_string(), args.to_string())],
                timeout_ms: 120_000,
            })
            .await
            {
                Ok(output) => output,
                Err(err) => {
                    return self.show_reply(format!("Skill /{} failed: {err}", skill.name), term);
                }
            };
            if output.exit_code != Some(0) {
                let detail = if output.stderr.is_empty() {
                    output.stdout
                } else {
                    output.stderr
                };
                return self.show_reply(
                    format!(
                        "Skill /{} failed (exit code {}).\n{}",
                        skill.name,
                        output
                            .exit_code
                            .map_or_else(|| "signal".to_string(), |code| code.to_string()),
                        detail
                    ),
                    term,
                );
            }
            Some(output.stdout)
        } else {
            None
        };

        match crate::skills::render_skill(&skill, args, script_output.as_deref()) {
            Ok(rendered) => {
                let display = if args.trim().is_empty() {
                    format!("/{}\n[skill input submitted]", skill.name)
                } else {
                    format!("/{} {}\n[skill input submitted]", skill.name, args)
                };
                self.submit_user_turn(rendered, Some(display), term).await
            }
            Err(err) => self.show_reply(err, term),
        }
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

    pub(crate) fn compact_transcript_state(&mut self) -> (bool, u64) {
        let keep = self
            .user_config
            .auto_compact_keep_messages
            .unwrap_or(DEFAULT_KEEP_MESSAGES);
        let before = self.token_stats.context_length;
        match compact_transcript(&self.transcript, keep) {
            std::borrow::Cow::Owned(owned) => {
                self.transcript = owned;
                let history = crate::chat::build_chat_history(&self.transcript);
                let tools = self.tools.definitions();
                let prompt_chars = Self::estimate_context_chars(&history, &tools);
                self.token_stats.set_context_estimate(prompt_chars);
                let after = self.token_stats.context_length;
                (true, before.saturating_sub(after))
            }
            std::borrow::Cow::Borrowed(_) => (false, 0),
        }
    }

    fn compacted_message(&self, prefix: &str, saved: u64) -> String {
        if saved > 0 {
            format!(
                "{prefix}. Saved ~{} tokens (was ~{}, now {}).",
                format_tokens(saved),
                format_tokens(saved + self.token_stats.context_length),
                format_tokens(self.token_stats.context_length)
            )
        } else {
            COMPACT_NOTICE.to_string()
        }
    }

    pub(crate) fn compact_chat(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let (compacted, saved) = self.compact_transcript_state();
        let msg = if compacted {
            self.compacted_message("Compacted older messages", saved)
        } else {
            "Chat history is already compact.".to_string()
        };
        self.messages.push(Message::system(msg));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)
    }

    /// Auto-compact transcript if the token threshold is exceeded.
    pub(crate) fn auto_compact_if_needed(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let should_compact = self
            .user_config
            .auto_compact_tokens
            .is_some_and(|limit| limit > 0 && self.token_stats.context_length >= limit);

        if should_compact {
            let (compacted, saved) = self.compact_transcript_state();
            if compacted {
                self.messages.push(Message::system(
                    self.compacted_message("Auto-compacted", saved),
                ));
            }
            self.renderer
                .flush_new_to_scrollback(&self.messages, term)?;
            self.redraw(term)?;
        }
        Ok(())
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
        let mut entry = self.providers_config.providers[&id].clone();
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
                    let edited = match selected {
                        0 => self.edit_value("label", &entry.label, false, term)?,
                        1 => self.edit_value("model", &entry.model, false, term)?,
                        2 => self.edit_value("base_url", &entry.base_url, false, term)?,
                        3 => self.edit_value("endpoint", &entry.endpoint, false, term)?,
                        4 => {
                            entry.handler = if entry.handler == "codex" {
                                "openai".to_string()
                            } else {
                                "codex".to_string()
                            };
                            None
                        }
                        5 => self.edit_value("api_key", "", true, term)?,
                        6 => {
                            self.providers_config
                                .providers
                                .insert(id.clone(), entry.clone());
                            config::save_providers(&self.providers_config);
                            let reply = if self.llm.id() == id {
                                match providers::create_provider_with_config(
                                    &id,
                                    &self.providers_config,
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
                        _ => None,
                    };
                    if let Some(value) = edited {
                        match selected {
                            0 => entry.label = value,
                            1 => entry.model = value,
                            2 => entry.base_url = value,
                            3 => entry.endpoint = value,
                            5 => entry.api_key = value,
                            _ => {}
                        }
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
                let loaded = crate::tools::load_tools();
                let all_names: Vec<String> = loaded
                    .registry
                    .definitions()
                    .iter()
                    .map(|d| d.name.clone())
                    .collect();
                self.custom_configs.sync_tools_from_registry(&all_names);
                let enabled = self.custom_configs.enabled_tool_names();
                let enabled = if enabled.is_empty() {
                    all_names
                } else {
                    enabled
                };
                self.tools = crate::tools::ToolHandler::with_enabled_safety_and_display(
                    loaded.registry,
                    &enabled,
                    loaded.dynamic_safety,
                    loaded.dynamic_display,
                );
                self.interaction_tools = loaded.interaction_tools;
                self.dynamic_scripts = loaded.dynamic_scripts;
                self.user_config.enabled_tools = self.tools.enabled_names();
                let count = self.tools.definitions().len();
                self.show_reply(format!("Tools reloaded. {count} tools enabled."), term)
            }
            _ => self.config_picker(term, Some("tools")).await,
        }
    }

    fn provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.providers_config.providers.keys().cloned().collect();
        ids.sort();
        ids
    }

    async fn config_picker(
        &mut self,
        term: &mut BoneTerminal,
        start_tab: Option<&str>,
    ) -> io::Result<()> {
        let mut custom = config::custom::CustomConfigs::load();

        let mut tabs: Vec<String> = Vec::new();
        let mut namespaces: Vec<String> = Vec::new();
        for (ns, page) in &custom.pages {
            tabs.push(page.title.clone());
            namespaces.push(ns.clone());
        }
        // Add Providers tab (not backed by a page file)
        let providers_tab_idx = tabs.len();
        tabs.push("Providers".to_string());
        namespaces.push("__providers__".to_string());
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
                let ids = self.provider_ids();
                ids.iter()
                    .map(|id| {
                        let entry = &self.providers_config.providers[id];
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
                let page = &custom.pages[active].1;
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
                        if ns == "tools"
                            && matches!(field.field_type, config::custom::ConfigFieldType::Bool)
                        {
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
                        let ids = self.provider_ids();
                        let Some(id) = ids.get(self.active_prompt.as_ref().unwrap().selected)
                        else {
                            continue;
                        };
                        let id = id.clone();
                        let reply = match providers::create_provider_with_config(
                            &id,
                            &self.providers_config,
                        ) {
                            Ok(new_provider) => match new_provider.validate().await {
                                Ok(()) => {
                                    self.provider =
                                        format!("{} ({})", new_provider.name(), new_provider.id());
                                    self.model = new_provider.model().to_string();
                                    self.llm = new_provider;
                                    self.providers_config.last_provider = id.clone();
                                    config::save_providers(&self.providers_config);
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
                    let page = &custom.pages[active].1;
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
                    let ids = self.provider_ids();
                    if let Some(id) = ids.get(self.active_prompt.as_ref().unwrap().selected) {
                        self.provider_editor(id.clone(), term)?;
                    }
                }
                _ => {}
            }
        }
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

    /// Rebuild a merged pane page from all state entries for a given source.
    /// Currently only handles the "subagents" source.
    fn rebuild_merged_pane(&self, source: &str) -> Option<PanePage> {
        let entries = self.tools.state_map.get_all(source)?;
        if entries.is_empty() {
            return None;
        }

        match source {
            "subagents" => Some(rebuild_subagents_pane(entries)),
            _ => None,
        }
    }
}
/// Rebuild the merged subagents pane from all agent state entries.
fn rebuild_subagents_pane(entries: &std::collections::HashMap<String, String>) -> PanePage {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    #[derive(serde::Deserialize)]
    struct AgentState {
        mode: String,
        model: String,
        title: String,
        sent: u64,
        received: u64,
        done: bool,
        started: f64,
    }

    let mut agents: Vec<(String, AgentState)> = Vec::new();
    for (key, raw) in entries {
        if let Ok(state) = serde_json::from_str::<AgentState>(raw) {
            agents.push((key.clone(), state));
        }
    }
    agents.sort_by(|a, b| {
        a.1.started
            .partial_cmp(&b.1.started)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let running = agents.iter().filter(|(_, a)| !a.done).count();
    let title = if agents.is_empty() {
        "Subagents".to_string()
    } else {
        format!("Subagents ({running})")
    };

    let visible = 8;

    if agents.is_empty() {
        return PanePage {
            source: "subagents".to_string(),
            title,
            content: Vec::new(),
            visible_rows: visible,
            scroll: 0,
        };
    }

    /// Format token counts. Mirrors the Python `fmt_tokens` in defaults/tools/subagent.yaml;
    /// keep both in sync when changing the format.
    fn fmt_tokens(sent: u64, received: u64) -> String {
        let total = sent + received;
        if total >= 1_000_000 {
            format!("{:.1}M", total as f64 / 1_000_000.0)
        } else if total >= 1000 {
            format!("{:.1}k", total as f64 / 1000.0)
        } else {
            total.to_string()
        }
    }

    let mode_width = agents
        .iter()
        .map(|(_, a)| a.mode.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let model_width = agents
        .iter()
        .map(|(_, a)| a.model.chars().count())
        .max()
        .unwrap_or(5)
        .max(5);
    let token_strs: Vec<String> = agents
        .iter()
        .map(|(_, a)| fmt_tokens(a.sent, a.received))
        .collect();
    let token_width = token_strs
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(6)
        .max(6);

    // Header line
    let header = format!(
        "    {:<mode_w$}  {:<model_w$}  {:<token_w$}  TASK",
        "MODE",
        "MODEL",
        "TOKENS",
        mode_w = mode_width,
        model_w = model_width,
        token_w = token_width
    );
    let mut lines = vec![Line::from(header)];

    for ((_, agent), tokens) in agents.iter().zip(token_strs.iter()) {
        let status_char = if agent.done { "\u{2713}" } else { "\u{25cb}" };
        let row = format!(
            "  {} {:<mode_w$}  {:<model_w$}  {:<token_w$}  {}",
            status_char,
            agent.mode,
            agent.model,
            tokens,
            agent.title,
            mode_w = mode_width,
            model_w = model_width,
            token_w = token_width
        );
        if agent.done {
            lines.push(Line::from(Span::styled(
                row,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
        } else {
            lines.push(Line::from(row));
        }
    }

    let scroll = if lines.len() > visible {
        lines.len() - visible
    } else {
        0
    };

    PanePage {
        source: "subagents".to_string(),
        title,
        content: lines,
        visible_rows: visible,
        scroll,
    }
}
