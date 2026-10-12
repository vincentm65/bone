//! `/v1/responses`, stateless (`store: false`), checked like OpenAI's:
//! reasoning comes back only sealed by this server for the same model, and
//! only followed by the item it led to; every call gets an output.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::common::{Call, Conv, Msg, Reply, Tool, tokens};
use crate::{ApiError, Fault, Flavor, State, Streamed, pieces, sse};

const FIELDS: &[&str] = &[
    "model",
    "input",
    "instructions",
    "stream",
    "store",
    "include",
    "reasoning",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "max_output_tokens",
    "previous_response_id",
    "text",
    "truncation",
    "metadata",
    "user",
    "temperature",
    "top_p",
];

pub(crate) async fn handle(
    state: &State,
    flavor: Flavor,
    fault: Option<Fault>,
    body: &Value,
) -> Result<Streamed, ApiError> {
    let (conv, replayed) = parse(state, body)?;
    let reply = state
        .backend
        .reply(&conv)
        .await
        .map_err(|e| ApiError(502, e))?;
    let include_sealed = body["include"]
        .as_array()
        .is_some_and(|i| i.iter().any(|v| v == "reasoning.encrypted_content"))
        && flavor != Flavor::Unsealed;
    let summaries = body["reasoning"]["summary"].is_string();
    Ok(Streamed {
        events: events(state, fault, &conv, reply, include_sealed, summaries),
        reasoning_replayed: replayed,
        ..Default::default()
    })
}

