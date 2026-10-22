//! One core, one TUI, one GUI: what holds between clients (see "Multiple
//! clients" in docs/protocol.md). Both frontends run headless against a
//! real core whose model is a fake OpenAI-compatible server.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bone_core::Core;
use bone_core::scripting;
use bone_gui::GuiOptions;
use bone_server::Server;
use bone_tui::RunOptions;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const WAIT: Duration = Duration::from_secs(10);

/// Serve one canned SSE body per request.
async fn fake_model(bodies: Vec<String>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        for body in bodies {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map(|v| v.trim().parse().unwrap())
                        .unwrap();
                    while buf.len() < i + 4 + len {
                        let n = sock.read(&mut chunk).await.unwrap();
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.shutdown().await.unwrap();
        }
    });
    url
}

fn text(s: &str) -> String {
    let chunk = json!({"choices":[{"delta":{"content": s}}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

fn shell(id: &str, command: &str) -> String {
    let args = json!({ "command": command }).to_string();
    let chunk = json!({"choices":[{"delta":{"tool_calls":[
        {"index":0,"id":id,"function":{"name":"shell","arguments":args}}
    ]}}]});
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

struct Env {
    config: tempfile::TempDir,
    work: tempfile::TempDir,
    server: Server,
}

/// Who answers the "Allow <tool>?" question the core asks before every
/// tool call, if it asks.
#[derive(PartialEq)]
enum Ask {
    No,
    /// Asked, and the TUI shows it in a popup (`y` allows).
    TuiPopup,
    /// Asked; only the GUI shows it.
    GuiOnly,
}

/// A core on the fake model.
async fn env(bodies: Vec<String>, ask: Ask) -> Env {
    let url = fake_model(bodies).await;
    let config = tempfile::tempdir().unwrap();
    let mut core =
        format!(r#"bone.config.providers.fake = {{ base_url = "{url}", model = "fake" }}"#);
    if ask != Ask::No {
        core.push_str(
            r#"
            bone.hook("tool_call", function(ev)
              local answer = bone.ask({ title = "Allow " .. ev.name .. "?", options = { "allow", "deny" } })
              if answer ~= "allow" then return { deny = "The user denied this tool call." } end
            end)
            "#,
        );
    }
    if ask == Ask::TuiPopup {
        std::fs::write(
            config.path().join("tui.lua"),
            r#"
            bone.on("ask/requested", function(ev)
              local popup
              popup = bone.ui.popup({ lines = { ev.question.title }, guard = 0,
                keys = { y = function()
                  bone.request("ask/respond", { ask_id = ev.ask_id, answer = "allow" })
                  bone.ui.close(popup)
                end } })
              bone.on("ask/resolved", function(r)
                if r.ask_id == ev.ask_id then bone.ui.close(popup) end
              end)
            end)
            "#,
        )
        .unwrap();
    }
    std::fs::write(config.path().join("core.lua"), core).unwrap();
    let work = tempfile::tempdir().unwrap();
    let loaded = scripting::load_with(config.path(), &|_| None).unwrap();
    let server = Server::new(Arc::new(Core::from_loaded(loaded)));
    Env {
        config,
        work,
        server,
    }
}

impl Env {
    async fn tui(&self) -> bone_tui::Headless {
        let opts = RunOptions {
            clipboard_executable: None,
            cwd: self.cwd(),
            config_dir: Some(self.config.path().to_owned()),
            resume: None,
            reload_core: true,
            reconnect: None,
        };
        bone_tui::Headless::start(self.server.connect_in_process(), opts, 100, 30)
            .await
            .unwrap()
    }

    async fn gui(&self) -> bone_gui::Headless {
        let opts = GuiOptions {
            cwd: self.cwd(),
            reconnect: None,
            config_dir: Some(self.config.path().to_owned()),
        };
        let gui = bone_gui::Headless::start(self.server.connect_in_process(), opts, 1000.0, 700.0)
            .await
            .unwrap();
        // The standard library loads in the GUI as it does in the TUI.
        assert!(
            gui.gui.lua_errors().is_empty(),
            "{:?}",
            gui.gui.lua_errors()
        );
        gui
    }

    fn cwd(&self) -> String {
        self.work.path().to_string_lossy().into_owned()
    }
}

fn session_of(gui: &bone_gui::Gui) -> String {
    gui.chats[gui.current].session_id().unwrap().to_owned()
}

fn idle(gui: &bone_gui::Gui) -> bool {
    !gui.running()
}

/// 1, 2, 4: both attach to one session, see each other's turns, and both
/// submit to it.
#[tokio::test(flavor = "multi_thread")]
async fn both_clients_drive_one_session() {
    let e = env(vec![text("hi gui"), text("hi tui")], Ask::No).await;
    let mut gui = e.gui().await;
    let mut tui = e.tui().await;

    gui.send("hello from gui");
    gui.wait_for(WAIT, |g| g.transcript().contains("hi gui") && idle(g))
        .await
        .unwrap();
    let id = session_of(&gui.gui);

    tui.open_session(id.clone());
    tui.wait_for(WAIT, |s| {
        s.contains("hello from gui") && s.contains("hi gui")
    })
    .await
    .unwrap_or_else(|s| panic!("TUI did not attach:\n{s}"));

    tui.type_text("hello from tui\n");
    gui.wait_for(WAIT, |g| {
        g.transcript().contains("hello from tui") && g.transcript().contains("hi tui") && idle(g)
    })
    .await
    .unwrap_or_else(|t| panic!("GUI did not see the TUI's turn:\n{t}"));
    tui.wait_for(WAIT, |s| s.contains("hi tui")).await.unwrap();
    assert_eq!(session_of(&gui.gui), id);
}

/// 3: tool and message events arrive in order: the live transcript is the
/// one the core saved.
#[tokio::test(flavor = "multi_thread")]
async fn events_arrive_in_order() {
    let e = env(
        vec![shell("c1", "echo tool-ran"), text("all done")],
        Ask::No,
    )
    .await;
    let mut gui = e.gui().await;
    let mut tui = e.tui().await;
    gui.send("run something");
    gui.wait_for(WAIT, |g| {
        g.transcript().contains("Ran 1 shell command")
            && g.transcript().contains("all done")
            && idle(g)
    })
    .await
    .unwrap();
    let live = gui.gui.transcript();
    // Drawn by the standard views, as in the TUI: tools collapsed.
    let order: Vec<_> = ["run something", "Ran 1 shell command", "all done"]
        .iter()
        .map(|s| {
            live.find(s)
                .unwrap_or_else(|| panic!("{s} missing:\n{live}"))
        })
        .collect();
    assert!(order.is_sorted(), "out of order:\n{live}");

    // The TUI, attaching afterwards, loads the same transcript (it shows
    // tools collapsed). Like a running TUI, it has kept up with events.
    tui.settle(Duration::from_millis(50)).await;
    tui.open_session(session_of(&gui.gui));
    let screen = tui
        .wait_for(WAIT, |s| {
            s.contains("Ran 1 shell command") && s.contains("all done")
        })
        .await
        .unwrap();
    assert_eq!(screen.matches("all done").count(), 1, "{screen}");

    // Reloading from the core's snapshot gives the same rows.
    lagged(&mut gui.gui);
    gui.settle(Duration::from_millis(100)).await;
    assert_eq!(gui.gui.transcript(), live);
}

/// 5: a question answered in one client disappears from the other.
#[tokio::test(flavor = "multi_thread")]
async fn a_question_answered_in_one_client_closes_in_the_other() {
    let e = env(
        vec![shell("c1", "echo allowed"), text("finished")],
        Ask::TuiPopup,
    )
    .await;
    let mut gui = e.gui().await;
    let mut tui = e.tui().await;
    gui.send("needs approval");
    gui.wait_for(WAIT, |g| !g.asks.is_empty()).await.unwrap();
    let ask = gui.gui.asks.values().next().unwrap().clone();
    assert_eq!(ask.question["title"], "Allow shell?");

    tui.wait_for(WAIT, |s| s.contains("Allow shell?"))
        .await
        .unwrap();
    assert_eq!(tui.context(), "popup");
    tui.press("y").unwrap();

    gui.wait_for(WAIT, |g| g.asks.is_empty()).await.unwrap();
    gui.wait_for(WAIT, |g| g.transcript().contains("finished") && idle(g))
        .await
        .unwrap();
    // The summary types itself out over a few frames.
    gui.wait_for(WAIT, |g| g.transcript().contains("Ran 1 shell command"))
        .await
        .unwrap();
    // The core no longer has it either: a late answer is refused.
    gui.gui.answer(ask.ask_id, json!("allow"));
    gui.wait_for(WAIT, |g| g.status.as_ref().is_some_and(|s| s.error))
        .await
        .unwrap();
}

/// 7: a GUI that missed events reloads every open chat.
#[tokio::test(flavor = "multi_thread")]
async fn a_lagging_gui_reloads_its_chats() {
    let e = env(vec![text("first answer")], Ask::No).await;
    let mut gui = e.gui().await;
    gui.send("question");
    gui.wait_for(WAIT, |g| g.transcript().contains("first answer") && idle(g))
        .await
        .unwrap();
    let saved = gui.gui.transcript();
    // Something only this client has, as if it had applied events wrongly.
    let cur = gui.gui.current;
    gui.gui.chats[cur].notice("stale".into(), false);
    gui.frame();
    assert!(gui.gui.transcript().contains("stale"));

    lagged(&mut gui.gui);
    gui.wait_for(WAIT, |g| !g.transcript().contains("stale"))
        .await
        .unwrap();
    assert_eq!(gui.gui.transcript(), saved);
}

fn lagged(gui: &mut bone_gui::Gui) {
    gui.handle_event(&bone_client::Event {
        method: "session/updated".into(),
        params: Some(json!({ "session_id": "", "reason": "lagged" })),
    });
}

/// 6: reconnecting restores the transcript, the running turn, the queue,
/// and open questions, all changed while the GUI was away.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn reconnecting_restores_everything() {
    let e = env(
        vec![shell("c1", "echo approved"), text("after the steer")],
        Ask::GuiOnly,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let core_sock = dir.path().join("core.sock");
    let listener = bone_server::Listener::bind(&core_sock).await.unwrap();
    let server = e.server.clone();
    tokio::spawn(async move { listener.run(&server).await });
    let gui_sock = dir.path().join("gui.sock");
    let proxy = Proxy::start(&gui_sock, &core_sock);

    let opts = GuiOptions {
        cwd: e.cwd(),
        reconnect: Some(gui_sock.clone()),
        config_dir: Some(e.config.path().to_owned()),
    };
    let conn = bone_client::connect(&gui_sock).await.unwrap();
    let mut gui = bone_gui::Headless::start(conn, opts, 1000.0, 700.0)
        .await
        .unwrap();
    let mut tui = e.tui().await;

    gui.send("start a turn");
    gui.wait_for(WAIT, |g| !g.asks.is_empty()).await.unwrap();
    let id = session_of(&gui.gui);

    // The GUI drops off; meanwhile the TUI queues a message.
    proxy.cut();
    gui.wait_for(WAIT, |g| {
        g.status
            .as_ref()
            .is_some_and(|s| s.text.contains("reconnecting"))
    })
    .await
    .unwrap();
    tui.settle(Duration::from_millis(50)).await;
    tui.open_session(id.clone());
    tui.wait_for(WAIT, |s| s.contains("start a turn"))
        .await
        .unwrap();
    tui.type_text("steer from tui\n");
    tui.wait_for(WAIT, |s| s.contains("steer from tui"))
        .await
        .unwrap();

    // Back: the snapshot has everything that happened.
    gui.wait_for(WAIT, |g| {
        let c = &g.chats[g.current];
        g.status
            .as_ref()
            .is_some_and(|s| s.text.contains("reconnected"))
            && c.turn.is_some()
            && c.queue.len() == 1
            && !g.asks.is_empty()
            && g.transcript().contains("start a turn")
    })
    .await
    .unwrap_or_else(|t| panic!("not restored:\n{t}"));
    assert_eq!(
        gui.gui.chats[gui.gui.current].queue[0].text,
        "steer from tui"
    );

    // And it still drives the session: answer, and the turn goes on.
    let ask_id = *gui.gui.asks.keys().next().unwrap();
    gui.gui.answer(ask_id, json!("allow"));
    gui.wait_for(WAIT, |g| {
        g.transcript().contains("after the steer") && idle(g)
    })
    .await
    .unwrap();
    gui.wait_for(WAIT, |g| {
        let t = g.transcript();
        t.contains("Ran 1 shell command") && t.contains("steer from tui")
    })
    .await
    .unwrap_or_else(|t| panic!("{t}"));
}

/// Forwards connections from one socket to another; `cut` drops the ones
/// open now and keeps accepting new ones, and `retarget` sends new ones to
/// another socket (a restarted server).
#[cfg(unix)]
struct Proxy {
    open: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    to: Arc<Mutex<std::path::PathBuf>>,
}

#[cfg(unix)]
impl Proxy {
    fn start(from: &Path, to: &Path) -> Proxy {
        let listener = tokio::net::UnixListener::bind(from).unwrap();
        let target = Arc::new(Mutex::new(to.to_owned()));
        let open: Arc<Mutex<Vec<_>>> = Arc::default();
        let (tasks, to) = (open.clone(), target.clone());
        tokio::spawn(async move {
            loop {
                let (mut a, _) = listener.accept().await.unwrap();
                let to = to.lock().unwrap().clone();
                let task = tokio::spawn(async move {
                    let Ok(mut b) = tokio::net::UnixStream::connect(&to).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
                });
                tasks.lock().unwrap().push(task);
            }
        });
        Proxy { open, to: target }
    }

    fn cut(&self) {
        for task in self.open.lock().unwrap().drain(..) {
            task.abort();
        }
    }

    fn retarget(&self, to: &Path) {
        *self.to.lock().unwrap() = to.to_owned();
    }
}

/// Serve `server` at `sock` until the task is aborted.
#[cfg(unix)]
async fn serve(
    server: Server,
    sock: std::path::PathBuf,
) -> tokio::task::JoinHandle<std::io::Result<()>> {
    let listener = bone_server::Listener::bind(&sock).await.unwrap();
    tokio::spawn(async move { listener.run(&server).await })
}

/// A server that restarts (a new process on the same data) comes back with
/// the session from disk; the GUI reconnects at the same path, reloads its
/// chat, and drives the new server.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_server_gives_back_the_session() {
    let e = env(vec![text("first answer")], Ask::No).await;
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.sock");
    let task = serve(e.server.clone(), first.clone()).await;
    let gui_sock = dir.path().join("gui.sock");
    let proxy = Proxy::start(&gui_sock, &first);
    let opts = GuiOptions {
        cwd: e.cwd(),
        reconnect: Some(gui_sock.clone()),
        config_dir: Some(e.config.path().to_owned()),
    };
    let conn = bone_client::connect(&gui_sock).await.unwrap();
    let mut gui = bone_gui::Headless::start(conn, opts, 1000.0, 700.0)
        .await
        .unwrap();
    gui.send("before the restart");
    gui.wait_for(WAIT, |g| g.transcript().contains("first answer") && idle(g))
        .await
        .unwrap();
    let id = session_of(&gui.gui);

    // The server goes away; a new one starts on the same config and data.
    task.abort();
    proxy.cut();
    let url = fake_model(vec![text("after the restart")]).await;
    std::fs::write(
        e.config.path().join("core.lua"),
        format!(r#"bone.config.providers.fake = {{ base_url = "{url}", model = "fake" }}"#),
    )
    .unwrap();
    let loaded = scripting::load_with(e.config.path(), &|_| None).unwrap();
    let second = dir.path().join("second.sock");
    let _task = serve(
        Server::new(Arc::new(Core::from_loaded(loaded))),
        second.clone(),
    )
    .await;
    proxy.retarget(&second);

    gui.wait_for(WAIT, |g| {
        g.status
            .as_ref()
            .is_some_and(|s| s.text.contains("reconnected"))
    })
    .await
    .unwrap();
    gui.wait_for(WAIT, |g| g.transcript().contains("first answer"))
        .await
        .unwrap_or_else(|t| panic!("the session did not come back:\n{t}"));
    assert_eq!(session_of(&gui.gui), id);
    assert!(idle(&gui.gui), "no turn survives a restart");
    gui.send("and now");
    gui.wait_for(WAIT, |g| {
        g.transcript().contains("after the restart") && idle(g)
    })
    .await
    .unwrap();
}
