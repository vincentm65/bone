//! Drive the app with keys and server events against a fake server, and check
//! what it requests and draws.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bone_client::{Client, EventStream};
use bone_proto::methods::*;
use bone_proto::types::{ChatMessage, DeltaKind, ToolCall, TurnOutcome, Usage};
use bone_proto::{Message, Notification, RpcError, transport};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::app::{App, AppEvent};
use crate::keymap::Context;
use crate::keys;
use crate::render;

type Log = Arc<Mutex<Vec<(String, Value)>>>;

struct Harness {
    app: App,
    rx: mpsc::UnboundedReceiver<AppEvent>,
    events: EventStream,
    log: Log,
    push: mpsc::Sender<Message>,
    fail: Arc<Mutex<Option<String>>>,
    /// The config dir, when the harness made one.
    _dir: Option<tempfile::TempDir>,
}

/// Install `examples/plugins/style` into a config dir.
fn install_style(config: &std::path::Path) {
    install_example(config, "style");
}

/// Install `examples/plugins/<name>` into a config dir.
fn install_example(config: &std::path::Path, name: &str) {
    fn copy(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(e.file_name()));
            } else {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }
    let from = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/plugins")
        .join(name);
    copy(&from, &config.join("plugins").join(name));
}

fn info(id: &str, title: Option<&str>) -> Value {
    json!({ "session_id": id, "cwd": "/work", "created_at": 0, "title": title })
}

/// Canned replies by method.
fn reply(method: &str, params: &Value) -> Result<Value, RpcError> {
    // The fake core has one core plugin, `corepart`; any other name is a
    // TUI-only plugin to it.
    if method == "session/fork" {
        let mut i = info("s-fork", Some("forked"));
        i["parent"] = params["session_id"].clone();
        return Ok(i);
    }
    if method == "session/rename" {
        return Ok(info(
            params["session_id"].as_str().unwrap(),
            params["title"].as_str(),
        ));
    }
    if let "plugin/load" | "plugin/unload" | "plugin/reload" = method {
        let name = params["name"].as_str().unwrap_or_default();
        if name != "corepart" {
            return Err(RpcError::new(
                RpcError::INVALID_PARAMS,
                format!("plugin {name} has no core.lua"),
            ));
        }
        return Ok(
            json!({ "plugins": [{ "name": "corepart", "core": true, "loaded": method != "plugin/unload" }] }),
        );
    }
    Ok(match method {
        "mcp/list" => json!([
            { "name": "gh", "state": "ready", "tools": ["gh_search"] },
            { "name": "db", "state": "failed", "error": "cannot run db-mcp", "tools": [] },
        ]),
        // The example plugins' core halves, as a core would answer for them.
        "lua/call" => match params["name"].as_str().unwrap_or_default() {
            "skills.list" => json!([{ "name": "release", "description": "cut a release" }]),
            "templates.list" => {
                json!([{ "name": "review", "description": "review", "args": ["path"] }])
            }
            "templates.expand" => json!(format!(
                "Review {}",
                params["args"]["args"].as_str().unwrap_or("")
            )),
            "compact" => json!("compacted"),
            other => {
                return Err(RpcError::new(
                    RpcError::INVALID_PARAMS,
                    format!("no function {other} registered with bone.rpc"),
                ));
            }
        },
        "model/complete" => json!({ "request_id": 41 }),
        "queue/add" => json!({ "id": 1 }),
        "plugin/list" => json!([{ "name": "corepart", "core": true, "loaded": true }]),
        "core/reload" => json!({ "plugins": [], "warnings": ["data_dir changed"] }),
        "initialize" => {
            json!({ "protocol_version": 0, "server_name": "fake", "server_version": "0" })
        }
        "session/create" => info("s-new", None),
        "turn/start" => json!({ "turn_id": 1 }),
        "session/list" => json!([info("s-two", Some("second")), info("s-one", Some("first"))]),
        "health/check" => {
            json!([{ "name": "provider", "status": "ok", "message": "m at http://x" }])
        }
        "session/messages" => json!({
            "info": info(params["session_id"].as_str().unwrap(), Some("loaded")),
            "messages": [
                { "role": "user", "content": "old question" },
                { "role": "assistant", "content": "old answer" },
            ],
        }),
        _ => Value::Null,
    })
}

impl Harness {
    /// With the style plugin installed (most tests check how things look).
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        install_style(dir.path());
        let mut h = Self::build(Some(dir.path().to_owned())).await;
        h.app.load_user_config();
        h.settle().await;
        h._dir = Some(dir);
        h
    }

    /// No config at all: the blank slate.
    async fn blank() -> Self {
        Self::build(None).await
    }

    /// With a config dir containing `tui.lua` (and the style plugin),
    /// loaded like at startup.
    async fn with_config(tui_lua: &str) -> (Self, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        install_style(dir.path());
        std::fs::write(dir.path().join("tui.lua"), tui_lua).unwrap();
        let mut h = Self::build(Some(dir.path().to_owned())).await;
        h.app.load_user_config();
        h.settle().await;
        (h, dir)
    }

    async fn build(config_dir: Option<std::path::PathBuf>) -> Self {
        let (client_end, server_end) = transport::in_process();
        let log: Log = Arc::default();
        let fail: Arc<Mutex<Option<String>>> = Arc::default();
        let push = server_end.tx.clone();
        let (log2, fail2) = (log.clone(), fail.clone());
        tokio::spawn(async move {
            let mut rx = server_end.rx;
            while let Some(msg) = rx.recv().await {
                let Message::Request { id, method, params } = msg else {
                    continue;
                };
                let params = params.unwrap_or(Value::Null);
                log2.lock().unwrap().push((method.clone(), params.clone()));
                let failing = fail2.lock().unwrap().as_deref() == Some(method.as_str());
                let result = if failing {
                    Err(RpcError::new(1, "nope"))
                } else {
                    reply(&method, &params)
                };
                let _ = server_end.tx.send(Message::Response { id, result }).await;
            }
        });
        let (client, events) = Client::new(client_end);
        client.initialize("test").await.unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let app = App::new(Arc::new(client), tx, "/work".into(), config_dir);
        let mut h = Harness {
            app,
            rx,
            events,
            log,
            push,
            fail,
            _dir: None,
        };
        h.screen(80, 24);
        h
    }

    /// Process replies and server events until nothing arrives for a bit.
    async fn settle(&mut self) {
        loop {
            tokio::select! {
                Some(ev) = self.rx.recv() => self.app.apply(ev),
                Some(ev) = self.events.recv() => self.app.handle_server(ev),
                _ = tokio::time::sleep(Duration::from_millis(30)) => return,
            }
        }
    }

    /// Type text; `{name}` presses a key, e.g. `"hi{alt+enter}there{enter}"`.
    async fn input(&mut self, s: &str) {
        let mut rest = s;
        while let Some(c) = rest.chars().next() {
            if c == '{'
                && let Some(end) = rest.find('}').filter(|&e| e > 1)
            {
                self.app.handle_key(keys::parse(&rest[1..end]).unwrap());
                rest = &rest[end + 1..];
                continue;
            }
            self.app.handle_key(keys::Key::char(c));
            rest = &rest[c.len_utf8()..];
        }
        self.settle().await;
    }

    async fn emit<N: Notification>(&mut self, params: N::Params) {
        let msg = Message::Notification {
            method: N::METHOD.into(),
            params: Some(serde_json::to_value(params).unwrap()),
        };
        self.push.send(msg).await.unwrap();
        self.settle().await;
    }

    fn requests(&self, method: &str) -> Vec<Value> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }

    fn screen(&mut self, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render::draw(f, &mut self.app)).unwrap();
        let buf = term.backend().buffer();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn prompt(&self) -> String {
        self.app.prompt.text()
    }

    fn message(&self) -> String {
        self.app
            .message
            .as_ref()
            .map(|m| m.0.clone())
            .unwrap_or_default()
    }

    async fn lua(&mut self, code: &str) -> String {
        self.app.exec_lua(code);
        self.settle().await;
        self.message()
    }
}

fn started(session: &str, text: &str) -> TurnStartedParams {
    TurnStartedParams {
        session_id: session.into(),
        turn_id: 1,
        text: text.into(),
    }
}

fn finished(session: &str, outcome: TurnOutcome) -> TurnFinishedParams {
    TurnFinishedParams {
        session_id: session.into(),
        turn_id: 1,
        outcome,
    }
}

fn tool_calls(session: &str, calls: Vec<ToolCall>) -> MessageCompletedParams {
    MessageCompletedParams {
        session_id: session.into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: calls,
        },
        usage: None,
    }
}

#[tokio::test]
async fn first_message_creates_a_session_and_streams_the_reply() {
    let mut h = Harness::new().await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("Message bone…") && screen.contains("New session."),
        "{screen}"
    );
    h.input("hello{alt+enter}there{enter}").await;

    assert_eq!(
        h.requests("session/create"),
        vec![json!({ "cwd": "/work" })]
    );
    assert_eq!(
        h.requests("turn/start"),
        vec![json!({ "session_id": "s-new", "text": "hello\nthere" })]
    );
    assert_eq!(h.prompt(), "");

    h.emit::<TurnStarted>(started("s-new", "hello\nthere"))
        .await;
    h.emit::<MessageDelta>(MessageDeltaParams {
        session_id: "s-new".into(),
        turn_id: 1,
        kind: DeltaKind::Text,
        text: "Hi! ".into(),
    })
    .await;
    let screen = h.screen(80, 24);
    assert!(screen.contains("› hello\n  there"), "{screen}");
    assert!(screen.contains("Hi!▍"), "{screen}");
    assert!(screen.contains("working 0s  ctrl+c to cancel"), "{screen}");

    h.emit::<MessageCompleted>(MessageCompletedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: "Hi! Done.".into(),
            reasoning: String::new(),
            tool_calls: vec![],
        },
        usage: Some(Usage {
            input_tokens: 1500,
            output_tokens: 20,
        }),
    })
    .await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    let screen = h.screen(80, 24);
    assert!(screen.contains("  Hi! Done."), "{screen}");
    assert!(
        screen.contains(" hello") && screen.contains("1.5k in · 20 out  │  /work"),
        "{screen}"
    );
    assert!(!screen.contains("working"), "{screen}");

    // Events for other sessions are ignored.
    h.emit::<TurnStarted>(started("other", "x")).await;
    assert!(!h.screen(80, 24).contains("› x"));
}

#[tokio::test]
async fn failed_turn_start_gives_the_text_back() {
    let mut h = Harness::new().await;
    *h.fail.lock().unwrap() = Some("session/create".into());
    h.input("keep me{enter}").await;
    assert_eq!(h.prompt(), "keep me");
    assert!(h.screen(80, 24).contains("cannot create session"));
}

fn approval(id: u64, tool: &str, args: Value) -> AskRequestedParams {
    AskRequestedParams {
        ask_id: id,
        session_id: Some("s-new".into()),
        question: json!({ "kind": "approval", "title": format!("Allow {tool}?"), "tool": tool, "arguments": args }),
    }
}

#[tokio::test]
async fn approve_plugin_popup_answers_questions() {
    let dir = tempfile::tempdir().unwrap();
    install_example(dir.path(), "approve");
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    h.settle().await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.input("typing").await;
    h.emit::<AskRequested>(approval(7, "shell", json!({"command": "rm -rf build"})))
        .await;
    assert_eq!(h.app.context(), Context::Popup);
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("Allow shell?")
            && screen.contains("$ rm -rf build")
            && screen.contains("a always"),
        "{screen}"
    );

    // Keys right after the popup appears are swallowed, and `y` never
    // reaches the prompt.
    h.input("y").await;
    assert!(h.requests("ask/respond").is_empty());
    tokio::time::sleep(Duration::from_millis(350)).await;
    h.input("y").await;
    assert_eq!(
        h.requests("ask/respond"),
        vec![json!({ "ask_id": 7, "answer": "allow" })]
    );
    assert_eq!(h.app.context(), Context::Main);
    assert_eq!(h.prompt(), "typing");

    // Another client answering (or a cancel) closes the popup.
    h.emit::<AskRequested>(approval(
        8,
        "write_file",
        json!({"path": "a", "content": "x"}),
    ))
    .await;
    assert!(h.screen(80, 24).contains("a (1 lines)"));
    h.emit::<AskResolved>(AskResolvedParams {
        ask_id: 8,
        answer: Value::Null,
    })
    .await;
    assert_eq!(h.app.context(), Context::Main);

    // Questions of other kinds are not this plugin's business.
    h.emit::<AskRequested>(AskRequestedParams {
        ask_id: 9,
        session_id: None,
        question: json!({ "kind": "other" }),
    })
    .await;
    assert_eq!(h.app.context(), Context::Main);
}

