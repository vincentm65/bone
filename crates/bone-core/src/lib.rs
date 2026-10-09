//! UI-agnostic bone core.
//!
//! The core takes requests and emits a stream of events. It knows nothing about
//! transports or connections; `bone-server` hosts it. It owns sessions, the
//! provider and tools, and runs core-side Lua (hooks, tools, questions).

mod agent;
mod attachments;
mod compact;
pub mod config;
mod health;
pub mod import;
mod index;
pub mod mcp;
pub mod provider;
mod runtime;
pub mod scripting;
pub mod session;
pub mod settings;
pub mod tools;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, Weak};

use bone_proto::methods::{
    AskRespond, AskRespondParams, AttachmentRead, AttachmentReadResult, AttachmentUpload,
    CoreReload, Echo, EchoParams, Echoed, HealthCheck, LuaCall, LuaCallParams, McpAuth,
    McpAuthCode, McpAuthCodeParams, McpList, McpReconnect, McpRef, McpSignOut, ModelCancel,
    ModelComplete, ModelCompleteParams, ModelCompleted, ModelCompletedParams, ModelDeltaEvent,
    ModelDeltaParams, ModelList, ModelRequest, PluginList, PluginLoad, PluginRef, PluginReload,
    PluginUnload, ProcessCancel, ProcessOutput, ProcessRead, ProcessRef, ProcessResize,
    ProcessSnapshot, ProcessState, ProcessesGet, ProcessesList, ProcessesResult, QueueAdd,
    QueueAddParams, QueueAddResult, QueueChanged, QueueChangedParams, QueueClear, QueueMode,
    QueueMove, QueueRemove, QueueResume, QueueUpdate, SecretSet, SecretsList, SecretsSet,
    SessionActive, SessionCompact, SessionCompactParams, SessionCreate, SessionCreateParams,
    SessionCreated, SessionDelete, SessionDeleted, SessionFork, SessionForkParams, SessionList,
    SessionMessages, SessionMessagesResult, SessionRef, SessionRename, SessionRenameParams,
    SessionUpdated, SessionUpdatedParams, SettingPath, SettingSet, SettingsChanged,
    SettingsChangedParams, SettingsGet, SettingsReset, SettingsSet, StoreQuery, StoreQueryParams,
    TurnCancel, TurnStart, TurnStartParams, TurnStartResult, TurnSteer, TurnSteerParams,
};
use bone_proto::types::{ImageAttachment, SessionInfo};
use bone_proto::{Method, Notification, RpcError};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::config::CoreConfig;
use crate::provider::{OpenAiProvider, Provider};
use crate::runtime::{Change, Runtime, Source};
use crate::scripting::{Loaded, Scripting};
use crate::session::{ActiveTurn, SessionError, SessionStore};
use crate::tools::{ProcessState as CoreProcessState, ProcessView, Registry};

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
    /// Where turns run, so they can start from any thread (core Lua's
    /// `bone.queue.add` runs on the Lua thread).
    tokio: Option<tokio::runtime::Handle>,
    /// MCP servers; they outlive reloads.
    mcp: mcp::McpManager,
    /// Running `model/complete` calls, to cancel them.
    model_requests: Mutex<std::collections::HashMap<u64, tokio::task::AbortHandle>>,
    next_model_request: std::sync::atomic::AtomicU64,
    sessions: SessionStore,
    attachments: Arc<attachments::AttachmentStore>,
    /// Managed shell jobs shared by all tools in this core.
    jobs: Arc<tools::ProcessRegistry>,
    events: broadcast::Sender<Event>,
    /// settings.json as last read or written; `Err` when the file could not
    /// be read (then it is not written either, so it is never clobbered).
    settings: Mutex<Result<Value, String>>,
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

    /// Start a turn on an idle session (an error if one is running).
    pub(crate) fn begin_turn(
        self: &Arc<Self>,
        session: session::SessionHandle,
        text: String,
    ) -> Result<bone_proto::types::TurnId, RpcError> {
        self.begin_turn_with_images(session, text, Vec::new())
    }

    pub(crate) fn begin_turn_with_images(
        self: &Arc<Self>,
        session: session::SessionHandle,
        text: String,
        mut images: Vec<ImageAttachment>,
    ) -> Result<bone_proto::types::TurnId, RpcError> {
        self.validate_input(&session.lock().unwrap(), &text, &mut images)?;
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
                closing: false,
            });
            (turn_id, cancel)
        };
        // The turn saves the message and announces itself after its
        // `turn_start` hooks have run.
        let turn = agent::run_turn(self.clone(), session, turn_id, text, images, cancel);
        match &self.tokio {
            Some(rt) => drop(rt.spawn(turn)),
            None => drop(tokio::spawn(turn)),
        }
        Ok(turn_id)
    }

    /// Validate stored attachments and the session's model before saving input.
    pub(crate) fn validate_input(
        &self,
        session: &session::Session,
        text: &str,
        images: &mut [ImageAttachment],
    ) -> Result<(), RpcError> {
        if text.trim().is_empty() && images.is_empty() {
            return Err(RpcError::invalid_params("message is empty"));
        }
        self.attachments
            .validate(images)
            .map_err(RpcError::invalid_params)?;
        let rt = self.runtime();
        let pin = session.model.clone();
        if !rt.supports_images(
            pin.as_ref().and_then(|p| p.0.as_deref()),
            !images.is_empty(),
        ) {
            return Err(RpcError::invalid_params(
                "this model does not support images; select a vision model",
            ));
        }
        Ok(())
    }

    pub(crate) fn queue_add_with_images(
        self: &Arc<Self>,
        session: &session::SessionHandle,
        text: String,
        mode: QueueMode,
        mut images: Vec<ImageAttachment>,
    ) -> Result<QueueAddResult, RpcError> {
        self.validate_input(&session.lock().unwrap(), &text, &mut images)?;
        if session.lock().unwrap().active.is_none() {
            // A new message also lets a paused queue go on afterwards.
            let mut s = session.lock().unwrap();
            if std::mem::take(&mut s.queue_paused) {
                self.emit_queue(&s, None);
            }
            drop(s);
            match self.begin_turn_with_images(session.clone(), text.clone(), images.clone()) {
                Ok(turn_id) => {
                    return Ok(QueueAddResult {
                        id: None,
                        turn_id: Some(turn_id),
                    });
                }
                // Another client started one meanwhile: queue it after all.
                Err(e) if e.code == RpcError::BUSY => {}
                Err(e) => return Err(e),
            }
        }
        let mut s = session.lock().unwrap();
        let ending = s
            .active
            .as_ref()
            .is_none_or(|a| a.closing || a.cancel.is_cancelled());
        let id = s.enqueue_with_images(text, if ending { QueueMode::Next } else { mode }, images);
        self.emit_queue(&s, None);
        Ok(QueueAddResult {
            id: Some(id),
            turn_id: None,
        })
    }

    /// The other `queue/*` methods (and `bone.queue.*`): change the queue,
    /// then tell clients and go on if idle.
    pub(crate) fn queue_edit(
        self: &Arc<Self>,
        session_id: &str,
        edit: impl FnOnce(&mut session::Session) -> Result<(), RpcError>,
    ) -> Result<(), RpcError> {
        let session = self.session(session_id).map_err(session_error)?;
        {
            let mut s = session.lock().unwrap();
            edit(&mut s)?;
            s.save_queue();
            self.emit_queue(&s, None);
        }
        self.start_next(&session);
        Ok(())
    }

    /// Tell clients what a session's queue holds now.
    pub(crate) fn emit_queue(&self, s: &session::Session, error: Option<String>) {
        self.emit::<QueueChanged>(QueueChangedParams {
            session_id: s.info.session_id.clone(),
            items: s.queue.clone(),
            paused: s.queue_paused,
            error,
        });
    }

    /// If the session is idle and its queue may go on, start the first
    /// queued message as a turn.
    pub(crate) fn start_next(self: &Arc<Self>, session: &session::SessionHandle) {
        let next = {
            let mut s = session.lock().unwrap();
            if s.active.is_some() || s.queue_paused || s.queue.is_empty() {
                return;
            }
            let q = s.queue.remove(0);
            s.save_queue();
            self.emit_queue(&s, None);
            q
        };
        if let Err(e) =
            self.begin_turn_with_images(session.clone(), next.text.clone(), next.images.clone())
        {
            let mut s = session.lock().unwrap();
            s.queue.insert(0, next);
            let error = if e.code == RpcError::BUSY {
                None
            } else {
                s.queue_paused = true;
                Some(e.message)
            };
            s.save_queue();
            self.emit_queue(&s, error);
        }
    }

    /// A turn ended: steer messages that never joined it wait for the next
    /// one; a cancelled turn pauses the queue, any other goes on to the next
    /// queued message.
    pub(crate) fn after_turn(self: &Arc<Self>, session: &session::SessionHandle, cancelled: bool) {
        {
            let mut s = session.lock().unwrap();
            let mut changed = false;
            for q in s.queue.iter_mut() {
                if q.mode == QueueMode::Steer {
                    q.mode = QueueMode::Next;
                    changed = true;
                }
            }
            if cancelled && !s.queue.is_empty() && !s.queue_paused {
                s.queue_paused = true;
                changed = true;
            }
            if changed {
                s.save_queue();
                self.emit_queue(&s, None);
            }
        }
        self.start_next(session);
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
        let saved = match &source {
            Some(s) => settings::load(&s.config_dir),
            None => Ok(Value::Object(Default::default())),
        };
        let core = Core {
            inner: Arc::new(Inner {
                sessions: SessionStore::new(&runtime.config.data_dir),
                attachments: Arc::new(attachments::AttachmentStore::new(&runtime.config.data_dir)),
                data_dir: runtime.config.data_dir.clone(),
                runtime: RwLock::new(Arc::new(runtime)),
                source,
                retired: Mutex::new(Vec::new()),
                reloading: tokio::sync::Mutex::new(()),
                started: Mutex::new(Default::default()),
                tokio: tokio::runtime::Handle::try_current().ok(),
                mcp: Default::default(),
                model_requests: Mutex::new(Default::default()),
                next_model_request: Default::default(),
                jobs: Default::default(),
                settings: Mutex::new(saved),
                events,
            }),
        };
        if let Some(source) = &core.inner.source {
            source.options.lock().unwrap().host.set(&core.inner);
        }
        if let Some(source) = &core.inner.source {
            mcp::oauth::set_dir(source.config_dir.clone());
        }
        core.inner.mcp.apply(core.inner.runtime().mcp.clone());
        // Index sessions written while no core was running (or before the
        // index existed) without holding up startup.
        let inner = core.inner.clone();
        std::thread::spawn(move || inner.sessions.catch_up());
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

    /// Save one setting and tell every client; a provider or model change
    /// reloads the configuration, and is undone if that fails.
    async fn set_setting(
        &self,
        path: String,
        value: Value,
        session_id: Option<String>,
    ) -> Result<Value, RpcError> {
        let source = self.inner.source.as_ref().ok_or_else(|| {
            RpcError::invalid_params("this core has no config dir to save settings in")
        })?;
        // The provider choices must name a configured provider.
        let provider_named = match path.split_once('.') {
            None if path == "provider" => value.as_str().map(str::to_owned),
            _ => None,
        };
        if let Some(name) = provider_named {
            let known = self.inner.runtime().model_list(None);
            if !known.iter().any(|m| m.name == name) {
                let names: Vec<&str> = known.iter().map(|m| m.name.as_str()).collect();
                return Err(RpcError::invalid_params(format!(
                    "no provider {name:?} in core.lua (configured: {})",
                    names.join(", ")
                )));
            }
        }
        let before = {
            let mut guard = self.inner.settings.lock().unwrap();
            let current = guard.as_mut().map_err(|e| {
                RpcError::invalid_params(format!("{e}; fix or remove it before changing settings"))
            })?;
            let before = current.clone();
            settings::set(current, &path, value.clone()).map_err(RpcError::invalid_params)?;
            // A provider added while none is chosen in settings is chosen.
            if let Some(name) = path.strip_prefix("providers.")
                && value.is_object()
                && settings::get(current, "provider").is_none()
            {
                settings::set(current, "provider", Value::String(name.to_owned()))
                    .map_err(RpcError::invalid_params)?;
            }
            settings::save(&source.config_dir, current).map_err(RpcError::internal)?;
            before
        };
        if (path == "provider" || path.starts_with("providers."))
            && let Err(e) = self.inner.reload(Change::Same).await
        {
            let mut guard = self.inner.settings.lock().unwrap();
            if let Ok(current) = guard.as_mut() {
                *current = before;
                let _ = settings::save(&source.config_dir, current);
            }
            return Err(RpcError::invalid_params(format!("not saved: {e}")));
        }
        // The session that asked follows the change; the others keep theirs.
        let model = path == "provider" || path.ends_with(".model");
        if let Some(session) = session_id
            .filter(|_| model)
            .and_then(|id| self.inner.session(&id).ok())
        {
            let rt = self.inner.runtime();
            let mut s = session.lock().unwrap();
            let default = rt.model_of(None, &Value::Null);
            if path == "provider" {
                s.pin(default).map_err(RpcError::internal)?;
            } else if let Some(e) = (s.model.clone().unwrap_or(default).0)
                .filter(|e| path == format!("providers.{e}.model"))
                && let Some(p) = rt.models.get(&e)
            {
                s.pin((Some(e), p.model.clone()))
                    .map_err(RpcError::internal)?;
            }
        }
        let all = self
            .inner
            .settings
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        self.inner.emit::<SettingsChanged>(SettingsChangedParams {
            path,
            value,
            settings: all.clone(),
        });
        Ok(all)
    }

    /// Subscribe to every event emitted from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// Handle one request. Unknown methods return `METHOD_NOT_FOUND`.
    /// Long work (turns) runs in the background and reports through events.
    pub async fn handle(&self, method: &str, params: Option<Value>) -> Result<Value, RpcError> {
        match method {
            AttachmentUpload::METHOD => {
                let p = decode::<AttachmentUpload>(params)?;
                let store = self.inner.attachments.clone();
                let image = tokio::task::spawn_blocking(move || store.upload(&p.data, &p.name))
                    .await
                    .map_err(RpcError::internal)?
                    .map_err(RpcError::invalid_params)?;
                Ok(serde_json::to_value(image).unwrap())
            }
            AttachmentRead::METHOD => {
                let p = decode::<AttachmentRead>(params)?;
                let store = self.inner.attachments.clone();
                let data = tokio::task::spawn_blocking(move || store.read(&p.id))
                    .await
                    .map_err(RpcError::internal)?
                    .map_err(RpcError::invalid_params)?;
                Ok(serde_json::to_value(AttachmentReadResult { data }).unwrap())
            }
            Echo::METHOD => dispatch::<Echo, _>(params, |p| self.echo(p)),
            SessionCreate::METHOD => {
                dispatch::<SessionCreate, _>(params, |p| self.session_create(p))
            }
            SessionList::METHOD => {
                let inner = self.inner.clone();
                tokio::task::spawn_blocking(move || {
                    dispatch::<SessionList, _>(params, |_| {
                        inner.sessions.list().map_err(session_error)
                    })
                })
                .await
                .map_err(RpcError::internal)?
            }
            SessionActive::METHOD => {
                dispatch::<SessionActive, _>(params, |_| Ok(self.inner.sessions.active()))
            }
            SessionMessages::METHOD => {
                dispatch::<SessionMessages, _>(params, |p| self.session_messages(p))
            }
            SessionRename::METHOD => {
                dispatch::<SessionRename, _>(params, |p| self.session_rename(p))
            }
            SessionFork::METHOD => dispatch::<SessionFork, _>(params, |p| self.session_fork(p)),
            SessionDelete::METHOD => {
                dispatch::<SessionDelete, _>(params, |p| self.session_delete(p))
            }
            SessionCompact::METHOD => {
                let p: SessionCompactParams = decode::<SessionCompact>(params)?;
                let session = self.inner.session(&p.session_id).map_err(session_error)?;
                if !session.lock().unwrap().writable() {
                    return Err(RpcError::invalid_params(
                        "a running turn's transcript can only change between model calls",
                    ));
                }
                let done = if p.clear {
                    self.inner.clear_compaction(&session)
                } else {
                    self.inner.compact(&session, "manual").await
                };
                // "nothing to compact yet" and the like: plain messages.
                done.map(|d| serde_json::to_value(d).unwrap_or_default())
                    .map_err(|e| RpcError::new(RpcError::INVALID_PARAMS, e))
            }
            TurnStart::METHOD => dispatch::<TurnStart, _>(params, |p| self.turn_start(p)),
            TurnCancel::METHOD => dispatch::<TurnCancel, _>(params, |p| self.turn_cancel(p)),
            ProcessesGet::METHOD => dispatch::<ProcessesGet, _>(params, |p| self.processes_get(p)),
            ProcessesList::METHOD => {
                dispatch::<ProcessesList, _>(params, |_| Ok(self.processes_list()))
            }
            ProcessCancel::METHOD => {
                dispatch::<ProcessCancel, _>(params, |p| self.process_cancel(p))
            }
            ProcessRead::METHOD => dispatch::<ProcessRead, _>(params, |p| {
                self.inner.session(&p.session_id).map_err(session_error)?;
                let (offset, data, total) = self
                    .inner
                    .jobs
                    .read(&p.session_id, &p.id, p.from)
                    .map_err(RpcError::invalid_params)?;
                Ok(ProcessOutput {
                    offset,
                    data,
                    total,
                })
            }),
            ProcessResize::METHOD => dispatch::<ProcessResize, _>(params, |p| {
                self.inner.session(&p.session_id).map_err(session_error)?;
                self.inner
                    .jobs
                    .resize(&p.session_id, &p.id, p.cols, p.rows)
                    .map_err(RpcError::invalid_params)
            }),
            TurnSteer::METHOD => {
                let p: TurnSteerParams = decode::<TurnSteer>(params)?;
                let session = self.inner.session(&p.session_id).map_err(session_error)?;
                if session.lock().unwrap().active.is_none() {
                    return Err(RpcError::invalid_params(
                        "no turn is running; start one with turn/start",
                    ));
                }
                self.queue_add(QueueAddParams {
                    session_id: p.session_id,
                    text: p.text,
                    images: p.images,
                    mode: QueueMode::Steer,
                })
                .await?;
                Ok(Value::Null)
            }
            QueueAdd::METHOD => {
                let p: QueueAddParams = decode::<QueueAdd>(params)?;
                let r = self.queue_add(p).await?;
                Ok(serde_json::to_value(r).unwrap_or_default())
            }
            QueueRemove::METHOD => dispatch::<QueueRemove, _>(params, |p| {
                self.inner.queue_edit(&p.session_id, |s| {
                    let before = s.queue.len();
                    s.queue.retain(|q| q.id != p.id);
                    if s.queue.len() == before {
                        return Err(no_item(p.id));
                    }
                    Ok(())
                })
            }),
            QueueUpdate::METHOD => dispatch::<QueueUpdate, _>(params, |p| {
                // Validate the complete replacement before changing either field.
                self.inner.queue_edit(&p.session_id, |s| {
                    let index = s
                        .queue
                        .iter()
                        .position(|q| q.id == p.id)
                        .ok_or_else(|| no_item(p.id))?;
                    let q = &s.queue[index];
                    let text = p.text.unwrap_or_else(|| q.text.clone());
                    let mut images = p.images.unwrap_or_else(|| q.images.clone());
                    self.inner.validate_input(s, &text, &mut images)?;
                    let q = &mut s.queue[index];
                    q.text = text;
                    q.images = images;
                    if let Some(m) = p.mode {
                        q.mode = m;
                    }
                    Ok(())
                })?;
                Ok(())
            }),
            QueueMove::METHOD => dispatch::<QueueMove, _>(params, |p| {
                self.inner.queue_edit(&p.session_id, |s| {
                    let i = s
                        .queue
                        .iter()
                        .position(|q| q.id == p.id)
                        .ok_or_else(|| no_item(p.id))?;
                    let q = s.queue.remove(i);
                    let to = p.to.min(s.queue.len());
                    s.queue.insert(to, q);
                    Ok(())
                })
            }),
            QueueClear::METHOD => dispatch::<QueueClear, _>(params, |p| {
                self.inner.queue_edit(&p.session_id, |s| {
                    s.queue.clear();
                    s.queue_paused = false;
                    Ok(())
                })
            }),
            QueueResume::METHOD => dispatch::<QueueResume, _>(params, |p| {
                self.inner.queue_edit(&p.session_id, |s| {
                    s.queue_paused = false;
                    Ok(())
                })
            }),
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
            SettingsGet::METHOD => {
                decode::<SettingsGet>(params)?;
                let s = self.inner.settings.lock().unwrap().clone();
                s.map_err(RpcError::invalid_params)
            }
            SettingsSet::METHOD => {
                let p: SettingSet = decode::<SettingsSet>(params)?;
                self.set_setting(p.path, p.value, p.session_id).await
            }
            SettingsReset::METHOD => {
                let p: SettingPath = decode::<SettingsReset>(params)?;
                self.set_setting(p.path, Value::Null, None).await
            }
            SecretsSet::METHOD => {
                let p: SecretSet = decode::<SecretsSet>(params)?;
                let source = self.inner.source.as_ref().ok_or_else(|| {
                    RpcError::invalid_params("this core has no config dir to save keys in")
                })?;
                let names = settings::set_secret(&source.config_dir, &p.provider, p.key.as_deref())
                    .map_err(RpcError::invalid_params)?;
                // The key is used from now on (a reload reads it).
                if let Err(e) = self.inner.reload(Change::Same).await {
                    return Err(RpcError::invalid_params(format!(
                        "saved, but the reload failed: {e}"
                    )));
                }
                Ok(serde_json::to_value(names).unwrap_or_default())
            }
            SecretsList::METHOD => {
                decode::<SecretsList>(params)?;
                let names = match &self.inner.source {
                    Some(s) => settings::load_secrets(&s.config_dir)
                        .map(|v| settings::secret_names(&v))
                        .map_err(RpcError::invalid_params)?,
                    None => Vec::new(),
                };
                Ok(serde_json::to_value(names).unwrap_or_default())
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
            LuaCall::METHOD => {
                let p: LuaCallParams = decode::<LuaCall>(params)?;
                let Some(s) = self.inner.runtime().scripting.clone() else {
                    return Err(RpcError::invalid_params("there is no core Lua to call"));
                };
                let ctx = serde_json::json!({ "session_id": p.session_id, "cwd": p.cwd });
                let args = if p.args.is_null() {
                    serde_json::json!({})
                } else {
                    p.args
                };
                s.call(
                    "_rpc_entry",
                    vec![serde_json::json!(p.name), args, ctx],
                    p.session_id.as_deref(),
                )
                .await
                .map_err(|e| RpcError::new(RpcError::INVALID_PARAMS, e))
            }
            StoreQuery::METHOD => {
                let p: StoreQueryParams = decode::<StoreQuery>(params)?;
                let Some(index) = self.inner.sessions.index().cloned() else {
                    return Err(RpcError::new(
                        RpcError::INTERNAL_ERROR,
                        "the session index is unavailable",
                    ));
                };
                tokio::task::spawn_blocking(move || index.query(&p.sql, &p.params))
                    .await
                    .map_err(|e| RpcError::new(RpcError::INTERNAL_ERROR, e.to_string()))?
                    .map_err(RpcError::invalid_params)
            }
            McpList::METHOD => {
                decode::<McpList>(params)?;
                Ok(serde_json::to_value(self.inner.mcp.list()).unwrap_or_default())
            }
            McpReconnect::METHOD => {
                let p: McpRef = decode::<McpReconnect>(params)?;
                self.inner
                    .mcp
                    .reconnect(&p.name)
                    .map_err(RpcError::invalid_params)?;
                Ok(serde_json::json!({}))
            }
            McpAuth::METHOD => {
                let p: McpRef = decode::<McpAuth>(params)?;
                let (url, redirect_uri) = self
                    .inner
                    .mcp
                    .auth_begin(&p.name)
                    .await
                    .map_err(RpcError::invalid_params)?;
                Ok(serde_json::json!({ "url": url, "redirect_uri": redirect_uri }))
            }
            McpAuthCode::METHOD => {
                let p: McpAuthCodeParams = decode::<McpAuthCode>(params)?;
                self.inner
                    .mcp
                    .auth_code(&p.name, &p.code)
                    .map_err(RpcError::invalid_params)?;
                Ok(serde_json::json!({}))
            }
            McpSignOut::METHOD => {
                let p: McpRef = decode::<McpSignOut>(params)?;
                self.inner
                    .mcp
                    .sign_out(&p.name)
                    .map_err(RpcError::invalid_params)?;
                Ok(serde_json::json!({}))
            }
            ModelList::METHOD => {
                let p = decode::<ModelList>(params)?;
                let session = p.session_id.and_then(|id| self.inner.session(&id).ok());
                let pin = session.and_then(|s| s.lock().unwrap().model.clone());
                Ok(serde_json::to_value(self.inner.runtime().model_list(pin)).unwrap_or_default())
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
            source: Some("client"),
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

    fn processes_get(&self, params: SessionRef) -> Result<ProcessesResult, RpcError> {
        self.inner
            .session(&params.session_id)
            .map_err(session_error)?;
        Ok(ProcessesResult {
            version: self.inner.jobs.version(),
            processes: self
                .inner
                .jobs
                .views(&params.session_id)
                .into_iter()
                .map(|view| process_snapshot(&params.session_id, view))
                .collect(),
        })
    }

    fn processes_list(&self) -> ProcessesResult {
        ProcessesResult {
            version: self.inner.jobs.version(),
            processes: self
                .inner
                .jobs
                .owned_views(None)
                .into_iter()
                .map(|(owner, view)| process_snapshot(&owner, view))
                .collect(),
        }
    }

    fn process_cancel(&self, params: ProcessRef) -> Result<(), RpcError> {
        self.inner
            .session(&params.session_id)
            .map_err(session_error)?;
        self.inner
            .jobs
            .cancel(&params.session_id, &params.id)
            .map_err(RpcError::invalid_params)
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
        self.inner.emit::<SessionCreated>(info.clone());
        Ok(info)
    }

    fn session_rename(&self, p: SessionRenameParams) -> Result<SessionInfo, RpcError> {
        let title = p.title.trim();
        if title.is_empty() {
            return Err(RpcError::invalid_params("the title is empty"));
        }
        let info = self
            .inner
            .sessions
            .rename(&p.session_id, title)
            .map_err(session_error)?;
        self.inner.emit::<SessionUpdated>(SessionUpdatedParams {
            session_id: p.session_id,
            reason: "rename".into(),
        });
        Ok(info)
    }

    fn session_fork(&self, p: SessionForkParams) -> Result<SessionInfo, RpcError> {
        let session = self
            .inner
            .sessions
            .fork(&p.session_id, p.before_turn)
            .map_err(session_error)?;
        let info = session.lock().unwrap().info.clone();
        self.inner
            .started_session(&info.session_id, &info.cwd, true);
        self.inner.emit::<SessionCreated>(info.clone());
        Ok(info)
    }

    fn session_delete(&self, p: SessionRef) -> Result<(), RpcError> {
        let session = self
            .inner
            .sessions
            .get(&p.session_id)
            .map_err(session_error)?;
        if let Some(active) = &session.lock().unwrap().active {
            return Err(RpcError::new(
                RpcError::BUSY,
                format!("turn {} is still running", active.turn_id),
            ));
        }
        self.inner.jobs.cancel_owner(&p.session_id);
        self.inner
            .sessions
            .delete(&p.session_id)
            .map_err(session_error)?;
        self.inner.started.lock().unwrap().remove(&p.session_id);
        self.inner.emit::<SessionDeleted>(p);
        Ok(())
    }

    fn session_messages(&self, params: SessionRef) -> Result<SessionMessagesResult, RpcError> {
        let session = self
            .inner
            .session(&params.session_id)
            .map_err(session_error)?;
        let s = session.lock().unwrap();
        Ok(SessionMessagesResult {
            queue: s.queue.clone(),
            queue_paused: s.queue_paused,
            info: s.info.clone(),
            messages: s.messages.clone(),
            active_turn: s.active.as_ref().map(|a| a.turn_id),
        })
    }

    fn turn_start(&self, params: TurnStartParams) -> Result<TurnStartResult, RpcError> {
        let session = self
            .inner
            .session(&params.session_id)
            .map_err(session_error)?;
        let turn_id = self
            .inner
            .begin_turn_with_images(session, params.text, params.images)?;
        Ok(TurnStartResult { turn_id })
    }

    /// `queue/add`: start a turn when idle, else queue the message.
    async fn queue_add(&self, p: QueueAddParams) -> Result<QueueAddResult, RpcError> {
        let (mut text, mut mode, mut images) = (p.text, p.mode, p.images);
        // queue_add hooks may rewrite the message or refuse it.
        if let Some(s) = self
            .inner
            .runtime()
            .scripting
            .clone()
            .filter(|s| s.has_hook("queue_add"))
        {
            let ev = serde_json::json!({ "session_id": p.session_id, "text": text, "mode": mode, "images": images });
            let out = s.hooks("queue_add", ev).await;
            if let Some(why) = out.deny {
                return Err(RpcError::invalid_params(why));
            }
            if let Some(v) = out.event.get("images") {
                images = serde_json::from_value(crate::agent::list(v))
                    .map_err(RpcError::invalid_params)?;
            }
            if let Some(t) = out.event["text"].as_str() {
                text = t.to_owned();
            }
            if let Ok(m) = serde_json::from_value(out.event["mode"].clone()) {
                mode = m;
            }
        }
        let session = self.inner.session(&p.session_id).map_err(session_error)?;
        self.inner
            .queue_add_with_images(&session, text, mode, images)
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

impl Drop for Core {
    fn drop(&mut self) {
        self.inner.jobs.cancel_all();
    }
}

pub(crate) fn process_snapshot(session_id: &str, view: ProcessView) -> ProcessSnapshot {
    let state = match view.state {
        CoreProcessState::Running => ProcessState::Running,
        CoreProcessState::Exited => ProcessState::Exited,
        CoreProcessState::Cancelled => ProcessState::Cancelled,
        CoreProcessState::TimedOut => ProcessState::TimedOut,
        CoreProcessState::Failed => ProcessState::Failed,
    };
    ProcessSnapshot {
        session_id: session_id.to_owned(),
        id: view.id,
        command: view.command,
        state,
        running: view.running,
        pid: view.pid,
        started_at_ms: view.started_at_ms,
        finished_at_ms: view.finished_at_ms,
        elapsed_ms: view.elapsed_ms,
        tail: view.tail,
        output_bytes: view.output_bytes,
        truncated: view.truncated,
        code: view.code,
        signal: view.signal,
        error: view.error,
        terminal: view.terminal,
    }
}

fn reloaded(r: Result<bone_proto::methods::ReloadResult, String>) -> Result<Value, RpcError> {
    match r {
        Ok(r) => Ok(serde_json::to_value(r).unwrap_or_default()),
        Err(e) => Err(RpcError::new(RpcError::INVALID_PARAMS, e)),
    }
}

fn no_item(id: u64) -> RpcError {
    RpcError::invalid_params(format!("no queued message {id}"))
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
