use crate::chat::{Context, Message, build_chat_history};
use crate::config::ProvidersConfig;
use crate::llm::{ChatEvent, ChatMessage, LlmProvider};
use crate::tools::{ApprovalMode, ToolCall, ToolHandler, ToolResult, builtin_tools};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{StreamExt, pin_mut};
use std::io;
use std::time::Instant;
use tokio::time::{self, Duration};

use super::commands;
use super::input::InputState;
use super::prompt::{Decision, Prompt};
use super::render::{BOTTOM_ROWS, BoneTerminal, Renderer, StatusInfo};

pub struct App {
    pub messages: Vec<Message>,
    pub input: InputState,
    pub streaming: bool,
    pub provider: String,
    pub model: String,
    pub llm: Box<dyn LlmProvider>,
    pub should_quit: bool,
    pub context: Context,
    pub renderer: Renderer,
    pub providers_config: ProvidersConfig,
    pub queue: Vec<String>,
    pub tools: ToolHandler,
    pub approval_mode: ApprovalMode,
    pub active_prompt: Option<Prompt>,
    /// Set to `true` to abort the current streaming response.
    pub cancel_streaming: bool,
    /// Timestamp of the last Ctrl+C press (for double-tap quit).
    pub last_ctrl_c: Option<Instant>,
}

