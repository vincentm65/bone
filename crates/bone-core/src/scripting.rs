//! Core-side Lua: `core.lua` configuration, Lua tools, hooks and questions.
//!
//! The Lua state lives on its own thread (it cannot be shared across the
//! async runtime's workers). The rest of the core talks to it through a job
//! channel. Every job runs as a coroutine, so Lua code can call
//! `bone.ask(question)`: the coroutine is parked, an `ask/requested` event
//! goes out to clients, and the coroutine resumes with whatever a client
//! answers (`ask/respond`), or `nil` if its turn is cancelled. Other jobs keep
//! running meanwhile.
//!
//! Slow work waits the same way: `bone.system`, `bone.sleep` and `bone.http`
//! called from a job yield `{ wait = spec }`; the work runs on a small async
//! runtime owned by the Lua thread and the coroutine resumes with the result.
//! Cancelling a session's turn kills its processes and resumes with `nil`.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use bone_lua::{Side, from_lua, short_error, to_lua};
use bone_proto::methods::{AskRequestedParams, AskResolvedParams};
use bone_proto::types::AskId;
use futures_util::future::BoxFuture;
use mlua::{FromLuaMulti, IntoLuaMulti, Lua, MultiValue, Table, Thread, ThreadStatus, Value};
use serde_json::{Value as Json, json};
use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::config::{CoreConfig, ProviderConfig};
use crate::tools::{Tool, ToolContext, ToolResult, ToolSpec};

/// What loading `core.lua` produced.
pub struct Loaded {
    pub config: CoreConfig,
    pub scripting: Arc<Scripting>,
    pub tools: Vec<ToolSpec>,
    /// Questions asked by Lua, for the core to broadcast.
    pub events: tokio_mpsc::UnboundedReceiver<AskEvent>,
    /// Where it was loaded from, and how, so the core can load it again.
    pub config_dir: PathBuf,
    pub options: LoadOptions,
    /// Every `bone.config.providers` entry, and the one turns use.
    pub providers: HashMap<String, ProviderConfig>,
    pub provider_name: Option<String>,
}

/// How to load: which plugins to leave out, and the question ids shared by
/// every load of one core (so ids stay unique across reloads).
#[derive(Clone, Default)]
pub struct LoadOptions {
    pub disabled: HashSet<String>,
    pub ask_ids: Arc<AtomicU64>,
    /// How Lua reaches the core (sessions).
    pub(crate) host: Arc<crate::runtime::Host>,
}

pub enum AskEvent {
    Requested(AskRequestedParams),
    Resolved(AskResolvedParams),
}

/// Handle to the Lua thread. Dropping the last handle stops the thread.
pub struct Scripting {
    jobs: mpsc::Sender<Job>,
    /// Hook points with at least one hook (`bone.hook` adds to it).
    hooks: Arc<Mutex<HashSet<String>>>,
    system_prompt_fn: bool,
}

impl Drop for Scripting {
    fn drop(&mut self) {
        let _ = self.jobs.send(Job::Stop);
    }
}

/// What hooks did to an event (see `bone.hook`).
#[derive(Debug, Default, PartialEq)]
pub struct HookOutcome {
    /// The event as the hooks left it.
    pub event: Json,
    /// Set when a hook refused (`{ deny = "why" }`) or failed.
    pub deny: Option<String>,
}

enum Job {
    Run {
        function: &'static str,
        args: Vec<Json>,
        session_id: Option<String>,
        done: Done,
    },
    Answer {
        ask_id: AskId,
        answer: Json,
        reply: oneshot::Sender<bool>,
    },
    /// Background work for a waiting coroutine finished.
    Resume {
        wait_id: u64,
        result: Result<Json, String>,
    },
    Cancel {
        session_id: String,
    },
    /// The runtime was replaced and nothing refers to it any more.
    Stop,
}

/// Where a coroutine's result goes once it finishes.
enum Done {
    Tool(oneshot::Sender<ToolResult>),
    Hooks {
        reply: oneshot::Sender<HookOutcome>,
        name: String,
        event: Json,
    },
    Prompt(oneshot::Sender<Result<String, String>>),
    /// The result as JSON (health checks).
    Json(oneshot::Sender<Result<Json, String>>),
    /// A Lua provider: its `emit` calls go to `deltas`, its result to `reply`.
    Provider {
        reply: oneshot::Sender<Result<Json, String>>,
        deltas: tokio_mpsc::UnboundedSender<Json>,
    },
}

/// Everything read out of `bone.config` and `bone._tools`.
struct Extracted {
    providers: HashMap<String, ProviderConfig>,
    provider: Option<String>,
    system_prompt: Option<String>,
    system_prompt_fn: bool,
    data_dir: Option<String>,
    tools: Vec<ToolSpec>,
    hooks: HashSet<String>,
    /// Names registered with `bone.provider.register`.
    lua_providers: HashSet<String>,
    /// Shared with the Lua thread, which adds to it.
    hook_set: Arc<Mutex<HashSet<String>>>,
}