#[tokio::test]
async fn lua_popups_take_the_keyboard() {
    let mut h = Harness::new().await;
    h.lua(
        "pid = bone.ui.popup({ lines = function(ctx) \
           return bone.ui.box({ 'width ' .. ctx.width, { { 'red', 'DiffDelete' } } }, { title = 'Pick' }) end, \
         keys = { r = function() picked = 'red'; bone.ui.close(pid) end } })",
    )
    .await;
    assert_eq!(h.app.context(), Context::Popup);
    let screen = h.screen(60, 16);
    assert!(
        screen.contains("╭ Pick ")
            && screen.contains("│ width 60 │")
            && screen.contains("│ red      │"),
        "{screen}"
    );
    // Unmapped keys do nothing; mapped ones run.
    h.input("x").await;
    assert_eq!(h.prompt(), "");
    h.input("r").await;
    assert_eq!(h.app.context(), Context::Main);
    assert_eq!(h.lua("=picked").await, "\"red\"");

    // Lua draws every cell: no border unless it makes one, placed where it
    // asks. on_key sees the keys `keys` doesn't name.
    h.lua("pid = bone.ui.popup({ lines = { 'TL' }, row = 0, col = 0, on_key = function(k) last = k; return true end })")
        .await;
    let screen = h.screen(60, 16);
    assert!(screen.starts_with("TL"), "{screen}");
    h.input("x").await;
    assert_eq!(h.lua("=last").await, "\"x\"");
    assert_eq!(h.prompt(), "");
    h.lua("bone.ui.close(pid)").await;
    h.lua("pid = bone.ui.popup({ lines = { 'BR' }, row = -1, col = -1 })")
        .await;
    let screen = h.screen(60, 16);
    assert!(screen.lines().last().unwrap().ends_with("BR"), "{screen}");
    h.lua("bone.ui.close(pid)").await;
    // ctrl+c in a popup still cancels a running turn.
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.lua("bone.ui.popup({ lines = { 'busy' }, keys = {} })")
        .await;
    h.input("{ctrl+c}").await;
    assert_eq!(h.requests("turn/cancel").len(), 1);
}

#[tokio::test]
async fn ctrl_c_cancels_clears_then_quits() {
    let mut h = Harness::new().await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.input("draft{ctrl+c}").await;
    assert_eq!(
        h.requests("turn/cancel"),
        vec![json!({ "session_id": "s-new" })]
    );
    assert_eq!(h.prompt(), "draft");
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Cancelled))
        .await;
    assert!(h.screen(80, 24).contains("! cancelled"));
    h.input("{ctrl+c}").await;
    assert_eq!(h.prompt(), "");
    h.input("{ctrl+c}").await;
    assert!(h.screen(80, 24).contains("Press ctrl+c again to quit"));
    assert!(h.app.quit.is_none());
    h.input("{ctrl+c}").await;
    assert_eq!(h.app.quit, Some(None));
}

#[tokio::test]
async fn slash_commands_suggest_complete_and_run() {
    let mut h = Harness::new().await;
    h.input("/se").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("/sessions")
            && screen.contains("/set")
            && screen.contains("pick a session"),
        "{screen}"
    );
    // Tab completes the selected suggestion; enter runs it.
    h.input("{down}{tab}").await;
    assert_eq!(h.prompt(), "/set ");
    h.input("tool_preview_lines=2 noshow_reasoning{enter}")
        .await;
    assert_eq!(h.app.options.tool_preview_lines, 2);
    assert!(!h.app.options.show_reasoning);
    assert_eq!(h.prompt(), "");

    // Enter on a partial name runs the selected suggestion.
    h.input("/hel{enter}").await;
    assert!(
        h.message().contains("/sessions") && h.message().contains("ctrl+r sessions"),
        "{}",
        h.message()
    );

    // Unknown commands stay in the prompt with an error.
    h.input("/bogus{enter}").await;
    assert!(h.message().contains("Unknown command /bogus"));
    assert_eq!(h.prompt(), "/bogus");
    h.input("{ctrl+u}").await;

    // Esc hides suggestions; `//` and paths are messages.
    h.input("/s{esc}").await;
    assert!(!h.screen(80, 24).contains("/sessions"));
    h.input("{ctrl+u}//not a command{enter}").await;
    assert_eq!(h.requests("turn/start")[0]["text"], "/not a command");
    h.emit::<TurnStarted>(started("s-new", "/not a command"))
        .await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    h.input("/etc/hosts has a typo{enter}").await;
    assert_eq!(h.requests("turn/start")[1]["text"], "/etc/hosts has a typo");

    h.input("/quit{enter}").await;
    assert_eq!(h.app.quit, Some(None));
}

#[tokio::test]
async fn session_picker_filters_and_opens() {
    let mut h = Harness::new().await;
    h.input("{ctrl+r}").await;
    // The picker is a Lua window (bone.ui.select), so the popup context.
    assert_eq!(h.app.context(), Context::Popup);
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("second") && screen.contains("first"),
        "{screen}"
    );
    // Typing filters.
    h.input("firs").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("Sessions  firs") && screen.contains("first") && !screen.contains("second"),
        "{screen}"
    );
    h.input("{enter}").await;
    assert_eq!(h.app.context(), Context::Main);
    assert_eq!(
        h.requests("session/messages"),
        vec![json!({ "session_id": "s-one" })]
    );
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("› old question") && screen.contains("  old answer"),
        "{screen}"
    );

    // Prompts go to the opened session; /new starts another.
    h.input("next{enter}").await;
    assert_eq!(h.requests("turn/start")[0]["session_id"], "s-one");
    h.emit::<TurnStarted>(started("s-one", "next")).await;
    h.emit::<TurnFinished>(finished("s-one", TurnOutcome::Completed))
        .await;
    h.input("/new{enter}").await;
    assert!(h.screen(80, 24).contains("New session."));
    // Esc closes the picker without opening anything.
    h.input("{ctrl+r}{esc}").await;
    assert_eq!(h.app.context(), Context::Main);
}

#[tokio::test]
async fn prompt_grows_and_history_recalls() {
    let mut h = Harness::new().await;
    h.input("one{ctrl+j}two{ctrl+j}three").await;
    let screen = h.screen(40, 12);
    assert!(screen.contains("› one\n  two\n  three"), "{screen}");
    h.input("{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "one")).await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    h.input("{up}").await;
    assert_eq!(h.prompt(), "one\ntwo\nthree");
    h.input("{down}").await;
    assert_eq!(h.prompt(), "");
    h.app.paste("pasted\ntext");
    assert_eq!(h.prompt(), "pasted\ntext");
    h.input("{ctrl+a}{ctrl+k}").await;
    assert_eq!(h.prompt(), "pasted\n");
}

#[tokio::test]
async fn tui_lua_keys_commands_options_and_events() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        bone.o.tool_preview_lines = 3
        bone.keymap.set("f2", "/sessions")
        bone.keymap.set("ctrl+g", function() bone.cmd("hello world") end)
        bone.keymap.set("a", "dismiss", { context = "popup" })
        bone.keymap.del("ctrl+n")
        bone.cmd.create("hello", function(c) bone.notify("hi " .. c.args) end, { desc = "greet" })
        bone.on("turn/started", function(ev) seen_text = ev.text end)
        bone.on("submit", function(ev)
          if ev.text == "skip" then return false end
          return ev.text:upper()
        end)
        bone.on("ready", function() bone.notify("ready! " .. bone.o.tool_preview_lines) end)
        "#,
    )
    .await;
    assert_eq!(h.message(), "ready! 3");
    assert_eq!(h.app.options.tool_preview_lines, 3);
    assert_eq!(h.lua("=bone.api_version").await, "1");
    assert_eq!(h.lua("=bone.api_info().side").await, "\"tui\"");
    assert_eq!(h.lua("=bone.has_capability('tui.keymaps')").await, "true");
    assert_eq!(
        h.lua("=bone.has_capability('tui.no_such_feature')").await,
        "false"
    );

    // A Lua keymap calling a user command (nested Lua calls).
    h.input("{ctrl+g}").await;
    assert_eq!(h.message(), "hi world");
    h.input("/hel").await;
    assert!(h.screen(80, 24).contains("greet"));
    // Suggestions sort by name: /hello, then /help.
    h.input("{down}{enter}").await;
    assert!(
        h.message().contains("/hello") && h.message().contains("greet"),
        "{}",
        h.message()
    );

    // Deleted default (ctrl+n is just ignored) and a new one.
    h.input("{ctrl+n}{f2}").await;
    assert_eq!(h.requests("session/list").len(), 1);
    h.input("{esc}").await;

    // submit hook: rewrite, then cancel.
    h.input("hello{enter}").await;
    assert_eq!(h.requests("turn/start")[0]["text"], "HELLO");
    h.emit::<TurnStarted>(started("s-new", "HELLO")).await;
    assert_eq!(h.lua("=seen_text").await, "\"HELLO\"");
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    h.input("skip{enter}").await;
    assert_eq!(h.requests("turn/start").len(), 1);
    assert_eq!(h.prompt(), "");
}

#[tokio::test]
async fn lua_phase2_commands_options_and_local_events() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        events = {}
        local function mark(name)
          return function(ev) events[#events + 1] = name end
        end
        bone.on("prompt", function(ev) prompt_event = ev; mark("prompt")(ev) end)
        bone.on("focus_changed", function(ev) focus_event = ev; mark("focus")(ev) end)
        bone.on("ui/resize", function(ev) resize_event = ev; mark("resize")(ev) end)
        bone.on("key/pressed", function(ev) key_event = ev; mark("key")(ev) end)
        bone.on("text/pasted", function(ev) paste_event = ev; mark("paste")(ev) end)
        bone.on("popup/opened", function(ev) opened_event = ev; mark("opened")(ev) end)
        bone.on("popup/updated", function(ev) updated_event = ev; mark("updated")(ev) end)
        bone.on("panel/closed", function(ev) closed_event = ev; mark("closed")(ev) end)
        changes = {}
        bone.o.define("plugin_limit", 2, {
          type = "integer",
          desc = "limit for the plugin",
          on_change = function(new, old) changes[#changes + 1] = new .. ":" .. old end,
        })
        bone.cmd.create("greet", function(ctx)
          seen = {
            raw = ctx.args,
            command = ctx.command,
            argv = #ctx.argv,
            name = ctx.arguments.name,
            count = ctx.arguments.count,
            enabled = ctx.arguments.enabled,
          }
        end, {
          aliases = { "greetme" },
          args = {
            { name = "name", required = true },
            { name = "count", type = "integer", default = 2 },
            { name = "enabled", type = "boolean", default = false },
          },
          complete = function(ctx)
            return { { value = ctx.token .. "x", desc = "example" } }
          end,
        })
        local noop = function() end
        collision_alias = not pcall(function()
          bone.cmd.create("other", noop, { aliases = { "greetme" } })
        end)
        collision_name = not pcall(function()
          bone.cmd.create("greetme", noop)
        end)
        bone.cmd.create("gone", noop, { aliases = { "goneby" } })
        local deleted_once = pcall(function() bone.cmd.del("goneby") end)
        deleted_alias = deleted_once and not pcall(function() bone.cmd.del("goneby") end)
        bad_default = not pcall(function()
          bone.o.define("bad_default", "x", { type = "integer" })
        end)
        wrong_type = not pcall(function() bone.o.plugin_limit = "x" end)
        old_changes, new_changes = 0, 0
        bone.o.define("drop_probe", 1, {
          type = "integer",
          on_change = function() old_changes = old_changes + 1 end,
        })
        bone.o.del("drop_probe")
        bone.o.define("drop_probe", 1, {
          type = "integer",
          on_change = function() new_changes = new_changes + 1 end,
        })
        "#,
    )
    .await;

    assert_eq!(
        h.lua("=bone.has_capability('tui.local_events')").await,
        "true"
    );
    assert_eq!(
        h.lua("=bone.has_capability('tui.command_specs')").await,
        "true"
    );
    assert_eq!(
        h.lua("=bone.has_capability('tui.dynamic_options')").await,
        "true"
    );
    assert_eq!(h.lua("=bone.o.plugin_limit").await, "2");
    h.lua("bone.o.plugin_limit = 5").await;
    assert_eq!(h.lua("=changes[1]").await, "\"5:2\"");
    h.lua("bone.o.plugin_limit = 5").await;
    assert_eq!(h.lua("=#changes").await, "1");
    assert_eq!(
        h.lua("=bone.o.info('plugin_limit').type").await,
        "\"integer\""
    );
    assert!(
        h.lua("=table.concat(bone.o.names(), ',')")
            .await
            .contains("plugin_limit")
    );
    assert_eq!(
        h.lua(
            "=collision_alias and collision_name and deleted_alias and bad_default and wrong_type"
        )
        .await,
        "true"
    );
    h.lua("bone.o.drop_probe = 2").await;
    assert_eq!(h.lua("=old_changes .. ':' .. new_changes").await, "\"0:1\"");
    h.input("/set plugin_limit=8{enter}").await;
    assert_eq!(h.lua("=changes[2]").await, "\"8:5\"");
    h.input("/set plugin_limit?{enter}").await;
    assert_eq!(h.message(), "plugin_limit=8");

    assert!(h.app.user_commands.contains_key("greet"), "{}", h.message());
    assert!(h.app.user_commands["greet"].completion.is_some());
    assert_eq!(h.app.user_commands["greet"].aliases, vec!["greetme"]);
    h.input("/greetme b{tab}").await;
    assert_eq!(h.prompt(), "/greetme bx ", "message={}", h.message());
    h.input("{ctrl+u}/greetme bob 7 true{enter}").await;
    assert_eq!(
        h.lua("=seen.raw .. '|' .. seen.command .. '|' .. seen.name .. '|' .. seen.count .. '|' .. tostring(seen.enabled)").await,
        "\"bob 7 true|greet|bob|7|true\"",
    );
    h.input("/help{enter}").await;
    assert!(h.message().contains("aliases: greetme"), "{}", h.message());

    h.lua("bone.keymap.context('phase2'); bone.keymap.focus('phase2')")
        .await;
    h.input("z").await;
    h.app.paste("pasted");
    h.app.resize(100, 30);
    h.lua(
        "w = bone.ui.win{ lines = {'one'} }; bone.ui.update(w, { width = 12 }); bone.ui.close(w)",
    )
    .await;
    assert!(h.lua("=prompt_event.text").await.contains("pasted"));
    assert_eq!(h.lua("=key_event.context").await, "\"phase2\"");
    assert_eq!(h.prompt(), "zpasted");
    assert_eq!(h.lua("=resize_event.width").await, "100");
    assert_eq!(h.lua("=opened_event.kind").await, "\"popup\"");
    assert_eq!(h.lua("=updated_event.id == opened_event.id").await, "true");
    assert_eq!(h.lua("=closed_event.id == opened_event.id").await, "true");
    let events = h.lua("=table.concat(events, ',')").await;
    for name in [
        "prompt", "focus", "key", "paste", "resize", "opened", "updated", "closed",
    ] {
        assert!(events.contains(name), "{name} missing from {events}");
    }
}