fn parse(state: &State, body: &Value) -> Result<(Conv, usize), ApiError> {
    let obj = body
        .as_object()
        .ok_or_else(|| ApiError::bad("the body must be a JSON object"))?;
    if let Some(k) = obj.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(ApiError::bad(format!("Unknown parameter: '{k}'.")));
    }
    let model = body["model"]
        .as_str()
        .ok_or_else(|| ApiError::bad("Missing required parameter: 'model'."))?;
    if body["stream"] != true {
        return Err(ApiError::bad("fake: only stream=true is supported"));
    }
    if body
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
    {
        return Err(ApiError::bad(
            "fake: previous_response_id is not supported (requests must be stateless)",
        ));
    }
    let stateless = body["store"] == false;
    let mut messages = Vec::new();
    if let Some(i) = body["instructions"].as_str() {
        messages.push(Msg::System(i.into()));
    }
    let items = match &body["input"] {
        Value::String(s) => vec![json!({ "role": "user", "content": s })],
        Value::Array(a) => a.clone(),
        _ => return Err(ApiError::bad("Missing required parameter: 'input'.")),
    };
    let mut pending: Vec<String> = Vec::new();
    let mut known: HashSet<String> = HashSet::new();
    let mut replayed = 0;
    // Consecutive assistant items (text, calls) form one assistant turn.
    let mut turn: Option<(String, Vec<Call>)> = None;
    let flush = |turn: &mut Option<(String, Vec<Call>)>, messages: &mut Vec<Msg>| {
        if let Some((text, calls)) = turn.take() {
            messages.push(Msg::Assistant { text, calls });
        }
    };
    for (i, item) in items.iter().enumerate() {
        let kind = match item["type"].as_str() {
            Some(t) => t,
            None if item.get("role").is_some() => "message",
            None => return Err(ApiError::bad(format!("input[{i}]: missing 'type'"))),
        };
        match kind {
            "message" => {
                let role = item["role"].as_str().unwrap_or_default();
                if role == "assistant" {
                    if let Some(id) = item["id"].as_str()
                        && !id.starts_with("msg_")
                    {
                        return Err(ApiError::bad(format!(
                            "Invalid 'input[{i}].id': '{id}'. Expected an ID that begins with 'msg'."
                        )));
                    }
                    let text = content_text(&item["content"], i, &["output_text", "refusal"])?;
                    turn.get_or_insert_default().0.push_str(&text);
                    continue;
                }
                flush(&mut turn, &mut messages);
                if !pending.is_empty() {
                    return Err(no_output(&pending[0]));
                }
                match role {
                    "system" | "developer" => messages.push(Msg::System(content_text(
                        &item["content"],
                        i,
                        &["input_text"],
                    )?)),
                    "user" => {
                        let (text, images) = user_content(&item["content"], i)?;
                        messages.push(Msg::User { text, images });
                    }
                    other => {
                        return Err(ApiError::bad(format!("input[{i}]: invalid role {other:?}")));
                    }
                }
            }
            "reasoning" => {
                let id = item["id"].as_str().unwrap_or_default();
                if !id.starts_with("rs_") {
                    return Err(ApiError::bad(format!(
                        "Invalid 'input[{i}].id': '{id}'. Expected an ID that begins with 'rs'."
                    )));
                }
                let Some(sealed) = item["encrypted_content"].as_str() else {
                    if stateless {
                        return Err(ApiError(
                            404,
                            format!(
                                "Item with id '{id}' not found. Items are not persisted when `store` is set to false. Try again with `store` set to true, or remove this item from your input."
                            ),
                        ));
                    }
                    return Err(ApiError::bad("fake: stored items are not supported"));
                };
                let payload = state
                    .open(sealed)
                    .filter(|p| p["model"] == model && p["id"] == id)
                    .ok_or_else(|| {
                        ApiError::bad(format!(
                            "The encrypted content for item {id} could not be verified."
                        ))
                    })?;
                let follows: Vec<&str> = payload["follows"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let next = items.get(i + 1).and_then(|n| n["id"].as_str());
                if !next.is_some_and(|n| follows.contains(&n)) {
                    return Err(ApiError::bad(format!(
                        "Item '{id}' of type 'reasoning' was provided without its required following item."
                    )));
                }
                replayed += 1;
                turn.get_or_insert_default();
            }
            "function_call" => {
                if let Some(id) = item["id"].as_str()
                    && !id.starts_with("fc_")
                {
                    return Err(ApiError::bad(format!(
                        "Invalid 'input[{i}].id': '{id}'. Expected an ID that begins with 'fc'."
                    )));
                }
                let call_id = item["call_id"]
                    .as_str()
                    .filter(|c| !c.is_empty())
                    .ok_or_else(|| {
                        ApiError::bad(format!("Missing required parameter: 'input[{i}].call_id'."))
                    })?;
                let name = item["name"].as_str().ok_or_else(|| {
                    ApiError::bad(format!("Missing required parameter: 'input[{i}].name'."))
                })?;
                let arguments = item["arguments"].as_str().ok_or_else(|| {
                    ApiError::bad(format!(
                        "Invalid type for 'input[{i}].arguments': expected a string."
                    ))
                })?;
                pending.push(call_id.into());
                known.insert(call_id.into());
                turn.get_or_insert_default().1.push(Call {
                    id: call_id.into(),
                    name: name.into(),
                    arguments: arguments.into(),
                });
            }
            "function_call_output" => {
                flush(&mut turn, &mut messages);
                let call_id = item["call_id"].as_str().unwrap_or_default();
                if !known.contains(call_id) {
                    return Err(ApiError::bad(format!(
                        "No tool call found for function call output with call_id {call_id}."
                    )));
                }
                pending.retain(|p| p != call_id);
                let output = match &item["output"] {
                    Value::String(s) => s.clone(),
                    other => {
                        return Err(ApiError::bad(format!(
                            "Invalid type for 'input[{i}].output': expected a string, got {other}."
                        )));
                    }
                };
                messages.push(Msg::Tool {
                    call_id: call_id.into(),
                    output,
                });
            }
            other => return Err(ApiError::bad(format!("Invalid value: '{other}'."))),
        }
    }
    flush(&mut turn, &mut messages);
    if let Some(p) = pending.first() {
        return Err(no_output(p));
    }
    let tools = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, t)| {
            let name = t["name"].as_str().filter(|_| t["type"] == "function");
            Ok(Tool {
                name: name
                    .ok_or_else(|| {
                        ApiError::bad(format!("Missing required parameter: 'tools[{i}].name'."))
                    })?
                    .into(),
                description: t["description"].as_str().unwrap_or_default().into(),
                parameters: t["parameters"].clone(),
            })
        })
        .collect::<Result<_, ApiError>>()?;
    let conv = Conv {
        model: model.into(),
        messages,
        tools,
    };
    Ok((conv, replayed))
}