/// Run the runtime, plugins and `<config_dir>/core.lua`, then apply `BONE_*`
/// environment overrides. The Lua thread keeps running for tools and hooks.
pub fn load(config_dir: &Path) -> Result<Loaded, String> {
    load_with(config_dir, &|k| {
        std::env::var(k).ok().filter(|v| !v.is_empty())
    })
}

/// [`load`] with the environment lookup supplied (tests pass none).
pub fn load_with(
    config_dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Loaded, String> {
    load_with_options(config_dir, env, &LoadOptions::default())
}

/// [`load_with`], leaving out disabled plugins.
pub fn load_with_options(
    config_dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    options: &LoadOptions,
) -> Result<Loaded, String> {
    let (init_tx, init_rx) = mpsc::channel();
    let (jobs, job_rx) = mpsc::channel();
    let (events_tx, events) = tokio_mpsc::unbounded_channel();
    let dir = config_dir.to_owned();
    let jobs_tx = jobs.clone();
    let opts = options.clone();
    std::thread::Builder::new()
        .name("bone-lua".into())
        .spawn(move || lua_thread(dir, opts, init_tx, job_rx, jobs_tx, events_tx))
        .map_err(|e| format!("cannot start the Lua thread: {e}"))?;
    let ex = init_rx
        .recv()
        .map_err(|_| "the Lua thread died while loading core.lua".to_string())??;

    let config = resolve(config_dir, &ex, env)?;
    ex.hook_set.lock().unwrap().extend(ex.hooks.iter().cloned());
    let scripting = Arc::new(Scripting {
        jobs,
        hooks: ex.hook_set.clone(),
        system_prompt_fn: ex.system_prompt_fn,
    });
    Ok(Loaded {
        config,
        scripting,
        tools: ex.tools,
        events,
        config_dir: config_dir.to_owned(),
        options: options.clone(),
        provider_name: ex
            .provider
            .clone()
            .or_else(|| (ex.providers.len() == 1).then(|| ex.providers.keys().next().cloned())?),
        providers: ex.providers,
    })
}

/// Combine Lua settings with environment overrides into the final config.
fn resolve(
    config_dir: &Path,
    ex: &Extracted,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<CoreConfig, String> {
    for (name, p) in &ex.providers {
        match &p.kind {
            Some(kind) if !ex.lua_providers.contains(kind) => {
                let mut known: Vec<&String> = ex.lua_providers.iter().collect();
                known.sort();
                return Err(format!(
                    "bone.config.providers.{name}: no Lua provider of type {kind:?} (registered: {})",
                    if known.is_empty() {
                        "none; install its plugin".to_owned()
                    } else {
                        known
                            .iter()
                            .map(|k| k.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ));
            }
            None if p.base_url.is_empty() => {
                return Err(format!("bone.config.providers.{name}: base_url is missing"));
            }
            _ => {}
        }
    }
    let mut provider = match &ex.provider {
        Some(name) => Some(ex.providers.get(name).cloned().ok_or_else(|| {
            format!("bone.config.provider is {name:?}, but bone.config.providers has no such entry")
        })?),
        None if ex.providers.len() == 1 => ex.providers.values().next().cloned(),
        None => None,
    };
    if let Some(base_url) = env("BONE_BASE_URL") {
        let model = env("BONE_MODEL").or_else(|| provider.as_ref().map(|p| p.model.clone()));
        let model = model.ok_or("BONE_BASE_URL is set but BONE_MODEL is not")?;
        provider = Some(ProviderConfig {
            kind: None,
            options: serde_json::Value::Null,
            base_url,
            model,
            api_key: None,
            reasoning_effort: None,
            stream_usage: true,
        });
    } else if let Some(model) = env("BONE_MODEL") {
        let p = provider
            .as_mut()
            .ok_or("BONE_MODEL is set but no provider is configured")?;
        p.model = model;
    }
    let mut provider = provider.ok_or_else(|| {
        format!(
            "no model provider configured. Set bone.config.providers and bone.config.provider in {}, \
             or BONE_BASE_URL and BONE_MODEL",
            config_dir.join("core.lua").display()
        )
    })?;
    if let Some(key) = env("BONE_API_KEY") {
        provider.api_key = Some(key);
    }
    if let Some(effort) = env("BONE_REASONING_EFFORT") {
        provider.reasoning_effort = Some(effort);
    }
    let data_dir = env("BONE_DATA_DIR")
        .or_else(|| ex.data_dir.clone())
        .map(|d| expand_home(&d))
        .unwrap_or_else(|| config_dir.to_owned());
    Ok(CoreConfig {
        provider,
        system_prompt: env("BONE_SYSTEM_PROMPT").or_else(|| ex.system_prompt.clone()),
        data_dir,
    })
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

impl Scripting {
    pub fn has_hook(&self, event: &str) -> bool {
        self.hooks.lock().unwrap().contains(event)
    }

    /// Run the hooks for `name` without waiting for them (their results are
    /// ignored). Queued in order with everything else sent to Lua.
    pub fn fire(&self, name: &str, event: Json) {
        if !self.has_hook(name) {
            return;
        }
        let (reply, _) = oneshot::channel();
        let session = event["session_id"].as_str().map(str::to_owned);
        let done = Done::Hooks {
            reply,
            name: name.to_owned(),
            event: event.clone(),
        };
        let _ = self.run(
            "_hooks_entry",
            vec![json!(name), event],
            session.as_deref(),
            done,
        );
    }

    /// Run the `bone.on_shutdown` functions before this runtime is
    /// replaced. Errors are returned as text; a stuck function is abandoned
    /// after `limit`.
    pub async fn shutdown(&self, limit: std::time::Duration) -> Vec<String> {
        if !self.has_hook("_shutdown") {
            return Vec::new();
        }
        let (reply, rx) = oneshot::channel();
        if let Err(e) = self.run("_shutdown_entry", vec![], None, Done::Json(reply)) {
            return vec![e];
        }
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(Ok(v))) => crate::agent::list(&v)
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            Ok(Ok(Err(e))) => vec![e],
            Ok(Err(_)) => vec!["the Lua thread stopped".into()],
            Err(_) => vec!["bone.on_shutdown did not finish in time".into()],
        }
    }

    pub fn has_system_prompt_fn(&self) -> bool {
        self.system_prompt_fn
    }

    fn run(
        &self,
        function: &'static str,
        args: Vec<Json>,
        session_id: Option<&str>,
        done: Done,
    ) -> Result<(), String> {
        let session_id = session_id.map(str::to_owned);
        self.jobs
            .send(Job::Run {
                function,
                args,
                session_id,
                done,
            })
            .map_err(|_| "the Lua thread stopped".to_string())
    }

    pub async fn system_prompt(&self, cwd: &str, session_id: &str) -> Result<String, String> {
        let (reply, rx) = oneshot::channel();
        let ctx = json!({ "cwd": cwd, "session_id": session_id });
        self.run(
            "_system_prompt",
            vec![ctx],
            Some(session_id),
            Done::Prompt(reply),
        )?;
        rx.await.map_err(|_| "the Lua thread stopped".to_string())?
    }

    /// Run the hooks registered for `name` on `event`. May wait for the user
    /// (a hook calling `bone.ask`).
    pub async fn hooks(&self, name: &str, event: Json) -> HookOutcome {
        let (reply, rx) = oneshot::channel();
        let session = event["session_id"].as_str().map(str::to_owned);
        let args = vec![json!(name), event.clone()];
        let done = Done::Hooks {
            reply,
            name: name.to_owned(),
            event: event.clone(),
        };
        if let Err(e) = self.run("_hooks_entry", args, session.as_deref(), done) {
            return HookOutcome {
                event,
                deny: Some(e),
            };
        }
        rx.await.unwrap_or(HookOutcome {
            event,
            deny: Some("the Lua thread stopped".into()),
        })
    }

    /// Run the checks core Lua registered with `bone.health`.
    pub async fn health(&self) -> Vec<bone_proto::methods::HealthItem> {
        use bone_proto::methods::{HealthItem, HealthStatus};
        let (reply, rx) = oneshot::channel();
        let failed = |e: String| {
            vec![HealthItem {
                name: "lua checks".into(),
                status: HealthStatus::Error,
                message: e,
            }]
        };
        if let Err(e) = self.run("_health_entry", vec![], None, Done::Json(reply)) {
            return failed(e);
        }
        match rx.await {
            Ok(Ok(v)) => serde_json::from_value(crate::agent::list(&v))
                .unwrap_or_else(|e| failed(e.to_string())),
            Ok(Err(e)) => failed(e),
            Err(_) => failed("the Lua thread stopped".into()),
        }
    }

    /// Answer a pending question. False if there is no such question.
    pub async fn answer(&self, ask_id: AskId, answer: Json) -> bool {
        let (reply, rx) = oneshot::channel();
        if self
            .jobs
            .send(Job::Answer {
                ask_id,
                answer,
                reply,
            })
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// A session's turn was cancelled: its open questions are dropped and
    /// its background work (processes, timers, requests) stopped. The
    /// waiting Lua code gets `nil`.
    pub fn cancel_session(&self, session_id: &str) {
        let _ = self.jobs.send(Job::Cancel {
            session_id: session_id.to_owned(),
        });
    }
}

/// A tool implemented in Lua.
pub struct LuaTool {
    spec: ToolSpec,
    scripting: Arc<Scripting>,
}

impl LuaTool {
    pub fn new(spec: ToolSpec, scripting: Arc<Scripting>) -> Self {
        LuaTool { spec, scripting }
    }
}

impl Tool for LuaTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn call<'a>(&'a self, args: Json, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let (reply, rx) = oneshot::channel();
            let lua_ctx = json!({ "cwd": ctx.cwd.to_string_lossy(), "session_id": ctx.session_id });
            let args = vec![json!(self.spec.name), args, lua_ctx];
            self.scripting
                .run("_run_tool", args, Some(&ctx.session_id), Done::Tool(reply))?;
            rx.await.map_err(|_| "the Lua thread stopped".to_string())?
        })
    }
}

