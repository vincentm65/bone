pub mod stream;

use crate::chat::Message;
use crate::config::{self, ProvidersConfig, UserConfig};
use crate::llm::{ChatMessage, LlmProvider, TokenStats, providers};
use crate::tools::{ApprovalMode, ToolCall, ToolHandler, builtin_tools};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::collections::VecDeque;
use std::io;
use std::time::Instant;
use tokio::time::Duration;

use super::commands;
use super::input::{InputAction, InputState};
use super::prompt::{Decision, Prompt};
use super::render::{BoneTerminal, MIN_ROWS, Renderer, StatusInfo};

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
}

impl App {
    pub fn new(
        llm: Box<dyn LlmProvider>,
        providers_config: ProvidersConfig,
        user_config: UserConfig,
    ) -> io::Result<Self> {
        let provider = format!("{} ({})", llm.name(), llm.id());
        let model = llm.model().to_string();
        let approval_mode = user_config.approval_mode;
        let tools = ToolHandler::with_enabled(builtin_tools(), &user_config.enabled_tools);

        Ok(Self {
            messages: vec![Message::system(
                "bone v0.1.0 — type /help for commands. Ctrl+C twice to quit.",
            )],
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
            queue: VecDeque::new(),
            tools,
            approval_mode,
            active_prompt: None,
            cancel_streaming: false,
            last_ctrl_c: None,
            token_stats: TokenStats::new(),
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        let mut terminal = Renderer::init_terminal(MIN_ROWS)?;

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
        let desired = Renderer::desired_height(&self.input, self.active_prompt.as_ref(), width);

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
        self.stream_status_info_with_tokens(None)
    }

    /// Build a [`StatusInfo`] for the streaming spinner wait, with an optional
    /// live cumulative output-token estimate.
    fn stream_status_info_with_tokens(&self, estimated_tokens: Option<u64>) -> StatusInfo {
        stream_status_info_with_token_stats(
            estimated_tokens,
            &self.model,
            &self.token_stats,
            self.streaming,
            self.approval_mode,
            self.queue.len(),
        )
    }
}

/// Build a [`StatusInfo`] with a live streaming cumulative output-token estimate.
pub(crate) fn stream_status_info_with_token_stats(
    streaming_completion_tokens: Option<u64>,
    model: &str,
    token_stats: &crate::llm::TokenStats,
    streaming: bool,
    approval_mode: crate::tools::ApprovalMode,
    queue_len: usize,
) -> StatusInfo {
    StatusInfo {
        model: model.to_string(),
        token_stats: token_stats.clone(),
        streaming_completion_tokens,
        streaming,
        approval_mode,
        queue_len,
    }
}

impl App {
    fn draw(&self, frame: &mut ratatui::Frame) {
        self.renderer.draw_bottom_pane(
            frame,
            &self.input,
            &self.status_info(),
            self.active_prompt.as_ref(),
        );
    }

    async fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
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
                config::save_user_config(&self.user_config);
                self.redraw(term)
            }
            InputAction::Redraw | InputAction::Escape => self.redraw(term),
            InputAction::OpenEditor => self.open_editor(term).await,
            InputAction::None => Ok(()),
        }
    }

    /// Handle a keypress while a blocking prompt is displayed.
    /// Up/Down move the cursor, Enter confirms, Esc rejects.
    fn handle_prompt_key(&mut self, code: KeyCode, term: &mut BoneTerminal) -> io::Result<()> {
        match code {
            KeyCode::Up => {
                if let Some(ref mut p) = self.active_prompt {
                    p.up();
                }
                self.redraw(term)?;
            }
            KeyCode::Down => {
                if let Some(ref mut p) = self.active_prompt {
                    p.down();
                }
                self.redraw(term)?;
            }
            KeyCode::PageUp => {
                if let Some(ref mut p) = self.active_prompt {
                    p.page_up();
                }
                self.redraw(term)?;
            }
            KeyCode::PageDown => {
                if let Some(ref mut p) = self.active_prompt {
                    p.page_down();
                }
                self.redraw(term)?;
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                if let Some(ref mut p) = self.active_prompt {
                    p.toggle_peek();
                    self.redraw(term)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Handle Ctrl+C: cancel streaming response, or quit on double-tap.
    fn handle_ctrl_c(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let now = Instant::now();
        let double_tap = self
            .last_ctrl_c
            .is_some_and(|prev| now.duration_since(prev) < Duration::from_secs(1));

        if double_tap {
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

        let prompt = if call.name == "shell" {
            let full_command = call.arguments["command"].as_str().map(String::from);
            let title = call.arguments["command"]
                .as_str()
                .unwrap_or("?")
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect::<String>();
            Prompt {
                title: format!("{} — {}", call.name, title),
                options: vec![
                    "Accept".to_string(),
                    "Advise".to_string(),
                    "Cancel".to_string(),
                ],
                selected: 0,
                scroll: 0,
                visible_rows: 10,
                hint: None,
                full_command,
                peek_mode: false,
            }
        } else {
            Prompt::new(
                format!("{} — {}", call.name, summary),
                vec!["Accept", "Advise", "Cancel"],
            )
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
                    KeyCode::Up => {
                        if let Some(ref mut p) = self.active_prompt {
                            p.up();
                        }
                        self.redraw(term)?;
                    }
                    KeyCode::Down => {
                        if let Some(ref mut p) = self.active_prompt {
                            p.down();
                        }
                        self.redraw(term)?;
                    }
                    KeyCode::PageUp => {
                        if let Some(ref mut p) = self.active_prompt {
                            p.page_up();
                        }
                        self.redraw(term)?;
                    }
                    KeyCode::PageDown => {
                        if let Some(ref mut p) = self.active_prompt {
                            p.page_down();
                        }
                        self.redraw(term)?;
                    }
                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        if let Some(ref mut p) = self.active_prompt {
                            p.toggle_peek();
                            self.redraw(term)?;
                        }
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
            return self.provider_picker(term).await;
        }
        if cmd == "tools" {
            return self.tools_picker(term);
        }
        if cmd == "config" {
            return self.config_picker(term);
        }

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

        match result {
            commands::CommandResult::Quit => {
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

    fn navigate_panel(&mut self, code: KeyCode, term: &mut BoneTerminal) -> io::Result<bool> {
        match code {
            KeyCode::Up => self.active_prompt.as_mut().unwrap().up(),
            KeyCode::Down => self.active_prompt.as_mut().unwrap().down(),
            KeyCode::PageUp => self.active_prompt.as_mut().unwrap().page_up(),
            KeyCode::PageDown => self.active_prompt.as_mut().unwrap().page_down(),
            _ => return Ok(false),
        }
        self.redraw(term)?;
        Ok(true)
    }

    async fn provider_picker(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let mut selected = 0usize;
        loop {
            let mut ids: Vec<String> = self.providers_config.providers.keys().cloned().collect();
            ids.sort();
            let options = ids
                .iter()
                .map(|id| {
                    let entry = &self.providers_config.providers[id];
                    let active = if id == self.llm.id() { "*" } else { " " };
                    format!("[{active}] {id}  {} ({})", entry.label, entry.model)
                })
                .collect::<Vec<_>>();
            let mut prompt = Prompt::new("Providers", options);
            prompt.selected = selected.min(ids.len().saturating_sub(1));
            prompt.scroll = prompt.selected.saturating_sub(prompt.visible_rows - 1);
            prompt.hint = Some("Enter select  e edit  Esc close".to_string());
            self.active_prompt = Some(prompt);
            self.redraw(term)?;

            let (code, modifiers) = self.panel_key(term)?;
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                return self.close_panel(term);
            }
            match code {
                code if self.navigate_panel(code, term)? => {
                    selected = self.active_prompt.as_ref().unwrap().selected;
                    continue;
                }
                KeyCode::Esc => return self.close_panel(term),
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    if let Some(id) = ids.get(self.active_prompt.as_ref().unwrap().selected) {
                        self.provider_editor(id.clone(), term)?;
                    }
                }
                KeyCode::Enter => {
                    let Some(id) = ids.get(self.active_prompt.as_ref().unwrap().selected) else {
                        return self.close_panel(term);
                    };
                    let reply =
                        match providers::create_provider_with_config(id, &self.providers_config) {
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
                _ => {}
            }
        }
    }

    fn mask_secret(value: &str) -> String {
        if value.is_empty() {
            "(empty)".to_string()
        } else {
            "*".repeat(value.chars().count().min(12).max(4))
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
                format!("label: {}", entry.label),
                format!("model: {}", entry.model),
                format!("base_url: {}", entry.base_url),
                format!("endpoint: {}", entry.endpoint),
                format!("handler: {}", entry.handler),
                format!("api_key: {}", Self::mask_secret(&entry.api_key)),
                "Save".to_string(),
            ];
            let mut prompt = Prompt::new(format!("Edit provider: {id}"), options);
            prompt.selected = selected;
            prompt.hint = Some("Enter edit/select  Esc back".to_string());
            self.active_prompt = Some(prompt);
            self.redraw(term)?;
            let (code, modifiers) = self.panel_key(term)?;
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(());
            }
            if self.navigate_panel(code, term)? {
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

    fn tools_picker(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let names = config::default_enabled_tools();
        let mut selected = 0usize;
        loop {
            let options = names
                .iter()
                .map(|name| {
                    let mark = if self.tools.is_enabled(name) {
                        "x"
                    } else {
                        " "
                    };
                    format!("[{mark}] {name}")
                })
                .collect();
            let mut prompt = Prompt::new("Tools", options);
            prompt.selected = selected;
            prompt.hint = Some("Space/Enter toggle  Esc close".to_string());
            self.active_prompt = Some(prompt);
            self.redraw(term)?;
            let (code, modifiers) = self.panel_key(term)?;
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                return self.close_panel(term);
            }
            if self.navigate_panel(code, term)? {
                selected = self.active_prompt.as_ref().unwrap().selected;
                continue;
            }
            match code {
                KeyCode::Esc => return self.close_panel(term),
                KeyCode::Char(' ') | KeyCode::Enter => {
                    let selected = self.active_prompt.as_ref().unwrap().selected;
                    if let Some(name) = names.get(selected) {
                        self.tools.set_enabled(name, !self.tools.is_enabled(name));
                        self.user_config.enabled_tools = self.tools.enabled_names();
                        config::save_user_config(&self.user_config);
                    }
                }
                _ => {}
            }
        }
    }

    fn config_picker(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let modes = [
            ApprovalMode::Safe,
            ApprovalMode::Edits,
            ApprovalMode::Danger,
        ];
        let mut selected = 0usize;
        loop {
            let options = modes
                .iter()
                .map(|mode| {
                    let active = if *mode == self.approval_mode {
                        "*"
                    } else {
                        " "
                    };
                    format!("[{active}] Approval mode: {}", mode.label())
                })
                .collect();
            let mut prompt = Prompt::new("Config", options);
            prompt.selected = selected;
            prompt.hint = Some("Enter choose  Esc close".to_string());
            self.active_prompt = Some(prompt);
            self.redraw(term)?;
            let (code, modifiers) = self.panel_key(term)?;
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                return self.close_panel(term);
            }
            if self.navigate_panel(code, term)? {
                selected = self.active_prompt.as_ref().unwrap().selected;
                continue;
            }
            match code {
                KeyCode::Esc => return self.close_panel(term),
                KeyCode::Enter => {
                    let selected = self.active_prompt.as_ref().unwrap().selected;
                    if let Some(mode) = modes.get(selected) {
                        self.approval_mode = *mode;
                        self.user_config.approval_mode = *mode;
                        config::save_user_config(&self.user_config);
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
}
