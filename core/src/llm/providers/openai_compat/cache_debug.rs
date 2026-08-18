//! Best-effort, opt-in diagnostics for prompt-cache behavior.
use crate::llm::provider::{ChatEvent, ResponseStream};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(crate) struct CacheDebug(Arc<Inner>);
struct Inner {
    file: Mutex<File>,
    state: Mutex<State>,
    salt: [u8; 32],
}
#[derive(Default)]
struct State {
    previous: BTreeMap<String, Snapshot>,
    aggregate: Aggregate,
}

#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct Aggregate {
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    pub reported_with_cache_count: u64,
    pub reported_without_cache_count: u64,
    pub missing_usage_count: u64,
    pub failures: u64,
    pub abandoned: u64,
}

#[derive(Clone, Debug)]
struct MessageSnapshot {
    total: String,
    fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    messages: Vec<MessageSnapshot>,
    system: Option<String>,
    tools: Vec<String>,
    timestamp_ms: u64,
}

impl CacheDebug {
    pub(crate) fn from_env() -> Option<Self> {
        let value = std::env::var("BONE_OAI_CACHE_DEBUG").ok()?;
        if value.is_empty() || value == "0" {
            return None;
        }
        let root = crate::config::try_bone_dir()?;
        let dir = root.join("cache-debug");
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let now = timestamp();
        let path = dir.join(format!(
            "openai-compat-{}-{now}-{n}.jsonl",
            std::process::id()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(path)
            .ok()?;
        let mut salt = [0u8; 32];
        if getrandom::fill(&mut salt).is_err() {
            let mut fallback = Sha256::new();
            fallback.update(std::process::id().to_le_bytes());
            fallback.update(now.to_le_bytes());
            fallback.update(n.to_le_bytes());
            salt.copy_from_slice(&fallback.finalize());
        }
        Some(Self(Arc::new(Inner {
            file: Mutex::new(file),
            state: Mutex::new(State::default()),
            salt,
        })))
    }

    pub(crate) fn start(
        &self,
        provider: &str,
        model: &str,
        scope: Option<&str>,
        request: &crate::llm::providers::openai_compat::ChatRequest,
    ) -> RequestDebug {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let scope_value = scope.unwrap_or("");
        let scope_kind = if scope_value.starts_with("conversation-") {
            "conversation"
        } else if scope_value.starts_with("run-") {
            "run"
        } else if scope_value.is_empty() {
            "none"
        } else {
            "other"
        };
        let snap = snapshot(self, request);
        let key = format!("{provider}\0{model}\0{}", hash(self, scope_value));
        let diff = {
            let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
            let old = state.previous.insert(key, snap.clone());
            old.map(|old| diff(&old, &snap))
        };
        let mut event = json!({
            "event": "request_started",
            "timestamp_ms": snap.timestamp_ms,
            "request_id": id,
            "provider": provider,
            "model": model,
            "scope_kind": scope_kind,
            "scope_hash": hash(self, scope_value),
            "prompt_cache_key_present": request.prompt_cache_key.is_some(),
            "prompt_cache_key_hash": request.prompt_cache_key.as_deref().map(|v| hash(self, v)),
            "request_options": {
                "stream": request.stream,
                "stream_usage": request.stream_options.as_ref().is_some_and(|options| options.include_usage),
                "max_tokens": request.max_tokens,
                "reasoning_effort": request.reasoning_effort.as_deref(),
            },
            "message_count": snap.messages.len(),
            "messages": message_events(self, request),
            "tool_count": request.tools.len(),
            "tools": tool_events(self, request)
        });
        if let Some(d) = diff {
            event["prior_diff"] = d;
        }
        self.write(event);
        RequestDebug {
            debug: self.clone(),
            id,
            done: false,
        }
    }

    fn write(&self, event: Value) {
        if let Ok(mut file) = self.0.file.lock() {
            let _ = writeln!(file, "{}", event);
            let _ = file.flush();
        }
    }
    fn finish(&self, request: &RequestDebug, outcome: &str, usage: Option<Usage>) {
        let usage_source = if usage.is_some() {
            "reported"
        } else {
            "missing"
        };
        let (prompt, completion, cached, uncached, rate, anomaly) =
            usage.map_or((None, None, None, None, None, false), |usage| {
                let metrics = usage_metrics(usage);
                (
                    Some(usage.prompt),
                    Some(usage.completion),
                    usage.cached,
                    metrics.uncached,
                    metrics.rate,
                    metrics.cached_greater_than_prompt,
                )
            });
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        account(&mut state.aggregate, outcome, usage);
        let a = &state.aggregate;
        let summary = json!({
            "event": "summary",
            "timestamp_ms": timestamp(),
            "reported_with_cache_count": a.reported_with_cache_count,
            "reported_with_cache_prompt_tokens": a.prompt,
            "reported_with_cache_completion_tokens": a.completion,
            "reported_with_cache_cached_tokens": a.cached,
            "reported_with_cache_rate": if a.prompt == 0 { Value::Null } else { json!(cache_rate(a.prompt, a.cached)) },
            "reported_without_cache_count": a.reported_without_cache_count,
            "missing_usage_count": a.missing_usage_count,
            "failures": a.failures,
            "abandoned": a.abandoned
        });
        drop(state);
        self.write(json!({
            "event": "request_result",
            "timestamp_ms": timestamp(),
            "request_id": request.id,
            "usage_source": usage_source,
            "outcome": outcome,
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "cached_tokens": cached,
            "uncached_tokens": uncached,
            "cache_rate": rate,
            "cached_greater_than_prompt": anomaly
        }));
        self.write(summary);
    }
}

pub(crate) struct RequestDebug {
    debug: CacheDebug,
    id: u64,
    done: bool,
}
impl RequestDebug {
    pub(crate) fn send_error(&mut self) {
        self.finish("send_error", None);
    }
    pub(crate) fn http_error(&mut self) {
        self.finish("http_error", None);
    }
    pub(crate) fn stream(self, inner: ResponseStream) -> ResponseStream {
        Box::pin(DiagnosticStream {
            inner,
            request: Some(self),
        })
    }
    fn finish(&mut self, outcome: &str, usage: Option<Usage>) {
        if !self.done {
            self.done = true;
            self.debug.finish(self, outcome, usage);
        }
    }
}
impl Drop for RequestDebug {
    fn drop(&mut self) {
        self.finish("abandoned_or_cancelled", None);
    }
}
struct DiagnosticStream {
    inner: ResponseStream,
    request: Option<RequestDebug>,
}
impl futures_util::Stream for DiagnosticStream {
    type Item = Result<ChatEvent, crate::llm::provider::LlmError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                if let ChatEvent::TokenUsage {
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                    ..
                } = &event
                    && let Some(mut request) = self.request.take()
                {
                    request.finish(
                        "completed",
                        Some(Usage {
                            prompt: *prompt_tokens,
                            completion: *completion_tokens,
                            cached: *cached_tokens,
                        }),
                    );
                }
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(Some(Err(error))) => {
                if let Some(mut r) = self.request.take() {
                    r.finish("stream_error", None);
                }
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                if let Some(mut request) = self.request.take() {
                    request.finish("completed", None);
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
impl Drop for DiagnosticStream {
    fn drop(&mut self) {
        if let Some(mut r) = self.request.take() {
            r.finish("abandoned_or_cancelled", None);
        }
    }
}

#[derive(Clone, Copy)]
struct Usage {
    prompt: u32,
    completion: u32,
    cached: Option<u32>,
}

#[derive(Debug, PartialEq)]
struct UsageMetrics {
    uncached: Option<u32>,
    rate: Option<f64>,
    cached_greater_than_prompt: bool,
}

fn usage_metrics(usage: Usage) -> UsageMetrics {
    let Some(cached) = usage.cached else {
        return UsageMetrics {
            uncached: None,
            rate: None,
            cached_greater_than_prompt: false,
        };
    };
    UsageMetrics {
        uncached: Some(usage.prompt.saturating_sub(cached)),
        rate: Some(cache_rate(usage.prompt as u64, cached as u64)),
        cached_greater_than_prompt: cached > usage.prompt,
    }
}
fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
fn hash(debug: &CacheDebug, text: &str) -> String {
    fingerprint_with_salt(&debug.0.salt, text)
}
fn snapshot(debug: &CacheDebug, r: &crate::llm::providers::openai_compat::ChatRequest) -> Snapshot {
    let messages = r
        .messages
        .iter()
        .map(|m| {
            let serialized = serde_json::to_string(m).unwrap_or_default();
            let mut fields = BTreeMap::new();
            fields.insert("role".into(), hash(debug, &m.role));
            if let Some(content) = &m.content {
                match content {
                    crate::llm::providers::openai_compat::OaiContent::Text(text) => {
                        fields.insert("content.text".into(), hash(debug, text));
                    }
                    crate::llm::providers::openai_compat::OaiContent::Parts(parts) => {
                        for (i, part) in parts.iter().enumerate() {
                            let key = format!("content.parts[{i}]");
                            fields.insert(
                                key,
                                hash(debug, &serde_json::to_string(part).unwrap_or_default()),
                            );
                        }
                    }
                }
            }
            for (i, call) in m.tool_calls.iter().enumerate() {
                fields.insert(format!("tool_calls[{i}].id"), hash(debug, &call.id));
                fields.insert(
                    format!("tool_calls[{i}].name"),
                    hash(debug, &call.function.name),
                );
                fields.insert(
                    format!("tool_calls[{i}].arguments"),
                    hash(debug, &call.function.arguments),
                );
            }
            if let Some(id) = &m.tool_call_id {
                fields.insert("tool_call_id".into(), hash(debug, id));
            }
            if let Some(name) = &m.name {
                fields.insert("name".into(), hash(debug, name));
            }
            for (key, value) in &m.reasoning {
                fields.insert(format!("reasoning.{key}"), hash(debug, value));
            }
            MessageSnapshot {
                total: hash(debug, &serialized),
                fields,
            }
        })
        .collect::<Vec<_>>();
    let tools = r
        .tools
        .iter()
        .map(|tool| hash(debug, &serde_json::to_string(tool).unwrap_or_default()))
        .collect();
    Snapshot {
        system: r
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| hash(debug, &serde_json::to_string(m).unwrap_or_default())),
        messages,
        tools,
        timestamp_ms: timestamp(),
    }
}

fn message_events(
    debug: &CacheDebug,
    r: &crate::llm::providers::openai_compat::ChatRequest,
) -> Vec<Value> {
    r.messages.iter().enumerate().map(|(index, m)| {
        let serialized = serde_json::to_string(m).unwrap_or_default();
        let mut fields = serde_json::Map::new();
        fields.insert("role".into(), json!(m.role));
        if let Some(content) = &m.content {
            match content {
                crate::llm::providers::openai_compat::OaiContent::Text(text) => { fields.insert("content.text".into(), json!({"len": text.len(), "fingerprint": hash(debug, text)})); }
                crate::llm::providers::openai_compat::OaiContent::Parts(parts) => {
                    for (i, part) in parts.iter().enumerate() {
                        let descriptor = match part {
                            crate::llm::providers::openai_compat::OaiPart::Text { text } => json!({"kind":"text", "len":text.len(), "fingerprint":hash(debug,text)}),
                            crate::llm::providers::openai_compat::OaiPart::ImageUrl { image_url } => {
                                let (media_type, encoded) = image_url.url.strip_prefix("data:").and_then(|s| s.split_once(";base64,")).map_or(("unknown", image_url.url.as_str()), |x| x);
                                let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).map_or(0, |v| v.len());
                                json!({"kind":"image", "media_type":media_type, "base64_len":encoded.len(), "decoded_len":decoded, "fingerprint":hash(debug,encoded)})
                            }
                        };
                        fields.insert(format!("content.parts[{i}]"), descriptor);
                    }
                }
            }
        }
        for (i, call) in m.tool_calls.iter().enumerate() { fields.insert(format!("tool_calls[{i}]"), json!({"id":call.id,"name":call.function.name,"arguments_len":call.function.arguments.len(),"arguments_fingerprint":hash(debug,&call.function.arguments)})); }
        if let Some(id) = &m.tool_call_id { fields.insert("tool_call_id".into(), json!(id)); }
        if let Some(name) = &m.name { fields.insert("name".into(), json!(name)); }
        for (key, value) in &m.reasoning { fields.insert(format!("reasoning.{key}"), json!({"key":key,"len":value.len(),"fingerprint":hash(debug,value)})); }
        json!({"index":index,"role":m.role,"serialized_bytes":serialized.len(),"fingerprint":hash(debug,&serialized),"rough_token_estimate":rough_message_tokens(m),"fields":fields})
    }).collect()
}
fn tool_events(
    debug: &CacheDebug,
    r: &crate::llm::providers::openai_compat::ChatRequest,
) -> Vec<Value> {
    r.tools.iter().enumerate().map(|(index,t)| { let serialized=serde_json::to_string(t).unwrap_or_default(); json!({"index":index,"function_name":t.function.name,"serialized_bytes":serialized.len(),"fingerprint":hash(debug,&serialized),"description_len":t.function.description.len(),"description_fingerprint":hash(debug,&t.function.description),"parameters_len":serde_json::to_vec(&t.function.parameters).map_or(0,|v|v.len()),"parameters_fingerprint":hash(debug,&t.function.parameters.to_string())}) }).collect()
}
fn rough_message_tokens(m: &crate::llm::providers::openai_compat::OpenAiMessage) -> u32 {
    serde_json::to_string(m).map_or(0, |s| (s.len() / 4) as u32)
}

pub(crate) fn classify<T: PartialEq>(old: &[T], new: &[T]) -> &'static str {
    let common = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if old == new {
        "identical"
    } else if common == old.len() && new.len() > old.len() {
        "append"
    } else if common == new.len() && old.len() > new.len() {
        "truncate"
    } else if common == 0 {
        "reset"
    } else {
        "mutate"
    }
}
pub(crate) fn fingerprint_with_salt(salt: &[u8], text: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}
fn diff(old: &Snapshot, new: &Snapshot) -> Value {
    let old_totals: Vec<_> = old.messages.iter().map(|m| &m.total).collect();
    let new_totals: Vec<_> = new.messages.iter().map(|m| &m.total).collect();
    let common = old_totals
        .iter()
        .zip(&new_totals)
        .take_while(|(a, b)| a == b)
        .count();
    let changed_fields: Vec<Value> = old
        .messages
        .iter()
        .zip(&new.messages)
        .enumerate()
        .filter_map(|(i, (a, b))| {
            let keys = a
                .fields
                .keys()
                .chain(b.fields.keys())
                .collect::<std::collections::BTreeSet<_>>();
            let changed = keys
                .into_iter()
                .filter(|k| a.fields.get(*k) != b.fields.get(*k))
                .map(|k| json!(k))
                .collect::<Vec<_>>();
            (!changed.is_empty()).then(|| json!({"index":i,"fields":changed}))
        })
        .collect();
    let tool_changed = old.tools != new.tools;
    json!({
        "first_changed_message": (common < old.messages.len() && common < new.messages.len()).then_some(common),
        "changed_fields": changed_fields,
        "longest_common_message_prefix": common,
        "classification": classify(&old_totals, &new_totals),
        "system_message_changed": old.system != new.system,
        "tool_definition_changed": tool_changed,
        "gap_ms": new.timestamp_ms.saturating_sub(old.timestamp_ms),
    })
}
fn cache_rate(prompt: u64, cached: u64) -> f64 {
    if prompt == 0 {
        0.0
    } else {
        cached as f64 / prompt as f64
    }
}
fn account(a: &mut Aggregate, outcome: &str, usage: Option<Usage>) {
    match outcome {
        "abandoned_or_cancelled" => a.abandoned += 1,
        "send_error" | "http_error" | "stream_error" => a.failures += 1,
        _ => {}
    }
    if outcome == "completed" {
        match usage {
            Some(u) if u.cached.is_some() => {
                a.prompt += u.prompt as u64;
                a.completion += u.completion as u64;
                a.cached += u.cached.unwrap_or(0) as u64;
                a.reported_with_cache_count += 1
            }
            Some(_) => a.reported_without_cache_count += 1,
            None => a.missing_usage_count += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::providers::openai_compat::{
        ChatRequest, OaiContent, OaiImageUrl, OaiPart, OpenAiFunction, OpenAiMessage, OpenAiTool,
        OpenAiToolCall, OpenAiToolCallFunction, StreamOptions,
    };

    fn debug() -> CacheDebug {
        CacheDebug(Arc::new(Inner {
            file: Mutex::new(tempfile::tempfile().expect("temporary diagnostic file")),
            state: Mutex::new(State::default()),
            salt: [7; 32],
        }))
    }

    fn message(role: &str, content: OaiContent) -> OpenAiMessage {
        OpenAiMessage {
            role: role.to_string(),
            content: Some(content),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            reasoning: BTreeMap::new(),
        }
    }

    fn request(messages: Vec<OpenAiMessage>, tools: Vec<OpenAiTool>) -> ChatRequest {
        ChatRequest {
            model: "test-model".to_string(),
            messages,
            stream: true,
            tools,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            max_tokens: None,
            reasoning_effort: None,
            prompt_cache_key: Some("conversation-1".to_string()),
        }
    }

    fn tool(description: &str, parameters: Value) -> OpenAiTool {
        OpenAiTool {
            r#type: "function",
            function: OpenAiFunction {
                name: "safe_tool".to_string(),
                description: description.to_string(),
                parameters,
            },
        }
    }

    #[test]
    fn classifies_ordered_message_changes() {
        let old = ["a", "b"];
        assert_eq!(classify(&old, &old), "identical");
        assert_eq!(classify(&old, &["a", "b", "c"]), "append");
        assert_eq!(classify(&old, &["a"]), "truncate");
        assert_eq!(classify(&old, &["a", "x"]), "mutate");
        assert_eq!(classify(&old, &["x"]), "reset");
    }

    #[test]
    fn diff_identifies_system_tools_and_changed_fields() {
        let debug = debug();
        let old = request(
            vec![
                message("system", OaiContent::Text("stable system".to_string())),
                message("user", OaiContent::Text("old user".to_string())),
            ],
            vec![tool("old description", json!({"type": "object"}))],
        );
        let new = request(
            vec![
                message("system", OaiContent::Text("changed system".to_string())),
                message("user", OaiContent::Text("new user".to_string())),
            ],
            vec![tool("new description", json!({"type": "object"}))],
        );
        let mut old_snapshot = snapshot(&debug, &old);
        old_snapshot.timestamp_ms = 100;
        let mut new_snapshot = snapshot(&debug, &new);
        new_snapshot.timestamp_ms = 175;

        let change = diff(&old_snapshot, &new_snapshot);
        assert_eq!(change["classification"], "reset");
        assert_eq!(change["first_changed_message"], 0);
        assert_eq!(change["system_message_changed"], true);
        assert_eq!(change["tool_definition_changed"], true);
        assert_eq!(change["gap_ms"], 75);
        assert_eq!(change["changed_fields"][0]["index"], 0);
        assert!(
            change["changed_fields"][0]["fields"]
                .as_array()
                .is_some_and(|fields| fields.iter().any(|field| field == "content.text"))
        );
    }

    #[test]
    fn aggregation_is_token_weighted_and_excludes_unknown_cache_usage() {
        let mut aggregate = Aggregate::default();
        account(
            &mut aggregate,
            "completed",
            Some(Usage {
                prompt: 100,
                completion: 10,
                cached: Some(0),
            }),
        );
        account(
            &mut aggregate,
            "completed",
            Some(Usage {
                prompt: 10_000,
                completion: 20,
                cached: Some(9_900),
            }),
        );
        account(
            &mut aggregate,
            "completed",
            Some(Usage {
                prompt: 500,
                completion: 5,
                cached: None,
            }),
        );
        account(&mut aggregate, "completed", None);
        account(&mut aggregate, "stream_error", None);
        account(&mut aggregate, "abandoned_or_cancelled", None);

        assert_eq!(aggregate.prompt, 10_100);
        assert_eq!(aggregate.cached, 9_900);
        assert_eq!(aggregate.reported_with_cache_count, 2);
        assert_eq!(aggregate.reported_without_cache_count, 1);
        assert_eq!(aggregate.missing_usage_count, 1);
        assert_eq!(aggregate.failures, 1);
        assert_eq!(aggregate.abandoned, 1);
        assert!((cache_rate(aggregate.prompt, aggregate.cached) - 0.980_198).abs() < 0.000_001);
    }

    #[test]
    fn usage_metrics_preserve_unknown_and_flag_provider_anomalies() {
        assert_eq!(
            usage_metrics(Usage {
                prompt: 10,
                completion: 1,
                cached: None,
            }),
            UsageMetrics {
                uncached: None,
                rate: None,
                cached_greater_than_prompt: false,
            }
        );
        assert_eq!(
            usage_metrics(Usage {
                prompt: 10,
                completion: 1,
                cached: Some(14),
            }),
            UsageMetrics {
                uncached: Some(0),
                rate: Some(1.4),
                cached_greater_than_prompt: true,
            }
        );
    }

    #[test]
    fn safe_diagnostics_do_not_contain_request_plaintext() {
        let debug = debug();
        let mut assistant = message(
            "assistant",
            OaiContent::Text("SECRET_CONTENT_7F29".to_string()),
        );
        assistant.tool_calls.push(OpenAiToolCall {
            id: "call-safe".to_string(),
            r#type: "function",
            function: OpenAiToolCallFunction {
                name: "safe_tool".to_string(),
                arguments: "SECRET_ARGUMENTS_8B31".to_string(),
            },
        });
        assistant.reasoning.insert(
            "reasoning_content".to_string(),
            "SECRET_REASONING_4E12".to_string(),
        );
        let image = message(
            "user",
            OaiContent::Parts(vec![OaiPart::ImageUrl {
                image_url: OaiImageUrl {
                    url: "data:image/png;base64,SECRET_IMAGE_9A77".to_string(),
                },
            }]),
        );
        let request = request(
            vec![assistant, image],
            vec![tool(
                "SECRET_DESCRIPTION_3C44",
                json!({"description": "SECRET_SCHEMA_2D55"}),
            )],
        );

        let safe = json!({
            "messages": message_events(&debug, &request),
            "tools": tool_events(&debug, &request),
        })
        .to_string();
        for secret in [
            "SECRET_CONTENT_7F29",
            "SECRET_ARGUMENTS_8B31",
            "SECRET_REASONING_4E12",
            "SECRET_IMAGE_9A77",
            "SECRET_DESCRIPTION_3C44",
            "SECRET_SCHEMA_2D55",
        ] {
            assert!(!safe.contains(secret), "diagnostic leaked {secret}");
        }
        assert!(safe.contains("safe_tool"));
        assert!(safe.contains("image/png"));
        assert!(safe.contains("fingerprint"));
    }
}
