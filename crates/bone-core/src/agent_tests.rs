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
