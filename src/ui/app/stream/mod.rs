use crate::chat::{Message, build_chat_history};
use crate::ext::EventDispatchResult;
use crate::llm::{ChatEvent, ChatMessage, ChatRole, LlmError, LlmErrorKind, ResponseStream};
use crate::tools::command_policy::CommandSafety;
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
use std::time::Instant;
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

pub fn show_immediate_tool_row(call: &ToolCall) -> bool {
    !matches!(call.name.as_str(), "read_file" | "edit_file")
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
        self.extensions.dispatch_simple(
            "message",
            serde_json::json!({ "role": "user", "content": text }),
        );
        if let Some(ref db) = self.session_db
            && let Some(conv_id) = self.conversation_id
        {
            self.session_seq += 1;
            db.append_message(conv_id, "user", &text, None, None, None, self.session_seq)
                .ok();
        }

        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;
        self.input.reset();
        self.redraw(term)?;

        let mut history = build_chat_history(&self.transcript, None);

        self.streaming = true;
        self.turn_start = Some(Instant::now());
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;
        self.shown_tool_rows.clear();
        self.redraw(term)?;

        let final_assistant_idx;
        loop {
            self.messages.push(Message::assistant(String::new()));
            self.renderer.streaming_source_flushed = 0;
            self.renderer.streaming_lines_flushed = 0;
            self.renderer.scrollback_cursor += 1;
            let assistant_idx = self.messages.len() - 1;

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
                let already_shown = self.shown_tool_rows.contains(&call.id);
                if (visible || has_result) && !call_row_shown_during_prepare(call) && !already_shown
                {
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
        self.turn_start = None;
        self.turn_paused_duration = std::time::Duration::ZERO;
        self.turn_pause_start = None;
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
        let mut stream_chars: u64 = 0;

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
                        stream_chars += text.len() as u64;
                        stream_estimated_received = received_baseline + ((stream_chars as f64 / 4.0) as u64);
                        self.redraw_streaming_tokens(assistant_idx, stream_estimated_received, term)?;
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
                        self.redraw_streaming_tokens(assistant_idx, stream_estimated_received, term)?;
                    }
                    Some(Ok(ChatEvent::TokenUsage { prompt_tokens, completion_tokens, cached_tokens, cost })) => {
                        idle.as_mut().reset(time::Instant::now() + STREAM_IDLE_TIMEOUT);
                        self.token_stats.record_request(prompt_tokens, completion_tokens, cached_tokens, cost);
                        stream_estimated_received = self.token_stats.received;
                        had_usage = true;
                        if let Some(ref db) = self.session_db
                            && let Some(conv_id) = self.conversation_id {
                                db.record_usage(conv_id, self.llm.id(), self.llm.model(), prompt_tokens, completion_tokens, cached_tokens, cost, false).ok();
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
                    // Refresh subagent pane if version changed.
                    if crate::ext::jobs::registry().version() != self.subagent_seen_version {
                        self.refresh_subagent_pane();
                        self.subagent_seen_version = crate::ext::jobs::registry().version();
                    }
                    self.renderer.tick_spinner(term, &PaneDraw {
                        input: &self.input,
                        status_info: &self.stream_status_info_with_tokens(Some(stream_estimated_received)),
                        pages: if self.panes_visible { &self.pages } else { &[] },
                        active_page: self.active_page,
                        pane_toggle_hint: pane_toggle_hint(self.panes_visible, !self.pages.is_empty()),
                        autocomplete: None,
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
            &PaneDraw {
                input: &self.input,
                status_info: &self.stream_status_info_with_tokens(Some(tokens)),
                pages: if self.panes_visible { &self.pages } else { &[] },
                active_page: self.active_page,
                pane_toggle_hint: pane_toggle_hint(self.panes_visible, !self.pages.is_empty()),
                autocomplete: None,
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

        // Dispatch tool_call events and filter blocked calls.
        let mut blocked_results: Vec<ToolResult> = Vec::new();
        let approved: Vec<ToolCall> = approved
            .into_iter()
            .filter(|call| {
                let safety = match self.tools.safety_for_call(call) {
                    CommandSafety::ReadOnly => "read_only",
                    CommandSafety::Danger => "danger",
                };
                match self.extensions.dispatch_tool_call(
                    &call.name,
                    &call.id,
                    &call.arguments,
                    safety,
                ) {
                    EventDispatchResult::Blocked { reason } => {
                        blocked_results.push(tool_error(call, reason));
                        false
                    }
                    EventDispatchResult::Continue => true,
                }
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
            .chain(blocked_results)
            .collect();

        // Dispatch tool_result events.
        for result in &results {
            self.extensions
                .dispatch_tool_result(&result.name, &result.call_id, result.is_error);
        }

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
                    // Rebuild merged pane instead of blindly removing the whole
                    // source page — other sub_key entries may still be active.
                    let merged = self.rebuild_merged_pane(&page.source);
                    match merged {
                        Some(merged_page) => {
                            let (_, new_active) =
                                PanePage::upsert(&mut self.pages, self.active_page, merged_page);
                            self.active_page = new_active;
                        }
                        None => {
                            self.active_page =
                                PanePage::remove(&mut self.pages, &page.source, self.active_page);
                        }
                    }
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
            None,
        )?;

        Ok((calls_for_display, results))
    }

    fn show_immediate_tool_rows(
        &mut self,
        approved: &[ToolCall],
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        for call in approved {
            if !show_immediate_tool_row(call) {
                continue;
            }
            let display = self.tools.display_for_call(call);
            let visible = display.and_then(|d| d.show).unwrap_or(true);
            if visible {
                self.messages.push(build_tool_row(
                    call,
                    &ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: String::new(),
                        is_error: false,
                        pane_page: None,
                        state: None,
                    },
                    display,
                ));
                self.shown_tool_rows.insert(call.id.clone());
            }
        }
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
        // Create a cancellation token for this batch of tool calls.
        let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.tools.cancel_token = Some(cancel_token.clone());
        let tools = self.tools.clone();
        self.wait_for_tool_future_live(
            |events| async move { tools.execute_all_live(approved, Some(events), 0, 0).await },
            &cancel_calls,
            term,
            cancel_token,
        )
        .await
    }

    /// Track live-state entries in a local set so synthetic StateRemove
    /// events can be emitted for stale entries on cancellation.
    fn track_live_state(
        active: &mut std::collections::HashSet<(String, String)>,
        event: &ToolLiveEvent,
    ) {
        match event {
            ToolLiveEvent::StateUpdate {
                source, sub_key, ..
            } => {
                active.insert((source.clone(), sub_key.clone()));
            }
            ToolLiveEvent::StateRemove { source, sub_key } => {
                active.remove(&(source.clone(), sub_key.clone()));
            }
            ToolLiveEvent::Pane(_) => {}
        }
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
        cancel_token: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> io::Result<Vec<ToolResult>>
    where
        F: FnOnce(mpsc::UnboundedSender<ToolLiveEvent>) -> Fut,
        Fut: std::future::Future<Output = Vec<ToolResult>>,
    {
        let mut spinner = time::interval(Duration::from_millis(90));
        let (tx, mut rx) = mpsc::unbounded_channel::<ToolLiveEvent>();
        let future = make_future(tx);
        pin_mut!(future);
        // Track active live-state entries locally so we can emit synthetic
        // StateRemove events on cancellation.
        let mut active_live_state = std::collections::HashSet::<(String, String)>::new();

        loop {
            tokio::select! {
                results = &mut future => {
                    while let Ok(event) = rx.try_recv() {
                        Self::track_live_state(&mut active_live_state, &event);
                        self.apply_tool_live_event(event);
                    }
                    return Ok(results);
                }
                Some(event) = rx.recv() => {
                    Self::track_live_state(&mut active_live_state, &event);
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
                        // Signal cancellation to any running subagents.
                        cancel_token.store(true, std::sync::atomic::Ordering::Relaxed);
                        // Drain remaining live events before returning so that
                        // any pending StateRemove events are processed.
                        while let Ok(event) = rx.try_recv() {
                            Self::track_live_state(&mut active_live_state, &event);
                            self.apply_tool_live_event(event);
                        }
                        // Clean up any stale live state entries for cancelled
                        // tools. The child process may have been killed before
                        // it could emit its own StateRemove events.
                        for (source, sub_key) in active_live_state {
                            self.apply_tool_live_event(ToolLiveEvent::StateRemove {
                                source,
                                sub_key,
                            });
                        }
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
                            autocomplete: None,
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
        let auto_approved = self.tools.allows_call(self.approval_mode, &call);

        if !auto_approved {
            let _pending = self.prompt_and_wait(&call, term)?;
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
                        self.shown_tool_rows.insert(call.id.clone());
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
            self.timer_pause();
            let decision = self.prompt_and_wait(display_call, term)?;
            self.timer_resume();
            match decision {
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
        let turn_start = self.turn_start;
        let turn_paused_duration = self.turn_paused_duration;
        let turn_pause_start = self.turn_pause_start;
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
                            crate::tools::ApprovalMode::Safe => "safe",
                        };
                        custom_configs.set_value("general", "approval_mode", mode.to_string());
                    }
                    let elapsed = turn_start.map(|start| {
                        let mut e = start.elapsed();
                        if turn_pause_start.is_none() {
                            e = e.saturating_sub(turn_paused_duration);
                        } else {
                            e = e.saturating_sub(turn_paused_duration);
                        }
                        let s = e.as_secs();
                        format!("{}:{:02}", s / 60, s % 60)
                    });                    let visible_pages = if *panes_visible { pages.as_slice() } else { &[] };
                    let hint = pane_toggle_hint(*panes_visible, !pages.is_empty());
                    renderer
                        .ensure_viewport_height(term, input, None, visible_pages, *active_page, None)
                        .ok();
                    term.draw(|frame| {
                        renderer.draw_bottom_pane_with_tick(frame, &PaneDraw {
                            input,
                            status_info: &StatusInfo {
                                model: model.clone(),
                                token_stats: token_stats.clone(),
                                streaming_completion_tokens: None,
                                status_show: user_config.status_show.clone(),
                                streaming: true,
                                approval_mode: *approval_mode,
                                queue_len: queue.len(),
                                elapsed,
                            },
                            pages: visible_pages,
                            active_page: *active_page,
                            pane_toggle_hint: hint,
                            autocomplete: None,
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
}
