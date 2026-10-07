//! The agent loop: stream a completion, run its tool calls, repeat until the
//! model answers without tools.
//!
//! Core-side Lua hooks (`bone.hook`) run at each step: `turn_start`,
//! `system`, `context`, `request`, `request_error`, `message`, `tool_call`,
//! `tool_result` and `turn_end`, plus `stream` (watching output) and
//! `session_start`. A hook can change the step's data or refuse it, and may
//! wait on the user.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bone_proto::methods::{
    MessageCompleted, MessageCompletedParams, MessageDelta, MessageDeltaParams, ProcessChanged,
    ProcessChangedParams, ProcessChunk, SessionCompactFailed, SessionCompactFailedParams,
    ToolFinished, ToolFinishedParams, ToolOutput, ToolOutputParams, ToolStarted, ToolStartedParams,
    TurnFinished, TurnFinishedParams, TurnStarted, TurnStartedParams, TurnSteered,
};
use bone_proto::types::{ChatMessage, DeltaKind, ToolCall, TurnId, TurnOutcome, Usage};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::Inner;
use crate::config::CoreConfig;
use crate::provider::{CompletionRequest, Delta};
use crate::runtime::Runtime;
use crate::session::{SessionHandle, UsageRecord};
use crate::tools::{ProcessView, ToolContext, ToolSpec, parse_args};

/// After cancellation, how long a running tool gets to clean up (e.g. kill
/// its process group) before its future is dropped.
const TOOL_CANCEL_GRACE: Duration = Duration::from_millis(500);
/// Grace for cleanup hooks on cancelled turns only.
const TURN_END_CANCEL_GRACE: Duration = Duration::from_secs(1);

const DEFAULT_SYSTEM_PROMPT: &str = "You are bone, a coding assistant working in the \
user's terminal. Use the tools to inspect files, make changes and run commands. Search with rg \
before reading large files, then use read_file offset/limit for focused sections. Read files before \
editing them; group independent hunks in one edit_file call. Keep changes minimal and focused, \
and keep answers concise.\n\n\
If the user wants to change or customize bone itself, first read ~/.bone/docs/customizing.md.";

const CANCELLED: &str = "Cancelled by the user before this tool call ran.";

/// How long a turn waits for MCP servers that are still starting.
const MCP_WAIT: Duration = Duration::from_secs(10);
/// Most tool calls that run at the same time.
const MAX_PARALLEL: usize = 8;
/// Retries `request_error` hooks may ask for, per model call.
const MAX_RETRIES: u32 = 5;
/// How long `stream` hooks' input collects before they run.
const STREAM_BATCH: Duration = Duration::from_millis(50);

enum Stop {
    Cancelled,
    Failed(String),
}

/// Why a hook point did not go ahead.
enum Refused {
    Denied(String),
    Cancelled,
}

struct Turn<'a> {
    inner: &'a Inner,
    /// The configuration this turn runs with, start to end.
    rt: Arc<Runtime>,
    session: &'a SessionHandle,
    session_id: String,
    turn_id: TurnId,
    cwd: PathBuf,
    cancel: &'a CancellationToken,
    /// Output for `stream` hooks, when there are any.
    stream: Option<tokio::sync::mpsc::UnboundedSender<(DeltaKind, String)>>,
}