#[tokio::test]
async fn lua_api_and_errors() {
    let mut h = Harness::new().await;
    assert_eq!(h.lua("=1 + 1").await, "2");
    h.input("/lua =bone.api.prompt_get(){enter}").await;
    assert_eq!(h.message(), "\"\"");

    // Errors show one line; /messages has the full traceback.
    h.input("/lua error('boom'){enter}").await;
    assert!(
        h.message().starts_with("lua: ") && h.message().contains("boom"),
        "{}",
        h.message()
    );
    h.input("/messages{enter}").await;
    assert!(h.message().contains("stack traceback"), "{}", h.message());

    // Requests from Lua with a callback.
    let r = h.lua("bone.request('session/list', {}, function(r) bone.notify(#r .. ' ' .. r[1].session_id) end)").await;
    assert_eq!(r, "2 s-two");
    *h.fail.lock().unwrap() = Some("nope".into());
    let r = h
        .lua(
            "bone.request('nope', {}, function(r, err) bone.notify(tostring(r) .. ' ' .. err) end)",
        )
        .await;
    assert!(r.starts_with("nil "), "{r}");

    // Prompt and keys from Lua.
    h.lua("bone.api.prompt_set('from lua'); bone.press('ctrl+e'); bone.press('!')")
        .await;
    assert_eq!(h.prompt(), "from lua!");

    // The default commands are Lua too, so they can be replaced.
    h.lua("bone.cmd.create('new', function() bone.notify('my new') end); bone.prompt.set('')")
        .await;
    h.input("/new{enter}").await;
    assert_eq!(h.message(), "my new");

    // Bad arguments are Lua errors, not crashes.
    for (code, want) in [
        ("bone.keymap.set('hyper+x', 'submit')", "unknown modifier"),
        (
            "bone.keymap.set('x', 'submit', { context = 'nope context' })",
            "invalid context",
        ),
        ("bone.keymap.set('x', 'nope')", "unknown action"),
        ("bone.o.nope = 1", "unknown option"),
        ("bone.cmd.create('Upper', function() end)", "lowercase"),
    ] {
        let msg = h.lua(code).await;
        assert!(msg.contains(want), "{code}: {msg}");
    }
}

#[tokio::test]
async fn runtime_defaults_can_be_replaced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("runtime/tui")).unwrap();
    std::fs::write(
        dir.path().join("runtime/tui/defaults.lua"),
        r#"bone.keymap.set("enter", "submit")"#,
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    // Only enter is mapped: ctrl+r does nothing.
    h.input("{ctrl+r}").await;
    assert_eq!(h.app.context(), Context::Main);
    h.input("x{enter}").await;
    assert_eq!(h.requests("turn/start").len(), 1);
}

#[tokio::test]
async fn lua_statusline_divider_and_broken_ui() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        bone.ui.statusline = function(ctx) return { { "<" .. ctx.title .. ">", "Accent" }, "%=", tostring(ctx.popup) } end
        bone.ui.divider = function(ctx) return { "[", tostring(ctx.width), "]", { fill = "=" } } end
        "#,
    )
    .await;
    let screen = h.screen(40, 10);
    assert!(
        screen.contains("<[new session]>                      nil"),
        "{screen}"
    );
    assert!(screen.contains("[40]====="), "{screen}");
    h.input("{ctrl+r}").await;
    assert!(h.screen(40, 10).contains("popup"));
    h.input("{esc}").await;

    // A failing statusline is reported once and leaves its row blank.
    h.lua("bone.ui.statusline = function() error('bad bar') end")
        .await;
    let screen = h.screen(40, 10);
    assert!(!screen.contains("<[new session]>"), "{screen}");
    assert!(
        h.message().contains("bad bar") && h.message().contains("redefined"),
        "{}",
        h.message()
    );
}

#[tokio::test]
async fn default_ui_fits_narrow_screens() {
    let (mut h, _dir) = Harness::with_config("").await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.emit::<MessageCompleted>(MessageCompletedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: "ok".into(),
            reasoning: String::new(),
            tool_calls: vec![],
        },
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 2,
        }),
    })
    .await;
    let wide = h.screen(100, 12);
    assert!(wide.contains("10 in · 2 out  │  /work"), "{wide}");
    let narrow = h.screen(24, 12);
    assert!(
        narrow.contains("10 in · 2 out") && !narrow.contains("/work"),
        "{narrow}"
    );
}

#[tokio::test]
async fn lua_tool_views_highlights_and_colorschemes() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        bone.ui.tool_views.shell = function(ev)
          if not ev.done then return { title = "running " .. ev.arguments.command } end
          return { title = { { "ran ", "ToolName" }, { ev.arguments.command, "ToolPath" } }, lines = { "out: " .. ev.output } }
        end
        "#,
    )
    .await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = ToolCall {
        id: "c1".into(),
        name: "shell".into(),
        arguments: r#"{"command":"ls"}"#.into(),
    };
    h.emit::<MessageCompleted>(tool_calls("s-new", vec![call]))
        .await;
    assert!(h.screen(60, 14).contains("◌ running ls"));
    h.emit::<ToolFinished>(ToolFinishedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        call_id: "c1".into(),
        output: "a.txt".into(),
        is_error: false,
    })
    .await;
    let screen = h.screen(60, 14);
    assert!(screen.contains("    ran ls\n    ╰ out: a.txt"), "{screen}");

    assert_eq!(h.app.colors_name.as_deref(), Some("black"));
    h.lua("bone.hl.set('ToolPath', { fg = '#010203', bold = true })")
        .await;
    assert_eq!(h.lua("=bone.hl.get('ToolPath').fg").await, "\"#010203\"");
    h.input("/hi Mine fg=red underline{enter}").await;
    assert_eq!(h.lua("=bone.hl.get('Mine').underline").await, "true");
    h.input("/colorscheme ansi{enter}").await;
    assert_eq!(h.lua("=bone.hl.get('ToolPath').fg").await, "\"cyan\"");
    h.input("/theme nope{enter}").await;
    assert!(h.message().contains("no colorscheme named nope"));
}

#[tokio::test]
async fn plugins_load_with_modules_and_colors() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    // The shipped example plugin, plus a local one and a disabled one.
    let example =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/git");
    std::fs::create_dir_all(plugins.join("git")).unwrap();
    for f in ["core.lua", "tui.lua"] {
        std::fs::copy(example.join(f), plugins.join("git").join(f)).unwrap();
    }
    std::fs::create_dir_all(plugins.join("demo/lua/demo")).unwrap();
    std::fs::create_dir_all(plugins.join("demo/colors")).unwrap();
    std::fs::write(
        plugins.join("demo/lua/demo/init.lua"),
        "return { hi = function() return 'from module' end }",
    )
    .unwrap();
    std::fs::write(
        plugins.join("demo/colors/demo.lua"),
        "bone.hl.set('Normal', { fg = '#123456' })",
    )
    .unwrap();
    std::fs::write(
        plugins.join("demo/tui.lua"),
        "bone.cmd.create('demo', function() bone.notify(require('demo').hi()) end)",
    )
    .unwrap();
    std::fs::create_dir_all(plugins.join("_off")).unwrap();
    std::fs::write(plugins.join("_off/tui.lua"), "error('should not load')").unwrap();
    // The user's config runs after plugins and can use what they set up.
    std::fs::write(
        dir.path().join("tui.lua"),
        "bone.cmd.create('demo2', function() bone.cmd('demo') end)",
    )
    .unwrap();

    install_style(dir.path());
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    h.settle().await;
    assert!(h.app.user_commands.contains_key("git"));
    h.input("/demo2{enter}").await;
    assert_eq!(h.message(), "from module");
    assert_eq!(
        h.lua("=table.concat(bone.plugins, ',')").await,
        "\"demo,git,style\""
    );
    h.input("/colorscheme demo{enter}").await;
    assert_eq!(h.lua("=bone.hl.get('Normal').fg").await, "\"#123456\"");

    // The example plugin's tool view renders git_status output.
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = ToolCall {
        id: "g".into(),
        name: "git_status".into(),
        arguments: "{}".into(),
    };
    h.emit::<MessageCompleted>(tool_calls("s-new", vec![call]))
        .await;
    h.emit::<ToolFinished>(ToolFinishedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        call_id: "g".into(),
        output: "## main...origin/main\n M src/lib.rs\n?? notes.md\n".into(),
        is_error: false,
    })
    .await;
    let screen = h.screen(60, 14);
    assert!(
        screen
            .contains("    git status main...origin/main\n    │  M src/lib.rs\n    ╰ ?? notes.md"),
        "{screen}"
    );
}

#[tokio::test]
async fn resume_newest_session() {
    let mut h = Harness::new().await;
    h.app.resume(None);
    h.settle().await;
    assert_eq!(
        h.requests("session/messages"),
        vec![json!({ "session_id": "s-two" })]
    );
}

