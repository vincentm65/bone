use crate::chat::{Message, build_chat_history};
use crate::config::ProvidersConfig;
use crate::llm::{ChatEvent, ChatMessage, ChatRole, LlmProvider, TokenStats};
use crate::tools::{ApprovalMode, ToolCall, ToolHandler, ToolResult, builtin_tools};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{StreamExt, pin_mut};
use std::collections::VecDeque;
use std::io;
use std::time::Instant;
use tokio::time::{self, Duration};

use super::commands;
use super::input::InputState;
use super::prompt::{Decision, Prompt};
use super::render::{BoneTerminal, Renderer, StatusInfo};
use super::tool_display::build_tool_row;
use crate::tools::edit_file::preview_edit_file;
use ratatui::widgets::Clear;

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
    pub fn new(llm: Box<dyn LlmProvider>, providers_config: ProvidersConfig) -> io::Result<Self> {
        let provider = format!("{} ({})", llm.name(), llm.id());
        let model = llm.model().to_string();

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
            queue: VecDeque::new(),
            tools: ToolHandler::new(builtin_tools()),
            approval_mode: ApprovalMode::default(),
            active_prompt: None,
            cancel_streaming: false,
            last_ctrl_c: None,
            token_stats: TokenStats::new(),
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        let mut terminal = Renderer::init_terminal()?;

        self.renderer
            .flush_new_to_scrollback(&self.messages, &mut terminal)?;
        self.renderer
            .render_banner(&mut terminal, &self.provider, &self.model)?;
        self.force_redraw(&mut terminal)?;

        // Main event loop
        while !self.should_quit {
            if event::poll(std::time::Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.handle_key(key.code, key.modifiers, &mut terminal)
                            .await?;
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

        Renderer::prepare_exit(&mut terminal)?;
        Renderer::shutdown_terminal()?;
        Ok(())
    }

    /// Redraw from scratch, updating the tracked terminal size.
    /// Used after resize or stale-size detection.
    fn force_redraw(&mut self, terminal: &mut BoneTerminal) -> io::Result<()> {
        terminal.draw(|frame| {
            frame.render_widget(Clear, frame.area());
            self.renderer.draw_bottom_pane(
                frame,
                &self.input,
                &self.status_info(),
                self.active_prompt.as_ref(),
            );
        })?;
        self.renderer.last_size = Some(crossterm::terminal::size()?);
        Ok(())
    }

    fn redraw(&mut self, terminal: &mut BoneTerminal) -> io::Result<()> {
        terminal.draw(|frame| self.draw(frame))?;
        Ok(())
    }

    fn status_info(&self) -> StatusInfo {
        StatusInfo {
            model: self.model.clone(),
            token_stats: self.token_stats.clone(),
            streaming: self.streaming,
            approval_mode: self.approval_mode,
            queue_len: self.queue.len(),
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
                    self.redraw(term)?;
                    return Ok(());
                }
                KeyCode::Char('e') => {
                    self.input.cursor_to_end();
                    self.redraw(term)?;
                    return Ok(());
                }
                KeyCode::Char('w') => {
                    self.input.delete_word_backward();
                    self.redraw(term)?;
                    return Ok(());
                }
                KeyCode::Char('u') => {
                    self.input.clear_buffer();
                    self.redraw(term)?;
                    return Ok(());
                }
                KeyCode::Char('d') => {
                    self.queue.clear();
                    self.redraw(term)?;
                    return Ok(());
                }
                _ => {}
            }
        }

        match code {
            KeyCode::BackTab => {
                self.approval_mode = self.approval_mode.cycle();
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Enter => {
                self.send_message(term).await?;
                // Drain queued messages — send each one without recursion.
                while let Some(queued) = self.queue.pop_front() {
                    self.input.buffer = queued;
                    self.input.cursor_pos = self.input.buffer.chars().count();
                    self.send_message(term).await?;
                }
                Ok(())
            }
            KeyCode::Char(c) => {
                self.input.insert_char(c);
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Backspace => {
                self.input.delete_backward();
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Left if self.input.cursor_pos > 0 => {
                self.input.cursor_pos -= 1;
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Left => {
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Right if self.input.cursor_pos < self.input.buffer.chars().count() => {
                self.input.cursor_pos += 1;
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Right => {
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Home => {
                self.input.cursor_to_start();
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::End => {
                self.input.cursor_to_end();
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Up => {
                self.input.history_up();
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Down => {
                self.input.history_down();
                self.redraw(term)?;
                Ok(())
            }
            KeyCode::Esc => {
                self.input.clear_buffer();
                self.redraw(term)?;
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
                self.redraw(term)?;
            }
            KeyCode::Down => {
                if let Some(ref mut p) = self.active_prompt {
                    p.down();
                }
                self.redraw(term)?;
            }
            KeyCode::Enter | KeyCode::Esc => {
                // Enter/Esc on a prompt in the main loop should dismiss it.
                // Currently unreachable — prompts use a blocking mini-event-loop
                // in prompt_and_wait — but guard against future non-blocking paths.
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

        self.redraw(term)?;

        // ── Mini blocking event loop ──
        let decision = loop {
            if event::poll(std::time::Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
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
                    KeyCode::Enter => {
                        if let Some(prompt) = self.active_prompt.as_ref() {
                            break prompt.decision();
                        }
                        break Decision::Cancel;
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
        };

        // Dismiss prompt and restore viewport.
        self.active_prompt = None;
        self.redraw(term)?;

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

        // Slash commands are handled locally, not sent to the LLM.
        if text.starts_with('/') {
            self.input.reset();
            self.redraw(term)?;
            return self.handle_command(&text, term).await;
        }

        self.messages.push(Message::user(&text));
        self.transcript
            .push(ChatMessage::new(ChatRole::User, &text));

        // Flush the submitted message before resetting the input.
        self.renderer.flush_new_to_scrollback(
            &self.messages,
            term,
        )?;
        self.input.reset();
        self.redraw(term)?;

        let mut history = build_chat_history(&self.transcript);

        self.streaming = true;
        self.messages.push(Message::assistant(String::new()));
        self.renderer.streaming_lines_flushed = 0;
        self.renderer.scrollback_cursor += 1;
        let assistant_idx = self.messages.len() - 1;
        self.redraw(term)?;

        let mut rounds = 0u32;
        loop {
            rounds += 1;
            if rounds > 64 {
                self.messages[assistant_idx]
                    .content
                    .push_str("\n[tool-call round limit reached]");
                break;
            }
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
                    let mut had_usage = false;
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
                                Some(Ok(ChatEvent::TokenUsage { prompt_tokens, completion_tokens })) => {
                                    self.token_stats.record_request(prompt_tokens, completion_tokens);
                                    had_usage = true;
                                }
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

                    // Fallback: if the provider didn't report real token
                    // usage, estimate from character counts so the status
                    // bar still shows something useful.
                    if !had_usage && !self.cancel_streaming {
                        let prompt_chars: usize = history.iter().map(|m| m.content.len()).sum();
                        let completion_chars = self.messages[assistant_idx].content.len();
                        self.token_stats
                            .record_estimate(prompt_chars, completion_chars);
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

            if tool_calls.is_empty() || self.cancel_streaming {
                self.transcript.push(ChatMessage::new(
                    ChatRole::Assistant,
                    self.messages[assistant_idx].content.clone(),
                ));
                break;
            }

            let assistant = ChatMessage::assistant_with_tools(
                self.messages[assistant_idx].content.clone(),
                tool_calls.clone(),
            );
            history.push(assistant.clone());
            self.transcript.push(assistant);

            // Reset the display slot so the next round starts fresh.
            // Without this, each round appends to the accumulated content,
            // causing earlier rounds' text to be duplicated in transcript
            // and sent back to the LLM on the next request.
            self.messages[assistant_idx].content.clear();
            self.renderer.streaming_lines_flushed = 0;

            // Per-call approval.  Calls auto-approved by the current mode
            // are collected; the rest block with an interactive prompt.
            let calls_for_display = tool_calls.clone();
            let mut was_rejected = vec![false; tool_calls.len()];
            let mut advised = vec![false; tool_calls.len()];
            let mut approved_calls = Vec::new();

            for (i, mut call) in tool_calls.into_iter().enumerate() {
                if call.name == "edit_file" {
                    match preview_edit_file(call.arguments.clone()).await {
                        Ok(preview) => {
                            call.arguments["expected_hash"] =
                                serde_json::Value::String(preview.before_hash);
                            self.messages.push(Message::system(preview.diff));
                            self.renderer
                                .flush_new_to_scrollback(&self.messages, term)?;
                        }
                        Err(err) => {
                            self.messages.push(Message::system(format!(
                                "edit_file preview failed for {}: {err}",
                                call.arguments["path"].as_str().unwrap_or("?")
                            )));
                            self.renderer
                                .flush_new_to_scrollback(&self.messages, term)?;
                            was_rejected[i] = true;
                            continue;
                        }
                    }
                }

                if self.approval_mode.allows_call(&call) {
                    approved_calls.push(call);
                } else {
                    match self.prompt_and_wait(&calls_for_display[i], term)? {
                        Decision::Accept => approved_calls.push(call),
                        Decision::Cancel => was_rejected[i] = true,
                        Decision::Advise => {
                            advised[i] = true;
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
                        let mut result = exec_iter.next().unwrap_or_else(|| {
                            ToolResult {
                                call_id: calls_for_display[i].id.clone(),
                                name: calls_for_display[i].name.clone(),
                                content: "internal error: tool result missing".into(),
                                is_error: true,
                            }
                        });
                        if advised[i] {
                            result.content.push_str(
                                "\n\nUser selected Advise: proceed carefully, verify assumptions, and explain the outcome.",
                            );
                        }
                        result
                    }
                })
                .collect();

            for (call, result) in calls_for_display.iter().zip(results.iter()) {
                self.messages.push(build_tool_row(call, result));
            }
            self.renderer
                .flush_new_to_scrollback(&self.messages, term)?;

            for result in results {
                let message = ChatMessage::tool(result);
                history.push(message.clone());
                self.transcript.push(message);
            }
        }

        self.streaming = false;
        self.cancel_streaming = false;
        self.last_ctrl_c = None;
        self.renderer
            .finalize_streaming_message(&self.messages[assistant_idx].content, term)?;
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)?;

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
            &mut self.transcript,
            &mut self.token_stats,
            &mut self.renderer,
            term,
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
                self.redraw(term)?;
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
        let model = self.model.clone();
        let token_stats = self.token_stats.clone();
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
                            model: model.clone(),
                            token_stats: token_stats.clone(),
                            streaming: true,
                            approval_mode: *approval_mode,
                            queue_len: queue.len(),
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
        queue: &mut VecDeque<String>,
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
                            queue.push_back(text);
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
                    KeyCode::Left if input.cursor_pos > 0 => {
                        input.cursor_pos -= 1;
                    }
                    KeyCode::Left => {}
                    KeyCode::Right if input.cursor_pos < input.buffer.chars().count() => {
                        input.cursor_pos += 1;
                    }
                    KeyCode::Right => {}
                    KeyCode::Home => input.cursor_to_start(),
                    KeyCode::End => input.cursor_to_end(),
                    _ => {}
                }
            }
        }
    }
}
