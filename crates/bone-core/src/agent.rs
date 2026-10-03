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
    MessageCompleted, MessageCompletedParams, MessageDelta, MessageDeltaParams, ToolFinished,
    ToolFinishedParams, ToolStarted, ToolStartedParams, TurnFinished, TurnFinishedParams,
    TurnStarted, TurnStartedParams,
};
use bone_proto::types::{ChatMessage, DeltaKind, ToolCall, TurnId, TurnOutcome, Usage};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::Inner;
use crate::config::CoreConfig;
use crate::provider::{CompletionRequest, Delta};
use crate::runtime::Runtime;
use crate::session::SessionHandle;
use crate::tools::{ToolContext, ToolSpec, parse_args};

/// After cancellation, how long a running tool gets to clean up (e.g. kill
/// its process group) before its future is dropped.
const TOOL_CANCEL_GRACE: Duration = Duration::from_millis(500);

const DEFAULT_SYSTEM_PROMPT: &str = "You are bone, a coding assistant working in the \
user's terminal. Use the tools to inspect files, make changes and run commands. Read files \
before editing them. Keep changes minimal and focused, and keep answers concise.";

const CANCELLED: &str = "Cancelled by the user before this tool call ran.";

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
    let outcome = match turn.drive(text).await {
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
    let ev = json!({ "session_id": turn.session_id, "turn_id": turn_id, "outcome": outcome });
    let _ = turn.hooks("turn_end", ev).await;
    inner.emit::<TurnFinished>(TurnFinishedParams {
        session_id: turn.session_id,
        turn_id,
        outcome,
    });
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

    /// A hook point that stops the turn when refused.
    async fn turn_hooks(&self, name: &str, event: Value) -> Result<Option<Value>, Stop> {
        self.hooks(name, event).await.map_err(|r| match r {
            Refused::Denied(why) => Stop::Failed(why),
            Refused::Cancelled => Stop::Cancelled,
        })
    }

    async fn drive(&self, text: String) -> Result<(), Stop> {
        let ev = json!({ "session_id": self.session_id, "cwd": self.cwd.to_string_lossy(), "text": text });
        let text = match self.safe_hooks("turn_start", ev).await? {
            Some(ev) => ev["text"].as_str().map(str::to_owned).unwrap_or(text),
            None => text,
        };
        self.record(ChatMessage::User {
            content: text.clone(),
        })?;
        self.inner.emit::<TurnStarted>(TurnStartedParams {
            session_id: self.session_id.clone(),
            turn_id: self.turn_id,
            text,
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
        loop {
            let mut messages = vec![ChatMessage::System {
                content: system.clone(),
            }];
            messages.extend(self.session.lock().unwrap().messages.iter().cloned());
            let ev = json!({ "session_id": self.session_id, "messages": messages });
            if let Some(ev) = self.safe_hooks("context", ev).await? {
                messages = serde_json::from_value(list(&ev["messages"])).map_err(|e| {
                    Stop::Failed(format!("context hook returned bad messages: {e}"))
                })?;
            }
            let mut tools: Vec<ToolSpec> = self.rt.tools.specs().to_vec();
            let ev = json!({
                "session_id": self.session_id,
                "messages": messages,
                "tools": tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "parameters": t.parameters })).collect::<Vec<_>>(),
            });
            if let Some(ev) = self.safe_hooks("request", ev).await? {
                messages = serde_json::from_value(list(&ev["messages"])).map_err(|e| {
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

            let mut attempt = 0;
            // A request_error hook may move the rest of this call elsewhere.
            let mut provider = self.rt.provider.clone();
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
                        messages: &messages,
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
                    // Nothing streamed yet: `request_error` hooks may retry.
                    Some(Err(e))
                        if text.is_empty() && reasoning.is_empty() && attempt <= MAX_RETRIES =>
                    {
                        let ev = json!({
                            "session_id": self.session_id,
                            "error": e.0,
                            "attempt": attempt,
                            "model": self.rt.config.provider.model,
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
            self.complete_message(content, reasoning, calls.clone(), completion.usage)?;
            if calls.is_empty() {
                return Ok(());
            }

            for (i, call) in calls.iter().enumerate() {
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
                self.inner.emit::<ToolStarted>(ToolStartedParams {
                    session_id: self.session_id.clone(),
                    turn_id: self.turn_id,
                    call: call.clone(),
                });
                let (output, is_error) = self.run_tool(call).await;
                self.record(ChatMessage::Tool {
                    call_id: call.id.clone(),
                    content: output.clone(),
                    is_error,
                })?;
                self.inner.emit::<ToolFinished>(ToolFinishedParams {
                    session_id: self.session_id.clone(),
                    turn_id: self.turn_id,
                    call_id: call.id.clone(),
                    output,
                    is_error,
                });
            }
            if self.cancel.is_cancelled() {
                return Err(Stop::Cancelled);
            }
        }
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

    /// Run one call. Returns the text for the model and whether it is an error.
    async fn run_tool(&self, call: &ToolCall) -> (String, bool) {
        let Some(tool) = self.rt.tools.get(&call.name) else {
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
        let ev = json!({
            "session_id": self.session_id,
            "cwd": self.cwd.to_string_lossy(),
            "id": call.id,
            "name": call.name,
            "arguments": args,
        });
        match self.hooks("tool_call", ev).await {
            Ok(Some(ev)) => args = ev["arguments"].clone(),
            Ok(None) => {}
            Err(Refused::Denied(why)) => return (why, true),
            Err(Refused::Cancelled) => return (CANCELLED.into(), true),
        }

        let ctx = ToolContext {
            cwd: self.cwd.clone(),
            session_id: self.session_id.clone(),
            cancel: self.cancel.clone(),
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
