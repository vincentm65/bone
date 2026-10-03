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
use std::sync::{Arc, mpsc};

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
}

pub enum AskEvent {
    Requested(AskRequestedParams),
    Resolved(AskResolvedParams),
}

/// Handle to the Lua thread.
pub struct Scripting {
    jobs: mpsc::Sender<Job>,
    hooks: HashSet<String>,
    system_prompt_fn: bool,
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
    let (init_tx, init_rx) = mpsc::channel();
    let (jobs, job_rx) = mpsc::channel();
    let (events_tx, events) = tokio_mpsc::unbounded_channel();
    let dir = config_dir.to_owned();
    let jobs_tx = jobs.clone();
    std::thread::Builder::new()
        .name("bone-lua".into())
        .spawn(move || lua_thread(dir, init_tx, job_rx, jobs_tx, events_tx))
        .map_err(|e| format!("cannot start the Lua thread: {e}"))?;
    let ex = init_rx
        .recv()
        .map_err(|_| "the Lua thread died while loading core.lua".to_string())??;

    let config = resolve(config_dir, &ex, env)?;
    let scripting = Arc::new(Scripting {
        jobs,
        hooks: ex.hooks,
        system_prompt_fn: ex.system_prompt_fn,
    });
    Ok(Loaded {
        config,
        scripting,
        tools: ex.tools,
        events,
    })
}

/// Combine Lua settings with environment overrides into the final config.
fn resolve(
    config_dir: &Path,
    ex: &Extracted,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<CoreConfig, String> {
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
        self.hooks.contains(event)
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
    next_ask: AskId,
    /// Coroutines waiting for background work, with a handle to stop it.
    pending: HashMap<u64, (Waiting, tokio::task::AbortHandle)>,
    next_wait: u64,
    events: tokio_mpsc::UnboundedSender<AskEvent>,
    jobs: mpsc::Sender<Job>,
    /// Runs background work for `bone.system`, `bone.sleep`, `bone.http`.
    rt: tokio::runtime::Runtime,
}

fn lua_thread(
    dir: PathBuf,
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
    let lua = match setup(&dir) {
        Ok(lua) => lua,
        Err(e) => {
            let _ = init.send(Err(e));
            return;
        }
    };
    if init
        .send(extract(&lua).map_err(|e| format!("core.lua: {e}")))
        .is_err()
    {
        return;
    }
    let mut st = State {
        lua,
        waiting: HashMap::new(),
        next_ask: 0,
        pending: HashMap::new(),
        next_wait: 0,
        events,
        jobs: jobs_tx,
        rt,
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
            Job::Cancel { session_id } => {
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
                self.next_wait += 1;
                let wait_id = self.next_wait;
                let jobs = self.jobs.clone();
                let task = self.rt.spawn(async move {
                    let result = bone_lua::wait::run(spec).await;
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
            self.next_ask += 1;
            let ask_id = self.next_ask;
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

fn setup(dir: &Path) -> Result<Lua, String> {
    let lua = bone_lua::new_state(Side::Core, Some(dir)).map_err(|e| e.to_string())?;
    let run = |rel: &str| {
        bone_lua::run_runtime(&lua, Some(dir), rel).map_err(|e| format!("runtime/{rel}: {e}"))
    };
    run("core/api.lua")?;
    install_helpers(&lua, dir).map_err(|e| e.to_string())?;
    run("core/defaults.lua")?;
    bone_lua::run_user_plugins(&lua, dir, "core.lua").map_err(|e| e.to_string())?;
    bone_lua::run_file(&lua, &dir.join("core.lua")).map_err(|e| e.to_string())?;
    Ok(lua)
}

fn install_helpers(lua: &Lua, dir: &Path) -> mlua::Result<()> {
    let bone: Table = lua.globals().get("bone")?;
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
            let p: ProviderConfig = serde_json::from_value(from_lua(&v)?)
                .map_err(|e| mlua::Error::runtime(format!("bone.config.providers.{name}: {e}")))?;
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
        providers,
        provider: config.get("provider")?,
        system_prompt,
        system_prompt_fn,
        data_dir: config.get("data_dir")?,
        tools,
        hooks,
    })
}

#[cfg(test)]
mod tests;