pub(crate) async fn run_turn(
    inner: Arc<Inner>,
    session: SessionHandle,
    turn_id: TurnId,
    text: String,
    images: Vec<bone_proto::types::ImageAttachment>,
    cancel: CancellationToken,
) {
    let (session_id, cwd) = {
        let s = session.lock().unwrap();
        (s.info.session_id.clone(), PathBuf::from(&s.info.cwd))
    };
    let rt = inner.runtime();
    let stream = rt
        .scripting
        .clone()
        .filter(|s| s.has_hook("stream"))
        .map(|s| stream_hooks(s, session_id.clone(), turn_id));
    let turn = Turn {
        rt,
        inner: &inner,
        session: &session,
        session_id,
        turn_id,
        cwd,
        cancel: &cancel,
        stream,
    };
    let outcome = match turn.drive(text, images).await {
        Ok(()) => TurnOutcome::Completed,
        Err(Stop::Cancelled) => TurnOutcome::Cancelled,
        Err(Stop::Failed(message)) => TurnOutcome::Failed { message },
    };
    {
        let mut s = session.lock().unwrap();
        if s.active.as_ref().is_some_and(|a| a.turn_id == turn_id) {
            s.active = None;
        }
    }
    let cancelled = matches!(outcome, TurnOutcome::Cancelled);
    let ev = json!({ "session_id": turn.session_id, "turn_id": turn_id, "outcome": outcome });
    // Cleanup hooks must run even when the turn's cancellation token is set.
    // Give cancelled turns a grace period so synchronous cleanup still runs,
    // but an async hook waiting on bone.ask/sleep cannot hold up turn/finished
    // forever. Normal turns retain their unbounded hook semantics.
    if let Some(s) = turn
        .rt
        .scripting
        .as_deref()
        .filter(|s| s.has_hook("turn_end"))
    {
        if cancelled {
            if tokio::time::timeout(TURN_END_CANCEL_GRACE, s.hooks("turn_end", ev))
                .await
                .is_err()
            {
                // Dropping the await alone leaves Lua parked; resume it with nil.
                s.cancel_session(&turn.session_id);
            }
        } else {
            let _ = s.hooks("turn_end", ev).await;
        }
    }
    inner.emit::<TurnFinished>(TurnFinishedParams {
        session_id: turn.session_id,
        turn_id,
        outcome,
    });
    // Then the next queued message, if any.
    inner.after_turn(&session, cancelled);
}

