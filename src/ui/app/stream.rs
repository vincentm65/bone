use crate::chat::{Message, build_chat_history};
use crate::llm::{ChatEvent, ChatMessage, ChatRole};
use crate::tools::edit_file::preview_edit_file;
use crate::tools::{ApprovalMode, ToolResult};
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

impl App {
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

    pub(crate) async fn send_message(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
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
                self.messages[assistant_idx]
                    .content
                    .push_str("\n[cancelled]");
                final_assistant_idx = assistant_idx;
                break;
            }

            let (stream_result, spinner_tick) = self.wait_for_stream(history.clone(), term).await;
            self.renderer.spinner_tick = spinner_tick;

            if self.cancel_streaming {
                self.messages[assistant_idx]
                    .content
                    .push_str("\n[cancelled]");
                final_assistant_idx = assistant_idx;
                break;
            }

            let mut tool_calls = Vec::new();
            // Accumulate reasoning/thinking content (DeepSeek V4) so it
            // can be passed back in the assistant message history.
            let mut reasoning_content = String::new();
            // Live estimate of cumulative output tokens during streaming.
            // Start from the current total, then replace with real usage when it arrives.
            let mut stream_estimated_received = self.token_stats.received;
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
                                    // Increment estimate: ~4 UTF-8 chars per token, rounded down to avoid overstating live output.
                                    stream_estimated_received += text.len() as u64 / 4;
                                    self.renderer.redraw_streaming_message(
                                        &self.messages[assistant_idx].content,
                                        term,
                                        &self.input,
                                        &self.stream_status_info_with_tokens(Some(stream_estimated_received)),
                                    )?;
                                }
                                Some(Ok(ChatEvent::ReasoningDelta(text))) => {
                                    reasoning_content.push_str(&text);
                                }
                                Some(Ok(ChatEvent::ToolCall(call))) => {
                                    // Estimate tokens from argument JSON size so the
                                    // live counter reflects tool activity immediately.
                                    let arg_tokens = call.arguments.to_string().len() as u64 / 4;
                                    stream_estimated_received += arg_tokens.max(1);
                                    tool_calls.push(call);
                                    self.renderer.redraw_streaming_message(
                                        &self.messages[assistant_idx].content,
                                        term,
                                        &self.input,
                                        &self.stream_status_info_with_tokens(Some(stream_estimated_received)),
                                    )?;
                                }
                                Some(Ok(ChatEvent::TokenUsage { prompt_tokens, completion_tokens })) => {
                                    self.token_stats.record_request(prompt_tokens, completion_tokens);
                                    stream_estimated_received = self.token_stats.received;
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
                                self.renderer.tick_spinner(term, &self.input, &self.stream_status_info_with_tokens(Some(stream_estimated_received)))?;
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
                        self.messages[assistant_idx].content = format!("[provider error: {err}]");
                    }
                    final_assistant_idx = assistant_idx;
                    break;
                }
            }

            if tool_calls.is_empty() || self.cancel_streaming {
                let mut msg = ChatMessage::new(
                    ChatRole::Assistant,
                    self.messages[assistant_idx].content.clone(),
                );
                if !reasoning_content.is_empty() {
                    msg.reasoning_content = Some(reasoning_content);
                }
                self.transcript.push(msg);
                final_assistant_idx = assistant_idx;
                break;
            }

            let mut assistant = ChatMessage::assistant_with_tools(
                self.messages[assistant_idx].content.clone(),
                tool_calls.clone(),
            );
            // DeepSeek V4 thinking mode: reasoning_content MUST be passed
            // back when the assistant turn involved tool calls.
            if !reasoning_content.is_empty() {
                assistant.reasoning_content = Some(reasoning_content);
            }
            history.push(assistant.clone());
            self.transcript.push(assistant);

            self.renderer.finalize_streaming_message(
                &self.messages[assistant_idx].content,
                term,
            )?;

            // Per-call approval.  Calls auto-approved by the current mode
            // are collected; the rest block with an interactive prompt.
            let calls_for_display = tool_calls.clone();
            let mut was_rejected = vec![false; tool_calls.len()];
            let mut preview_errors = vec![String::new(); tool_calls.len()];
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
                            let path = call.arguments["path"].as_str().unwrap_or("?");
                            self.messages.push(Message::system(format!(
                                "edit_file preview failed for {path}: {err}"
                            )));
                            self.renderer
                                .flush_new_to_scrollback(&self.messages, term)?;
                            preview_errors[i] = err;
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
                            // Advise does NOT execute the tool — it returns
                            // a result so the LLM can adapt its approach.
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

            // Merge rejected + advised + executed results in original order.
            let mut exec_iter = exec_results.into_iter();
            let results: Vec<ToolResult> = (0..calls_for_display.len())
                .map(|i| {
                    if !preview_errors[i].is_empty() {
                        ToolResult {
                            call_id: calls_for_display[i].id.clone(),
                            name: calls_for_display[i].name.clone(),
                            content: format!(
                                "edit_file preview failed: {}",
                                preview_errors[i]
                            ),
                            is_error: true,
                        }
                    } else if was_rejected[i] {
                        ToolResult {
                            call_id: calls_for_display[i].id.clone(),
                            name: calls_for_display[i].name.clone(),
                            content: "rejected by user".into(),
                            is_error: true,
                        }
                    } else if advised[i] {
                        // Advise: tool was NOT executed. Return a result so the
                        // LLM can adapt its approach instead of proceeding blindly.
                        ToolResult {
                            call_id: calls_for_display[i].id.clone(),
                            name: calls_for_display[i].name.clone(),
                            content: "[exit_code=1] Tool not executed. User advice: proceed carefully, verify assumptions, and explain your approach before taking action.".into(),
                            is_error: true,
                        }
                    } else {
                        exec_iter.next().unwrap_or_else(|| ToolResult {
                            call_id: calls_for_display[i].id.clone(),
                            name: calls_for_display[i].name.clone(),
                            content: "internal error: tool result missing".into(),
                            is_error: true,
                        })
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
        self.renderer.finalize_streaming_message(
            &self.messages[final_assistant_idx].content,
            term,
        )?;
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)?;

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
                    InputAction::Redraw | InputAction::Escape | InputAction::None => {}
                }
            }
        }
    }
}