fn no_output(call_id: &str) -> ApiError {
    ApiError::bad(format!("No tool output found for function call {call_id}."))
}

fn content_text(content: &Value, i: usize, kinds: &[&str]) -> Result<String, ApiError> {
    match content {
        Value::String(s) => Ok(s.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match p["type"].as_str() {
                Some(t) if kinds.contains(&t) => Ok(p["text"]
                    .as_str()
                    .or(p["refusal"].as_str())
                    .unwrap_or_default()
                    .to_owned()),
                other => Err(ApiError::bad(format!(
                    "Invalid value: '{}'. Supported values are: {}. (input[{i}].content)",
                    other.unwrap_or("null"),
                    kinds.join(", ")
                ))),
            })
            .collect(),
        _ => Err(ApiError::bad(format!(
            "input[{i}].content must be a string or parts"
        ))),
    }
}

fn user_content(content: &Value, i: usize) -> Result<(String, Vec<String>), ApiError> {
    let Value::Array(parts) = content else {
        return Ok((content_text(content, i, &["input_text"])?, Vec::new()));
    };
    let mut out = (String::new(), Vec::new());
    for p in parts {
        match p["type"].as_str() {
            Some("input_text") => out.0.push_str(p["text"].as_str().unwrap_or_default()),
            Some("input_image") => {
                let url = p["image_url"].as_str().unwrap_or_default();
                if !url.starts_with("data:image/") && !url.starts_with("http") {
                    return Err(ApiError::bad(format!("input[{i}]: invalid image_url")));
                }
                out.1.push(url.into());
            }
            other => {
                return Err(ApiError::bad(format!(
                    "Invalid value: '{}'. Supported values are: input_text, input_image. (input[{i}].content)",
                    other.unwrap_or("null")
                )));
            }
        }
    }
    Ok(out)
}

