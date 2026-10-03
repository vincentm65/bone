//! UI-agnostic bone core.
//!
//! The core takes requests and emits a stream of events. It knows nothing about
//! transports or connections; `bone-server` hosts it. It owns sessions, the
//! provider and tools, and runs core-side Lua (hooks, tools, questions).

mod agent;
pub mod config;
mod health;
pub mod mcp;
pub mod provider;
mod runtime;
pub mod scripting;
pub mod session;
pub mod tools;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, Weak};

use bone_proto::methods::{
    AskRespond, AskRespondParams, CoreReload, Echo, EchoParams, Echoed, HealthCheck, McpList,
    ModelCancel, ModelComplete, ModelCompleteParams, ModelCompleted, ModelCompletedParams,
    ModelDeltaEvent, ModelDeltaParams, ModelList, ModelRequest, PluginList, PluginLoad, PluginRef,
    PluginReload, PluginUnload, SessionCreate, SessionCreateParams, SessionList, SessionMessages,
    SessionMessagesResult, SessionRef, TurnCancel, TurnStart, TurnStartParams, TurnStartResult,
};
use bone_proto::types::SessionInfo;
use bone_proto::{Method, Notification, RpcError};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::config::CoreConfig;
use crate::provider::{OpenAiProvider, Provider};
use crate::runtime::{Change, Runtime, Source};
use crate::scripting::{Loaded, Scripting};
use crate::session::{ActiveTurn, SessionError, SessionStore};
use crate::tools::Registry;

/// Large because streamed deltas are many small events and a lagging client
/// loses the ones it missed.
const EVENT_CAPACITY: usize = 8192;

/// An event emitted by the core, already in wire shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub method: &'static str,
    pub params: Value,
}

impl Event {
    pub fn new<N: Notification>(params: N::Params) -> Self {
        Event {
            method: N::METHOD,
            params: serde_json::to_value(params).expect("event params serialize"),
        }
    }
}

pub struct Core {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    /// Where sessions live; fixed for the core's lifetime.
    data_dir: PathBuf,
    /// The current configuration (see `runtime.rs`).
    runtime: RwLock<Arc<Runtime>>,
    /// How to load it again, when it came from a config dir.
    source: Option<Source>,
    /// Replaced Lua threads that may still have open questions.
    retired: Mutex<Vec<Weak<Scripting>>>,
    reloading: tokio::sync::Mutex<()>,
    /// Sessions `session_start` hooks have seen.
    started: Mutex<std::collections::HashSet<String>>,
    /// MCP servers; they outlive reloads.
    mcp: mcp::McpManager,
    /// Running `model/complete` calls, to cancel them.
    model_requests: Mutex<std::collections::HashMap<u64, tokio::task::AbortHandle>>,
    next_model_request: std::sync::atomic::AtomicU64,
    sessions: SessionStore,
    events: broadcast::Sender<Event>,
}

impl Inner {
    fn emit<N: Notification>(&self, params: N::Params) {
        // No subscribers is not an error.
        let _ = self.events.send(Event::new::<N>(params));
    }

    /// A session from the store; the first time this core uses it,
    /// `session_start` hooks hear about it.
    fn session(&self, id: &str) -> Result<session::SessionHandle, SessionError> {
        let s = self.sessions.get(id)?;
        let cwd = s.lock().unwrap().info.cwd.clone();
        self.started_session(id, &cwd, false);
        Ok(s)
    }

    fn started_session(&self, id: &str, cwd: &str, new: bool) {
        if !self.started.lock().unwrap().insert(id.to_owned()) {
            return;
        }
        if let Some(s) = &self.runtime().scripting {
            s.fire(
                "session_start",
                serde_json::json!({ "session_id": id, "cwd": cwd, "new": new }),
            );
        }
    }
}

impl Core {
    /// A core using the configured OpenAI-compatible provider and the
    /// built-in tools.
    pub fn new(config: CoreConfig) -> Self {
        let provider = Arc::new(OpenAiProvider::new(config.provider.clone()));
        Self::with_parts(config, provider, Registry::builtin())
    }

