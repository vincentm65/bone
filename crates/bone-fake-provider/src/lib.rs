//! A strict stand-in for model APIs. It speaks OpenAI Chat Completions
//! (`/v1/chat/completions`) and Responses (`/v1/responses`) and Anthropic
//! Messages (`/v1/messages`), checks requests the way those APIs do, and
//! answers with a local model or a script.
//!
//! Reasoning is sealed like OpenAI's encrypted reasoning: only this server,
//! for the same model, accepts it back, and only followed by the item it led
//! to. Anthropic thinking is signed with the conversation it was written in,
//! and prompt caching is simulated (breakpoints, the 20-block lookback, a
//! minimum prefix), so cache hits can be checked. Path prefixes before `/v1` pick a flavor and inject faults:
//!
//! - `/flavor/<openai|deepseek|qwen|openrouter>`: Chat Completions dialect
//!   (whether reasoning streams, and under which field; whether it may be
//!   sent back). `/flavor/unsealed`: Responses without `encrypted_content`.
//! - `/fault/<name>[@n]`: on every request, or only the n-th one through this
//!   prefix: `cut` (the stream stops early), `incomplete` (the output budget
//!   ran out), `error_body` (an error inside a 200 stream), `http429`,
//!   `http500`, `http529`, `empty` (no output), `slow` (two seconds before
//!   the first byte).
//!
//! `GET /_fake/log` lists every request with what was wrong with it.

mod common;
mod completions;
mod messages;
mod responses;

pub use common::{Backend, Call, Reply};

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Body = UnsyncBoxBody<Bytes, Infallible>;

/// One request the server saw.
#[derive(Debug, Clone)]
pub struct Logged {
    pub path: String,
    pub body: Value,
    pub status: u16,
    /// Why it was rejected, or the fault injected.
    pub error: Option<String>,
    /// Sealed reasoning items it carried that verified.
    pub reasoning_replayed: usize,
    /// Anthropic: prompt tokens read from and written to the cache.
    pub cache_read: u64,
    pub cache_write: u64,
    /// Anthropic: all prompt tokens.
    pub prompt_tokens: u64,
}

pub struct FakeServer {
    /// `http://127.0.0.1:<port>`; append `/v1` (or a prefix and `/v1`).
    pub url: String,
    state: Arc<State>,
}

impl FakeServer {
    /// Serve on `127.0.0.1:port` (0 picks a free port).
    pub async fn start(backend: Backend, port: u16) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let addr: SocketAddr = listener.local_addr()?;
        let state = Arc::new(State {
            backend,
            secret: format!("fake-{}", std::process::id()),
            ids: AtomicU64::new(1),
            counters: Mutex::default(),
            log: Mutex::default(),
            cache: Mutex::default(),
        });
        let served = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    continue;
                };
                let state = served.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |req| {
                        let state = state.clone();
                        async move { Ok::<_, Infallible>(handle(state, req).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(sock), service)
                        .await;
                });
            }
        });
        Ok(FakeServer {
            url: format!("http://{addr}"),
            state,
        })
    }

    pub fn log(&self) -> Vec<Logged> {
        self.state.log.lock().unwrap().clone()
    }
}

pub(crate) struct State {
    pub backend: Backend,
    secret: String,
    ids: AtomicU64,
    counters: Mutex<HashMap<String, u64>>,
    log: Mutex<Vec<Logged>>,
    /// Anthropic prompt cache: prefix fingerprint -> its tokens.
    pub cache: Mutex<HashMap<String, u64>>,
}

impl State {
    pub fn id(&self, prefix: &str) -> String {
        format!("{prefix}_{:012x}", self.ids.fetch_add(1, Ordering::Relaxed))
    }

    /// Seal `payload` so only this server can open it.
    pub fn seal(&self, payload: &Value) -> String {
        let body = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
        format!("{body}.{}", self.mac(&body))
    }

