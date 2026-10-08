//! Compaction: keeping long sessions inside the model's context without
//! changing them. The older part of a session is summarized by a model and
//! the summary is kept as a session record (`session.rs`); from then on
//! model calls get the summary in place of those messages, while the
//! transcript, the session file and what clients show keep everything.
//! Compacting again folds the earlier summary into the new one.
//!
//! It runs on request (`session/compact`), before a model call estimated
//! above `compact.limit`, and when the model says the context is too long
//! (the call is then retried with the summary). `bone.config.compact`, with
//! settings.json's `compact` over it, configures it ([`CompactConfig`]).

use bone_proto::methods::{SessionCompacted, SessionCompactedParams};
use bone_proto::types::ChatMessage;
use serde_json::Value;

use crate::Inner;
use crate::config::CompactConfig;
use crate::runtime::ModelCall;
use crate::session::{Session, SessionHandle, Summary};
use crate::tools::ToolSpec;

const PROMPT: &str = "Summarize the conversation below for the assistant that will continue it. \
Keep: the user's goals and constraints, decisions made, files and commands involved, what is \
done and what is left. Be concise; use short bullet points.";

const SUMMARY_HEADER: &str = "Summary of the earlier conversation:\n\n";

/// Characters per token until a reply reports its usage.
const CHARS_PER_TOKEN: f64 = 4.0;

/// Most characters of one message (and of one tool result) the summarizer
/// reads; longer ones keep their start and end.
const MESSAGE_CHARS: usize = 4000;
const TOOL_RESULT_CHARS: usize = 2000;

/// How providers say the context is too long.
const OVERFLOW: &[&str] = &[
    "context length",
    "context window",
    "context_length_exceeded",
    "maximum context",
    "too many tokens",
    "prompt is too long",
];

/// The message standing in for the summarized part of a transcript.
pub(crate) fn summary_message(text: &str) -> ChatMessage {
    ChatMessage::User {
        content: format!("{SUMMARY_HEADER}{text}"),
    }
}

/// Whether a provider error says the context is too long.
pub(crate) fn is_overflow(error: &str) -> bool {
    let e = error.to_lowercase();
    OVERFLOW.iter().any(|p| e.contains(p))
}

/// Characters a model call sends for these messages.
pub(crate) fn chars(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| match m {
            ChatMessage::System { content } | ChatMessage::User { content } => content.len(),
            ChatMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                content.len()
                    + tool_calls
                        .iter()
                        .map(|c| c.name.len() + c.arguments.len())
                        .sum::<usize>()
            }
            ChatMessage::Tool { content, .. } => content.len(),
        })
        .sum()
}

/// Characters a model call sends for these tool definitions.
pub(crate) fn tool_chars(tools: &[ToolSpec]) -> usize {
    tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.parameters.to_string().len())
        .sum()
}

/// Estimated tokens for a model call with these messages in this session,
/// tool definitions included.
pub(crate) fn tokens(s: &Session, messages: &[ChatMessage]) -> u64 {
    ((chars(messages) + s.tool_chars) as f64 / s.chars_per_token.unwrap_or(CHARS_PER_TOKEN)).round()
        as u64
}

/// What one compaction would summarize: the earlier summary (if any) and
/// the messages after it, up to `cut`, the start of the last `keep` user
/// turns (so no tool call is separated from its result).
struct Plan {
    cut: usize,
    input: Vec<ChatMessage>,
    /// Transcript messages newly summarized.
    messages: usize,
}

fn plan(s: &Session, keep: usize) -> Option<Plan> {
    let from = s.summary.as_ref().map_or(0, |sum| sum.through);
    let users: Vec<usize> = (from..s.messages.len())
        .filter(|&i| matches!(s.messages[i], ChatMessage::User { .. }))
        .collect();
    if users.len() <= keep {
        return None;
    }
    let cut = match keep {
        0 => s.messages.len(),
        k => users[users.len() - k],
    };
    let mut input: Vec<ChatMessage> = s
        .summary
        .iter()
        .map(|sum| summary_message(&sum.text))
        .collect();
    input.extend(s.messages[from..cut].iter().cloned());
    Some(Plan {
        cut,
        input,
        messages: cut - from,
    })
}

pub(crate) fn can_compact(s: &Session, keep: usize) -> bool {
    plan(s, keep).is_some()
}

/// `text` cut to about `max` bytes, keeping its start and end.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut head = max * 3 / 4;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - max / 4;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{} […] {}", &text[..head], &text[tail..])
}

/// The conversation as text for the summarizer.
fn render(messages: &[ChatMessage]) -> String {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        match m {
            ChatMessage::System { .. } => {}
            ChatMessage::User { content } => {
                out.push(format!("USER: {}", clip(content, MESSAGE_CHARS)));
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut line = format!("ASSISTANT: {}", clip(content, MESSAGE_CHARS));
                if !tool_calls.is_empty() {
                    let calls: Vec<String> = tool_calls
                        .iter()
                        .map(|c| format!("{} {}", c.name, clip(&c.arguments, MESSAGE_CHARS)))
                        .collect();
                    line.push_str(&format!(" [called {}]", calls.join("; ")));
                }
                out.push(line);
            }
            ChatMessage::Tool { content, .. } => {
                out.push(format!("TOOL RESULT: {}", clip(content, TOOL_RESULT_CHARS)));
            }
        }
    }
    out.join("\n")
}