fn events(
    state: &State,
    fault: Option<Fault>,
    conv: &Conv,
    mut reply: Reply,
    include_sealed: bool,
    summaries: bool,
) -> Vec<String> {
    let mut seq = 0u64;
    let mut out = Vec::new();
    let mut ev = |mut v: Value| {
        v["sequence_number"] = json!(seq);
        seq += 1;
        out.push(sse(&v));
    };
    if fault == Some(Fault::ErrorBody) {
        // What LiteLLM sends when the upstream call fails mid-request.
        return vec![sse(&json!({ "error": {
            "message": "fake: upstream failed mid-request", "type": "server_error",
        }}))];
    }
    if fault == Some(Fault::Empty) {
        reply = Reply::default();
    }
    let response_id = state.id("resp");
    let shell = |status: &str, output: &[Value], usage: Value| {
        json!({
            "id": response_id, "object": "response", "status": status, "model": conv.model,
            "output": output, "usage": usage,
        })
    };
    ev(json!({ "type": "response.created", "response": shell("in_progress", &[], Value::Null) }));
    ev(
        json!({ "type": "response.in_progress", "response": shell("in_progress", &[], Value::Null) }),
    );

    // Ids are known up front: the sealed reasoning names what follows it.
    let message_id = (!reply.text.is_empty()).then(|| state.id("msg"));
    let calls: Vec<(String, String, &Call)> = reply
        .calls
        .iter()
        .map(|c| {
            let call_id = if c.id.is_empty() {
                state.id("call")
            } else {
                c.id.clone()
            };
            (state.id("fc"), call_id, c)
        })
        .collect();
    let mut follows: Vec<String> = message_id.iter().cloned().collect();
    follows.extend(calls.iter().map(|(id, ..)| id.clone()));

    let mut output = Vec::new();
    let mut index = 0;
    if !reply.reasoning.is_empty() {
        let id = state.id("rs");
        ev(
            json!({ "type": "response.output_item.added", "output_index": index,
            "item": { "type": "reasoning", "id": id, "summary": [] } }),
        );
        let mut summary = Vec::new();
        if summaries {
            ev(
                json!({ "type": "response.reasoning_summary_part.added", "item_id": id,
                "output_index": index, "summary_index": 0,
                "part": { "type": "summary_text", "text": "" } }),
            );
            for p in pieces(&reply.reasoning) {
                ev(
                    json!({ "type": "response.reasoning_summary_text.delta", "item_id": id,
                    "output_index": index, "summary_index": 0, "delta": p }),
                );
            }
            ev(
                json!({ "type": "response.reasoning_summary_text.done", "item_id": id,
                "output_index": index, "summary_index": 0, "text": reply.reasoning }),
            );
            ev(
                json!({ "type": "response.reasoning_summary_part.done", "item_id": id,
                "output_index": index, "summary_index": 0,
                "part": { "type": "summary_text", "text": reply.reasoning } }),
            );
            summary.push(json!({ "type": "summary_text", "text": reply.reasoning }));
        }
        let mut item = json!({ "type": "reasoning", "id": id, "summary": summary });
        if include_sealed {
            item["encrypted_content"] = json!(state.seal(&json!({
                "id": id, "model": conv.model, "text": reply.reasoning, "follows": follows,
            })));
        }
        ev(json!({ "type": "response.output_item.done", "output_index": index, "item": item }));
        output.push(item);
        index += 1;
    }
    if fault == Some(Fault::Incomplete) {
        ev(json!({ "type": "response.incomplete", "response": {
            "id": response_id, "object": "response", "status": "incomplete", "model": conv.model,
            "incomplete_details": { "reason": "max_output_tokens" }, "output": output,
        }}));
        return out;
    }
    if let Some(id) = &message_id {
        ev(
            json!({ "type": "response.output_item.added", "output_index": index,
            "item": { "type": "message", "id": id, "role": "assistant", "status": "in_progress", "content": [] } }),
        );
        ev(
            json!({ "type": "response.content_part.added", "item_id": id, "output_index": index,
            "content_index": 0, "part": { "type": "output_text", "text": "", "annotations": [] } }),
        );
        for p in pieces(&reply.text) {
            ev(json!({ "type": "response.output_text.delta", "item_id": id,
                "output_index": index, "content_index": 0, "delta": p }));
        }
        ev(
            json!({ "type": "response.output_text.done", "item_id": id, "output_index": index,
            "content_index": 0, "text": reply.text }),
        );
        let part = json!({ "type": "output_text", "text": reply.text, "annotations": [] });
        ev(
            json!({ "type": "response.content_part.done", "item_id": id, "output_index": index,
            "content_index": 0, "part": part }),
        );
        let item = json!({ "type": "message", "id": id, "role": "assistant",
            "status": "completed", "content": [part] });
        ev(json!({ "type": "response.output_item.done", "output_index": index, "item": item }));
        output.push(item);
        index += 1;
    }
    for (id, call_id, c) in &calls {
        ev(
            json!({ "type": "response.output_item.added", "output_index": index,
            "item": { "type": "function_call", "id": id, "call_id": call_id, "name": c.name,
                "arguments": "", "status": "in_progress" } }),
        );
        for p in pieces(&c.arguments) {
            ev(
                json!({ "type": "response.function_call_arguments.delta", "item_id": id,
                "output_index": index, "delta": p }),
            );
        }
        ev(
            json!({ "type": "response.function_call_arguments.done", "item_id": id,
            "output_index": index, "arguments": c.arguments }),
        );
        let item = json!({ "type": "function_call", "id": id, "call_id": call_id,
            "name": c.name, "arguments": c.arguments, "status": "completed" });
        ev(json!({ "type": "response.output_item.done", "output_index": index, "item": item }));
        output.push(item);
        index += 1;
    }
    let output_tokens = tokens(&reply.reasoning) + tokens(&reply.text);
    let usage = json!({
        "input_tokens": conv.tokens(), "input_tokens_details": { "cached_tokens": 0 },
        "output_tokens": output_tokens,
        "output_tokens_details": { "reasoning_tokens": tokens(&reply.reasoning) },
        "total_tokens": conv.tokens() + output_tokens,
    });
    ev(json!({ "type": "response.completed", "response": shell("completed", &output, usage) }));
    if fault == Some(Fault::Cut) {
        out.truncate(out.len().div_ceil(2).max(1));
    }
    out
}
