//! `/v1/chat/completions`, checked like OpenAI's: every tool call answered
//! before anything else, no unknown fields, streaming only.

use serde_json::{Value, json};

use crate::common::{Call, Conv, Msg, Reply, Tool, tokens};
use crate::{ApiError, Fault, Flavor, State, Streamed, pieces, sse};

const FIELDS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "stream_options",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "reasoning_effort",
    "temperature",
    "top_p",
    "max_tokens",
    "max_completion_tokens",
    "user",
];

pub(crate) async fn handle(
    state: &State,
    flavor: Flavor,
    fault: Option<Fault>,
    body: &Value,
) -> Result<Streamed, ApiError> {
    let conv = parse(body, flavor)?;
    let reply = state
        .backend
        .reply(&conv)
        .await
        .map_err(|e| ApiError(502, e))?;
    let usage = body["stream_options"]["include_usage"] == true;
    Ok(Streamed {
        events: events(state, flavor, fault, &conv, reply, usage),
        ..Default::default()
    })
}

fn parse(body: &Value, flavor: Flavor) -> Result<Conv, ApiError> {
    let obj = body
        .as_object()
        .ok_or_else(|| ApiError::bad("the body must be a JSON object"))?;
    if flavor == Flavor::OpenAi
        && let Some(k) = obj.keys().find(|k| !FIELDS.contains(&k.as_str()))
    {
        return Err(ApiError::bad(format!(
            "Unrecognized request argument supplied: {k}"
        )));
    }
    let model = body["model"]
        .as_str()
        .ok_or_else(|| ApiError::bad("you must provide a model parameter"))?;
    if body["stream"] != true {
        return Err(ApiError::bad("fake: only stream=true is supported"));
    }
    let messages = body["messages"]
        .as_array()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| ApiError::bad("'messages' must be a non-empty array"))?;
    let mut out = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        let role = m["role"].as_str().unwrap_or_default();
        if role != "tool" && !pending.is_empty() {
            return Err(unanswered(&pending));
        }
        if m.get("reasoning_content").is_some() && flavor != Flavor::Qwen {
            return Err(ApiError::bad(format!(
                "messages[{i}]: Additional properties are not allowed ('reasoning_content' was unexpected)"
            )));
        }
        match role {
            "system" | "developer" => out.push(Msg::System(text(&m["content"], i)?)),
            "user" => {
                let (text, images) = user_content(&m["content"], i)?;
                out.push(Msg::User { text, images });
            }
            "assistant" => {
                let mut calls = Vec::new();
                for c in m["tool_calls"].as_array().into_iter().flatten() {
                    let id = c["id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| {
                            ApiError::bad(format!("messages[{i}].tool_calls: missing id"))
                        })?;
                    let name = c["function"]["name"].as_str().ok_or_else(|| {
                        ApiError::bad(format!("messages[{i}].tool_calls: missing function.name"))
                    })?;
                    let arguments = c["function"]["arguments"].as_str().ok_or_else(|| {
                        ApiError::bad(format!(
                            "messages[{i}].tool_calls: function.arguments must be a string"
                        ))
                    })?;
                    pending.push(id.into());
                    calls.push(Call {
                        id: id.into(),
                        name: name.into(),
                        arguments: arguments.into(),
                    });
                }
                let content = match &m["content"] {
                    Value::Null => String::new(),
                    c => text(c, i)?,
                };
                if content.is_empty() && calls.is_empty() {
                    return Err(ApiError::bad(format!(
                        "messages[{i}]: an assistant message needs content or tool_calls"
                    )));
                }
                out.push(Msg::Assistant {
                    text: content,
                    calls,
                });
            }
            "tool" => {
                let id = m["tool_call_id"].as_str().unwrap_or_default();
                let Some(at) = pending.iter().position(|p| p == id) else {
                    return Err(ApiError::bad(format!(
                        "messages[{i}]: 'tool' messages must respond to a preceding message with 'tool_calls' (no pending call {id:?})"
                    )));
                };
                pending.remove(at);
                out.push(Msg::Tool {
                    call_id: id.into(),
                    output: text(&m["content"], i)?,
                });
            }
            other => {
                return Err(ApiError::bad(format!(
                    "messages[{i}]: invalid role {other:?}"
                )));
            }
        }
    }
    if !pending.is_empty() {
        return Err(unanswered(&pending));
    }
    let tools = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, t)| {
            let f = &t["function"];
            let name = f["name"].as_str().filter(|_| t["type"] == "function");
            Ok(Tool {
                name: name
                    .ok_or_else(|| {
                        ApiError::bad(format!("tools[{i}]: needs type function and function.name"))
                    })?
                    .into(),
                description: f["description"].as_str().unwrap_or_default().into(),
                parameters: f["parameters"].clone(),
            })
        })
        .collect::<Result<_, ApiError>>()?;
    Ok(Conv {
        model: model.into(),
        messages: out,
        tools,
    })
}