// ---- Lua providers ---------------------------------------------------------

/// A model provider written in Lua (`bone.provider.register`), selected by
/// `type` in a `bone.config.providers` entry.
pub struct LuaProvider {
    kind: String,
    options: Json,
    scripting: Arc<Scripting>,
}

impl LuaProvider {
    pub fn new(config: &ProviderConfig, scripting: Arc<Scripting>) -> Self {
        LuaProvider {
            kind: config.kind.clone().unwrap_or_default(),
            options: config.options.clone(),
            scripting,
        }
    }
}

/// Cancels the session's Lua work if the completion is dropped (the turn
/// was cancelled) before it finished.
struct CancelGuard<'a> {
    scripting: &'a Scripting,
    session_id: &'a str,
    armed: bool,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.scripting.cancel_session(self.session_id);
        }
    }
}

impl crate::provider::Provider for LuaProvider {
    fn complete<'a>(
        &'a self,
        req: crate::provider::CompletionRequest<'a>,
        on_delta: crate::provider::DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<crate::provider::Completion, crate::provider::ProviderError>> {
        use crate::provider::{Delta, ProviderError};
        Box::pin(async move {
            let err = |e: String| ProviderError(format!("{} provider: {e}", self.kind));
            let tools: Vec<Json> = req
                .tools
                .iter()
                .map(|t| json!({ "name": t.name, "description": t.description, "parameters": t.parameters }))
                .collect();
            let request = json!({
                "messages": req.messages,
                "tools": tools,
                "options": self.options,
                "session_id": req.session_id,
                "depth": req.depth,
            });
            let (reply, mut rx) = oneshot::channel();
            let (deltas, mut drx) = tokio_mpsc::unbounded_channel();
            self.scripting
                .run(
                    "_provider_entry",
                    vec![json!(self.kind), request],
                    Some(req.session_id),
                    Done::Provider { reply, deltas },
                )
                .map_err(err)?;
            let mut guard = CancelGuard {
                scripting: &self.scripting,
                session_id: req.session_id,
                armed: true,
            };
            let send = |d: Json, on_delta: &mut crate::provider::DeltaSink<'a>| match d {
                Json::String(t) => on_delta(Delta::Text(t)),
                d => {
                    if let Some(t) = d["text"].as_str() {
                        on_delta(Delta::Text(t.to_owned()));
                    }
                    if let Some(t) = d["reasoning"].as_str() {
                        on_delta(Delta::Reasoning(t.to_owned()));
                    }
                }
            };
            let mut on_delta = on_delta;
            let result = loop {
                tokio::select! {
                    biased;
                    Some(d) = drx.recv() => send(d, &mut on_delta),
                    r = &mut rx => break r,
                }
            };
            while let Ok(d) = drx.try_recv() {
                send(d, &mut on_delta);
            }
            guard.armed = false;
            let v = result
                .map_err(|_| err("the Lua thread stopped".into()))?
                .map_err(err)?;
            completion(&v).map_err(err)
        })
    }
}

