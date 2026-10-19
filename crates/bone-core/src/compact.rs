//! Compaction: keeping long sessions inside the model's context without
//! changing them. The older part of a session is summarized by a model and
//! the summary is kept as a session record (`session.rs`); from then on
//! model calls get the summary in place of those messages, while the
//! transcript, the session file and what clients show keep everything.
//! Compacting again folds the earlier summary into the new one.
//!
//! The summary is a paraphrase, so each model call also gets a handoff
//! rebuilt from the transcript at the cut: the request in progress, the
//! last step, and whether newer turns follow word for word. That is what
//! tells the assistant what the work is, what the last rounds found, and
//! what is left, instead of treating the summary as a new request.
//!
//! It runs on request (`session/compact`), before a model call estimated
//! above `compact.limit`, and when the model says the context is too long
//! (the call is then retried with the summary). `bone.config.compact`, with
//! settings.json's `compact` over it, configures it ([`CompactConfig`]).

use bone_proto::methods::{SessionCompacted, SessionCompactedParams};
use bone_proto::types::{ChatMessage, ToolCall};
use serde_json::Value;

use crate::Inner;
use crate::config::CompactConfig;
use crate::runtime::ModelCall;
use crate::session::{Session, SessionHandle, Summary};
use crate::tools::ToolSpec;

const PROMPT: &str = "Summarize the conversation below as a handoff for the assistant that will continue it. \
The assistant will not see these messages, only your summary, then a short excerpt of the last rounds \
and any newer turns kept word for word. Your summary is the only record of everything older.\n\
- Working on: the task underway and the constraints that still apply. Quote the latest user request.\n\
- Learned: what the last rounds established. Findings, decisions, errors, and the paths or commands they came from. \
Not which tools were called. Quote the important bits exactly. If a result was cut with […], say it was truncated.\n\
- Left: what is not done yet and the immediate next step. Note side effects that already happened so they are not repeated.\n\
Do not invent.";

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

/// The previous summary as the next summarizer reads it. Not what the
/// continuing assistant sees: that handoff is rebuilt from the transcript.
pub(crate) fn summary_message(text: &str) -> ChatMessage {
    ChatMessage::User {
        content: format!("{SUMMARY_HEADER}{text}"),
        images: Vec::new(),
    }
}

/// What a model call gets in place of `covered`. The handoff is rebuilt on
/// each call from the transcript at the cut, not stored in the summary, so
/// a later compaction cannot paraphrase it away.
pub(crate) fn context_message(text: &str, covered: &[ChatMessage], kept: bool) -> ChatMessage {
    ChatMessage::User {
        content: format!("{SUMMARY_HEADER}{text}{}", handoff(covered, kept)),
        images: Vec::new(),
    }
}

/// How much of the cut to quote. The summary holds the older story; this
/// only has to say what the work is, what the last rounds found, and what
/// is left. Tool names are not part of that.
const REQUEST_CHARS: usize = 1500;
const LEARNED_CHARS: usize = 500;
const LEFT_CHARS: usize = 600;
const LEARNED_ROUNDS: usize = 4;
/// A shorter reply than this, next to a tool result, is a preamble ("Looking.")
/// rather than what was found.
const STUB_CHARS: usize = 80;

fn handoff(covered: &[ChatMessage], kept: bool) -> String {
    let mut out = String::from("\n\n---\n");
    if kept {
        out.push_str(
            "Handoff, not a new request. The messages after this one are the newest turns, \
unchanged, and they are the work to continue. The summary is older background. \
Do not start that work over, and do not repeat a side effect that already happened. \
Reading a file again before editing it is fine.\n\n\
Where the summarized part left off:\n",
        );
    } else {
        out.push_str(
            "Handoff, not a new request. No later turns were kept, so resume from the state below. \
Do not start over, and do not repeat a side effect that already happened \
(an edit, a command that changed something, a message already sent). \
Tool output is an excerpt: read a file again before editing it.\n\n\
In progress:\n",
        );
    }
    match covered
        .iter()
        .rposition(|m| matches!(m, ChatMessage::User { .. }))
    {
        None => out.push_str("No user request was in the summarized messages.\n"),
        Some(i) => {
            if kept {
                out.push_str("Before that, work had got to:\n");
            } else {
                out.push_str("Working on:\n");
            }
            out.push_str(&request_text(&covered[i]));
            out.push('\n');
            write_turn(&covered[i + 1..], kept, &mut out);
        }
    }
    out
}

