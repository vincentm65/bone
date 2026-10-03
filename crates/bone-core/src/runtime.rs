//! What the core's Lua configuration produces, as one swappable snapshot:
//! the config, the provider, the tools and the Lua thread. A turn takes the
//! current snapshot when it starts and keeps it until it ends, so reloading
//! (`core/reload`, a core plugin loaded or unloaded) never changes a running
//! turn. Reloading builds a whole new snapshot on a fresh Lua thread and
//! switches only if that worked; the old one stops once nothing uses it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use bone_proto::methods::{
    AskRequested, AskResolved, PluginInfo, ReloadResult, SessionUpdated, SessionUpdatedParams,
};
use bone_proto::types::ChatMessage;
use serde_json::{Value as Json, json};
use tokio::sync::broadcast;

use crate::config::CoreConfig;
use crate::provider::{OpenAiProvider, Provider};
use crate::scripting::{AskEvent, LoadOptions, Loaded, LuaProvider, LuaTool, Scripting};
use crate::tools::Registry;
use crate::{Event, Inner};

/// How long `bone.on_shutdown` functions get before a reload goes ahead.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(5);

pub(crate) struct Runtime {
    pub config: CoreConfig,
    pub provider: Arc<dyn Provider>,
    pub tools: Registry,
    pub scripting: Option<Arc<Scripting>>,
}

impl Runtime {
    /// A runtime without Lua (tests, `Core::new`).
    pub fn plain(config: CoreConfig, provider: Arc<dyn Provider>, tools: Registry) -> Self {
        Runtime {
            config,
            provider,
            tools,
            scripting: None,
        }
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
            None => Arc::new(OpenAiProvider::new(p.clone())),
        });
        let mut tools = Registry::builtin();
        for spec in loaded.tools {
            tools.register(Arc::new(LuaTool::new(spec, loaded.scripting.clone())));
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
        }
    }
}

/// The core, as core Lua reaches it (`bone.session` now). Shared by every
/// runtime of one core; set once the core exists.
#[derive(Default)]
pub struct Host {
    inner: OnceLock<Weak<Inner>>,
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

    /// `bone.session.*`: `{ op = "messages" | "append" | "compact", id,
    /// message?, messages? }`.
    pub(crate) fn session_op(&self, spec: &Json) -> Result<Json, String> {
        let inner = self.inner()?;
        let id = spec["id"].as_str().ok_or("a session id is needed")?;
        let session = inner.sessions.get(id).map_err(|e| e.to_string())?;
        let op = spec["op"].as_str().unwrap_or_default();
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
