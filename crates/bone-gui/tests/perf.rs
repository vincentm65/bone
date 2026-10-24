//! Timings for the desktop GUI. Ignored in the normal suite:
//! `cargo test -p bone-gui --release --test perf -- --ignored --nocapture`

use std::path::Path;
use std::time::Duration;

use bone_core::Core;
use bone_core::scripting;
use bone_gui::{GuiOptions, Headless};
use bone_proto::methods::{MessageCompletedParams, ToolFinishedParams, TurnFinishedParams};
use bone_proto::types::{ChatMessage, ToolCall, TurnOutcome, Usage};
use bone_server::Server;

async fn gui(config: &Path, work: &Path) -> Headless {
    std::fs::write(
        config.join("core.lua"),
        r#"bone.config.providers.fake = { base_url = "http://127.0.0.1:9", model = "fake" }"#,
    )
    .unwrap();
    std::fs::write(config.join("gui-library"), "desktop\n").unwrap();
    let loaded = scripting::load_with(config, &|_| None).unwrap();
    let server = Server::new(std::sync::Arc::new(Core::from_loaded(loaded)));
    let opts = GuiOptions {
        cwd: work.to_string_lossy().into_owned(),
        reconnect: None,
        config_dir: Some(config.to_owned()),
    };
    Headless::start(server.connect_in_process(), opts, 1440.0, 900.0)
        .await
        .unwrap()
}

