//! The format every front end converts to, and the backends that answer it.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{Value, json};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// What the model said.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    pub reasoning: String,
    pub text: String,
    pub calls: Vec<Call>,
}

impl Reply {
    pub fn text(text: &str) -> Self {
        Reply {
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn call(name: &str, arguments: Value) -> Self {
        Reply {
            calls: vec![Call {
                id: String::new(),
                name: name.into(),
                arguments: arguments.to_string(),
            }],
            ..Default::default()
        }
    }

    pub fn thinking(mut self, reasoning: &str) -> Self {
        self.reasoning = reasoning.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    System(String),
    User { text: String, images: Vec<String> },
    Assistant { text: String, calls: Vec<Call> },
    Tool { call_id: String, output: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// A request, whatever format it came in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conv {
    pub model: String,
    pub messages: Vec<Msg>,
    pub tools: Vec<Tool>,
}

impl Conv {
    /// Rough tokens, for usage reports.
    pub fn tokens(&self) -> u64 {
        let chars: usize = self
            .messages
            .iter()
            .map(|m| match m {
                Msg::System(t) => t.len(),
                Msg::User { text, .. } => text.len(),
                Msg::Assistant { text, calls } => {
                    text.len() + calls.iter().map(|c| c.arguments.len()).sum::<usize>()
                }
                Msg::Tool { output, .. } => output.len(),
            })
            .sum();
        (chars / 4) as u64 + 1
    }
}

pub fn tokens(text: &str) -> u64 {
    (text.len() / 4) as u64 + 1
}

/// Who writes the replies.
pub enum Backend {
    /// An OpenAI-compatible `/chat/completions` server (e.g. tabbyAPI).
    Live {
        base_url: String,
        model: String,
        http: reqwest::Client,
    },
    /// Replies handed out in order; tests queue them.
    Scripted(Mutex<VecDeque<Reply>>),
}

impl Backend {
    pub fn live(base_url: &str, model: &str) -> Self {
        Backend::Live {
            base_url: base_url.trim_end_matches('/').into(),
            model: model.into(),
            http: reqwest::Client::new(),
        }
    }

    pub fn scripted(replies: impl IntoIterator<Item = Reply>) -> Self {
        Backend::Scripted(Mutex::new(replies.into_iter().collect()))
    }

    pub async fn reply(&self, conv: &Conv) -> Result<Reply, String> {
        match self {
            Backend::Scripted(queue) => queue
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "fake: the script has no replies left".into()),
            Backend::Live {
                base_url,
                model,
                http,
            } => {
                let body = live_request(model, conv);
                let resp = http
                    .post(format!("{base_url}/chat/completions"))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| format!("fake: backend unreachable: {e}"))?;
                let status = resp.status();
                let v: Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("fake: bad backend reply: {e}"))?;
                if !status.is_success() {
                    return Err(format!("fake: backend said {status}: {v}"));
                }
                let m = &v["choices"][0]["message"];
                let reasoning = [&m["reasoning_content"], &m["reasoning"]]
                    .into_iter()
                    .find_map(|r| r.as_str().filter(|r| !r.is_empty()))
                    .unwrap_or_default();
                let calls = m["tool_calls"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| Call {
                        id: c["id"].as_str().unwrap_or_default().into(),
                        name: c["function"]["name"].as_str().unwrap_or_default().into(),
                        arguments: c["function"]["arguments"].as_str().unwrap_or("{}").into(),
                    })
                    .collect();
                Ok(Reply {
                    reasoning: reasoning.trim().into(),
                    text: m["content"].as_str().unwrap_or_default().trim().into(),
                    calls,
                })
            }
        }
    }
}

fn live_request(model: &str, conv: &Conv) -> Value {
    let messages: Vec<Value> = conv
        .messages
        .iter()
        .map(|m| match m {
            Msg::System(t) => json!({ "role": "system", "content": t }),
            Msg::User { text, images } if images.is_empty() => {
                json!({ "role": "user", "content": text })
            }
            Msg::User { text, images } => {
                let mut parts = vec![json!({ "type": "text", "text": text })];
                parts.extend(
                    images
                        .iter()
                        .map(|url| json!({ "type": "image_url", "image_url": { "url": url } })),
                );
                json!({ "role": "user", "content": parts })
            }
            Msg::Assistant { text, calls } => {
                let mut m = json!({ "role": "assistant", "content": text });
                if !calls.is_empty() {
                    m["tool_calls"] = calls
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id, "type": "function",
                                "function": { "name": c.name, "arguments": c.arguments },
                            })
                        })
                        .collect();
                }
                m
            }
            Msg::Tool { call_id, output } => {
                json!({ "role": "tool", "tool_call_id": call_id, "content": output })
            }
        })
        .collect();
    let mut body = json!({ "model": model, "messages": messages, "stream": false });
    if !conv.tools.is_empty() {
        body["tools"] = conv
            .tools
            .iter()
            .map(|t| {
                json!({ "type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters,
                }})
            })
            .collect();
    }
    body
}