fn unanswered(pending: &[String]) -> ApiError {
    ApiError::bad(format!(
        "An assistant message with 'tool_calls' must be followed by tool messages responding to each 'tool_call_id'. The following tool_call_ids did not have response messages: {}",
        pending.join(", ")
    ))
}

fn text(content: &Value, i: usize) -> Result<String, ApiError> {
    match content {
        Value::String(s) => Ok(s.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match p["type"].as_str() {
                Some("text") => Ok(p["text"].as_str().unwrap_or_default().to_owned()),
                _ => Err(ApiError::bad(format!(
                    "messages[{i}]: only text parts here"
                ))),
            })
            .collect(),
        _ => Err(ApiError::bad(format!(
            "messages[{i}]: content must be a string or parts"
        ))),
    }
}

fn user_content(content: &Value, i: usize) -> Result<(String, Vec<String>), ApiError> {
    let Value::Array(parts) = content else {
        return Ok((text(content, i)?, Vec::new()));
    };
    let mut out = (String::new(), Vec::new());
    for p in parts {
        match p["type"].as_str() {
            Some("text") => out.0.push_str(p["text"].as_str().unwrap_or_default()),
            Some("image_url") => {
                let url = p["image_url"]["url"].as_str().unwrap_or_default();
                if !url.starts_with("data:image/") && !url.starts_with("http") {
                    return Err(ApiError::bad(format!("messages[{i}]: invalid image_url")));
                }
                out.1.push(url.into());
            }
            other => {
                return Err(ApiError::bad(format!(
                    "messages[{i}]: unknown part type {other:?}"
                )));
            }
        }
    }
    Ok(out)
}

fn events(
    state: &State,
    flavor: Flavor,
    fault: Option<Fault>,
    conv: &Conv,
    mut reply: Reply,
    usage: bool,
) -> Vec<String> {
    let id = state.id("chatcmpl");
    let chunk = |delta: Value, finish: Value| {
        sse(&json!({
            "id": id, "object": "chat.completion.chunk", "created": 0, "model": conv.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        }))
    };
    if fault == Some(Fault::ErrorBody) {
        return vec![sse(&json!({ "error": {
            "message": "fake: upstream failed mid-request", "type": "server_error",
        }}))];
    }
    if fault == Some(Fault::Empty) {
        reply = Reply::default();
    }
    let mut out = vec![chunk(
        json!({ "role": "assistant", "content": "" }),
        Value::Null,
    )];
    let field = match flavor {
        Flavor::OpenAi | Flavor::Unsealed => None,
        Flavor::DeepSeek | Flavor::Qwen => Some("reasoning_content"),
        Flavor::OpenRouter => Some("reasoning"),
    };
    if let Some(field) = field {
        for p in pieces(&reply.reasoning) {
            out.push(chunk(json!({ field: p }), Value::Null));
        }
    }
    if fault == Some(Fault::Incomplete) {
        out.push(chunk(json!({}), json!("length")));
        out.push("data: [DONE]\n\n".into());
        return out;
    }
    for p in pieces(&reply.text) {
        out.push(chunk(json!({ "content": p }), Value::Null));
    }
    for (index, c) in reply.calls.iter().enumerate() {
        let call_id = if c.id.is_empty() {
            state.id("call")
        } else {
            c.id.clone()
        };
        out.push(chunk(
            json!({ "tool_calls": [{ "index": index, "id": call_id, "type": "function",
                "function": { "name": c.name, "arguments": "" } }] }),
            Value::Null,
        ));
        for p in pieces(&c.arguments) {
            out.push(chunk(
                json!({ "tool_calls": [{ "index": index, "function": { "arguments": p } }] }),
                Value::Null,
            ));
        }
    }
    if fault == Some(Fault::Cut) {
        // Stop partway, as a proxy closing a slow stream would.
        out.truncate(out.len().div_ceil(2).max(1));
        return out;
    }
    let finish = if reply.calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    out.push(chunk(json!({}), json!(finish)));
    if usage {
        let output = tokens(&reply.reasoning) + tokens(&reply.text);
        out.push(sse(&json!({
            "id": id, "object": "chat.completion.chunk", "created": 0, "model": conv.model,
            "choices": [],
            "usage": { "prompt_tokens": conv.tokens(), "completion_tokens": output,
                "total_tokens": conv.tokens() + output },
        })));
    }
    out.push("data: [DONE]\n\n".into());
    out
}
