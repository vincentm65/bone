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

fn info(id: &str, title: Option<&str>) -> Value {
    json!({ "session_id": id, "cwd": "/work", "created_at": 0, "title": title })
}

/// A session as `session/messages` gives it; `child-*` sessions are
/// sub-agents of s-new.
fn loaded_info(id: &str) -> Value {
    let mut i = info(id, Some("loaded"));
    if id.starts_with("child-") {
        i["owner"] = json!({ "session_id": "s-new", "call_id": "c1", "name": "reviewer" });
    }
    i
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
        // Core RPC replies used by the API-contract tests.
        "session/compact" => json!({
            "session_id": params["session_id"],
            "messages": if params["clear"] == json!(true) { 0 } else { 12 },
            "tokens_before": 90000,
            "tokens_after": 30000,
            "reason": if params["clear"] == json!(true) { "clear" } else { "manual" },
        }),
        "lua/call" => match params["name"].as_str().unwrap_or_default() {
            "demo.echo" => json!(format!(
                "Review {}",
                params["args"]["args"].as_str().unwrap_or("")
            )),
            other => {
                return Err(RpcError::new(
                    RpcError::INVALID_PARAMS,
                    format!("no function {other} registered with bone.rpc"),
                ));
            }
        },
        // Usage for the runtime session picker and statusline.
        "store/query" => {
            let rows = if params["sql"]
                .as_str()
                .unwrap_or_default()
                .contains("SELECT s.id, s.updated_at")
            {
                json!([
                    ["s-two", 1700000001, 12800, 3],
                    ["s-one", 1700000000, 42, 1]
                ])
            } else {
                json!([[3, 12000, 800, 600, 1, 1]])
            };
            json!({ "columns": [], "rows": rows, "truncated": false })
        }
        "model/complete" => json!({ "request_id": 41 }),
        "queue/add" => json!({ "id": 1 }),
        "plugin/list" => json!([{ "name": "corepart", "core": true, "loaded": true }]),
        "model/list" => json!([
            { "name": "ds", "model": "d1", "current": false },
            { "name": "qwen", "model": "q1", "current": true, "base_url": "http://localhost:8081/v1" },
            { "name": "extra", "model": "e1", "current": false, "base_url": "http://e/v1", "added": true },
        ]),
        "core/reload" => json!({ "plugins": [], "warnings": ["data_dir changed"] }),
        "initialize" => {
            json!({ "protocol_version": 0, "server_name": "fake", "server_version": "0" })
        }
        "session/create" => info("s-new", None),
        "process/read" => json!({
            "offset": 0,
            "data": "\u{1b}[32mlistening\u{1b}[0m on 3000\r\n",
            "total": 28,
        }),

        "turn/start" => json!({ "turn_id": 1 }),
        "session/list" => json!([info("s-two", Some("second")), info("s-one", Some("first"))]),
        "session/active" => json!(["s-one"]),
        "health/check" => {
            json!([{ "name": "provider", "status": "ok", "message": "m at http://x" }])
        }
        "session/messages" => json!({
            "info": loaded_info(params["session_id"].as_str().unwrap()),
            "messages": [
                { "role": "user", "content": "old question" },
                { "role": "assistant", "content": "old answer" },
            ],
        }),
        _ => Value::Null,
    })
}

impl Harness {
    /// The standard runtime UI, with an isolated user config.
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Self::build(Some(dir.path().to_owned())).await;
        h.app.load_user_config();
        h.settle().await;
        h._dir = Some(dir);
        h
    }

    /// No config and the standard UI taken off again: the blank slate the
    /// Rust fallbacks draw (the defaults' keys and actions, a blank divider).
    async fn blank() -> Self {
        let mut h = Self::build(None).await;
        h.app
            .with_api(|lua| {
                lua.load(
                    r#"
                    local actions = {}
                    for k, v in pairs(bone.ui.actions) do actions[k] = v end
                    bone.ui.clear()
                    for k, v in pairs(actions) do bone.ui.actions[k] = v end
                    -- One blank row between the chat and the prompt.
                    bone.ui.divider = function() return {} end
                    "#,
                )
                .exec()
            })
            .unwrap();
        h
    }

    /// With a config dir containing `tui.lua`,
    /// loaded like at startup.
    async fn with_config(tui_lua: &str) -> (Self, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
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
    assert!(screen.contains("thinking"), "{screen}");

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
            context_tokens: None,
            cached_tokens: None,
        }),
    })
    .await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    let screen = h.screen(80, 24);
    assert!(screen.contains("  Hi! Done."), "{screen}");
    assert!(
        screen.contains(" hello") && screen.contains("curr 600 | total 12.0k"),
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
async fn approval_popup_answers_questions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tui.lua"),
        r#"
        local popups = {}
        bone.on("ask/requested", function(ev)
          if ev.question.kind ~= "approval" then return end
          local function answer()
            bone.request("ask/respond", { ask_id = ev.ask_id, answer = "allow" })
            bone.ui.close(popups[ev.ask_id])
          end
          popups[ev.ask_id] = bone.ui.popup({
            lines = { ev.question.title }, keys = { y = answer }, guard = 300,
          })
        end)
        bone.on("ask/resolved", function(ev)
          if popups[ev.ask_id] then bone.ui.close(popups[ev.ask_id]) end
        end)
    "#,
    )
    .unwrap();
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
    assert!(screen.contains("Allow shell?"), "{screen}");

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
    assert!(h.screen(80, 24).contains("Allow write_file?"));
    h.emit::<AskResolved>(AskResolvedParams {
        ask_id: 8,
        answer: Value::Null,
    })
    .await;
    assert_eq!(h.app.context(), Context::Main);

    // The handler ignores unrelated questions.
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
    h.input("/n").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("/new") && screen.contains("start a new session"),
        "{screen}"
    );
    // Tab completes the selected suggestion; enter runs it.
    h.input("{tab}").await;
    assert_eq!(h.prompt(), "/new ");
    h.input("{enter}").await;
    assert_eq!(h.message(), "");
    assert_eq!(h.prompt(), "");

    // Enter on a partial name runs the selected suggestion.
    h.input("/hel{enter}").await;
    assert_eq!(h.app.context(), Context::Popup);
    h.input("sessions").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("/sessions") && screen.contains("ctrl+o"),
        "{screen}"
    );
    h.input("{esc}").await;

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
async fn slash_menu_scrolls_when_it_overflows() {
    let mut h = Harness::new().await;
    // 32 extra commands overflow the window (the 22 builtin names/aliases
    // alone do not).
    h.lua(
        "for i = 1, 32 do bone.cmd.create('zz' .. i, function() end, { desc = 'generated ' .. i }) end",
    )
    .await;
    h.input("/").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("/?") && !screen.contains(" of 54"),
        "{screen}"
    );
    // The wheel and pages scroll the menu to the end.
    h.input("{wheeldown}").await;
    let screen = h.screen(80, 24);
    // One wheel down moves the selection while the list remains attached to the prompt.
    assert!(!screen.contains(" of 54"), "{screen}");
    for _ in 0..10 {
        h.input("{pagedown}").await;
    }
    let screen = h.screen(80, 24);
    assert!(screen.contains("/zz9"), "{screen}");
    // Up/down move the selection; tab completes it.
    h.input("{tab}").await;
    assert_eq!(h.prompt(), "/zz9 ");
    for i in 1..=32 {
        h.lua(&format!("bone.cmd.del('zz{}')", i)).await;
    }
}

#[tokio::test]
async fn session_sidebar_groups_live_and_recent_chats() {
    let mut h = Harness::new().await;
    h.lua("bone.ui.spinner = { frames = { 'A', 'B' }, interval = 1 }")
        .await;
    h.input("{ctrl+o}").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("Recent · last 1h · 1") && screen.contains("History · 1"),
        "{screen}"
    );
    assert!(
        screen.find("first").unwrap() < screen.find("second").unwrap(),
        "{screen}"
    );
    assert!(
        screen.contains("A first") || screen.contains("B first"),
        "{screen}"
    );
    assert!(h.requests("session/messages").is_empty());
    h.emit::<TurnFinished>(finished("s-one", TurnOutcome::Completed))
        .await;
    let screen = h.screen(80, 24);
    assert!(
        !screen.contains("Running ·") && screen.contains("Recent · last 1h"),
        "{screen}"
    );
    h.lua("bone.ui.sessions_recent_seconds = 1800").await;
    assert!(h.screen(80, 24).contains("Recent · last 30m"));
    let queries_before_start = h.requests("store/query").len();
    h.emit::<TurnStarted>(started("s-two", "work")).await;
    assert_eq!(
        h.requests("store/query").len(),
        queries_before_start + 1,
        "starting a turn must refresh sidebar counts, not only finishing it"
    );
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("A second") || screen.contains("B second"),
        "{screen}"
    );
    // Header lines don't open a transcript; selection stays on its session.
    h.app.mouse("down", "left", (5, 2));
    h.settle().await;
    assert!(h.requests("session/messages").is_empty());
    h.input("{enter}").await;
    assert_eq!(
        h.requests("session/messages"),
        vec![json!({ "session_id": "s-one" })]
    );
    assert!(h.screen(80, 24).contains("Recent · last 30m · 2"));
}
#[tokio::test]
async fn session_sidebar_mouse_opens_conversation() {
    let mut h = Harness::new().await;
    h.input("{ctrl+o}").await;
    let screen = h.screen(80, 24);
    let y = screen
        .lines()
        .position(|line| line.contains("first"))
        .unwrap() as u16;
    h.app.mouse("down", "left", (5, y));
    h.settle().await;
    assert_eq!(h.app.context(), Context::Main);
    assert_eq!(
        h.requests("session/messages"),
        vec![json!({ "session_id": "s-one" })]
    );
}
#[tokio::test]
async fn session_picker_filters_and_opens() {
    let mut h = Harness::new().await;
    h.input("{ctrl+o}").await;
    // Conversations use a docked Lua panel, not a floating picker.
    assert_eq!(
        h.app.context(),
        Context::Panel,
        "{} {}",
        h.message(),
        h.screen(80, 24)
    );
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("second")
            && screen.contains("first")
            && screen.contains("12.8k tokens")
            && screen.contains("3 turns"),
        "{screen}"
    );
    // Typing filters.
    h.input("firs").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("Search: firs") && screen.contains("first") && !screen.contains("second"),
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
    h.input("{ctrl+o}{esc}").await;
    assert_eq!(h.app.context(), Context::Main);
    h.input("{ctrl+o}{tab}{tab}{esc}").await;
    assert!(h.screen(80, 24).contains("Search: firs"));
}

#[tokio::test]
async fn prompt_grows_and_history_recalls() {
    let mut h = Harness::new().await;
    h.input("one{ctrl+j}two{ctrl+j}three").await;
    let screen = h.screen(40, 12);
    assert!(screen.contains(" › one\n   two\n   three"), "{screen}");
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
    h.input("{esc}").await;
    assert_eq!(h.prompt(), "");
    h.input("clear me{esc}").await;
    assert_eq!(h.prompt(), "");
    let bulk = "x".repeat(501);
    h.app.paste(&format!("{bulk}\r\nend"));
    assert!(h.prompt().starts_with("[Pasted text #1 +505 chars]"));
    h.input("{left}").await;
    assert_eq!(h.prompt(), format!("{bulk}\nend"));
    h.input("{esc}").await;
    h.app.history_add("/first");
    h.app.history_add("/second");
    h.input("{up}{up}").await;
    assert_eq!(h.prompt(), "/first");
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
    assert_eq!(h.app.context(), Context::Popup);
    h.input("greet").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("/hello") && screen.contains("greet"),
        "{screen}"
    );
    h.input("{esc}").await;

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
    h.lua("bone.o.plugin_limit = 8").await;
    assert_eq!(h.lua("=changes[2]").await, "\"8:5\"");

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
    h.input("greetme").await;
    let screen = h.screen(100, 24);
    assert!(screen.contains("aliases: greetme"), "{screen}");
    h.input("{esc}").await;

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
    assert_eq!(h.lua("=bone.api.prompt_get()").await, "\"\"");

    // Errors show one line; bone.api.log has the full traceback.
    h.lua("error('boom')").await;
    assert!(
        h.message().starts_with("lua: ") && h.message().contains("boom"),
        "{}",
        h.message()
    );
    assert!(
        h.lua("=table.concat(bone.api.log(20), '\\n')")
            .await
            .contains("stack traceback"),
        "{}",
        h.message()
    );

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
        bone.ui.layout = nil
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
    h.input("{ctrl+o}").await;
    assert!(h.screen(40, 10).contains("panel"));
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
            context_tokens: None,
            cached_tokens: None,
        }),
    })
    .await;
    // The statusline asks the index for the usage when it first draws.
    h.screen(100, 12);
    h.settle().await;
    let wide = h.screen(100, 12);
    assert!(wide.contains("curr 600 | total 12.0k | cache"), "{wide}");
    let narrow = h.screen(24, 12);
    assert!(
        narrow.contains("thinking") && !narrow.contains("/work"),
        "{narrow}"
    );
}

#[tokio::test]
async fn lua_tool_views_highlights_and_colorschemes() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        bone.o.tool_detail = "rows"
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
        duration_ms: None,
    })
    .await;
    let screen = h.screen(60, 14);
    assert!(screen.contains("    ran ls\n    out: a.txt"), "{screen}");

    assert_eq!(h.app.colors_name.as_deref(), Some("black"));
    h.lua("bone.hl.set('ToolPath', { fg = '#010203', bold = true })")
        .await;
    assert_eq!(h.lua("=bone.hl.get('ToolPath').fg").await, "\"#010203\"");
    h.lua("bone.hl.set('Mine', { fg = 'red', underline = true })")
        .await;
    assert_eq!(h.lua("=bone.hl.get('Mine').underline").await, "true");
    h.lua("bone.colorscheme('ansi')").await;
    assert_eq!(h.lua("=bone.hl.get('ToolPath').fg").await, "\"cyan\"");
    h.lua("bone.colorscheme('nope')").await;
    assert!(h.message().contains("no colorscheme named nope"));
}

#[tokio::test]
async fn plugins_load_with_modules_and_colors() {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
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

    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    h.settle().await;
    h.input("/demo2{enter}").await;
    assert_eq!(h.message(), "from module");
    assert_eq!(h.lua("=table.concat(bone.plugins, ',')").await, "\"demo\"");
    h.lua("bone.colorscheme('demo')").await;
    assert_eq!(h.lua("=bone.hl.get('Normal').fg").await, "\"#123456\"");
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
    h.lua("bone.o.tool_detail = 'rows'; bone.ui.layout = nil; bone.ui.prompt = { prefix = '› ' } ")
        .await;
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
            duration_ms: None,
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
    let pending = tool_rows("shell", json!({"command": "git status"}), None, 60).await;
    assert_eq!(pending, ["  ◌ shell git status"]);
    let done = tool_rows(
        "shell",
        json!({"command": "echo hi"}),
        Some(("hi\n[exit code: 0]", false)),
        60,
    )
    .await;
    assert!(
        done.iter().any(|row| row.contains("shell echo hi")),
        "{done:?}"
    );
    assert!(
        done.iter()
            .any(|row| row.contains("hi") && row.contains("╰")),
        "{done:?}"
    );
    let error = tool_rows("word_count", json!({}), Some(("failed", true)), 60).await;
    assert!(
        error
            .iter()
            .any(|row| row.contains("✕") && row.contains("word_count")),
        "{error:?}"
    );
    assert!(error.iter().any(|row| row.contains("failed")), "{error:?}");
}

