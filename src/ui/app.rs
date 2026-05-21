use crate::chat::{Context, Message, build_chat_history};
use crate::config::ProvidersConfig;
use crate::llm::{ChatEvent, ChatMessage, LlmProvider};
use crate::tools::{ToolHandler, builtin_tools};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{pin_mut, StreamExt};
use std::io;
use tokio::time::{self, Duration};

use super::commands;
use super::input::InputState;
use super::render::{BoneTerminal, Renderer, StatusInfo};

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
                "bone v0.1.0 — type /help for commands. Esc to quit.",
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
        })
    }

    pub async fn run(&mut self) -> io::Result<()> {
        let mut terminal = Renderer::init_terminal()?;

        // Startup banner
        self.renderer.render_banner(
            &mut terminal,
            &self.provider,
            &self.model,
        )?;

        // Push initial system message into scrollback
        self.renderer.flush_new_to_scrollback(&self.messages, &mut terminal)?;

        // Initial draw of the bottom pane
        terminal.draw(|frame| {
            self.renderer.draw_bottom_pane(frame, &self.input, &self.status_info());
        })?;

        // Main event loop
        while !self.should_quit {
            if event::poll(std::time::Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press {
                        self.handle_key(key.code, key.modifiers, &mut terminal).await?;
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
        }
    }

    fn draw(&self, frame: &mut ratatui::Frame) {
        self.renderer.draw_bottom_pane(frame, &self.input, &self.status_info());
    }

    async fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
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
                self.should_quit = true;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Process queued key events while streaming. Enter queues a message,
    /// Ctrl+D clears the queue. All other input edits work normally.
    fn process_keys_while_streaming(&mut self) {
        Self::drain_keys(&mut self.input, &mut self.queue);
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
        self.renderer.flush_new_to_scrollback(&self.messages, term)?;

        let mut history = build_chat_history(&self.messages, &self.context);

        self.streaming = true;
        self.messages.push(Message::assistant(String::new()));
        self.renderer.streaming_lines_flushed = 0;
        self.renderer.scrollback_cursor += 1;
        let assistant_idx = self.messages.len() - 1;
        term.draw(|frame| self.draw(frame))?;

        for _ in 0..8 {
            let (stream_result, spinner_tick) = self.wait_for_stream(history.clone(), term).await;
            self.renderer.spinner_tick = spinner_tick;
            let mut tool_calls = Vec::new();
            match stream_result {
                Ok(mut stream) => {
                    let mut spinner = time::interval(Duration::from_millis(90));
                    loop {
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
                                self.renderer.tick_spinner(term, &self.input, &self.status_info())?;
                            }
                        }
                    }
                }
                Err(err) => {
                    self.messages[assistant_idx].content = format!(
                        "[provider error: {err}]\n\nIs llama.cpp server running at http://127.0.0.1:8080?"
                    );
                    break;
                }
            }

            history.push(ChatMessage::assistant_with_tools(
                self.messages[assistant_idx].content.clone(),
                tool_calls.clone(),
            ));

            if tool_calls.is_empty() {
                break;
            }

            let results = self.tools.execute_all(tool_calls).await;
            for result in results {
                history.push(ChatMessage::tool(result));
            }
        }

        self.streaming = false;
        self.renderer.finalize_streaming_message(
            &self.messages[assistant_idx].content,
            term,
        )?;
        self.renderer.flush_new_to_scrollback(&self.messages, term)?;
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
        ).await?;

        match result {
            commands::CommandResult::Quit => {
                self.should_quit = true;
            }
            commands::CommandResult::Continue { reply } => {
                self.messages.push(Message::system(reply));
                self.renderer.flush_new_to_scrollback(&self.messages, term)?;
                term.draw(|frame| self.draw(frame))?;
            }
        }
        Ok(())
    }

    async fn wait_for_stream(
        &mut self,
        history: Vec<crate::llm::ChatMessage>,
        term: &mut BoneTerminal,
    ) -> (Result<crate::llm::ResponseStream, crate::llm::LlmError>, usize) {
        let request = self.llm.chat_stream(history, self.tools.definitions());
        let spinner = time::sleep(Duration::from_millis(90));
        let tick = self.renderer.spinner_tick;
        let provider = self.provider.clone();
        let model = self.model.clone();
        let msg_count = self.messages.len();
        let input = &mut self.input;
        let queue = &mut self.queue;
        let renderer = &mut self.renderer;
        pin_mut!(request, spinner);

        loop {
            tokio::select! {
                result = &mut request => return (result, tick),
                _ = &mut spinner => {
                    renderer.spinner_tick = renderer.spinner_tick.wrapping_add(1);
                    Self::drain_keys(input, queue);
                    term.draw(|frame| {
                        renderer.draw_bottom_pane_with_tick(frame, input, &StatusInfo {
                            queue_len: queue.len(),
                            provider: provider.clone(),
                            model: model.clone(),
                            msg_count,
                            streaming: true,
                        }, renderer.spinner_tick);
                    }).ok();
                    spinner.as_mut().reset(time::Instant::now() + Duration::from_millis(90));
                }
            }
        }
    }

    /// Drain pending key events into input edits or queue. Used during streaming.
    fn drain_keys(input: &mut InputState, queue: &mut Vec<String>) {
        while event::poll(std::time::Duration::from_millis(0)).unwrap_or(false) {
            if let Event::Key(key) = event::read().unwrap_or(Event::Key(crossterm::event::KeyEvent::new(KeyCode::Null, KeyModifiers::NONE))) {
                if key.kind != KeyEventKind::Press { continue; }
                let mods = key.modifiers;
                match key.code {
                    KeyCode::Enter => {
                        let text = input.buffer.trim().to_string();
                        if !text.is_empty() { queue.push(text); input.reset(); }
                    }
                    KeyCode::Char('a') if mods.contains(KeyModifiers::CONTROL) => input.cursor_to_start(),
                    KeyCode::Char('e') if mods.contains(KeyModifiers::CONTROL) => input.cursor_to_end(),
                    KeyCode::Char('w') if mods.contains(KeyModifiers::CONTROL) => input.delete_word_backward(),
                    KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => input.clear_buffer(),
                    KeyCode::Char('d') if mods.contains(KeyModifiers::CONTROL) => queue.clear(),
                    KeyCode::Char(c) => input.insert_char(c),
                    KeyCode::Backspace => input.delete_backward(),
                    KeyCode::Left => { if input.cursor_pos > 0 { input.cursor_pos -= 1; } }
                    KeyCode::Right => { if input.cursor_pos < input.buffer.chars().count() { input.cursor_pos += 1; } }
                    KeyCode::Home => input.cursor_to_start(),
                    KeyCode::End => input.cursor_to_end(),
                    _ => {}
                }
            }
        }
    }
}
