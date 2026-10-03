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
    Ok(match method {
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

    // Bad arguments are Lua errors, not crashes.
    for (code, want) in [
        ("bone.keymap.set('hyper+x', 'submit')", "unknown modifier"),
        (
            "bone.keymap.set('x', 'submit', { context = 'nope' })",
            "unknown context",
        ),
        ("bone.keymap.set('x', 'nope')", "unknown action"),
        ("bone.o.nope = 1", "unknown option"),
        ("bone.cmd.create('Upper', function() end)", "lowercase"),
        ("bone.cmd.create('new', function() end)", "built-in"),
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
