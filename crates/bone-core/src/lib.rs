//! UI-agnostic bone core.
//!
//! The core takes requests and emits a stream of events. It knows nothing about
//! transports or connections; `bone-server` hosts it. It owns sessions, the
//! provider and tools, and runs core-side Lua (hooks, tools, questions).

mod agent;
pub mod config;
mod health;
pub mod provider;
mod runtime;
pub mod scripting;
pub mod session;
pub mod tools;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, Weak};

use bone_proto::methods::{
    AskRespond, AskRespondParams, CoreReload, Echo, EchoParams, Echoed, HealthCheck, PluginList,
    PluginLoad, PluginRef, PluginReload, PluginUnload, SessionCreate, SessionCreateParams,
    SessionList, SessionMessages, SessionMessagesResult, SessionRef, TurnCancel, TurnStart,
    TurnStartParams, TurnStartResult,
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
    sessions: SessionStore,
    events: broadcast::Sender<Event>,
}

impl Inner {
    fn emit<N: Notification>(&self, params: N::Params) {
        // No subscribers is not an error.
        let _ = self.events.send(Event::new::<N>(params));
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
        Core {
            inner: Arc::new(Inner {
                sessions: SessionStore::new(&runtime.config.data_dir),
                data_dir: runtime.config.data_dir.clone(),
                runtime: RwLock::new(Arc::new(runtime)),
                source,
                retired: Mutex::new(Vec::new()),
                reloading: tokio::sync::Mutex::new(()),
                events,
            }),
        }
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
            _ => Err(RpcError::method_not_found(method)),
        }
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
        Ok(info)
    }

    fn session_list(&self) -> Result<Vec<SessionInfo>, RpcError> {
        self.inner.sessions.list().map_err(session_error)
    }

    fn session_messages(&self, params: SessionRef) -> Result<SessionMessagesResult, RpcError> {
        let session = self
            .inner
            .sessions
            .get(&params.session_id)
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
            .sessions
            .get(&params.session_id)
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
            .sessions
            .get(&params.session_id)
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
