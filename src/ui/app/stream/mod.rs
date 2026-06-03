use crate::chat::{Message, build_chat_history};
use crate::llm::{ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, ResponseStream};
use crate::tools::edit_file::preview_edit_file;
use crate::tools::shell::ShellTool;
use crate::tools::types::{Tool, ToolLiveEvent};
use crate::tools::{ApprovalMode, ToolCall, ToolResult};
use crate::ui::input::{InputAction, InputState};
use crate::ui::pane_page::PanePage;
use crate::ui::prompt::Decision;
use crate::ui::render::{BoneTerminal, PaneDraw, StatusInfo};
use crate::ui::tool_display::build_tool_row;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{StreamExt, pin_mut};
use std::collections::VecDeque;
use std::io;
use tokio::sync::mpsc;
use tokio::time::{self, Duration};

use super::App;

const INITIAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(90);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_PROVIDER_ATTEMPTS: usize = 2;

enum PendingTool {
    Approved(ToolCall),
    Result(ToolResult),
}

pub fn tool_error(call: &ToolCall, content: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        content: content.into(),
        is_error: true,
        pane_page: None,
        state: None,
    }
}

pub fn assistant_message(
    content: String,
    tool_calls: Vec<ToolCall>,
    reasoning: String,
) -> ChatMessage {
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

pub fn call_row_shown_during_prepare(call: &ToolCall) -> bool {
    call.name == "edit_file"
}

pub enum StreamFailure {
    Provider(LlmError),
    InitialTimeout,
    IdleTimeout,
}

impl StreamFailure {
    pub fn retryable(&self) -> bool {
        match self {
            Self::Provider(err) => matches!(
                err.kind,
                LlmErrorKind::Timeout | LlmErrorKind::Connection | LlmErrorKind::Server(_)
            ),
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
        format!("[{prefix}: {detail} within 90s; retried once]")
    } else {
        format!("[{prefix}: {detail} within 90s]")
    }
}

pub fn pane_toggle_hint(panes_visible: bool, has_pages: bool) -> Option<&'static str> {
    if !has_pages {
        None
    } else if panes_visible {
        Some("Ctrl+T hide panel")
    } else {
        Some("Ctrl+T show panel")
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

        self.submit_user_turn(text, None, term).await
    }

    pub(super) async fn submit_user_turn(
        &mut self,
        text: String,
        display_text: Option<String>,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        self.messages
            .push(Message::user(display_text.as_deref().unwrap_or(&text)));
        self.transcript
            .push(ChatMessage::new(ChatRole::User, &text));
        if let Some(ref db) = self.session_db {
            if let Some(conv_id) = self.conversation_id {
                self.session_seq += 1;
                db.append_message(conv_id, "user", &text, None, None, None, self.session_seq).ok();
            }
        }

        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.input.reset();
        self.redraw(term)?;

        self.auto_compact_if_needed(term)?;

        let mut history = build_chat_history(&self.transcript, None);

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

            if rounds > self.user_config.max_rounds {
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
                let content = self.messages[assistant_idx].content.clone();
                final_assistant_idx = assistant_idx;
                self.append_assistant_to_db(&content, None);
                break;
            }

            let assistant = assistant_message(
                self.messages[assistant_idx].content.clone(),
                tool_calls.clone(),
                reasoning_content,
            );
            history.push(assistant.clone());
            self.transcript.push(assistant);
            let content = self.messages[assistant_idx].content.clone();
            let tool_calls_json = serde_json::to_string(&tool_calls).ok();
            self.append_assistant_to_db(&content, tool_calls_json.as_deref());

            self.renderer
                .finalize_streaming_message(&self.messages[assistant_idx].content, term)?;

            let (calls_for_display, results) = self.handle_tool_calls(tool_calls, term).await?;
            if self.cancel_streaming {
                self.queue.clear();
                final_assistant_idx = assistant_idx;
                break;
            }

            for (call, result) in calls_for_display.iter().zip(results.iter()) {
                let display = self.tools.display_for_call(call);
                let visible = display.and_then(|d| d.show).unwrap_or(true);
                let has_result = display.and_then(|d| d.show_result).unwrap_or(false);
                if (visible || has_result) && !call_row_shown_during_prepare(call) {
                    self.messages.push(build_tool_row(call, result, display));
                }
            }
            self.renderer
                .flush_new_to_scrollback(&self.messages, term)?;

            for result in results {
                let message = ChatMessage::tool(result.clone());
                history.push(message.clone());
                self.transcript.push(message);
                self.append_tool_result_to_db(&result.name, &result.call_id, &result.content);
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

        self.auto_compact_if_needed(term)?;

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

        let result = ShellTool
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
            crate::tools::shell::truncate_output(&format!("$ {cmd}\n{result}"), 500);
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
        let received_baseline = self.token_stats.received;
        let mut real_completion_tokens: Option<u32> = None;
        let mut stream_start: Option<std::time::Instant> = None;
        let mut stream_end: Option<std::time::Instant> = None;
        let mut stream_chars: u64 = 0;
        let mut displayed_tps: Option<f64> = None;
        let mut last_tps_update: Option<std::time::Instant> = None;

        pin_mut!(idle);

        loop {
            if self.cancel_streaming {
                self.mark_cancelled(assistant_idx);
                break;
            }
            tokio::select! {
                chunk = stream.next() => match chunk {
                    Some(Ok(ChatEvent::TextDelta(text))) => {
                        if stream_start.is_none() {
                            stream_start = Some(std::time::Instant::now());
                        }
                        idle.as_mut().reset(time::Instant::now() + STREAM_IDLE_TIMEOUT);
                        self.messages[assistant_idx].content.push_str(&text);
                        stream_chars += text.len() as u64;
                        stream_estimated_received = received_baseline + ((stream_chars as f64 / 4.0) as u64);
                        stream_end = Some(std::time::Instant::now());
                        Self::refresh_tps(&mut displayed_tps, &mut last_tps_update, stream_start, stream_estimated_received, received_baseline);
                        self.redraw_streaming_tokens(assistant_idx, stream_estimated_received, displayed_tps, term)?;
                    }
                    Some(Ok(ChatEvent::ReasoningDelta(text))) => {
                        idle.as_mut().reset(time::Instant::now() + STREAM_IDLE_TIMEOUT);
                        reasoning_content.push_str(&text);
                    }
                    Some(Ok(ChatEvent::ToolCall(call))) => {
                        idle.as_mut().reset(time::Instant::now() + STREAM_IDLE_TIMEOUT);
                        stream_chars += call.arguments.to_string().len() as u64;
                        stream_estimated_received = received_baseline + ((stream_chars as f64 / 4.0) as u64).max(1);
                        tool_calls.push(call);
                        Self::refresh_tps(&mut displayed_tps, &mut last_tps_update, stream_start, stream_estimated_received, received_baseline);
                        self.redraw_streaming_tokens(assistant_idx, stream_estimated_received, displayed_tps, term)?;
                    }
                    Some(Ok(ChatEvent::TokenUsage { prompt_tokens, completion_tokens, cached_tokens, cost })) => {
                        idle.as_mut().reset(time::Instant::now() + STREAM_IDLE_TIMEOUT);
                        self.token_stats.record_request(prompt_tokens, completion_tokens, cached_tokens, cost);
                        stream_estimated_received = self.token_stats.received;
                        had_usage = true;
                        real_completion_tokens = Some(completion_tokens);
                        stream_end = Some(std::time::Instant::now());
                        if let Some(ref db) = self.session_db {
                            if let Some(conv_id) = self.conversation_id {
                                db.record_usage(conv_id, &self.llm.id(), self.llm.model(), prompt_tokens, completion_tokens, cached_tokens, cost).ok();
                            }
                        }
                    }
                    Some(Err(err)) => {
                        return Ok(Err(StreamFailure::Provider(err)));
                    }
                    None => break,
                },
                _ = &mut idle => return Ok(Err(StreamFailure::IdleTimeout)),
                _ = spinner.tick() => {
                    if Self::drain_keys(
                        &mut self.input,
                        &mut self.queue,
                        &mut self.approval_mode,
                        &mut self.cancel_streaming,
                        &mut self.panes_visible,
                        &mut self.pages,
                        &mut self.active_page,
                    ) {
                        self.user_config.approval_mode = self.approval_mode;
                        self.persist_runtime_config();
                    }
                    if self.cancel_streaming {
                        self.mark_cancelled(assistant_idx);
                        break;
                    }
                    let pages = if self.panes_visible {
                        self.pages.as_slice()
                    } else {
                        &[]
                    };
                    let hint = pane_toggle_hint(self.panes_visible, !self.pages.is_empty());
                    Self::refresh_tps(&mut displayed_tps, &mut last_tps_update, stream_start, stream_estimated_received, received_baseline);
                    self.renderer.tick_spinner(term, &PaneDraw {
                        input: &self.input,
                        status_info: &self.stream_status_info_with_tokens(Some(stream_estimated_received), displayed_tps),
                        pages,
                        active_page: self.active_page,
                        pane_toggle_hint: hint,
                    })?;
                }
            }
        }

        if !had_usage && !self.cancel_streaming {
            let prompt_chars = Self::estimate_context_chars(history, &self.tools.definitions());
            let completion_chars = self.messages[assistant_idx].content.chars().count();
            self.token_stats
                .record_estimate(prompt_chars, completion_chars);
        }

        // Store final tokens/sec for the status bar after streaming ends.
        if let (Some(start), Some(end)) = (stream_start, stream_end) {
            let elapsed = end.duration_since(start).as_secs_f64();
            if elapsed >= 0.1 {
                let new_tokens = if let Some(real) = real_completion_tokens {
                    real as u64
                } else {
                    stream_estimated_received.saturating_sub(received_baseline)
                };
                self.last_tokens_per_sec = Some(new_tokens as f64 / elapsed);
            }
        }
        Ok(Ok((tool_calls, reasoning_content)))
    }

    fn redraw_streaming_tokens(
        &mut self,
        assistant_idx: usize,
        tokens: u64,
        tokens_per_sec: Option<f64>,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        self.renderer.redraw_streaming_message(
            &self.messages[assistant_idx].content,
            term,
            &PaneDraw {
                input: &self.input,
                status_info: &self.stream_status_info_with_tokens(Some(tokens), tokens_per_sec),
                pages: if self.panes_visible { &self.pages } else { &[] },
                active_page: self.active_page,
                pane_toggle_hint: pane_toggle_hint(self.panes_visible, !self.pages.is_empty()),
            },
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

        let approved: Vec<ToolCall> = pending
            .iter()
            .filter_map(|pending| match pending {
                PendingTool::Approved(call) => Some(call.clone()),
                PendingTool::Result(_) => None,
            })
            .collect();

        self.show_immediate_tool_rows(&approved, term)?;
        let mut exec_results = self
            .execute_tools_responsive(approved, term)
            .await?
            .into_iter();
        let results: Vec<ToolResult> = pending
            .into_iter()
            .map(|pending| match pending {
                PendingTool::Result(result) => result,
                PendingTool::Approved(call) => exec_results
                    .next()
                    .unwrap_or_else(|| tool_error(&call, "internal error: tool result missing")),
            })
            .collect();

        // Process pane pages and session state from tool results
        for result in &results {
            // Store session state (e.g. task_list state)
            if let Some(ref state) = result.state {
                let source = result
                    .pane_page
                    .as_ref()
                    .map(|p| p.source.as_str())
                    .unwrap_or(&result.name);
                self.tools.state_map.set(source, "default", state.clone());
            }
            if let Some(page) = &result.pane_page {
                if page.content.is_empty() {
                    self.tools.state_map.remove(&page.source, "default");
                    self.active_page =
                        PanePage::remove(&mut self.pages, &page.source, self.active_page);
                } else {
                    let (_, new_active) =
                        PanePage::upsert(&mut self.pages, self.active_page, page.clone());
                    self.active_page = new_active;
                }
            }
        }

        // Resize viewport if pages changed during tool execution.
        self.renderer.ensure_viewport_height(
            term,
            &self.input,
            self.active_prompt.as_ref(),
            if self.panes_visible { &self.pages } else { &[] },
            self.active_page,
        )?;

        Ok((calls_for_display, results))
    }

    fn show_immediate_tool_rows(
        &mut self,
        _approved: &[ToolCall],
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        // Subagent calls stream their output in real-time via child process,
        // so they don't need placeholder rows in chat history.
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.redraw(term)
    }

    async fn execute_tools_responsive(
        &mut self,
        approved: Vec<ToolCall>,
        term: &mut BoneTerminal,
    ) -> io::Result<Vec<ToolResult>> {
        if approved.is_empty() {
            return Ok(Vec::new());
        }

        let cancel_calls = approved.clone();
        let tools = self.tools.clone();
        self.wait_for_tool_future_live(
            |events| async move { tools.execute_all_live(approved, Some(events)).await },
            &cancel_calls,
            term,
        )
        .await
    }

    fn apply_tool_live_event(&mut self, event: ToolLiveEvent) {
        match event {
            ToolLiveEvent::Pane(page) => {
                if page.content.is_empty() {
                    self.active_page =
                        PanePage::remove(&mut self.pages, &page.source, self.active_page);
                } else {
                    let (_, active) = PanePage::upsert(&mut self.pages, self.active_page, page);
                    self.active_page = active;
                }
            }
            ToolLiveEvent::StateUpdate {
                source,
                sub_key,
                state,
            } => {
                self.tools.state_map.set(&source, &sub_key, state);
                let merged = self.rebuild_merged_pane(&source);
                if let Some(page) = merged {
                    let (_, active) = PanePage::upsert(&mut self.pages, self.active_page, page);
                    self.active_page = active;
                }
            }
            ToolLiveEvent::StateRemove { source, sub_key } => {
                self.tools.state_map.remove(&source, &sub_key);
                let merged = self.rebuild_merged_pane(&source);
                match merged {
                    Some(page) => {
                        let (_, active) = PanePage::upsert(&mut self.pages, self.active_page, page);
                        self.active_page = active;
                    }
                    None => {
                        self.active_page =
                            PanePage::remove(&mut self.pages, &source, self.active_page);
                    }
                }
            }
        }
    }

    async fn wait_for_tool_future_live<F, Fut>(
        &mut self,
        make_future: F,
        cancel_calls: &[ToolCall],
        term: &mut BoneTerminal,
    ) -> io::Result<Vec<ToolResult>>
    where
        F: FnOnce(mpsc::UnboundedSender<ToolLiveEvent>) -> Fut,
        Fut: std::future::Future<Output = Vec<ToolResult>>,
    {
        let mut spinner = time::interval(Duration::from_millis(90));
        let (tx, mut rx) = mpsc::unbounded_channel::<ToolLiveEvent>();
        let future = make_future(tx);
        pin_mut!(future);

        loop {
            tokio::select! {
                results = &mut future => {
                    while let Ok(event) = rx.try_recv() {
                        self.apply_tool_live_event(event);
                    }
                    return Ok(results);
                }
                Some(event) = rx.recv() => {
                    self.apply_tool_live_event(event);
                }
                _ = spinner.tick() => {
                    if Self::drain_keys(
                        &mut self.input,
                        &mut self.queue,
                        &mut self.approval_mode,
                        &mut self.cancel_streaming,
                        &mut self.panes_visible,
                        &mut self.pages,
                        &mut self.active_page,
                    ) {
                        self.user_config.approval_mode = self.approval_mode;
                        self.persist_runtime_config();
                    }
                    if self.cancel_streaming {
                        let results = cancel_calls
                            .iter()
                            .map(|call| tool_error(call, "cancelled by user"))
                            .collect();
                        return Ok(results);
                    }

                    let visible_pages = if self.panes_visible {
                        self.pages.as_slice()
                    } else {
                        &[]
                    };
                    let hint = pane_toggle_hint(self.panes_visible, !self.pages.is_empty());
                    self.renderer.tick_spinner(
                        term,
                        &PaneDraw {
                            input: &self.input,
                            status_info: &self.status_info(),
                            pages: visible_pages,
                            active_page: self.active_page,
                            pane_toggle_hint: hint,
                        },
                    )?;
                }
            }
        }
    }

    async fn prepare_tool_call(
        &mut self,
        display_call: &ToolCall,
        mut call: ToolCall,
        term: &mut BoneTerminal,
    ) -> io::Result<PendingTool> {
        // Check if this is a dynamic tool with interaction
        if self.interaction_tools.contains(&call.name) {
            let question = call.arguments["question"].as_str().unwrap_or("");
            let mut options: Vec<String> = call.arguments["options"]
                .as_array()
                .and_then(|a| a.iter().map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if options.is_empty() {
                return Ok(PendingTool::Result(tool_error(
                    &call,
                    "interaction tool: options must be a non-empty array of strings",
                )));
            }
            let allow_custom = call.arguments["allow_custom"].as_bool().unwrap_or(true);
            let custom_option_index = if allow_custom {
                options.push("Other (type answer)".to_string());
                Some(options.len() - 1)
            } else {
                None
            };
            let prompt = crate::ui::prompt::Prompt::new(question, options.clone());
            self.active_prompt = Some(prompt);
            self.redraw(term)?;

            let selection = loop {
                if event::poll(std::time::Duration::from_millis(50))? {
                    let event = event::read()?;
                    if let Event::Paste(text) = event {
                        if self.active_prompt.is_none() {
                            self.input.insert_paste(&text);
                            self.redraw(term)?;
                        }
                        continue;
                    }
                    match event {
                        Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                            code if self.active_prompt.is_none() => {
                                match self.input.apply_key(code, key.modifiers) {
                                    InputAction::Submit => {
                                        let answer = self.input.buffer.trim().to_string();
                                        self.input.reset();
                                        break Some(answer);
                                    }
                                    InputAction::Cancel | InputAction::Escape => {
                                        self.input.clear_buffer();
                                        break None;
                                    }
                                    InputAction::Redraw => self.redraw(term)?,
                                    InputAction::None if code == KeyCode::Enter => {
                                        break Some(String::new());
                                    }
                                    _ => {}
                                }
                            }
                            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                                self.navigate_prompt(key.code, false, term)?;
                            }
                            KeyCode::Enter => {
                                if let Some(prompt) = self.active_prompt.as_ref()
                                    && Some(prompt.selected) == custom_option_index
                                {
                                    self.input.clear_buffer();
                                    self.active_prompt = None;
                                    self.redraw(term)?;
                                    continue;
                                }
                                break self
                                    .active_prompt
                                    .as_ref()
                                    .and_then(|p| options.get(p.selected).cloned());
                            }
                            KeyCode::Esc => break None,
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break None;
                            }
                            KeyCode::Char(c)
                                if key.modifiers.is_empty()
                                    && self.active_prompt.as_ref().is_some_and(|prompt| {
                                        Some(prompt.selected) == custom_option_index
                                    }) =>
                            {
                                self.input.clear_buffer();
                                self.input.insert_char(c);
                                self.active_prompt = None;
                                self.redraw(term)?;
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
            };

            self.active_prompt = None;
            self.redraw(term)?;

            return Ok(match selection {
                Some(choice) => PendingTool::Result(ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    content: choice,
                    is_error: false,
                    pane_page: None,
                    state: None,
                }),
                None => PendingTool::Result(tool_error(&call, "cancelled by user")),
            });
        }

        if call.name == "edit_file" {
            match preview_edit_file(call.arguments.clone()).await {
                Ok(preview) => {
                    if self
                        .tools
                        .display_for_call(&call)
                        .and_then(|display| display.show)
                        .unwrap_or(true)
                    {
                        self.messages.push(build_tool_row(
                            &call,
                            &ToolResult {
                                call_id: call.id.clone(),
                                name: call.name.clone(),
                                content: String::new(),
                                is_error: false,
                                pane_page: None,
                                state: None,
                            },
                            self.tools.display_for_call(&call),
                        ));
                    }
                    call.arguments["expected_hash"] =
                        serde_json::Value::String(preview.before_hash);
                    self.messages.push(Message::system(preview.diff));
                }
                Err(err) => {
                    return Ok(PendingTool::Result(tool_error(
                        &call,
                        format!("edit_file preview failed: {err}"),
                    )));
                }
            }
            self.renderer
                .flush_new_to_scrollback(&self.messages, term)?;
        }

        Ok(if self.tools.allows_call(self.approval_mode, &call) {
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

    pub(crate) fn estimate_context_chars(
        history: &[ChatMessage],
        tools: &[crate::tools::ToolDefinition],
    ) -> usize {
        let message_chars: usize = history
            .iter()
            .map(|message| {
                message.content.chars().count()
                    + message
                        .reasoning_content
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
                    + serde_json::to_string(&message.tool_calls)
                        .map(|json| json.chars().count())
                        .unwrap_or(0)
                    + message
                        .tool_call_id
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
                    + message
                        .name
                        .as_deref()
                        .map(str::chars)
                        .map(Iterator::count)
                        .unwrap_or(0)
            })
            .sum();
        let tool_chars = serde_json::to_string(tools)
            .map(|json| json.chars().count())
            .unwrap_or(0);
        message_chars + tool_chars
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
        let show_token_metrics = self.user_config.show_token_metrics;
        let input = &mut self.input;
        let queue = &mut self.queue;
        let renderer = &mut self.renderer;
        let approval_mode = &mut self.approval_mode;
        let user_config = &mut self.user_config;
        let custom_configs = &mut self.custom_configs;
        let cancel = &mut self.cancel_streaming;
        let panes_visible = &mut self.panes_visible;
        let pages = &mut self.pages;
        let active_page = &mut self.active_page;
        pin_mut!(request, spinner, timeout);

        loop {
            if *cancel {
                return (
                    Err(StreamFailure::Provider(LlmError::new_with_kind(
                        LlmErrorKind::Config,
                        "cancelled",
                    ))),
                    tick,
                );
            }
            tokio::select! {
                result = &mut request => return (result.map_err(StreamFailure::Provider), tick),
                _ = &mut timeout => return (Err(StreamFailure::InitialTimeout), tick),
                _ = &mut spinner => {
                    renderer.spinner_tick = renderer.spinner_tick.wrapping_add(1);
                    if Self::drain_keys(input, queue, approval_mode, cancel, panes_visible, pages, active_page) {
                        user_config.approval_mode = *approval_mode;
                        let mode = match user_config.approval_mode {
                            crate::tools::ApprovalMode::Danger => "danger",
                            crate::tools::ApprovalMode::Edits => "edit",
                            crate::tools::ApprovalMode::Safe => "safe",
                        };
                        custom_configs.set_value("general", "approval_mode", mode.to_string());
                    }
                    let visible_pages = if *panes_visible { pages.as_slice() } else { &[] };
                    let hint = pane_toggle_hint(*panes_visible, !pages.is_empty());
                    renderer
                        .ensure_viewport_height(term, input, None, visible_pages, *active_page)
                        .ok();
                    term.draw(|frame| {
                        renderer.draw_bottom_pane_with_tick(frame, &PaneDraw {
                            input,
                            status_info: &StatusInfo {
                                model: model.clone(),
                                token_stats: token_stats.clone(),
                                streaming_completion_tokens: None,
                                tokens_per_sec: None,
                                show_token_metrics: show_token_metrics,
                                streaming: true,
                                approval_mode: *approval_mode,
                                queue_len: queue.len(),
                            },
                            pages: visible_pages,
                            active_page: *active_page,
                            pane_toggle_hint: hint,
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
        panes_visible: &mut bool,
        pages: &mut [PanePage],
        active_page: &mut usize,
    ) -> bool {
        let mut mode_changed = false;
        while event::poll(std::time::Duration::from_millis(0)).unwrap_or(false) {
            match event::read().unwrap_or(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Null,
                KeyModifiers::NONE,
            ))) {
                Event::Paste(text) => input.insert_paste(&text),
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if key.code == KeyCode::Char('t')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        *panes_visible = !*panes_visible;
                        continue;
                    }
                    if *panes_visible && !pages.is_empty() {
                        *active_page = (*active_page).min(pages.len() - 1);
                        match (key.code, key.modifiers) {
                            (KeyCode::Tab, m) if m.is_empty() => {
                                *active_page = (*active_page + 1) % pages.len();
                                continue;
                            }
                            (KeyCode::BackTab, m) if m.is_empty() => {
                                *active_page = if *active_page == 0 {
                                    pages.len() - 1
                                } else {
                                    *active_page - 1
                                };
                                continue;
                            }
                            _ => {}
                        }
                        let page = &mut pages[*active_page];
                        match (key.code, key.modifiers) {
                            (KeyCode::PageUp, m) if m.is_empty() => {
                                page.scroll =
                                    page.scroll.saturating_sub(crate::ui::render::MAX_PANE_ROWS);
                                continue;
                            }
                            (KeyCode::PageDown, m) if m.is_empty() => {
                                let max_scroll = page.content.len().saturating_sub(
                                    crate::ui::render::clamped_pane_visible_rows(page.visible_rows),
                                );
                                page.scroll = (page.scroll + crate::ui::render::MAX_PANE_ROWS)
                                    .min(max_scroll);
                                continue;
                            }
                            (KeyCode::Up, m) if m.contains(KeyModifiers::CONTROL) => {
                                page.scroll = page.scroll.saturating_sub(1);
                                continue;
                            }
                            (KeyCode::Down, m) if m.contains(KeyModifiers::CONTROL) => {
                                let max_scroll = page.content.len().saturating_sub(
                                    crate::ui::render::clamped_pane_visible_rows(page.visible_rows),
                                );
                                page.scroll = (page.scroll + 1).min(max_scroll);
                                continue;
                            }
                            _ => {}
                        }
                    }
                    match input.apply_key(key.code, key.modifiers) {
                        InputAction::Cancel => {
                            *cancel = true;
                            queue.clear();
                            return mode_changed;
                        }
                        InputAction::Submit => {
                            let text = input.buffer.trim().to_string();
                            if !text.is_empty() {
                                queue.push_back(text);
                                input.reset();
                            }
                        }
                        InputAction::ClearQueue => queue.clear(),
                        InputAction::CycleMode => {
                            *mode = mode.cycle();
                            mode_changed = true;
                        }
                        InputAction::Redraw
                        | InputAction::Escape
                        | InputAction::OpenEditor
                        | InputAction::None => {}
                    }
                }
                _ => {}
            }
        }
        mode_changed
    }

    /// Compute output tokens/sec from stream start time and cumulative tokens.
    fn calc_tokens_per_sec(
        start: Option<std::time::Instant>,
        estimated: u64,
        base: u64,
    ) -> Option<f64> {
        let start = start?;
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed < 0.1 {
            return None;
        }
        let new_tokens = estimated.saturating_sub(base);
        Some(new_tokens as f64 / elapsed)
    }

    /// Refresh `displayed_tps` at most once every 500ms.
    fn refresh_tps(
        displayed_tps: &mut Option<f64>,
        last_update: &mut Option<std::time::Instant>,
        stream_start: Option<std::time::Instant>,
        estimated: u64,
        base: u64,
    ) {
        let now = std::time::Instant::now();
        let should_update = last_update
            .map(|t| now.duration_since(t) >= Duration::from_millis(500))
            .unwrap_or(true);
        if should_update {
            *displayed_tps = Self::calc_tokens_per_sec(stream_start, estimated, base);
            *last_update = Some(now);
        }
    }
}