/// What a Lua provider returns: `{ content, reasoning, tool_calls = { { id,
/// name, arguments (string or table) } }, usage = { input_tokens,
/// output_tokens } }`.
fn completion(v: &Json) -> Result<crate::provider::Completion, String> {
    use bone_proto::types::{ToolCall, Usage};
    if !v.is_object() {
        return Err(format!("complete() must return a table, not {v}"));
    }
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
    let calls = match &v["tool_calls"] {
        Json::Array(a) => a.clone(),
        _ => Vec::new(),
    };
    let tool_calls = calls
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let name = c["name"]
                .as_str()
                .ok_or("a tool call has no name")?
                .to_owned();
            let arguments = match &c["arguments"] {
                Json::String(s) => s.clone(),
                Json::Null => "{}".to_owned(),
                other => crate::agent::list(other).to_string(),
            };
            let id = c["id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{i}"));
            Ok(ToolCall {
                id,
                name,
                arguments,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let usage = v["usage"].as_object().map(|u| Usage {
        input_tokens: u.get("input_tokens").and_then(Json::as_u64).unwrap_or(0),
        output_tokens: u.get("output_tokens").and_then(Json::as_u64).unwrap_or(0),
    });
    Ok(crate::provider::Completion {
        content: text("content"),
        reasoning: text("reasoning"),
        tool_calls,
        usage,
    })
}

// ---- HTTP streams (bone.http_stream) --------------------------------------

/// Open streaming responses by id. Each is read one server-sent event at a
/// time by a waiting coroutine.
#[derive(Default)]
pub(crate) struct Streams {
    next: std::sync::atomic::AtomicU64,
    open: std::sync::Mutex<HashMap<u64, Arc<tokio::sync::Mutex<HttpStream>>>>,
}

struct HttpStream {
    res: reqwest::Response,
    sse: crate::provider::sse::SseParser,
    queue: std::collections::VecDeque<String>,
    done: bool,
}

impl Streams {
    fn close(&self, id: u64) {
        self.open.lock().unwrap().remove(&id);
    }

    fn get(&self, id: u64) -> Result<Arc<tokio::sync::Mutex<HttpStream>>, String> {
        self.open
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| "this stream is closed".to_string())
    }

    /// Stream waits: `{ http_stream = req }` opens one, `{ stream_next = id }`
    /// is its next event's data (nil at the end), `{ stream_text = id }` the
    /// rest of the body. `None` for other waits.
    async fn wait(&self, spec: &Json) -> Option<Result<Json, String>> {
        if spec["http_stream"].is_object() {
            return Some(self.open_stream(&spec["http_stream"]).await);
        }
        if let Some(id) = spec["stream_next"].as_u64() {
            return Some(match self.get(id) {
                Ok(s) => s
                    .lock()
                    .await
                    .next()
                    .await
                    .map(|e| e.map_or(Json::Null, Json::String)),
                Err(e) => Err(e),
            });
        }
        if let Some(id) = spec["stream_text"].as_u64() {
            return Some(match self.get(id) {
                Ok(s) => s.lock().await.rest().await.map(Json::String),
                Err(e) => Err(e),
            });
        }
        None
    }

    async fn open_stream(&self, req: &Json) -> Result<Json, String> {
        let url = req["url"].as_str().unwrap_or_default().to_owned();
        let res = bone_lua::wait::request(req)?
            .send()
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        let status = res.status().as_u16();
        let headers: serde_json::Map<String, Json> = res
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v.to_str().unwrap_or_default())))
            .collect();
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let stream = HttpStream {
            res,
            sse: Default::default(),
            queue: Default::default(),
            done: false,
        };
        self.open
            .lock()
            .unwrap()
            .insert(id, Arc::new(tokio::sync::Mutex::new(stream)));
        Ok(json!({ "status": status, "headers": headers, "stream": id }))
    }
}

