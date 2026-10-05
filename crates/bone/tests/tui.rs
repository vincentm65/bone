//! End to end, no terminal: the TUI (headless) → in-process server → core
//! configured by core.lua → OpenAI-compatible HTTP → a fake model. Screens
//! are compared with snapshots in `tests/snapshots/`.
//!
//! Regenerate snapshots after an intended UI change with:
//!   UPDATE_SNAPSHOTS=1 cargo test -p bone --test tui

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bone_core::Core;
use bone_core::scripting;
use bone_server::Server;
use bone_tui::{Headless, RunOptions};
use serde_json::{Value, json};
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

fn sse(chunks: &[Value]) -> String {
    let mut out: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    out.push_str("data: [DONE]\n\n");
    out
}

struct Env {
    config: tempfile::TempDir,
    work: tempfile::TempDir,
    server: Server,
}

/// Copy a directory tree.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// With `styled`, the config has the `style` and `approve` example plugins
/// installed; without it bone is as shipped: a blank screen, no approvals.
async fn env(bodies: Vec<String>, styled: bool) -> Env {
    let url = fake_model(bodies).await;
    let config = tempfile::tempdir().unwrap();
    if styled {
        for name in ["style", "approve"] {
            let from = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/plugins")
                .join(name);
            copy_dir(&from, &config.path().join("plugins").join(name));
        }
    }
    std::fs::write(
        config.path().join("core.lua"),
        format!(r#"bone.config.providers.fake = {{ base_url = "{url}", model = "fake" }}"#),
    )
    .unwrap();
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
    async fn tui(&self, resume: Option<Option<String>>) -> Headless {
        let opts = RunOptions {
            cwd: self.work.path().to_string_lossy().into_owned(),
            config_dir: Some(self.config.path().to_owned()),
            resume,
            reload_core: true,
        };
        Headless::start(self.server.connect_in_process(), opts, 80, 24)
            .await
            .unwrap()
    }
}

/// Compare with a snapshot, after masking paths that change between runs.
fn snapshot(name: &str, screen: &str, work: &Path) {
    let screen = mask_done(&mask_timers(
        &screen.replace(&*work.to_string_lossy(), "<cwd>"),
    )) + "\n";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&path, &screen).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing {}; run with UPDATE_SNAPSHOTS=1", path.display()));
    assert_eq!(
        screen,
        want,
        "screen differs from {}; if intended, run with UPDATE_SNAPSHOTS=1",
        path.display()
    );
}

/// Spinner frames and elapsed times change between runs: `⠸ working 3s`
/// becomes `* working <t>`.
/// `worked 12s, finished at 3:42 pm` (a finished turn's times) as
/// `worked <t>, finished at <t>`.
fn mask_done(screen: &str) -> String {
    let mut out = String::new();
    let mut rest = screen;
    while let Some(i) = rest.find("worked ") {
        out.push_str(&rest[..i + 7]);
        rest = &rest[i + 7..];
        let n = timer_len(rest);
        out.push_str("<t>");
        rest = &rest[n..];
        if let Some(j) = rest.find(", finished at ") {
            out.push_str(", finished at ");
            rest = &rest[j + 14..];
            let m = clock_len(rest);
            out.push_str("<t>");
            rest = &rest[m..];
        }
    }
    out.push_str(rest);
    out
}

/// Length of a wall-clock time like `3:42 pm` at the start of `s`.
fn clock_len(s: &str) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        let stop = c.is_ascii_digit() || c == ':' || c.is_ascii_lowercase();
        let space = c == ' '
            && s[i + c.len_utf8()..]
                .chars()
                .next()
                .is_some_and(|c2| c2.is_ascii_lowercase());
        if stop || space {
            n = i + c.len_utf8();
        } else {
            break;
        }
    }
    n
}

/// Length of an elapsed time (like `12s` or `1m 05s`) at the start of `chars`,
/// allowing leading spaces; 0 if there is none.
fn timer_len(s: &str) -> usize {
    let s: String = s.chars().take(10).collect();
    let start = s
        .find(|c: char| !c.is_ascii_whitespace())
        .unwrap_or(s.len());
    let t = &s[start..];
    let digits = |t: &str| -> Option<usize> {
        let n = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        (n > 0).then_some(n)
    };
    let mut pos = match digits(t) {
        Some(n) => n,
        None => return 0,
    };
    if t[pos..].starts_with('m') {
        pos += 1;
        if t[pos..].starts_with(' ') {
            pos += 1;
        }
        match digits(&t[pos..]) {
            Some(n) => pos += n,
            None => return 0,
        }
    }
    if t[pos..].starts_with('s') {
        pos += 1;
    } else {
        return 0;
    }
    start + pos
}

fn mask_timers(screen: &str) -> String {
    const SPINNER: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";
    let mut out = String::new();
    let mut chars = screen.chars().peekable();
    while let Some(c) = chars.next() {
        if !SPINNER.contains(c) {
            out.push(c);
            continue;
        }
        out.push('*');
        let rest: String = chars.clone().take(9).collect();
        if rest.starts_with(" working ") {
            out.push_str(" working");
            chars.nth(7);
        }
        // Elapsed time: e.g. `12s`, `1m 05s` (digits, units, one inner space).
        let timer = timer_len(&chars.clone().take(10).collect::<String>());
        if timer > 0 {
            out.push_str(" <t>");
            for _ in 0..timer {
                chars.next();
            }
        }
    }
    out
}