#[tokio::test]
async fn views_are_lua_and_can_be_replaced() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        -- Rust's default arrangement instead of the standard layout.
        bone.ui.layout = nil
        bone.ui.regions.thinking = nil
        bone.ui.prompt = { prefix = "› " }
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
    assert!(rows[above + 1].starts_with("›"), "{screen}");

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
async fn markdown_tables_wrap_instead_of_clipping() {
    let mut h = Harness::new().await;
    assert_eq!(h.lua(r#"=(function() local md = '| **abcdefghij** | 日本語 | [link](https://example.com) |\n|---|:---:|---:|\n| z |'
        for _, width in ipairs({ 10, 24, 80 }) do
          local text, bold, header = '', false, false
          for _, row in ipairs(bone.ui.markdown(md, width, '  ')) do
            local n = 0
            for _, s in ipairs(row) do
              n = n + bone.text.width(s[1])
              text = text .. s[1]
              bold = bold or s[2] == 'MdBold'
              header = header or s[2] == 'MdTableHeader'
            end
            assert(n <= width, n .. ' > ' .. width)
          end
          text = text:gsub('%s', ''):gsub('│', '')
          for ch in ('abcdefghij日本語link<https://example.com>z'):gmatch('.') do
            local pos = assert(text:find(ch, 1, true), text)
            text = text:sub(1, pos - 1) .. text:sub(pos + 1)
          end
          assert(text == '', text)
          assert(bold and header)
        end
        return 'ok' end)()"#).await, "\"ok\"");
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

/// The last `n` rows of a `w`×`h` screen, and the cursor.
fn bottom(h: &mut Harness, w: u16, rows: u16, n: usize) -> (Vec<String>, Option<(u16, u16)>) {
    let mut term = Terminal::new(TestBackend::new(w, rows)).unwrap();
    term.draw(|f| render::draw(f, &mut h.app)).unwrap();
    let cursor = term.get_cursor_position().ok().map(|p| (p.x, p.y));
    let buf = term.backend().buffer();
    let lines: Vec<String> = (0..rows)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    (lines[lines.len() - n..].to_vec(), cursor)
}

#[tokio::test]
async fn lua_draws_the_prompt_box_and_rust_keeps_the_text() {
    let mut h = Harness::blank().await;
    h.lua(
        r##"
        bone.hl.set("InputBackground", { bg = "#202020" })
        bone.ui.prompt = {
          border = { style = "rounded", hl = "InputBorder" },
          background = "InputBackground",
          padding = 1,
          prefix = "› ",
          continuation = "· ",
          placeholder = "Message bone…",
          top = { { " bone ", "InputTitle" }, { fill = "─" } },
          bottom = function(ctx)
            local state = ctx.running and "thinking" or (ctx.focused and "ready" or "away")
            return { { fill = "─" }, " " .. state .. " " .. ctx.rows .. "/" .. ctx.height .. " " }
          end,
        }
        "##,
    )
    .await;
    let (rows, cursor) = bottom(&mut h, 20, 10, 3);
    assert_eq!(
        rows,
        [
            "╭ bone ────────────╮",
            "│ › Message bone…  │",
            "╰─────── ready 1/1 ╯",
        ]
    );
    assert_eq!(cursor, Some((4, 8)));

    // Text wraps inside the box, after the prefix and the continuation,
    // and the cursor lands where the text is.
    h.input("hello world foo").await;
    let (rows, cursor) = bottom(&mut h, 20, 10, 4);
    assert_eq!(
        rows,
        [
            "╭ bone ────────────╮",
            "│ › hello world fo │",
            "│ · o              │",
            "╰─────── ready 2/2 ╯",
        ]
    );
    assert_eq!(cursor, Some((5, 8)));
    {
        let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
        term.draw(|f| render::draw(f, &mut h.app)).unwrap();
        let buf = term.backend().buffer();
        let bg = h.app.theme.hl("InputBackground").bg;
        assert!(bg.is_some());
        // The background fills the box, padding and text alike.
        assert_eq!(buf[(1, 7)].style().bg, bg);
        assert_eq!(buf[(6, 7)].style().bg, bg);
    }

    // Selection is still drawn by Rust, inside the box.
    h.lua("bone.prompt.select(6, 11)").await;
    {
        let sel = h.app.theme.hl("Selection");
        let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
        term.draw(|f| render::draw(f, &mut h.app)).unwrap();
        let buf = term.backend().buffer();
        let selected = |x: u16| {
            buf[(x, 7)].style().add_modifier.contains(sel.add_modifier)
                && buf[(x, 7)].style().bg == sel.bg
        };
        // "world" is columns 10..15 (border, pad, prefix, then "hello ").
        assert!(!selected(9) && selected(10) && selected(14) && !selected(15));
    }

    // The edges follow the session: running shows in the bottom border.
    h.input("{ctrl+u}go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let (rows, _) = bottom(&mut h, 20, 10, 1);
    assert_eq!(rows, ["╰──── thinking 1/1 ╯"]);
}

#[tokio::test]
async fn prompt_box_sides_and_edges_without_a_border() {
    let mut h = Harness::blank().await;
    // Rules above and below only, like the old bone.
    h.lua(r#"bone.ui.prompt = { border = { style = "single", sides = "tb" }, prefix = "> " }"#)
        .await;
    h.input("hi").await;
    let (rows, cursor) = bottom(&mut h, 12, 6, 3);
    assert_eq!(rows, ["────────────", "> hi        ", "────────────"]);
    assert_eq!(cursor, Some((4, 4)));

    // No border: a top line still gets its row, and a bad style is an error.
    h.lua(r#"bone.ui.prompt = { top = function(ctx) return ctx.lines .. " line" end }"#)
        .await;
    let (rows, _) = bottom(&mut h, 12, 6, 2);
    assert_eq!(rows, ["1 line      ", "hi          "]);
    h.lua(r#"bone.ui.prompt = { border = "wavy" }"#).await;
    h.screen(12, 6);
    assert!(
        h.message().contains("not one of rounded"),
        "{}",
        h.message()
    );
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
async fn help_browser_searches_commands_keys_and_docs() {
    let mut h = Harness::blank().await;
    h.lua("bone.cmd.create('demo', function() demo_ran = true end, { desc = 'A custom plugin command', aliases = { 'demonstrate' } })").await;
    h.input("/help{enter}").await;
    assert_eq!(h.app.context(), Context::Popup);
    let screen = h.screen(100, 24);
    assert!(
        screen.contains("bone / help") && screen.contains("Commands") && screen.contains("Docs"),
        "{screen}"
    );
    // Alias and description search, including user/plugin commands.
    h.input("demonstrate plugin").await;
    let screen = h.screen(100, 24);
    assert!(
        screen.contains("/demo") && screen.contains("aliases: demonstrate"),
        "{screen}"
    );
    assert!(!screen.contains("/quit"), "{screen}");
    h.input("{enter}").await;
    assert_eq!(h.app.context(), Context::Main);
    assert_eq!(h.prompt(), "/demo ");
    assert_eq!(h.lua("=demo_ran == nil").await, "true");
    // F1 can open help without disturbing a draft; closing restores it.
    h.input("{ctrl+u}a draft{f1}").await;
    assert_eq!(h.app.context(), Context::Popup);
    h.input("{tab}reasoning").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("ctrl+r") && screen.contains("default shortcut"),
        "{screen}"
    );
    h.input("{tab}lua hooks{enter}").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("lua.md: Hooks") && screen.contains("### Hooks"),
        "{screen}"
    );
    h.input("{esc}").await;
    assert_eq!(h.app.context(), Context::Popup);
    assert!(h.screen(80, 24).contains("lua hooks"));
    h.input("{esc}").await;
    assert_eq!(h.app.context(), Context::Main);
    assert_eq!(h.prompt(), "a draft");
}

#[tokio::test]
async fn help_browser_handles_empty_search_scrolling_and_resize() {
    let mut h = Harness::blank().await;
    h.input("/?{enter}no-such-command").await;
    let screen = h.screen(80, 24);
    assert!(screen.contains("No matches"), "{screen}");
    h.input("{enter}").await;
    assert_eq!(h.app.context(), Context::Popup);
    h.input("{ctrl+u}{end}").await;
    let screen = h.screen(80, 24);
    assert!(screen.contains("/setup"), "{screen}");
    h.input("{home}{pagedown}").await;
    let screen = h.screen(40, 12);
    assert!(
        screen.contains("bone / help") && screen.contains("esc"),
        "{screen}"
    );
    // A tiny terminal falls back to a compact hint and remains dismissible.
    assert!(h.screen(18, 5).contains("Help"));
    h.input("{esc}").await;
    assert_eq!(h.app.context(), Context::Main);
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
            duration_ms: None,
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
    h.input("/plugins load demo{enter}").await;
    assert_eq!(h.lua("=cur .. loads").await, "\"demo1\"");
    assert_eq!(
        h.lua("=bone.has_capability('plugins.lifecycle')").await,
        "true"
    );
    h.input("{f6}{f5}").await;
    assert_eq!(h.message(), "demo f5 1");
    assert_eq!(h.lua("=#bone.ui.panel.list()").await, "2");
    assert!(h.app.user_commands.contains_key("demo"));

    h.input("/plugins unload demo{enter}").await;
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
    h.input("/plugins reload demo{enter}").await;
    assert!(h.message().contains("not loaded"), "{}", h.message());
    h.input("/plugins load demo{enter}").await;
    h.input("{f5}").await;
    assert_eq!(h.message(), "demo f5 2");
    h.input("/plugins reload demo{enter}").await;
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
    h.input("/plugins unload nope{enter}").await;
    assert!(
        h.message().contains("plugin nope has no core.lua"),
        "{}",
        h.message()
    );

    // Both halves: the list merges the core's plugins in, and a name that
    // only the core has goes to the core.
    h.input("/plugins list{enter}").await;
    assert!(
        h.message().contains("corepart (core loaded)") && h.message().contains("demo (tui loaded)"),
        "{}",
        h.message()
    );
    h.input("/plugins reload corepart{enter}").await;
    assert_eq!(h.message(), "corepart: core reloaded");
    assert_eq!(
        h.requests("plugin/reload").last().unwrap()["name"],
        "corepart"
    );
    h.input("/plugins reload demo{enter}").await;
    assert_eq!(h.message(), "demo: tui reloaded");
    h.input("/plugins reload{enter}").await;
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
    assert!(h.message().contains("/plugins trust"), "{}", h.message());
    assert_eq!(h.lua("=project_loads").await, "nil");
    assert_eq!(h.lua("=bone.project.info().trusted").await, "false");

    h.input("/plugins trust{enter}").await;
    assert!(
        h.message().ends_with(": trusted and loaded"),
        "{}",
        h.message()
    );
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

    h.input("/plugins untrust{enter}").await;
    assert!(
        h.message().ends_with(": no longer trusted, unloaded"),
        "{}",
        h.message()
    );
    h.input("{f9}").await;
    assert_ne!(h.message(), "project f9");
    assert_eq!(h.lua("=bone.project.info().trusted").await, "false");
    h.app.cwd = "/".into();
    assert!(
        h.lua("=tostring(bone.project.info())")
            .await
            .contains("nil"),
        "{}",
        h.message()
    );
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
    // Events were lost: every open chat loads again.
    h.emit::<SessionUpdated>(SessionUpdatedParams {
        session_id: String::new(),
        reason: "lagged".into(),
    })
    .await;
    assert_eq!(h.requests("session/messages").len(), 2);
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
        bone.rpc.call("demo.echo", { name = "review", args = "src/x.rs" }, function(text) expanded = text end)
        bone.rpc.call("nothing.here", {}, function(r, err) failed = err end)
        "#,
    )
    .await;
    assert_eq!(h.lua("=expanded").await, "\"Review src/x.rs\"");
    assert!(h.lua("=failed").await.contains("no function nothing.here"));
    let req = h.requests("lua/call")[0].clone();
    assert_eq!(req["name"], "demo.echo");
    assert_eq!(req["args"]["args"], "src/x.rs");
}

#[tokio::test]
async fn compact_command_and_chat_note() {
    let (mut h, _dir) = Harness::with_config("").await;
    h.input("/compact{enter}").await;
    assert_eq!(h.message(), "this session has no messages yet");
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    h.input("/compact{enter}").await;
    assert_eq!(
        h.requests("session/compact").last().unwrap(),
        &json!({ "session_id": "s-new" })
    );
    assert_eq!(h.message(), "compacted");
    h.input("/compact clear{enter}").await;
    assert_eq!(
        h.requests("session/compact").last().unwrap(),
        &json!({ "session_id": "s-new", "clear": true })
    );

    // Every compaction, however it came about, leaves one line in the
    // chat; the messages above it stay.
    h.emit::<SessionCompacted>(SessionCompactedParams {
        session_id: "s-new".into(),
        messages: 48,
        tokens_before: 210_000,
        tokens_after: 125_000,
        reason: "overflow".into(),
    })
    .await;
    let screen = h.screen(100, 20);
    assert!(
        screen.contains("› go")
            && screen.contains(
                "◇ compacted 48 messages · ~85k tokens saved (210k → 125k) (the context was full)"
            ),
        "{screen}"
    );
    h.emit::<SessionCompactFailed>(SessionCompactFailedParams {
        session_id: "s-new".into(),
        reason: "limit".into(),
        error: "the summary failed: HTTP 429".into(),
    })
    .await;
    let screen = h.screen(120, 20);
    assert!(
        screen.contains("compaction failed (over compact.limit): the summary failed: HTTP 429"),
        "{screen}"
    );
    assert!(screen.contains("context unchanged"), "{screen}");
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
async fn session_commands_rename_and_fork() {
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

    // Up on an empty prompt edits the last queued message in place.
    h.emit::<QueueChanged>(changed(vec![
        queued(6, "first", QueueMode::Next),
        queued(7, "edit me", QueueMode::Next),
    ]))
    .await;
    h.input("/queue{enter}").await;
    assert_eq!(h.message(), "1. [next] first\n2. [next] edit me");
    h.input("{up}").await;
    assert_eq!(h.prompt(), "edit me");
    h.input(" now{enter}").await;
    assert_eq!(
        h.requests("queue/update")[0],
        json!({ "session_id": "s-new", "id": 7, "text": "edit me now" })
    );
    assert_eq!(
        (h.prompt(), h.requests("queue/add").len()),
        (String::new(), 3)
    );
    // The tray's Queue page: steer, move, drop.
    h.input("{down}s{shift+down}{shift+down}d").await;
    assert_eq!(
        h.requests("queue/update")[1],
        json!({ "session_id": "s-new", "id": 6, "mode": "steer" })
    );
    assert_eq!(
        h.requests("queue/move")[0],
        json!({ "session_id": "s-new", "id": 6, "to": 1 })
    );
    assert_eq!(
        h.requests("queue/remove")[0],
        json!({ "session_id": "s-new", "id": 6 })
    );
    assert_eq!(h.requests("queue/move")[0], h.requests("queue/move")[1]);
    h.emit::<QueueChanged>(changed(vec![
        queued(7, "edit me", QueueMode::Next),
        queued(6, "first", QueueMode::Next),
    ]))
    .await;
    h.input("d{esc}").await;
    assert_eq!(h.requests("queue/remove")[0], h.requests("queue/remove")[1]);
    h.input("/queue clear{enter}").await;
    assert_eq!(h.requests("queue/clear").len(), 1);

    // One the core refuses comes back at once.
    *h.fail.lock().unwrap() = Some("queue/add".into());
    h.input("refused{enter}").await;
    assert_eq!(h.prompt(), "refused");
    assert!(h.message().starts_with("not sent"), "{}", h.message());

    // Delay queue replies to exercise ordering and draft recovery.
    h.emit::<TurnFinished>(finished("s-new", TurnOutcome::Completed))
        .await;
    h.lua(
        r#"pending = {}
      bone.request = function(method, params, callback)
        pending[#pending + 1] = { method, params, callback }
      end"#,
    )
    .await;
    h.input("{ctrl+u}{down}{down}s").await;
    h.lua("assert(#pending == 1 and pending[1][1] == 'queue/move' and pending[1][2].id == 6)")
        .await;
    assert_eq!(h.message(), "");
    h.lua(
        r#"local session = bone.chat.session
      bone.chat.session = function() return { session_id = 'other' } end
      pending[1][3]()
      bone.chat.session = session
      assert(#pending == 2 and pending[2][1] == 'queue/resume')
      assert(pending[2][2].session_id == 's-new')"#,
    )
    .await;
    assert_eq!(h.message(), "");
    h.input("{esc}{down}{down}s").await;
    h.lua("pending[3][3](nil, 'missing'); assert(#pending == 3)")
        .await;
    assert_eq!(h.message(), "missing");
    h.lua("require('bone.ui.tray').edit({ id = 7, text = 'edit' })")
        .await;
    h.input("{enter}new draft").await;
    h.lua("pending[4][3](nil, 'missing')").await;
    assert_eq!(h.prompt(), "new draft\nedit");
    h.lua("require('bone.ui.tray').edit({ id = 7, text = 'old session' })")
        .await;
    h.input("{enter}").await;
    h.lua(
        r#"bone.chat.session = function() return { session_id = 'other' } end
      bone.prompt.set('other draft')
      pending[5][3](nil, 'missing')"#,
    )
    .await;
    assert_eq!(h.prompt(), "other draft");
}

#[tokio::test]
async fn the_command_menu_and_actions_are_lua() {
    let mut h = Harness::new().await;
    // The window is drawn by bone.ui.suggestions: show only the first row.
    h.lua("bone.ui.suggestions = function(ctx) return { '/' .. ctx.items[1].name } end")
        .await;
    h.input("/").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.contains("/help") && !screen.contains("/sessions"),
        "{screen}"
    );
    h.input("{ctrl+u}").await;
    h.lua("bone.ui.suggestions = nil").await;
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

#[tokio::test]
async fn reload_picks_up_edited_standard_library_modules() {
    let dir = tempfile::tempdir().unwrap();
    let views = dir.path().join("runtime/lua/bone/ui/views.lua");
    std::fs::create_dir_all(views.parent().unwrap()).unwrap();
    let write = |tag: &str| {
        std::fs::write(
            &views,
            format!("bone.ui.views.reasoning = function() return {{ {{ {{ '{tag}' }} }} }} end"),
        )
        .unwrap();
    };
    write("THINK-A");
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    let probe = |h: &mut Harness| -> String {
        h.app
            .with_api(|lua| {
                lua.load(
                    r#"local f = bone.ui.views.reasoning
                       return f and f()[1][1][1] or "missing""#,
                )
                .eval()
            })
            .unwrap()
    };
    assert_eq!(probe(&mut h), "THINK-A");
    write("THINK-B");
    h.app.reload_user_config();
    assert_eq!(probe(&mut h), "THINK-B");
}

#[tokio::test]
async fn reload_resets_views_and_keeps_the_standard_ui() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(&tui, "bone.ui.views.custom_kind = function() return {} end").unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    let has = |h: &mut Harness, expr: &str| -> bool {
        h.app
            .with_api(|lua| lua.load(format!("return {expr} ~= nil")).eval())
            .unwrap()
    };
    assert!(has(&mut h, "bone.ui.views.custom_kind"));
    assert!(has(&mut h, "bone.ui.views.reasoning"));
    assert!(has(&mut h, "bone.ui.regions.chat_empty"));

    // Deleting the line removes the view; the defaults' standard UI stays.
    std::fs::write(&tui, "").unwrap();
    h.app.reload_user_config();
    assert!(!has(&mut h, "bone.ui.views.custom_kind"));
    assert!(has(&mut h, "bone.ui.views.reasoning"));
    assert!(has(&mut h, "bone.ui.regions.chat_empty"));
    assert!(has(&mut h, "bone.ui.layout"));

    // An edited copy of the standard layout module is used after a reload.
    let layout = dir.path().join("runtime/lua/bone/ui/layout.lua");
    std::fs::create_dir_all(layout.parent().unwrap()).unwrap();
    std::fs::write(
        &layout,
        "return { setup = function() bone.ui.regions.thinking = { size = 7 } end }",
    )
    .unwrap();
    h.app.reload_user_config();
    let size: i64 = h
        .app
        .with_api(|lua| lua.load("return bone.ui.regions.thinking.size").eval())
        .unwrap();
    assert_eq!(size, 7);
}

#[tokio::test]
async fn reload_reruns_the_defaults_and_keeps_option_values() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(&tui, r#"bone.o.define("my_opt", 1, { type = "integer" })"#).unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    let eval = |h: &mut Harness, code: &str| -> String {
        h.app.with_api(|lua| lua.load(code).eval()).unwrap()
    };
    h.app
        .with_api(|lua| {
            lua.load(r#"bone.o.my_opt = 5 bone.o.queue_mode = "next""#)
                .exec()
        })
        .unwrap();

    // Edit the standard menu module: its actions must come from the new file.
    let menu = dir.path().join("runtime/lua/bone/menu.lua");
    std::fs::create_dir_all(menu.parent().unwrap()).unwrap();
    std::fs::write(
        &menu,
        "bone.ui.actions.dismiss = function() return 'EDITED' end",
    )
    .unwrap();
    h.app.reload_user_config();
    assert_eq!(
        h.app.log.last().unwrap(),
        "Lua configuration reloaded",
        "{:?}",
        h.app.log
    );

    assert_eq!(eval(&mut h, "return bone.ui.actions.dismiss()"), "EDITED");
    // Re-defining an option from the same file keeps what the user set.
    assert_eq!(eval(&mut h, "return tostring(bone.o.my_opt)"), "5");
    assert_eq!(eval(&mut h, "return bone.o.queue_mode"), "next");
    // The defaults' divider is back, and Lua's own libraries are untouched.
    assert_eq!(eval(&mut h, "return type(bone.ui.divider)"), "function");
    assert_eq!(
        eval(&mut h, r#"return tostring(require("bit").band(6, 3))"#),
        "2"
    );
}

#[tokio::test]
async fn standard_ui_has_a_three_row_prompt_and_a_running_statusline() {
    let mut h = Harness::build(None).await;
    let (w, rows) = (40, 16);
    let screen = h.screen(w, rows);
    assert!(
        screen.lines().last().unwrap_or("").contains("curr 0"),
        "{screen}"
    );
    let lines: Vec<&str> = screen.split('\n').collect();
    let n = lines.len();
    // The prompt's three rows, then the statusline on the last row.
    assert!(lines[n - 3].starts_with(" › Message bone"), "{screen}");
    assert_eq!((lines[n - 4], lines[n - 2]), ("", ""), "{screen}");

    // All three prompt rows share the input background, and the rows above
    // and below do not, also on a short terminal where regions get squeezed.
    for rows in [rows, 8] {
        let mut term = Terminal::new(TestBackend::new(w, rows)).unwrap();
        term.draw(|f| render::draw(f, &mut h.app)).unwrap();
        let buf = term.backend().buffer();
        let bg = |y: u16| buf[(w - 1, y)].bg;
        assert_ne!(bg(rows - 3), ratatui::style::Color::Reset, "{rows} rows");
        assert_eq!(bg(rows - 4), bg(rows - 3), "{rows} rows");
        assert_eq!(bg(rows - 2), bg(rows - 3), "{rows} rows");
        assert_ne!(bg(rows - 5), bg(rows - 3), "{rows} rows");
        assert_ne!(bg(rows - 1), bg(rows - 3), "{rows} rows");
    }

    // The statusline shows that a turn is running.
    h.input("hello{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "hello")).await;
    let screen = h.screen(w, rows);
    assert!(
        screen.lines().last().unwrap_or("").contains("thinking"),
        "{screen}"
    );
}

/// Chat rows the standard UI draws for these finished tool calls.
async fn std_tool_screen(
    calls: &[(&str, Value, &str, bool)],
    width: u16,
    ctrl_t: usize,
) -> Vec<String> {
    let mut h = Harness::build(None).await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let list = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args, _, _))| ToolCall {
            id: format!("c{i}"),
            name: (*name).into(),
            arguments: args.to_string(),
        })
        .collect();
    h.emit::<MessageCompleted>(tool_calls("s-new", list)).await;
    for (i, (_, _, output, is_error)) in calls.iter().enumerate() {
        h.emit::<ToolFinished>(ToolFinishedParams {
            session_id: "s-new".into(),
            turn_id: 1,
            call_id: format!("c{i}"),
            output: (*output).into(),
            is_error: *is_error,
            duration_ms: None,
        })
        .await;
    }
    for _ in 0..ctrl_t {
        h.input("{ctrl+t}").await;
    }
    let screen = h.screen(width, 60);
    screen
        .lines()
        .skip_while(|l| !l.starts_with(" › go") && !l.starts_with("› go"))
        .skip(1)
        .take_while(|l| !l.starts_with('─'))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn standard_tool_views_match_the_first_bone() {
    let calls = [
        (
            "read_file",
            json!({"path": "src/main.rs"}),
            "1#AB|fn main() {\n2#CD|}\n3#EF|x",
            false,
        ),
        (
            "shell",
            json!({"command": "ls -la | grep foo"}),
            "a\nb\nc\nd\ne\nf\ng\n[exit code: 0]",
            false,
        ),
        (
            "shell",
            json!({"command": "echo hi"}),
            "hi\n[exit code: 0]",
            false,
        ),
        (
            "web_search",
            json!({"query": "rust ratatui"}),
            "r1\nr2\nr3\nr4\nr5\nr6\nr7",
            false,
        ),
        (
            "edit_file",
            json!({"path": "a.rs"}),
            "Edited a.rs (-1 +2)\n4#AA|fn a() {\n-    old();\n+5#BB|    new();\n+6#CC|    more();\n7#DD|}",
            false,
        ),
        (
            "grep",
            json!({"pattern": "foo"}),
            "no such dir\nsecond",
            true,
        ),
    ];
    let collapsed = [
        "",
        "    read_file src/main.rs (lines 1-3, 3 read)",
        "",
        "    shell ls -la | grep foo",
        "      │ a",
        "      │ b",
        "      │ ⋮ +3 terminal lines (ctrl+t)",
        "      │ f",
        "      ╰ g",
        "",
        "    shell echo hi",
        "      ╰ hi",
        "",
        "    web_search rust ratatui",
        "r1",
        "r2",
        "r3",
        "r4",
        "r5",
        "⋮ +2 more lines (ctrl+t)",
        "",
        "    edit_file a.rs (-1 | +2)",
        "      4   fn a() {",
        "      5 -     old();",
        "      5 +     new();",
        "      6 +     more();",
        "      6   }",
        "",
        "  ✕ grep",
        "      │ no such dir",
        "      ╰ second",
    ];
    // The default: one line for the stretch before the edit; the edit and
    // the failed call in full.
    let rows = std_tool_screen(&calls, 60, 0).await;
    assert_eq!(
        rows[..5],
        [
            "",
            "    Read 1 file, ran 2 shell commands, called web_search",
            "",
            "    edit_file a.rs (-1 | +2)",
            "      4   fn a() {",
        ],
        "{}",
        rows.join("\n")
    );
    assert!(rows.join("\n").contains("Called grep, 1 failed"));
    assert!(!rows.join("\n").contains("✕ Called grep"));

    // ctrl+t once: a row per call.
    let rows = std_tool_screen(&calls, 60, 1).await;
    assert_eq!(rows[..collapsed.len()], collapsed, "{}", rows.join("\n"));

    // ctrl+t twice: every output in full, file contents included.
    let rows = std_tool_screen(&calls, 60, 2).await;
    let text = rows.join("\n");
    for want in [
        "    read_file src/main.rs (lines 1-3, 3 read)\n      1   fn main() {\n      2   }\n      3   x",
        "      │ c\n      │ d\n      │ e\n      │ f\n      ╰ g",
        "r5\nr6\nr7\n",
    ] {
        assert!(text.contains(want), "{want:?} in\n{text}");
    }
    assert!(!text.contains("⋮"), "{text}");
}

#[tokio::test]
async fn subagent_reports_have_a_task_header_and_a_markdown_preview() {
    let report = "Implemented `session/active` in **bone-proto** and **bone-core**.\n\nChanges:\n- Registered `SessionActive`.\n- Added request dispatch.\n- Added coverage.\n\nValidation: all checks passed.";
    let calls = [(
        "subagent",
        json!({"task": "Expose active sessions", "name": "implementer"}),
        report,
        false,
    )];
    for detail in [0, 1] {
        let text = std_tool_screen(&calls, 80, detail).await.join("\n");
        assert!(
            text.contains("✓ Expose active sessions · implementer · done"),
            "{text}"
        );
        assert!(
            text.contains("│ Implemented session/active in bone-proto and bone-core."),
            "{text}"
        );
        assert!(text.contains("╰ ⋮ +"), "{text}");
        assert!(text.contains("report lines (ctrl+t)"), "{text}");
        assert!(!text.contains("Changes:"), "{text}");
        assert!(!text.contains("Registered SessionActive"), "{text}");
        assert!(!text.contains('`') && !text.contains("**"), "{text}");
    }
    let text = std_tool_screen(&calls, 80, 2).await.join("\n");
    assert!(text.contains("│ • Registered SessionActive."), "{text}");
    assert!(text.contains("╰ Validation: all checks passed."), "{text}");
    assert!(!text.contains("ctrl+t") && !text.contains('⋮'), "{text}");

    // Narrow terminals wrap both the header and the report inside its gutter.
    let rows = std_tool_screen(&calls, 32, 2).await;
    assert!(
        rows.iter()
            .all(|r| unicode_width::UnicodeWidthStr::width(r.as_str()) <= 32)
    );
    let text = rows.join("\n");
    assert!(text.contains("│ Implemented"), "{text}");
    assert!(
        text.contains("Validation: all checks\n      ╰ passed."),
        "{text}"
    );

    // Failures keep the entire explanation even at the default detail level.
    let calls = [("subagent", json!({"name": ""}), report, true)];
    let text = std_tool_screen(&calls, 80, 0).await.join("\n");
    assert!(text.contains("✕ subagent · failed"), "{text}");
    assert!(text.contains("╰ Validation: all checks passed."), "{text}");
    assert!(!text.contains('⋮'), "{text}");
}

#[tokio::test]
async fn subagent_report_header_opens_the_running_session() {
    let mut h = Harness::build(None).await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.emit::<MessageCompleted>(tool_calls(
        "s-new",
        vec![ToolCall {
            id: "c1".into(),
            name: "subagent".into(),
            arguments: json!({"task": "Find callers", "name": "reviewer"}).to_string(),
        }],
    ))
    .await;
    h.emit::<SessionCreated>(bone_proto::types::SessionInfo {
        session_id: "child-1".into(),
        cwd: "/work".into(),
        created_at: 0,
        title: Some("Find callers".into()),
        parent: None,
        owner: Some(bone_proto::types::SessionOwner {
            session_id: "s-new".into(),
            call_id: Some("c1".into()),
            name: Some("reviewer".into()),
        }),
    })
    .await;
    let screen = h.screen(80, 24);
    let y = screen
        .lines()
        .position(|l| l.contains("Find callers · reviewer · running"))
        .unwrap_or_else(|| panic!("{screen}")) as u16;
    h.app.mouse("down", "left", (10, y));
    h.settle().await;
    assert_eq!(
        h.lua("=bone.chat.session().session_id").await,
        "\"child-1\""
    );
}

#[tokio::test]
async fn ctrl_t_steps_through_tool_detail() {
    let mut h = Harness::build(None).await;
    let get = |h: &mut Harness| -> String {
        h.app
            .with_api(|lua| lua.load("return bone.o.tool_detail").eval())
            .unwrap()
    };
    let mut seen = vec![get(&mut h)];
    for _ in 0..3 {
        h.input("{ctrl+t}").await;
        seen.push(get(&mut h));
    }
    assert_eq!(seen, ["summary", "rows", "full", "summary"]);
}

#[tokio::test]
async fn tool_summary_follows_a_running_turn() {
    let mut h = Harness::build(None).await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = |id: &str, name: &str, args: Value| ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args.to_string(),
    };
    let finish = |id: &str, output: &str| ToolFinishedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        call_id: id.into(),
        output: output.into(),
        is_error: false,
        duration_ms: None,
    };
    let chat = |h: &mut Harness| -> String {
        h.screen(60, 20)
            .lines()
            .filter(|l| l.starts_with("  ◌ ") || l.starts_with("    ") && !l.contains('›'))
            .collect::<Vec<_>>()
            .join("\n")
    };
    h.emit::<MessageCompleted>(tool_calls(
        "s-new",
        vec![call("a", "read_file", json!({"path": "x"}))],
    ))
    .await;
    assert_eq!(chat(&mut h), "  ◌ Read 1 file");
    h.emit::<ToolFinished>(finish("a", "1#AB|x")).await;
    assert_eq!(chat(&mut h), "    Read 1 file");
    // A later round joins the same line, drawn when it was first seen.
    h.emit::<MessageCompleted>(tool_calls(
        "s-new",
        vec![call("b", "shell", json!({"command": "ls"}))],
    ))
    .await;
    assert_eq!(chat(&mut h), "  ◌ Read 1 file, ran 1 shell command");
    h.emit::<ToolFinished>(finish("b", "x\n[exit code: 0]"))
        .await;
    assert_eq!(chat(&mut h), "    Read 1 file, ran 1 shell command");
}

#[tokio::test]
async fn tool_summary_follows_calls_finishing_out_of_order() {
    // Calls of one message run concurrently: the first may finish last,
    // and the summary (drawn by the first) must still notice.
    let mut h = Harness::build(None).await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = |id: &str| ToolCall {
        id: id.into(),
        name: "shell".into(),
        arguments: json!({"command": "ls"}).to_string(),
    };
    let finish = |id: &str, is_error: bool| ToolFinishedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        call_id: id.into(),
        output: "x".into(),
        is_error,
        duration_ms: None,
    };
    let summary = |h: &mut Harness| -> String {
        h.screen(60, 20)
            .lines()
            .find(|l| l.contains("shell command"))
            .unwrap_or_default()
            .to_owned()
    };
    h.emit::<MessageCompleted>(tool_calls("s-new", vec![call("a"), call("b"), call("c")]))
        .await;
    assert_eq!(summary(&mut h), "  ◌ Ran 3 shell commands");
    h.emit::<ToolFinished>(finish("c", false)).await;
    h.emit::<ToolFinished>(finish("b", true)).await;
    assert_eq!(summary(&mut h), "  ◌ Ran 3 shell commands, 1 failed");
    h.emit::<ToolFinished>(finish("a", false)).await;
    assert_eq!(summary(&mut h), "    Ran 3 shell commands, 1 failed");
}

#[tokio::test]
async fn views_redraw_when_chat_data_they_read_changes() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        renders = { user = 0, notice = 0 }
        -- The user line counts the tool calls after it: data from other items.
        bone.ui.views.user = function(item)
          renders.user = renders.user + 1
          local n = #bone.chat.items({ kind = "tool", from = item.index })
          return { { { "you: " .. item.text .. " [" .. n .. " tools]", "Normal" } } }
        end
        "#,
    )
    .await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let user_row = |h: &mut Harness| -> String {
        h.screen(60, 20)
            .lines()
            .find(|l| l.starts_with("you: "))
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(user_row(&mut h), "you: go [0 tools]");
    let call = |id: &str| ToolCall {
        id: id.into(),
        name: "shell".into(),
        arguments: json!({"command": "ls"}).to_string(),
    };
    h.emit::<MessageCompleted>(tool_calls("s-new", vec![call("a"), call("b")]))
        .await;
    assert_eq!(user_row(&mut h), "you: go [2 tools]");

    // Unrelated changes do not redraw a view whose reads did not change.
    let renders = |h: &mut Harness| -> i64 {
        h.app
            .with_api(|lua| lua.load("return renders.user").eval())
            .unwrap()
    };
    let before = renders(&mut h);
    h.screen(60, 20);
    assert_eq!(renders(&mut h), before);
}

#[tokio::test]
async fn views_can_ask_to_be_drawn_again() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        ticks = 0
        bone.ui.views.user = function(item)
          ticks = ticks + 1
          bone.chat.refresh_in(1)
          return { { { "tick " .. ticks, "Normal" } } }
        end
        "#,
    )
    .await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let screen = h.screen(40, 10);
    assert!(screen.contains("tick 1"), "{screen}");
    assert!(h.app.chat_expiry.is_some());
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let screen = h.screen(40, 10);
    assert!(screen.contains("tick 2"), "{screen}");

    // Outside a view it is an error; bone.chat.redraw works anywhere.
    let e = h
        .app
        .with_api(|lua| lua.load("bone.chat.refresh_in(10)").exec())
        .unwrap_err();
    assert!(
        e.to_string().contains("only works inside a chat view"),
        "{e}"
    );
    h.app
        .with_api(|lua| {
            lua.load("bone.ui.views.user = function(i) return { { { 'plain', 'Normal' } } } end")
                .exec()
        })
        .unwrap();
    assert!(h.screen(40, 10).contains("plain"));
    h.app
        .with_api(|lua| lua.load("bone.chat.redraw(1) bone.chat.redraw()").exec())
        .unwrap();
    assert!(h.screen(40, 10).contains("plain"));
    assert!(
        h.app
            .with_api(|lua| lua.load("return bone.now() > 0").eval::<bool>())
            .unwrap()
    );
}