impl Turn<'_> {
    /// Run the hooks for `name`. `Ok(None)` when none are registered;
    /// otherwise the event as the hooks left it.
    async fn hooks(&self, name: &str, event: Value) -> Result<Option<Value>, Refused> {
        let Some(s) = self.rt.scripting.as_deref().filter(|s| s.has_hook(name)) else {
            return Ok(None);
        };
        // Hooks may wait on the user (bone.ask); a cancel drops the question.
        let out = tokio::select! {
            out = s.hooks(name, event) => out,
            _ = self.cancel.cancelled() => {
                s.cancel_session(&self.session_id);
                return Err(Refused::Cancelled);
            }
        };
        match out.deny {
            Some(why) => Err(Refused::Denied(why)),
            None => Ok(Some(out.event)),
        }
    }

    /// [`turn_hooks`](Self::turn_hooks) at a safe point: between model
    /// calls, where Lua may change the transcript (`bone.session`).
    async fn safe_hooks(&self, name: &str, event: Value) -> Result<Option<Value>, Stop> {
        self.session.lock().unwrap().safe_point = true;
        let r = self.turn_hooks(name, event).await;
        self.session.lock().unwrap().safe_point = false;
        r
    }

    /// Compact the session between model calls (see `compact.rs`); whether
    /// it did. Failing to (nothing to compact yet, the summary failed) is
    /// not the turn's failure.
    async fn compact(&self, reason: &str) -> Result<bool, Stop> {
        tokio::select! {
            r = self.inner.compact(self.session, reason) => {
                if let Err(error) = &r {
                    self.inner.emit::<SessionCompactFailed>(SessionCompactFailedParams {
                        session_id: self.session_id.clone(), reason: reason.into(), error: error.clone(),
                    });
                }
                Ok(r.is_ok())
            },
            _ = self.cancel.cancelled() => Err(Stop::Cancelled),
        }
    }

    /// A hook point that stops the turn when refused.
    async fn turn_hooks(&self, name: &str, event: Value) -> Result<Option<Value>, Stop> {
        self.hooks(name, event).await.map_err(|r| match r {
            Refused::Denied(why) => Stop::Failed(why),
            Refused::Cancelled => Stop::Cancelled,
        })
    }

    async fn drive(
        &self,
        text: String,
        mut images: Vec<bone_proto::types::ImageAttachment>,
    ) -> Result<(), Stop> {
        let ev = json!({ "session_id": self.session_id, "cwd": self.cwd.to_string_lossy(), "text": text, "images": images });
        let text = match self.safe_hooks("turn_start", ev).await? {
            Some(ev) => {
                if let Some(v) = ev.get("images") {
                    images =
                        serde_json::from_value(list(v)).map_err(|e| Stop::Failed(e.to_string()))?;
                }
                ev["text"].as_str().map(str::to_owned).unwrap_or(text)
            }
            None => text,
        };
        self.inner
            .validate_input(&self.session.lock().unwrap(), &text, &mut images)
            .map_err(|e| Stop::Failed(e.message))?;
        self.record(ChatMessage::User {
            content: text.clone(),
            images: images.clone(),
        })?;
        if !self.inner.mcp.is_empty() {
            self.inner.mcp.ensure_ready(MCP_WAIT).await;
        }
        self.inner.emit::<TurnStarted>(TurnStartedParams {
            session_id: self.session_id.clone(),
            turn_id: self.turn_id,
            text,
            images,
        });

        let dynamic = match &self.rt.scripting {
            Some(s) if s.has_system_prompt_fn() => {
                let cwd = self.cwd.to_string_lossy();
                Some(
                    s.system_prompt(&cwd, &self.session_id)
                        .await
                        .map_err(Stop::Failed)?,
                )
            }
            _ => None,
        };
        let mut system = system_prompt(&self.rt.config, dynamic.as_deref(), &self.cwd);
        let ev = json!({ "session_id": self.session_id, "cwd": self.cwd.to_string_lossy(), "prompt": system });
        if let Some(ev) = self.safe_hooks("system", ev).await?
            && let Some(p) = ev["prompt"].as_str()
        {
            system = p.to_owned();
        }
        // Compacting after a context overflow, once per model call.
        let mut overflowed = false;
        'call: loop {
            self.take_steer()?;
            let system = ChatMessage::System {
                content: system.clone(),
            };
            let compact = self.inner.compact_config();
            if let Some(limit) = compact.limit {
                // Proactive estimates describe the session's pinned provider,
                // not a one-call request override or a previous retry target.
                let pin = self
                    .session
                    .lock()
                    .unwrap()
                    .model
                    .clone()
                    .unwrap_or_else(|| self.rt.model_of(None, &Value::Null));
                let provider = self.rt.pinned(&pin).map_err(Stop::Failed)?;
                self.session
                    .lock()
                    .unwrap()
                    .set_replays_reasoning(provider.replays_reasoning());
                let over = {
                    let s = self.session.lock().unwrap();
                    let mut all = vec![system.clone()];
                    all.extend(s.context());
                    crate::compact::tokens(&s, &all) > limit
                };
                if over {
                    let can = {
                        let s = self.session.lock().unwrap();
                        crate::compact::can_compact(&s, compact.keep)
                    };
                    if can {
                        self.compact("limit").await?;
                    }
                }
            }
            let mut messages = vec![system];
            messages.extend(self.session.lock().unwrap().context());
            let ev = json!({ "session_id": self.session_id, "messages": messages });
            if let Some(ev) = self.safe_hooks("context", ev).await? {
                messages = messages_from_lua(&ev["messages"]).map_err(|e| {
                    Stop::Failed(format!("context hook returned bad messages: {e}"))
                })?;
            }
            let mut tools: Vec<ToolSpec> = self.rt.tools.specs().to_vec();
            // MCP tools, unless a built-in or Lua tool has the name.
            for t in self.inner.mcp.tool_specs() {
                if !tools.iter().any(|have| have.name == t.name) {
                    tools.push(t);
                }
            }
            let ev = json!({
                "session_id": self.session_id,
                "messages": messages,
                "tools": tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "parameters": t.parameters })).collect::<Vec<_>>(),
            });
            let mut use_provider = None;
            let mut use_model = None;
            if let Some(ev) = self.safe_hooks("request", ev).await? {
                use_provider = ev["provider"].as_str().map(str::to_owned);
                use_model = ev["model"].as_str().map(str::to_owned);
                messages = messages_from_lua(&ev["messages"]).map_err(|e| {
                    Stop::Failed(format!("request hook returned bad messages: {e}"))
                })?;
                tools = list(&ev["tools"])
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|t| ToolSpec {
                                name: t["name"].as_str().unwrap_or_default().to_owned(),
                                description: t["description"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_owned(),
                                parameters: t["parameters"].clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            }

            // Tool definitions count toward the provider's input tokens; left
            // out, a short transcript with many tools looks several times
            // its size once the ratio is learned.
            let tool_chars = crate::compact::tool_chars(&tools);
            self.session.lock().unwrap().tool_chars = tool_chars;
            let mut attempt = 0;
            // A request_error hook may move the rest of this call elsewhere.
            // Which entry and model answered, for the usage record: the
            // session's, which its first turn locks to the default.
            let pinned = self.session.lock().unwrap().model.clone();
            let mut served = pinned.unwrap_or_else(|| {
                let pin = self.rt.model_of(None, &Value::Null);
                let _ = self.session.lock().unwrap().pin(pin.clone());
                pin
            });
            // Request overrides last for this call only. A model alone keeps
            // the session's pinned entry, not the runtime's current default.
            let mut provider = if use_provider.is_some() || use_model.is_some() {
                let options = use_model
                    .as_ref()
                    .map(|model| json!({ "model": model }))
                    .unwrap_or(Value::Null);
                if let Some(name) = use_provider.as_deref() {
                    served = self.rt.model_of(Some(name), &options);
                } else if let Some(model) = use_model {
                    served.1 = model;
                }
                self.rt
                    .provider_for(served.0.as_deref(), &options)
                    .map_err(|why| Stop::Failed(format!("request hook: {why}")))?
            } else {
                self.rt.pinned(&served).map_err(Stop::Failed)?
            };
            let hydrated = self
                .inner
                .attachments
                .hydrate(&messages)
                .map_err(Stop::Failed)?;
            let completion = loop {
                attempt += 1;
                let mut text = String::new();
                let mut reasoning = String::new();
                let result = {
                    let mut on_delta = |d: Delta| {
                        let (kind, chunk) = match d {
                            Delta::Text(t) => {
                                text.push_str(&t);
                                (DeltaKind::Text, t)
                            }
                            Delta::Reasoning(t) => {
                                reasoning.push_str(&t);
                                (DeltaKind::Reasoning, t)
                            }
                        };
                        if let Some(tx) = &self.stream {
                            let _ = tx.send((kind, chunk.clone()));
                        }
                        self.inner.emit::<MessageDelta>(MessageDeltaParams {
                            session_id: self.session_id.clone(),
                            turn_id: self.turn_id,
                            kind,
                            text: chunk,
                        });
                    };
                    let req = CompletionRequest {
                        session_id: &self.session_id,
                        messages: &hydrated,
                        tools: &tools,
                        depth: 0,
                    };
                    tokio::select! {
                        _ = self.cancel.cancelled() => None,
                        r = provider.complete(req, &mut on_delta) => Some(r),
                    }
                };
                match result {
                    Some(Ok(c)) => break c,
                    // Too long for the model: compact, then build the call
                    // again with the summary.
                    Some(Err(e))
                        if text.is_empty()
                            && reasoning.is_empty()
                            && !overflowed
                            && compact.auto
                            && crate::compact::is_overflow(&e.0)
                            && self.compact("overflow").await? =>
                    {
                        overflowed = true;
                        continue 'call;
                    }
                    // Nothing streamed yet: `request_error` hooks may retry.
                    Some(Err(e))
                        if text.is_empty() && reasoning.is_empty() && attempt <= MAX_RETRIES =>
                    {
                        let ev = json!({
                            "session_id": self.session_id,
                            "error": e.0,
                            "attempt": attempt,
                            "model": served.1,
                        });
                        let ev = self.safe_hooks("request_error", ev).await?;
                        let Some(ms) = ev.as_ref().and_then(|ev| ev["retry"].as_u64()) else {
                            return Err(Stop::Failed(e.0));
                        };
                        if let Some(name) = ev.as_ref().and_then(|ev| ev["provider"].as_str()) {
                            provider =
                                self.rt
                                    .provider_for(Some(name), &Value::Null)
                                    .map_err(|why| {
                                        Stop::Failed(format!("request_error hook: {why}"))
                                    })?;
                            served = (
                                Some(name.to_owned()),
                                self.rt
                                    .models
                                    .get(name)
                                    .map(|p| p.model.clone())
                                    .unwrap_or_default(),
                            );
                        }
                        tokio::select! {
                            _ = self.cancel.cancelled() => return Err(Stop::Cancelled),
                            _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
                        }
                    }
                    interrupted => {
                        // Keep what the user already saw stream in.
                        if !text.is_empty() || !reasoning.is_empty() {
                            self.complete_message(text, reasoning, Vec::new(), None)?;
                        }
                        return Err(match interrupted {
                            Some(Err(e)) => Stop::Failed(e.0),
                            _ => Stop::Cancelled,
                        });
                    }
                }
            };

            // Retries may have switched providers. Calibrate with the mode
            // of the provider that actually answered, after all request hooks.
            let replays_reasoning = provider.replays_reasoning();
            let sent_chars = crate::compact::chars(&messages, replays_reasoning) + tool_chars;
            self.session
                .lock()
                .unwrap()
                .set_replays_reasoning(replays_reasoning);
            if let Some(u) = completion.usage {
                let context_tokens = u.context_tokens.unwrap_or(u.input_tokens);
                if context_tokens > 0 {
                    self.session.lock().unwrap().chars_per_token =
                        Some(sent_chars as f64 / context_tokens as f64);
                }
                let record = UsageRecord {
                    turn_id: self.turn_id,
                    provider: served.0.clone(),
                    model: served.1.clone(),
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    context_tokens: u.context_tokens,
                    cached_tokens: u.cached_tokens,
                    source: None,
                };
                // Losing a usage record is not worth failing the turn.
                let _ = self.session.lock().unwrap().usage(record);
            }
            let (mut content, mut reasoning, mut calls) = (
                completion.content,
                completion.reasoning,
                completion.tool_calls,
            );
            let ev = json!({ "session_id": self.session_id, "content": content, "reasoning": reasoning, "tool_calls": calls, "usage": completion.usage });
            if let Some(ev) = self.turn_hooks("message", ev).await? {
                content = ev["content"].as_str().unwrap_or_default().to_owned();
                reasoning = ev["reasoning"].as_str().unwrap_or_default().to_owned();
                calls = serde_json::from_value(list(&ev["tool_calls"])).map_err(|e| {
                    Stop::Failed(format!("message hook returned bad tool_calls: {e}"))
                })?;
            }
            overflowed = false;
            self.complete_message(content, reasoning, calls.clone(), completion.usage)?;
            if calls.is_empty() {
                let compact = self.inner.compact_config();
                if let Some(limit) = compact.limit {
                    let can = {
                        let s = self.session.lock().unwrap();
                        let context = s.context();
                        crate::compact::tokens(&s, &context) > limit
                            && crate::compact::can_compact(&s, compact.keep)
                    };
                    if can {
                        self.compact("limit").await?;
                    }
                }
                // A message steered in meanwhile gets an answer before the
                // turn ends; otherwise no more are accepted.
                let mut s = self.session.lock().unwrap();
                if s.has_steer() {
                    continue;
                }
                if let Some(a) = &mut s.active {
                    a.closing = true;
                }
                return Ok(());
            }

            let mut i = 0;
            while i < calls.len() {
                if self.cancel.is_cancelled() {
                    for skipped in &calls[i..] {
                        self.record(ChatMessage::Tool {
                            call_id: skipped.id.clone(),
                            content: CANCELLED.into(),
                            is_error: true,
                        })?;
                    }
                    return Err(Stop::Cancelled);
                }
                // A run of calls that only read goes together; any other
                // call runs on its own, in order.
                let mut end = i + 1;
                if self.rt.config.parallel_tools && self.parallel(&calls[i]) {
                    while end < calls.len() && end - i < MAX_PARALLEL && self.parallel(&calls[end])
                    {
                        end += 1;
                    }
                }
                let batch = &calls[i..end];
                for call in batch {
                    self.inner.emit::<ToolStarted>(ToolStartedParams {
                        session_id: self.session_id.clone(),
                        turn_id: self.turn_id,
                        call: call.clone(),
                        started_at: Some(unix_ms()),
                    });
                }
                let results = futures_util::future::join_all(batch.iter().map(|call| async move {
                    let began = std::time::Instant::now();
                    let (output, is_error) = self.run_tool(call).await;
                    self.inner.emit::<ToolFinished>(ToolFinishedParams {
                        session_id: self.session_id.clone(),
                        turn_id: self.turn_id,
                        call_id: call.id.clone(),
                        output: output.clone(),
                        is_error,
                        duration_ms: Some(began.elapsed().as_millis() as u64),
                    });
                    (output, is_error)
                }))
                .await;
                // The transcript keeps the calls' order.
                for (call, (output, is_error)) in batch.iter().zip(results) {
                    self.record(ChatMessage::Tool {
                        call_id: call.id.clone(),
                        content: output,
                        is_error,
                    })?;
                }
                i = end;
            }
            if self.cancel.is_cancelled() {
                return Err(Stop::Cancelled);
            }
        }
    }

    /// Add the waiting `turn/steer` messages to the transcript.
    fn take_steer(&self) -> Result<(), Stop> {
        let queued = {
            let mut s = self.session.lock().unwrap();
            let steer = s.take_steer();
            if !steer.is_empty() {
                self.inner.emit_queue(&s, None);
            }
            steer
        };
        for q in queued {
            self.record(ChatMessage::User {
                content: q.text.clone(),
                images: q.images.clone(),
            })?;
            self.inner.emit::<TurnSteered>(TurnStartedParams {
                session_id: self.session_id.clone(),
                turn_id: self.turn_id,
                text: q.text,
                images: q.images,
            });
        }
        Ok(())
    }

    fn complete_message(
        &self,
        content: String,
        reasoning: String,
        tool_calls: Vec<ToolCall>,
        usage: Option<Usage>,
    ) -> Result<(), Stop> {
        let message = ChatMessage::Assistant {
            content,
            reasoning,
            tool_calls,
        };
        self.record(message.clone())?;
        self.inner.emit::<MessageCompleted>(MessageCompletedParams {
            session_id: self.session_id.clone(),
            turn_id: self.turn_id,
            message,
            usage,
        });
        Ok(())
    }

    fn record(&self, msg: ChatMessage) -> Result<(), Stop> {
        self.session
            .lock()
            .unwrap()
            .push(msg)
            .map_err(|e| Stop::Failed(format!("cannot save session: {e}")))
    }

    /// Whether a call may run alongside others: its tool only reads.
    fn parallel(&self, call: &ToolCall) -> bool {
        match self.rt.tools.get(&call.name) {
            Some(t) => t.parallel(),
            None => self
                .inner
                .mcp
                .tool(&call.name)
                .is_some_and(|t| t.parallel()),
        }
    }

    /// Run one call. Returns the text for the model and whether it is an error.
    async fn run_tool(&self, call: &ToolCall) -> (String, bool) {
        let tool = self
            .rt
            .tools
            .get(&call.name)
            .cloned()
            .or_else(|| self.inner.mcp.tool(&call.name));
        let Some(tool) = tool else {
            let names: Vec<_> = self
                .rt
                .tools
                .specs()
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            return (
                format!(
                    "Unknown tool {:?}. Available: {}",
                    call.name,
                    names.join(", ")
                ),
                true,
            );
        };
        let mut args = match parse_args(&call.arguments) {
            Ok(a) => a,
            Err(e) => return (e, true),
        };
        let mut ev = json!({
            "session_id": self.session_id,
            "cwd": self.cwd.to_string_lossy(),
            "id": call.id,
            "name": call.name,
            "arguments": args,
        });
        if self.rt.tools.get(&call.name).is_none()
            && let Some(mcp) = self.inner.mcp.describe(&call.name)
        {
            ev["mcp"] = mcp;
        }
        match self.hooks("tool_call", ev).await {
            Ok(Some(ev)) => args = ev["arguments"].clone(),
            Ok(None) => {}
            Err(Refused::Denied(why)) => return (why, true),
            Err(Refused::Cancelled) => return (CANCELLED.into(), true),
        }

        let output: crate::tools::OutputSink = {
            let events = self.inner.events.clone();
            let (session_id, turn_id, call_id) =
                (self.session_id.clone(), self.turn_id, call.id.clone());
            Arc::new(move |text: &str| {
                // No subscribers is not an error.
                let _ = events.send(crate::Event::new::<ToolOutput>(ToolOutputParams {
                    session_id: session_id.clone(),
                    turn_id,
                    call_id: call_id.clone(),
                    text: text.to_owned(),
                }));
            })
        };
        let processes = {
            let events = self.inner.events.clone();
            let session_id = self.session_id.clone();
            Arc::new(move |mut view: ProcessView, version: u64| {
                let chunk = view
                    .chunk
                    .take()
                    .map(|(offset, data)| ProcessChunk { offset, data });
                let _ = events.send(crate::Event::new::<ProcessChanged>(ProcessChangedParams {
                    session_id: session_id.clone(),
                    version,
                    process: crate::process_snapshot(&session_id, view),
                    chunk,
                }));
            })
        };
        let ctx = ToolContext {
            cwd: self.cwd.clone(),
            session_id: self.session_id.clone(),
            call_id: call.id.clone(),
            cancel: self.cancel.clone(),
            jobs: self.inner.jobs.clone(),
            output: Some(output),
            processes: Some(processes),
        };
        let hook_args = args.clone();
        let grace = async {
            self.cancel.cancelled().await;
            tokio::time::sleep(TOOL_CANCEL_GRACE).await;
        };
        let (output, is_error) = tokio::select! {
            biased;
            r = tool.call(args, &ctx) => match r {
                Ok(out) => (out, false),
                Err(out) => (out, true),
            },
            _ = grace => {
                if let Some(s) = self.rt.scripting.as_deref() {
                    s.cancel_session(&self.session_id);
                }
                return ("Cancelled by the user while running.".into(), true);
            }
        };
        let ev = json!({
            "session_id": self.session_id,
            "id": call.id,
            "name": call.name,
            "arguments": hook_args,
            "output": output,
            "is_error": is_error,
        });
        match self.hooks("tool_result", ev).await {
            Ok(Some(ev)) => (
                ev["output"].as_str().map(str::to_owned).unwrap_or(output),
                ev["is_error"].as_bool().unwrap_or(is_error),
            ),
            Ok(None) | Err(Refused::Cancelled) => (output, is_error),
            Err(Refused::Denied(why)) => (why, true),
        }
    }
}

