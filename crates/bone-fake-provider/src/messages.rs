//! `/v1/messages`, checked like Anthropic's: non-empty text, every
//! `tool_use` answered in the next user message, at most four cache
//! breakpoints, and thinking accepted back only unchanged, from the same
//! model, after the same history (preserved thinking, enforced).
//!
//! Prompt caching is simulated: a breakpoint writes the prefix up to it, and
//! a later request reads the longest written prefix within 20 positions
//! before one of its breakpoints (a run of `tool_use` or of `tool_result`
//! blocks is one position). Prefixes under `MIN_CACHED` tokens are not
//! cached.

use std::collections::HashSet;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::common::{Call, Conv, Msg, Reply, Tool, tokens};
use crate::{ApiError, Fault, State, Streamed, pieces};

const FIELDS: &[&str] = &[
    "model",
    "max_tokens",
    "messages",
    "system",
    "tools",
    "tool_choice",
    "stream",
    "thinking",
    "output_config",
    "metadata",
    "stop_sequences",
    "temperature",
    "top_p",
    "top_k",
    "cache_control",
];
const MIN_CACHED: u64 = 1024;
const LOOKBACK: usize = 20;
const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

pub(crate) async fn handle(
    state: &State,
    fault: Option<Fault>,
    body: &Value,
) -> Result<Streamed, ApiError> {
    let req = parse(state, body)?;
    let reply = state
        .backend
        .reply(&req.conv)
        .await
        .map_err(|e| ApiError(502, e))?;
    let (read, write) = cache(state, &req);
    let prompt = req.positions.last().map_or(0, |p| p.tokens);
    let usage = json!({
        "input_tokens": prompt - read - write,
        "cache_read_input_tokens": read,
        "cache_creation_input_tokens": write,
        "output_tokens": 1,
    });
    Ok(Streamed {
        events: events(state, fault, &req, reply, usage),
        reasoning_replayed: req.replayed,
        cache_read: read,
        cache_write: write,
        prompt_tokens: prompt,
    })
}

/// One cacheable position of the prompt: the fingerprint and tokens of the
/// prefix through it, and whether a breakpoint is on it.
struct Position {
    hash: String,
    tokens: u64,
    breakpoint: bool,
}

struct Parsed {
    conv: Conv,
    positions: Vec<Position>,
    replayed: usize,
    thinking: Option<bool>,
    /// The fingerprint of the whole history, for the reply's thinking.
    history: String,
}