impl HttpStream {
    async fn next(&mut self) -> Result<Option<String>, String> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                return Ok(Some(ev));
            }
            if self.done {
                return Ok(None);
            }
            match self.res.chunk().await.map_err(|e| e.to_string())? {
                Some(bytes) => self.queue.extend(self.sse.push(&bytes)),
                None => {
                    self.done = true;
                    self.queue.extend(self.sse.finish());
                }
            }
        }
    }

    async fn rest(&mut self) -> Result<String, String> {
        let mut out = String::new();
        while let Some(bytes) = self.res.chunk().await.map_err(|e| e.to_string())? {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        self.done = true;
        Ok(out)
    }
}

/// The Lua side of an open stream; dropping it closes the stream.
struct StreamHandle {
    id: u64,
    streams: Arc<Streams>,
}

impl mlua::UserData for StreamHandle {}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.streams.close(self.id);
    }
}

// ---- the Lua thread ------------------------------------------------------

/// A coroutine waiting for an answer or for background work.
struct Waiting {
    thread: Thread,
    session_id: Option<String>,
    done: Done,
}

struct State {
    lua: Lua,
    /// Coroutines waiting for an answer, by question.
    waiting: HashMap<AskId, Waiting>,
    /// Shared by every runtime of one core, so ids never repeat.
    ask_ids: Arc<AtomicU64>,
    /// Coroutines waiting for background work, with a handle to stop it.
    pending: HashMap<u64, (Waiting, tokio::task::AbortHandle)>,
    next_wait: u64,
    events: tokio_mpsc::UnboundedSender<AskEvent>,
    jobs: mpsc::Sender<Job>,
    /// Runs background work for `bone.system`, `bone.sleep`, `bone.http`.
    rt: tokio::runtime::Runtime,
    /// Open `bone.http_stream` responses.
    streams: Arc<Streams>,
    host: Arc<crate::runtime::Host>,
}