#[tokio::test]
async fn lua_sees_clicks_on_chat_items_and_scrolls_the_chat() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        bone.ui.layout = { "chat" }
        bone.ui.views.user = function(item)
          local out = {}
          for i = 1, 12 do out[i] = { { "user line " .. i, "Normal" } } end
          return out
        end
        bone.ui.views.tool = function(item)
          return { { { "tool " .. item.index, "Normal" } } }
        end
        clicks = {}
        bone.on("mouse", function(ev)
          clicks[#clicks + 1] = ev
          return ev.button == "right" or ev.index == 2
        end)
        "#,
    )
    .await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = ToolCall {
        id: "a".into(),
        name: "shell".into(),
        arguments: json!({"command": "ls"}).to_string(),
    };
    h.emit::<MessageCompleted>(tool_calls("s-new", vec![call]))
        .await;
    let eval = |h: &mut Harness, code: &str| -> Value {
        let v: mlua::Value = h.app.with_api(|lua| lua.load(code).eval()).unwrap();
        serde_json::to_value(&v).unwrap()
    };

    // Following the end: the tool (item 2) is on the last row.
    let screen = h.screen(30, 6);
    assert!(screen.ends_with("tool 2"), "{screen}");
    let view = eval(&mut h, "return bone.chat.view()");
    assert_eq!(
        (view["rows"].clone(), view["follow"].clone()),
        (json!(13), json!(true))
    );
    assert_eq!(
        (view["first"].clone(), view["last"].clone()),
        (json!(1), json!(2))
    );
    assert_eq!(
        eval(&mut h, "return bone.chat.at(0, 5)"),
        json!({"index": 2, "line": 1})
    );
    assert_eq!(
        eval(&mut h, "return bone.chat.at(0, 4)"),
        json!({"index": 1, "line": 12})
    );

    // A click Lua takes does not start a text selection.
    h.app.mouse("down", "left", (0, 5));
    assert!(h.app.selection.is_none());
    assert_eq!(
        eval(
            &mut h,
            "local c = clicks[1] return { c.action, c.button, c.index, c.line, c.y }"
        ),
        json!(["down", "left", 2, 1, 5])
    );
    // One it leaves (the user item) selects text as before.
    h.app.mouse("down", "left", (0, 1));
    assert!(h.app.selection.is_some());
    h.app.mouse("up", "left", (0, 1));

    // Scrolling from Lua.
    assert_eq!(eval(&mut h, "return bone.chat.scroll_to(1)"), json!(true));
    let screen = h.screen(30, 6);
    assert!(screen.starts_with("user line 1\n"), "{screen}");
    assert_eq!(eval(&mut h, "return bone.chat.view().follow"), json!(false));
    h.lua("bone.chat.scroll(3)").await;
    assert!(h.screen(30, 6).starts_with("user line 4\n"));
    h.lua("bone.chat.scroll('bottom')").await;
    assert!(h.screen(30, 6).ends_with("tool 2"));
    assert_eq!(eval(&mut h, "return bone.chat.scroll_to(9)"), json!(false));
}

