//! What the core's Lua configuration produces, as one swappable snapshot:
//! the config, the provider, the tools and the Lua thread. A turn takes the
//! current snapshot when it starts and keeps it until it ends, so reloading
//! (`core/reload`, a core plugin loaded or unloaded) never changes a running
//! turn. Reloading builds a whole new snapshot on a fresh Lua thread and
//! switches only if that worked; the old one stops once nothing uses it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use bone_proto::methods::ModelInfo;
use bone_proto::methods::{
    AskRequested, AskResolved, PluginInfo, ReloadResult, SessionUpdated, SessionUpdatedParams,
};
use bone_proto::types::ChatMessage;
use serde_json::{Value as Json, json};
use tokio::sync::broadcast;
use tokio::sync::{mpsc, oneshot};

use crate::config::{CoreConfig, ProviderConfig};
use crate::provider::{Completion, CompletionRequest, Delta, OpenAiProvider, Provider};
use crate::scripting::{AskEvent, LoadOptions, Loaded, LuaProvider, LuaTool, Scripting};
use crate::tools::Registry;
use crate::tools::ToolSpec;
use crate::{Event, Inner};

/// How long `bone.on_shutdown` functions get before a reload goes ahead.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(5);
/// Model calls nested in model calls (a Lua provider calling `bone.model`).
pub(crate) const MAX_MODEL_DEPTH: u32 = 4;

pub(crate) struct Runtime {
    pub config: CoreConfig,
    pub provider: Arc<dyn Provider>,
    pub tools: Registry,
    pub scripting: Option<Arc<Scripting>>,
    /// Every `bone.config.providers` entry, for `bone.model` and
    /// `model/complete`, and the name of the one turns use.
    pub models: HashMap<String, ProviderConfig>,
    /// Providers that exist only in settings.json (added in the app).
    pub settings_providers: std::collections::HashSet<String>,
    pub selected: Option<String>,
    /// MCP servers this configuration asks for.
    pub mcp: Vec<crate::mcp::ServerConfig>,
}

impl Runtime {
    /// A runtime without Lua (tests, `Core::new`).
    pub fn plain(config: CoreConfig, provider: Arc<dyn Provider>, tools: Registry) -> Self {
        Runtime {
            config,
            provider,
            tools,
            scripting: None,
            models: HashMap::new(),
            settings_providers: Default::default(),
            selected: None,
            mcp: Vec::new(),
        }
    }

    /// The provider entry and model a call with these settings uses.
    pub fn model_of(&self, name: Option<&str>, options: &Json) -> (Option<String>, String) {
        let entry = name.map(str::to_owned).or_else(|| self.selected.clone());
        let model = options["model"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| match name {
                Some(n) => self.models.get(n).map(|p| p.model.clone()),
                None => None,
            })
            .unwrap_or_else(|| self.config.provider.model.clone());
        (entry, model)
    }