    pub fn with_parts(config: CoreConfig, provider: Arc<dyn Provider>, tools: Registry) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self::build(Runtime::plain(config, provider, tools), None, events)
    }

    /// A core configured by `core.lua` (see [`scripting::load`]): its
    /// provider, the built-in tools plus Lua tools, and its hooks. It can
    /// reload that configuration (`core/reload`).
    /// Needs a tokio runtime: questions from Lua are forwarded as events.
    pub fn from_loaded(loaded: Loaded) -> Self {
        Self::loaded(loaded, None)
    }

    /// [`from_loaded`](Self::from_loaded) with a given provider, kept
    /// across reloads (tests).
    pub fn with_lua(loaded: Loaded, provider: Arc<dyn Provider>) -> Self {
        Self::loaded(loaded, Some(provider))
    }

    fn loaded(loaded: Loaded, provider: Option<Arc<dyn Provider>>) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let source = Source::new(&loaded, provider.clone());
        let runtime = Runtime::from_loaded(loaded, provider, &events);
        Self::build(runtime, Some(source), events)
    }

    fn build(runtime: Runtime, source: Option<Source>, events: broadcast::Sender<Event>) -> Self {
        let core = Core {
            inner: Arc::new(Inner {
                sessions: SessionStore::new(&runtime.config.data_dir),
                data_dir: runtime.config.data_dir.clone(),
                runtime: RwLock::new(Arc::new(runtime)),
                source,
                retired: Mutex::new(Vec::new()),
                reloading: tokio::sync::Mutex::new(()),
                started: Mutex::new(Default::default()),
                mcp: Default::default(),
                model_requests: Mutex::new(Default::default()),
                next_model_request: Default::default(),
                events,
            }),
        };
        if let Some(source) = &core.inner.source {
            source.options.lock().unwrap().host.set(&core.inner);
        }
        core.inner.mcp.apply(core.inner.runtime().mcp.clone());
        core
    }

    /// The MCP servers (tests swap how they connect).
    pub fn mcp(&self) -> &mcp::McpManager {
        &self.inner.mcp
    }

    /// Load the Lua configuration again and switch to it; see
    /// [`CoreReload`].
    pub async fn reload(&self) -> Result<bone_proto::methods::ReloadResult, String> {
        self.inner.reload(Change::Same).await
    }

    /// Subscribe to every event emitted from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// Handle one request. Unknown methods return `METHOD_NOT_FOUND`.
    /// Long work (turns) runs in the background and reports through events.
    pub async fn handle(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        match method {
            Echo::METHOD => dispatch::<Echo, _>(params, |p| self.echo(p)),
            SessionCreate::METHOD => {
                dispatch::<SessionCreate, _>(params, |p| self.session_create(p))
            }
            SessionList::METHOD => dispatch::<SessionList, _>(params, |_| self.session_list()),
            SessionMessages::METHOD => {
                dispatch::<SessionMessages, _>(params, |p| self.session_messages(p))
            }
            TurnStart::METHOD => dispatch::<TurnStart, _>(params, |p| self.turn_start(p)),
            TurnCancel::METHOD => dispatch::<TurnCancel, _>(params, |p| self.turn_cancel(p)),
            HealthCheck::METHOD => {
                decode::<HealthCheck>(params)?;
                Ok(serde_json::to_value(health::check(&self.inner).await).unwrap_or_default())
            }
            AskRespond::METHOD => {
                let p: AskRespondParams = decode::<AskRespond>(params)?;
                let mut answered = false;
                for s in self.inner.all_scripting() {
                    if s.answer(p.ask_id, p.answer.clone()).await {
                        answered = true;
                        break;
                    }
                }
                if !answered {
                    return Err(RpcError::invalid_params(format!(
                        "no open question {}",
                        p.ask_id
                    )));
                }
                Ok(Value::Null)
            }
            CoreReload::METHOD => {
                decode::<CoreReload>(params)?;
                let r = self.inner.reload(Change::Same).await;
                reloaded(r)
            }
            PluginList::METHOD => {
                decode::<PluginList>(params)?;
                let list = self
                    .inner
                    .source
                    .as_ref()
                    .map(Source::plugins)
                    .unwrap_or_default();
                Ok(serde_json::to_value(list).unwrap_or_default())
            }
            PluginLoad::METHOD | PluginReload::METHOD => {
                let p: PluginRef = decode::<PluginLoad>(params)?;
                reloaded(self.inner.reload(Change::Enable(p.name)).await)
            }
            PluginUnload::METHOD => {
                let p: PluginRef = decode::<PluginUnload>(params)?;
                reloaded(self.inner.reload(Change::Disable(p.name)).await)
            }
            McpList::METHOD => {
                decode::<McpList>(params)?;
                Ok(serde_json::to_value(self.inner.mcp.list()).unwrap_or_default())
            }
            ModelList::METHOD => {
                decode::<ModelList>(params)?;
                Ok(serde_json::to_value(self.inner.runtime().model_list()).unwrap_or_default())
            }
            ModelComplete::METHOD => {
                dispatch::<ModelComplete, _>(params, |p| self.model_complete(p))
            }
            ModelCancel::METHOD => dispatch::<ModelCancel, _>(params, |p| {
                let task = self
                    .inner
                    .model_requests
                    .lock()
                    .unwrap()
                    .remove(&p.request_id);
                if let Some(task) = task {
                    task.abort();
                    self.inner.emit::<ModelCompleted>(ModelCompletedParams {
                        request_id: p.request_id,
                        message: None,
                        usage: None,
                        error: Some("cancelled".into()),
                    });
                }
                Ok(())
            }),
            _ => Err(RpcError::method_not_found(method)),
        }
    }

    /// Start a model call; it reports through `model/delta` and
    /// `model/completed`.
    fn model_complete(&self, p: ModelCompleteParams) -> Result<ModelRequest, RpcError> {
        use std::sync::atomic::Ordering;
        let request_id = self
            .inner
            .next_model_request
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let inner = self.inner.clone();
        let call = runtime::ModelCall {
            provider: p.provider,
            options: p.options,
            messages: p.messages,
            tools: p.tools,
            depth: 0,
            session_id: None,
        };
        let stream = p.stream;
        // Registered before it can finish, so it is always removed after.
        let mut requests = self.inner.model_requests.lock().unwrap();
        let task = tokio::spawn(async move {
            let events = inner.clone();
            let mut on_delta = |d: provider::Delta| {
                if !stream {
                    return;
                }
                let (kind, text) = match d {
                    provider::Delta::Text(t) => (bone_proto::types::DeltaKind::Text, t),
                    provider::Delta::Reasoning(t) => (bone_proto::types::DeltaKind::Reasoning, t),
                };
                events.emit::<ModelDeltaEvent>(ModelDeltaParams {
                    request_id,
                    kind,
                    text,
                });
            };
            let r = inner.model_call(call, &mut on_delta).await;
            inner.model_requests.lock().unwrap().remove(&request_id);
            let done = match r {
                Ok(c) => ModelCompletedParams {
                    request_id,
                    message: Some(bone_proto::types::ChatMessage::Assistant {
                        content: c.content,
                        reasoning: c.reasoning,
                        tool_calls: c.tool_calls,
                    }),
                    usage: c.usage,
                    error: None,
                },
                Err(e) => ModelCompletedParams {
                    request_id,
                    message: None,
                    usage: None,
                    error: Some(e),
                },
            };
            inner.emit::<ModelCompleted>(done);
        });
        requests.insert(request_id, task.abort_handle());
        Ok(ModelRequest { request_id })
    }

    fn echo(&self, params: EchoParams) -> Result<EchoParams, RpcError> {
        self.inner.emit::<Echoed>(params.clone());
        Ok(params)
    }

    fn session_create(&self, params: SessionCreateParams) -> Result<SessionInfo, RpcError> {
        let cwd = match params.cwd {
            Some(cwd) => PathBuf::from(cwd),
            None => std::env::current_dir().map_err(RpcError::internal)?,
        };
        let cwd = std::fs::canonicalize(&cwd)
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| {
                RpcError::invalid_params(format!("not a directory: {}", cwd.display()))
            })?;
        let session = self
            .inner
            .sessions
            .create(cwd.to_string_lossy().into_owned())
            .map_err(session_error)?;
        let info = session.lock().unwrap().info.clone();
        self.inner
            .started_session(&info.session_id, &info.cwd, true);
        Ok(info)
    }

    fn session_list(&self) -> Result<Vec<SessionInfo>, RpcError> {
        self.inner.sessions.list().map_err(session_error)
    }

    fn session_messages(&self, params: SessionRef) -> Result<SessionMessagesResult, RpcError> {
        let session = self
            .inner
            .session(&params.session_id)
            .map_err(session_error)?;
        let s = session.lock().unwrap();
        Ok(SessionMessagesResult {
            info: s.info.clone(),
            messages: s.messages.clone(),
            active_turn: s.active.as_ref().map(|a| a.turn_id),
        })
    }

    fn turn_start(&self, params: TurnStartParams) -> Result<TurnStartResult, RpcError> {
        if params.text.trim().is_empty() {
            return Err(RpcError::invalid_params("text is empty"));
        }
        let session = self
            .inner
            .session(&params.session_id)
            .map_err(session_error)?;
        let (turn_id, cancel) = {
            let mut s = session.lock().unwrap();
            if let Some(active) = &s.active {
                return Err(RpcError::new(
                    RpcError::BUSY,
                    format!("turn {} is still running", active.turn_id),
                ));
            }
            let turn_id = s.next_turn_id();
            let cancel = CancellationToken::new();
            s.active = Some(ActiveTurn {
                turn_id,
                cancel: cancel.clone(),
            });
            (turn_id, cancel)
        };
        // The turn saves the message and announces itself after its
        // `turn_start` hooks have run.
        tokio::spawn(agent::run_turn(
            self.inner.clone(),
            session,
            turn_id,
            params.text,
            cancel,
        ));
        Ok(TurnStartResult { turn_id })
    }

    fn turn_cancel(&self, params: SessionRef) -> Result<(), RpcError> {
        let session = self
            .inner
            .session(&params.session_id)
            .map_err(session_error)?;
        if let Some(active) = &session.lock().unwrap().active {
            active.cancel.cancel();
        }
        Ok(())
    }
}