impl App {
    pub fn new(
        llm: Box<dyn LlmProvider>,
        context_window: usize,
        response_budget: usize,
        providers_config: ProvidersConfig,
    ) -> io::Result<Self> {
        let provider = format!("{} ({})", llm.name(), llm.id());
        let model = llm.model().to_string();

        Ok(Self {
            messages: vec![Message::system(
                "bone v0.1.0 — type /help for commands. Ctrl+C twice to quit.",
            )],
            input: InputState::default(),
            streaming: false,
            provider,
            model,
            llm,
            should_quit: false,
            context: Context::new(context_window).with_response_budget(response_budget),
            renderer: Renderer::new(),
            providers_config,
            queue: Vec::new(),
            tools: ToolHandler::new(builtin_tools()),
            approval_mode: ApprovalMode::default(),
            active_prompt: None,
            cancel_streaming: false,
            last_ctrl_c: None,
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        let mut terminal = Renderer::init_terminal()?;

        // Startup banner
        self.renderer
            .render_banner(&mut terminal, &self.provider, &self.model)?;

        // Push initial system message into scrollback
        self.renderer
            .flush_new_to_scrollback(&self.messages, &mut terminal)?;

        // Initial draw of the bottom pane
        terminal.draw(|frame| {
            self.renderer
                .draw_bottom_pane(frame, &self.input, &self.status_info(), None);
        })?;

        // Main event loop
        while !self.should_quit {
            if event::poll(std::time::Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.handle_key(key.code, key.modifiers, &mut terminal)
                    .await?;
            }
        }

        Renderer::shutdown_terminal()?;
        Ok(())
    }

    fn status_info(&self) -> StatusInfo {
        StatusInfo {
            provider: self.provider.clone(),
            model: self.model.clone(),
            msg_count: self.messages.len(),
            streaming: self.streaming,
            queue_len: self.queue.len(),
            approval_mode: self.approval_mode,
        }
    }

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
        // ── Ctrl+C: cancel stream or double-tap quit ──
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return self.handle_ctrl_c(term);
        }

        // If a blocking prompt is active, only prompt keys are handled.
        if self.active_prompt.is_some() {
            return self.handle_prompt_key(code, term);
        }

        // Ctrl shortcuts take priority.
        if modifiers.contains(KeyModifiers::CONTROL) {
            match code {
                KeyCode::Char('a') => {
                    self.input.cursor_to_start();
                    term.draw(|frame| self.draw(frame))?;
                    return Ok(());
                }
                KeyCode::Char('e') => {
                    self.input.cursor_to_end();
                    term.draw(|frame| self.draw(frame))?;
                    return Ok(());
                }
                KeyCode::Char('w') => {
                    self.input.delete_word_backward();
                    term.draw(|frame| self.draw(frame))?;
                    return Ok(());
                }
                KeyCode::Char('u') => {
                    self.input.clear_buffer();
                    term.draw(|frame| self.draw(frame))?;
                    return Ok(());
                }
                KeyCode::Char('d') => {
                    self.queue.clear();
                    term.draw(|frame| self.draw(frame))?;
                    return Ok(());
                }
                _ => {}
            }
        }

        match code {
            KeyCode::BackTab => {
                self.approval_mode = self.approval_mode.cycle();
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            KeyCode::Enter => self.send_message(term).await,
            KeyCode::Char(c) => {
                self.input.insert_char(c);
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            KeyCode::Backspace => {
                self.input.delete_backward();
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            KeyCode::Left => {
                if self.input.cursor_pos > 0 {
                    self.input.cursor_pos -= 1;
                }
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            KeyCode::Right => {
                if self.input.cursor_pos < self.input.buffer.chars().count() {
                    self.input.cursor_pos += 1;
                }
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            KeyCode::Home => {
                self.input.cursor_to_start();
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            KeyCode::End => {
                self.input.cursor_to_end();
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            KeyCode::Up => {
                self.input.history_up();
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            KeyCode::Down => {
                self.input.history_down();
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            KeyCode::Esc => {
                self.input.clear_buffer();
                term.draw(|frame| self.draw(frame))?;
                Ok(())
            }
            _ => Ok(()),
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
                term.draw(|frame| self.draw(frame))?;
            }
            KeyCode::Down => {
                if let Some(ref mut p) = self.active_prompt {
                    p.down();
                }
                term.draw(|frame| self.draw(frame))?;
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

        term.draw(|frame| self.draw(frame))?;
        Ok(())
    }

    /// Show a blocking prompt for a tool call that needs approval.
    /// Resizes the viewport, waits for the user to pick, then restores.
    fn prompt_and_wait(
        &mut self,
        call: &ToolCall,
        term: &mut BoneTerminal,
    ) -> io::Result<Decision> {
        let summary = match call.name.as_str() {
            "read_file" | "write_file" | "edit_file" => {
                call.arguments["path"].as_str().unwrap_or("?").to_string()
            }
            "bash" => call.arguments["command"]
                .as_str()
                .unwrap_or("?")
                .to_string(),
            _ => call.name.clone(),
        };

        self.active_prompt = Some(Prompt::new(
            format!("{} — {}", call.name, summary),
            vec!["Accept", "Advise", "Cancel"],
        ));

        let height = BOTTOM_ROWS - 1 + self.active_prompt.as_ref().unwrap().height();
        Renderer::resize_viewport(term, height)?;
        term.draw(|frame| self.draw(frame))?;

        // ── Mini blocking event loop ──
        let decision = loop {
            if event::poll(std::time::Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Up => {
                            if let Some(ref mut p) = self.active_prompt {
                                p.up();
                            }
                            term.draw(|frame| self.draw(frame))?;
                        }
                        KeyCode::Down => {
                            if let Some(ref mut p) = self.active_prompt {
                                p.down();
                            }
                            term.draw(|frame| self.draw(frame))?;
                        }
                        KeyCode::Enter => {
                            break self.active_prompt.as_ref().unwrap().decision();
                        }
                        KeyCode::Esc => {
                            break Decision::Cancel;
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break Decision::Cancel;
                        }
                        _ => {}
                    }
                }
            }
        };

        // Dismiss prompt and restore viewport.
        self.active_prompt = None;
        Renderer::resize_viewport(term, BOTTOM_ROWS)?;
        term.draw(|frame| self.draw(frame))?;

        Ok(decision)
    }

    /// Process queued key events while streaming. Enter queues a message,
    /// Ctrl+D clears the queue. All other input edits work normally.
    fn process_keys_while_streaming(&mut self) {
        Self::drain_keys(
            &mut self.input,
            &mut self.queue,
            &mut self.approval_mode,
            &mut self.cancel_streaming,
        );
    }

    async fn send_message(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let text = self.input.buffer.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        self.input.reset();

        // Slash commands are handled locally, not sent to the LLM.
        if text.starts_with('/') {
            return self.handle_command(&text, term).await;
        }

        self.messages.push(Message::user(&text));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;

        let mut history = build_chat_history(&self.messages, &self.context);

        self.streaming = true;
        self.messages.push(Message::assistant(String::new()));
        self.renderer.streaming_lines_flushed = 0;
        self.renderer.scrollback_cursor += 1;
        let assistant_idx = self.messages.len() - 1;
        term.draw(|frame| self.draw(frame))?;

        for _ in 0..8 {
            if self.cancel_streaming {
                self.messages[assistant_idx]
                    .content
                    .push_str("\n[cancelled]");
                break;
            }

            let (stream_result, spinner_tick) = self.wait_for_stream(history.clone(), term).await;
            self.renderer.spinner_tick = spinner_tick;

            if self.cancel_streaming {
                self.messages[assistant_idx]
                    .content
                    .push_str("\n[cancelled]");
                break;
            }

            let mut tool_calls = Vec::new();
            match stream_result {
                Ok(mut stream) => {
                    let mut spinner = time::interval(Duration::from_millis(90));
                    loop {
                        if self.cancel_streaming {
                            self.messages[assistant_idx]
                                .content
                                .push_str("\n[cancelled]");
                            break;
                        }
                        tokio::select! {
                            chunk = stream.next() => match chunk {
                                Some(Ok(ChatEvent::TextDelta(text))) => {
                                    self.messages[assistant_idx].content.push_str(&text);
                                    self.renderer.redraw_streaming_message(
                                        &self.messages[assistant_idx].content,
                                        term,
                                        &self.input,
                                        &self.status_info(),
                                    )?;
                                }
                                Some(Ok(ChatEvent::ToolCall(call))) => tool_calls.push(call),
                                Some(Err(err)) => {
                                    self.messages[assistant_idx].content.push_str(&format!("\n[stream error: {err}]"));
                                    break;
                                }
                                None => break,
                            },
                            _ = spinner.tick() => {
                                self.process_keys_while_streaming();
                                if self.cancel_streaming {
                                    self.messages[assistant_idx].content.push_str("\n[cancelled]");
                                    break;
                                }
                                self.renderer.tick_spinner(term, &self.input, &self.status_info())?;
                            }
                        }
                    }
                }
                Err(err) => {
                    if self.cancel_streaming {
                        self.messages[assistant_idx]
                            .content
                            .push_str("\n[cancelled]");
                    } else {
                        self.messages[assistant_idx].content = format!(
                            "[provider error: {err}]\n\nIs llama.cpp server running at http://127.0.0.1:8080?"
                        );
                    }
                    break;
                }
            }

            history.push(ChatMessage::assistant_with_tools(
                self.messages[assistant_idx].content.clone(),
                tool_calls.clone(),
            ));

            if tool_calls.is_empty() || self.cancel_streaming {
                break;
            }

            // Per-call approval.  Calls auto-approved by the current mode
            // are collected; the rest block with an interactive prompt.
            let calls_for_display = tool_calls.clone();
            let mut was_rejected = vec![false; tool_calls.len()];
            let mut approved_calls = Vec::new();

            for (i, call) in tool_calls.into_iter().enumerate() {
                if self.approval_mode.allows_call(&call) {
                    approved_calls.push(call);
                } else {
                    match self.prompt_and_wait(&calls_for_display[i], term)? {
                        Decision::Accept => approved_calls.push(call),
                        Decision::Cancel => was_rejected[i] = true,
                        Decision::Advise => {
                            // Treat as accept but flag for advisory feedback.
                            approved_calls.push(call);
                        }
                    }
                }
            }

            // Execute approved calls.
            let exec_results = if !approved_calls.is_empty() {
                self.tools.execute_all(approved_calls).await
            } else {
                Vec::new()
            };

            // Merge rejected + executed results in original order.
            let mut exec_iter = exec_results.into_iter();
            let results: Vec<ToolResult> = (0..calls_for_display.len())
                .map(|i| {
                    if was_rejected[i] {
                        ToolResult {
                            call_id: calls_for_display[i].id.clone(),
                            name: calls_for_display[i].name.clone(),
                            content: "rejected by user".into(),
                            is_error: true,
                        }
                    } else {
                        exec_iter.next().unwrap()
                    }
                })
                .collect();

            for (call, result) in calls_for_display.iter().zip(results.iter()) {
                self.messages.push(build_tool_row(call, result));
            }
            self.renderer
                .flush_new_to_scrollback(&self.messages, term)?;

            for result in results {
                history.push(ChatMessage::tool(result));
            }
        }

        self.streaming = false;
        self.cancel_streaming = false;
        self.renderer
            .finalize_streaming_message(&self.messages[assistant_idx].content, term)?;
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        term.draw(|frame| self.draw(frame))?;

        // Drain queued messages — send each one.
        while let Some(queued) = self.queue.first().cloned() {
            self.queue.remove(0);
            self.input.buffer = queued;
            self.input.cursor_pos = self.input.buffer.chars().count();
            Box::pin(self.send_message(term)).await?;
        }

        Ok(())
    }

    async fn handle_command(&mut self, input: &str, term: &mut BoneTerminal) -> io::Result<()> {
        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        let cmd = parts[0].to_string();
        let arg = parts.get(1).copied().unwrap_or("").to_string();

        let result = commands::handle(
            &cmd,
            &arg,
            &mut self.messages,
            &mut self.renderer,
            term,
            &self.context,
            &mut self.llm,
            &mut self.provider,
            &mut self.model,
            &self.providers_config,
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
                term.draw(|frame| self.draw(frame))?;
            }
        }
        Ok(())
    }

    async fn wait_for_stream(
        &mut self,
        history: Vec<crate::llm::ChatMessage>,
        term: &mut BoneTerminal,
    ) -> (
        Result<crate::llm::ResponseStream, crate::llm::LlmError>,
        usize,
    ) {
        let request = self.llm.chat_stream(history, self.tools.definitions());
        let spinner = time::sleep(Duration::from_millis(90));
        let tick = self.renderer.spinner_tick;
        let provider = self.provider.clone();
        let model = self.model.clone();
        let msg_count = self.messages.len();
        let input = &mut self.input;
        let queue = &mut self.queue;
        let renderer = &mut self.renderer;
        let approval_mode = &mut self.approval_mode;
        let cancel = &mut self.cancel_streaming;
        pin_mut!(request, spinner);

        loop {
            if *cancel {
                return (Err(crate::llm::LlmError::new("cancelled")), tick);
            }
            tokio::select! {
                result = &mut request => return (result, tick),
                _ = &mut spinner => {
                    renderer.spinner_tick = renderer.spinner_tick.wrapping_add(1);
                    Self::drain_keys(input, queue, approval_mode, cancel);
                    term.draw(|frame| {
                        renderer.draw_bottom_pane_with_tick(frame, input, &StatusInfo {
                            queue_len: queue.len(),
                            provider: provider.clone(),
                            model: model.clone(),
                            msg_count,
                            streaming: true,
                            approval_mode: *approval_mode,
                        }, renderer.spinner_tick, None);
                    }).ok();
                    spinner.as_mut().reset(time::Instant::now() + Duration::from_millis(90));
                }
            }
        }
    }

    /// Drain pending key events into input edits or queue. Used during streaming.
    fn drain_keys(
        input: &mut InputState,
        queue: &mut Vec<String>,
        mode: &mut ApprovalMode,
        cancel: &mut bool,
    ) {
        while event::poll(std::time::Duration::from_millis(0)).unwrap_or(false) {
            if let Event::Key(key) = event::read().unwrap_or(Event::Key(
                crossterm::event::KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
            )) {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let mods = key.modifiers;
                match key.code {
                    KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => {
                        *cancel = true;
                        queue.clear();
                        return;
                    }
                    KeyCode::BackTab => {
                        *mode = mode.cycle();
                    }
                    KeyCode::Enter => {
                        let text = input.buffer.trim().to_string();
                        if !text.is_empty() {
                            queue.push(text);
                            input.reset();
                        }
                    }
                    KeyCode::Char('a') if mods.contains(KeyModifiers::CONTROL) => {
                        input.cursor_to_start()
                    }
                    KeyCode::Char('e') if mods.contains(KeyModifiers::CONTROL) => {
                        input.cursor_to_end()
                    }
                    KeyCode::Char('w') if mods.contains(KeyModifiers::CONTROL) => {
                        input.delete_word_backward()
                    }
                    KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
                        input.clear_buffer()
                    }
                    KeyCode::Char('d') if mods.contains(KeyModifiers::CONTROL) => queue.clear(),
                    KeyCode::Char(c) => input.insert_char(c),
                    KeyCode::Backspace => input.delete_backward(),
                    KeyCode::Left => {
                        if input.cursor_pos > 0 {
                            input.cursor_pos -= 1;
                        }
                    }
                    KeyCode::Right => {
                        if input.cursor_pos < input.buffer.chars().count() {
                            input.cursor_pos += 1;
                        }
                    }
                    KeyCode::Home => input.cursor_to_start(),
                    KeyCode::End => input.cursor_to_end(),
                    _ => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tool display helpers.
// ---------------------------------------------------------------------------

fn build_tool_row(call: &ToolCall, result: &ToolResult) -> Message {
    Message::tool_row(tool_label(call), result.is_error)
}

fn tool_label(call: &ToolCall) -> String {
    let target = match call.name.as_str() {
        "read_file" | "write_file" | "edit_file" => call.arguments["path"].as_str(),
        "bash" => call.arguments["command"].as_str(),
        _ => None,
    };

    match target {
        Some(target) if !target.is_empty() => format!("{} {}", call.name, target),
        _ => call.name.clone(),
    }
}
