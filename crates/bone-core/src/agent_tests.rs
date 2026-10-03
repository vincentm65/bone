//! Agent loop tests against a scripted provider.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bone_proto::methods::*;
use bone_proto::types::*;
use bone_proto::{Method, Notification, RpcError};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::config::{CoreConfig, ProviderConfig};
use crate::provider::{Completion, CompletionRequest, Delta, DeltaSink, Provider, ProviderError};
use crate::tools::Registry;
use crate::{Core, Event};

enum Step {
    Reply(Completion),
    /// Reply after this many milliseconds.
    Delay(u64, Completion),
    /// Stream this text, then hang until cancelled.
    Hang(String),
    Fail(String),
}

#[derive(Default)]
struct Scripted {
    steps: Mutex<VecDeque<Step>>,
    /// Messages sent on each request (system prompt excluded).
    seen: Mutex<Vec<Vec<ChatMessage>>>,
    /// Tool names offered on each request.
    tools: Mutex<Vec<Vec<String>>>,
    /// The system prompt of each request.
    systems: Mutex<Vec<String>>,
}

impl Provider for Scripted {
    fn complete<'a>(
        &'a self,
        req: CompletionRequest<'a>,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, ProviderError>> {
        self.seen.lock().unwrap().push(req.messages[1..].to_vec());
        if let ChatMessage::System { content } = &req.messages[0] {
            self.systems.lock().unwrap().push(content.clone());
        }
        self.tools
            .lock()
            .unwrap()
            .push(req.tools.iter().map(|t| t.name.clone()).collect());
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unscripted request");
        Box::pin(async move {
            match step {
                Step::Reply(c) => {
                    if !c.content.is_empty() {
                        on_delta(Delta::Text(c.content.clone()));
                    }
                    Ok(c)
                }
                Step::Delay(ms, c) => {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    if !c.content.is_empty() {
                        on_delta(Delta::Text(c.content.clone()));
                    }
                    Ok(c)
                }
                Step::Hang(text) => {
                    on_delta(Delta::Text(text));
                    std::future::pending().await
                }
                Step::Fail(msg) => Err(ProviderError(msg)),
            }
        })
    }
}

fn text(s: &str) -> Step {
    Step::Reply(Completion {
        content: s.into(),
        ..Default::default()
    })
}

fn calls(calls: &[(&str, &str, Value)]) -> Step {
    Step::Reply(Completion {
        tool_calls: calls
            .iter()
            .map(|(id, name, args)| ToolCall {
                id: (*id).into(),
                name: (*name).into(),
                // A JSON string stands for raw (possibly invalid) argument text.
                arguments: args
                    .as_str()
                    .map_or_else(|| args.to_string(), str::to_owned),
            })
            .collect(),
        ..Default::default()
    })
}

struct Harness {
    core: Core,
    provider: Arc<Scripted>,
    events: broadcast::Receiver<Event>,
    session_id: String,
    work: tempfile::TempDir,
    _data: tempfile::TempDir,
}

impl Harness {
    async fn new(steps: Vec<Step>) -> Self {
        let data = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let config = CoreConfig {
            provider: ProviderConfig {
                kind: None,
                options: serde_json::Value::Null,
                base_url: "http://unused".into(),
                model: "m".into(),
                api_key: None,
                reasoning_effort: None,
                stream_usage: true,
            },
            system_prompt: None,
            data_dir: data.path().to_owned(),
            parallel_tools: true,
        };
        let provider = Arc::new(Scripted {
            steps: Mutex::new(steps.into()),
            ..Default::default()
        });
        let core = Core::with_parts(config, provider.clone(), Registry::builtin());
        Self::finish(core, provider, work, data).await
    }

    /// A core running `core_lua` (plus the runtime and built-in plugins).
    async fn with_lua(core_lua: &str, steps: Vec<Step>) -> Self {
        Self::with_plugins(&[], core_lua, steps).await
    }

    /// With plugins from `examples/plugins/` installed.
    async fn with_plugins(plugins: &[&str], core_lua: &str, steps: Vec<Step>) -> Self {
        let data = tempfile::tempdir().unwrap();
        for name in plugins {
            let dir = data.path().join("plugins").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/plugins")
                .join(name)
                .join("core.lua");
            std::fs::copy(example, dir.join("core.lua")).unwrap();
        }
        let work = tempfile::tempdir().unwrap();
        let lua = format!(
            "bone.config.providers.x = {{ base_url = \"http://unused\", model = \"m\" }}
{core_lua}"
        );
        std::fs::write(data.path().join("core.lua"), lua).unwrap();
        let loaded = crate::scripting::load_with(data.path(), &|_| None).unwrap();
        let provider = Arc::new(Scripted {
            steps: Mutex::new(steps.into()),
            ..Default::default()
        });
        let core = Core::with_lua(loaded, provider.clone());
        Self::finish(core, provider, work, data).await
    }

    async fn finish(
        core: Core,
        provider: Arc<Scripted>,
        work: tempfile::TempDir,
        data: tempfile::TempDir,
    ) -> Self {
        let events = core.subscribe();
        let mut h = Harness {
            core,
            provider,
            events,
            session_id: String::new(),
            work,
            _data: data,
        };
        let info: SessionInfo = h
            .call::<SessionCreate>(SessionCreateParams {
                cwd: Some(h.work.path().to_string_lossy().into()),
            })
            .await
            .unwrap();
        h.session_id = info.session_id;
        h
    }

    async fn call<M: Method>(&self, params: M::Params) -> Result<M::Result, RpcError> {
        let v = self
            .core
            .handle(M::METHOD, Some(serde_json::to_value(params).unwrap()))
            .await?;
        Ok(serde_json::from_value(v).unwrap())
    }

    async fn start(&self, text: &str) -> TurnId {
        self.call::<TurnStart>(TurnStartParams {
            session_id: self.session_id.clone(),
            text: text.into(),
        })
        .await
        .unwrap()
        .turn_id
    }

    async fn next(&mut self) -> Event {
        tokio::time::timeout(Duration::from_secs(5), self.events.recv())
            .await
            .expect("event")
            .unwrap()
    }

    /// Skip events until one of type `N`.
    async fn until<N: Notification>(&mut self) -> N::Params {
        loop {
            let e = self.next().await;
            if e.method == N::METHOD {
                return serde_json::from_value(e.params).unwrap();
            }
        }
    }

    async fn transcript(&self) -> Vec<ChatMessage> {
        self.call::<SessionMessages>(SessionRef {
            session_id: self.session_id.clone(),
        })
        .await
        .unwrap()
        .messages
    }

    async fn cancel(&self) {
        self.call::<TurnCancel>(SessionRef {
            session_id: self.session_id.clone(),
        })
        .await
        .unwrap();
    }
}

impl Event {
    fn parse_as<N: Notification>(&self) -> Option<serde_json::Result<N::Params>> {
        (self.method == N::METHOD).then(|| serde_json::from_value(self.params.clone()))
    }
}

fn tool_result(m: &ChatMessage) -> (&str, bool) {
    match m {
        ChatMessage::Tool {
            content, is_error, ..
        } => (content, *is_error),
        other => panic!("expected tool message, got {other:?}"),
    }
}

#[tokio::test]
async fn runs_tools_until_the_model_answers() {
    let mut h = Harness::new(vec![
        calls(&[
            (
                "c1",
                "write_file",
                json!({"path": "a.txt", "content": "hi"}),
            ),
            ("c2", "nope", json!({})),
        ]),
        calls(&[("c3", "read_file", Value::String("{broken".into()))]),
        text("all done"),
    ])
    .await;
    let turn_id = h.start("make a.txt").await;

    let started = h.until::<ToolStarted>().await;
    assert_eq!(
        (started.turn_id, started.call.name.as_str()),
        (turn_id, "write_file")
    );
    let finished = h.until::<TurnFinished>().await;
    assert_eq!(finished.outcome, TurnOutcome::Completed);

    assert_eq!(
        std::fs::read_to_string(h.work.path().join("a.txt")).unwrap(),
        "hi"
    );
    let t = h.transcript().await;
    assert_eq!(t.len(), 7, "{t:#?}"); // user, asst, tool, tool, asst, tool, asst
    assert!(!tool_result(&t[2]).1);
    assert!(tool_result(&t[3]).0.starts_with("Unknown tool \"nope\""));
    assert!(
        tool_result(&t[5])
            .0
            .starts_with("arguments are not valid JSON")
    );
    assert_eq!(
        t[6],
        ChatMessage::Assistant {
            content: "all done".into(),
            reasoning: String::new(),
            tool_calls: vec![]
        }
    );

    // The model saw every tool result before answering.
    assert_eq!(h.provider.seen.lock().unwrap()[2].len(), 6);
}