fn answer(n: usize) -> String {
    format!(
        r#"## Turn {n}: laying out a reply

A paragraph of ordinary text, long enough to wrap across the column, with `code`
spans, **bold** words and a [link](https://example.com) so the markdown becomes
more than one run of glyphs. The point is the window doing real work: wrapping,
measuring and painting, not an empty label.

- first bullet with some extra words so the row is not tiny
- second bullet, still wrapping, with `inline code` in it
- third bullet

> A quoted line that is also its own block and should not be free.

```
fn work(i: usize) -> String {{
    let mut out = String::new();
    for n in 0..24 {{
        out.push_str(&format!("line {{n}} of item {{i}}\n"));
    }}
    out
}}
```

| name | role | notes |
| --- | --- | --- |
| alpha | read | a short cell |
| beta | search | another cell that is a bit wider |
| gamma | edit | the last row of the table |

And a closing paragraph so the item does not end on the table. Turn {n} again,
with a few more words to push the height around: the quick brown fox jumps
over the lazy dog, repeatedly, until the line wraps at least once more.
"#
    )
}

fn shell_out() -> String {
    (0..40)
        .map(|i| format!("cargo test line {i}: ok in 0.0{i}s"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn fill(g: &mut Headless) {
    let chat = &mut g.gui.chats[0];
    for n in 0..24 {
        let turn = (n + 1) as u64;
        chat.turn_started(turn, &format!("please look at item {n} and explain it"));
        let call = format!("c{n}");
        chat.message_completed(&MessageCompletedParams {
            session_id: "s".into(),
            turn_id: turn,
            message: ChatMessage::Assistant {
                content: answer(n),
                reasoning: "Weighing two ways to explain this, then picking the shorter one."
                    .into(),
                tool_calls: vec![ToolCall {
                    id: call.clone(),
                    name: "shell".into(),
                    arguments: format!(r#"{{"command":"cargo test -q {n}"}}"#),
                }],
                reasoning_state: None,
            },
            usage: Some(Usage {
                input_tokens: 8000 + n as u64 * 400,
                output_tokens: 900,
                cached_tokens: Some(6000),
                context_tokens: Some(12000),
            }),
        });
        chat.tool_finished(&ToolFinishedParams {
            session_id: "s".into(),
            turn_id: turn,
            call_id: call,
            output: shell_out(),
            is_error: false,
            duration_ms: Some(120 + n as u64),
        });
        chat.turn_finished(&TurnFinishedParams {
            session_id: "s".into(),
            turn_id: turn,
            outcome: TurnOutcome::Completed,
        });
    }
    g.gui.view.invalidate();
}

fn stats(samples: &[f64]) -> String {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    let sum: f64 = s.iter().sum();
    let p = |q: f64| s[((n as f64 - 1.0) * q).round() as usize];
    format!(
        "n={n} mean={:.2}ms median={:.2}ms p95={:.2}ms min={:.2}ms max={:.2}ms",
        sum / n as f64 * 1000.0,
        p(0.5) * 1000.0,
        p(0.95) * 1000.0,
        s[0] * 1000.0,
        s[n - 1] * 1000.0
    )
}

fn bench(g: &mut Headless, name: &str, warm: usize, n: usize, mut each: impl FnMut(&mut Headless)) {
    bone_gui::profile_reset();
    for _ in 0..warm {
        each(g);
        g.frame();
    }
    bone_gui::profile_reset();
    let mut samples = Vec::with_capacity(n);
    let mut tess = Vec::with_capacity(n);
    for _ in 0..n {
        each(g);
        g.frame();
        samples.push(g.ui_secs);
        tess.push(g.tess_secs);
    }
    println!("{name}: {}", stats(&samples));
    println!("  tess: {}", stats(&tess));
    println!("  {}", bone_gui::profile_report());
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn desktop_frames() {
    bone_gui::enable_profile(true);
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());

    bench(&mut g, "welcome", 3, 20, |_| {});

    // A real animation tick: the field redraws at its fps, so wait one out.
    // Back-to-back frames above reuse the mesh, which is the other case.
    bench(&mut g, "welcome-tick", 1, 8, |_| {
        std::thread::sleep(Duration::from_millis(55));
    });

    // Composing the first message, while the field is still up.
    bench(&mut g, "welcome-type", 1, 20, |g| {
        g.text("a");
    });
    g.gui.draft.clear();

    fill(&mut g);
    // Land on the latest items, then sit.
    for _ in 0..6 {
        g.frame();
    }
    bench(&mut g, "chat-idle-end", 2, 20, |_| {});

    g.gui
        .run_lua("t", "bone.gui.scroll('chat', 'top')")
        .unwrap();
    for _ in 0..4 {
        g.frame();
    }
    bench(&mut g, "chat-idle-top", 2, 20, |_| {});

    // One tall reply: the list keeps a whole item, including the part
    // above and below the viewport.
    let long = {
        let mut s = String::new();
        for i in 0..400 {
            s.push_str(&format!(
                "\n\nParagraph {i} discusses the change in `module_{i}.rs` with enough prose to wrap twice on the chat column, plus a list:\n\n- point one about behavior {i}\n- point two, still specific\n\n"
            ));
        }
        s
    };
    g.gui.chats[0].turn_started(80, "review the whole diff");
    g.gui.chats[0].message_completed(&MessageCompletedParams {
        session_id: "s".into(),
        turn_id: 80,
        message: ChatMessage::Assistant {
            content: long,
            reasoning: String::new(),
            tool_calls: vec![],
            reasoning_state: None,
        },
        usage: Some(Usage {
            input_tokens: 20000,
            output_tokens: 8000,
            cached_tokens: Some(10000),
            context_tokens: Some(28000),
        }),
    });
    g.gui.chats[0].turn_finished(&TurnFinishedParams {
        session_id: "s".into(),
        turn_id: 80,
        outcome: TurnOutcome::Completed,
    });
    g.gui.view.invalidate();
    for _ in 0..4 {
        g.frame();
    }
    bench(&mut g, "long-reply", 2, 12, |_| {});

    // Wheel over the chat column. Scrolling does not rebuild the Lua tree.
    g.hover_at(720.0, 400.0);
    bench(&mut g, "chat-scroll", 1, 15, |g| {
        g.hover_at(720.0, 400.0);
        g.wheel(0.0, -180.0);
    });

    bench(&mut g, "typing", 1, 20, |g| {
        g.text("a");
    });

    // A turn in progress: the spinner asks the view to be built again, and
    // the answer grows.
    g.gui.chats[0].turn_started(100, "stream please");
    g.gui.chats[0].delta(&bone_proto::methods::MessageDeltaParams {
        session_id: "s".into(),
        turn_id: 100,
        kind: bone_proto::types::DeltaKind::Text,
        text: answer(100),
    });
    g.gui.view.invalidate();
    for _ in 0..4 {
        g.frame();
    }
    bench(&mut g, "streaming", 1, 12, |g| {
        g.gui.chats[0].delta(&bone_proto::methods::MessageDeltaParams {
            session_id: "s".into(),
            turn_id: 100,
            kind: bone_proto::types::DeltaKind::Text,
            text: " more words to append onto the streaming answer so markdown is redone.".into(),
        });
        g.gui.view.invalidate();
        // The running turn's spinner also schedules a rebuild.
        std::thread::sleep(Duration::from_millis(1));
    });

    g.gui
        .run_lua("t", "require('bone.desktop.sidebar').open()")
        .unwrap();
    // Exercise list bookkeeping independently of Lua/Markdown rendering.
    let chat = &mut g.gui.chats[0];
    for n in 0..5000 {
        let turn = (1000 + n) as u64;
        chat.turn_started(turn, "show the next row");
        chat.message_completed(&MessageCompletedParams {
            session_id: "s".into(),
            turn_id: turn,
            message: ChatMessage::Assistant {
                content: format!("row {n}"),
                reasoning: String::new(),
                tool_calls: vec![],
                reasoning_state: None,
            },
            usage: None,
        });
        chat.turn_finished(&TurnFinishedParams {
            session_id: "s".into(),
            turn_id: turn,
            outcome: TurnOutcome::Completed,
        });
    }
    g.gui.view.invalidate();
    for _ in 0..4 {
        g.frame();
    }
    g.gui
        .run_lua("t", "bone.gui.scroll('chat', 'end')")
        .unwrap();
    for _ in 0..3 {
        g.frame();
    }
    bench(&mut g, "list-10000-idle-end", 2, 20, |_| {});
    g.gui
        .run_lua("t", "bone.gui.scroll('chat', 'top')")
        .unwrap();
    for _ in 0..3 {
        g.frame();
    }
    bench(&mut g, "list-10000-idle-top", 2, 20, |_| {});

    g.gui.chats[0].turn_started(100000, "stream a large transcript");
    g.gui.chats[0].delta(&bone_proto::methods::MessageDeltaParams {
        session_id: "s".into(),
        turn_id: 100000,
        kind: bone_proto::types::DeltaKind::Text,
        text: "initial streaming text".into(),
    });
    g.gui.view.invalidate();
    for _ in 0..4 {
        g.frame();
    }
    bench(&mut g, "list-10000-streaming", 1, 12, |g| {
        g.gui.chats[0].delta(&bone_proto::methods::MessageDeltaParams {
            session_id: "s".into(),
            turn_id: 100000,
            kind: bone_proto::types::DeltaKind::Text,
            text: " more streaming text".into(),
        });
        g.gui.view.invalidate();
        std::thread::sleep(Duration::from_millis(1));
    });

    bench(&mut g, "sidebar-slide", 0, 16, |_| {});
}