/// Render one tool call through the default Lua views.
async fn tool_rows(
    name: &str,
    args: Value,
    output: Option<(&str, bool)>,
    width: u16,
) -> Vec<String> {
    let mut h = Harness::new().await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = ToolCall {
        id: "c".into(),
        name: name.into(),
        arguments: args.to_string(),
    };
    h.emit::<MessageCompleted>(tool_calls("s-new", vec![call]))
        .await;
    if let Some((text, is_error)) = output {
        h.emit::<ToolFinished>(ToolFinishedParams {
            session_id: "s-new".into(),
            turn_id: 1,
            call_id: "c".into(),
            output: text.into(),
            is_error,
        })
        .await;
    }
    let screen = h.screen(width, 30);
    screen
        .lines()
        .skip(2)
        .take_while(|l| !l.starts_with("──") && !l.starts_with("─"))
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn default_tool_views() {
    let cmd = json!({"command": "git status"});
    assert_eq!(
        tool_rows("shell", cmd.clone(), None, 60).await,
        ["  ◌ $ git status"]
    );
    assert_eq!(
        tool_rows(
            "shell",
            cmd.clone(),
            Some(("M a\nM b\n[exit code: 0]", false)),
            60
        )
        .await,
        ["    $ git status", "    │ M a", "    ╰ M b"]
    );
    assert_eq!(
        tool_rows(
            "shell",
            cmd.clone(),
            Some(("(no output)\n[exit code: 0]", false)),
            60
        )
        .await,
        ["    $ git status"]
    );
    let long: String = (1..=9).map(|i| format!("l{i}\n")).collect::<String>() + "[exit code: 2]";
    assert_eq!(
        tool_rows("shell", cmd, Some((&long, false)), 60).await,
        [
            "    $ git status  exit 2",
            "    │ l1",
            "    │ ⋮ +6 lines",
            "    │ l8",
            "    ╰ l9"
        ]
    );
    assert_eq!(
        tool_rows("shell", json!({"command": "cat <<EOF\nx\nEOF"}), None, 60).await,
        ["  ◌ $ cat <<EOF (+2 lines)"]
    );
    assert_eq!(
        tool_rows(
            "shell",
            json!({"command": "sleep 9"}),
            Some(("[timed out after 1s; process killed]", true)),
            60
        )
        .await,
        [
            "  ✕ $ sleep 9",
            "    ╰ [timed out after 1s; process killed]"
        ]
    );
    assert_eq!(
        tool_rows(
            "read_file",
            json!({"path": "src/x.rs"}),
            Some(("     1\ta\n     2\tb\n", false)),
            60
        )
        .await,
        ["    read_file src/x.rs (2 lines)"]
    );
    let partial = "    10\ta\n    11\tb\n\n[showing lines 10-11 of 99; use offset to read more]\n";
    assert_eq!(
        tool_rows(
            "read_file",
            json!({"path": "x"}),
            Some((partial, false)),
            60
        )
        .await,
        ["    read_file x (lines 10–11 of 99)"]
    );
    assert_eq!(
        tool_rows(
            "write_file",
            json!({"path": "a", "content": "x\ny\n"}),
            Some(("Created a (4 bytes)", false)),
            60
        )
        .await,
        ["    write_file a (2 lines)"]
    );
    let edit = json!({"path": "a.rs", "old_string": "fn a() {\n    old();\n}", "new_string": "fn a() {\n    new();\n    more();\n}"});
    assert_eq!(
        tool_rows(
            "edit_file",
            edit,
            Some(("Replaced 1 occurrence in a.rs", false)),
            30
        )
        .await,
        [
            "    edit_file a.rs (+2 −1)",
            "    │ -     old();",
            "    │ +     new();",
            "    ╰ +     more();"
        ]
    );
    assert_eq!(
        tool_rows("word_count", json!({"text": "a b"}), Some(("2", false)), 60).await,
        ["    word_count {\"text\":\"a b\"}", "    ╰ 2"]
    );
}

#[tokio::test]
async fn views_are_lua_and_can_be_replaced() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        -- Reasoning out of the chat; a compact user line; no blank lines.
        bone.ui.views.reasoning = function(item, ctx)
          if ctx.region == "chat" then return nil end
          return { { { "∴ " .. item.text, "Reasoning" } } }
        end
        bone.ui.views.user = function(item, ctx) return { { { "you: " .. item.text, "UserMessage" } } } end
        bone.ui.views.assistant = function(item, ctx) return bone.ui.markdown(item.text, ctx.width, "") end
        -- ...and show the latest reasoning above the prompt while it streams.
        bone.ui.regions.above_prompt = {
          size = "auto", max = 2,
          render = function(ctx)
            local r = bone.chat.items({ kind = "reasoning", last = 1 })[1]
            if not r or not r.streaming then return nil end
            return bone.ui.render(r, ctx.width)
          end,
        }
        bone.ui.regions.right = { size = 12, render = function(ctx) return { "side " .. ctx.height, { { fill = ".", hl = "Dim" } } } end }
        bone.ui.regions.top = function(ctx) return { "== " .. (ctx.session and ctx.session.title or "?") .. " ==" } end
        "#,
    )
    .await;
    h.input("hello{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "hello")).await;
    h.emit::<MessageDelta>(MessageDeltaParams {
        session_id: "s-new".into(),
        turn_id: 1,
        kind: DeltaKind::Reasoning,
        text: "pondering".into(),
    })
    .await;
    let screen = h.screen(60, 14);
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(rows[0], "== hello ==", "{screen}");
    assert!(
        rows[1].starts_with("you: hello") && rows[1].ends_with("│side 9"),
        "{screen}"
    );
    assert!(rows[2].ends_with("│............"), "{screen}");
    assert!(
        !rows[1..12]
            .iter()
            .any(|r| r.contains("pondering") && !r.starts_with("∴")),
        "{screen}"
    );
    let above = rows
        .iter()
        .position(|r| r.starts_with("∴ pondering"))
        .expect(&screen);
    assert!(rows[above + 1].starts_with("› Message bone"), "{screen}");

    // Once the answer lands, the region disappears and the text shows.
    h.emit::<MessageCompleted>(MessageCompletedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: "# Done".into(),
            reasoning: "pondering".into(),
            tool_calls: vec![],
        },
        usage: None,
    })
    .await;
    let screen = h.screen(60, 14);
    assert!(
        !screen.contains("∴") && screen.contains("\nDone"),
        "{screen}"
    );

    // A broken view falls back to plain text, reported once.
    h.lua("bone.ui.views.notice = function() error('bad view') end")
        .await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Cancelled))
        .await;
    let screen = h.screen(60, 14);
    assert!(
        screen.contains("! cancelled") && h.message().contains("bad view"),
        "{screen}"
    );
}

#[tokio::test]
async fn text_helpers_and_markdown_data() {
    let mut h = Harness::new().await;
    let r = h
        .lua("=bone.text.wrap({ { 'aa bbb', 'A' }, ' cc' }, 8, { first = '> ', pad = 'P' })")
        .await;
    assert!(
        r.contains("\"> \"") && r.contains("\"aa bbb\"") && r.contains("\"cc\""),
        "{r}"
    );
    assert_eq!(
        h.lua("=#bone.text.wrap({ { 'aa bbb', 'A' }, ' cc' }, 8, { first = '> ', pad = 'P' })[2]")
            .await,
        "3"
    );
    assert_eq!(h.lua("=#bone.text.wrap('one two three', 7)").await, "2");
    assert_eq!(
        h.lua("=bone.text.clip({ { 'abcdef', 'X' } }, 4)[1][1]")
            .await,
        "\"abc…\""
    );
    assert_eq!(h.lua("=bone.text.width('日本')").await, "4");
    assert_eq!(
        h.lua("=bone.text.shell('ls -la')[3][2]").await,
        "\"ShellFlag\""
    );
    assert_eq!(
        h.lua("=bone.markdown.parse('# Hi\\n- **x**')[2].spans[1].bold")
            .await,
        "true"
    );
    assert_eq!(h.lua("=#bone.chat.items()").await, "0");
}

#[tokio::test]
async fn without_lua_views_rust_draws_plain_text() {
    let mut h = Harness::blank().await;
    h.input("hello **world**{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "hello **world**"))
        .await;
    h.emit::<MessageCompleted>(MessageCompletedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: "\n\n# Title\n- item\n".into(),
            reasoning: "hmm\n".into(),
            tool_calls: vec![],
        },
        usage: None,
    })
    .await;
    let screen = h.screen(40, 12);
    // One blank line between items, edges trimmed, reasoning hidden.
    assert!(
        screen.starts_with("> hello **world**\n\n# Title\n- item\n\n"),
        "{screen}"
    );
    // A Lua view brings reasoning back.
    h.lua("bone.ui.views.reasoning = function(item) return { '~ ' .. item.text } end")
        .await;
    assert!(
        h.screen(40, 12)
            .starts_with("> hello **world**\n~ hmm\n\n# Title"),
        "{}",
        h.screen(40, 12)
    );
}

#[tokio::test]
async fn blank_slate_draws_only_text() {
    let mut h = Harness::blank().await;
    assert!(h.screen(30, 6).trim().is_empty());
    h.input("hi{alt+enter}you").await;
    // The prompt sits at the bottom with no prefix, divider or statusline.
    assert_eq!(h.screen(30, 6), "\n\n\n\nhi\nyou");

    // A prefix and placeholder come from Lua.
    h.lua("bone.ui.prompt = { prefix = '> ', placeholder = { { 'say', 'Dim' } } }")
        .await;
    assert!(h.screen(30, 6).ends_with("> hi\n  you"));
    h.input("{ctrl+u}{backspace}{ctrl+u}").await;
    assert!(h.screen(30, 6).ends_with("> say"), "{}", h.screen(30, 6));
}

#[tokio::test]
async fn mouse_wheel_scrolls_the_chat() {
    let mut h = Harness::blank().await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let text: Vec<String> = (1..=30).map(|i| format!("line {i}")).collect();
    h.emit::<MessageDelta>(MessageDeltaParams {
        session_id: "s-new".into(),
        turn_id: 1,
        kind: DeltaKind::Text,
        text: text.join("\n"),
    })
    .await;
    assert!(h.screen(30, 10).contains("line 30"));
    h.input("{wheelup}").await;
    let screen = h.screen(30, 10);
    assert!(
        !screen.contains("line 30") && screen.contains("line 27"),
        "{screen}"
    );
    h.input("{wheeldown}").await;
    assert!(h.screen(30, 10).contains("line 30"));
}

#[tokio::test]
async fn drag_selects_and_copies_text() {
    let mut h = Harness::blank().await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.emit::<MessageDelta>(MessageDeltaParams {
        session_id: "s-new".into(),
        turn_id: 1,
        kind: DeltaKind::Text,
        text: "first line\nsecond line".into(),
    })
    .await;
    assert!(
        h.screen(30, 6)
            .starts_with("> go\n\nfirst line\nsecond line")
    );
    // Drag from "line" on row 2 to "second" on row 3, then release.
    h.app.mouse_down((6, 2));
    h.app.mouse_drag((3, 3));
    h.screen(30, 6);
    assert_eq!(h.app.clipboard, None, "copied only on release");
    h.app.mouse_up((5, 3));
    h.screen(30, 6);
    assert_eq!(h.app.clipboard.take().as_deref(), Some("line\nsecond"));
    // Copied once; a key press clears the highlight.
    h.screen(30, 6);
    assert_eq!(h.app.clipboard, None);
    assert!(h.app.selection.is_some());
    h.input("x").await;
    assert!(h.app.selection.is_none());
    // A plain click selects nothing.
    h.app.mouse_down((1, 1));
    h.app.mouse_up((1, 1));
    h.screen(30, 6);
    assert!(h.app.selection.is_none() && h.app.clipboard.is_none());
}

#[tokio::test]
async fn system_http_and_defer_run_in_the_background() {
    let mut h = Harness::blank().await;
    h.app.exec_lua(
        "order = {} \
         bone.system('sleep 0.2; printf slow', function(r) order[#order + 1] = r.stdout end) \
         bone.system('tr a-z A-Z', { stdin = 'fast' }, function(r) order[#order + 1] = r.stdout end) \
         bone.defer(50, function() order[#order + 1] = 'timer' end) \
         bone.http({ url = 'http://127.0.0.1:1/' }, function(r, err) failed = err ~= nil end)",
    );
    // Nothing waited; results arrive as they finish.
    h.app.exec_lua("=#order");
    assert_eq!(h.message(), "0");
    for _ in 0..50 {
        if h.lua("=#order").await == "3" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        h.lua("=table.concat(order, ',')").await,
        "\"FAST,timer,slow\""
    );
    assert_eq!(h.lua("=failed").await, "true");
}

#[tokio::test]
async fn windows_without_focus_anchors_z_and_updates() {
    let mut h = Harness::blank().await;
    // A display-only window above the prompt: typing still reaches the prompt.
    h.lua("info = bone.ui.win({ anchor = 'prompt', lines = { 'INFO' } })")
        .await;
    assert_eq!(h.app.context(), Context::Main);
    h.input("hi").await;
    assert_eq!(h.prompt(), "hi");
    assert_eq!(h.screen(20, 5), "\n\n\nINFO\nhi");

    // Updating changes it in place; another window with a higher z covers it.
    h.lua("bone.ui.update(info, { lines = { 'INFO 2' }, col = -1 })")
        .await;
    assert_eq!(h.screen(20, 5), "\n\n\n              INFO 2\nhi");
    h.lua("top = bone.ui.win({ anchor = 'prompt', z = 5, col = -1, lines = { 'OVER' } })")
        .await;
    assert_eq!(h.screen(20, 5), "\n\n\n              INOVER\nhi");
    h.lua("bone.ui.update(top, { z = -1 })").await;
    assert_eq!(h.screen(20, 5), "\n\n\n              INFO 2\nhi");
    assert_eq!(h.lua("=bone.ui.is_open(top)").await, "true");
    h.lua("bone.ui.close(top); bone.ui.close(info)").await;
    assert_eq!(h.lua("=bone.ui.is_open(top)").await, "false");
    assert_eq!(h.lua("=bone.ui.update(top, {})").await, "false");

    // A window anchored in the chat area, at its top left.
    h.lua("bone.ui.win({ anchor = 'chat', row = 0, col = 0, lines = { 'C' } })")
        .await;
    assert!(h.screen(20, 5).starts_with("C\n"));
    let err = h
        .lua("bone.ui.win({ anchor = 'nowhere', lines = {} })")
        .await;
    assert!(err.contains("unknown anchor"), "{err}");
}