fn request_text(message: &ChatMessage) -> String {
    let ChatMessage::User { content, images } = message else {
        return String::new();
    };
    let mut text = clip(content.trim(), REQUEST_CHARS);
    if text.is_empty() {
        text = "(no text)".into();
    }
    for image in images {
        text.push_str(&format!(
            " [Image: {} {}×{}]",
            image.name, image.width, image.height
        ));
    }
    text
}

struct Step<'a> {
    content: &'a str,
    reasoning: &'a str,
    calls: &'a [ToolCall],
    results: Vec<&'a ChatMessage>,
}

fn turn_steps(turn: &[ChatMessage]) -> Vec<Step<'_>> {
    let mut steps = Vec::new();
    let mut i = 0;
    while i < turn.len() {
        let ChatMessage::Assistant {
            content,
            reasoning,
            tool_calls,
            ..
        } = &turn[i]
        else {
            i += 1;
            continue;
        };
        i += 1;
        let start = i;
        while i < turn.len() && matches!(turn[i], ChatMessage::Tool { .. }) {
            i += 1;
        }
        steps.push(Step {
            content,
            reasoning,
            calls: tool_calls,
            results: turn[start..i].iter().collect(),
        });
    }
    steps
}

fn write_turn(turn: &[ChatMessage], kept: bool, out: &mut String) {
    let steps = turn_steps(turn);
    let Some(last) = steps.last() else {
        out.push_str("Learned:\n- nothing yet\nLeft:\n");
        out.push_str(if kept {
            "The messages after this one.\n"
        } else {
            "Answer the request. Nothing has been done yet.\n"
        });
        return;
    };
    write_learned(&steps, out);
    write_left(last, kept, out);
}