fn lua_thread(
    dir: PathBuf,
    opts: LoadOptions,
    init: mpsc::Sender<Result<Extracted, String>>,
    jobs: mpsc::Receiver<Job>,
    jobs_tx: mpsc::Sender<Job>,
    events: tokio_mpsc::UnboundedSender<AskEvent>,
) {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("bone-lua-io")
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = init.send(Err(format!("cannot start the Lua runtime: {e}")));
            return;
        }
    };
    let streams = Arc::new(Streams::default());
    let hook_set: Arc<Mutex<HashSet<String>>> = Default::default();
    let lua = match setup(&dir, &streams, &opts, &hook_set) {
        Ok(lua) => lua,
        Err(e) => {
            let _ = init.send(Err(e));
            return;
        }
    };
    let extracted = extract(&lua).map(|mut ex| {
        ex.hook_set = hook_set;
        ex
    });
    if init
        .send(extracted.map_err(|e| format!("core.lua: {e}")))
        .is_err()
    {
        return;
    }
    let mut st = State {
        lua,
        waiting: HashMap::new(),
        ask_ids: opts.ask_ids.clone(),
        pending: HashMap::new(),
        next_wait: 0,
        events,
        jobs: jobs_tx,
        rt,
        streams,
        host: opts.host.clone(),
    };
    for job in jobs {
        match job {
            Job::Run {
                function,
                args,
                session_id,
                done,
            } => {
                let started = (|| -> mlua::Result<(Thread, MultiValue)> {
                    let bone = st.lua.globals().get::<Table>("bone")?;
                    let f = bone.get::<mlua::Function>(function)?;
                    let thread = st.lua.create_thread(f)?;
                    // So bone.system & co. know they may yield here.
                    bone.get::<Table>("_jobs")?.set(&thread, true)?;
                    let args = args
                        .iter()
                        .map(|a| to_lua(&st.lua, a))
                        .collect::<mlua::Result<Vec<_>>>()?;
                    Ok((thread, MultiValue::from_iter(args)))
                })();
                // A provider's emit(delta) goes straight to its caller.
                let started = started.and_then(|(thread, mut args)| {
                    if let Done::Provider { deltas, .. } = &done {
                        let deltas = deltas.clone();
                        let emit = st.lua.create_function(move |_, d: Value| {
                            let _ = deltas.send(from_lua(&d)?);
                            Ok(())
                        })?;
                        args.push_back(Value::Function(emit));
                    }
                    Ok((thread, args))
                });
                match started {
                    Ok((thread, args)) => st.step(thread, args, session_id, done),
                    Err(e) => st.finish(done, Err(e)),
                }
            }
            Job::Answer {
                ask_id,
                answer,
                reply,
            } => {
                let Some(w) = st.waiting.remove(&ask_id) else {
                    let _ = reply.send(false);
                    continue;
                };
                let _ = reply.send(true);
                let _ = st.events.send(AskEvent::Resolved(AskResolvedParams {
                    ask_id,
                    answer: answer.clone(),
                }));
                let arg = to_lua(&st.lua, &answer).and_then(|v| v.into_lua_multi(&st.lua));
                match arg {
                    Ok(arg) => st.step(w.thread, arg, w.session_id, w.done),
                    Err(e) => st.finish(w.done, Err(e)),
                }
            }
            Job::Resume { wait_id, result } => {
                let Some((w, _)) = st.pending.remove(&wait_id) else {
                    continue;
                };
                let args = match result {
                    Ok(v) => to_lua(&st.lua, &v).map(|v| MultiValue::from_iter([v])),
                    Err(e) => st
                        .lua
                        .create_string(&e)
                        .map(|e| MultiValue::from_iter([Value::Nil, Value::String(e)])),
                };
                match args {
                    Ok(args) => st.step(w.thread, args, w.session_id, w.done),
                    Err(e) => st.finish(w.done, Err(e)),
                }
            }
            Job::Stop => break,
            Job::Cancel { session_id } => {
                st.host.cancel_models(&session_id);
                let ids: Vec<u64> = st
                    .pending
                    .iter()
                    .filter(|(_, (w, _))| w.session_id.as_deref() == Some(session_id.as_str()))
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    let (w, abort) = st.pending.remove(&id).unwrap();
                    abort.abort();
                    st.step(
                        w.thread,
                        MultiValue::from_iter([Value::Nil]),
                        w.session_id,
                        w.done,
                    );
                }
                let ids: Vec<AskId> = st
                    .waiting
                    .iter()
                    .filter(|(_, w)| w.session_id.as_deref() == Some(session_id.as_str()))
                    .map(|(id, _)| *id)
                    .collect();
                for ask_id in ids {
                    let w = st.waiting.remove(&ask_id).unwrap();
                    let _ = st.events.send(AskEvent::Resolved(AskResolvedParams {
                        ask_id,
                        answer: Json::Null,
                    }));
                    st.step(
                        w.thread,
                        MultiValue::from_iter([Value::Nil]),
                        w.session_id,
                        w.done,
                    );
                }
            }
        }
    }
}