#[tokio::test]
async fn layout_orders_rows_and_takes_any_region() {
    let mut h = Harness::blank().await;
    h.lua(
        "bone.ui.statusline = function() return 'STATUS' end \
         bone.ui.regions.bar = { size = 1, render = function() return { 'BAR' } end } \
         bone.ui.layout = { 'statusline', 'bar', 'prompt', 'chat' }",
    )
    .await;
    h.input("typed").await;
    assert_eq!(h.screen(20, 7), "STATUS\nBAR\ntyped\n\n\n\n");
    // Without "prompt" in the layout there is no prompt row.
    h.lua("bone.ui.layout = { 'chat', 'statusline' }").await;
    assert_eq!(h.screen(20, 3), "\n\nSTATUS");
}

#[tokio::test]
async fn select_and_suggestions_are_lua() {
    let mut h = Harness::blank().await;
    h.lua(
        "pick = bone.ui.select({ 'apple', 'banana', 'cherry' }, { prompt = 'Fruit', \
           on_choice = function(item, i) chosen = tostring(item) .. ':' .. tostring(i) end })",
    )
    .await;
    assert_eq!(h.app.context(), Context::Popup);
    let screen = h.screen(40, 12);
    assert!(
        screen.contains("│ Fruit  ▏") && screen.contains("│ banana"),
        "{screen}"
    );
    // Filter, move with the wheel, pick.
    h.input("an{wheeldown}{enter}").await;
    assert_eq!(h.lua("=chosen").await, "\"banana:2\"");
    assert_eq!(h.app.context(), Context::Main);
    // Esc cancels with nil; items can arrive later.
    h.lua("pick = bone.ui.select({}, { loading = true, on_choice = function(x) chosen = tostring(x) end })")
        .await;
    assert!(h.screen(40, 12).contains("loading…"));
    h.lua("pick:set_items({ 'later' })").await;
    assert!(h.screen(40, 12).contains("│ later"));
    h.input("{esc}").await;
    assert_eq!(h.lua("=chosen").await, "\"nil\"");

    // The slash-command list is drawn by bone.ui.suggestions.
    h.lua("bone.ui.suggestions = function(ctx) return { '#' .. #ctx.items .. ' ' .. ctx.items[ctx.selected].name } end")
        .await;
    h.input("/ne").await;
    assert!(
        h.screen(40, 6).ends_with("#1 new\n/ne"),
        "{}",
        h.screen(40, 6)
    );
    h.lua("bone.ui.suggestions = nil").await;
    assert_eq!(h.screen(40, 6), "\n\n\n\n\n/ne");
}

#[tokio::test]
async fn help_topics_and_health_in_a_pager() {
    let mut h = Harness::blank().await;
    h.input("/help hooks{enter}").await;
    assert_eq!(h.app.context(), Context::Popup);
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("lua.md: Hooks") && screen.contains("### Hooks"),
        "{screen}"
    );
    // It scrolls, and esc closes it.
    assert!(screen.contains("1-"), "{screen}");
    h.input("{pagedown}").await;
    assert!(!h.screen(80, 24).contains("### Hooks"));
    h.input("{esc}").await;
    assert_eq!(h.app.context(), Context::Main);
    h.input("/help no-such-thing-anywhere{enter}").await;
    assert!(h.message().contains("no help for"), "{}", h.message());

    // /health: the TUI's checks, Lua checks, then the core's answer.
    h.lua("bone.health('mine', function() return 'warn', 'look here' end)")
        .await;
    h.input("/health{enter}").await;
    let screen = h.screen(100, 24);
    assert!(
        screen.contains("✓ terminal: TERM=")
            && screen.contains("clipboard:")
            && screen.contains("! mine: look here")
            && screen.contains("✓ provider: m at http://x"),
        "{screen}"
    );
    assert_eq!(h.requests("health/check").len(), 1);
}

#[tokio::test]
async fn a_blank_row_separates_the_chat_from_the_prompt() {
    let mut h = Harness::blank().await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let text: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
    h.emit::<MessageDelta>(MessageDeltaParams {
        session_id: "s-new".into(),
        turn_id: 1,
        kind: DeltaKind::Text,
        text: text.join("\n"),
    })
    .await;
    h.input("typing").await;
    assert!(
        h.screen(30, 6).ends_with("line 19\nline 20\n\ntyping"),
        "{}",
        h.screen(30, 6)
    );
    // It is just Lua: remove it and the text sits on the prompt.
    h.lua("bone.ui.divider = nil").await;
    assert!(h.screen(30, 6).ends_with("line 20\ntyping"));
}

#[tokio::test]
async fn panels_dock_beside_and_above_the_chat() {
    let mut h = Harness::blank().await;
    h.lua(
        r#"
        local rows = {}
        for i = 1, 20 do rows[i] = "row " .. i end
        p = bone.ui.panel.open({ id = "notes", size = 12, title = "Notes", lines = rows })
        t = bone.ui.panel.open({ dock = "top", render = function(ctx) return { "top " .. ctx.width } end })
        "#,
    )
    .await;
    assert_eq!(h.message(), "");
    assert_eq!(h.lua("=bone.has_capability('tui.panels')").await, "true");
    h.app.message = None;
    // 60 columns: the right panel takes 12 and a separator; the top panel
    // fits its one row over the chat column.
    let screen = h.screen(60, 10);
    let rows: Vec<&str> = screen.lines().collect();
    assert_eq!(rows[0], format!("{:<47}│Notes", "top 47"), "{screen}");
    assert_eq!(rows[1], format!("{:<47}│row 1", ""), "{screen}");
    assert_eq!(rows[7], format!("{:<47}│row 7", ""), "{screen}");
    assert_eq!(h.lua("=p:info().width").await, "12");
    assert_eq!(h.lua("=p:info().rows").await, "20");

    // Hidden panels give their room back and keep their state.
    h.lua("p:scroll(5); p:hide()").await;
    let screen = h.screen(60, 10);
    assert!(
        !screen.contains("Notes") && screen.starts_with("top 60"),
        "{screen}"
    );
    assert_eq!(h.lua("=p:info().visible").await, "false");
    h.lua("p:toggle()").await;
    assert!(h.screen(60, 10).lines().nth(1).unwrap().ends_with("│row 6"));

    // Too narrow: the chat keeps 20 columns and a panel below `min` is not drawn.
    h.lua("p:update({ min = 10 })").await;
    assert!(!h.screen(30, 10).contains("Notes"));
    assert_eq!(h.lua("=p:info().visible").await, "false");
    h.lua("p:update({ min = 1, size = 0.5, dock = 'left', title = false })")
        .await;
    let screen = h.screen(60, 10);
    assert!(
        screen.lines().next().unwrap().starts_with("row 6"),
        "{screen}"
    );
    assert_eq!(h.lua("=p:info().width").await, "30");
    assert_eq!(
        h.lua("=#bone.ui.panel.list() .. bone.ui.panel.list()[1].id")
            .await,
        "\"2notes\""
    );
}