#[test]
fn masking() {
    assert_eq!(mask_timers("── x  ⠸ 12s ──"), "── x  * <t> ──");
    assert_eq!(mask_timers("│  ⠋ working 1m 05s  │"), "│  * working <t>  │");
}

#[tokio::test(flavor = "multi_thread")]
async fn full_turn_with_approval_then_resume() {
    let e = env(vec![
        sse(&[
            json!({"choices":[{"delta":{"reasoning_content":"I'll run it."}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"shell","arguments":"{\"command\":\"echo hello && seq 1 9\"}"}}]}}]}),
            json!({"choices":[],"usage":{"prompt_tokens":900,"completion_tokens":30}}),
        ]),
        sse(&[
            json!({"choices":[{"delta":{"content":"## Done\n\n- printed `hello`\n- counted to **9**"}}]}),
            json!({"choices":[],"usage":{"prompt_tokens":1500,"completion_tokens":20}}),
        ]),
    ], true)
    .await;

    let mut tui = e.tui(None).await;
    tui.type_text("run it\n");
    let screen = tui
        .wait_for(WAIT, |s| s.contains("Allow shell?"))
        .await
        .unwrap();
    assert_eq!(tui.context(), "popup");
    snapshot("approval", &screen, e.work.path());

    // Keys are ignored briefly after the prompt appears.
    tokio::time::sleep(Duration::from_millis(350)).await;
    tui.press("y").unwrap();
    let screen = tui
        .wait_for(WAIT, |s| {
            s.contains("counted to 9") && !s.contains("working")
        })
        .await
        .unwrap_or_else(|s| panic!("turn did not finish:\n{s}"));
    snapshot("finished_turn", &screen, e.work.path());

    // A second UI resumes the newest session and sees the same transcript.
    tui.type_text("/quit\n");
    assert_eq!(tui.quit(), Some(None));
    let mut again = e.tui(Some(None)).await;
    let screen = again
        .wait_for(WAIT, |s| s.contains("counted to 9"))
        .await
        .unwrap();
    assert!(screen.contains("› run it"), "{screen}");
    assert!(screen.contains("$ echo hello && seq 1 9"), "{screen}");

    // The session list shows it.
    again.press("ctrl+o").unwrap();
    let screen = again
        .wait_for(WAIT, |s| s.contains("enter open"))
        .await
        .unwrap();
    assert!(screen.contains("run it"), "{screen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_tool_and_startup_screen() {
    let e = env(vec![
        sse(&[json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"write_file","arguments":"{\"path\":\"x.txt\",\"content\":\"hi\"}"}}]}}]})]),
        sse(&[json!({"choices":[{"delta":{"content":"Okay, I won't."}}]})]),
    ], true)
    .await;
    let mut tui = e.tui(None).await;
    snapshot("startup", &tui.screen(), e.work.path());

    tui.type_text("make x.txt\n");
    tui.wait_for(WAIT, |s| s.contains("Allow write_file?"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    tui.press("n").unwrap();
    let screen = tui
        .wait_for(WAIT, |s| s.contains("Okay, I won't."))
        .await
        .unwrap();
    assert!(screen.contains("✕ write_file x.txt"), "{screen}");
    assert!(
        screen.contains("The user denied this tool call."),
        "{screen}"
    );
    assert!(!e.work.path().join("x.txt").exists());
}

/// Without plugins the runtime's standard UI draws the screen.
#[tokio::test(flavor = "multi_thread")]
async fn standard_ui_without_plugins() {
    let e = env(
        vec![sse(&[
            json!({"choices":[{"delta":{"reasoning_content":"The user said hello.\n"}}]}),
            json!({"choices":[{"delta":{"content":"\n\nhi there"}}]}),
        ])],
        false,
    )
    .await;
    let mut tui = e.tui(None).await;
    tui.settle(Duration::from_millis(50)).await;
    assert!(tui.screen().contains("New session."), "{}", tui.screen());
    tui.type_text("hello\n");
    let screen = tui
        .wait_for(WAIT, |s| s.contains("hi there"))
        .await
        .unwrap();
    snapshot("standard", &screen, e.work.path());
}

/// The files `bone --init` writes load cleanly on both sides.
#[tokio::test(flavor = "multi_thread")]
async fn init_templates_load() {
    let config = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bone"))
        .arg("--init")
        .env("BONE_CONFIG_DIR", config.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(config.path().join("core.lua").exists() && config.path().join("tui.lua").exists());

    // The template leaves the provider to the user; env fills it in here.
    let env = |k: &str| match k {
        "BONE_BASE_URL" => Some("http://127.0.0.1:9/v1".to_owned()),
        "BONE_MODEL" => Some("m".to_owned()),
        _ => None,
    };
    let loaded = scripting::load_with(config.path(), &env).unwrap();
    let server = Server::new(Arc::new(Core::from_loaded(loaded)));
    let work = tempfile::tempdir().unwrap();
    let opts = RunOptions {
        cwd: work.path().to_string_lossy().into_owned(),
        config_dir: Some(config.path().to_owned()),
        resume: None,
        reload_core: true,
    };
    let mut tui = Headless::start(server.connect_in_process(), opts, 80, 24)
        .await
        .unwrap();
    tui.settle(Duration::from_millis(50)).await;
    let screen = tui.screen();
    assert!(!screen.to_lowercase().contains("error"), "{screen}");
    // A command from the template works.
    tui.type_text("/where\n");
    tui.wait_for(WAIT, |s| s.contains("new session (not started yet)"))
        .await
        .unwrap();
}

/// A choice made with a key is saved by the core and back after a restart.
#[tokio::test(flavor = "multi_thread")]
async fn settings_survive_a_restart() {
    let e = env(vec![], false).await;
    std::fs::write(
        e.config.path().join("tui.lua"),
        r#"bone.ui.statusline = function() return { "detail=" .. bone.o.tool_detail } end"#,
    )
    .unwrap();
    let mut tui = e.tui(None).await;
    tui.wait_for(WAIT, |s| s.contains("detail=summary"))
        .await
        .unwrap();
    tui.press("ctrl+t").unwrap();
    tui.wait_for(WAIT, |s| s.contains("detail=rows"))
        .await
        .unwrap();
    let file = e.config.path().join("settings.json");
    let deadline = std::time::Instant::now() + WAIT;
    while !std::fs::read_to_string(&file).is_ok_and(|t| t.contains("\"rows\"")) {
        assert!(
            std::time::Instant::now() < deadline,
            "settings.json was not written"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved, json!({ "tui": { "tool_detail": "rows" } }));

    // A new TUI starts with it.
    let mut again = e.tui(None).await;
    again
        .wait_for(WAIT, |s| s.contains("detail=rows"))
        .await
        .unwrap();
}

/// No provider at all: setup opens by itself, adds one (its key in
/// secrets.json), and the next message goes to it.
#[tokio::test(flavor = "multi_thread")]
async fn first_run_setup_adds_a_provider() {
    let url = fake_model(vec![sse(&[
        json!({"choices":[{"delta":{"content":"hello from fake"}}]}),
    ])])
    .await;
    let config = tempfile::tempdir().unwrap();
    std::fs::write(config.path().join("core.lua"), "-- nothing yet\n").unwrap();
    // The catalog step finds no catalog (no network in tests) and moves on.
    std::fs::write(
        config.path().join("settings.json"),
        r#"{ "catalog": { "url": "/nonexistent/catalog" } }"#,
    )
    .unwrap();
    let work = tempfile::tempdir().unwrap();
    let loaded = scripting::load_with(config.path(), &|_| None).unwrap();
    let server = Server::new(Arc::new(Core::from_loaded(loaded)));
    let e = Env {
        config,
        work,
        server,
    };

    let mut tui = e.tui(None).await;
    tui.wait_for(WAIT, |s| s.contains("Welcome to bone"))
        .await
        .unwrap();
    tui.press("enter").unwrap(); // to the providers
    tui.press("enter").unwrap(); // a server on this machine
    tui.wait_for(WAIT, |s| s.contains("› Model")).await.unwrap();
    tui.press("up").unwrap(); // URL
    tui.press("enter").unwrap();
    tui.press("ctrl+u").unwrap();
    tui.type_text(&url);
    tui.press("enter").unwrap();
    tui.press("down").unwrap(); // Model
    tui.press("enter").unwrap();
    tui.type_text("fake");
    tui.press("enter").unwrap();
    tui.press("down").unwrap(); // API key
    tui.press("enter").unwrap();
    tui.type_text("sk-test");
    tui.press("enter").unwrap();
    tui.press("down").unwrap();
    tui.press("down").unwrap(); // Continue
    tui.press("enter").unwrap();
    tui.wait_for(WAIT, |s| s.contains("not reachable"))
        .await
        .unwrap();
    tui.press("enter").unwrap(); // Continue (no packages)
    tui.wait_for(WAIT, |s| s.contains("Done.")).await.unwrap();
    tui.press("enter").unwrap();

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(e.config.path().join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["provider"], "local");
    assert_eq!(settings["providers"]["local"]["model"], "fake");
    let secrets = std::fs::read_to_string(e.config.path().join("secrets.json")).unwrap();
    assert!(secrets.contains("sk-test"));
    assert!(
        !std::fs::read_to_string(e.config.path().join("settings.json"))
            .unwrap()
            .contains("sk-test")
    );

    // The provider is in use: a message gets the fake model's reply.
    tui.type_text("hi\n");
    tui.wait_for(WAIT, |s| s.contains("hello from fake"))
        .await
        .unwrap();

    // With a provider now, a new TUI does not open setup.
    let mut again = e.tui(None).await;
    again.settle(Duration::from_millis(200)).await;
    assert!(
        !again.screen().contains("Welcome to bone"),
        "{}",
        again.screen()
    );
}
