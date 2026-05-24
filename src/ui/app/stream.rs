use crate::chat::{Message, build_chat_history};
use crate::llm::{ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, ResponseStream};
use crate::tools::bash::BashTool;
use crate::tools::edit_file::preview_edit_file;
use crate::tools::types::Tool;
use crate::tools::{ApprovalMode, ToolCall, ToolResult};
use crate::ui::input::{InputAction, InputState};
use crate::ui::prompt::Decision;
use crate::ui::render::{BoneTerminal, StatusInfo};
use crate::ui::tool_display::build_tool_row;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{StreamExt, pin_mut};
use std::collections::VecDeque;
use std::io;
use tokio::time::{self, Duration};

use super::App;

const INITIAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PROVIDER_ATTEMPTS: usize = 2;

enum PendingTool {
    Approved(ToolCall),
    Result(ToolResult),
}

fn tool_error(call: &ToolCall, content: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        content: content.into(),
        is_error: true,
    }
}

fn assistant_message(content: String, tool_calls: Vec<ToolCall>, reasoning: String) -> ChatMessage {
    let mut message = if tool_calls.is_empty() {
        ChatMessage::new(ChatRole::Assistant, content)
    } else {
        ChatMessage::assistant_with_tools(content, tool_calls)
    };
    if !reasoning.is_empty() {
        message.reasoning_content = Some(reasoning);
    }
    message
}

pub enum StreamFailure {
    Provider(LlmError),
    InitialTimeout,
    IdleTimeout,
}

impl StreamFailure {
    pub fn retryable(&self) -> bool {
        match self {
            Self::Provider(err) => {
                matches!(err.kind, LlmErrorKind::Timeout | LlmErrorKind::Connection)
            }
            Self::InitialTimeout | Self::IdleTimeout => true,
        }
    }

    fn display_message(&self, retried: bool) -> String {
        match self {
            Self::InitialTimeout => timeout_message("provider timeout", "no response", retried),
            Self::IdleTimeout => timeout_message("stream timeout", "no events", retried),
            Self::Provider(err) if matches!(err.kind, LlmErrorKind::Timeout) => {
                timeout_message("provider timeout", "request timed out", retried)
            }
            Self::Provider(err) if matches!(err.kind, LlmErrorKind::Connection) => {
                timeout_message("provider error", "connection refused", retried)
            }
            Self::Provider(err) => format!("[provider error: {err}]"),
        }
    }
}

pub fn timeout_message(prefix: &str, detail: &str, retried: bool) -> String {
    if retried {
        format!("[{prefix}: {detail} within 30s; retried once]")
    } else {
        format!("[{prefix}: {detail} within 30s]")
    }
}