fn parse(state: &State, body: &Value) -> Result<Parsed, ApiError> {
    let obj = body
        .as_object()
        .ok_or_else(|| ApiError::bad("the body must be a JSON object"))?;
    if let Some(k) = obj.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(ApiError::bad(format!(
            "{k}: Extra inputs are not permitted"
        )));
    }
    let model = body["model"]
        .as_str()
        .ok_or_else(|| ApiError::bad("model: Field required"))?;
    if body["max_tokens"].as_u64().is_none_or(|n| n == 0) {
        return Err(ApiError::bad("max_tokens: must be a positive integer"));
    }
    if body["stream"] != true {
        return Err(ApiError::bad("fake: only stream=true is supported"));
    }
    let thinking = match body["thinking"]["type"].as_str() {
        None => None,
        Some("adaptive") => Some(body["thinking"]["display"] == "summarized"),
        Some("disabled") => None,
        Some("enabled") => {
            return Err(ApiError::bad(
                "thinking.type.enabled is not supported for this model. Use thinking.type.adaptive and output_config.effort to control thinking behavior.",
            ));
        }
        Some(other) => {
            return Err(ApiError::bad(format!(
                "thinking.type: invalid value {other:?}"
            )));
        }
    };
    if let Some(effort) = body["output_config"]["effort"].as_str()
        && !EFFORTS.contains(&effort)
    {
        return Err(ApiError::bad(format!(
            "output_config.effort: invalid value {effort:?}"
        )));
    }

    // Normalized pieces, cache marks stripped, for fingerprints.
    let mut marks = 0;
    let mut strip = |block: &Value| -> Result<(Value, bool), ApiError> {
        let mut b = block.clone();
        let marked = b
            .as_object_mut()
            .and_then(|o| o.remove("cache_control"))
            .is_some();
        if marked {
            marks += 1;
            if matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")) {
                return Err(ApiError::bad(
                    "cache_control cannot be set on thinking blocks",
                ));
            }
        }
        Ok((b, marked))
    };
    let mut tools_n = Vec::new();
    let mut tools = Vec::new();
    for (i, t) in body["tools"].as_array().into_iter().flatten().enumerate() {
        let (t, marked) = strip(t)?;
        let name = t["name"]
            .as_str()
            .ok_or_else(|| ApiError::bad(format!("tools.{i}.name: Field required")))?;
        if !t["input_schema"].is_object() {
            return Err(ApiError::bad(format!(
                "tools.{i}.input_schema: Field required"
            )));
        }
        tools.push(Tool {
            name: name.into(),
            description: t["description"].as_str().unwrap_or_default().into(),
            parameters: t["input_schema"].clone(),
        });
        tools_n.push((t, marked));
    }
    let mut system_n = Vec::new();
    let mut messages = Vec::new();
    match &body["system"] {
        Value::Null => {}
        Value::String(s) => system_n.push((json!({ "type": "text", "text": s }), false)),
        Value::Array(blocks) => {
            for b in blocks {
                let (b, marked) = strip(b)?;
                if b["type"] != "text" || b["text"].as_str().is_none_or(str::is_empty) {
                    return Err(ApiError::bad(
                        "system: text content blocks must be non-empty",
                    ));
                }
                system_n.push((b, marked));
            }
        }
        _ => return Err(ApiError::bad("system: must be a string or text blocks")),
    }
    if !system_n.is_empty() {
        let all: Vec<&str> = system_n
            .iter()
            .filter_map(|(b, _)| b["text"].as_str())
            .collect();
        messages.push(Msg::System(all.join("\n")));
    }

    // Messages: consecutive turns of one role are combined.
    let mut turns: Vec<(String, Vec<(Value, bool)>)> = Vec::new();
    let raw = body["messages"]
        .as_array()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| ApiError::bad("messages: at least one message is required"))?;
    for (i, m) in raw.iter().enumerate() {
        let role = m["role"].as_str().unwrap_or_default();
        if role != "user" && role != "assistant" {
            return Err(ApiError::bad(format!(
                "messages.{i}.role: invalid value {role:?}"
            )));
        }
        let blocks = match &m["content"] {
            Value::String(s) => vec![json!({ "type": "text", "text": s })],
            Value::Array(a) => a.clone(),
            _ => {
                return Err(ApiError::bad(format!(
                    "messages.{i}.content: Field required"
                )));
            }
        };
        let mut out = Vec::new();
        for b in &blocks {
            out.push(strip(b)?);
        }
        match turns.last_mut().filter(|(r, _)| r == role) {
            Some((_, prev)) => prev.extend(out),
            None => turns.push((role.into(), out)),
        }
    }
    if marks > 4 {
        return Err(ApiError::bad(format!(
            "A maximum of 4 blocks with cache_control may be provided. Found {marks}."
        )));
    }
    if turns[0].0 != "user" {
        return Err(ApiError::bad(
            "messages: first message must use the \"user\" role",
        ));
    }

    let unmarked = |v: &[(Value, bool)]| v.iter().map(|(b, _)| b.clone()).collect::<Vec<_>>();
    let prefix_of = |turns: &[(String, Vec<(Value, bool)>)]| {
        let msgs: Vec<Value> = turns
            .iter()
            .map(|(r, b)| json!({ "role": r, "content": unmarked(b) }))
            .collect();
        fingerprint(&json!([
            model,
            unmarked(&system_n),
            unmarked(&tools_n),
            msgs
        ]))
    };
    let mut replayed = 0;
    let mut pending: Vec<String> = Vec::new();
    for (i, (role, blocks)) in turns.iter().enumerate() {
        let mut answered = HashSet::new();
        let mut text = String::new();
        let mut images = Vec::new();
        let mut calls = Vec::new();
        for (j, (b, _)) in blocks.iter().enumerate() {
            let at = format!("messages.{i}.content.{j}");
            match (role.as_str(), b["type"].as_str().unwrap_or_default()) {
                (_, "text") => {
                    let t = b["text"].as_str().unwrap_or_default();
                    if t.trim().is_empty() {
                        return Err(ApiError::bad(format!(
                            "{at}: text content blocks must contain non-whitespace text"
                        )));
                    }
                    text.push_str(t);
                }
                ("user", "image") => {
                    let s = &b["source"];
                    if s["type"] != "base64"
                        || !s["media_type"].is_string()
                        || !s["data"].is_string()
                    {
                        return Err(ApiError::bad(format!("{at}.source: invalid image source")));
                    }
                    images.push(format!(
                        "data:{};base64,{}",
                        s["media_type"].as_str().unwrap(),
                        s["data"].as_str().unwrap()
                    ));
                }
                ("user", "tool_result") => {
                    let id = b["tool_use_id"].as_str().unwrap_or_default();
                    if !pending.iter().any(|p| p == id) {
                        return Err(ApiError::bad(format!(
                            "{at}: unexpected `tool_use_id` found in `tool_result` blocks: {id}. Each `tool_result` block must have a corresponding `tool_use` block in the previous message."
                        )));
                    }
                    answered.insert(id.to_owned());
                    let output = match &b["content"] {
                        Value::Null => String::new(),
                        Value::String(s) => s.clone(),
                        Value::Array(parts) => {
                            let mut out = String::new();
                            for p in parts {
                                match p["type"].as_str() {
                                    Some("text") => {
                                        out.push_str(p["text"].as_str().unwrap_or_default())
                                    }
                                    Some("image") => images.push("data:image/png;base64,".into()),
                                    other => {
                                        return Err(ApiError::bad(format!(
                                            "{at}.content: unexpected block type {other:?}"
                                        )));
                                    }
                                }
                            }
                            out
                        }
                        _ => return Err(ApiError::bad(format!("{at}.content: invalid"))),
                    };
                    messages.push(Msg::Tool {
                        call_id: id.into(),
                        output,
                    });
                }
                ("assistant", "tool_use") => {
                    let id = b["id"].as_str().unwrap_or_default();
                    let name = b["name"].as_str().unwrap_or_default();
                    if id.is_empty() || name.is_empty() || !b["input"].is_object() {
                        return Err(ApiError::bad(format!(
                            "{at}: tool_use needs an id, a name and an object input"
                        )));
                    }
                    calls.push(Call {
                        id: id.into(),
                        name: name.into(),
                        arguments: b["input"].to_string(),
                    });
                }
                ("assistant", "thinking") => {
                    let payload = b["signature"]
                        .as_str()
                        .and_then(|s| state.open(s))
                        .ok_or_else(|| {
                            ApiError::bad(format!("{at}: Invalid `signature` in `thinking` block"))
                        })?;
                    // Another model's thinking is dropped, as the API does.
                    if payload["model"] != model {
                        continue;
                    }
                    if payload["thinking"] != b["thinking"] {
                        return Err(ApiError::bad(format!(
                            "{at}: `thinking` blocks must be passed back unmodified"
                        )));
                    }
                    if payload["prefix"] != prefix_of(&turns[..i]) {
                        return Err(ApiError::bad(format!(
                            "{at}: this thinking block was produced with a different history before it (system, tools or earlier messages changed); pass it back only in the conversation that produced it"
                        )));
                    }
                    replayed += 1;
                }
                (_, other) => {
                    return Err(ApiError::bad(format!(
                        "{at}: Input tag '{other}' found using 'type' does not match any of the expected tags for a {role} message"
                    )));
                }
            }
        }
        if !pending.is_empty() {
            let missing: Vec<&String> = pending.iter().filter(|p| !answered.contains(*p)).collect();
            if !missing.is_empty() {
                let missing: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
                return Err(ApiError::bad(format!(
                    "messages.{i}: `tool_use` ids were found without `tool_result` blocks immediately after: {}. Each `tool_use` block must have a corresponding `tool_result` block in the next message.",
                    missing.join(", ")
                )));
            }
            pending.clear();
        }
        if role == "assistant" {
            pending = calls.iter().map(|c| c.id.clone()).collect();
            messages.push(Msg::Assistant { text, calls });
        } else if !text.is_empty() || !images.is_empty() {
            messages.push(Msg::User { text, images });
        }
    }
    if !pending.is_empty() {
        return Err(ApiError::bad(format!(
            "`tool_use` ids were found without `tool_result` blocks immediately after: {}",
            pending.join(", ")
        )));
    }

    // Cache positions: tools, system, then message blocks; a run of
    // tool_use or tool_result blocks is one position.
    let mut positions: Vec<Position> = Vec::new();
    let mut hash = fingerprint(&json!(model));
    let mut total = 0u64;
    let mut last_kind = String::new();
    let flat = tools_n
        .iter()
        .chain(&system_n)
        .chain(turns.iter().flat_map(|(_, b)| b));
    for (b, marked) in flat {
        hash = fingerprint(&json!([hash, b]));
        total += tokens(&b.to_string());
        let kind = b["type"].as_str().unwrap_or_default().to_owned();
        let run = matches!(kind.as_str(), "tool_use" | "tool_result") && kind == last_kind;
        last_kind = kind;
        match positions.last_mut().filter(|_| run) {
            Some(p) => {
                p.hash = hash.clone();
                p.tokens = total;
                p.breakpoint |= *marked;
            }
            None => positions.push(Position {
                hash: hash.clone(),
                tokens: total,
                breakpoint: *marked,
            }),
        }
    }
    // Top-level cache_control: a breakpoint on the last position.
    if body.get("cache_control").is_some_and(|c| !c.is_null())
        && let Some(last) = positions.last_mut()
    {
        last.breakpoint = true;
    }
    let history = prefix_of(&turns);
    Ok(Parsed {
        conv: Conv {
            model: model.into(),
            messages,
            tools,
        },
        positions,
        replayed,
        thinking,
        history,
    })
}