fn write_learned(steps: &[Step<'_>], out: &mut String) {
    let start = steps.len().saturating_sub(LEARNED_ROUNDS);
    if start > 0 {
        out.push_str("Learned (earlier rounds omitted):\n");
    } else {
        out.push_str("Learned:\n");
    }
    let mut any = false;
    for step in &steps[start..] {
        for line in round_findings(step) {
            any = true;
            out.push_str("- ");
            out.push_str(&line);
            out.push('\n');
        }
    }
    if !any {
        out.push_str("- nothing recorded\n");
    }
}

/// What a round established: the assistant's conclusion, or, when it only
/// called tools, the result itself. The tool name is not the finding.
fn round_findings(step: &Step<'_>) -> Vec<String> {
    let said = step.content.trim();
    let substantive = !said.is_empty() && (said.len() >= STUB_CHARS || step.results.is_empty());
    let mut lines = Vec::new();
    if substantive {
        lines.push(flatten_clip(said, LEARNED_CHARS));
    }
    for result in &step.results {
        let ChatMessage::Tool {
            call_id,
            content,
            is_error,
            images,
            ..
        } = result
        else {
            continue;
        };
        if substantive && !is_error {
            continue;
        }
        let call = step.calls.iter().find(|call| call.id == *call_id);
        lines.push(format_finding(call, content, *is_error, !images.is_empty()));
    }
    if lines.is_empty() && !said.is_empty() {
        lines.push(flatten_clip(said, LEARNED_CHARS));
    }
    lines
}

fn format_finding(call: Option<&ToolCall>, content: &str, is_error: bool, image: bool) -> String {
    let body = if is_error {
        flatten_clip_tail(content, LEARNED_CHARS)
    } else {
        flatten_clip(content, LEARNED_CHARS)
    };
    let mut line = String::new();
    if let Some(subject) = call.and_then(subject) {
        line.push_str(&subject);
        line.push_str(": ");
    }
    if is_error {
        line.push_str("failed: ");
    }
    if body.is_empty() {
        line.push_str(if is_error { "no detail" } else { "(empty)" });
    } else {
        line.push_str(&body);
    }
    if image {
        line.push_str(" [image]");
    }
    line
}

/// The path or command a call was about, when the arguments say. Not the tool name.
fn subject(call: &ToolCall) -> Option<String> {
    let value: Value = serde_json::from_str(&call.arguments).ok()?;
    for key in ["command", "path", "file", "pattern", "query"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            let text = flatten_clip(text, 120);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn write_left(step: &Step<'_>, kept: bool, out: &mut String) {
    out.push_str("Left:\n");
    if kept {
        out.push_str("The messages after this one.\n");
        return;
    }
    if !step.reasoning.trim().is_empty() {
        out.push_str(&flatten_clip(step.reasoning, LEFT_CHARS));
        out.push('\n');
        return;
    }
    out.push_str(
        "Not stated. Continue from what was learned, and do not repeat a side effect that already happened.\n",
    );
}

fn flatten(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

fn flatten_clip(text: &str, max: usize) -> String {
    clip(&flatten(text), max)
}

fn flatten_clip_tail(text: &str, max: usize) -> String {
    clip_tail(&flatten(text), max)
}

/// Whether a provider error says the context is too long.
pub(crate) fn is_overflow(error: &str) -> bool {
    let e = error.to_lowercase();
    OVERFLOW.iter().any(|p| e.contains(p))
}

/// Characters a model call sends for these messages.
pub(crate) fn chars(messages: &[ChatMessage], replays_reasoning: bool) -> usize {
    messages
        .iter()
        .map(|m| match m {
            ChatMessage::System { content } => content.len(),
            ChatMessage::User { content, images }
            | ChatMessage::Tool {
                content, images, ..
            } => {
                content.len()
                    + images
                        .iter()
                        .map(|i| {
                            // Conservative image allowance, independent of base64 size.
                            let tiles = u64::from(i.width.div_ceil(512))
                                * u64::from(i.height.div_ceil(512));
                            (85 + 170 * tiles.min(256)) as usize * 4
                        })
                        .sum::<usize>()
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                reasoning,
                ..
            } => {
                content.len()
                    + if replays_reasoning {
                        reasoning.len()
                    } else {
                        0
                    }
                    + tool_calls
                        .iter()
                        .map(|c| c.name.len() + c.arguments.len())
                        .sum::<usize>()
            }
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
    ((chars(messages, s.replays_reasoning) + s.tool_chars) as f64
        / s.chars_per_token.unwrap_or(CHARS_PER_TOKEN))
    .round() as u64
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

/// Like [`clip`], but most of what remains is the end. Errors and command
/// output put the part that matters last.
fn clip_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut head = max / 4;
    while head > 0 && !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - max * 3 / 4;
    while tail < text.len() && !text.is_char_boundary(tail) {
        tail += 1;
    }
    if head >= tail {
        return clip(text, max);
    }
    format!("{} […] {}", &text[..head], &text[tail..])
}

/// The conversation as text for the summarizer.
fn render(messages: &[ChatMessage]) -> String {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        match m {
            ChatMessage::System { .. } => {}
            ChatMessage::User { content, images } => {
                let labels = images
                    .iter()
                    .map(|i| format!(" [Image: {} {}×{}]", i.name, i.width, i.height))
                    .collect::<String>();
                out.push(format!("USER: {}{}", clip(content, MESSAGE_CHARS), labels));
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                reasoning,
                ..
            } => {
                let mut line = format!("ASSISTANT: {}", clip(content, MESSAGE_CHARS));
                if !reasoning.trim().is_empty() {
                    line.push_str(&format!("\nTHINKING: {}", clip(reasoning, MESSAGE_CHARS)));
                }
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
        let (session_id, plan, generation, messages_len) = {
            let mut s = session.lock().unwrap();
            if s.compacting {
                return Err("this session is already being compacted".into());
            }
            let plan = plan(&s, cfg.keep).ok_or("nothing to compact yet")?;
            s.compacting = true;
            (
                s.info.session_id.clone(),
                plan,
                s.generation,
                s.messages.len(),
            )
        };
        let _busy = Compacting(session);
        let rt = self.runtime();
        let can_see_images = cfg
            .provider
            .as_ref()
            .and_then(|n| rt.models.get(n))
            .unwrap_or(&rt.config.provider)
            .supports_images
            != Some(false);
        // The kept turns are not in the rendered transcript. Say so, or the
        // summary describes the previous request as if it were still current.
        let note = if plan.cut < messages_len {
            "Newer turns after this transcript are kept word for word and are not shown here. \
Summarize only this part."
        } else {
            "Nothing after this transcript is kept. A short excerpt of what the last rounds \
found, and what is left, is attached separately; your summary is the only record of everything \
older, so quote the findings and the remaining work exactly. Do not list tool names."
        };
        let mut messages = vec![
            ChatMessage::System {
                content: cfg.prompt.clone().unwrap_or_else(|| PROMPT.into()),
            },
            ChatMessage::User {
                content: format!("{}\n\n{note}", render(&plan.input)),
                images: Vec::new(),
            },
        ];
        if can_see_images {
            for (index, message) in plan.input.iter().enumerate() {
                let images = message.images();
                if !images.is_empty() {
                    messages.push(ChatMessage::User {
                        content: format!("Images from transcript message {}. Preserve their relevant visual details in the summary.", index + 1),
                        images: images.to_vec(),
                    });
                }
            }
        }
        let call = ModelCall {
            provider: cfg.provider.clone(),
            options: Value::Null,
            messages,
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
    fn reasoning_counts_only_when_replayed() {
        let messages = [ChatMessage::Assistant {
            content: "answer".into(),
            reasoning: "thought".into(),
            tool_calls: vec![],
            reasoning_state: None,
        }];
        assert_eq!(chars(&messages, false), 6);
        assert_eq!(chars(&messages, true), 13);
        let dir = tempfile::tempdir().unwrap();
        let store = crate::session::SessionStore::new(dir.path());
        let session = store.create(".".into()).unwrap();
        let mut s = session.lock().unwrap();
        s.chars_per_token = Some(2.0);
        s.set_replays_reasoning(true);
        assert_eq!(s.chars_per_token, None);
        assert_eq!(tokens(&s, &messages), 3);
    }

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

    fn user(text: &str) -> ChatMessage {
        ChatMessage::User {
            content: text.into(),
            images: Vec::new(),
        }
    }

    fn assistant(content: &str, reasoning: &str, calls: Vec<ToolCall>) -> ChatMessage {
        ChatMessage::Assistant {
            content: content.into(),
            reasoning: reasoning.into(),
            tool_calls: calls,
            reasoning_state: None,
        }
    }

    fn tool(id: &str, content: &str, is_error: bool) -> ChatMessage {
        ChatMessage::Tool {
            call_id: id.into(),
            content: content.into(),
            images: Vec::new(),
            is_error,
        }
    }

    #[test]
    fn render_includes_the_plan_the_reply_left_in_thinking() {
        let text = render(&[assistant("editing", "next: update compact.rs", vec![])]);
        assert!(text.contains("ASSISTANT: editing"), "{text}");
        assert!(text.contains("THINKING: next: update compact.rs"), "{text}");
    }

    #[test]
    fn handoff_resumes_an_open_tool_loop_when_nothing_was_kept() {
        let call = ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            arguments: r#"{"command":"cargo test"}"#.into(),
        };
        let earlier = ToolCall {
            id: "c0".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"src/compact.rs"}"#.into(),
        };
        let covered = vec![
            user("fix the bug"),
            assistant("Looking.", "", vec![earlier]),
            tool("c0", "fn handoff", false),
            assistant("", "edit compact.rs next", vec![call]),
            tool("c1", &format!("{}UNIQUE_TAIL", "x".repeat(2000)), true),
        ];
        let ChatMessage::User { content, .. } = context_message("SUM", &covered, false) else {
            panic!("summary is a user message");
        };
        assert!(content.starts_with("Summary of the earlier conversation:\n\nSUM\n"));
        assert!(content.contains("Handoff, not a new request."));
        assert!(content.contains("No later turns were kept"));
        assert!(content.contains("Working on:\nfix the bug"));
        assert!(content.contains("Learned:"));
        assert!(content.contains("src/compact.rs: fn handoff"));
        assert!(content.contains("cargo test: failed:"));
        assert!(content.contains("UNIQUE_TAIL"), "{content}");
        assert!(content.contains("Left:\nedit compact.rs next"));
        assert!(!content.contains("shell"), "{content}");
        assert!(!content.contains("read_file"), "{content}");
        assert!(!content.contains("called "), "{content}");
        assert!(!content.contains(&"x".repeat(800)), "{content}");
        assert!(content.contains("read a file again before editing"));
        assert!(!content.contains("newest turns"), "{content}");
    }

    #[test]
    fn handoff_defers_to_turns_that_were_kept() {
        let covered = vec![user("q1"), assistant("a1", "", vec![])];
        let ChatMessage::User { content, .. } = context_message("SUM", &covered, true) else {
            panic!("summary is a user message");
        };
        assert!(content.contains("newest turns"));
        assert!(content.contains("older background"));
        assert!(content.contains("Before that, work had got to:\nq1"));
        assert!(content.contains("- a1"));
        assert!(content.contains("Left:\nThe messages after this one."));
        assert!(!content.contains("No later turns"), "{content}");
    }

    #[test]
    fn handoff_says_when_the_request_had_no_reply() {
        let covered = vec![user("older"), user("still open")];
        let ChatMessage::User { content, .. } = context_message("SUM", &covered, false) else {
            panic!("summary is a user message");
        };
        assert!(content.contains("Working on:\nstill open"));
        assert!(content.contains("Learned:\n- nothing yet"));
        assert!(content.contains("Answer the request. Nothing has been done yet."));
        assert!(!content.contains("older"), "{content}");
    }
}