impl State {
    /// Resume a coroutine. A yield is either background work
    /// (`{ wait = spec }`: start it and park the coroutine) or a question
    /// (`bone.ask`: park it and tell clients). Otherwise it finished.
    fn step(&mut self, thread: Thread, args: MultiValue, session_id: Option<String>, done: Done) {
        let r = thread.resume::<MultiValue>(args);
        if r.is_ok() && thread.status() == ThreadStatus::Resumable {
            let yielded = r.ok().and_then(|v| v.into_iter().next());
            if let Some(Value::Table(t)) = &yielded
                && let Ok(spec @ Value::Table(_)) = t.get::<Value>("wait")
            {
                let spec = from_lua(&spec).unwrap_or(Json::Null);
                // Session reads and writes are quick and answered here.
                if spec["session"].is_object() {
                    let args = match self.host.session_op(&spec["session"]) {
                        Ok(v) => to_lua(&self.lua, &v).map(|v| MultiValue::from_iter([v])),
                        Err(e) => self
                            .lua
                            .create_string(&e)
                            .map(|e| MultiValue::from_iter([Value::Nil, Value::String(e)])),
                    };
                    match args {
                        Ok(args) => self.step(thread, args, session_id, done),
                        Err(e) => self.finish(done, Err(e)),
                    }
                    return;
                }
                self.next_wait += 1;
                let wait_id = self.next_wait;
                let jobs = self.jobs.clone();
                let streams = self.streams.clone();
                let host = self.host.clone();
                let calling = session_id.clone();
                let task = self.rt.spawn(async move {
                    let result = if let Some(r) = host.model_wait(&spec, calling).await {
                        r
                    } else if let Some(r) = streams.wait(&spec).await {
                        r
                    } else {
                        bone_lua::wait::run(spec).await
                    };
                    let _ = jobs.send(Job::Resume { wait_id, result });
                });
                let w = Waiting {
                    thread,
                    session_id,
                    done,
                };
                self.pending.insert(wait_id, (w, task.abort_handle()));
                return;
            }
            let question = yielded
                .and_then(|v| match v {
                    Value::Table(t) => t.get::<Value>("ask").ok(),
                    other => Some(other),
                })
                .and_then(|v| from_lua(&v).ok())
                .unwrap_or(Json::Null);
            let ask_id = self.ask_ids.fetch_add(1, Ordering::Relaxed) + 1;
            let params = AskRequestedParams {
                ask_id,
                session_id: session_id.clone(),
                question,
            };
            let _ = self.events.send(AskEvent::Requested(params));
            self.waiting.insert(
                ask_id,
                Waiting {
                    thread,
                    session_id,
                    done,
                },
            );
            return;
        }
        self.finish(done, r);
    }

    fn finish(&self, done: Done, r: mlua::Result<MultiValue>) {
        let lua = &self.lua;
        match done {
            Done::Tool(reply) => {
                let out = r.and_then(|v| <(Value, Option<String>)>::from_lua_multi(v, lua));
                let _ = reply.send(match out {
                    Err(e) => Err(short_error(&e)),
                    Ok((Value::Nil, Some(err))) => Err(err),
                    Ok((Value::Nil, None)) => Ok(String::new()),
                    Ok((Value::String(s), _)) => Ok(s.to_string_lossy()),
                    Ok((v @ Value::Table(_), _)) => from_lua(&v)
                        .map(|j| serde_json::to_string_pretty(&j).unwrap_or_default())
                        .map_err(|e| e.to_string()),
                    Ok((v, _)) => Ok(v.to_string().unwrap_or_default()),
                });
            }
            Done::Hooks { reply, name, event } => {
                let out = r.and_then(|v| Table::from_lua_multi(v, lua)).and_then(|t| {
                    let event = match t.get::<Value>("event")? {
                        Value::Nil => event.clone(),
                        v => from_lua(&v)?,
                    };
                    Ok(HookOutcome {
                        event,
                        deny: t.get("deny")?,
                    })
                });
                let _ = reply.send(out.unwrap_or_else(|e| HookOutcome {
                    event,
                    deny: Some(format!("{name} hook failed: {}", short_error(&e))),
                }));
            }
            Done::Provider { reply, .. } => {
                let v = r
                    .and_then(|v| Value::from_lua_multi(v, lua))
                    .and_then(|v| from_lua(&v));
                let _ = reply.send(v.map_err(|e| short_error(&e)));
            }
            Done::Json(reply) => {
                let v = r
                    .and_then(|v| Value::from_lua_multi(v, lua))
                    .and_then(|v| from_lua(&v));
                let _ = reply.send(v.map_err(|e| short_error(&e)));
            }
            Done::Prompt(reply) => {
                let s = r.and_then(|v| String::from_lua_multi(v, lua));
                let _ = reply.send(s.map_err(|e| format!("system_prompt: {}", short_error(&e))));
            }
        }
    }
}

