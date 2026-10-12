//! OpenAI `/responses` streaming provider. Unlike `/chat/completions` it
//! returns reasoning models' thinking as encrypted items, which are kept on
//! the assistant message and sent back so the model keeps its reasoning
//! between calls. Stateless: `store` is off and the whole history is sent.

use bone_proto::types::{ChatMessage, ImageAttachment, ReasoningState, ToolCall, Usage};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::openai::{Capture, bearer, check_images, configured, http_client, send};
use super::sse::SseParser;
use super::{Completion, CompletionRequest, Delta, DeltaSink, Provider, ProviderError};
use crate::config::ProviderConfig;

pub struct OpenAiResponsesProvider {
    http: reqwest::Client,
    config: ProviderConfig,
}

impl OpenAiResponsesProvider {
    pub fn new(config: ProviderConfig) -> Self {
        OpenAiResponsesProvider {
            http: http_client(),
            config,
        }
    }

    fn url(&self) -> String {
        format!("{}/responses", self.config.base_url.trim_end_matches('/'))
    }

    async fn stream(
        &self,
        req: CompletionRequest<'_>,
        on_delta: DeltaSink<'_>,
    ) -> Result<Completion, ProviderError> {
        check_images(&self.config, req.messages)?;
        let (body, headers) = configured(
            request_body(&self.config, &req),
            bearer(&self.config),
            &self.config,
        );
        let mut capture = Capture::start(&self.url(), &body);
        let resp = send(&self.http, &self.url(), &headers, &body, &mut capture).await?;
        let mut bytes = resp.bytes_stream();
        let mut parser = SseParser::default();
        let mut acc = Accumulator::default();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| ProviderError(format!("stream error: {e}")))?;
            capture.chunk(&chunk);
            for data in parser.push(&chunk) {
                if acc.apply(&data, on_delta)? {
                    return acc.finish(&self.config.model);
                }
            }
        }
        if let Some(data) = parser.finish() {
            acc.apply(&data, on_delta)?;
        }
        acc.finish(&self.config.model)
    }
}

impl Provider for OpenAiResponsesProvider {
    fn complete<'a>(
        &'a self,
        req: CompletionRequest<'a>,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, ProviderError>> {
        Box::pin(self.stream(req, on_delta))
    }
}

fn request_body(config: &ProviderConfig, req: &CompletionRequest<'_>) -> Value {
    let mut reasoning = json!({ "summary": "auto" });
    if let Some(effort) = config.reasoning_effort.as_deref().filter(|e| !e.is_empty()) {
        reasoning["effort"] = json!(effort);
    }
    let mut body = json!({
        "model": config.model,
        "input": wire_input(req.messages, &config.model),
        "stream": true,
        "store": false,
        "include": ["reasoning.encrypted_content"],
        "reasoning": reasoning,
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
    }
    body
}

fn wire_input(messages: &[ChatMessage], model: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut images = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        match message {
            ChatMessage::System { content } => {
                out.push(json!({ "role": "system", "content": content }));
            }
            ChatMessage::User { content, images } => out.push(wire_user(content, images)),
            ChatMessage::Assistant {
                content,
                tool_calls,
                reasoning_state,
                ..
            } => {
                // Reasoning only means something to the model that wrote it.
                let state = reasoning_state.as_ref().filter(|s| s.model == model);
                wire_assistant(&mut out, content, tool_calls, state);
            }
            ChatMessage::Tool {
                call_id,
                content,
                images: attached,
                ..
            } => {
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content,
                }));
                if !attached.is_empty() {
                    images.push(wire_user(
                        &format!("Images from tool call {call_id}"),
                        attached,
                    ));
                }
            }
        }
        // Complete all tool replies before adding their multimodal content.
        if !matches!(messages.get(index + 1), Some(ChatMessage::Tool { .. })) {
            out.append(&mut images);
        }
    }
    out
}