/// Clears a session's `compacting` flag however the compaction ends
/// (including a cancelled turn dropping it).
struct Compacting<'a>(&'a SessionHandle);

impl Drop for Compacting<'_> {
    fn drop(&mut self) {
        self.0.lock().unwrap().compacting = false;
    }
}

impl Inner {
    /// `bone.config.compact` with settings.json's `compact` over it, field
    /// by field. Bad settings are reported and left out.
    pub(crate) fn compact_config(&self) -> CompactConfig {
        let base = self.runtime().config.compact.clone();
        let saved = match &*self.settings.lock().unwrap() {
            Ok(all) => all.get("compact").cloned(),
            Err(_) => None,
        };
        let Some(Value::Object(fields)) = saved else {
            return base;
        };
        let mut merged = serde_json::to_value(&base).unwrap_or_default();
        for (k, v) in fields {
            merged[k] = v;
        }
        let mut cfg = serde_json::from_value(merged).unwrap_or_else(|e| {
            eprintln!("bone: settings.json compact: {e}");
            base
        });
        // 0 and "" (as a settings page saves "none") mean none.
        cfg.limit = cfg.limit.filter(|&l| l > 0);
        cfg.provider = cfg.provider.filter(|p| !p.trim().is_empty());
        cfg.prompt = cfg.prompt.filter(|p| !p.trim().is_empty());
        cfg
    }

    /// Summarize the older part of a session for the model and announce
    /// it. `reason` is how it came about (see `SessionCompactedParams`).
    pub(crate) async fn compact(
        &self,
        session: &SessionHandle,
        reason: &str,
    ) -> Result<SessionCompactedParams, String> {
        let cfg = self.compact_config();
        let (session_id, plan, generation) = {
            let mut s = session.lock().unwrap();
            if s.compacting {
                return Err("this session is already being compacted".into());
            }
            let plan = plan(&s, cfg.keep).ok_or("nothing to compact yet")?;
            s.compacting = true;
            (s.info.session_id.clone(), plan, s.generation)
        };
        let _busy = Compacting(session);
        let call = ModelCall {
            provider: cfg.provider.clone(),
            options: Value::Null,
            messages: vec![
                ChatMessage::System {
                    content: cfg.prompt.clone().unwrap_or_else(|| PROMPT.into()),
                },
                ChatMessage::User {
                    content: render(&plan.input),
                },
            ],
            tools: Vec::new(),
            depth: 0,
            session_id: Some(session_id.clone()),
            source: Some("compact"),
        };
        let reply = self
            .model_call(call, &mut |_| {})
            .await
            .map_err(|e| format!("the summary failed: {e}"))?;
        let text = reply.content.trim();
        if text.is_empty() {
            return Err("the summary came back empty".into());
        }

        let mut s = session.lock().unwrap();
        // Positions only move when bone.session.compact rewrites the
        // transcript; then this summary no longer fits it.
        if s.generation != generation {
            return Err("the transcript was replaced while compacting".into());
        }
        let before = tokens(&s, &s.context());
        s.summarize(Some(Summary {
            through: plan.cut,
            text: text.to_owned(),
        }))
        .map_err(|e| format!("cannot save session: {e}"))?;
        let done = SessionCompactedParams {
            session_id,
            messages: plan.messages,
            tokens_before: before,
            tokens_after: tokens(&s, &s.context()),
            reason: reason.to_owned(),
        };
        drop(s);
        self.emit::<SessionCompacted>(done.clone());
        Ok(done)
    }

    /// Drop a session's summary: the model gets the whole transcript again.
    pub(crate) fn clear_compaction(
        &self,
        session: &SessionHandle,
    ) -> Result<SessionCompactedParams, String> {
        let mut s = session.lock().unwrap();
        if s.summary.is_none() {
            return Err("this session is not compacted".into());
        }
        if s.compacting {
            return Err("this session is being compacted".into());
        }
        let before = tokens(&s, &s.context());
        s.summarize(None)
            .map_err(|e| format!("cannot save session: {e}"))?;
        let done = SessionCompactedParams {
            session_id: s.info.session_id.clone(),
            messages: 0,
            tokens_before: before,
            tokens_after: tokens(&s, &s.context()),
            reason: "clear".into(),
        };
        drop(s);
        self.emit::<SessionCompacted>(done.clone());
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_errors_are_recognized() {
        assert!(is_overflow(
            "HTTP 400: This model's maximum context length is 8192 tokens"
        ));
        assert!(is_overflow("prompt is too long: 210000 tokens > 200000"));
        assert!(!is_overflow("HTTP 429: rate limited"));
    }

    #[test]
    fn clip_keeps_both_ends_on_char_boundaries() {
        let text = format!("{}é{}", "a".repeat(10), "b".repeat(10));
        let out = clip(&text, 8);
        assert!(out.starts_with("aaaaaa") && out.ends_with("bb"), "{out}");
        assert_eq!(clip("short", 8), "short");
    }
}