    /// The payload, if this server sealed it.
    pub fn open(&self, sealed: &str) -> Option<Value> {
        let (body, mac) = sealed.rsplit_once('.')?;
        if mac != self.mac(body) {
            return None;
        }
        let raw = base64::engine::general_purpose::STANDARD
            .decode(body)
            .ok()?;
        serde_json::from_slice(&raw).ok()
    }

    fn mac(&self, body: &str) -> String {
        let digest = Sha256::digest(format!("{}:{body}", self.secret));
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    Cut,
    Incomplete,
    ErrorBody,
    Http429,
    Http500,
    Http529,
    Empty,
    Slow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Flavor {
    #[default]
    OpenAi,
    DeepSeek,
    Qwen,
    OpenRouter,
    /// Responses: reasoning items without `encrypted_content`, as a gateway
    /// that drops it sends them.
    Unsealed,
}

/// A rejected request, in OpenAI's error shape.
#[derive(Debug)]
pub(crate) struct ApiError(pub u16, pub String);

impl ApiError {
    pub fn bad(message: impl Into<String>) -> Self {
        ApiError(400, message.into())
    }
}

/// What a front end hands back: the SSE events to stream, and how many
/// sealed reasoning items it accepted.
#[derive(Default)]
pub(crate) struct Streamed {
    pub events: Vec<String>,
    pub reasoning_replayed: usize,
    pub cache_read: u64,
    pub cache_write: u64,
    pub prompt_tokens: u64,
}

pub(crate) fn sse(data: &Value) -> String {
    format!("data: {data}\n\n")
}

/// Text in a few pieces, as a model streams it.
pub(crate) fn pieces(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if cur.len() >= 16 && (ch == ' ' || ch == '\n') {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

async fn handle(state: Arc<State>, req: Request<Incoming>) -> Response<Body> {
    let path = req.uri().path().to_owned();
    let anthropic_version = req.headers().contains_key("anthropic-version");
    if path == "/_fake/log" {
        let log: Vec<Value> = state
            .log
            .lock()
            .unwrap()
            .iter()
            .map(|l| {
                json!({
                    "path": l.path, "status": l.status, "error": l.error,
                    "reasoning_replayed": l.reasoning_replayed,
                    "cache_read": l.cache_read, "cache_write": l.cache_write,
                    "prompt_tokens": l.prompt_tokens,
                })
            })
            .collect();
        return json_response(200, &json!(log));
    }
    let (flavor, faults, route) = match prefix(&path) {
        Ok(p) => p,
        Err(e) => return json_response(404, &error_json(&e)),
    };
    if route.ends_with("/models") {
        return json_response(
            200,
            &json!({ "object": "list", "data": [{ "id": "fake", "object": "model" }] }),
        );
    }
    let bytes = req
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let fault = faults.iter().find_map(|(spec, fault, nth)| {
        let mut counters = state.counters.lock().unwrap();
        let n = counters.entry(spec.clone()).or_default();
        *n += 1;
        nth.is_none_or(|nth| nth == *n).then_some(*fault)
    });
    let mut log = Logged {
        path: path.clone(),
        body: body.clone(),
        status: 200,
        error: fault.map(|f| format!("fault: {f:?}")),
        reasoning_replayed: 0,
        cache_read: 0,
        cache_write: 0,
        prompt_tokens: 0,
    };
    let status = match fault {
        Some(Fault::Http429) => Some(429),
        Some(Fault::Http500) => Some(500),
        Some(Fault::Http529) => Some(529),
        _ => None,
    };
    let result = match (status, route.as_str()) {
        (Some(s), _) => Err(ApiError(s, format!("fake: injected HTTP {s}"))),
        (None, "/v1/chat/completions") => completions::handle(&state, flavor, fault, &body).await,
        (None, "/v1/responses") => responses::handle(&state, flavor, fault, &body).await,
        (None, "/v1/messages") if !anthropic_version => {
            Err(ApiError::bad("anthropic-version: header is required"))
        }
        (None, "/v1/messages") => messages::handle(&state, fault, &body).await,
        (None, other) => Err(ApiError(404, format!("fake: no route {other}"))),
    };
    let response = match result {
        Ok(streamed) => {
            log.reasoning_replayed = streamed.reasoning_replayed;
            log.cache_read = streamed.cache_read;
            log.cache_write = streamed.cache_write;
            log.prompt_tokens = streamed.prompt_tokens;
            stream_response(streamed.events, fault == Some(Fault::Slow))
        }
        Err(ApiError(status, message)) => {
            log.status = status;
            log.error = Some(message.clone());
            json_response(status, &error_json(&message))
        }
    };
    state.log.lock().unwrap().push(log);
    response
}

type Faults = Vec<(String, Fault, Option<u64>)>;

/// `/flavor/x/fault/y@2/v1/...` -> (flavor, faults, `/v1/...`).
fn prefix(path: &str) -> Result<(Flavor, Faults, String), String> {
    let mut flavor = Flavor::default();
    let mut faults = Vec::new();
    let mut rest = path;
    loop {
        if rest.starts_with("/v1/") {
            return Ok((flavor, faults, rest.to_owned()));
        }
        let mut parts = rest.trim_start_matches('/').splitn(3, '/');
        let (Some(key), Some(value)) = (parts.next(), parts.next()) else {
            return Err(format!("fake: no route {path}"));
        };
        let tail = parts.next().unwrap_or_default();
        match key {
            "flavor" => {
                flavor = match value {
                    "openai" => Flavor::OpenAi,
                    "deepseek" => Flavor::DeepSeek,
                    "qwen" => Flavor::Qwen,
                    "openrouter" => Flavor::OpenRouter,
                    "unsealed" => Flavor::Unsealed,
                    other => return Err(format!("fake: unknown flavor {other}")),
                }
            }
            "fault" => {
                let (name, nth) = match value.split_once('@') {
                    Some((name, n)) => (name, n.parse().ok()),
                    None => (value, None),
                };
                let fault = match name {
                    "cut" => Fault::Cut,
                    "incomplete" => Fault::Incomplete,
                    "error_body" => Fault::ErrorBody,
                    "http429" => Fault::Http429,
                    "http500" => Fault::Http500,
                    "http529" => Fault::Http529,
                    "empty" => Fault::Empty,
                    "slow" => Fault::Slow,
                    other => return Err(format!("fake: unknown fault {other}")),
                };
                faults.push((format!("{path}:{value}"), fault, nth));
            }
            other => return Err(format!("fake: unknown prefix {other}")),
        }
        rest = &path[path.len() - tail.len() - 1..];
    }
}

fn error_json(message: &str) -> Value {
    json!({ "error": {
        "message": message, "type": "invalid_request_error", "param": null, "code": null,
    }})
}

fn json_response(status: u16, body: &Value) -> Response<Body> {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())).boxed_unsync())
        .unwrap()
}

/// Stream SSE events a few milliseconds apart, as a model would.
fn stream_response(events: Vec<String>, slow: bool) -> Response<Body> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(16);
    tokio::spawn(async move {
        if slow {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        for event in events {
            if tx.send(Bytes::from(event)).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    let frames = futures_util::stream::unfold(rx, |mut rx| async move {
        let bytes = rx.recv().await?;
        Some((Ok::<_, Infallible>(Frame::data(bytes)), rx))
    });
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(StreamBody::new(frames).boxed_unsync())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        let (flavor, faults, route) = prefix("/flavor/qwen/fault/cut@2/v1/responses").unwrap();
        assert_eq!(flavor, Flavor::Qwen);
        assert_eq!(faults.len(), 1);
        assert_eq!((faults[0].1, faults[0].2), (Fault::Cut, Some(2)));
        assert_eq!(route, "/v1/responses");
        assert_eq!(
            prefix("/v1/chat/completions").unwrap().2,
            "/v1/chat/completions"
        );
        assert!(prefix("/nope/x/v1/responses").is_err());
    }
}