impl App {
    pub(crate) async fn send_message(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        let text = self.input.buffer.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        if let Some(cmd) = text.strip_prefix(':') {
            return self.run_inline_command(cmd, term).await;
        }

        if let Some(cmd) = text.strip_prefix('/') {
            self.input.reset();
            self.redraw(term)?;
            return self.handle_command(cmd, term).await;
        }

        self.messages.push(Message::user(&text));
        self.transcript
            .push(ChatMessage::new(ChatRole::User, &text));

        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.input.reset();
        self.redraw(term)?;

        let mut history = build_chat_history(&self.transcript);

        self.streaming = true;
        self.redraw(term)?;

        let mut rounds = 0u32;
        let final_assistant_idx;
        loop {
            rounds += 1;
            self.messages.push(Message::assistant(String::new()));
            self.renderer.streaming_source_flushed = 0;
            self.renderer.streaming_lines_flushed = 0;
            self.renderer.scrollback_cursor += 1;
            let assistant_idx = self.messages.len() - 1;

            if rounds > 64 {
                self.messages[assistant_idx]
                    .content
                    .push_str("\n[tool-call round limit reached]");
                final_assistant_idx = assistant_idx;
                break;
            }
            if self.cancel_streaming {
                self.mark_cancelled(assistant_idx);
                final_assistant_idx = assistant_idx;
                break;
            }

            let mut last_failure = None;
            let mut stream_output = None;
            for attempt in 1..=MAX_PROVIDER_ATTEMPTS {
                self.messages[assistant_idx].content.clear();
                self.renderer.streaming_source_flushed = 0;
                self.renderer.streaming_lines_flushed = 0;

                let (stream_result, spinner_tick) =
                    self.wait_for_stream(history.clone(), term).await;
                self.renderer.spinner_tick = spinner_tick;

                if self.cancel_streaming {
                    self.mark_cancelled(assistant_idx);
                    break;
                }

                let stream = match stream_result {
                    Ok(stream) => stream,
                    Err(failure) => {
                        let retryable = failure.retryable();
                        last_failure = Some(failure);
                        if retryable && attempt < MAX_PROVIDER_ATTEMPTS {
                            time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                        break;
                    }
                };

                match self
                    .consume_stream(stream, assistant_idx, &history, term)
                    .await?
                {
                    Ok(output) => {
                        stream_output = Some(output);
                        break;
                    }
                    Err(failure) => {
                        if self.cancel_streaming {
                            self.mark_cancelled(assistant_idx);
                            break;
                        }
                        let retryable = failure.retryable();
                        last_failure = Some(failure);
                        if retryable && attempt < MAX_PROVIDER_ATTEMPTS {
                            time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                        break;
                    }
                }
            }

            if self.cancel_streaming {
                self.mark_cancelled(assistant_idx);
                final_assistant_idx = assistant_idx;
                break;
            }

            let Some((tool_calls, reasoning_content)) = stream_output else {
                if let Some(failure) = last_failure {
                    self.messages[assistant_idx].content =
                        failure.display_message(MAX_PROVIDER_ATTEMPTS > 1);
                }
                final_assistant_idx = assistant_idx;
                break;
            };

            if tool_calls.is_empty() || self.cancel_streaming {
                let msg = assistant_message(
                    self.messages[assistant_idx].content.clone(),
                    Vec::new(),
                    reasoning_content,
                );
                self.transcript.push(msg);
                final_assistant_idx = assistant_idx;
                break;
            }

            let assistant = assistant_message(
                self.messages[assistant_idx].content.clone(),
                tool_calls.clone(),
                reasoning_content,
            );
            history.push(assistant.clone());
            self.transcript.push(assistant);

            self.renderer
                .finalize_streaming_message(&self.messages[assistant_idx].content, term)?;

            let (calls_for_display, results) = self.handle_tool_calls(tool_calls, term).await?;
            if self.cancel_streaming {
                self.queue.clear();
                final_assistant_idx = assistant_idx;
                break;
            }

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
            .finalize_streaming_message(&self.messages[final_assistant_idx].content, term)?;
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)?;

        Ok(())
    }

    async fn run_inline_command(&mut self, cmd: &str, term: &mut BoneTerminal) -> io::Result<()> {
        use crate::tools::command_policy::classify_command;

        let safety = classify_command(cmd);
        let classification = match safety {
            crate::tools::command_policy::CommandSafety::ReadOnly => "read_only",
            crate::tools::command_policy::CommandSafety::Edit => "edit",
            crate::tools::command_policy::CommandSafety::Danger => "danger",
        };

        let result = BashTool
            .execute(serde_json::json!({
                "command": cmd,
                "classification": classification,
                "timeout_ms": 60_000,
            }))
            .await
            .unwrap_or_else(|e| format!("[error: {e}]"));

        let is_error = result.contains("exit code: 1") || result.contains("timed out");
        let display = format!("{cmd}\n{result}");
        self.input.reset();
        self.messages
            .push(Message::terminal_output(cmd.to_string(), display, is_error));
        let transcript_text =
            crate::tools::bash::truncate_output(&format!("$ {cmd}\n{result}"), 500);
        self.transcript
            .push(ChatMessage::new(ChatRole::User, &transcript_text));
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)?;
        Ok(())
    }

    fn mark_cancelled(&mut self, assistant_idx: usize) {
        if !self.messages[assistant_idx]
            .content
            .ends_with("\n[cancelled]")
        {
            self.messages[assistant_idx]
                .content
                .push_str("\n[cancelled]");
        }
    }

    async fn consume_stream(
        &mut self,
        mut stream: ResponseStream,
        assistant_idx: usize,
        history: &[ChatMessage],
        term: &mut BoneTerminal,
    ) -> io::Result<Result<(Vec<ToolCall>, String), StreamFailure>> {
        let mut spinner = time::interval(Duration::from_millis(90));
        let idle = time::sleep(STREAM_IDLE_TIMEOUT);
        let mut had_usage = false;
        let mut tool_calls = Vec::new();
        let mut reasoning_content = String::new();
        let mut stream_estimated_received = self.token_stats.received;
        pin_mut!(idle);

        loop {
            if self.cancel_streaming {
                self.mark_cancelled(assistant_idx);
                break;
            }
            tokio::select! {
                chunk = stream.next() => match chunk {
                    Some(Ok(ChatEvent::TextDelta(text))) => {
                        idle.as_mut().reset(time::Instant::now() + STREAM_IDLE_TIMEOUT);
                        self.messages[assistant_idx].content.push_str(&text);
                        stream_estimated_received += text.len() as u64 / 4;
                        self.redraw_streaming_tokens(assistant_idx, stream_estimated_received, term)?;
                    }
                    Some(Ok(ChatEvent::ReasoningDelta(text))) => {
                        idle.as_mut().reset(time::Instant::now() + STREAM_IDLE_TIMEOUT);
                        reasoning_content.push_str(&text);
                    }
                    Some(Ok(ChatEvent::ToolCall(call))) => {
                        idle.as_mut().reset(time::Instant::now() + STREAM_IDLE_TIMEOUT);
                        stream_estimated_received += (call.arguments.to_string().len() as u64 / 4).max(1);
                        tool_calls.push(call);
                        self.redraw_streaming_tokens(assistant_idx, stream_estimated_received, term)?;
                    }
                    Some(Ok(ChatEvent::TokenUsage { prompt_tokens, completion_tokens })) => {
                        idle.as_mut().reset(time::Instant::now() + STREAM_IDLE_TIMEOUT);
                        self.token_stats.record_request(prompt_tokens, completion_tokens);
                        stream_estimated_received = self.token_stats.received;
                        had_usage = true;
                    }
                    Some(Err(err)) => {
                        return Ok(Err(StreamFailure::Provider(err)));
                    }
                    None => break,
                },
                _ = &mut idle => return Ok(Err(StreamFailure::IdleTimeout)),
                _ = spinner.tick() => {
                    Self::drain_keys(
                        &mut self.input,
                        &mut self.queue,
                        &mut self.approval_mode,
                        &mut self.cancel_streaming,
                    );
                    if self.cancel_streaming {
                        self.mark_cancelled(assistant_idx);
                        break;
                    }
                    self.renderer.tick_spinner(term, &self.input, &self.stream_status_info_with_tokens(Some(stream_estimated_received)))?;
                }
            }
        }

        if !had_usage && !self.cancel_streaming {
            let prompt_chars: usize = history.iter().map(|m| m.content.len()).sum();
            let completion_chars = self.messages[assistant_idx].content.len();
            self.token_stats
                .record_estimate(prompt_chars, completion_chars);
        }

        Ok(Ok((tool_calls, reasoning_content)))
    }

    fn redraw_streaming_tokens(
        &mut self,
        assistant_idx: usize,
        tokens: u64,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        self.renderer.redraw_streaming_message(
            &self.messages[assistant_idx].content,
            term,
            &self.input,
            &self.stream_status_info_with_tokens(Some(tokens)),
        )
    }

    async fn handle_tool_calls(
        &mut self,
        tool_calls: Vec<ToolCall>,
        term: &mut BoneTerminal,
    ) -> io::Result<(Vec<ToolCall>, Vec<ToolResult>)> {
        let calls_for_display = tool_calls.clone();
        let mut pending = Vec::with_capacity(tool_calls.len());

        for (display_call, call) in calls_for_display.iter().zip(tool_calls) {
            if self.cancel_streaming {
                break;
            }
            pending.push(self.prepare_tool_call(display_call, call, term).await?);
        }

        let approved = pending
            .iter()
            .filter_map(|pending| match pending {
                PendingTool::Approved(call) => Some(call.clone()),
                PendingTool::Result(_) => None,
            })
            .collect();
        let mut exec_results = self.tools.execute_all(approved).await.into_iter();
        let results = pending
            .into_iter()
            .map(|pending| match pending {
                PendingTool::Result(result) => result,
                PendingTool::Approved(call) => exec_results
                    .next()
                    .unwrap_or_else(|| tool_error(&call, "internal error: tool result missing")),
            })
            .collect();

        Ok((calls_for_display, results))
    }

    async fn prepare_tool_call(
        &mut self,
        display_call: &ToolCall,
        mut call: ToolCall,
        term: &mut BoneTerminal,
    ) -> io::Result<PendingTool> {
        if call.name == "edit_file" {
            match preview_edit_file(call.arguments.clone()).await {
                Ok(preview) => {
                    call.arguments["expected_hash"] =
                        serde_json::Value::String(preview.before_hash);
                    self.messages.push(Message::system(preview.diff));
                }
                Err(err) => {
                    let path = call.arguments["path"].as_str().unwrap_or("?");
                    self.messages.push(Message::system(format!(
                        "edit_file preview failed for {path}: {err}"
                    )));
                    self.renderer
                        .flush_new_to_scrollback(&self.messages, term)?;
                    return Ok(PendingTool::Result(tool_error(
                        &call,
                        format!("edit_file preview failed: {err}"),
                    )));
                }
            }
            self.renderer
                .flush_new_to_scrollback(&self.messages, term)?;
        }

        Ok(if self.approval_mode.allows_call(&call) {
            PendingTool::Approved(call)
        } else {
            match self.prompt_and_wait(display_call, term)? {
                Decision::Accept => PendingTool::Approved(call),
                Decision::Cancel => {
                    self.cancel_streaming = true;
                    self.queue.clear();
                    PendingTool::Result(tool_error(&call, "cancelled by user"))
                }
                Decision::Advise(advice) => {
                    let advice = if advice.trim().is_empty() {
                        "proceed carefully, verify assumptions, and explain your approach before taking action"
                    } else {
                        advice.trim()
                    };
                    PendingTool::Result(tool_error(
                        &call,
                        format!("[exit_code=1] Tool not executed. User advice: {advice}"),
                    ))
                }
            }
        })
    }

    async fn wait_for_stream(
        &mut self,
        history: Vec<ChatMessage>,
        term: &mut BoneTerminal,
    ) -> (Result<ResponseStream, StreamFailure>, usize) {
        let request = self.llm.chat_stream(history, self.tools.definitions());
        let spinner = time::sleep(Duration::from_millis(90));
        let timeout = time::sleep(INITIAL_RESPONSE_TIMEOUT);
        let tick = self.renderer.spinner_tick;
        let model = self.model.clone();
        let token_stats = self.token_stats.clone();
        let input = &mut self.input;
        let queue = &mut self.queue;
        let renderer = &mut self.renderer;
        let approval_mode = &mut self.approval_mode;
        let cancel = &mut self.cancel_streaming;
        pin_mut!(request, spinner, timeout);

        loop {
            if *cancel {
                return (
                    Err(StreamFailure::Provider(LlmError::new("cancelled"))),
                    tick,
                );
            }
            tokio::select! {
                result = &mut request => return (result.map_err(StreamFailure::Provider), tick),
                _ = &mut timeout => return (Err(StreamFailure::InitialTimeout), tick),
                _ = &mut spinner => {
                    renderer.spinner_tick = renderer.spinner_tick.wrapping_add(1);
                    Self::drain_keys(input, queue, approval_mode, cancel);
                    term.draw(|frame| {
                        renderer.draw_bottom_pane_with_tick(frame, input, &StatusInfo {
                            model: model.clone(),
                            token_stats: token_stats.clone(),
                            streaming_completion_tokens: None,
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
                match input.apply_key(key.code, key.modifiers) {
                    InputAction::Cancel => {
                        *cancel = true;
                        queue.clear();
                        return;
                    }
                    InputAction::Submit => {
                        let text = input.buffer.trim().to_string();
                        if !text.is_empty() {
                            queue.push_back(text);
                            input.reset();
                        }
                    }
                    InputAction::ClearQueue => queue.clear(),
                    InputAction::CycleMode => *mode = mode.cycle(),
                    InputAction::Redraw
                    | InputAction::Escape
                    | InputAction::OpenEditor
                    | InputAction::None => {}
                }
            }
        }
    }
}