/// Feed `stream` hooks: output is collected for a moment and handed over in
/// order, one batch at a time, without ever holding up the stream itself.
fn stream_hooks(
    scripting: Arc<crate::scripting::Scripting>,
    session_id: String,
    turn_id: TurnId,
) -> tokio::sync::mpsc::UnboundedSender<(DeltaKind, String)> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(DeltaKind, String)>();
    tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            let (mut text, mut reasoning) = (String::new(), String::new());
            let mut add = |(kind, chunk): (DeltaKind, String)| match kind {
                DeltaKind::Text => text.push_str(&chunk),
                DeltaKind::Reasoning => reasoning.push_str(&chunk),
            };
            add(first);
            tokio::time::sleep(STREAM_BATCH).await;
            while let Ok(d) = rx.try_recv() {
                add(d);
            }
            let ev = json!({ "session_id": session_id, "turn_id": turn_id, "text": text, "reasoning": reasoning });
            let _ = scripting.hooks("stream", ev).await;
        }
    });
    tx
}

/// Lua has one kind of table: an empty list comes back from a hook as `{}`.
pub(crate) fn list(v: &Value) -> Value {
    match v {
        Value::Object(o) if o.is_empty() => Value::Array(Vec::new()),
        v => v.clone(),
    }
}

fn system_prompt(config: &CoreConfig, dynamic: Option<&str>, cwd: &Path) -> String {
    let base = dynamic
        .or(config.system_prompt.as_deref())
        .unwrap_or(DEFAULT_SYSTEM_PROMPT);
    format!("{base}\n\nWorking directory: {cwd}", cwd = cwd.display())
}

/// Milliseconds since the Unix epoch.
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn messages_from_lua(v: &Value) -> Result<Vec<ChatMessage>, String> {
    list(v)
        .as_array()
        .ok_or("messages must be a list")?
        .iter()
        .map(crate::runtime::message)
        .collect()
}