fn fingerprint(v: &Value) -> String {
    Sha256::digest(v.to_string())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Read the longest cached prefix in reach of a breakpoint; write each
/// breakpoint's prefix. Returns (read, written) tokens.
fn cache(state: &State, req: &Parsed) -> (u64, u64) {
    let mut cache = state.cache.lock().unwrap();
    let mut read = 0;
    let breakpoints: Vec<usize> = (0..req.positions.len())
        .filter(|&i| req.positions[i].breakpoint)
        .collect();
    for &b in &breakpoints {
        let reach = b.saturating_sub(LOOKBACK - 1)..=b;
        if let Some(hit) = reach
            .rev()
            .find_map(|i| cache.get(&req.positions[i].hash).copied())
        {
            read = read.max(hit);
        }
    }
    let mut written = 0;
    for &b in &breakpoints {
        let p = &req.positions[b];
        if p.tokens >= MIN_CACHED {
            cache.insert(p.hash.clone(), p.tokens);
            written = written.max(p.tokens);
        }
    }
    (read, written.saturating_sub(read))
}

fn events(
    state: &State,
    fault: Option<Fault>,
    req: &Parsed,
    mut reply: Reply,
    usage: Value,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut ev = |v: Value| {
        out.push(format!(
            "event: {}\ndata: {v}\n\n",
            v["type"].as_str().unwrap_or_default()
        ));
    };
    if fault == Some(Fault::ErrorBody) {
        ev(json!({ "type": "error", "error": {
            "type": "overloaded_error", "message": "fake: upstream failed mid-request",
        }}));
        return out;
    }
    if fault == Some(Fault::Empty) {
        reply = Reply::default();
    }
    let model = &req.conv.model;
    ev(json!({ "type": "message_start", "message": {
        "id": state.id("msg"), "type": "message", "role": "assistant", "model": model,
        "content": [], "stop_reason": null, "stop_sequence": null, "usage": usage,
    }}));
    let mut index = 0;
    if let Some(summarized) = req.thinking
        && !reply.reasoning.is_empty()
    {
        let shown = if summarized {
            reply.reasoning.clone()
        } else {
            String::new()
        };
        ev(json!({ "type": "content_block_start", "index": index,
            "content_block": { "type": "thinking", "thinking": "", "signature": "" } }));
        for p in pieces(&shown) {
            ev(json!({ "type": "content_block_delta", "index": index,
                "delta": { "type": "thinking_delta", "thinking": p } }));
        }
        let signature = state.seal(&json!({
            "model": model, "thinking": shown, "prefix": req.history,
        }));
        ev(json!({ "type": "content_block_delta", "index": index,
            "delta": { "type": "signature_delta", "signature": signature } }));
        ev(json!({ "type": "content_block_stop", "index": index }));
        index += 1;
    }
    let output_tokens = tokens(&reply.reasoning) + tokens(&reply.text);
    if fault == Some(Fault::Incomplete) {
        ev(
            json!({ "type": "message_delta", "delta": { "stop_reason": "max_tokens" },
            "usage": { "output_tokens": output_tokens } }),
        );
        ev(json!({ "type": "message_stop" }));
        return out;
    }
    if !reply.text.is_empty() {
        ev(json!({ "type": "content_block_start", "index": index,
            "content_block": { "type": "text", "text": "" } }));
        for p in pieces(&reply.text) {
            ev(json!({ "type": "content_block_delta", "index": index,
                "delta": { "type": "text_delta", "text": p } }));
        }
        ev(json!({ "type": "content_block_stop", "index": index }));
        index += 1;
    }
    for c in &reply.calls {
        let id = state.id("toolu");
        ev(json!({ "type": "content_block_start", "index": index,
            "content_block": { "type": "tool_use", "id": id, "name": c.name, "input": {} } }));
        let args = serde_json::from_str::<Value>(&c.arguments)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}))
            .to_string();
        for p in pieces(&args) {
            ev(json!({ "type": "content_block_delta", "index": index,
                "delta": { "type": "input_json_delta", "partial_json": p } }));
        }
        ev(json!({ "type": "content_block_stop", "index": index }));
        index += 1;
    }
    let stop = if reply.calls.is_empty() {
        "end_turn"
    } else {
        "tool_use"
    };
    ev(
        json!({ "type": "message_delta", "delta": { "stop_reason": stop, "stop_sequence": null },
        "usage": { "output_tokens": output_tokens } }),
    );
    ev(json!({ "type": "message_stop" }));
    if fault == Some(Fault::Cut) {
        out.truncate(out.len().div_ceil(2).max(1));
    }
    out
}