#[tokio::test]
async fn panels_take_the_keyboard_and_scroll() {
    let mut h = Harness::blank().await;
    h.lua(
        r#"
        events, hits = {}, {}
        bone.on("panel/opened", function(ev) events[#events + 1] = "open:" .. ev.id .. ":" .. ev.kind end)
        bone.on("panel/closed", function(ev) events[#events + 1] = "close:" .. ev.id end)
        bone.on("focus/changed", function(ev) events[#events + 1] = "focus:" .. tostring(ev.panel) .. ":" .. ev.context end)
        local rows = {}
        for i = 1, 20 do rows[i] = "row " .. i end
        p = bone.ui.panel.open({
          id = "notes", size = 12, lines = rows,
          keys = { x = function(panel) hits[#hits + 1] = "x:" .. panel.id end },
          on_key = function(key, panel)
            if key == "y" then hits[#hits + 1] = "y:" .. panel.id; return true end
          end,
          on_close = function(panel) hits[#hits + 1] = "closed:" .. panel.id end,
        })
        other = bone.ui.panel.open({ id = "other", dock = "left", size = 10, lines = { "o" } })
        "#,
    )
    .await;
    h.screen(60, 10);
    h.lua("p:focus()").await;
    assert_eq!(h.app.context().name(), "panel");
    assert_eq!(h.lua("=bone.ui.panel.focused()").await, "\"notes\"");
    // Its own keys, then on_key; unmapped text is not typed anywhere.
    h.input("xyz").await;
    assert_eq!(
        h.lua("=table.concat(hits, ',')").await,
        "\"x:notes,y:notes\""
    );
    assert_eq!(h.prompt(), "");
    // The panel keymaps scroll the panel, not the chat. 8 rows show.
    h.input("{pagedown}").await;
    assert_eq!(h.lua("=p:info().top").await, "7");
    h.input("{end}").await;
    assert!(
        h.screen(60, 10)
            .lines()
            .next()
            .unwrap()
            .ends_with("│row 13")
    );
    h.input("{home}{down}").await;
    assert_eq!(h.lua("=p:info().top").await, "1");
    // The wheel over a panel scrolls it, wherever the keyboard is.
    assert!(h.app.panel_wheel((55, 2), false));
    assert_eq!(h.lua("=p:info().top").await, "4");
    assert!(!h.app.panel_wheel((30, 2), false));

    // tab cycles the panels in placement order, then the prompt.
    h.input("{tab}").await;
    assert_eq!(h.lua("=bone.ui.panel.focused()").await, "nil");
    h.input("a").await;
    assert_eq!(h.prompt(), "a");
    h.lua("bone.action('focus_next')").await;
    assert_eq!(h.lua("=bone.ui.panel.focused()").await, "\"other\"");
    h.input("{esc}").await;
    assert_eq!(h.app.context().name(), "main");

    // A panel can bring its own context; a click focuses it.
    h.lua(
        r#"
        bone.keymap.context("review", { fallback = "panel" })
        bone.keymap.set("r", function() hits[#hits + 1] = "review" end, { context = "review" })
        p:update({ context = "review" })
        "#,
    )
    .await;
    h.app.mouse_down((55, 3));
    h.app.mouse_up((55, 3));
    assert_eq!(h.lua("=bone.keymap.current()").await, "\"review\"");
    h.input("rq{down}").await;
    assert_eq!(h.lua("=hits[#hits]").await, "\"review\"");
    assert_eq!(h.prompt(), "a");
    assert_eq!(h.lua("=p:info().top").await, "5");

    // Hiding or closing a focused panel gives the keyboard back.
    h.lua("p:hide()").await;
    assert_eq!(h.app.context().name(), "main");
    h.lua("p:show(); p:focus(); p:close()").await;
    assert_eq!(h.app.context().name(), "main");
    assert_eq!(h.lua("=bone.ui.panel.get('notes')").await, "nil");
    assert_eq!(h.lua("=hits[#hits]").await, "\"closed:notes\"");
    assert_eq!(
        h.lua("=table.concat(events, ',')").await,
        "\"open:notes:panel,open:other:panel,focus:notes:panel,focus:nil:main,\
          focus:other:panel,focus:nil:main,focus:notes:review,focus:nil:main,\
          focus:notes:review,focus:nil:main,close:notes\""
    );
    // Unknown ids are not errors for update/close.
    assert_eq!(h.lua("=p:close()").await, "false");
    assert_eq!(h.lua("=p:update({})").await, "false");
}

#[tokio::test]
async fn panel_errors_and_following_content() {
    let mut h = Harness::blank().await;
    for (code, want) in [
        ("bone.ui.panel.open({})", "needs render"),
        (
            "bone.ui.panel.open({ lines = {}, dock = 'middle' })",
            "unknown dock",
        ),
        (
            "bone.ui.panel.open({ lines = {}, size = -2 })",
            "panel size",
        ),
        (
            "bone.ui.panel.open({ lines = {}, id = 'a b' })",
            "invalid panel id",
        ),
        (
            "bone.ui.panel.open({ lines = {}, context = 'popup' })",
            "context cannot be popup",
        ),
        ("bone.ui.panel.focus('nope')", "no panel nope"),
    ] {
        h.lua(code).await;
        assert!(h.message().contains(want), "{code}: {}", h.message());
    }
    h.lua("bone.ui.panel.open({ id = 'a', lines = {}, focusable = false })")
        .await;
    h.lua("bone.ui.panel.open({ id = 'a', lines = {} })").await;
    assert!(h.message().contains("already open"), "{}", h.message());
    h.lua("bone.ui.panel.focus('a')").await;
    assert!(h.message().contains("not focusable"), "{}", h.message());
    assert_eq!(h.lua("=#bone.ui.panel.list()").await, "1");

    // A render error is reported once; the panel stays, empty, until fixed.
    h.lua("b = bone.ui.panel.open({ id = 'b', dock = 'bottom', size = 2, render = function() error('boom') end })")
        .await;
    h.screen(40, 10);
    assert!(
        h.message().contains("bone.ui.panel.b failed"),
        "{}",
        h.message()
    );
    h.lua("b:set_lines({ 'fixed' })").await;
    assert!(h.screen(40, 10).contains("fixed"));

    // follow keeps the end in view until scrolled away from it.
    h.lua(
        r#"
        bone.ui.panel.close('a')
        log = {}
        f = bone.ui.panel.open({ id = 'log', dock = 'top', size = 3, follow = true, lines = log })
        for i = 1, 5 do log[i] = "entry " .. i end
        "#,
    )
    .await;
    let screen = h.screen(40, 12);
    assert!(screen.starts_with("entry 3\nentry 4\nentry 5"), "{screen}");
    h.lua("f:scroll(-1); log[6] = 'entry 6'").await;
    assert!(h.screen(40, 12).starts_with("entry 2"));
    h.lua("f:scroll('bottom'); log[7] = 'entry 7'").await;
    assert!(h.screen(40, 12).starts_with("entry 5\nentry 6\nentry 7"));
}

#[tokio::test]
async fn lua_edits_the_prompt_by_position_range_and_selection() {
    let mut h = Harness::blank().await;
    h.input("hello world").await;
    h.lua(r#"events = {}; bone.on("prompt", function(ev) events[#events + 1] = ev end)"#)
        .await;
    assert_eq!(
        h.lua("=bone.has_capability('tui.prompt_edit')").await,
        "true"
    );
    h.lua("bone.prompt.set_cursor({ row = 0, col = 5 }); bone.prompt.insert(',')")
        .await;
    assert_eq!(h.prompt(), "hello, world");
    assert_eq!(h.lua("=bone.prompt.cursor().col").await, "6");

    // A selection is drawn, reported, and replaced by typing.
    h.lua("bone.prompt.select(7, 12)").await;
    assert_eq!(h.lua("=bone.prompt.selection().text").await, "\"world\"");
    assert_eq!(h.lua("=events[#events].selection.start.col").await, "7");
    {
        let sel = h.app.theme.hl("Selection");
        let mut term = Terminal::new(TestBackend::new(30, 4)).unwrap();
        term.draw(|f| render::draw(f, &mut h.app)).unwrap();
        let buf = term.backend().buffer();
        let row = (0..4)
            .find(|&y| buf[(0, y)].symbol() == "h")
            .expect("prompt row");
        let selected = |x| {
            let st = buf[(x, row)].style();
            st.bg == sel.bg && st.add_modifier.contains(sel.add_modifier)
        };
        assert!(sel != ratatui::style::Style::default());
        assert!(!selected(6) && selected(7) && selected(11) && !selected(12));
    }
    h.input("there").await;
    assert_eq!(h.prompt(), "hello, there");
    assert_eq!(h.lua("=bone.prompt.selection()").await, "nil");

    h.lua(r#"bone.prompt.set_range({ row = 0, col = 0 }, { row = 0, col = 5 }, "hi\nyou")"#)
        .await;
    assert_eq!(h.prompt(), "hi\nyou, there");
    assert_eq!(h.lua("=bone.prompt.cursor().row").await, "1");
    assert_eq!(h.lua("=bone.prompt.get_range(0, 2)").await, "\"hi\"");
    assert_eq!(
        h.lua("=bone.prompt.offset({ row = 1, col = 0 })").await,
        "3"
    );
    assert_eq!(h.lua("=bone.prompt.position(4).col").await, "1");
    assert_eq!(h.lua("=#bone.prompt.lines()").await, "2");

    // Deleting removes the selection; moving drops it.
    h.lua("bone.prompt.select({ row = 1, col = 3 }, { row = 1, col = 10 })")
        .await;
    h.input("{backspace}").await;
    assert_eq!(h.prompt(), "hi\nyou");
    h.lua("bone.prompt.select(0, 2)").await;
    h.input("{left}").await;
    assert_eq!(h.prompt(), "hi\nyou");
    assert_eq!(h.lua("=bone.prompt.selection()").await, "nil");

    h.lua("bone.prompt.set_cursor('x')").await;
    assert!(h.message().contains("prompt position"), "{}", h.message());
    // The compatibility calls still work.
    h.lua("bone.api.prompt_set('again')").await;
    assert_eq!(h.lua("=bone.prompt.get()").await, "\"again\"");
}

#[tokio::test]
async fn lua_reads_turns_items_and_sessions() {
    let mut h = Harness::blank().await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = |id: &str, name: &str| ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: r#"{"path":"a"}"#.into(),
    };
    h.emit::<MessageCompleted>(tool_calls(
        "s-new",
        vec![call("c1", "shell"), call("c2", "read_file")],
    ))
    .await;
    assert_eq!(h.lua("=bone.chat.count({ running = true })").await, "2");
    for (id, is_error) in [("c1", false), ("c2", true)] {
        h.emit::<ToolFinished>(ToolFinishedParams {
            session_id: "s-new".into(),
            turn_id: 1,
            call_id: id.into(),
            output: "out".into(),
            is_error,
        })
        .await;
    }
    h.emit::<MessageCompleted>(MessageCompletedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: "done".into(),
            reasoning: String::new(),
            tool_calls: vec![],
        },
        usage: None,
    })
    .await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    h.input("again{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "again")).await;
    assert_eq!(h.lua("=bone.chat.turns()[2].running").await, "true");
    h.emit::<TurnFinished>(finished(
        "s-new",
        TurnOutcome::Failed {
            message: "boom".into(),
        },
    ))
    .await;

    let lua = |code: &'static str| code;
    for (code, want) in [
        (lua("=bone.has_capability('tui.chat_data')"), "true"),
        ("=bone.chat.count()", "6"),
        ("=bone.chat.count({ kind = 'tool', running = true })", "0"),
        (
            "=bone.chat.items({ name = 'read_file' })[1].is_error",
            "true",
        ),
        ("=bone.chat.count({ error = true })", "2"),
        ("=bone.chat.count({ turn = 2 })", "2"),
        ("=bone.chat.items({ last = 1 })[1].kind", "\"notice\""),
        (
            "=bone.chat.items({ from = 2, to = 3, first = 1 })[1].name",
            "\"shell\"",
        ),
        (
            "=bone.chat.item(4).text .. bone.chat.item(4).turn",
            "\"done1\"",
        ),
        ("=bone.chat.item(99)", "nil"),
        ("=#bone.chat.turns()", "2"),
        (
            "=bone.chat.turns()[1].tools .. bone.chat.turns()[1].tool_errors",
            "\"21\"",
        ),
        ("=bone.chat.turns()[1].outcome", "\"completed\""),
        (
            "=bone.chat.turns()[2].outcome .. ':' .. bone.chat.turns()[2].error",
            "\"failed:boom\"",
        ),
        (
            "=bone.chat.turns()[2].first .. '-' .. bone.chat.turns()[2].last",
            "\"5-6\"",
        ),
        ("=bone.chat.turns()[2].running", "false"),
        ("=bone.chat.session().session_id", "\"s-new\""),
        (
            "=bone.chat.session().cwd .. bone.chat.session().turns",
            "\"/work2\"",
        ),
        ("=bone.chat.session().current", "true"),
        ("=bone.chat.session({ session = 'nope' })", "nil"),
        ("=bone.chat.count({ session = 'nope' })", "0"),
        ("=bone.chat.count({ session = 's-new' })", "6"),
        ("=#bone.chat.sessions()", "1"),
    ] {
        assert_eq!(h.lua(code).await, want, "{code}");
    }
    // Copies: changing them changes nothing.
    h.lua("bone.chat.items()[1].text = 'changed'").await;
    assert_eq!(h.lua("=bone.chat.item(1).text").await, "\"go\"");

    // The stored transcript comes from the core.
    h.lua("bone.chat.messages(function(m) got = #m end)").await;
    assert_eq!(h.lua("=got").await, "2");
    assert_eq!(
        h.requests("session/messages"),
        vec![json!({ "session_id": "s-new" })]
    );
}

impl Harness {
    /// Keep handling events until the Lua expression `cond` is true.
    async fn until(&mut self, cond: &str) {
        let code = format!("={cond}");
        for _ in 0..200 {
            if self.lua(&code).await == "true" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {cond}: {}", self.message());
    }
}

#[tokio::test]
async fn jobs_stream_output_and_report_how_they_end() {
    let mut h = Harness::blank().await;
    assert_eq!(
        h.lua("=bone.has_capability('jobs.streaming')").await,
        "true"
    );
    h.app.message = None;
    h.lua(
        r#"
        events = {}
        bone.on("job/started", function(ev) events[#events + 1] = "start:" .. ev.name end)
        bone.on("job/finished", function(ev) events[#events + 1] = "end:" .. ev.name .. ":" .. ev.state end)
        out, err = {}, {}
        j = bone.job.start("printf 'a\\nb\\303'; sleep 0.1; printf 'c\\n' >&2; printf '\\251'; exit 3", {
          name = "t", lines = true,
          on_stdout = function(l, job) out[#out + 1] = l; seen_id = job.id end,
          on_stderr = function(l) err[#err + 1] = l end,
          on_exit = function(r, job) res = r end,
        })
        "#,
    )
    .await;
    assert_eq!(h.message(), "");
    h.until("res ~= nil").await;
    for (code, want) in [
        ("=table.concat(out, '|')", "\"a|bé\""),
        ("=table.concat(err, '|')", "\"c\""),
        ("=res.code .. res.state", "\"3exited\""),
        ("=res.stdout", "nil"),
        ("=res.stdout_bytes", "5"),
        ("=seen_id == j.id", "true"),
        ("=j:running()", "false"),
        ("=j:status().state", "\"exited\""),
        ("=table.concat(events, ',')", "\"start:t,end:t:exited\""),
    ] {
        assert_eq!(h.lua(code).await, want, "{code}");
    }

    // Without output callbacks the output is kept; stdin, env and cwd.
    h.lua(
        r#"
        k = bone.job.start({ "cat" }, { stdin = true, on_exit = function(r) kept = r end })
        k:write("hi\n")
        k:close_stdin()
        bone.job.start("cat; echo $FOO; pwd", { stdin = "xyz\n", env = { FOO = "bar" }, cwd = "/",
          on_exit = function(r) piped = r.stdout end })
        "#,
    )
    .await;
    h.until("kept ~= nil and piped ~= nil").await;
    assert_eq!(h.lua("=kept.stdout == 'hi\\n' and kept.code").await, "0");
    assert_eq!(h.lua("=piped == 'xyz\\nbar\\n/\\n'").await, "true");
}

#[tokio::test]
async fn jobs_cancel_time_out_and_fail() {
    let mut h = Harness::blank().await;
    // The sleep holds stdout open: cancelling must stop the whole group.
    h.lua(
        r#"
        c = bone.job.start("sleep 30; echo late", { on_exit = function(r) cancelled = r end })
        t = bone.job.start("sleep 30", { timeout = 100, on_exit = function(r) timed = r end })
        f = bone.job.start({ "/nonexistent/bone-test" }, { on_exit = function(r) failed = r end })
        "#,
    )
    .await;
    assert_eq!(
        h.lua("=#bone.job.list() .. ':' .. c:status().state").await,
        "\"3:running\""
    );
    h.until("c:status().pid ~= nil").await;
    assert_eq!(h.lua("=c:cancel()").await, "true");
    h.until("cancelled ~= nil and timed ~= nil and failed ~= nil")
        .await;
    for (code, want) in [
        ("=cancelled.cancelled and cancelled.state", "\"cancelled\""),
        ("=cancelled.stdout", "\"\""),
        ("=cancelled.duration_ms < 1900", "true"),
        ("=timed.timed_out and timed.state", "\"timed_out\""),
        ("=failed.state", "\"failed\""),
        ("=c:cancel()", "false"),
    ] {
        assert_eq!(h.lua(code).await, want, "{code}");
    }
    assert!(h.lua("=failed.error").await.contains("cannot run"));

    for (code, want) in [
        ("bone.job.start(42)", "shell command or an argv list"),
        ("bone.job.start({})", "argv list is empty"),
        ("bone.job.start('true', { timeout = -1 })", "timeout"),
        (
            "bone.job.start('true', { on_exit = 1 })",
            "must be a function",
        ),
    ] {
        h.lua(code).await;
        assert!(h.message().contains(want), "{code}: {}", h.message());
    }
}

#[tokio::test]
async fn plugins_own_what_they_create_and_unload_cleanly() {
    let (mut h, dir) = Harness::with_config(
        r#"
        bone.keymap.set("f8", function() bone.notify("user f8") end)
        unloaded = {}
        bone.on("plugin/unloaded", function(ev) unloaded[#unloaded + 1] = ev.name end)
        "#,
    )
    .await;
    let demo = dir.path().join("plugins/demo");
    std::fs::create_dir_all(demo.join("lua/demo")).unwrap();
    std::fs::write(demo.join("lua/demo/mod.lua"), "return { v = 1 }").unwrap();
    std::fs::write(
        demo.join("tui.lua"),
        r#"
        local m = require("demo.mod")
        loads = (loads or 0) + 1
        cur = bone.plugin.current().name
        bone.keymap.set("f5", function() bone.notify("demo f5 " .. m.v) end)
        bone.keymap.set("f6", function() bone.ui.panel.open({ id = "late", lines = { "late" } }) end)
        bone.cmd.create("demo", function() end, { desc = "demo" })
        bone.on("prompt", function() demo_prompt = (demo_prompt or 0) + 1 end)
        bone.o.define("demo_level", 1)
        bone.keymap.context("demo-ctx")
        bone.ui.panel.open({ id = "demo", lines = { "demo panel" } })
        bone.job.start("sleep 30", { on_exit = function() demo_job_exit = true end })
        local st = bone.plugin.state()
        st.count = (st.count or 0) + 1
        bone.plugin.on_shutdown(function() shutdowns = (shutdowns or 0) + 1 end)
        "#,
    )
    .unwrap();
    h.input("/plugin load demo{enter}").await;
    assert_eq!(h.lua("=cur .. loads").await, "\"demo1\"");
    assert_eq!(
        h.lua("=bone.has_capability('plugins.lifecycle')").await,
        "true"
    );
    h.input("{f6}{f5}").await;
    assert_eq!(h.message(), "demo f5 1");
    assert_eq!(h.lua("=#bone.ui.panel.list()").await, "2");
    assert!(h.app.user_commands.contains_key("demo"));

    h.input("/plugin unload demo{enter}").await;
    assert_eq!(h.lua("=shutdowns").await, "1");
    assert_eq!(h.lua("=#bone.ui.panel.list()").await, "0");
    assert!(!h.app.user_commands.contains_key("demo"));
    assert_eq!(h.lua("=pcall(bone.o.get, 'demo_level')").await, "false");
    assert_eq!(
        h.lua("=pcall(bone.keymap.focus, 'demo-ctx')").await,
        "false"
    );
    assert_eq!(h.lua("=package.loaded['demo.mod']").await, "nil");
    assert_eq!(h.lua("=unloaded[1]").await, "\"demo\"");
    assert_eq!(h.lua("=bone.job.list()[1].running").await, "false");
    assert_eq!(h.lua("=demo_job_exit").await, "nil");
    // Its prompt handler is gone; the user's own key is not.
    h.lua("demo_prompt = 0").await;
    h.input("x{backspace}").await;
    assert_eq!(h.lua("=demo_prompt").await, "0");
    h.input("{f5}").await;
    assert_ne!(h.message(), "demo f5 1");
    h.input("{f8}").await;
    assert_eq!(h.message(), "user f8");
    let saved = std::fs::read_to_string(dir.path().join("state/tui/demo.json")).unwrap();
    assert!(saved.contains("\"count\": 1"), "{saved}");

    // Loading again re-reads its modules and its state.
    std::fs::write(demo.join("lua/demo/mod.lua"), "return { v = 2 }").unwrap();
    h.input("/plugin reload demo{enter}").await;
    assert!(h.message().contains("not loaded"), "{}", h.message());
    h.input("/plugin load demo{enter}").await;
    h.input("{f5}").await;
    assert_eq!(h.message(), "demo f5 2");
    h.input("/plugin reload demo{enter}").await;
    assert_eq!(h.lua("=shutdowns .. ':' .. loads").await, "\"2:3\"");
    assert!(
        h.lua("=bone.inspect(bone.plugin.list())")
            .await
            .contains("demo")
    );
    h.app.shutdown();
    assert_eq!(h.lua("=shutdowns").await, "3");
    let saved = std::fs::read_to_string(dir.path().join("state/tui/demo.json")).unwrap();
    assert!(saved.contains("\"count\": 3"), "{saved}");
    h.input("/plugin unload nope{enter}").await;
    assert!(
        h.message().contains("plugin nope has no core.lua"),
        "{}",
        h.message()
    );

    // Both halves: the list merges the core's plugins in, and a name that
    // only the core has goes to the core.
    h.input("/plugin{enter}").await;
    assert!(
        h.message().contains("corepart (core loaded)")
            && h.message().contains("demo (tui loaded)")
            && h.message().contains("style (tui loaded)"),
        "{}",
        h.message()
    );
    h.input("/plugin reload corepart{enter}").await;
    assert_eq!(h.message(), "corepart: core reloaded");
    assert_eq!(
        h.requests("plugin/reload").last().unwrap()["name"],
        "corepart"
    );
    h.input("/plugin reload demo{enter}").await;
    assert_eq!(h.message(), "demo: tui reloaded");
    h.input("/plugin reload{enter}").await;
    assert_eq!(h.message(), "core configuration reloaded: data_dir changed");
    assert_eq!(h.requests("core/reload").len(), 1);
}

#[tokio::test]
async fn a_project_config_runs_only_when_trusted() {
    let (mut h, dir) = Harness::with_config("").await;
    let project = tempfile::tempdir().unwrap();
    let work = project.path().join("src/deep");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(project.path().join(".bone")).unwrap();
    std::fs::write(
        project.path().join(".bone/tui.lua"),
        r#"
        project_loads = (project_loads or 0) + 1
        bone.keymap.set("f9", function() bone.notify("project f9") end)
        "#,
    )
    .unwrap();
    h.app.cwd = work.to_string_lossy().into_owned();
    h.app.load_project();
    assert!(h.message().contains("/project trust"), "{}", h.message());
    assert_eq!(h.lua("=project_loads").await, "nil");
    assert_eq!(h.lua("=bone.project.info().trusted").await, "false");

    h.input("/project trust{enter}").await;
    assert_eq!(h.lua("=project_loads").await, "1");
    h.input("{f9}").await;
    assert_eq!(h.message(), "project f9");
    let trusted =
        std::fs::read_to_string(dir.path().join("state/tui/trusted-projects.json")).unwrap();
    assert!(
        trusted.contains(
            &std::fs::canonicalize(project.path())
                .unwrap()
                .to_string_lossy()
                .into_owned()
        ),
        "{trusted}"
    );
    // Trust is remembered: the next start loads it.
    h.app.unload_plugin("project").unwrap();
    h.app.load_project();
    assert_eq!(h.lua("=project_loads").await, "2");

    h.input("/project untrust{enter}").await;
    h.input("{f9}").await;
    assert_ne!(h.message(), "project f9");
    assert_eq!(h.lua("=bone.project.info().trusted").await, "false");
    h.app.cwd = "/".into();
    h.input("/project{enter}").await;
    assert!(h.message().contains("no .bone/tui.lua"), "{}", h.message());
}

/// A harness with the style plugin plus `examples/plugins/<name>` loaded.
async fn with_example(name: &str) -> (Harness, tempfile::TempDir) {
    let (mut h, dir) = Harness::with_config("").await;
    install_example(dir.path(), name);
    h.input(&format!("/plugin load {name}{{enter}}")).await;
    assert!(
        h.app
            .plugin(name)
            .is_some_and(|p| p.loaded && p.error.is_none()),
        "{}",
        h.message()
    );
    h.app.message = None;
    (h, dir)
}

#[tokio::test]
async fn example_tasks_plugin() {
    let (mut h, dir) = with_example("tasks").await;
    h.input("/task write docs{enter}/task ship it{enter}").await;
    let screen = h.screen(80, 12);
    assert!(
        screen.contains("Tasks 2/2")
            && screen.contains("· write docs")
            && screen.contains("· ship it"),
        "{screen}"
    );
    h.input("/task{enter}").await;
    assert_eq!(h.app.context().name(), "panel");
    h.input("{up}{enter}").await;
    assert!(h.screen(80, 12).contains("Tasks 1/2"));
    let saved = std::fs::read_to_string(dir.path().join("state/tui/tasks.json")).unwrap();
    assert!(
        saved.contains("\"done\": true") && saved.contains("\"open\": true"),
        "{saved}"
    );
    h.input("{down}s").await;
    assert_eq!(h.prompt(), "ship it");
    assert_eq!(h.app.context().name(), "main");
    h.input("{ctrl+u}/tasks{enter}").await;
    assert!(!h.screen(80, 12).contains("Tasks"));
    // Reloading reads the saved list back.
    h.input("/tasks{enter}/plugin reload tasks{enter}").await;
    assert!(h.screen(80, 12).contains("✓ write docs"));
}

#[tokio::test]
async fn example_review_plugin() {
    let (mut h, _dir) = with_example("review").await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = |id: &str, name: &str, path: &str| ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: json!({ "path": path }).to_string(),
    };
    h.emit::<MessageCompleted>(tool_calls(
        "s-new",
        vec![
            call("1", "edit_file", "src/a.rs"),
            call("2", "edit_file", "src/a.rs"),
            call("3", "write_file", "b.md"),
            call("4", "read_file", "c.rs"),
        ],
    ))
    .await;
    for (id, is_error) in [("1", false), ("2", false), ("3", true), ("4", false)] {
        h.emit::<ToolFinished>(ToolFinishedParams {
            session_id: "s-new".into(),
            turn_id: 1,
            call_id: id.into(),
            output: "ok".into(),
            is_error,
        })
        .await;
    }
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    h.input("/review{enter}").await;
    let screen = h.screen(100, 24);
    assert!(
        screen.contains("Changed files")
            && screen.contains("src/a.rs  2 edits, turn 1")
            && screen.contains("b.md  1 edit, turn 1, 1 failed")
            && !screen.contains("c.rs  "),
        "{screen}"
    );
    h.lua("bone.ui.panel.focus('review')").await;
    h.input("{down}{enter}").await;
    assert!(
        h.prompt().starts_with("Review your changes to b.md:"),
        "{}",
        h.prompt()
    );
    assert_eq!(h.lua("=bone.prompt.selection().start.col").await, "0");
    h.input("/review all{enter}").await;
    assert!(h.prompt().contains("src/a.rs, b.md"), "{}", h.prompt());
}

#[tokio::test]
async fn example_testrun_plugin() {
    let (mut h, _dir) = with_example("testrun").await;
    assert_eq!(h.lua("=bone.o.test_command").await, "\"cargo test\"");
    h.input("/test printf 'ok 1\\nFAILED tests::x\\n'; exit 1{enter}")
        .await;
    h.until("bone.ui.panel.get('testrun'):info().title:find('failed') ~= nil")
        .await;
    let screen = h.screen(80, 20);
    assert!(
        screen.contains("Tests: failed (exit 1) · 1 failing lines")
            && screen.contains("FAILED tests::x")
            && screen.contains("ok 1"),
        "{screen}"
    );
    h.lua("bone.ui.panel.focus('testrun')").await;
    h.input("f").await;
    assert!(
        h.prompt().contains("These tests fail:\n\nFAILED tests::x"),
        "{}",
        h.prompt()
    );
    h.lua("bone.prompt.set('')").await;
    h.input("/test sleep 30{enter}").await;
    h.until("bone.ui.panel.get('testrun'):info().title:find('running') ~= nil")
        .await;
    h.input("/test cancel{enter}").await;
    h.until("bone.ui.panel.get('testrun'):info().title:find('cancelled') ~= nil")
        .await;
}

#[tokio::test]
async fn example_switch_plugin_tui_side() {
    let (mut h, dir) = with_example("switch").await;
    h.lua(
        r#"bone.state.save("switch-providers", { default = "a", providers = {
          { name = "a", model = "m1", type = "openai" },
          { name = "b", model = "m2", type = "anthropic" },
        } }, { shared = true })"#,
    )
    .await;
    assert_eq!(h.lua("=bone.switch.current()").await, "\"a\"");
    h.input("/provider b{enter}").await;
    assert_eq!(h.message(), "provider: b (m2)");
    let saved = std::fs::read_to_string(dir.path().join("state/shared/switch.json")).unwrap();
    assert!(saved.contains("\"current\": \"b\""), "{saved}");
    h.input("/provider zzz{enter}").await;
    assert!(h.message().contains("no provider zzz"), "{}", h.message());
    h.input("/provider{enter}").await;
    let screen = h.screen(100, 20);
    assert!(
        screen.contains("● b  m2  anthropic") && screen.contains("  a  m1  openai"),
        "{screen}"
    );
    h.input("{up}{enter}").await;
    assert_eq!(h.lua("=bone.switch.current()").await, "\"a\"");
}

#[tokio::test]
async fn a_session_changed_by_core_lua_is_reloaded() {
    let mut h = Harness::blank().await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    h.emit::<SessionUpdated>(SessionUpdatedParams {
        session_id: "s-new".into(),
        reason: "compact".into(),
    })
    .await;
    assert_eq!(h.requests("session/messages").len(), 1);
    let screen = h.screen(60, 10);
    assert!(
        screen.contains("old question") && !screen.contains("> go"),
        "{screen}"
    );
    // Other sessions are not this TUI's business.
    h.emit::<SessionUpdated>(SessionUpdatedParams {
        session_id: "elsewhere".into(),
        reason: "append".into(),
    })
    .await;
    assert_eq!(h.requests("session/messages").len(), 1);
}

#[tokio::test]
async fn tui_lua_calls_the_model_through_the_core() {
    let mut h = Harness::blank().await;
    assert_eq!(h.lua("=bone.has_capability('tui.model')").await, "true");
    h.lua(
        r#"
        got, parts = nil, {}
        call = bone.model.complete({ prompt = "name it", system = "short", provider = "cheap" },
          function(d) parts[#parts + 1] = d.text end,
          function(r, err) got = r and r.content or err end)
        "#,
    )
    .await;
    let req = h.requests("model/complete")[0].clone();
    assert_eq!(req["provider"], "cheap");
    assert_eq!(req["stream"], true);
    assert_eq!(req["messages"][0]["role"], "system");
    assert_eq!(req["messages"][1]["content"], "name it");
    h.emit::<ModelDeltaEvent>(ModelDeltaParams {
        request_id: 41,
        kind: DeltaKind::Text,
        text: "Fix ".into(),
    })
    .await;
    h.emit::<ModelDeltaEvent>(ModelDeltaParams {
        request_id: 99,
        kind: DeltaKind::Text,
        text: "someone else's".into(),
    })
    .await;
    h.emit::<ModelCompleted>(ModelCompletedParams {
        request_id: 41,
        message: Some(ChatMessage::Assistant {
            content: "Fix typo".into(),
            reasoning: String::new(),
            tool_calls: vec![],
        }),
        usage: None,
        error: None,
    })
    .await;
    assert_eq!(
        h.lua("=got .. '/' .. table.concat(parts)").await,
        "\"Fix typo/Fix \""
    );
    h.lua("call:cancel()").await;
    assert_eq!(h.requests("model/cancel")[0]["request_id"], 41);
}

#[tokio::test]
async fn tui_lua_calls_core_lua_functions() {
    let mut h = Harness::blank().await;
    assert_eq!(h.lua("=bone.has_capability('tui.rpc')").await, "true");
    h.lua(
        r#"
        bone.rpc.call("templates.expand", { name = "review", args = "src/x.rs" }, function(text) expanded = text end)
        bone.rpc.call("nothing.here", {}, function(r, err) failed = err end)
        "#,
    )
    .await;
    assert_eq!(h.lua("=expanded").await, "\"Review src/x.rs\"");
    assert!(h.lua("=failed").await.contains("no function nothing.here"));
    let req = h.requests("lua/call")[0].clone();
    assert_eq!(req["name"], "templates.expand");
    assert_eq!(req["args"]["args"], "src/x.rs");
}

#[tokio::test]
async fn example_agent_extension_plugins_in_the_tui() {
    let (mut h, dir) = Harness::with_config("").await;
    for name in ["compact", "mcp", "skills", "templates", "ask-model"] {
        install_example(dir.path(), name);
        h.input(&format!("/plugin load {name}{{enter}}")).await;
        assert!(
            h.app
                .plugin(name)
                .is_some_and(|p| p.loaded && p.error.is_none()),
            "{name}: {}",
            h.message()
        );
    }

    // compact: nothing before a session; then the core's template does it.
    h.input("/compact{enter}").await;
    assert_eq!(h.message(), "nothing to compact yet");
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.emit::<MessageCompleted>(MessageCompletedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: "a long answer".into(),
            reasoning: String::new(),
            tool_calls: vec![],
        },
        usage: None,
    })
    .await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    h.input("/compact{enter}").await;
    let req = h.requests("lua/call").last().unwrap().clone();
    assert_eq!(
        (req["name"].as_str(), req["session_id"].as_str()),
        (Some("compact"), Some("s-new"))
    );
    assert_eq!(h.message(), "compacted");

    // mcp: a panel of servers.
    h.input("/mcp{enter}").await;
    let screen = h.screen(120, 20);
    assert!(
        screen.contains("gh  ready  1 tools")
            && screen.contains("gh_search")
            && screen.contains("cannot run db-mcp"),
        "{screen}"
    );
    h.input("{esc}").await;

    // skills: names complete, and /skill writes the request.
    h.input("/skill release do it now{enter}").await;
    assert_eq!(h.prompt(), "Use the release skill: do it now");
    h.lua("bone.prompt.set('')").await;

    // templates: each one is a command that fills the prompt.
    assert!(h.app.user_commands.contains_key("review"));
    assert!(h.app.user_commands["review"].desc.ends_with("<path>"));
    h.input("/review src/x.rs{enter}").await;
    assert_eq!(h.prompt(), "Review src/x.rs");
    h.lua("bone.prompt.set('')").await;

    // ask-model: /ask alone explains the latest answer, in a pager.
    h.input("/ask{enter}").await;
    let req = h.requests("model/complete").last().unwrap().clone();
    assert!(
        req["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("a long answer")
    );
    h.emit::<ModelDeltaEvent>(ModelDeltaParams {
        request_id: 41,
        kind: DeltaKind::Text,
        text: "It means".into(),
    })
    .await;
    assert!(h.screen(100, 20).contains("It means"));
    h.emit::<ModelCompleted>(ModelCompletedParams {
        request_id: 41,
        message: Some(ChatMessage::Assistant {
            content: "It means: be careful.".into(),
            reasoning: String::new(),
            tool_calls: vec![],
        }),
        usage: None,
        error: None,
    })
    .await;
    assert!(h.screen(100, 20).contains("It means: be careful."));
}

#[tokio::test]
async fn model_events_before_the_reply_are_kept() {
    let mut h = Harness::blank().await;
    // The server runs requests concurrently, so a call's events can beat
    // its reply.
    h.emit::<ModelDeltaEvent>(ModelDeltaParams {
        request_id: 41,
        kind: DeltaKind::Text,
        text: "early ".into(),
    })
    .await;
    h.emit::<ModelCompleted>(ModelCompletedParams {
        request_id: 41,
        message: Some(ChatMessage::Assistant {
            content: "early answer".into(),
            reasoning: String::new(),
            tool_calls: vec![],
        }),
        usage: None,
        error: None,
    })
    .await;
    h.lua(
        r#"parts = {}
           bone.model.complete({ prompt = "x" }, function(d) parts[#parts + 1] = d.text end,
             function(r) got = r.content end)"#,
    )
    .await;
    assert_eq!(
        h.lua("=got .. '|' .. table.concat(parts)").await,
        "\"early answer|early \""
    );
}

#[tokio::test]
async fn session_commands_rename_fork_and_delete() {
    let mut h = Harness::new().await;
    h.input("/rename x{enter}").await;
    assert!(h.message().contains("no messages yet"), "{}", h.message());
    h.input("hello{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "hello")).await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;

    h.input("/rename Typo hunt{enter}").await;
    assert_eq!(h.message(), "renamed to Typo hunt");
    assert_eq!(h.requests("session/rename")[0]["title"], "Typo hunt");
    assert!(h.screen(80, 24).contains("Typo hunt"));

    h.input("/fork 2{enter}").await;
    assert_eq!(
        h.requests("session/fork")[0],
        json!({ "session_id": "s-new", "before_turn": 2 })
    );
    assert_eq!(h.message(), "forked from before turn 2");
    assert_eq!(h.lua("=bone.chat.session().session_id").await, "\"s-fork\"");
    h.input("/fork two{enter}").await;
    assert!(h.message().contains("usage"));

    h.input("/delete{enter}").await;
    assert!(h.message().contains("/delete yes"));
    assert!(h.requests("session/delete").is_empty());
    h.input("/delete yes{enter}").await;
    assert_eq!(h.requests("session/delete")[0]["session_id"], "s-fork");
    // Any client's delete clears the chat that shows it.
    h.emit::<SessionDeleted>(SessionRef {
        session_id: "s-fork".into(),
    })
    .await;
    assert_eq!(h.message(), "session deleted");
    assert_eq!(h.lua("=bone.chat.session().new").await, "true");
}

#[tokio::test]
async fn typing_during_a_turn_queues_it() {
    let mut h = Harness::new().await;
    let queued = |id: u64, text: &str, mode: QueueMode| QueuedMessage {
        id,
        text: text.into(),
        mode,
        created_at: 0,
    };
    let changed = |items: Vec<QueuedMessage>| QueueChangedParams {
        session_id: "s-new".into(),
        items,
        paused: false,
    };
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.input("also the README{enter}").await;
    assert_eq!(
        h.requests("queue/add"),
        vec![json!({ "session_id": "s-new", "text": "also the README", "mode": "steer" })]
    );
    assert_eq!(h.prompt(), "");
    // The core's queue shows at the end of the chat, through a Lua view.
    h.emit::<QueueChanged>(changed(vec![queued(
        1,
        "also the README",
        QueueMode::Steer,
    )]))
    .await;
    let screen = h.screen(80, 20);
    assert!(
        screen.contains("◦ also the README") && screen.contains("joins this turn"),
        "{screen}"
    );
    assert_eq!(
        h.lua("=bone.chat.items({ kind = 'queued' })[1].mode").await,
        "\"steer\""
    );
    // It joins: a user message, and the queue is empty again.
    h.emit::<TurnSteered>(started("s-new", "also the README"))
        .await;
    h.emit::<QueueChanged>(changed(vec![])).await;
    let screen = h.screen(80, 20);
    assert!(
        screen.contains("› also the README") && !screen.contains("joins this turn"),
        "{screen}"
    );

    // queue_mode, and the actions that pick a mode.
    h.lua("bone.o.queue_mode = 'next'").await;
    h.input("later{enter}").await;
    h.lua("bone.prompt.set('now'); bone.action('queue_steer')")
        .await;
    let modes: Vec<Value> = h
        .requests("queue/add")
        .iter()
        .map(|r| r["mode"].clone())
        .collect();
    assert_eq!(modes, [json!("steer"), json!("next"), json!("steer")]);

    // Up on an empty prompt takes the last queued message back.
    h.emit::<QueueChanged>(changed(vec![
        queued(6, "first", QueueMode::Next),
        queued(7, "edit me", QueueMode::Next),
    ]))
    .await;
    h.input("/queue{enter}").await;
    assert_eq!(h.message(), "1. [next] first\n2. [next] edit me");
    h.input("{up}").await;
    assert_eq!(h.prompt(), "edit me");
    assert_eq!(
        h.requests("queue/remove")[0],
        json!({ "session_id": "s-new", "id": 7 })
    );
    h.input("{ctrl+u}/unqueue 1{enter}").await;
    assert_eq!(h.requests("queue/remove")[1]["id"], 6);
    h.input("/queue clear{enter}").await;
    assert_eq!(h.requests("queue/clear").len(), 1);

    // One the core refuses comes back at once.
    *h.fail.lock().unwrap() = Some("queue/add".into());
    h.input("refused{enter}").await;
    assert_eq!(h.prompt(), "refused");
    assert!(h.message().starts_with("not sent"), "{}", h.message());
}

#[tokio::test]
async fn the_command_menu_and_actions_are_lua() {
    let mut h = Harness::new().await;
    // Matching is the bone.menu module: fewer rows.
    h.lua("require('bone.menu').max = 2").await;
    h.input("/s").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("/sessions") && screen.contains("/set") && !screen.contains("/source"),
        "{screen}"
    );
    h.input("{ctrl+u}").await;
    // A builtin action can be taken over; returning false lets it run.
    h.lua(
        r#"bone.ui.actions.submit = function()
             if bone.prompt.get() == "shout" then bone.notify("HEY") bone.prompt.set("") return true end
             return false
           end"#,
    )
    .await;
    h.input("shout{enter}").await;
    assert_eq!(h.message(), "HEY");
    assert!(h.requests("turn/start").is_empty());
    h.input("hello{enter}").await;
    assert_eq!(h.requests("turn/start")[0]["text"], "hello");
}