    /// The configured providers, sorted by name.
    pub fn model_list(&self) -> Vec<ModelInfo> {
        let mut out: Vec<ModelInfo> = self
            .models
            .iter()
            .map(|(name, p)| ModelInfo {
                name: name.clone(),
                model: p.model.clone(),
                kind: p.kind.clone(),
                current: self.selected.as_ref() == Some(name),
                base_url: p.base_url.clone(),
                reasoning_effort: p.reasoning_effort.clone(),
                stream_usage: p.stream_usage,
                has_key: p.api_key.is_some(),
                added: self.settings_providers.contains(name),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The provider for a model call: entry `name` (default: the one turns
    /// use) with `options` laid over its settings.
    pub(crate) fn provider_for(
        &self,
        name: Option<&str>,
        options: &Json,
    ) -> Result<Arc<dyn Provider>, String> {
        let overrides = options.as_object().filter(|o| !o.is_empty());
        let name = match (name, overrides) {
            // The turns' own provider, exactly (env overrides included).
            (None, None) => return Ok(self.provider.clone()),
            (Some(n), _) => n.to_owned(),
            (None, Some(_)) => self
                .selected
                .clone()
                .ok_or("there is no provider entry to apply options to")?,
        };
        let mut p = self
            .models
            .get(&name)
            .cloned()
            .ok_or_else(|| format!("no provider {name:?} in bone.config.providers"))?;
        if let Some(o) = overrides {
            if let Some(m) = o.get("model").and_then(Json::as_str) {
                p.model = m.to_owned();
            }
            if let Some(e) = o.get("reasoning_effort").and_then(Json::as_str) {
                p.reasoning_effort = Some(e.to_owned());
            }
            if let Some(opts) = p.options.as_object_mut() {
                for (k, v) in o {
                    opts.insert(k.clone(), v.clone());
                }
            }
        }
        Ok(match &p.kind {
            Some(_) => {
                let scripting = self
                    .scripting
                    .clone()
                    .ok_or("a Lua provider needs core Lua")?;
                Arc::new(LuaProvider::new(&p, scripting))
            }
            None => Arc::new(OpenAiProvider::new(p)),
        })
    }

    /// The runtime `core.lua` described: its provider (unless one is given),
    /// the built-in tools plus Lua tools, and its hooks. Questions asked by
    /// its Lua code are forwarded as events. Needs a tokio runtime.
    pub fn from_loaded(
        loaded: Loaded,
        provider: Option<Arc<dyn Provider>>,
        events: &broadcast::Sender<Event>,
    ) -> Self {
        let p = &loaded.config.provider;
        let provider = provider.unwrap_or_else(|| match &p.kind {
            Some(_) => Arc::new(LuaProvider::new(p, loaded.scripting.clone())),
            None if crate::provider::unconfigured(p) => Arc::new(crate::provider::Unconfigured),
            None => Arc::new(OpenAiProvider::new(p.clone())),
        });
        let mut tools = Registry::builtin();
        for spec in loaded.tools {
            let parallel = loaded.parallel_tools.contains(&spec.name);
            tools.register(Arc::new(LuaTool::new(
                spec,
                loaded.scripting.clone(),
                parallel,
            )));
        }
        let events = events.clone();
        let mut asks = loaded.events;
        tokio::spawn(async move {
            while let Some(ev) = asks.recv().await {
                let ev = match ev {
                    AskEvent::Requested(p) => Event::new::<AskRequested>(p),
                    AskEvent::Resolved(p) => Event::new::<AskResolved>(p),
                };
                let _ = events.send(ev);
            }
        });
        Runtime {
            config: loaded.config,
            provider,
            tools,
            scripting: Some(loaded.scripting),
            models: loaded.providers,
            settings_providers: loaded.settings_providers,
            selected: loaded.provider_name,
            mcp: loaded.mcp,
        }
    }
}

/// One model call: who asks, what for, and where its output goes.
pub(crate) struct ModelCall {
    pub provider: Option<String>,
    pub options: Json,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<Json>,
    pub depth: u32,
    /// The session it is for, if any (for Lua providers and cancelling).
    pub session_id: Option<String>,
    /// Who asked, for the usage record (`"lua"`, `"client"`); `None` when
    /// a provider asked, as its own result counts what it used.
    pub source: Option<&'static str>,
}

impl Inner {
    /// Run a model call outside the agent loop; `on_delta` gets the output
    /// as it streams.
    pub(crate) async fn model_call(
        &self,
        call: ModelCall,
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<Completion, String> {
        if call.depth > MAX_MODEL_DEPTH {
            return Err(format!(
                "model calls nested more than {MAX_MODEL_DEPTH} deep (a provider calling itself?)"
            ));
        }
        let provider = self
            .runtime()
            .provider_for(call.provider.as_deref(), &call.options)?;
        let tools: Vec<ToolSpec> = call
            .tools
            .iter()
            .map(|t| ToolSpec {
                name: t["name"].as_str().unwrap_or_default().to_owned(),
                description: t["description"].as_str().unwrap_or_default().to_owned(),
                parameters: if t["parameters"].is_null() {
                    json!({ "type": "object", "properties": {} })
                } else {
                    t["parameters"].clone()
                },
            })
            .collect();
        let (entry, model) = self
            .runtime()
            .model_of(call.provider.as_deref(), &call.options);
        let session_id = call.session_id.unwrap_or_default();
        let req = CompletionRequest {
            session_id: &session_id,
            messages: &call.messages,
            tools: &tools,
            depth: call.depth,
        };
        let c = provider.complete(req, on_delta).await.map_err(|e| e.0)?;
        if let (Some(source), Some(u)) = (call.source, c.usage) {
            let session = (!session_id.is_empty()).then_some(session_id.as_str());
            self.sessions.record_usage(
                session,
                crate::session::UsageRecord {
                    turn_id: 0,
                    provider: entry,
                    model,
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    cached_tokens: None,
                    source: Some(source.to_owned()),
                },
            );
        }
        Ok(c)
    }
}

/// A completion as Lua and clients see it.
pub(crate) fn completion_json(c: &Completion) -> Json {
    json!({
        "content": c.content,
        "reasoning": c.reasoning,
        "tool_calls": c.tool_calls,
        "usage": c.usage,
    })
}

/// Model calls core Lua has open (`bone.model.stream`): each reads its
/// deltas one at a time, then its result.
#[derive(Default)]
struct ModelStreams {
    next: AtomicU64,
    open: Mutex<HashMap<u64, Arc<ModelStream>>>,
}

struct ModelStream {
    session_id: Option<String>,
    deltas: tokio::sync::Mutex<mpsc::UnboundedReceiver<Json>>,
    result: tokio::sync::Mutex<Option<oneshot::Receiver<Result<Json, String>>>>,
    task: tokio::task::AbortHandle,
}

/// The core, as core Lua reaches it (`bone.session` now). Shared by every
/// runtime of one core; set once the core exists.
#[derive(Default)]
pub struct Host {
    inner: OnceLock<Weak<Inner>>,
    models: ModelStreams,
}

impl Host {
    pub(crate) fn set(&self, inner: &Arc<Inner>) {
        let _ = self.inner.set(Arc::downgrade(inner));
    }

    fn inner(&self) -> Result<Arc<Inner>, String> {
        self.inner
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| "the core is not running".to_string())
    }

    /// `bone.mcp.list()`.
    pub(crate) fn mcp_list(&self) -> mlua::Result<Json> {
        let inner = self.inner().map_err(mlua::Error::runtime)?;
        serde_json::to_value(inner.mcp.list()).map_err(mlua::Error::external)
    }

    /// `{ mcp_call = { server, tool, arguments } }` from core Lua: `{ text,
    /// is_error }`. `None` for other waits.
    pub(crate) async fn mcp_wait(&self, spec: &Json) -> Option<Result<Json, String>> {
        let call = spec.get("mcp_call")?;
        let r = async {
            let inner = self.inner()?;
            let server = call["server"]
                .as_str()
                .ok_or("bone.mcp.call needs a server")?;
            let tool = call["tool"].as_str().ok_or("bone.mcp.call needs a tool")?;
            let args = match &call["arguments"] {
                Json::Null => json!({}),
                a => a.clone(),
            };
            let (text, is_error) = inner.mcp.call(server, tool, args).await?;
            Ok(json!({ "text": text, "is_error": is_error }))
        };
        Some(r.await)
    }

    /// `bone.model.list()`.
    pub(crate) fn model_list(&self) -> mlua::Result<Json> {
        let inner = self.inner().map_err(mlua::Error::runtime)?;
        serde_json::to_value(inner.runtime().model_list()).map_err(mlua::Error::external)
    }

    /// Model waits from core Lua: `{ model_open = req }` starts a call and
    /// gives `{ stream = id }`; `{ model_next = id }` is its next delta (nil
    /// at the end), `{ model_result = id }` its result, `{ model_close = id }`
    /// stops it. `None` for other waits.
    pub(crate) async fn model_wait(
        &self,
        spec: &Json,
        session_id: Option<String>,
    ) -> Option<Result<Json, String>> {
        if spec["model_open"].is_object() {
            return Some(self.model_open(&spec["model_open"], session_id));
        }
        let get = |k: &str| -> Option<Result<Arc<ModelStream>, String>> {
            let id = spec[k].as_u64()?;
            Some(
                self.models
                    .open
                    .lock()
                    .unwrap()
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| "this model call is closed".to_string()),
            )
        };
        if let Some(s) = get("model_next") {
            return Some(match s {
                Ok(s) => Ok(s.deltas.lock().await.recv().await.unwrap_or(Json::Null)),
                Err(e) => Err(e),
            });
        }
        if let Some(s) = get("model_result") {
            let s = match s {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let rx = s.result.lock().await.take();
            let r = match rx {
                Some(rx) => rx.await.unwrap_or_else(|_| Err("cancelled".into())),
                None => Err("this model call's result was already read".into()),
            };
            if let Some(id) = spec["model_result"].as_u64() {
                self.models.open.lock().unwrap().remove(&id);
            }
            return Some(r);
        }
        if let Some(id) = spec["model_close"].as_u64() {
            if let Some(s) = self.models.open.lock().unwrap().remove(&id) {
                s.task.abort();
            }
            return Some(Ok(json!(true)));
        }
        None
    }

    fn model_open(&self, req: &Json, session_id: Option<String>) -> Result<Json, String> {
        let inner = self.inner()?;
        let messages: Vec<ChatMessage> = crate::agent::list(&req["messages"])
            .as_array()
            .ok_or("a model call needs messages (or a prompt)")?
            .iter()
            .map(message)
            .collect::<Result<_, _>>()?;
        let call = ModelCall {
            provider: req["provider"].as_str().map(str::to_owned),
            options: req["options"].clone(),
            messages,
            tools: crate::agent::list(&req["tools"])
                .as_array()
                .cloned()
                .unwrap_or_default(),
            depth: req["depth"].as_u64().unwrap_or(1) as u32,
            session_id: session_id.clone(),
            source: (!req["in_provider"].as_bool().unwrap_or(false)).then_some("lua"),
        };
        let (dtx, drx) = mpsc::unbounded_channel();
        let (rtx, rrx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut on_delta = |d: Delta| {
                let _ = dtx.send(match d {
                    Delta::Text(t) => json!({ "text": t }),
                    Delta::Reasoning(t) => json!({ "reasoning": t }),
                });
            };
            let r = inner.model_call(call, &mut on_delta).await;
            let _ = rtx.send(r.map(|c| completion_json(&c)));
        });
        let id = self.models.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.models.open.lock().unwrap().insert(
            id,
            Arc::new(ModelStream {
                session_id,
                deltas: tokio::sync::Mutex::new(drx),
                result: tokio::sync::Mutex::new(Some(rrx)),
                task: task.abort_handle(),
            }),
        );
        Ok(json!({ "stream": id }))
    }

    /// A session's turn was cancelled: stop the model calls its Lua made.
    pub(crate) fn cancel_models(&self, session_id: &str) {
        self.models.open.lock().unwrap().retain(|_, s| {
            let ours = s.session_id.as_deref() == Some(session_id);
            if ours {
                s.task.abort();
            }
            !ours
        });
    }

    /// `bone.session.*`: `{ op = "messages" | "append" | "compact", id,
    /// message?, messages? }`.
    pub(crate) fn session_op(&self, spec: &Json) -> Result<Json, String> {
        let inner = self.inner()?;
        let id = spec["id"].as_str().ok_or("a session id is needed")?;
        let session = inner.sessions.get(id).map_err(|e| e.to_string())?;
        let op = spec["op"].as_str().unwrap_or_default();
        // The queue (bone.queue): no turn restrictions, and its own event.
        match op {
            "queue_list" => {
                let s = session.lock().unwrap();
                return serde_json::to_value(&s.queue).map_err(|e| e.to_string());
            }
            "queue_add" => {
                let text = spec["text"].as_str().unwrap_or_default().to_owned();
                if text.trim().is_empty() {
                    return Err("bone.queue.add needs text".into());
                }
                let mut mode: bone_proto::methods::QueueMode =
                    serde_json::from_value(spec["mode"].clone()).unwrap_or_default();
                if session.lock().unwrap().active.is_none()
                    && let Ok(turn_id) = inner.begin_turn(session.clone(), text.clone())
                {
                    return Ok(json!({ "turn_id": turn_id }));
                }
                let mut s = session.lock().unwrap();
                let ending = s
                    .active
                    .as_ref()
                    .is_none_or(|a| a.closing || a.cancel.is_cancelled());
                if ending {
                    mode = bone_proto::methods::QueueMode::Next;
                }
                let qid = s.enqueue(text, mode);
                inner.emit_queue(&s);
                return Ok(json!({ "id": qid }));
            }
            "queue_remove" | "queue_clear" => {
                {
                    let mut s = session.lock().unwrap();
                    match spec["queue_id"].as_u64() {
                        Some(qid) if op == "queue_remove" => s.queue.retain(|q| q.id != qid),
                        _ => s.queue.clear(),
                    }
                    s.save_queue();
                    inner.emit_queue(&s);
                }
                return Ok(json!(true));
            }
            _ => {}
        }
        let reason = match op {
            "messages" => {
                let s = session.lock().unwrap();
                return serde_json::to_value(&s.messages).map_err(|e| e.to_string());
            }
            "append" => {
                let msg = message(&spec["message"])?;
                let mut s = session.lock().unwrap();
                if !s.writable() {
                    return Err(NOT_NOW.into());
                }
                s.push(msg)
                    .map_err(|e| format!("cannot save session: {e}"))?;
                "append"
            }
            "compact" => {
                let msgs = crate::agent::list(&spec["messages"])
                    .as_array()
                    .ok_or("compact needs a list of messages")?
                    .iter()
                    .map(message)
                    .collect::<Result<Vec<_>, _>>()?;
                let mut s = session.lock().unwrap();
                if !s.writable() {
                    return Err(NOT_NOW.into());
                }
                s.compact(msgs)
                    .map_err(|e| format!("cannot save session: {e}"))?;
                "compact"
            }
            other => return Err(format!("unknown session operation {other:?}")),
        };
        inner.emit::<SessionUpdated>(SessionUpdatedParams {
            session_id: id.to_owned(),
            reason: reason.into(),
        });
        Ok(json!(true))
    }
}

const NOT_NOW: &str = "a running turn's transcript can only change between model calls \
(in system, context, request or request_error hooks), or after the turn";

/// A message from Lua, where an empty list may have arrived as `{}`.
fn message(v: &Json) -> Result<ChatMessage, String> {
    let mut v = v.clone();
    if let Some(calls) = v.get("tool_calls") {
        v["tool_calls"] = crate::agent::list(calls);
    }
    serde_json::from_value(v).map_err(|e| format!("bad message: {e}"))
}

/// Looks up an environment variable (tests use a fixed set).
pub(crate) type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Where the configuration came from, to load it again.
pub(crate) struct Source {
    pub config_dir: PathBuf,
    pub env: EnvLookup,
    pub options: Mutex<LoadOptions>,
    /// A provider kept across reloads instead of the configured one (tests).
    pub provider: Option<Arc<dyn Provider>>,
}

impl Source {
    pub fn new(loaded: &Loaded, provider: Option<Arc<dyn Provider>>) -> Self {
        Source {
            config_dir: loaded.config_dir.clone(),
            env: Arc::new(|k| std::env::var(k).ok().filter(|v| !v.is_empty())),
            options: Mutex::new(loaded.options.clone()),
            provider,
        }
    }

    /// The plugins folder's plugins, and whether each one's `core.lua` runs.
    pub fn plugins(&self) -> Vec<PluginInfo> {
        let disabled = self.options.lock().unwrap().disabled.clone();
        bone_lua::plugins(&self.config_dir)
            .into_iter()
            .map(|dir| {
                let name = bone_lua::plugin_name(&dir);
                let core = dir.join("core.lua").is_file();
                PluginInfo {
                    loaded: core && !disabled.contains(&name),
                    core,
                    name,
                }
            })
            .collect()
    }
}

/// Which plugins a reload leaves out.
pub(crate) enum Change {
    /// The same ones as now.
    Same,
    Enable(String),
    Disable(String),
}

impl Inner {
    /// The runtime new work uses.
    pub(crate) fn runtime(&self) -> Arc<Runtime> {
        self.runtime.read().unwrap().clone()
    }

    /// Load the configuration again (with `change` applied to the plugins)
    /// and switch to it. On failure nothing changes.
    pub(crate) async fn reload(&self, change: Change) -> Result<ReloadResult, String> {
        let source = self
            .source
            .as_ref()
            .ok_or("this core was not loaded from a config dir, so it cannot reload")?;
        // One reload at a time.
        let _one = self.reloading.lock().await;
        let mut options = source.options.lock().unwrap().clone();
        if let Change::Enable(name) | Change::Disable(name) = &change {
            let plugins = source.plugins();
            let p = plugins
                .iter()
                .find(|p| &p.name == name)
                .ok_or_else(|| format!("no plugin {name} in the plugins folder"))?;
            if !p.core {
                return Err(format!("plugin {name} has no core.lua"));
            }
        }
        match &change {
            Change::Same => {}
            Change::Enable(name) => {
                options.disabled.remove(name);
            }
            Change::Disable(name) => {
                options.disabled.insert(name.clone());
            }
        }
        let dir = source.config_dir.clone();
        let load_options = options.clone();
        // Loading runs Lua that may block; keep it off the async workers.
        let env = source.env.clone();
        let loaded = tokio::task::spawn_blocking(move || {
            crate::scripting::load_with_options(&dir, &|k| env(k), &load_options)
        })
        .await
        .map_err(|e| format!("loading stopped: {e}"))??;
        let mut warnings = Vec::new();
        if loaded.config.data_dir != self.data_dir {
            warnings.push(format!(
                "data_dir changed to {}; sessions stay in {} until bone restarts",
                loaded.config.data_dir.display(),
                self.data_dir.display()
            ));
        }
        let next = Arc::new(Runtime::from_loaded(
            loaded,
            source.provider.clone(),
            &self.events,
        ));
        let old = self.runtime();
        if let Some(s) = &old.scripting {
            warnings.extend(s.shutdown(SHUTDOWN_LIMIT).await);
            // Its open questions can still be answered while turns use it.
            let mut retired = self.retired.lock().unwrap();
            retired.retain(|w| w.strong_count() > 0);
            retired.push(Arc::downgrade(s));
        }
        self.mcp.apply(next.mcp.clone());
        *self.runtime.write().unwrap() = next;
        *source.options.lock().unwrap() = options;
        let result = ReloadResult {
            plugins: source.plugins(),
            warnings,
        };
        self.emit::<bone_proto::methods::CoreReloaded>(result.clone());
        Ok(result)
    }

    /// Every Lua thread that may hold an open question: the current one,
    /// then replaced ones still in use.
    pub(crate) fn all_scripting(&self) -> Vec<Arc<Scripting>> {
        let mut out: Vec<Arc<Scripting>> = self.runtime().scripting.iter().cloned().collect();
        out.extend(
            self.retired
                .lock()
                .unwrap()
                .iter()
                .filter_map(|w| w.upgrade()),
        );
        out
    }
}
