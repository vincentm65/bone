mod stream;

use crate::chat::Message;
use crate::config::ProvidersConfig;
use crate::llm::{ChatMessage, LlmProvider, TokenStats};
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
        let mut terminal = Renderer::init_terminal(MIN_ROWS)?;

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
        // If a blocking prompt is active, only prompt keys are handled.
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
                self.redraw(term)
            }
            InputAction::Redraw | InputAction::Escape => self.redraw(term),
            InputAction::OpenEditor => self.open_editor(term).await,
            InputAction::None => Ok(()),
        }
    }

    /// Handle a keypress while a blocking prompt is displayed.
    /// Up/Down move the cursor, Enter confirms, Esc rejects.
    fn handle_prompt_key(
        &mut self,
        code: KeyCode,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
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

    pub(super) async fn handle_command(
        &mut self,
        input: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
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

    async fn open_editor(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        Renderer::prepare_exit(term)?;
        let tmp = std::env::temp_dir().join("bone-edit.txt");
        std::fs::write(&tmp, "")?;
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "nano".to_string());
        let _ = tokio::process::Command::new(&editor)
            .arg(&tmp)
            .spawn()?.wait().await;
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