/// An assistant message as input items. With saved reasoning, the items go
/// back in their original order and with their ids (a reasoning item must
/// be followed by the item it led to), but carry the message's current text
/// and calls, which hooks may have changed.
fn wire_assistant(
    out: &mut Vec<Value>,
    content: &str,
    tool_calls: &[ToolCall],
    state: Option<&ReasoningState>,
) {
    let call_item = |c: &ToolCall| {
        json!({
            "type": "function_call",
            "call_id": c.id,
            "name": c.name,
            "arguments": c.arguments,
        })
    };
    let start = out.len();
    let mut sent_text = content.is_empty();
    let mut sent_calls = vec![false; tool_calls.len()];
    for item in state.map(|s| s.items.as_slice()).unwrap_or_default() {
        match item["type"].as_str() {
            // Unsealed reasoning cannot be sent back statelessly.
            Some("reasoning") if item["encrypted_content"].is_string() => out.push(item.clone()),
            Some("message") if !sent_text => {
                sent_text = true;
                out.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "id": item["id"],
                    "status": "completed",
                    "content": [{ "type": "output_text", "text": content, "annotations": [] }],
                }));
            }
            Some("function_call") => {
                let Some(i) = tool_calls
                    .iter()
                    .position(|c| item["call_id"].as_str() == Some(&c.id))
                else {
                    continue;
                };
                sent_calls[i] = true;
                let mut call = call_item(&tool_calls[i]);
                call["id"] = item["id"].clone();
                out.push(call);
            }
            _ => {}
        }
    }
    // Reasoning whose answer is gone would be rejected.
    while out.len() > start && out.last().is_some_and(|i| i["type"] == "reasoning") {
        out.pop();
    }
    if !sent_text {
        out.push(json!({ "role": "assistant", "content": content }));
    }
    for (call, sent) in tool_calls.iter().zip(sent_calls) {
        if !sent {
            out.push(call_item(call));
        }
    }
}

fn wire_user(content: &str, images: &[ImageAttachment]) -> Value {
    if images.is_empty() {
        return json!({ "role": "user", "content": content });
    }
    let mut parts = Vec::new();
    if !content.is_empty() {
        parts.push(json!({ "type": "input_text", "text": content }));
    }
    for image in images {
        parts.push(json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.data.as_deref().unwrap_or_default()),
        }));
    }
    json!({ "role": "user", "content": parts })
}

// ---- stream events -------------------------------------------------------

#[derive(Default)]
struct Accumulator {
    content: String,
    reasoning: String,
    /// Finished output items, by output index.
    items: Vec<(u64, Value)>,
    /// The final output list from `response.completed`, when given.
    output: Option<Vec<Value>>,
    usage: Option<Usage>,
    /// `response.completed` arrived: without it the answer is cut short.
    completed: bool,
}