#[tokio::test]
async fn regions_see_the_chat_back_at_its_end() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        bone.ui.layout = { "chat", "gap" }
        bone.ui.regions.gap = { size = 1, render = function()
          return bone.chat.view().follow and {} or { "END" }
        end }
        bone.ui.views.user = function(item)
          local out = {}
          for i = 1, 12 do out[i] = { { "user line " .. i, "Normal" } } end
          return out
        end
        "#,
    )
    .await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    assert!(!h.screen(30, 6).contains("END"));
    h.lua("bone.chat.scroll(-3)").await;
    assert!(h.screen(30, 6).contains("END"));
    // Scrolling back down by rows clears the marker in the first frame.
    h.lua("bone.chat.scroll(3)").await;
    h.app.redraw = false;
    let screen = h.screen(30, 6);
    assert!(!screen.contains("END"), "{screen}");
    // Further wheel events at the bottom must not flash it again.
    for _ in 0..3 {
        h.input("{wheeldown}").await;
        let screen = h.screen(30, 6);
        assert!(!screen.contains("END"), "{screen}");
    }
    // Settled: no more extra frames.
    h.app.redraw = false;
    h.screen(30, 6);
    assert!(!h.app.redraw);
}

#[tokio::test]
async fn lua_adds_its_own_items_to_the_chat() {
    let (mut h, _dir) = Harness::with_config(
        r#"
        bone.ui.layout = { "chat" }
        bone.ui.views.user = function(item) return { { { "> " .. item.text, "Normal" } } } end
        bone.ui.views.assistant = function(item) return { { { item.text, "Normal" } } } end
        bone.ui.views.build = function(item, ctx)
          return { { { (item.ok and "✓ " or "… ") .. item.text .. " after " .. (ctx.prev and ctx.prev.kind or "-"), "Normal" } } }
        end
        "#,
    )
    .await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let eval = |h: &mut Harness, code: &str| -> Value {
        let v: mlua::Value = h.app.with_api(|lua| lua.load(code).eval()).unwrap();
        serde_json::to_value(&v).unwrap()
    };
    let id = eval(
        &mut h,
        r#"return bone.chat.add("build", { text = "building" })"#,
    );
    assert_eq!(id, json!("build-1"));
    // Streaming after it does not split the answer or move the item.
    for text in ["hel", "lo"] {
        h.emit::<MessageDelta>(MessageDeltaParams {
            session_id: "s-new".into(),
            turn_id: 1,
            kind: DeltaKind::Text,
            text: text.into(),
        })
        .await;
    }
    assert_eq!(h.screen(40, 5), "> go\n… building after user\nhello\n\n");
    h.emit::<MessageCompleted>(MessageCompletedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: "hello".into(),
            reasoning: String::new(),
            tool_calls: vec![],
        },
        usage: None,
    })
    .await;
    assert_eq!(h.screen(40, 5), "> go\n… building after user\nhello\n\n");

    // Updating redraws it; it is data like any other item.
    assert_eq!(
        eval(
            &mut h,
            r#"return bone.chat.update("build-1", { ok = true, text = "built" })"#
        ),
        json!(true)
    );
    assert!(h.screen(40, 5).contains("✓ built after user"));
    assert_eq!(
        eval(
            &mut h,
            r#"local i = bone.chat.items({ kind = "build" })[1] return { i.id, i.kind, i.index, i.ok }"#
        ),
        json!(["build-1", "build", 2, true])
    );

    // Removed, it is gone; bad kinds are refused.
    assert_eq!(
        eval(&mut h, r#"return bone.chat.remove("build-1")"#),
        json!(true)
    );
    assert_eq!(h.screen(40, 5), "> go\nhello\n\n\n");
    assert_eq!(
        eval(&mut h, r#"return bone.chat.update("build-1", {})"#),
        json!(false)
    );
    let e = h
        .app
        .with_api(|lua| lua.load(r#"bone.chat.add("tool", {})"#).exec())
        .unwrap_err();
    assert!(e.to_string().contains("not a built-in kind"), "{e}");
    // Without a view, its text shows.
    eval(
        &mut h,
        r#"return bone.chat.add("note", { text = "plain note" })"#,
    );
    assert!(h.screen(40, 5).contains("plain note"));
}

#[tokio::test]
async fn layout_nests_rows_and_columns() {
    let mut h = Harness::blank().await;
    h.lua(
        r#"
        bone.ui.views.user = function(item) return { { { "> " .. item.text, "Normal" } } } end
        bone.ui.statusline = function() return { "STATUS" } end
        bone.ui.regions.files = function(ctx) return { "files " .. ctx.width .. "x" .. ctx.height } end
        bone.ui.regions.notes = { size = 3, render = function(ctx) return { "N" .. ctx.width } end }
        bone.ui.layout = {
          "statusline",
          { cols = {
              { "files", size = "25%" },
              { rows = { "chat", "notes" } },
              { "message", size = 6 },
            }, sep = "|" },
          { "prompt", size = 1 },
        }
        "#,
    )
    .await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    h.lua(r#"bone.notify("hi")"#).await;
    let screen = h.screen(41, 8);
    let rows: Vec<&str> = screen.split('\n').collect();
    // 41 columns: files 10 (25% of them), message 6, 2 separators, chat 23.
    assert_eq!(rows[0], "STATUS", "{screen}");
    assert_eq!(rows[1], "files 10x6|> go                   |hi", "{screen}");
    assert!(rows[4].starts_with("          |N23"), "{screen}");

    // A region set to fill shares the space with the chat.
    h.lua(r#"bone.ui.layout = { { cols = { "chat", { "files", size = "fill" } } } }"#)
        .await;
    let screen = h.screen(40, 4);
    // (The message is outside this layout, so it takes the bottom row.)
    assert!(
        screen.starts_with("> go                files 20x3"),
        "{screen}"
    );
    assert!(screen.ends_with("hi"), "{screen}");

    // A bad size says what is wrong, and the default layout is kept.
    h.lua(r#"bone.ui.layout = { { "chat", size = "lots" } }"#)
        .await;
    let screen = h.screen(40, 12);
    assert!(screen.contains("> go"), "{screen}");
    assert!(
        h.app.log.iter().any(|l| l.contains("bad size \"lots\"")),
        "{:?}",
        h.app.log
    );
}

#[tokio::test]
async fn lua_styles_the_prompt_text_and_shows_ghost_text() {
    let mut h = Harness::blank().await;
    h.lua(
        r##"
        bone.hl.set("Cmd", { fg = "#ff0000" })
        bone.hl.set("Ghost", { fg = "#00ff00" })
        bone.ui.layout = { "chat", "prompt" }
        bone.ui.prompt_highlight = function(ctx)
          local out = { highlights = {} }
          local s, e = ctx.text:find("^/%w+")
          if s then
            out.highlights[1] = { row = 0, from = s - 1, to = e, hl = "Cmd" }
          end
          if ctx.text == "/he" then out.ghost = "lp"; out.ghost_hl = "Ghost" end
          return out
        end
        "##,
    )
    .await;
    h.input("/he").await;
    let (w, rows) = (20u16, 3u16);
    let draw = |h: &mut Harness| {
        let mut term = Terminal::new(TestBackend::new(w, rows)).unwrap();
        term.draw(|f| render::draw(f, &mut h.app)).unwrap();
        term.backend().buffer().clone()
    };
    let buf = draw(&mut h);
    let y = rows - 1;
    let row: String = (0..w).map(|x| buf[(x, y)].symbol().to_owned()).collect();
    assert_eq!(row.trim_end(), "/help");
    let red = ratatui::style::Color::Rgb(255, 0, 0);
    let green = ratatui::style::Color::Rgb(0, 255, 0);
    assert_eq!([buf[(0, y)].fg, buf[(2, y)].fg], [red, red]);
    assert_eq!([buf[(3, y)].fg, buf[(4, y)].fg], [green, green]);
    // Typing on past the ghost drops it; the selection wins over marks.
    h.input(" x").await;
    let buf = draw(&mut h);
    let row: String = (0..w).map(|x| buf[(x, y)].symbol().to_owned()).collect();
    assert_eq!(row.trim_end(), "/he x");
    assert_eq!(buf[(1, y)].fg, red);
    h.app
        .with_api(|lua| {
            lua.load("bone.prompt.select({ row = 0, col = 0 }, { row = 0, col = 2 })")
                .exec()
        })
        .unwrap();
    let buf = draw(&mut h);
    assert_ne!(buf[(1, y)].fg, red);
    assert_eq!(buf[(2, y)].fg, red);
}

#[tokio::test]
async fn tool_items_carry_live_output_timing_and_usage() {
    let mut h = Harness::build(None).await;
    h.input("{ctrl+t}").await; // rows: one call per row
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = ToolCall {
        id: "c".into(),
        name: "shell".into(),
        arguments: json!({"command": "make"}).to_string(),
    };
    h.emit::<MessageCompleted>(MessageCompletedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        message: ChatMessage::Assistant {
            content: String::new(),
            reasoning: String::new(),
            tool_calls: vec![call.clone()],
        },
        usage: Some(Usage {
            input_tokens: 1200,
            output_tokens: 30,
            context_tokens: None,
            cached_tokens: None,
        }),
    })
    .await;
    h.emit::<ToolStarted>(ToolStartedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        call,
        started_at: Some(1_000),
    })
    .await;
    for text in ["compiling a\n", "compiling b\n"] {
        h.emit::<ToolOutput>(ToolOutputParams {
            session_id: "s-new".into(),
            turn_id: 1,
            call_id: "c".into(),
            text: text.into(),
        })
        .await;
    }
    let screen = h.screen(50, 20);
    assert!(
        screen.contains("  ◌ shell make\n      │ compiling a\n      ╰ compiling b"),
        "{screen}"
    );
    let item = |h: &mut Harness| -> Value {
        let v: mlua::Value = h
            .app
            .with_api(|lua| {
                lua.load(r#"return bone.chat.items({ kind = "tool", full = true })[1]"#)
                    .eval()
            })
            .unwrap();
        serde_json::to_value(&v).unwrap()
    };
    let i = item(&mut h);
    assert_eq!(i["live"], json!("compiling a\ncompiling b\n"));
    // Without `full`, the costly fields stay out.
    let brief: bool = h
        .app
        .with_api(|lua| {
            lua.load(r#"local i = bone.chat.items({ kind = "tool" })[1]
                return i.live == nil and i.output == nil and i.raw_arguments == nil and i.name == "shell""#)
                .eval()
        })
        .unwrap();
    assert!(brief);
    assert_eq!(i["started_at"], json!(1000));
    assert_eq!(i["usage"], json!({"input": 1200, "output": 30}));
    h.emit::<ToolFinished>(ToolFinishedParams {
        session_id: "s-new".into(),
        turn_id: 1,
        call_id: "c".into(),
        output: "compiling a\ncompiling b\n[exit code: 0]".into(),
        is_error: false,
        duration_ms: Some(1500),
    })
    .await;
    assert_eq!(item(&mut h)["duration_ms"], json!(1500));
    assert!(
        h.screen(50, 20)
            .contains("    shell make\n      │ compiling a\n      ╰ compiling b")
    );
}

fn process(state: ProcessState, tail: &str) -> ProcessSnapshot {
    ProcessSnapshot {
        session_id: "s-new".into(),
        id: "shell-1".into(),
        command: "npm run dev".into(),
        state,
        running: state == ProcessState::Running,
        pid: Some(42),
        started_at_ms: 1_790_000_000_000,
        finished_at_ms: (state != ProcessState::Running).then_some(1_790_000_041_000),
        elapsed_ms: 3000,
        tail: tail.into(),
        output_bytes: 18,
        truncated: false,
        code: (state == ProcessState::Exited).then_some(0),
        signal: None,
        error: None,
        terminal: true,
    }
}

fn running_shell() -> ProcessChangedParams {
    ProcessChangedParams {
        session_id: "s-new".into(),
        version: 1,
        process: process(ProcessState::Running, "listening on 3000"),
        chunk: None,
    }
}

#[tokio::test]
async fn tray_shells_belong_to_the_chat_on_screen() {
    let mut h = Harness::build(None).await;
    h.input("hi{enter}").await;
    h.emit::<ProcessChanged>(running_shell()).await;
    assert!(h.screen(100, 20).contains("Shells 1"));
    // A new chat has no shells, and the other chat's output doesn't take it over.
    h.input("/new{enter}").await;
    h.emit::<ProcessChanged>(running_shell()).await;
    let screen = h.screen(100, 20);
    assert!(!screen.contains("Shells"), "{screen}");
}

#[tokio::test]
async fn sidebar_processes_page_lists_every_chats_background_work() {
    let mut h = Harness::build(None).await;
    h.input("hi{enter}").await;
    h.emit::<ProcessChanged>(running_shell()).await;
    h.emit::<SessionCreated>(bone_proto::types::SessionInfo {
        session_id: "child-1".into(),
        cwd: "/work".into(),
        created_at: 0,
        title: Some("find callers".into()),
        parent: None,
        owner: Some(bone_proto::types::SessionOwner {
            session_id: "s-new".into(),
            call_id: Some("c1".into()),
            name: Some("reviewer".into()),
        }),
    })
    .await;
    h.input("/new{enter}").await;
    h.input("{ctrl+o}").await;
    h.emit::<TurnStarted>(started("child-1", "find callers"))
        .await;
    let screen = h.screen(100, 30);
    assert!(screen.contains("Processes 2"), "{screen}");
    h.input("{tab}").await;
    let screen = h.screen(100, 30);
    assert!(
        screen.contains("$ npm run dev") && screen.contains("reviewer · find callers"),
        "{screen}"
    );
    // x stops the selected one: the shell, the older of the two.
    h.input("x").await;
    assert_eq!(
        h.requests("process/cancel"),
        vec![json!({ "session_id": "s-new", "id": "shell-1" })]
    );
    h.emit::<TurnFinished>(finished("child-1", TurnOutcome::Completed))
        .await;
    let screen = h.screen(100, 30);
    assert!(!screen.contains("reviewer"), "{screen}");
    h.input("{tab}").await;
    assert!(h.screen(100, 30).contains("Search conversations"));
}

#[tokio::test]
async fn shell_jobs_show_in_the_tray_and_open_in_place() {
    let mut h = Harness::build(None).await;
    h.input("hi{enter}").await;
    h.emit::<ProcessChanged>(ProcessChangedParams {
        session_id: "s-new".into(),
        version: 1,
        process: process(ProcessState::Running, "listening on 3000"),
        chunk: Some(ProcessChunk {
            offset: 0,
            data: "\u{1b}[32mlistening\u{1b}[0m on 3000\r\n".into(),
        }),
    })
    .await;
    let screen = h.screen(100, 20);
    assert!(screen.contains("Shells 1"), "{screen}");

    // Down on the empty prompt moves into the tray; enter opens the job's
    // terminal right there, which reads the output and shows it.
    h.input("{down}review").await;
    assert_eq!(h.prompt(), "review");
    h.input("{ctrl+u}{down}").await;
    h.app.windows.get_mut(&crate::app::CHAT_WIN).unwrap().top = 5;
    h.input("{shift+up}").await;
    assert_eq!(h.app.windows[&crate::app::CHAT_WIN].top, 2);
    h.input("{shift+down}{enter}").await;
    assert_eq!(h.app.windows[&crate::app::CHAT_WIN].top, 5);
    h.screen(100, 20);
    h.settle().await;
    assert_eq!(h.requests("process/read").len(), 1);
    let screen = h.screen(100, 20);
    assert!(screen.contains("esc close"), "{screen}");
    assert!(screen.contains("\n   listening on 3000"), "{screen}");
    // The running process's terminal gets the tray's width.
    let resize = h.requests("process/resize");
    assert_eq!(resize.last().unwrap()["cols"], json!(97));
    // Later output arrives as chunks.
    h.emit::<ProcessChanged>(ProcessChangedParams {
        session_id: "s-new".into(),
        version: 2,
        process: process(ProcessState::Exited, "bye"),
        chunk: Some(ProcessChunk {
            offset: 28,
            data: "bye\r\n".into(),
        }),
    })
    .await;
    let screen = h.screen(100, 20);
    assert!(screen.contains("\n   bye"), "{screen}");
    assert!(screen.contains("✓ $ npm run dev  exit 0"), "{screen}");
    // Esc closes the terminal, esc again gives the prompt the keyboard.
    h.input("{esc}").await;
    assert!(!h.screen(100, 20).contains("esc close"));
    h.input("{esc}").await;
    assert_eq!(h.lua("=bone.keymap.current()").await, "\"main\"");
    // The finished row stays until the next message.
    assert!(h.screen(100, 20).contains("$ npm run dev"));
    h.input("next{enter}").await;
    assert!(!h.screen(100, 20).contains("$ npm run dev"));
    assert_eq!(h.lua("=#require('bone.ui.tray').rows('shells')").await, "0");
}

#[tokio::test]
async fn terminal_resize_retries_after_a_startup_failure() {
    let mut h = Harness::build(None).await;
    h.input("hi{enter}").await;
    h.emit::<ProcessChanged>(ProcessChangedParams {
        session_id: "s-new".into(),
        version: 1,
        process: process(ProcessState::Running, "listening on 3000"),
        chunk: None,
    })
    .await;
    h.input("{down}{enter}").await;

    *h.fail.lock().unwrap() = Some("process/resize".into());
    h.screen(100, 20);
    h.settle().await;
    assert_eq!(h.requests("process/resize").len(), 1);

    *h.fail.lock().unwrap() = None;
    h.screen(100, 20);
    h.settle().await;
    assert_eq!(h.requests("process/resize").len(), 2);
}

#[tokio::test]
async fn subagents_show_in_the_tray_and_open_on_click() {
    let mut h = Harness::build(None).await;
    h.input("hi{enter}").await;
    h.emit::<SessionCreated>(bone_proto::types::SessionInfo {
        session_id: "child-1".into(),
        cwd: "/work".into(),
        created_at: 0,
        title: Some("find callers".into()),
        parent: None,
        owner: Some(bone_proto::types::SessionOwner {
            session_id: "s-new".into(),
            call_id: Some("c1".into()),
            name: Some("reviewer".into()),
        }),
    })
    .await;
    h.emit::<TurnStarted>(started("child-1", "look around"))
        .await;
    h.emit::<ToolStarted>(ToolStartedParams {
        session_id: "child-1".into(),
        turn_id: 1,
        call: ToolCall {
            id: "x".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"/work/src/panel.rs"}"#.into(),
        },
        started_at: None,
    })
    .await;
    let screen = h.screen(100, 20);
    assert!(screen.contains("Agents 1"), "{screen}");
    assert!(
        screen.contains("reviewer  find callers  read_file src/panel.rs"),
        "{screen}"
    );

    // Typing from the tray returns intact to the prompt.
    h.input("{down}send").await;
    assert_eq!(h.prompt(), "send");
    h.input("{ctrl+u}{down}draft").await;
    assert_eq!(h.prompt(), "draft");

    // A click on the row opens the sub-agent's session, with a way back.
    let y = screen
        .lines()
        .position(|l| l.contains("find callers"))
        .unwrap() as u16;
    h.app.mouse("down", "left", (10, y));
    h.settle().await;
    assert!(
        h.requests("session/messages")
            .iter()
            .any(|p| p["session_id"] == "child-1")
    );
    let screen = h.screen(100, 20);
    assert!(screen.contains(" ‹ main │ Agents 1"), "{screen}");
    // Esc on an empty prompt goes back to the session that started it.
    h.input("{ctrl+u}{esc}").await;
    assert_eq!(h.lua("=bone.chat.session().session_id").await, "\"s-new\"");

    // Finished: a check mark, until the next message.
    h.emit::<TurnFinished>(TurnFinishedParams {
        session_id: "child-1".into(),
        turn_id: 1,
        outcome: TurnOutcome::Completed,
    })
    .await;
    let screen = h.screen(100, 20);
    assert!(
        screen.contains("✓ reviewer  find callers  done"),
        "{screen}"
    );
    // ctrl+b folds the tray; with nothing running it is gone.
    h.input("{ctrl+b}").await;
    assert!(!h.screen(100, 20).contains("find callers  done"));
}

#[tokio::test]
async fn spinner_selection_and_title_are_lua() {
    let mut h = Harness::blank().await;
    h.lua(
        r#"
        bone.ui.spinner = { frames = { "A", "B" }, interval = 50 }
        bone.ui.statusline = function(ctx) return { "[" .. ctx.spinner .. "]" } end
        bone.ui.layout = { "chat", "statusline" }
        bone.ui.title = function(ctx) return "bone: " .. ctx.title end
        picked = nil
        bone.on("select", function(ev) picked = ev.text return ev.text:find("keep") ~= nil end)
        "#,
    )
    .await;
    let screen = h.screen(20, 3);
    assert!(
        screen.ends_with("[A]") || screen.ends_with("[B]"),
        "{screen}"
    );
    assert_eq!(h.app.spinner.interval_ms, 50);
    assert_eq!(h.app.ui_title().as_deref(), Some("bone: [new session]"));
    h.lua("bone.ui.spinner = nil").await;
    h.screen(20, 3);
    assert_eq!(h.app.spinner.frames.len(), 10);
    h.lua("bone.ui.spinner = { frames = {} }").await;
    h.screen(20, 3);
    assert!(
        h.app
            .log
            .iter()
            .any(|l| l.contains("bone.ui.spinner needs frames")),
        "{:?}",
        h.app.log
    );

    // A selection: the handler sees its text; returning true keeps it from
    // the clipboard, anything else lets it through.
    let select = |h: &mut Harness| -> (Option<String>, Option<String>) {
        h.screen(20, 3);
        h.app.mouse("down", "left", (0, 2));
        h.app.mouse("drag", "left", (8, 2));
        h.app.mouse("up", "left", (8, 2));
        h.app.clipboard = None;
        h.screen(20, 3);
        let picked: Option<String> = h
            .app
            .with_api(|lua| lua.load("return picked").eval())
            .unwrap();
        (picked, h.app.clipboard.take())
    };
    h.lua(r#"bone.notify("keep this")"#).await;
    assert_eq!(select(&mut h), (Some("keep this".into()), None));
    h.lua(r#"bone.notify("other one")"#).await;
    assert_eq!(
        select(&mut h),
        (Some("other one".into()), Some("other one".into()))
    );
}

#[tokio::test]
async fn reload_starts_a_fresh_lua_state() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    let marker = dir.path().join("shut-down");
    std::fs::write(
        &tui,
        format!(
            r##"
            bone.keymap.set("f5", "/hello")
            bone.cmd.create("hello", function() bone.notify("hi") end)
            bone.on("submit", function() return false end)
            bone.hl.set("UserMessage", {{ fg = "#123456" }})
            fresh = (fresh or 0) + 1
            bone.plugin.on_shutdown(function()
              local f = io.open({marker:?}, "w") f:write("x") f:close()
            end)
            "##
        ),
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    let eval = |h: &mut Harness, code: &str| -> String {
        h.app.with_api(|lua| lua.load(code).eval()).unwrap()
    };
    assert_eq!(
        eval(&mut h, "return tostring(bone.cmd.find('hello') ~= nil)"),
        "true"
    );
    let tweaked = h.app.theme.hl("UserMessage");

    // Deleting every line takes all of it away, not just views.
    std::fs::write(&tui, "").unwrap();
    h.app.reload_user_config();
    assert!(marker.exists(), "shutdown hooks ran");
    assert_eq!(
        eval(&mut h, "return tostring(fresh)"),
        "nil",
        "a new Lua state"
    );
    assert_eq!(
        eval(&mut h, "return tostring(bone.cmd.find('hello') ~= nil)"),
        "false"
    );
    assert_ne!(h.app.theme.hl("UserMessage"), tweaked);
    h.input("{f5}").await;
    assert!(h.prompt().is_empty(), "f5 is unmapped");
    // The submit handler is gone too: messages go out again.
    h.input("go{enter}").await;
    assert_eq!(h.requests("turn/start").len(), 1);
    // The defaults are back.
    assert_eq!(eval(&mut h, "return type(bone.ui.views.user)"), "function");
}

#[tokio::test]
async fn runtime_overrides_can_extend_the_builtin_and_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let ui = dir.path().join("runtime/lua/bone/ui");
    std::fs::create_dir_all(&ui).unwrap();
    // Start from the built-in layout, change one thing.
    std::fs::write(
        ui.join("layout.lua"),
        r#"
        local M = bone.builtin("bone.ui.layout")
        local setup = M.setup
        function M.setup(opts)
          setup(opts)
          bone.ui.regions.top = function() return { "MINE" } end
        end
        return M
        "#,
    )
    .unwrap();
    // A plain copy of a built-in file.
    std::fs::write(
        ui.join("statusline.lua"),
        bone_lua::builtin_source("lua/bone/ui/statusline.lua").unwrap(),
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    let screen = h.screen(40, 10);
    assert!(screen.contains("MINE"), "{screen}");
    // The rest of the built-in layout still applies: the three-row prompt.
    assert!(screen.contains(" › Message bone"), "{screen}");

    let health = serde_json::to_string(&h.app.tui_health()).unwrap();
    assert!(
        health.contains("lua/bone/ui/layout.lua (differs from built-in)"),
        "{health}"
    );
    assert!(
        health.contains("lua/bone/ui/statusline.lua (same as built-in)"),
        "{health}"
    );
    h.app.note_runtime_overrides();
    assert!(
        h.message().contains("2 runtime files overridden"),
        "{}",
        h.message()
    );
    let e = h
        .app
        .with_api(|lua| lua.load(r#"bone.builtin("bone.nope")"#).exec())
        .unwrap_err();
    assert!(e.to_string().contains("no built-in runtime file"), "{e}");
}

#[tokio::test]
async fn chat_items_around_an_item() {
    let mut h = Harness::build(None).await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let call = |id: &str| ToolCall {
        id: id.into(),
        name: "shell".into(),
        arguments: json!({"command": "ls"}).to_string(),
    };
    h.emit::<MessageCompleted>(tool_calls("s-new", vec![call("a"), call("b")]))
        .await;
    h.lua(r#"bone.chat.add("note", { text = "between" })"#)
        .await;
    h.emit::<MessageCompleted>(tool_calls("s-new", vec![call("c")]))
        .await;
    let ids = |h: &mut Harness, q: &str| -> Value {
        let v: mlua::Value = h
            .app
            .with_api(|lua| {
                lua.load(format!(
                    "local out = {{}} for _, i in ipairs(bone.chat.items({q})) do out[#out + 1] = i.id or i.kind end return out"
                ))
                .eval()
            })
            .unwrap();
        serde_json::to_value(&v).unwrap()
    };
    // Items: user, a, b, note, c. The note breaks the stretch of tools.
    assert_eq!(
        ids(&mut h, r#"{ around = 2, kind = "tool" }"#),
        json!(["a", "b"])
    );
    assert_eq!(
        ids(&mut h, r#"{ around = 5, kind = "tool" }"#),
        json!(["c"])
    );
    assert_eq!(
        ids(&mut h, r#"{ around = 5, kind = { "tool", "note" } }"#),
        json!(["a", "b", "note-1", "c"])
    );
    // Around an item not of `kind`: nothing (an empty Lua table).
    assert_eq!(ids(&mut h, r#"{ around = 1, kind = "tool" }"#), json!({}));
}

#[tokio::test]
async fn left_and_right_are_columns_of_the_default_layout() {
    let mut h = Harness::blank().await;
    h.lua(
        r#"
        bone.ui.views.user = function(item) return { { { "> " .. item.text, "Normal" } } } end
        bone.ui.regions.left = { size = 6, render = function() return { "LEFT" } end }
        "#,
    )
    .await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    // Only a separator between the two shown columns, none for `right`.
    let screen = h.screen(30, 4);
    assert!(screen.starts_with("LEFT  │> go\n"), "{screen}");
}

#[tokio::test]
async fn runtime_command_lists_and_resets_copies() {
    let dir = tempfile::tempdir().unwrap();
    let ui = dir.path().join("runtime/lua/bone/ui");
    std::fs::create_dir_all(&ui).unwrap();
    std::fs::write(ui.join("views.lua"), "-- my views\\n").unwrap();
    std::fs::write(
        ui.join("statusline.lua"),
        bone_lua::builtin_source("lua/bone/ui/statusline.lua").unwrap(),
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();

    h.input("/runtime{enter}").await;
    let m = h.message();
    assert!(m.contains("lua/bone/ui/views.lua  (changed)"), "{m}");
    assert!(
        m.contains("lua/bone/ui/statusline.lua  (same as built-in)"),
        "{m}"
    );

    // Tab completion offers the copies after `reset`.
    let names: Vec<String> = h
        .app
        .with_api(|lua| {
            lua.load(
                r#"local out = {}
                   for _, c in ipairs(bone.cmd.complete("runtime", { args = "reset ", argv = { "reset" } }) or {}) do
                     out[#out + 1] = type(c) == "table" and c.value or c
                   end
                   return out"#,
            )
            .eval()
        })
        .unwrap();
    assert!(
        names.contains(&"lua/bone/ui/views.lua".to_owned()),
        "{names:?}"
    );
    assert!(names.contains(&"all".to_owned()), "{names:?}");

    // One file: moved to a backup, the built-in is used again.
    h.input("/runtime reset lua/bone/ui/views.lua{enter}").await;
    let m = h.message();
    assert!(
        m.starts_with("Reset 1 file to the built-in version."),
        "{m}"
    );
    assert!(!ui.join("views.lua").exists());
    let backups: Vec<_> = std::fs::read_dir(dir.path().join("runtime-backup"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1);
    let saved = backups[0]
        .as_ref()
        .unwrap()
        .path()
        .join("lua/bone/ui/views.lua");
    assert_eq!(std::fs::read_to_string(saved).unwrap(), "-- my views\\n");

    // Bad names are refused; `all` takes the rest.
    h.input("/runtime reset nope.lua{enter}").await;
    assert!(
        h.message()
            .contains("nope.lua is not a built-in runtime file"),
        "{}",
        h.message()
    );
    h.input("/runtime reset lua/bone/ui/views.lua{enter}").await;
    assert!(h.message().contains("is not overridden"), "{}", h.message());
    h.input("/runtime reset all{enter}").await;
    assert!(h.message().starts_with("Reset 1 file"), "{}", h.message());
    assert!(!ui.join("statusline.lua").exists());
    h.input("/runtime{enter}").await;
    assert!(
        h.message().starts_with("No runtime files are overridden"),
        "{}",
        h.message()
    );
}

#[tokio::test]
async fn the_command_menu_rests_on_the_prompt_with_few_matches() {
    let mut h = Harness::build(None).await;
    h.input("/qu").await;
    let screen = h.screen(60, 20);
    let rows: Vec<&str> = screen.split('\n').collect();
    let prompt = rows
        .iter()
        .position(|r| r.trim_end().ends_with("› /qu"))
        .expect(&screen);
    // Suggestions are part of the composer and sit above the prompt text;
    // there is no floating box edge between them.
    let queue = rows
        .iter()
        .position(|r| r.contains("/queue"))
        .expect(&screen);
    let quit = rows
        .iter()
        .position(|r| r.contains("/quit"))
        .expect(&screen);
    assert!(quit < queue && queue < prompt, "{screen}");
    assert!(
        !rows[..prompt]
            .iter()
            .any(|r| r.starts_with('╭') || r.starts_with('╰')),
        "{screen}"
    );
}

#[tokio::test]
async fn health_shows_the_last_lua_error_in_full() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tui.lua"),
        "local function boom() error('kaboom') end\nbone.on('submit', function() boom() end)",
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    h.input("x{enter}").await;
    let health = serde_json::to_string(&h.app.tui_health()).unwrap();
    assert!(
        health.contains("last error:") && health.contains("kaboom"),
        "{health}"
    );
    assert!(
        health.contains("traceback"),
        "the full error, not one line: {health}"
    );
    h.app.reload_user_config();
    let health = serde_json::to_string(&h.app.tui_health()).unwrap();
    assert!(!health.contains("kaboom"), "{health}");
}

#[tokio::test]
async fn saved_settings_apply_before_tui_lua_and_keys_save_them() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tui.lua"),
        r#"bone.o.prompt_max_height = 4"#,
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.settings = json!({ "tui": {
        "tool_detail": "full", "show_reasoning": true, "prompt_max_height": 7, "nope": 1 } });
    h.app.load_user_config();
    assert_eq!(h.lua("=bone.o.tool_detail").await, "\"full\"");
    assert!(h.app.options.show_reasoning);
    // tui.lua runs last and wins.
    assert_eq!(h.app.options.prompt_max_height, 4);
    // An unknown saved option is reported, not fatal.
    assert!(
        h.app.lua_errors.iter().any(|e| e.contains("tui.nope")),
        "{:?}",
        h.app.lua_errors
    );
    assert_eq!(
        h.lua("=bone.settings.get('tui.tool_detail')").await,
        "\"full\""
    );

    // ctrl+t saves the new choice through the core.
    h.input("{ctrl+t}").await;
    let req = h.requests("settings/set");
    assert_eq!(
        req.last().unwrap(),
        &json!({ "path": "tui.tool_detail", "value": "summary" })
    );

    // A change from anywhere (another client) applies now and is an event.
    h.lua("seen = nil bone.on('settings/changed', function(ev) seen = ev.path end)")
        .await;
    h.emit::<SettingsChanged>(SettingsChangedParams {
        path: "tui.tool_detail".into(),
        value: json!("rows"),
        settings: json!({ "tui": { "tool_detail": "rows" } }),
    })
    .await;
    assert_eq!(h.lua("=bone.o.tool_detail").await, "\"rows\"");
    assert_eq!(h.lua("=seen").await, "\"tui.tool_detail\"");
    assert_eq!(
        h.lua("=bone.settings.get('tui.tool_detail')").await,
        "\"rows\""
    );
}

#[tokio::test]
async fn config_page_sets_options_providers_plugins_and_plugin_settings() {
    let dir = tempfile::tempdir().unwrap();
    let webby = dir.path().join("plugins/webby");
    std::fs::create_dir_all(&webby).unwrap();
    std::fs::write(
        webby.join("manifest.json"),
        r#"{ "title": "Webby", "settings": [
             { "key": "n", "label": "Results", "type": "integer", "default": 5, "min": 1, "max": 10 } ] }"#,
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    h.lua("for i = 1, 12 do bone.o.define('test_' .. i, i) end")
        .await;
    h.input("/config{enter}").await;
    let screen = h.screen(100, 34);
    let top = |screen: &str| screen.lines().position(|l| l.contains("── Settings"));
    let config_top = top(&screen);
    assert!(screen.contains("1–10 of"), "{screen}");
    for _ in 0..9 {
        h.input("{down}").await;
    }
    assert!(h.screen(100, 34).contains("1–10 of"));
    h.input("{down}").await;
    assert!(h.screen(100, 34).contains("2–11 of"));
    for _ in 0..10 {
        h.input("{up}").await;
    }
    for tab in [" General ", " Providers ", " Plugins ", " Webby"] {
        assert!(screen.contains(tab), "{tab} in {screen}");
    }
    let last_set = |h: &Harness| {
        h.requests("settings/set")
            .last()
            .cloned()
            .unwrap_or_default()
    };
    // Move to the row whose label starts with `label`.
    async fn select(h: &mut Harness, label: &str) {
        for _ in 0..30 {
            if h.screen(100, 34).contains(&format!("› {label}")) {
                return;
            }
            h.input("{down}").await;
        }
        panic!("no row {label}: {}", h.screen(100, 34));
    }

    // General: a boolean toggles, a choice cycles, a number is typed.
    select(&mut h, "show reasoning").await;
    h.input("{enter}").await;
    assert!(h.app.options.show_reasoning);
    assert_eq!(
        last_set(&h),
        json!({ "path": "tui.show_reasoning", "value": true })
    );
    select(&mut h, "tool detail").await;
    h.input("{enter}").await;
    assert_eq!(
        last_set(&h),
        json!({ "path": "tui.tool_detail", "value": "rows" })
    );
    for _ in 0..30 {
        h.input("{up}").await;
    }
    select(&mut h, "prompt max height").await;
    h.input("{enter}{ctrl+u}x{enter}").await;
    assert!(h.screen(100, 34).contains("must be a whole number"));
    h.input("{enter}{ctrl+u}7{enter}").await;
    assert_eq!(h.app.options.prompt_max_height, 7);
    assert_eq!(
        last_set(&h),
        json!({ "path": "tui.prompt_max_height", "value": 7 })
    );

    // Providers: enter uses one, e sets its model.
    h.input("{tab}").await;
    let screen = h.screen(100, 34);
    assert_eq!(top(&screen), config_top);
    assert!(
        screen.contains("q1") && screen.contains("in use") && screen.contains("d1"),
        "{screen}"
    );
    h.input("{enter}").await; // ds is first
    assert_eq!(last_set(&h), json!({ "path": "provider", "value": "ds" }));
    // e opens the editor; every field saves at once, as an override.
    h.input("e").await;
    let screen = h.screen(100, 34);
    assert_eq!(top(&screen), config_top);
    for row in [
        "› Model",
        "URL",
        "Type",
        "Reasoning effort",
        "Stream usage",
        "API key",
        "Key variable",
    ] {
        assert!(screen.contains(row), "{row} in {screen}");
    }
    h.input("{enter}{ctrl+u}").await; // Model
    h.app.paste("d2");
    h.input("{enter}").await;
    assert_eq!(
        last_set(&h),
        json!({ "path": "providers.ds.model", "value": "d2" })
    );
    h.input("{down}{down}{down}").await;
    let screen = h.screen(100, 34);
    let y = screen
        .lines()
        .position(|l| l.contains("› Reasoning effort"))
        .unwrap() as u16;
    let mut term = Terminal::new(TestBackend::new(100, 34)).unwrap();
    term.draw(|f| render::draw(f, &mut h.app)).unwrap();
    let buf = term.backend().buffer();
    for x in 0..100 {
        assert_eq!(buf[(x, y)].style().bg, h.app.theme.hl("Selection").bg);
    }
    let x = (0..100).find(|&x| buf[(x, y)].symbol() == "[").unwrap();
    assert!(screen.contains("[default]"));
    assert_eq!(buf[(x, y)].style().fg, h.app.theme.hl("Accent").fg);
    assert_eq!(buf[(x + 12, y)].style().fg, h.app.theme.hl("Dim").fg);
    h.input("{enter}").await; // Reasoning effort: default -> low
    assert_eq!(
        last_set(&h),
        json!({ "path": "providers.ds.reasoning_effort", "value": "low" })
    );
    h.input("{down}{enter}").await; // Stream usage: on -> off
    assert_eq!(
        last_set(&h),
        json!({ "path": "providers.ds.stream_usage", "value": false })
    );
    h.input("{down}{enter}sk-9{enter}").await; // API key
    assert!(!h.screen(100, 34).contains("sk-9"));
    assert_eq!(
        h.requests("secrets/set").last().unwrap(),
        &json!({ "provider": "ds", "key": "sk-9" })
    );
    h.input("{up}{up}{up}{up}{up}r").await; // reset the model to core.lua's
    assert_eq!(
        h.requests("settings/reset").last().unwrap(),
        &json!({ "path": "providers.ds.model" })
    );
    h.input("{esc}").await; // back to the list
    // A provider from core.lua cannot be deleted here.
    h.input("d").await;
    assert!(h.screen(100, 34).contains("is defined in core.lua"));

    // a adds one: saved whole, its key in secrets.json.
    h.input("a").await;
    assert_eq!(top(&h.screen(100, 34)), config_top);
    assert!(h.screen(100, 34).contains("› new provider"));
    h.input("{enter}mine{enter}{down}{enter}http://h/v1{enter}{down}{enter}m1{enter}")
        .await;
    h.input("{down}{down}{enter}sk-new{enter}{down}{down}{enter}")
        .await; // key, then Save
    assert_eq!(
        last_set(&h),
        json!({ "path": "providers.mine", "value": { "base_url": "http://h/v1", "model": "m1" } })
    );
    assert_eq!(
        h.requests("secrets/set").last().unwrap(),
        &json!({ "provider": "mine", "key": "sk-new" })
    );
    h.input("{esc}").await;
    // An added provider can be deleted (and its saved key with it).
    h.input("{down}{down}d").await;
    assert!(
        h.screen(100, 34)
            .contains("delete extra and its saved key?"),
        "{}",
        h.screen(100, 34)
    );
    h.input("y").await;
    assert_eq!(
        h.requests("settings/reset").last().unwrap(),
        &json!({ "path": "providers.extra" })
    );
    assert_eq!(
        h.requests("secrets/set").last().unwrap(),
        &json!({ "provider": "extra" })
    );

    // Plugins: space switches one off, saved and unloaded on both sides.
    h.input("{tab}").await;
    assert_eq!(top(&h.screen(100, 34)), config_top);
    select(&mut h, "corepart").await;
    h.input("{space}").await;
    assert_eq!(
        last_set(&h),
        json!({ "path": "plugins.disabled", "value": ["corepart"] })
    );
    assert_eq!(
        h.requests("plugin/unload").last().unwrap()["name"],
        "corepart"
    );

    // The core's compaction settings have a tab of their own.
    h.input("{tab}").await;
    assert_eq!(top(&h.screen(100, 34)), config_top);
    assert!(
        h.screen(100, 34).contains("Turns kept in full"),
        "{}",
        h.screen(100, 34)
    );

    // A plugin's own tab, from its manifest, with its limits.
    h.input("{tab}").await;
    assert_eq!(top(&h.screen(100, 34)), config_top);
    assert!(
        h.screen(100, 34).contains("Results"),
        "{}",
        h.screen(100, 34)
    );
    h.input("{enter}{ctrl+u}12{enter}").await;
    assert!(h.screen(100, 34).contains("between 1 and 10"));
    h.input("{enter}{ctrl+u}7{enter}").await;
    assert_eq!(last_set(&h), json!({ "path": "webby.n", "value": 7 }));

    h.input("{esc}").await;
    assert!(!h.screen(100, 34).contains(" Providers "));
}

#[tokio::test]
async fn plugins_switched_off_in_settings_are_listed_not_loaded() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["kept", "off"] {
        let p = dir.path().join("plugins").join(name);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("tui.lua"), format!("ran_{name} = true")).unwrap();
    }
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.settings = json!({ "plugins": { "disabled": ["off"] } });
    h.app.load_user_config();
    assert_eq!(h.lua("=ran_kept").await, "true");
    assert_eq!(h.lua("=ran_off").await, "nil");
    let off = h
        .app
        .plugins
        .iter()
        .find(|p| p.name == "off")
        .expect("listed");
    assert!(!off.loaded);
    // It can be switched on again.
    h.lua("bone.plugin.load('off')").await;
    assert_eq!(h.lua("=ran_off").await, "true");
}

#[tokio::test]
async fn options_with_choices_take_only_those() {
    let mut h = Harness::build(None).await;
    let set = |h: &mut Harness, code: &str| -> Result<(), String> {
        h.app
            .with_api(|lua| lua.load(code).exec())
            .map_err(|e| e.to_string())
    };
    set(&mut h, "bone.o.tool_detail = 'full'").unwrap();
    let e = set(&mut h, "bone.o.tool_detail = 'bogus'").unwrap_err();
    assert!(
        e.contains("tool_detail must be one of: summary, rows, full"),
        "{e}"
    );
    let e = set(&mut h, "bone.o.apply('tool_detail=bogus')").unwrap_err();
    assert!(e.contains("must be one of"), "{e}");
    let info: String = h
        .app
        .with_api(|lua| {
            lua.load("return table.concat(bone.o.info('tool_detail').choices, ',')")
                .eval()
        })
        .unwrap();
    assert_eq!(info, "summary,rows,full");
    let e = set(
        &mut h,
        "bone.o.define('x_choice', 'a', { choices = { 'b' } })",
    )
    .unwrap_err();
    assert!(e.contains("not one of its choices"), "{e}");
}

/// A local catalog with one package, `demo`, in `dir/catalog`, its hashes
/// computed with bone.sha256; settings point /catalog at it.
async fn local_catalog(h: &mut Harness, dir: &std::path::Path) -> std::path::PathBuf {
    let src = dir.join("catalog");
    let pkg = src.join("plugins/demo");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("tui.lua"), "demo_loaded = (demo_loaded or 0) + 1").unwrap();
    std::fs::write(
        pkg.join("manifest.json"),
        r#"{ "version": "1.0.0", "description": "a demo" }"#,
    )
    .unwrap();
    h.lua(&format!(
        r#"local function file(p) local f = io.open("{src}/plugins/demo/" .. p, "rb") local t = f:read("*a") f:close() return t end
           local files = {{}}
           for _, p in ipairs({{ "manifest.json", "tui.lua" }}) do files[#files + 1] = {{ path = p, sha256 = bone.sha256(file(p)) }} end
           bone.fs.write("{src}/catalog.json", bone.json.encode({{ {{ name = "demo", version = "1.0.0", description = "a demo", files = files }} }}))"#,
        src = src.display()
    ))
    .await;
    h.app.settings = json!({ "catalog": { "url": src.to_string_lossy() } });
    src
}

async fn add_local_catalog_package(h: &mut Harness, src: &std::path::Path, name: &str) {
    let pkg = src.join("plugins").join(name);
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("tui.lua"), format!("{name}_loaded = true")).unwrap();
    std::fs::write(
        pkg.join("manifest.json"),
        format!(r#"{{ "version": "1.0.0", "description": "{name}" }}"#),
    )
    .unwrap();
    h.lua(&format!(
        r#"local function file(p) local f = io.open("{src}/plugins/{name}/" .. p, "rb") local t = f:read("*a") f:close() return t end
           local index_file = io.open("{src}/catalog.json", "rb")
           local index = bone.json.decode(index_file:read("*a"))
           index_file:close()
           local files = {{}}
           for _, p in ipairs({{ "manifest.json", "tui.lua" }}) do files[#files + 1] = {{ path = p, sha256 = bone.sha256(file(p)) }} end
           index[#index + 1] = {{ name = "{name}", version = "1.0.0", description = "{name}", files = files }}
           bone.fs.write("{src}/catalog.json", bone.json.encode(index))"#,
        src = src.display(),
        name = name,
    ))
    .await;
}

#[tokio::test]
async fn catalog_selects_and_installs_multiple_packages() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    let src = local_catalog(&mut h, dir.path()).await;
    add_local_catalog_package(&mut h, &src, "demo_two").await;

    h.input("/catalog{enter}").await;
    assert!(h.screen(100, 30).contains("demo") && h.screen(100, 30).contains("demo_two"));
    h.input("a{enter}").await;
    assert!(
        h.screen(100, 30).contains("install 2 selected packages?"),
        "{}",
        h.screen(100, 30)
    );
    h.input("y").await;

    assert!(dir.path().join("plugins/demo/tui.lua").is_file());
    assert!(dir.path().join("plugins/demo_two/tui.lua").is_file());
    assert!(
        h.screen(100, 30).contains("installed 2 packages"),
        "{}",
        h.screen(100, 30)
    );
}

#[tokio::test]
async fn catalog_installs_updates_and_removes_packages() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    let src = local_catalog(&mut h, dir.path()).await;
    h.input("/catalog{enter}").await;
    let screen = h.screen(100, 30);
    assert!(
        screen.contains("Available") && screen.contains("demo"),
        "{screen}"
    );

    // Enter asks first; y installs (checked against the hashes) and loads it.
    h.input("{enter}").await;
    assert!(
        h.screen(100, 30)
            .contains("install demo? Its Lua runs with your permissions.")
    );
    h.input("y").await;
    let installed = dir.path().join("plugins/demo");
    assert!(installed.join("tui.lua").is_file() && installed.join("manifest.json").is_file());
    assert_eq!(h.lua("=demo_loaded").await, "1");
    let screen = h.screen(100, 30);
    assert!(
        screen.contains("Installed") && screen.contains("demo installed"),
        "{screen}"
    );

    // A changed installed file shows as an update.
    std::fs::write(installed.join("tui.lua"), "-- edited").unwrap();
    h.input("u").await;
    assert!(
        h.screen(100, 30).contains("Updates"),
        "{}",
        h.screen(100, 30)
    );

    // A source file that does not match the catalog is refused.
    std::fs::write(src.join("plugins/demo/tui.lua"), "tampered = true").unwrap();
    h.input("{enter}y").await;
    assert!(
        h.screen(100, 30).contains("does not match the catalog"),
        "{}",
        h.screen(100, 30)
    );
    assert_eq!(
        std::fs::read_to_string(installed.join("tui.lua")).unwrap(),
        "-- edited"
    );

    // Remove: moved aside, not deleted.
    h.input("x").await;
    assert!(h.screen(100, 30).contains("remove demo?"));
    h.input("y").await;
    assert!(!installed.exists());
    let backups: Vec<_> = std::fs::read_dir(dir.path().join("plugins-backup"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1);
    h.input("{esc}").await;
}

#[tokio::test]
async fn setup_adds_a_provider_its_key_and_packages() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    local_catalog(&mut h, dir.path()).await;
    h.lua("require('bone.setup').open()").await;
    assert!(h.screen(100, 30).contains("Welcome to bone"));
    h.input("{enter}").await; // to the providers
    h.input("{enter}").await; // a server on this machine
    let screen = h.screen(100, 30);
    assert!(
        screen.contains("http://localhost:8080/v1") && screen.contains("› Model"),
        "{screen}"
    );
    // Continue without a model is refused.
    h.input("{down}{down}{down}{enter}").await;
    assert!(h.screen(100, 30).contains("the model is needed"));
    h.input("{up}{up}{up}{enter}").await; // Model
    h.app.paste("qwen");
    h.input("{enter}").await;
    h.input("{down}{enter}sk-1{enter}").await; // API key
    let screen = h.screen(100, 30);
    assert!(
        screen.contains("••••") && !screen.contains("sk-1"),
        "{screen}"
    );
    h.input("{down}{down}{enter}").await; // Continue
    let screen = h.screen(100, 30);
    assert!(
        screen.contains("3/3 packages") && screen.contains("demo"),
        "{screen}"
    );
    h.input("{space}{down}{enter}").await; // pick demo, Continue

    assert_eq!(
        h.requests("settings/set")[0],
        json!({ "path": "providers.local", "value": { "base_url": "http://localhost:8080/v1", "model": "qwen" } })
    );
    assert_eq!(
        h.requests("secrets/set")[0],
        json!({ "provider": "local", "key": "sk-1" })
    );
    assert_eq!(
        h.requests("settings/set")[1],
        json!({ "path": "provider", "value": "local" })
    );
    assert!(dir.path().join("plugins/demo/tui.lua").is_file());
    let screen = h.screen(100, 30);
    assert!(
        screen.contains("installed demo") && screen.contains("Bone is ready."),
        "{screen}"
    );
    // Enter hands the chosen first prompt to the prompt box and closes setup.
    h.input("{enter}").await;
    assert!(!h.screen(100, 30).contains("Bone is ready."));
    assert_eq!(h.app.prompt_text(), "Find the main entry point");
}

#[path = "reload_tests.rs"]
mod reload_tests;

/// A running turn of `n` calls in one stretch, all but the last finished
/// with `size` bytes of output, the last a running shell.
async fn long_stretch(n: usize, size: usize) -> Harness {
    let mut h = Harness::build(None).await;
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    let calls = (0..n)
        .map(|i| ToolCall {
            id: format!("c{i}"),
            name: if i == n - 1 { "shell" } else { "read_file" }.into(),
            arguments: json!({ "path": format!("f{i}.rs"), "command": "ls" }).to_string(),
        })
        .collect();
    h.emit::<MessageCompleted>(tool_calls("s-new", calls)).await;
    for i in 0..n - 1 {
        h.emit::<ToolFinished>(ToolFinishedParams {
            session_id: "s-new".into(),
            turn_id: 1,
            call_id: format!("c{i}"),
            output: "x".repeat(size),
            is_error: false,
            duration_ms: None,
        })
        .await;
    }
    h
}

/// How long drawing a frame takes.
fn frame_time(h: &mut Harness) -> std::time::Duration {
    let t = std::time::Instant::now();
    h.screen(120, 40);
    t.elapsed()
}

/// Render cost of a long, tool-heavy chat in the default (summary) view.
/// `cargo test -p bone-tui --release perf_probe -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn perf_probe() {
    for (n, size) in [(50, 2_000), (200, 2_000), (500, 2_000), (500, 20_000)] {
        let mut h = long_stretch(n, size).await;
        let first = frame_time(&mut h);
        let steady = frame_time(&mut h);
        let mut live = std::time::Duration::ZERO;
        for _ in 0..10 {
            h.emit::<ToolOutput>(ToolOutputParams {
                session_id: "s-new".into(),
                turn_id: 1,
                call_id: format!("c{}", n - 1),
                text: "line\n".into(),
            })
            .await;
            live += frame_time(&mut h) / 10;
        }
        h.app
            .with_api(|lua| lua.load("bone.ui.refresh()").exec())
            .unwrap();
        let refresh = frame_time(&mut h);
        println!(
            "{n} calls, {size} B each: first {first:?}, steady {steady:?}, live output {live:?}, after bone.ui.refresh {refresh:?}"
        );
    }
}

#[tokio::test]
async fn empty_session_hint_is_centered_in_chat_beside_panels() {
    let mut h = Harness::build(None).await;
    h.lua(r#"bone.ui.panel.open({ id = "notes", dock = "left", size = 20, lines = { "NOTES" } })"#)
        .await;
    for (width, height) in [(100, 24), (120, 30)] {
        let screen = h.screen(width, height);
        let area = h.app.placed[&crate::app::CHAT_WIN].area;
        let lines: Vec<_> = screen.lines().collect();
        let hint = "New session. Type a message and press enter.";
        let row = lines.iter().position(|l| l.contains(hint)).unwrap();
        assert_eq!(row, (area.y + (area.height - 2) / 2) as usize, "{screen}");
        let col = lines[row][..lines[row].find(hint).unwrap()].chars().count();
        assert_eq!(
            col,
            area.x as usize + (area.width as usize - hint.len()) / 2,
            "{screen}"
        );
        assert!(lines[0].contains("NOTES"), "{screen}");
        assert!(!lines[0].contains("New session"), "{screen}");
    }
    h.input("go{enter}").await;
    h.emit::<TurnStarted>(started("s-new", "go")).await;
    assert!(!h.screen(100, 24).contains("New session."));
}

#[tokio::test]
async fn history_sidebar_spans_screen_and_keeps_composer_beside_it() {
    let mut h = Harness::new().await;
    h.input("{ctrl+o}").await;
    for (width, height) in [(100, 24), (120, 30)] {
        h.screen(width, height);
        let sidebar = h.app.panel("sessions").unwrap().area.unwrap();
        assert_eq!(sidebar.y, 0);
        assert_eq!(sidebar.height, height);
        let prompt = h.app.placed[&crate::app::PROMPT_WIN].area;
        assert_eq!(prompt.x, sidebar.right() + 1);
        let status = h
            .app
            .leaves
            .iter()
            .find(|(name, _)| name == "statusline")
            .unwrap()
            .1;
        assert_eq!(status.x, prompt.x);
        assert_eq!(status.bottom(), height);
    }
    h.input("{ctrl+o}").await;
    h.screen(100, 24);
    assert_eq!(h.app.placed[&crate::app::PROMPT_WIN].area.x, 0);
}

#[tokio::test]
async fn idle_session_sidebar_schedules_timestamp_refresh() {
    let mut h = Harness::new().await;
    h.input("{ctrl+o}").await;
    h.emit::<TurnFinished>(finished("s-one", TurnOutcome::Completed))
        .await;
    h.lua("bone.ui.refresh_in = function(ms) sidebar_delay = ms end")
        .await;
    h.screen(80, 24);
    assert_eq!(
        h.lua("=sidebar_delay > 120 and sidebar_delay <= 60000")
            .await,
        "true"
    );
}

#[tokio::test]
async fn statusline_keeps_each_sessions_timing_across_switches() {
    let mut h = Harness::new().await;
    h.lua("bone._standard_status.turns.a = { started = os.time() - 65, finished = os.time() }; line = function(s) local o = {} for _, it in ipairs(bone.ui.statusline({session = s, width = 120})) do o[#o + 1] = type(it) == 'table' and it[1] or it end return table.concat(o) end").await;
    assert!(
        h.lua("=line({session_id = 'b', running = true, elapsed = 65})")
            .await
            .contains("thinking 1m")
    );
    assert!(
        h.lua("=line({session_id = 'a'})")
            .await
            .contains("worked 1m")
    );
}

#[tokio::test]
async fn session_sidebar_search_accepts_unicode_and_alt_a_does_not_archive() {
    let mut h = Harness::new().await;
    h.input("{ctrl+o}").await;
    h.input("a中文+🦴").await;
    let screen = h.screen(80, 24);
    assert!(
        screen.replace(' ', "").contains("Search:a中文+🦴"),
        "{screen}"
    );
    assert_eq!(
        h.lua("=next(bone.state.load('sessions-archived')) == nil")
            .await,
        "true"
    );
    h.input("{ctrl+u}{alt+a}").await;
    assert_eq!(
        h.lua("=next(bone.state.load('sessions-archived')) == nil")
            .await,
        "true"
    );
    let screen = h.screen(80, 24);
    assert!(!screen.contains("alt+a archive"), "{screen}");
    assert!(h.requests("session/messages").is_empty());
}

#[tokio::test]
async fn screen_popups_cover_full_height_sidebars() {
    let mut h = Harness::new().await;
    h.input("{ctrl+o}").await;
    h.lua("bone.ui.win({ anchor = 'screen', row = 0, col = 0, width = 80, lines = { string.rep('X', 80) } })")
        .await;
    let screen = h.screen(80, 24);
    assert_eq!(screen.lines().next().unwrap(), "X".repeat(80));
}

#[tokio::test]
async fn session_sidebar_formats_only_visible_rows_for_large_history() {
    let mut h = Harness::new().await;
    h.lua(
        r#"
        local request = bone.request
        bone.request = function(method, params, cb)
          if method == 'session/list' then
            local sessions = {}
            for i = 1, 4500 do
              sessions[i] = { session_id = string.format('history-%04d', i),
                title = 'conversation ' .. i, cwd = '/work', created_at = 0 }
            end
            cb(sessions)
          else
            return request(method, params, cb)
          end
        end
        sidebar_formats = 0
        local truncate = bone.text.truncate
        bone.text.truncate = function(...)
          sidebar_formats = sidebar_formats + 1
          return truncate(...)
        end
    "#,
    )
    .await;
    h.lua("bone.api.open_session('history-4500')").await;
    h.input("{ctrl+o}").await;
    // First draw must reveal the current chat, not just select it offscreen.
    assert!(h.screen(100, 24).contains("conversation 4500"));
    assert!(h.app.panel("sessions").unwrap().rows >= 13500);
    h.lua("sidebar_formats = 0").await;
    let screen = h.screen(100, 24);
    assert!(screen.contains("conversation 4500"), "{screen}");
    let formats: usize = h.lua("=sidebar_formats").await.parse().unwrap();
    assert!(
        formats <= 30,
        "formatted {formats} rows for a 24-row viewport"
    );
    let saved_top = h.app.panel("sessions").unwrap().top;
    h.input("{tab}").await;
    assert!(
        h.screen(100, 24)
            .contains("nothing running in the background")
    );
    h.input("{tab}").await;
    assert!(h.screen(100, 24).contains("conversation 4500"));
    assert_eq!(h.app.panel("sessions").unwrap().top, saved_top);
    // Manual scrolling must not snap back to the still-selected current chat.
    h.lua("bone.ui.panel.get('sessions'):scroll('top')").await;
    let screen = h.screen(100, 24);
    assert!(screen.contains("History · 4500") && screen.contains("conversation 1"));
    h.input("{home}").await;
    assert!(h.screen(100, 24).contains("conversation 1"));
    assert_eq!(h.app.panel("sessions").unwrap().top, 0);
    assert_eq!(h.requests("session/messages").len(), 1);
}

#[tokio::test]
async fn flexible_panel_tree_composes_and_publishes_final_geometry() {
    let mut h = Harness::new().await;
    h.lua(r#"
        p = bone.ui.panel.open({ id = 'tree', full_height = true, dock = 'left', title = 'TREE',
          render = function(ctx) panel_w = ctx.width; panel_h = ctx.height; return {'one', 'two'} end })
        bone.ui.layout = { { cols = { { panel = 'tree', size = 12 },
          { rows = { 'chat', 'prompt', 'statusline' } } }, sep = '│' } }
        bone.on('mouse', function(ev) mouse_region = ev.region; mouse_panel = ev.panel; return true end)
    "#).await;
    h.screen(80, 24);
    assert_eq!(
        h.app.panel("tree").unwrap().area.unwrap(),
        ratatui::layout::Rect::new(0, 0, 12, 24)
    );
    assert_eq!(h.lua("=panel_w .. ',' .. panel_h").await, "\"12,23\"");
    assert_eq!(h.app.placed[&crate::app::CHAT_WIN].area.x, 13);
    h.app.mouse("down", "left", (1, 1));
    assert_eq!(
        h.lua("=mouse_region .. ',' .. mouse_panel").await,
        "\"panel:tree,tree\""
    );
}

#[tokio::test]
async fn flexible_layout_clamps_tiny_and_oversubscribed_frames() {
    let mut h = Harness::new().await;
    h.lua(
        r#"
      bone.ui.panel.open({id='tiny', lines={'a'}})
      bone.ui.layout = {{ cols = { { panel='tiny', size=999999999 },
        { rows = { {'chat', size=65535}, {'prompt', size=65535} } },
        {'extra', size=65535} }, sep='│' }}
    "#,
    )
    .await;
    for (w, height) in [(0, 0), (1, 1), (2, 2), (10, 3), (80, 24)] {
        h.screen(w, height);
        for (_, r) in &h.app.leaves {
            assert!(
                r.right() <= w && r.bottom() <= height,
                "{r:?} in {w}x{height}"
            );
        }
        if let Some(r) = h.app.panel("tiny").unwrap().area {
            assert!(r.right() <= w && r.bottom() <= height);
        }
    }
}

#[tokio::test]
async fn flexible_docks_mouse_and_auto_content_use_final_rectangles() {
    let mut h = Harness::new().await;
    h.lua(
        r#"
      bone.ui.panel.open({id='auto', dock='right', size='auto',
        render=function(ctx) final_w=ctx.width; final_h=ctx.height; return {'1234567890'} end})
      bone.ui.layout={'chat','prompt'}
      bone.on('mouse', function(ev) mouse_region=ev.region; mouse_panel=ev.panel; return true end)
    "#,
    )
    .await;
    h.screen(40, 10);
    let r = h.app.panel("auto").unwrap().area.unwrap();
    assert_eq!(
        h.lua("=final_w .. ',' .. final_h").await,
        format!("\"{},{}\"", r.width, r.height)
    );
    assert_eq!(
        h.app.leaves.iter().find(|(n, _)| n == "chat").unwrap().1,
        h.app.placed[&crate::app::CHAT_WIN].area
    );
    h.app.mouse("down", "left", (r.x, r.y));
    assert_eq!(
        h.lua("=tostring(mouse_region) .. ',' .. mouse_panel").await,
        "\"nil,auto\""
    );
    h.screen(10, 2);
    assert!(h.app.panel("auto").unwrap().area.is_none());
}

#[tokio::test]
async fn flexible_auto_panel_reserves_prompt_and_fill_rows() {
    let mut h = Harness::new().await;
    h.lua(
        r#"
        local lines = {}; for i=1,30 do lines[i]='PANEL' end
        bone.ui.panel.open({id='p', max=100, lines=lines})
        bone.ui.layout={'chat', {panel='p', size='auto'}, 'prompt'}
    "#,
    )
    .await;
    h.screen(80, 24);
    assert_eq!(h.app.placed[&crate::app::CHAT_WIN].area.height, 3);
    assert_eq!(h.app.placed[&crate::app::PROMPT_WIN].area.height, 3);
    assert_eq!(h.app.panel("p").unwrap().area.unwrap().height, 18);
}

#[tokio::test]
async fn flexible_empty_panel_subtrees_do_not_reserve_space_or_separators() {
    let mut h = Harness::new().await;
    h.lua(
        r#"
        p=bone.ui.panel.open({id='p', lines={'PANEL'}}); p:hide()
        bone.ui.layout={{cols={
            {rows={{cols={{panel='p'}, {panel='missing'}}}}, size=12},
            {rows={'chat','prompt'}}}, sep='|'}}
    "#,
    )
    .await;
    let screen = h.screen(80, 24);
    let chat = h.app.placed[&crate::app::CHAT_WIN].area;
    assert_eq!((chat.x, chat.width), (0, 80));
    assert!(!screen.contains('|'), "{screen}");
    h.lua("p:show()").await;
    h.screen(80, 24);
    assert_eq!(h.app.placed[&crate::app::CHAT_WIN].area.x, 13);
}

#[tokio::test]
async fn flexible_render_callback_hide_clears_geometry_and_hits() {
    let mut h = Harness::new().await;
    h.lua(
        r#"
        p=bone.ui.panel.open({id='p', lines={'PANEL'}})
        bone.ui.layout={{panel='p', size=4}, 'chat', 'prompt'}
        bone.ui.regions.chat_empty=function() p:hide(); return {'EMPTY'} end
    "#,
    )
    .await;
    let screen = h.screen(80, 24);
    assert!(!screen.contains("PANEL"), "{screen}");
    assert!(h.app.panel("p").unwrap().area.is_none());
    assert!(h.app.panel_hit((0, 0)).is_none());
    let r = h.app.leaves.iter().find(|(n, _)| n == "panel:p").unwrap().1;
    assert_eq!((r.width, r.height), (0, 0));
    h.screen(80, 24);
    assert_eq!(h.app.placed[&crate::app::CHAT_WIN].area.y, 0);
}