fn reloaded(r: Result<bone_proto::methods::ReloadResult, String>) -> Result<Value, RpcError> {
    match r {
        Ok(r) => Ok(serde_json::to_value(r).unwrap_or_default()),
        Err(e) => Err(RpcError::new(RpcError::INVALID_PARAMS, e)),
    }
}

fn session_error(e: SessionError) -> RpcError {
    match e {
        SessionError::NotFound(_) => RpcError::invalid_params(e),
        _ => RpcError::internal(e),
    }
}

/// Decode params for `M`. Absent params decode as `{}`.
fn decode<M: Method>(params: Option<Value>) -> Result<M::Params, RpcError> {
    serde_json::from_value(params.unwrap_or_else(|| Value::Object(Default::default())))
        .map_err(RpcError::invalid_params)
}

/// Decode params for `M`, run the handler, encode its result.
pub fn dispatch<M: Method, F>(params: Option<Value>, handler: F) -> Result<Value, RpcError>
where
    F: FnOnce(M::Params) -> Result<M::Result, RpcError>,
{
    // Absent params decode as `{}` so param-less methods accept both forms.
    let params =
        serde_json::from_value(params.unwrap_or_else(|| Value::Object(Default::default())))
            .map_err(RpcError::invalid_params)?;
    let result = handler(params)?;
    serde_json::to_value(result).map_err(RpcError::internal)
}

#[cfg(test)]
mod agent_tests;