fn setup(
    dir: &Path,
    streams: &Arc<Streams>,
    opts: &LoadOptions,
    hook_set: &Arc<Mutex<HashSet<String>>>,
) -> Result<Lua, String> {
    let disabled = &opts.disabled;
    let lua = bone_lua::new_state(Side::Core, Some(dir)).map_err(|e| e.to_string())?;
    let run = |rel: &str| {
        bone_lua::run_runtime(&lua, Some(dir), rel).map_err(|e| format!("runtime/{rel}: {e}"))
    };
    // Hooks added at any time count (the core skips points with none).
    let hooks = hook_set.clone();
    let added = lua
        .create_function(move |_, name: String| {
            hooks.lock().unwrap().insert(name);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    lua.globals()
        .get::<Table>("bone")
        .and_then(|b| b.set("_hook_added", added))
        .map_err(|e| e.to_string())?;
    run("core/api.lua")?;
    install_helpers(&lua, dir, streams).map_err(|e| e.to_string())?;
    // bone.model.list(): the providers of the configuration in use.
    let host = opts.host.clone();
    let models = lua
        .create_function(move |lua, ()| to_lua(lua, &host.model_list()?))
        .map_err(|e| e.to_string())?;
    lua.globals()
        .get::<Table>("bone")
        .and_then(|b| b.set("_models", models))
        .map_err(|e| e.to_string())?;
    run("core/defaults.lua")?;
    bone_lua::run_user_plugins_except(&lua, dir, "core.lua", &|name| disabled.contains(name))
        .map_err(|e| e.to_string())?;
    bone_lua::run_file(&lua, &dir.join("core.lua")).map_err(|e| e.to_string())?;
    lua.load("for _, f in ipairs(bone._ready) do f() end")
        .set_name("=bone.on_ready")
        .exec()
        .map_err(|e| format!("bone.on_ready: {e}"))?;
    Ok(lua)
}

fn install_helpers(lua: &Lua, dir: &Path, streams: &Arc<Streams>) -> mlua::Result<()> {
    let bone: Table = lua.globals().get("bone")?;
    // Ties an open stream to a Lua value: when Lua drops it, it closes.
    let st = streams.clone();
    bone.set(
        "_stream_handle",
        lua.create_function(move |lua, id: u64| {
            lua.create_userdata(StreamHandle {
                id,
                streams: st.clone(),
            })
        })?,
    )?;
    let st = streams.clone();
    bone.set(
        "_stream_close",
        lua.create_function(move |_, id: u64| {
            st.close(id);
            Ok(())
        })?,
    )?;
    // Blocking versions, for code that runs outside a job (while core.lua
    // loads). Inside hooks and tools bone.system & co. wait without blocking.
    bone.set(
        "_sleep_sync",
        lua.create_function(|_, ms: u64| {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(())
        })?,
    )?;
    bone.set(
        "_system_sync",
        lua.create_function(|lua, (cmd, opts): (String, Option<Table>)| {
            let mut c = std::process::Command::new("bash");
            c.arg("-c").arg(&cmd).stdin(std::process::Stdio::null());
            if let Some(cwd) = opts
                .map(|o| o.get::<Option<String>>("cwd"))
                .transpose()?
                .flatten()
            {
                c.current_dir(cwd);
            }
            let out = c
                .output()
                .map_err(|e| mlua::Error::runtime(format!("cannot run {cmd:?}: {e}")))?;
            let t = lua.create_table()?;
            t.set("code", out.status.code())?;
            t.set("stdout", String::from_utf8_lossy(&out.stdout).into_owned())?;
            t.set("stderr", String::from_utf8_lossy(&out.stderr).into_owned())?;
            Ok(t)
        })?,
    )?;
    // The core may share a terminal with the TUI; print goes to a log file.
    let log = dir.join("core.log");
    lua.globals().set(
        "print",
        lua.create_function(move |_, args: mlua::Variadic<Value>| {
            let line: Vec<String> = args
                .iter()
                .map(|v| v.to_string().unwrap_or_else(|_| format!("{v:?}")))
                .collect();
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
            {
                let _ = writeln!(f, "{}", line.join("\t"));
            }
            Ok(())
        })?,
    )?;
    Ok(())
}

fn extract(lua: &Lua) -> mlua::Result<Extracted> {
    let bone: Table = lua.globals().get("bone")?;
    let config: Table = bone.get("config")?;

    let mut providers = HashMap::new();
    if let Some(t) = config.get::<Option<Table>>("providers")? {
        for pair in t.pairs::<String, Value>() {
            let (name, v) = pair?;
            let json = from_lua(&v)?;
            let mut p: ProviderConfig = serde_json::from_value(json.clone())
                .map_err(|e| mlua::Error::runtime(format!("bone.config.providers.{name}: {e}")))?;
            p.options = json;
            providers.insert(name, p);
        }
    }
    let (system_prompt, system_prompt_fn) = match config.get::<Value>("system_prompt")? {
        Value::Nil => (None, false),
        Value::String(s) => (Some(s.to_str()?.to_owned()), false),
        Value::Function(_) => (None, true),
        other => {
            return Err(mlua::Error::runtime(format!(
                "bone.config.system_prompt must be a string or function, not {}",
                other.type_name()
            )));
        }
    };

    let mut tools = Vec::new();
    for pair in bone.get::<Table>("_tools")?.pairs::<String, Table>() {
        let (name, t) = pair?;
        let parameters = match t.get::<Value>("parameters")? {
            Value::Nil => json!({ "type": "object", "properties": {} }),
            v => from_lua(&v)?,
        };
        tools.push(ToolSpec {
            description: t.get::<Option<String>>("description")?.unwrap_or_default(),
            parameters,
            name,
        });
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));

    let hooks = bone
        .get::<Table>("_hooks")?
        .pairs::<String, Value>()
        .filter_map(|p| p.ok().map(|(k, _)| k))
        .collect();
    Ok(Extracted {
        lua_providers: bone
            .get::<Table>("_providers")?
            .pairs::<String, Value>()
            .map(|p| p.map(|(k, _)| k))
            .collect::<mlua::Result<_>>()?,
        providers,
        provider: config.get("provider")?,
        system_prompt,
        system_prompt_fn,
        data_dir: config.get("data_dir")?,
        tools,
        hooks,
        hook_set: Default::default(),
    })
}

#[cfg(test)]
mod tests;