#[tokio::test]
async fn hooks_run_at_every_step() {
    let mut h = Harness::with_lua(
        r#"
        bone.hook("turn_start", function(ev) return { text = ev.text .. " (via hook)" } end)
        bone.hook("request", function(ev)
          local keep = {}
          for _, t in ipairs(ev.tools) do if t.name ~= "shell" then keep[#keep + 1] = t end end
          table.insert(ev.messages, 2, { role = "system", content = "extra context" })
          return { tools = keep, messages = ev.messages }
        end)
        bone.hook("message", function(ev)
          if ev.content ~= "" then return { content = ev.content .. "!" } end
        end)
        bone.hook("tool_call", function(ev)
          if ev.name == "write_file" then
            local answer = bone.ask({ kind = "confirm", path = ev.arguments.path })
            if answer ~= "yes" then return { deny = "not confirmed" } end
            return { arguments = { path = ev.arguments.path, content = "from hook" } }
          end
        end)
        bone.hook("tool_result", function(ev) return { output = "seen: " .. ev.output } end)
        bone.hook("turn_end", function(ev) print("ended " .. ev.outcome.status) end)
        "#,
        vec![
            calls(&[
                (
                    "c1",
                    "write_file",
                    json!({"path": "a.txt", "content": "hi"}),
                ),
                (
                    "c2",
                    "write_file",
                    json!({"path": "b.txt", "content": "hi"}),
                ),
            ]),
            text("done"),
        ],
    )
    .await;
    h.start("go").await;
    let started = h.until::<TurnStarted>().await;
    assert_eq!(started.text, "go (via hook)");

    let q = h.until::<AskRequested>().await;
    assert_eq!(
        (q.session_id.as_deref(), q.question["path"].as_str()),
        (Some(h.session_id.as_str()), Some("a.txt"))
    );
    h.call::<AskRespond>(AskRespondParams {
        ask_id: q.ask_id,
        answer: json!("yes"),
    })
    .await
    .unwrap();
    assert_eq!(h.until::<AskResolved>().await.answer, "yes");
    let q = h.until::<AskRequested>().await;
    h.call::<AskRespond>(AskRespondParams {
        ask_id: q.ask_id,
        answer: json!("no"),
    })
    .await
    .unwrap();
    assert_eq!(
        h.until::<TurnFinished>().await.outcome,
        TurnOutcome::Completed
    );
    // Answering twice is an error.
    let err = h
        .call::<AskRespond>(AskRespondParams {
            ask_id: q.ask_id,
            answer: json!("no"),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, RpcError::INVALID_PARAMS);

    assert_eq!(
        std::fs::read_to_string(h.work.path().join("a.txt")).unwrap(),
        "from hook"
    );
    assert!(!h.work.path().join("b.txt").exists());
    let t = h.transcript().await;
    assert_eq!(
        t[0],
        ChatMessage::User {
            content: "go (via hook)".into()
        }
    );
    assert!(tool_result(&t[2]).0.starts_with("seen: Created"), "{t:#?}");
    assert_eq!(tool_result(&t[3]), ("not confirmed", true));
    assert_eq!(
        t[4],
        ChatMessage::Assistant {
            content: "done!".into(),
            reasoning: String::new(),
            tool_calls: vec![]
        }
    );
    // The request hook removed a tool and added a message.
    assert!(!h.provider.tools.lock().unwrap()[0].contains(&"shell".to_owned()));
    assert_eq!(
        h.provider.seen.lock().unwrap()[0][0],
        ChatMessage::System {
            content: "extra context".into()
        }
    );
    let log = std::fs::read_to_string(h._data.path().join("core.log")).unwrap();
    assert_eq!(
        log,
        "ended completed
"
    );
}

#[tokio::test]
async fn turn_start_hooks_can_refuse() {
    let mut h = Harness::with_lua(
        r#"bone.hook("turn_start", function(ev) return { deny = "quiet hours" } end)"#,
        vec![],
    )
    .await;
    h.start("go").await;
    assert_eq!(
        h.until::<TurnFinished>().await.outcome,
        TurnOutcome::Failed {
            message: "quiet hours".into()
        }
    );
    assert!(h.transcript().await.is_empty());
}

#[tokio::test]
async fn cancel_while_streaming_keeps_partial_text() {
    let mut h = Harness::new(vec![Step::Hang("partial".into()), text("next")]).await;
    h.start("one").await;
    let delta = h.until::<MessageDelta>().await;
    assert_eq!(delta.text, "partial");

    // A second turn on a busy session is refused.
    let err = h
        .call::<TurnStart>(TurnStartParams {
            session_id: h.session_id.clone(),
            text: "x".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, RpcError::BUSY);

    h.cancel().await;
    assert_eq!(
        h.until::<TurnFinished>().await.outcome,
        TurnOutcome::Cancelled
    );
    let t = h.transcript().await;
    assert_eq!(
        t[1],
        ChatMessage::Assistant {
            content: "partial".into(),
            reasoning: String::new(),
            tool_calls: vec![]
        }
    );

    // The session is usable again.
    let turn = h.start("two").await;
    assert_eq!(turn, 2);
    assert_eq!(
        h.until::<TurnFinished>().await.outcome,
        TurnOutcome::Completed
    );
}

#[tokio::test]
async fn cancel_while_asking_closes_every_tool_call() {
    // The approve plugin asks before shell commands.
    let mut h = Harness::with_plugins(
        &["approve"],
        "",
        vec![
            calls(&[
                ("c1", "shell", json!({"command": "true"})),
                ("c2", "shell", json!({"command": "true"})),
            ]),
            text("after"),
        ],
    )
    .await;
    h.start("go").await;
    let q = h.until::<AskRequested>().await;
    assert_eq!(
        (q.question["kind"].as_str(), q.question["tool"].as_str()),
        (Some("approval"), Some("shell"))
    );
    h.cancel().await;
    // The question is dropped (answer null) and the turn ends, in either order.
    let (mut resolved, mut outcome) = (None, None);
    while resolved.is_none() || outcome.is_none() {
        let e = h.next().await;
        if let Some(Ok(r)) = Event::parse_as::<AskResolved>(&e) {
            resolved = Some(r);
        } else if let Some(Ok(f)) = Event::parse_as::<TurnFinished>(&e) {
            outcome = Some(f.outcome);
        }
    }
    let resolved = resolved.unwrap();
    assert_eq!((resolved.ask_id, resolved.answer), (q.ask_id, Value::Null));
    assert_eq!(outcome, Some(TurnOutcome::Cancelled));

    // Every tool call has a result, so the next request is well-formed.
    let t = h.transcript().await;
    assert_eq!(t.len(), 4, "{t:#?}");
    assert!(tool_result(&t[2]).1 && tool_result(&t[3]).1);
}

#[tokio::test]
async fn provider_errors_fail_the_turn() {
    let mut h = Harness::new(vec![Step::Fail("HTTP 500: boom".into())]).await;
    h.start("go").await;
    assert_eq!(
        h.until::<TurnFinished>().await.outcome,
        TurnOutcome::Failed {
            message: "HTTP 500: boom".into()
        }
    );
}

#[tokio::test]
async fn sessions_reload_from_disk() {
    let mut h = Harness::new(vec![text("hello")]).await;
    h.start("hi there").await;
    h.until::<TurnFinished>().await;

    let config = h.core.inner.runtime().config.clone();
    let fresh = Core::with_parts(config, h.provider.clone(), Registry::builtin());
    let list: Vec<SessionInfo> =
        serde_json::from_value(fresh.handle(SessionList::METHOD, None).await.unwrap()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title.as_deref(), Some("hi there"));
    let msgs: SessionMessagesResult = serde_json::from_value(
        fresh
            .handle(
                SessionMessages::METHOD,
                Some(json!({"session_id": h.session_id})),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(msgs.messages, h.transcript().await);
}

#[tokio::test]
async fn tools_run_without_asking_by_default() {
    let mut h = Harness::with_lua(
        "",
        vec![
            calls(&[("c1", "shell", json!({"command": "echo ran"}))]),
            text("done"),
        ],
    )
    .await;
    h.start("go").await;
    loop {
        let e = h.next().await;
        assert!(e.parse_as::<AskRequested>().is_none(), "nothing should ask");
        if let Some(f) = e.parse_as::<ToolFinished>() {
            assert!(f.unwrap().output.starts_with("ran\n"));
        }
        if let Some(f) = e.parse_as::<TurnFinished>() {
            assert_eq!(f.unwrap().outcome, TurnOutcome::Completed);
            break;
        }
    }
}

#[tokio::test]
async fn health_check_reports_core_and_lua_checks() {
    let h = Harness::with_lua(
        r#"
        bone.tool.register { name = "mine", run = function() return "" end }
        bone.health("echo", function()
          local r = bone.system("printf fine")
          return r.code == 0, r.stdout
        end)
        bone.health("warns", function() return "warn", "careful" end)
        bone.health("breaks", function() error("oops") end)
        "#,
        vec![],
    )
    .await;
    let items = h.call::<HealthCheck>(Empty {}).await.unwrap();
    let get = |n: &str| {
        let i = items
            .iter()
            .find(|i| i.name == n)
            .unwrap_or_else(|| panic!("{n}: {items:?}"));
        (i.status, i.message.clone())
    };
    use bone_proto::methods::HealthStatus::*;
    assert_eq!(get("provider").0, Ok);
    // "http://unused" is remote and has no key, and nothing answers there.
    assert_eq!(get("api key").0, Warn);
    assert_eq!(get("reachable").0, Error);
    assert_eq!(get("sessions").0, Ok);
    assert_eq!(get("core lua"), (Ok, "loaded; Lua tools: mine".into()));
    assert_eq!(get("echo"), (Ok, "fine".into()));
    assert_eq!(get("warns"), (Warn, "careful".into()));
    let (status, msg) = get("breaks");
    assert!(status == Error && msg.contains("oops"), "{msg}");
}

// ---- reloading the Lua configuration ----------------------------------------

impl Harness {
    /// Replace `core.lua` (with the test provider entry in front).
    fn write_core_lua(&self, core_lua: &str) {
        std::fs::write(
            self._data.path().join("core.lua"),
            format!("bone.config.providers.x = {{ base_url = \"http://unused\", model = \"m\" }}\n{core_lua}"),
        )
        .unwrap();
    }

    fn tool_names(&self) -> Vec<String> {
        self.core
            .inner
            .runtime()
            .tools
            .specs()
            .iter()
            .map(|s| s.name.clone())
            .collect()
    }
}

#[tokio::test]
async fn reload_switches_config_but_running_turns_keep_theirs() {
    let mut h = Harness::with_lua(
        r#"bone.tool.register { name = "probe", run = function() bone.sleep(300) return "old" end }"#,
        vec![
            calls(&[("c1", "probe", json!({}))]),
            text("one"),
            calls(&[("c2", "probe", json!({}))]),
            text("two"),
        ],
    )
    .await;
    h.start("first").await;
    h.until::<ToolStarted>().await;
    // Reload while the old probe is still sleeping.
    h.write_core_lua(
        r#"bone.tool.register { name = "probe", run = function() return "new" end }
           bone.tool.register { name = "extra", run = function() return "" end }"#,
    );
    let r = h.core.reload().await.unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(h.tool_names().contains(&"extra".to_owned()));
    let reloaded = h.until::<CoreReloaded>().await;
    assert_eq!(reloaded, r);
    h.until::<TurnFinished>().await;
    h.start("second").await;
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    assert_eq!(tool_result(&t[2]).0, "old", "{t:#?}");
    assert_eq!(tool_result(&t[6]).0, "new", "{t:#?}");
    // The old Lua thread is gone once its turn is.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        h.core
            .inner
            .retired
            .lock()
            .unwrap()
            .iter()
            .all(|w| w.strong_count() == 0)
    );
}

#[tokio::test]
async fn a_failed_reload_changes_nothing() {
    let h = Harness::with_lua(
        r#"bone.tool.register { name = "probe", run = function() return "kept" end }"#,
        vec![],
    )
    .await;
    h.write_core_lua("error('broken config')");
    let e = h.core.reload().await.unwrap_err();
    assert!(e.contains("broken config"), "{e}");
    assert!(h.tool_names().contains(&"probe".to_owned()));
    let e = h.core.handle(CoreReload::METHOD, None).await.unwrap_err();
    assert!(e.message.contains("broken config"), "{e:?}");
}

#[tokio::test]
async fn core_plugins_load_and_unload_by_reloading() {
    let h = Harness::with_lua("", vec![]).await;
    let plugins = h._data.path().join("plugins");
    std::fs::create_dir_all(plugins.join("p")).unwrap();
    std::fs::create_dir_all(plugins.join("ui_only")).unwrap();
    std::fs::write(
        plugins.join("p/core.lua"),
        r#"bone.tool.register { name = "p_tool", run = function() return bone.plugin.current() and "" or "ok" end }
           bone.on_shutdown(function() bone.state.save("p-bye", { at = "shutdown" }) end)"#,
    )
    .unwrap();
    std::fs::write(plugins.join("ui_only/tui.lua"), "").unwrap();

    let list = h.call::<PluginList>(Empty {}).await.unwrap();
    assert_eq!(
        list,
        vec![
            PluginInfo {
                name: "p".into(),
                core: true,
                loaded: true
            },
            PluginInfo {
                name: "ui_only".into(),
                core: false,
                loaded: false
            },
        ]
    );
    // Installed after startup: a reload picks it up.
    assert!(!h.tool_names().contains(&"p_tool".to_owned()));
    h.call::<CoreReload>(Empty {}).await.unwrap();
    assert!(h.tool_names().contains(&"p_tool".to_owned()));

    let name = || PluginRef { name: "p".into() };
    let r = h.call::<PluginUnload>(name()).await.unwrap();
    assert!(!r.plugins[0].loaded);
    assert!(!h.tool_names().contains(&"p_tool".to_owned()));
    // Its shutdown hook ran on the outgoing configuration.
    assert!(h._data.path().join("state/core/p-bye.json").is_file());
    let r = h.call::<PluginLoad>(name()).await.unwrap();
    assert!(r.plugins[0].loaded);
    assert!(h.tool_names().contains(&"p_tool".to_owned()));
    h.call::<PluginReload>(name()).await.unwrap();
    assert!(h.tool_names().contains(&"p_tool".to_owned()));

    for (method, name, want) in [
        (PluginLoad::METHOD, "nope", "no plugin nope"),
        (PluginUnload::METHOD, "ui_only", "has no core.lua"),
    ] {
        let e = h
            .core
            .handle(method, Some(json!({ "name": name })))
            .await
            .unwrap_err();
        assert!(e.message.contains(want), "{e:?}");
    }
}

#[tokio::test]
async fn shutdown_errors_are_warnings_and_hooks_can_be_added_later() {
    let mut h = Harness::with_lua(
        r#"bone.on_shutdown(function() error("cleanup failed") end)
           bone.tool.register { name = "late", run = function()
             if not added then
               added = true
               bone.hook("tool_result", function(ev) return { output = ev.output .. " (seen)" } end)
             end
             return "result"
           end }"#,
        vec![
            calls(&[("c1", "late", json!({}))]),
            calls(&[("c2", "late", json!({}))]),
            text("done"),
        ],
    )
    .await;
    h.start("go").await;
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    // Registered while the first call ran, the hook already sees its result.
    assert_eq!(tool_result(&t[2]).0, "result (seen)");
    assert_eq!(tool_result(&t[4]).0, "result (seen)");

    let r = h.core.reload().await.unwrap();
    assert!(
        r.warnings.iter().any(|w| w.contains("cleanup failed")),
        "{:?}",
        r.warnings
    );
}

#[tokio::test]
async fn questions_survive_a_reload_and_ids_stay_unique() {
    let mut h = Harness::with_lua(
        r#"bone.tool.register { name = "ask", run = function() return tostring(bone.ask({ q = "?" })) end }"#,
        vec![
            calls(&[("c1", "ask", json!({}))]),
            text("one"),
            calls(&[("c2", "ask", json!({}))]),
            text("two"),
        ],
    )
    .await;
    h.start("first").await;
    let q1 = h.until::<AskRequested>().await;
    h.core.reload().await.unwrap();
    // The old configuration still answers its own question.
    h.call::<AskRespond>(AskRespondParams {
        ask_id: q1.ask_id,
        answer: json!("yes"),
    })
    .await
    .unwrap();
    h.until::<TurnFinished>().await;
    h.start("second").await;
    let q2 = h.until::<AskRequested>().await;
    assert!(q2.ask_id > q1.ask_id);
    h.call::<AskRespond>(AskRespondParams {
        ask_id: q2.ask_id,
        answer: json!("again"),
    })
    .await
    .unwrap();
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    assert_eq!(tool_result(&t[2]).0, "yes");
    assert_eq!(tool_result(&t[6]).0, "again");
}

// ---- more hook points and session writes ------------------------------------

/// Wait until `path` exists and return its text.
async fn file_text(path: &std::path::Path) -> String {
    for _ in 0..200 {
        if let Ok(t) = std::fs::read_to_string(path) {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{} never appeared", path.display());
}

#[tokio::test]
async fn system_and_context_hooks_shape_each_request() {
    let mut h = Harness::with_lua(
        r#"
        bone.hook("system", function(ev) return { prompt = ev.prompt .. "\nB" } end)
        bone.hook("system", function(ev) return { prompt = ev.prompt .. "\nA" } end, { priority = 5 })
        -- Only the latest user message reaches the model.
        bone.hook("context", function(ev)
          local keep = { ev.messages[1] }
          keep[2] = ev.messages[#ev.messages]
          return { messages = keep }
        end)
        bone.hook("message", function(ev) seen_usage = ev.usage and ev.usage.output_tokens end)
        "#,
        vec![text("one"), text("two")],
    )
    .await;
    h.start("first").await;
    h.until::<TurnFinished>().await;
    h.start("second").await;
    h.until::<TurnFinished>().await;
    let systems = h.provider.systems.lock().unwrap().clone();
    assert!(systems[0].ends_with("\nA\nB"), "{:?}", systems[0]);
    let seen = h.provider.seen.lock().unwrap().clone();
    assert_eq!(
        seen[1],
        vec![ChatMessage::User {
            content: "second".into()
        }]
    );
    // The stored transcript is untouched.
    assert_eq!(h.transcript().await.len(), 4);
}

#[tokio::test]
async fn request_error_hooks_can_retry() {
    let mut h = Harness::with_lua(
        r#"
        tries = {}
        bone.hook("request_error", function(ev)
          tries[#tries + 1] = ev.attempt .. ":" .. ev.error
          if ev.attempt < 3 then return { retry = 1 } end
        end)
        bone.tool.register { name = "tries", run = function() return table.concat(tries, ",") end }
        "#,
        vec![
            Step::Fail("overloaded".into()),
            Step::Fail("still overloaded".into()),
            calls(&[("c1", "tries", json!({}))]),
            Step::Fail("a".into()),
            Step::Fail("b".into()),
            Step::Fail("c".into()),
        ],
    )
    .await;
    h.start("go").await;
    let finished = h.until::<TurnFinished>().await;
    // The third failure of the second call is not retried.
    assert_eq!(
        finished.outcome,
        TurnOutcome::Failed {
            message: "c".into()
        }
    );
    let t = h.transcript().await;
    assert_eq!(tool_result(&t[2]).0, "1:overloaded,2:still overloaded");
}

#[tokio::test]
async fn stream_and_session_start_hooks_watch_without_blocking() {
    let mut h = Harness::with_lua(
        r#"
        local text = ""
        bone.hook("stream", function(ev)
          text = text .. ev.text
          if text == "Hello there" then bone.state.save("streamed", { text = text }) end
        end)
        bone.hook("session_start", function(ev)
          bone.state.save("started", { new = ev.new, cwd = ev.cwd })
        end)
        "#,
        vec![text("Hello there")],
    )
    .await;
    let started = file_text(&h._data.path().join("state/core/started.json")).await;
    assert!(started.contains("\"new\": true"), "{started}");
    h.start("hi").await;
    h.until::<TurnFinished>().await;
    let streamed = file_text(&h._data.path().join("state/core/streamed.json")).await;
    assert!(streamed.contains("Hello there"), "{streamed}");
}

#[tokio::test]
async fn hooks_can_add_to_and_compact_the_transcript() {
    let mut h = Harness::with_lua(
        r#"
        bone.hook("context", function(ev)
          if not injected then
            injected = true
            bone.session.append(ev.session_id, { role = "user", content = "remember: be brief" })
          end
        end)
        bone.tool.register { name = "poke", run = function(_, ctx)
          local ok, err = pcall(bone.session.append, ctx.session_id, { role = "user", content = "x" })
          return tostring(ok) .. ": " .. tostring(err)
        end }
        bone.hook("turn_end", function(ev)
          local before = #bone.session.messages(ev.session_id)
          bone.session.compact(ev.session_id, {
            { role = "user", content = "summary of " .. before .. " messages" },
          })
        end)
        "#,
        vec![calls(&[("c1", "poke", json!({}))]), text("done"), text("again")],
    )
    .await;
    let first = h.start("go").await;
    // The injection, then the compaction (turn_end runs before turn/finished).
    assert_eq!(h.until::<SessionUpdated>().await.reason, "append");
    assert_eq!(h.until::<SessionUpdated>().await.reason, "compact");
    h.until::<TurnFinished>().await;
    // Mid tool calls the transcript cannot change.
    let seen = h.provider.seen.lock().unwrap().clone();
    let poke = seen[1].iter().find_map(|m| match m {
        ChatMessage::Tool { content, .. } => Some(content.clone()),
        _ => None,
    });
    assert!(
        poke.as_deref().is_some_and(|p| p.starts_with("false: ")
            && p.contains("a running turn's transcript can only change between model calls")),
        "{seen:#?}"
    );
    // The injected message reached the model on the next call.
    assert!(seen[1].contains(&ChatMessage::User {
        content: "remember: be brief".into()
    }));
    // go, injected, assistant(poke), tool, done = 5 before compaction.
    let t = h.transcript().await;
    assert_eq!(
        t,
        vec![ChatMessage::User {
            content: "summary of 5 messages".into()
        }]
    );
    // Turn ids keep counting, and the checkpoint survives a restart.
    let second = h.start("next").await;
    assert!(second > first);
    h.until::<TurnFinished>().await;
    let store = crate::session::SessionStore::new(h._data.path());
    let reloaded = store.get(&h.session_id).unwrap();
    let s = reloaded.lock().unwrap();
    assert_eq!(s.messages.len(), 1);
    assert_eq!(
        s.messages[0],
        ChatMessage::User {
            content: "summary of 3 messages".into()
        }
    );
}

// ---- model calls ------------------------------------------------------------

#[tokio::test]
async fn lua_tools_call_the_model() {
    let mut h = Harness::with_lua(
        r#"
        bone.tool.register { name = "ask_model", run = function()
          local parts = {}
          local r = assert(bone.model.complete({
            prompt = "say hi", system = "be brief",
            on_delta = function(d) parts[#parts + 1] = d.text end,
          }))
          return r.content .. "|" .. table.concat(parts) .. "|" .. #bone.model.list()
        end }
        "#,
        vec![
            calls(&[("c1", "ask_model", json!({}))]),
            text("inner answer"),
            text("outer"),
        ],
    )
    .await;
    h.start("go").await;
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    assert_eq!(tool_result(&t[2]).0, "inner answer|inner answer|1");
    assert_eq!(
        h.provider.seen.lock().unwrap()[1],
        vec![ChatMessage::User {
            content: "say hi".into()
        }]
    );
    assert_eq!(h.provider.systems.lock().unwrap()[1], "be brief");
}

#[tokio::test]
async fn nested_model_calls_stop_at_the_limit() {
    let mut h = Harness::with_lua(
        r#"
        bone.provider.register("loop", { complete = function(req)
          return assert(bone.model.complete({ provider = "loopy", prompt = "again" }))
        end })
        bone.config.providers.loopy = { type = "loop", model = "l" }
        bone.config.provider = "x"
        bone.tool.register { name = "spin", run = function()
          local r, err = bone.model.complete({ provider = "loopy", prompt = "go" })
          return tostring(r) .. " " .. tostring(err)
        end }
        "#,
        vec![calls(&[("c1", "spin", json!({}))]), text("done")],
    )
    .await;
    h.start("go").await;
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    assert!(
        tool_result(&t[2]).0.contains("nested more than 4 deep"),
        "{}",
        tool_result(&t[2]).0
    );
}

#[tokio::test]
async fn cancelling_a_turn_stops_its_model_calls() {
    let mut h = Harness::with_lua(
        r#"bone.tool.register { name = "slow", run = function()
             local r, err = bone.model.complete({ prompt = "think" })
             return tostring(err)
           end }"#,
        vec![
            calls(&[("c1", "slow", json!({}))]),
            Step::Hang("partial".into()),
        ],
    )
    .await;
    h.start("go").await;
    h.until::<ToolStarted>().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.cancel().await;
    let finished = tokio::time::timeout(Duration::from_secs(3), h.until::<TurnFinished>())
        .await
        .expect("the turn ends promptly");
    assert_eq!(finished.outcome, TurnOutcome::Cancelled);
}

#[tokio::test]
async fn clients_call_the_model_over_the_protocol() {
    let mut h = Harness::with_lua(
        "",
        vec![text("streamed answer"), Step::Hang("never ends".into())],
    )
    .await;
    let list = h.call::<ModelList>(Empty {}).await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].current && list[0].name == "x");
    let req = h
        .call::<ModelComplete>(ModelCompleteParams {
            provider: None,
            messages: vec![ChatMessage::User {
                content: "hi".into(),
            }],
            tools: vec![],
            options: Value::Null,
            stream: true,
        })
        .await
        .unwrap();
    let delta = h.until::<ModelDeltaEvent>().await;
    assert_eq!(
        (delta.request_id, delta.text.as_str()),
        (req.request_id, "streamed answer")
    );
    let done = h.until::<ModelCompleted>().await;
    assert_eq!(done.request_id, req.request_id);
    assert!(matches!(
        done.message,
        Some(ChatMessage::Assistant { ref content, .. }) if content == "streamed answer"
    ));

    // A call that hangs can be cancelled.
    let req = h
        .call::<ModelComplete>(ModelCompleteParams {
            provider: None,
            messages: vec![ChatMessage::User {
                content: "again".into(),
            }],
            tools: vec![],
            options: Value::Null,
            stream: false,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.call::<ModelCancel>(req).await.unwrap();
    let done = h.until::<ModelCompleted>().await;
    assert_eq!(done.error.as_deref(), Some("cancelled"));
    // An unknown provider is an error result, not a hang.
    h.call::<ModelComplete>(ModelCompleteParams {
        provider: Some("nope".into()),
        messages: vec![],
        tools: vec![],
        options: Value::Null,
        stream: false,
    })
    .await
    .unwrap();
    let done = h.until::<ModelCompleted>().await;
    assert!(done.error.unwrap().contains("no provider \"nope\""));
}

// ---- MCP ----------------------------------------------------------------------

#[tokio::test]
async fn mcp_tools_reach_the_model_and_survive_reloads() {
    let fixtures = tempfile::tempdir().unwrap();
    let script = fixtures.path().join("server.sh");
    std::fs::write(&script, crate::mcp::tests::BASH_SERVER).unwrap();
    let starts = fixtures.path().join("starts");
    let mcp_json = fixtures.path().join("mcp.json");
    std::fs::write(
        &mcp_json,
        json!({ "mcpServers": {
            "fromfile": { "command": "bash", "args": [script.to_string_lossy()] },
            "off": { "command": "nope", "disabled": true },
        } })
        .to_string(),
    )
    .unwrap();
    let mcp = |extra: &str| {
        format!(
            r#"
            bone.mcp.add("sh", {{ command = "bash", args = {{ "{script}"{extra} }}, env = {{ STARTS = "{starts}" }} }})
            bone.mcp.load("{json}")
            bone.hook("tool_call", function(ev)
              if ev.mcp then seen = ev.mcp.server .. "/" .. ev.mcp.tool end
            end)
            bone.tool.register {{ name = "probe", run = function()
              local r = bone.mcp.call("sh", "hello", {{}})
              return seen .. " " .. r.text .. " " .. #bone.mcp.list()
            end }}
            "#,
            script = script.display(),
            starts = starts.display(),
            json = mcp_json.display(),
        )
    };
    let mut h = Harness::with_lua(
        &mcp(""),
        vec![
            calls(&[("c1", "sh_hello", json!({}))]),
            calls(&[("c2", "probe", json!({}))]),
            text("done"),
        ],
    )
    .await;
    h.start("go").await;
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    assert_eq!(tool_result(&t[2]).0, "hi from bash", "{t:#?}");
    assert_eq!(tool_result(&t[4]).0, "sh/hello hi from bash 2");
    assert!(h.provider.tools.lock().unwrap()[0].contains(&"sh_hello".to_owned()));

    let list = h.call::<McpList>(Empty {}).await.unwrap();
    let names: Vec<(&str, &str)> = list
        .iter()
        .map(|s| (s.name.as_str(), s.state.as_str()))
        .collect();
    assert_eq!(names, [("fromfile", "ready"), ("sh", "ready")]);

    // Reloading with the same server keeps its process; changing it restarts it.
    let started = || std::fs::read_to_string(&starts).unwrap().lines().count();
    assert_eq!(started(), 1);
    h.write_core_lua(&(mcp("") + "\n-- something else changed"));
    h.core.reload().await.unwrap();
    assert_eq!(started(), 1);
    h.write_core_lua(&mcp(r#", "--again""#));
    h.core.reload().await.unwrap();
    h.core.mcp().ensure_ready(Duration::from_secs(10)).await;
    assert_eq!(started(), 2);
    // And removing it stops it.
    h.write_core_lua("");
    h.core.reload().await.unwrap();
    assert!(h.call::<McpList>(Empty {}).await.unwrap().is_empty());
}

// ---- skills and prompt templates (plugins), lua/call ----------------------------

impl Harness {
    /// `lua/call` for this session.
    async fn lua_call(
        &self,
        name: &str,
        args: Value,
        cwd: Option<&str>,
    ) -> Result<Value, RpcError> {
        self.call::<LuaCall>(LuaCallParams {
            name: name.into(),
            args,
            session_id: Some(self.session_id.clone()),
            cwd: cwd.map(str::to_owned),
        })
        .await
    }
}

#[tokio::test]
async fn nothing_changes_without_skills() {
    let mut h = Harness::with_lua("", vec![text("plain")]).await;
    h.start("hi").await;
    h.until::<TurnFinished>().await;
    assert!(!h.provider.systems.lock().unwrap()[0].contains("## Skills"));
    assert!(!h.provider.tools.lock().unwrap()[0].contains(&"skill".to_owned()));
    // The core has no skills or templates of its own.
    let e = h
        .lua_call("skills.list", json!({}), None)
        .await
        .unwrap_err();
    assert!(e.message.contains("no function skills.list"), "{e:?}");
}

#[tokio::test]
async fn clients_call_functions_core_lua_registered() {
    let h = Harness::with_lua(
        r#"bone.rpc.register("demo.echo", function(args, ctx)
             bone.sleep(1)
             return { said = args.word, session = ctx.session_id, cwd = ctx.cwd }
           end)
           bone.rpc.register("demo.fail", function() error("nope") end)"#,
        vec![],
    )
    .await;
    let r = h
        .lua_call("demo.echo", json!({ "word": "hi" }), Some("/proj"))
        .await
        .unwrap();
    assert_eq!(
        r,
        json!({ "said": "hi", "session": h.session_id, "cwd": "/proj" })
    );
    let e = h.lua_call("demo.fail", json!({}), None).await.unwrap_err();
    assert!(e.message.contains("nope"), "{e:?}");
}

#[tokio::test]
async fn registered_skills_are_listed_and_loaded_on_demand() {
    let skills = tempfile::tempdir().unwrap();
    let release = skills.path().join("release");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(
        release.join("SKILL.md"),
        "---\nname: release\ndescription: \"Cut a release: changelog, tag, publish\"\n---\nStep 1: update CHANGELOG.md\n",
    )
    .unwrap();
    std::fs::write(release.join("checklist.md"), "- [ ] tag").unwrap();
    std::fs::create_dir_all(skills.path().join("not-a-skill")).unwrap();
    let mut h = Harness::with_plugins(
        &["skills"],
        &format!(
            r#"
            assert(bone.skill.load_dir("{dir}") == 1)
            bone.skill.register {{ name = "style", description = "house style", content = "Use tabs." }}
            bone.skill.register {{ name = "hidden", description = "never shown", content = "x",
              enabled = function(ctx) return false end }}
            "#,
            dir = skills.path().display()
        ),
        vec![
            calls(&[
                ("c1", "skill", json!({ "name": "release" })),
                ("c2", "skill", json!({ "name": "hidden" })),
                ("c3", "skill", json!({ "name": "style" })),
            ]),
            text("done"),
        ],
    )
    .await;
    h.start("ship it").await;
    h.until::<TurnFinished>().await;
    let system = h.provider.systems.lock().unwrap()[0].clone();
    assert!(
        system.contains("## Skills")
            && system.contains("- release: Cut a release: changelog, tag, publish")
            && system.contains("- style: house style")
            && !system.contains("hidden"),
        "{system}"
    );
    assert!(h.provider.tools.lock().unwrap()[0].contains(&"skill".to_owned()));
    let t = h.transcript().await;
    let release_text = tool_result(&t[2]).0;
    assert!(
        release_text.starts_with("# Skill: release")
            && release_text.contains("Step 1: update CHANGELOG.md")
            && !release_text.contains("description:")
            && release_text.contains("release/checklist.md"),
        "{release_text}"
    );
    assert_eq!(tool_result(&t[3]), ("no skill named hidden", true));
    assert!(tool_result(&t[4]).0.contains("Use tabs."));

    let list = h.lua_call("skills.list", json!({}), None).await.unwrap();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["hidden", "release", "style"]);
    assert!(
        list[1]["path"]
            .as_str()
            .unwrap()
            .ends_with("release/SKILL.md")
    );
}

#[tokio::test]
async fn the_skills_prompt_can_be_left_to_plugins() {
    let mut h = Harness::with_plugins(
        &["skills"],
        r#"bone.skill.register { name = "a", description = "b", content = "c" }
           bone.config.skills.prompt = false"#,
        vec![text("ok")],
    )
    .await;
    h.start("hi").await;
    h.until::<TurnFinished>().await;
    assert!(!h.provider.systems.lock().unwrap()[0].contains("## Skills"));
    assert!(h.provider.tools.lock().unwrap()[0].contains(&"skill".to_owned()));
}

#[tokio::test]
async fn templates_expand_with_arguments() {
    let prompts = tempfile::tempdir().unwrap();
    std::fs::write(
        prompts.path().join("review.md"),
        "---\ndescription: Review a file\nargs: path, focus\n---\nReview {{path}} for {{focus}}.\n",
    )
    .unwrap();
    std::fs::write(prompts.path().join("notes.txt"), "not a template").unwrap();
    let h = Harness::with_plugins(
        &["templates"],
        &format!(
            r#"
            bone.template.load_dir("{dir}")
            bone.template.register {{ name = "fix", description = "fix an issue", args = {{ "issue" }},
              body = "Fix issue $1 ($@) in 100% of cases" }}
            bone.template.register {{ name = "status", body = function(args, ctx)
              local r = bone.system("printf clean")
              return "git says " .. r.stdout .. " for " .. (args.argv[1] or "?") .. " in " .. tostring(ctx.cwd)
            end }}
            "#,
            dir = prompts.path().display()
        ),
        vec![],
    )
    .await;
    let list = h.lua_call("templates.list", json!({}), None).await.unwrap();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["fix", "review", "status"]);
    assert_eq!(list[1]["args"], json!(["path", "focus"]));
    assert_eq!(list[1]["description"], "Review a file");
    let expand = |name: &str, args: &str| json!({ "name": name, "args": args });
    let call = |args: Value| h.lua_call("templates.expand", args, Some("/proj"));
    assert_eq!(
        call(expand("review", r#"src/main.rs "error handling""#))
            .await
            .unwrap(),
        "Review src/main.rs for error handling.\n"
    );
    assert_eq!(
        call(expand("fix", "42 crash")).await.unwrap(),
        "Fix issue 42 (42 crash) in 100% of cases"
    );
    assert_eq!(
        call(expand("status", "main")).await.unwrap(),
        "git says clean for main in /proj"
    );
    let e = call(expand("nope", "")).await.unwrap_err();
    assert!(e.message.contains("no template named nope"), "{e:?}");
}

// ---- the example plugins (core halves) -------------------------------------------

#[tokio::test]
async fn example_compact_plugin_summarizes_older_turns() {
    let mut h = Harness::with_plugins(
        &["compact"],
        "bone.config.compact.keep = 1",
        vec![text("a1"), text("a2"), text("SUMMARY")],
    )
    .await;
    for q in ["q1", "q2"] {
        h.start(q).await;
        h.until::<TurnFinished>().await;
    }
    let r = h.lua_call("compact", json!({}), None).await.unwrap();
    assert_eq!(
        r,
        "compacted 2 messages into a summary; kept the last 1 turns"
    );
    let t = h.transcript().await;
    assert_eq!(
        t[0],
        ChatMessage::User {
            content: "Summary of the earlier conversation:\n\nSUMMARY".into()
        }
    );
    assert_eq!(t.len(), 3, "{t:#?}");
    let seen = h.provider.seen.lock().unwrap().clone();
    assert!(matches!(&seen[2][0], ChatMessage::User { content } if content.contains("USER: q1")));
}

#[tokio::test]
async fn example_compact_plugin_retries_when_the_context_is_full() {
    let mut h = Harness::with_plugins(
        &["compact"],
        "bone.config.compact.keep = 1",
        vec![
            text("a1"),
            Step::Fail("HTTP 400: maximum context length exceeded".into()),
            text("SUM"),
            text("a2"),
        ],
    )
    .await;
    h.start("q1").await;
    h.until::<TurnFinished>().await;
    h.start("q2").await;
    let finished = h.until::<TurnFinished>().await;
    assert_eq!(finished.outcome, TurnOutcome::Completed);
    let t = h.transcript().await;
    assert_eq!(t.len(), 3, "{t:#?}");
    assert!(matches!(&t[0], ChatMessage::User { content } if content.ends_with("SUM")));
}

#[tokio::test]
async fn example_retry_plugin_retries_passing_errors_only() {
    let mut h = Harness::with_plugins(
        &["retry"],
        "bone.config.retry.delay = 1",
        vec![
            Step::Fail("HTTP 503 Service Unavailable".into()),
            Step::Fail("overloaded".into()),
            text("ok"),
            Step::Fail("HTTP 400: bad request".into()),
        ],
    )
    .await;
    h.start("one").await;
    assert_eq!(
        h.until::<TurnFinished>().await.outcome,
        TurnOutcome::Completed
    );
    h.start("two").await;
    assert_eq!(
        h.until::<TurnFinished>().await.outcome,
        TurnOutcome::Failed {
            message: "HTTP 400: bad request".into()
        }
    );
}

#[tokio::test]
async fn example_folder_plugins_load_mcp_skills_and_templates() {
    let files = tempfile::tempdir().unwrap();
    let script = files.path().join("server.sh");
    std::fs::write(&script, crate::mcp::tests::BASH_SERVER).unwrap();
    std::fs::write(
        files.path().join("mcp.json"),
        json!({ "mcpServers": { "sh": { "command": "bash", "args": [script.to_string_lossy()] } } })
            .to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(files.path().join("skills/release")).unwrap();
    std::fs::write(
        files.path().join("skills/release/SKILL.md"),
        "---\ndescription: cut a release\n---\nsteps",
    )
    .unwrap();
    std::fs::create_dir_all(files.path().join("prompts")).unwrap();
    std::fs::write(files.path().join("prompts/review.md"), "Review $1").unwrap();
    let h = Harness::with_plugins(
        &["mcp", "skills", "templates"],
        &format!(
            r#"
            bone.config.mcp_files = {{ "{dir}/mcp.json" }}
            bone.config.mcp_options = {{ lazy = true }}
            bone.config.skill_dirs = {{ "{dir}/skills" }}
            bone.config.template_dirs = {{ "{dir}/prompts" }}
            "#,
            dir = files.path().display()
        ),
        vec![],
    )
    .await;
    let servers = h.call::<McpList>(Empty {}).await.unwrap();
    assert_eq!(
        (servers[0].name.as_str(), servers[0].state.as_str()),
        ("sh", "idle")
    );
    let skills = h.lua_call("skills.list", json!({}), None).await.unwrap();
    assert_eq!(skills[0]["name"], "release");
    let templates = h.lua_call("templates.list", json!({}), None).await.unwrap();
    assert_eq!(templates[0]["name"], "review");
}

// ---- parallel tool calls and output limits --------------------------------------

const PARALLEL_TOOLS: &str = r#"
log = {}
local function slow(name, parallel)
  bone.tool.register { name = name, parallel = parallel, run = function()
    log[#log + 1] = "start " .. name
    bone.sleep(100)
    log[#log + 1] = "end " .. name
    return name .. " done"
  end }
end
slow("p1", true)
slow("p2", true)
slow("p3", true)
slow("w", false)
bone.tool.register { name = "log", run = function() return table.concat(log, ", ") end }
"#;

fn parallel_steps() -> Vec<Step> {
    vec![
        calls(&[
            ("c1", "p1", json!({})),
            ("c2", "p2", json!({})),
            ("c3", "w", json!({})),
            ("c4", "p3", json!({})),
        ]),
        calls(&[("c5", "log", json!({}))]),
        text("done"),
    ]
}

#[tokio::test]
async fn read_only_calls_run_together_in_order() {
    let mut h = Harness::with_lua(PARALLEL_TOOLS, parallel_steps()).await;
    h.start("go").await;
    // Both parallel calls start before either finishes.
    let mut seen = Vec::new();
    while seen.len() < 3 {
        let e = h.next().await;
        if e.method == ToolStarted::METHOD {
            seen.push(format!(
                "start {}",
                e.params["call"]["name"].as_str().unwrap()
            ));
        } else if e.method == ToolFinished::METHOD {
            seen.push(format!("end {}", e.params["call_id"].as_str().unwrap()));
        }
    }
    assert_eq!(seen[..2], ["start p1", "start p2"]);
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    let results: Vec<&str> = t[2..6].iter().map(|m| tool_result(m).0).collect();
    assert_eq!(results, ["p1 done", "p2 done", "w done", "p3 done"]);
    let log = tool_result(&t[7]).0;
    assert!(log.starts_with("start p1, start p2, "), "{log}");
    assert!(log.ends_with("start w, end w, start p3, end p3"), "{log}");
}

#[tokio::test]
async fn parallel_calls_can_be_switched_off() {
    let mut h = Harness::with_lua(
        &format!("{PARALLEL_TOOLS}\nbone.config.parallel_tools = false"),
        parallel_steps(),
    )
    .await;
    h.start("go").await;
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    assert_eq!(
        tool_result(&t[7]).0,
        "start p1, end p1, start p2, end p2, start w, end w, start p3, end p3"
    );
}

#[tokio::test]
async fn example_output_cap_plugin_cuts_long_results() {
    let mut h = Harness::with_plugins(
        &["output-cap"],
        r#"bone.config.output_cap.max = 200
           bone.tool.register { name = "big", run = function() return "A" .. string.rep("x", 5000) .. "Z" end }"#,
        vec![calls(&[("c1", "big", json!({}))]), text("ok")],
    )
    .await;
    h.start("go").await;
    let finished = h.until::<ToolFinished>().await;
    h.until::<TurnFinished>().await;
    let out = tool_result(&h.transcript().await[2]).0.to_owned();
    assert!(out.len() < 400, "{}", out.len());
    assert!(out.starts_with('A') && out.ends_with('Z') && out.contains("bytes omitted"));
    // Clients see what the model sees.
    assert_eq!(finished.output, out);
}

// ---- session management --------------------------------------------------------

#[tokio::test]
async fn sessions_can_be_renamed_forked_and_deleted() {
    let mut h = Harness::new(vec![
        text("a1"),
        text("a2"),
        text("in fork"),
        Step::Hang("…".into()),
    ])
    .await;
    for q in ["q1", "q2"] {
        h.start(q).await;
        h.until::<TurnFinished>().await;
    }
    let sid = h.session_id.clone();

    let info = h
        .call::<SessionRename>(SessionRenameParams {
            session_id: sid.clone(),
            title: "  Typo hunt ".into(),
        })
        .await
        .unwrap();
    assert_eq!(info.title.as_deref(), Some("Typo hunt"));
    assert_eq!(h.until::<SessionUpdated>().await.reason, "rename");
    // The title survives a restart, and shows in the list.
    let store = crate::session::SessionStore::new(h._data.path());
    assert_eq!(store.list().unwrap()[0].title.as_deref(), Some("Typo hunt"));

    // Fork from before the second turn: the first exchange only.
    let fork = h
        .call::<SessionFork>(SessionForkParams {
            session_id: sid.clone(),
            before_turn: Some(2),
        })
        .await
        .unwrap();
    assert_eq!(fork.parent.as_deref(), Some(sid.as_str()));
    assert_ne!(fork.session_id, sid);
    let msgs = |id: &str| SessionRef {
        session_id: id.to_owned(),
    };
    let forked = h
        .call::<SessionMessages>(msgs(&fork.session_id))
        .await
        .unwrap();
    assert_eq!(forked.messages.len(), 2);
    assert_eq!(forked.info.title.as_deref(), Some("q1"));
    // It goes its own way; the original is untouched.
    let turn = h
        .call::<TurnStart>(TurnStartParams {
            session_id: fork.session_id.clone(),
            text: "other way".into(),
        })
        .await
        .unwrap()
        .turn_id;
    assert_eq!(turn, 2);
    h.until::<TurnFinished>().await;
    assert_eq!(
        h.call::<SessionMessages>(msgs(&fork.session_id))
            .await
            .unwrap()
            .messages
            .len(),
        4
    );
    assert_eq!(h.transcript().await.len(), 4);
    let whole = h
        .call::<SessionFork>(SessionForkParams {
            session_id: sid.clone(),
            before_turn: None,
        })
        .await
        .unwrap();
    assert_eq!(
        h.call::<SessionMessages>(msgs(&whole.session_id))
            .await
            .unwrap()
            .messages
            .len(),
        4
    );

    // Not while a turn runs; then it is gone.
    h.start("busy").await;
    h.until::<MessageDelta>().await;
    let e = h.call::<SessionDelete>(msgs(&sid)).await.unwrap_err();
    assert_eq!(e.code, RpcError::BUSY);
    h.cancel().await;
    h.until::<TurnFinished>().await;
    h.call::<SessionDelete>(msgs(&sid)).await.unwrap();
    assert_eq!(h.until::<SessionDeleted>().await.session_id, sid);
    assert!(h.call::<SessionMessages>(msgs(&sid)).await.is_err());
    assert!(
        !h._data
            .path()
            .join(format!("sessions/{sid}.jsonl"))
            .exists()
    );
    let left: Vec<String> = h
        .call::<SessionList>(Empty {})
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.session_id)
        .collect();
    assert!(!left.contains(&sid) && left.len() == 2);
}

// ---- steering a running turn ------------------------------------------------------

fn later(ms: u64, s: &str) -> Step {
    Step::Delay(
        ms,
        Completion {
            content: s.into(),
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn steered_messages_join_before_the_next_model_call() {
    let mut h = Harness::with_lua(
        r#"bone.tool.register { name = "slow", run = function() bone.sleep(200) return "slow done" end }"#,
        vec![calls(&[("c1", "slow", json!({}))]), text("done")],
    )
    .await;
    h.start("go").await;
    h.until::<ToolStarted>().await;
    h.call::<TurnSteer>(TurnSteerParams {
        session_id: h.session_id.clone(),
        text: "also check the README".into(),
    })
    .await
    .unwrap();
    let steered = h.until::<TurnSteered>().await;
    assert_eq!(steered.text, "also check the README");
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    assert_eq!(
        t[3],
        ChatMessage::User {
            content: "also check the README".into()
        }
    );
    // The model saw it on its next call.
    assert_eq!(h.provider.seen.lock().unwrap()[1].last(), Some(&t[3]));
}

#[tokio::test]
async fn a_message_steered_during_the_last_answer_still_gets_one() {
    let mut h = Harness::new(vec![later(200, "first answer"), text("second answer")]).await;
    h.start("go").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    h.call::<TurnSteer>(TurnSteerParams {
        session_id: h.session_id.clone(),
        text: "and one more thing".into(),
    })
    .await
    .unwrap();
    h.until::<TurnFinished>().await;
    let t = h.transcript().await;
    let texts: Vec<String> = t
        .iter()
        .map(|m| match m {
            ChatMessage::User { content } => format!("user: {content}"),
            ChatMessage::Assistant { content, .. } => format!("assistant: {content}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        [
            "user: go",
            "assistant: first answer",
            "user: and one more thing",
            "assistant: second answer"
        ]
    );
    // With no turn running there is nothing to steer.
    let e = h
        .call::<TurnSteer>(TurnSteerParams {
            session_id: h.session_id.clone(),
            text: "late".into(),
        })
        .await
        .unwrap_err();
    assert!(e.message.contains("no turn is running"), "{e:?}");
}

// ---- the message queue ------------------------------------------------------------

impl Harness {
    async fn queue(&self, text: &str, mode: QueueMode) -> QueueAddResult {
        self.call::<QueueAdd>(QueueAddParams {
            session_id: self.session_id.clone(),
            text: text.into(),
            mode,
        })
        .await
        .unwrap()
    }

    fn queued_texts(&self) -> Vec<String> {
        let s = self.core.inner.sessions.get(&self.session_id).unwrap();
        let s = s.lock().unwrap();
        s.queue.iter().map(|q| q.text.clone()).collect()
    }

    /// The text of each turn that starts, until `n` have finished.
    async fn turns(&mut self, n: usize) -> Vec<(String, TurnOutcome)> {
        let mut out = Vec::new();
        let mut text = String::new();
        while out.len() < n {
            let e = self.next().await;
            if let Some(Ok(p)) = e.parse_as::<TurnStarted>() {
                text = p.text;
            } else if let Some(Ok(p)) = e.parse_as::<TurnFinished>() {
                out.push((text.clone(), p.outcome));
            }
        }
        out
    }
}

#[tokio::test]
async fn queued_messages_run_one_turn_each_in_order() {
    let mut h = Harness::new(vec![
        later(200, "a1"),
        text("a2"),
        Step::Fail("boom".into()),
        text("a4"),
    ])
    .await;
    // Idle: it starts a turn right away.
    let first = h.queue("one", QueueMode::Next).await;
    assert!(first.turn_id.is_some() && first.id.is_none());
    // Busy: queued, in order. A failed turn does not stop the queue.
    let second = h.queue("two", QueueMode::Next).await;
    assert!(second.id.is_some());
    h.queue("three", QueueMode::Next).await;
    h.queue("four", QueueMode::Next).await;
    let changed = h.until::<QueueChanged>().await;
    assert_eq!(changed.items.len(), 1);
    let turns = h.turns(4).await;
    let texts: Vec<&str> = turns.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(texts, ["one", "two", "three", "four"]);
    assert!(matches!(turns[2].1, TurnOutcome::Failed { .. }));
    assert!(h.queued_texts().is_empty());
}

#[tokio::test]
async fn a_cancelled_turn_pauses_the_queue_until_resumed() {
    let mut h = Harness::new(vec![Step::Hang("…".into()), text("after")]).await;
    h.queue("first", QueueMode::Next).await;
    h.until::<MessageDelta>().await;
    // A steer message that never joins the turn waits for the next one.
    h.queue("steered", QueueMode::Steer).await;
    h.queue("second", QueueMode::Next).await;
    h.cancel().await;
    h.until::<TurnFinished>().await;
    let state = h
        .call::<SessionMessages>(SessionRef {
            session_id: h.session_id.clone(),
        })
        .await
        .unwrap();
    assert!(state.queue_paused);
    assert_eq!(
        state
            .queue
            .iter()
            .map(|q| (q.text.as_str(), q.mode))
            .collect::<Vec<_>>(),
        [("steered", QueueMode::Next), ("second", QueueMode::Next)]
    );
    // Edit it while paused.
    let second = state.queue[1].id;
    let steered = state.queue[0].id;
    let r = |id| QueueItemRef {
        session_id: h.session_id.clone(),
        id,
    };
    h.call::<QueueUpdate>(QueueUpdateParams {
        session_id: h.session_id.clone(),
        id: second,
        text: Some("second, edited".into()),
        mode: None,
    })
    .await
    .unwrap();
    h.call::<QueueMove>(QueueMoveParams {
        session_id: h.session_id.clone(),
        id: second,
        to: 0,
    })
    .await
    .unwrap();
    h.call::<QueueRemove>(r(steered)).await.unwrap();
    assert!(h.call::<QueueRemove>(r(99)).await.is_err());
    assert_eq!(h.queued_texts(), ["second, edited"]);
    // Resume: it runs.
    h.call::<QueueResume>(SessionRef {
        session_id: h.session_id.clone(),
    })
    .await
    .unwrap();
    let turns = h.turns(1).await;
    assert_eq!(turns[0].0, "second, edited");
    assert!(h.queued_texts().is_empty());
}

#[tokio::test]
async fn the_queue_survives_a_restart() {
    let mut h = Harness::new(vec![Step::Hang("…".into())]).await;
    h.queue("running", QueueMode::Next).await;
    h.until::<MessageDelta>().await;
    h.queue("later", QueueMode::Next).await;
    let path = h
        ._data
        .path()
        .join(format!("sessions/{}.queue.json", h.session_id));
    assert!(path.is_file());
    // A fresh core finds it, waiting.
    let store = crate::session::SessionStore::new(h._data.path());
    {
        let s = store.get(&h.session_id).unwrap();
        let s = s.lock().unwrap();
        assert_eq!(s.queue[0].text, "later");
        assert!(s.queue_paused);
    }
    h.call::<QueueClear>(SessionRef {
        session_id: h.session_id.clone(),
    })
    .await
    .unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn queue_add_hooks_rewrite_or_refuse() {
    let mut h = Harness::with_lua(
        r#"bone.hook("queue_add", function(ev)
             if ev.text == "no" then return { deny = "not that" } end
             return { text = ev.text .. "!", mode = "next" }
           end)"#,
        vec![Step::Hang("…".into())],
    )
    .await;
    h.queue("go", QueueMode::Steer).await;
    h.until::<TurnStarted>().await;
    h.queue("more", QueueMode::Steer).await;
    let state = h
        .call::<SessionMessages>(SessionRef {
            session_id: h.session_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        (state.queue[0].text.as_str(), state.queue[0].mode),
        ("more!", QueueMode::Next)
    );
    let e = h
        .call::<QueueAdd>(QueueAddParams {
            session_id: h.session_id.clone(),
            text: "no".into(),
            mode: QueueMode::Next,
        })
        .await
        .unwrap_err();
    assert!(e.message.contains("not that"));
}

#[tokio::test]
async fn core_lua_can_queue_messages() {
    let mut h = Harness::with_lua(
        r#"bone.hook("turn_end", function(ev)
             if not queued then
               queued = true
               bone.queue.add(ev.session_id, "follow up", "next")
             end
           end)
           bone.tool.register { name = "peek", run = function(_, ctx)
             return #bone.queue.list(ctx.session_id)
           end }"#,
        vec![
            text("first"),
            calls(&[("c1", "peek", json!({}))]),
            text("second"),
        ],
    )
    .await;
    h.start("go").await;
    let turns = h.turns(2).await;
    assert_eq!(turns[1].0, "follow up");
    assert_eq!(tool_result(&h.transcript().await[4]).0, "0");
}