impl Accumulator {
    /// Apply one SSE data payload. Returns true once the response is done.
    fn apply(&mut self, data: &str, on_delta: DeltaSink<'_>) -> Result<bool, ProviderError> {
        if data.trim() == "[DONE]" {
            return Ok(true);
        }
        let ev: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError(format!("bad stream event ({e}): {data}")))?;
        let text = || {
            ev["delta"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
        };
        match ev["type"].as_str().unwrap_or_default() {
            _ if ev["error"].is_object() && ev["type"].is_null() => {
                return Err(ProviderError(format!("provider error: {}", ev["error"])));
            }
            "response.output_text.delta" => {
                if let Some(t) = text() {
                    self.content.push_str(&t);
                    on_delta(Delta::Text(t));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(t) = text() {
                    self.reasoning.push_str(&t);
                    on_delta(Delta::Reasoning(t));
                }
            }
            "response.reasoning_summary_part.added" if !self.reasoning.is_empty() => {
                self.reasoning.push_str("\n\n");
                on_delta(Delta::Reasoning("\n\n".into()));
            }
            "response.output_item.done" => {
                let index = ev["output_index"]
                    .as_u64()
                    .unwrap_or(self.items.len() as u64);
                self.items.push((index, ev["item"].clone()));
            }
            "response.incomplete" => {
                let reason = &ev["response"]["incomplete_details"]["reason"];
                let reason = reason.as_str().unwrap_or("no reason given");
                return Err(ProviderError(format!("response incomplete: {reason}")));
            }
            "response.completed" => {
                self.completed = true;
                let response = &ev["response"];
                if let Some(output) = response["output"].as_array().filter(|o| !o.is_empty()) {
                    self.output = Some(output.clone());
                }
                let u = &response["usage"];
                if u.is_object() {
                    self.usage = Some(Usage {
                        input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                        context_tokens: None,
                        cached_tokens: u["input_tokens_details"]["cached_tokens"].as_u64(),
                    });
                }
                return Ok(true);
            }
            "response.failed" => {
                let err = &ev["response"]["error"];
                return Err(ProviderError(format!("provider error: {err}")));
            }
            "error" => return Err(ProviderError(format!("provider error: {ev}"))),
            _ => {}
        }
        Ok(false)
    }

    fn finish(mut self, model: &str) -> Result<Completion, ProviderError> {
        if !self.completed {
            return Err(ProviderError(
                "the stream ended before the response completed".into(),
            ));
        }
        self.items.sort_by_key(|(i, _)| *i);
        let items = self
            .output
            .unwrap_or_else(|| self.items.into_iter().map(|(_, item)| item).collect());
        let tool_calls = items
            .iter()
            .filter(|i| i["type"] == "function_call")
            .enumerate()
            .map(|(n, i)| ToolCall {
                id: i["call_id"]
                    .as_str()
                    .map_or_else(|| format!("call_{n}"), str::to_owned),
                name: i["name"].as_str().unwrap_or_default().to_owned(),
                arguments: i["arguments"].as_str().unwrap_or("{}").to_owned(),
            })
            .collect();
        // A server that did not stream the text still sent it whole.
        if self.content.is_empty() {
            self.content = items
                .iter()
                .filter(|i| i["type"] == "message")
                .flat_map(|i| i["content"].as_array().cloned().unwrap_or_default())
                .filter_map(|part| part["text"].as_str().map(str::to_owned))
                .collect();
        }
        // Only sealed reasoning can go back (a gateway may drop the seal).
        let items: Vec<Value> = items
            .into_iter()
            .filter(|i| i["type"] != "reasoning" || i["encrypted_content"].is_string())
            .collect();
        let keep = items.iter().any(|i| i["type"] == "reasoning");
        Ok(Completion {
            content: self.content,
            reasoning: self.reasoning,
            tool_calls,
            usage: self.usage,
            reasoning_state: keep.then(|| ReasoningState {
                model: model.to_owned(),
                items,
                prefix: None,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_reasoning_items_and_replays_them() {
        let mut acc = Accumulator::default();
        let mut deltas = Vec::new();
        let mut sink = |d: Delta| deltas.push(d);
        let reasoning =
            json!({"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"sealed"});
        let call = json!({"type":"function_call","id":"fc_1","call_id":"call_a","name":"shell","arguments":"{}"});
        for ev in [
            json!({"type":"response.reasoning_summary_text.delta","delta":"think"}),
            json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
            json!({"type":"response.output_item.done","output_index":1,"item":call}),
            json!({"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":5,"input_tokens_details":{"cached_tokens":4}}}}),
        ] {
            if acc.apply(&ev.to_string(), &mut sink).unwrap() {
                break;
            }
        }
        let c = acc.finish("gpt-x").unwrap();
        assert_eq!(deltas, vec![Delta::Reasoning("think".into())]);
        assert_eq!(c.reasoning, "think");
        assert_eq!(c.tool_calls[0].id, "call_a");
        assert_eq!(c.usage.unwrap().cached_tokens, Some(4));
        let state = c.reasoning_state.clone().unwrap();
        assert_eq!(state.items, vec![reasoning.clone(), call]);

        let msg = ChatMessage::Assistant {
            content: String::new(),
            reasoning: c.reasoning,
            tool_calls: c.tool_calls,
            reasoning_state: c.reasoning_state,
        };
        let replay =
            json!({"type":"function_call","call_id":"call_a","name":"shell","arguments":"{}"});
        let mut with_id = replay.clone();
        with_id["id"] = json!("fc_1");
        assert_eq!(
            wire_input(std::slice::from_ref(&msg), "gpt-x"),
            vec![reasoning, with_id]
        );
        // Another model gets the calls without the reasoning.
        assert_eq!(wire_input(&[msg], "gpt-y"), vec![replay]);
    }
}
