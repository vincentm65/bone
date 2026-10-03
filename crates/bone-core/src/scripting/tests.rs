use std::time::Duration;

use super::*;

fn write(dir: &Path, core_lua: &str) {
    std::fs::write(dir.join("core.lua"), core_lua).unwrap();
}

fn no_env(_: &str) -> Option<String> {
    None
}

const PROVIDER: &str = r#"bone.config.providers.x = { base_url = "http://x/v1", model = "m" }"#;

async fn next_event(events: &mut tokio_mpsc::UnboundedReceiver<AskEvent>) -> AskEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("event")
        .expect("open")
}

#[test]
fn config_and_env_overrides() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        r#"
        bone.config.providers.a = { base_url = "http://a/v1", model = "ma" }
        bone.config.providers.b = { base_url = "http://b/v1", model = "mb", api_key = "kb", stream_usage = false }
        bone.config.provider = "b"
        bone.config.data_dir = "~/somewhere"
        "#,
    );
    let lua = setup(
        dir.path(),
        &Default::default(),
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let ex = extract(&lua).unwrap();
    let c = resolve(dir.path(), &ex, &no_env).unwrap();
    assert_eq!(
        (c.provider.model.as_str(), c.provider.api_key.as_deref()),
        ("mb", Some("kb"))
    );
    assert!(!c.provider.stream_usage);
    assert!(c.data_dir.ends_with("somewhere") && !c.data_dir.starts_with("~"));
    assert!(c.system_prompt.unwrap().starts_with("You are bone"));

    let env = |k: &str| match k {
        "BONE_MODEL" => Some("override".into()),
        "BONE_DATA_DIR" => Some("/d".into()),
        _ => None,
    };
    let c = resolve(dir.path(), &ex, &env).unwrap();
    assert_eq!(
        (c.provider.base_url.as_str(), c.provider.model.as_str()),
        ("http://b/v1", "override")
    );
    assert_eq!(c.data_dir, PathBuf::from("/d"));

    let env = |k: &str| (k == "BONE_BASE_URL").then(|| "http://env/v1".to_string());
    let c = resolve(dir.path(), &ex, &env).unwrap();
    assert_eq!(
        (c.provider.base_url.as_str(), c.provider.model.as_str()),
        ("http://env/v1", "mb")
    );
}

#[test]
fn missing_or_bad_config_is_explained() {
    let dir = tempfile::tempdir().unwrap();
    let ex = extract(
        &setup(
            dir.path(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap(),
    )
    .unwrap();
    let err = resolve(dir.path(), &ex, &no_env).unwrap_err();
    assert!(err.contains("no model provider configured"), "{err}");
    let env = |k: &str| match k {
        "BONE_BASE_URL" => Some("http://x".into()),
        "BONE_MODEL" => Some("m".into()),
        _ => None,
    };
    assert_eq!(resolve(dir.path(), &ex, &env).unwrap().data_dir, dir.path());

    write(dir.path(), r#"bone.config.provider = "nope""#);
    let ex = extract(
        &setup(
            dir.path(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        resolve(dir.path(), &ex, &no_env)
            .unwrap_err()
            .contains("no such entry")
    );

    write(dir.path(), "this is not lua");
    assert!(load(dir.path()).err().unwrap().contains("core.lua"));
}

#[tokio::test]
async fn hooks_tools_and_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            {PROVIDER}
            bone.config.system_prompt = function(ctx) return "prompt for " .. ctx.cwd end
            bone.tool.register {{
              name = "shout",
              description = "Upper-case text",
              parameters = {{ type = "object", properties = {{ text = {{ type = "string" }} }} }},
              run = function(args, ctx)
                if args.text == "" then return nil, "nothing to shout" end
                if args.text == "table" then return {{ a = 1 }} end
                if args.text == "boom" then error("exploded") end
                print("shouting", args.text)
                return args.text:upper() .. " in " .. ctx.cwd .. " " .. bone.system("echo hi").stdout
              end,
            }}
            bone.hook("tool_call", function(ev)
              if ev.name == "shell" and ev.arguments.command:match("rm %-rf") then return {{ deny = "no rm -rf" }} end
              if ev.name == "read_file" then return {{ arguments = {{ path = "x" .. ev.arguments.path }} }} end
            end)
            bone.hook("tool_call", function(ev)
              if ev.name == "read_file" and ev.arguments.path ~= "xa" then error("saw old args") end
              if ev.name == "explode" then error("hook bug") end
            end)
            bone.hook("tool_result", function(ev)
              if ev.is_error then return {{ output = "[hidden] " .. ev.output, is_error = false }} end
            end)
            -- A custom hook point, run from Lua.
            bone.hook("greet", function(ev) return {{ text = "hello " .. ev.name }} end)
            bone.tool.register {{
              name = "greet",
              run = function(args) local ev = bone.run_hooks("greet", {{ name = args.name }}); return ev.text end,
            }}
            "#
        ),
    );
    let loaded = load(dir.path()).unwrap();
    let s = loaded.scripting.clone();
    assert!(s.has_hook("tool_call") && s.has_hook("tool_result") && s.has_system_prompt_fn());
    assert_eq!(s.system_prompt("/w", "s").await.unwrap(), "prompt for /w");

    let tool = |name: &str| {
        LuaTool::new(
            loaded
                .tools
                .iter()
                .find(|t| t.name == name)
                .unwrap()
                .clone(),
            s.clone(),
            false,
        )
    };
    let ctx = ToolContext {
        cwd: "/w".into(),
        session_id: "s".into(),
        cancel: Default::default(),
    };
    let shout = tool("shout");
    assert_eq!(
        shout.call(json!({"text": "hey"}), &ctx).await.unwrap(),
        "HEY in /w hi\n"
    );
    assert_eq!(
        shout.call(json!({"text": ""}), &ctx).await.unwrap_err(),
        "nothing to shout"
    );
    assert_eq!(
        shout.call(json!({"text": "table"}), &ctx).await.unwrap(),
        "{\n  \"a\": 1\n}"
    );
    assert!(
        shout
            .call(json!({"text": "boom"}), &ctx)
            .await
            .unwrap_err()
            .contains("exploded")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("core.log")).unwrap(),
        "shouting\they\n"
    );
    assert_eq!(
        tool("greet")
            .call(json!({"name": "bo"}), &ctx)
            .await
            .unwrap(),
        "hello bo"
    );

    let ev = |name: &str, args: Json| json!({ "id": "c", "name": name, "arguments": args, "session_id": "s" });
    let out = s
        .hooks("tool_call", ev("shell", json!({"command": "rm -rf /"})))
        .await;
    assert_eq!(out.deny.as_deref(), Some("no rm -rf"));
    let out = s
        .hooks("tool_call", ev("read_file", json!({"path": "a"})))
        .await;
    assert_eq!(
        (out.deny, out.event["arguments"].clone()),
        (None, json!({"path": "xa"}))
    );
    let out = s.hooks("tool_call", ev("explode", json!({}))).await;
    assert!(out.deny.unwrap().contains("hook bug"));
    let out = s
        .hooks("tool_result", json!({"output": "bad", "is_error": true}))
        .await;
    assert_eq!(
        (
            out.event["output"].as_str(),
            out.event["is_error"].as_bool()
        ),
        (Some("[hidden] bad"), Some(false))
    );
    // No hooks for a point: the event comes back as it was.
    let out = s.hooks("nothing", json!({"x": 1})).await;
    assert_eq!((out.deny, out.event), (None, json!({"x": 1})));
}

#[tokio::test]
async fn ask_waits_for_an_answer_or_a_cancel() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            {PROVIDER}
            bone.tool.register {{
              name = "pick",
              run = function(args)
                local a = bone.ask({{ kind = "choice", options = {{ "red", "blue" }} }})
                return "picked " .. tostring(a)
              end,
            }}
            "#
        ),
    );
    let mut loaded = load(dir.path()).unwrap();
    let s = loaded.scripting.clone();
    let spec = loaded
        .tools
        .iter()
        .find(|t| t.name == "pick")
        .unwrap()
        .clone();
    let tool = std::sync::Arc::new(LuaTool::new(spec, s.clone(), false));

    let call = |session: &str| {
        let tool = tool.clone();
        let ctx = ToolContext {
            cwd: "/".into(),
            session_id: session.into(),
            cancel: Default::default(),
        };
        tokio::spawn(async move { tool.call(json!({}), &ctx).await })
    };
    // Two sessions ask at once; answering one doesn't block the other.
    let first = call("s1");
    let AskEvent::Requested(q1) = next_event(&mut loaded.events).await else {
        panic!()
    };
    assert_eq!(
        (q1.session_id.as_deref(), q1.question["kind"].as_str()),
        (Some("s1"), Some("choice"))
    );
    let second = call("s2");
    let AskEvent::Requested(q2) = next_event(&mut loaded.events).await else {
        panic!()
    };
    assert!(s.answer(q2.ask_id, json!("blue")).await);
    assert!(
        matches!(next_event(&mut loaded.events).await, AskEvent::Resolved(r) if r.answer == "blue")
    );
    assert_eq!(second.await.unwrap().unwrap(), "picked blue");

    // Cancelling the session resumes its question with nil.
    s.cancel_session("s1");
    assert!(
        matches!(next_event(&mut loaded.events).await, AskEvent::Resolved(r) if r.answer.is_null())
    );
    assert_eq!(first.await.unwrap().unwrap(), "picked nil");
    assert!(!s.answer(q1.ask_id, json!("late")).await);
}

#[tokio::test]
async fn approve_plugin_asks_before_changes() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("plugins/approve");
    std::fs::create_dir_all(&plugin).unwrap();
    let example =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/approve/core.lua");
    std::fs::copy(example, plugin.join("core.lua")).unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            {PROVIDER}
            bone.config.approve.allow = function(ev) return ev.name == "shell" and ev.arguments.command == "ls" end
            bone.tool.register {{ name = "safe", needs_approval = false, run = function() return "" end }}
            bone.tool.register {{ name = "risky", run = function() return "" end }}
            "#
        ),
    );
    let mut loaded = load(dir.path()).unwrap();
    let s = loaded.scripting.clone();
    let ev = |name: &str, args: Json| json!({ "id": "c", "name": name, "arguments": args, "session_id": "s" });
    let quick = |name: &str, args: Json| {
        let (s, ev) = (s.clone(), ev(name, args));
        async move {
            tokio::time::timeout(Duration::from_millis(500), s.hooks("tool_call", ev))
                .await
                .ok()
        }
    };
    // No question for reads, opted-out tools, or allowed commands.
    for (name, args) in [
        ("read_file", json!({"path": "a"})),
        ("safe", json!({})),
        ("shell", json!({"command": "ls"})),
    ] {
        assert_eq!(quick(name, args).await.and_then(|o| o.deny), None, "{name}");
    }

    let ask = |name: &str, args: Json| {
        let (s, ev) = (s.clone(), ev(name, args));
        tokio::spawn(async move { s.hooks("tool_call", ev).await })
    };
    let pending = ask("shell", json!({"command": "rm x"}));
    let AskEvent::Requested(q) = next_event(&mut loaded.events).await else {
        panic!()
    };
    assert_eq!(
        q.question,
        json!({ "kind": "approval", "title": "Allow shell?", "tool": "shell", "arguments": {"command": "rm x"} })
    );
    s.answer(q.ask_id, json!("deny")).await;
    assert_eq!(
        pending.await.unwrap().deny.as_deref(),
        Some("The user denied this tool call.")
    );
    next_event(&mut loaded.events).await; // resolved

    let pending = ask("risky", json!({}));
    let AskEvent::Requested(q) = next_event(&mut loaded.events).await else {
        panic!()
    };
    s.answer(q.ask_id, json!("always")).await;
    assert_eq!(pending.await.unwrap().deny, None);
    next_event(&mut loaded.events).await;
    // "always" holds for that tool in that session.
    assert_eq!(quick("risky", json!({})).await.map(|o| o.deny), Some(None));
}

#[tokio::test]
async fn plugins_run_before_user_config() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("plugins/git");
    std::fs::create_dir_all(&plugin).unwrap();
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/git/core.lua");
    std::fs::copy(example, plugin.join("core.lua")).unwrap();
    std::fs::create_dir_all(dir.path().join("plugins/.hidden")).unwrap();
    std::fs::write(
        dir.path().join("plugins/.hidden/core.lua"),
        "error('must not load')",
    )
    .unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            {PROVIDER}
            assert(bone._tools.git_status, "plugin loaded first")
            assert(table.concat(bone.plugins, ",") == "git", table.concat(bone.plugins, ","))
            "#
        ),
    );
    let loaded = load(dir.path()).unwrap();
    let spec = loaded
        .tools
        .iter()
        .find(|t| t.name == "git_status")
        .unwrap()
        .clone();

    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .output()
            .unwrap()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("new.txt"), "x").unwrap();
    let tool = LuaTool::new(spec, loaded.scripting.clone(), false);
    let ctx = ToolContext {
        cwd: repo.path().to_owned(),
        session_id: "s".into(),
        cancel: Default::default(),
    };
    assert_eq!(
        tool.call(json!({}), &ctx).await.unwrap(),
        "## No commits yet on main\n?? new.txt\n"
    );
}

/// A Lua tool by name, ready to call.
fn lua_tool(loaded: &Loaded, name: &str) -> std::sync::Arc<LuaTool> {
    let spec = loaded
        .tools
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .clone();
    std::sync::Arc::new(LuaTool::new(spec, loaded.scripting.clone(), false))
}

fn ctx(session: &str) -> ToolContext {
    ToolContext {
        cwd: "/".into(),
        session_id: session.into(),
        cancel: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn system_sleep_and_http_wait_without_blocking() {
    // A tiny HTTP server for bone.http.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/x", listener.local_addr().unwrap());
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = sock.read(&mut buf).await.unwrap();
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = if req.contains("x-test: yes") {
            "hello"
        } else {
            "no header"
        };
        let resp = format!(
            "HTTP/1.1 201 Created\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });

    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            {PROVIDER}
            -- Blocking fallback while core.lua loads.
            assert(bone.system("echo loading").stdout == "loading\n")
            bone.tool.register {{ name = "slow", run = function(args)
              bone.sleep(100)
              local r = bone.system("sleep 0.3; printf %s " .. args.tag)
              if r == nil then return "cancelled" end
              return r.stdout .. " " .. r.code
            end }}
            bone.tool.register {{ name = "fetch", run = function(args)
              local r = bone.http({{ url = args.url, headers = {{ ["x-test"] = "yes" }} }})
              return r.status .. " " .. r.body
            end }}
            bone.tool.register {{ name = "stdin", run = function()
              local r = bone.system("tr a-z A-Z", {{ stdin = "abc" }})
              local t = bone.system("sleep 5", {{ timeout = 50 }})
              return r.stdout .. " " .. tostring(t.timed_out)
            end }}
            bone.tool.register {{ name = "bad", run = function()
              return bone.http({{ url = "http://127.0.0.1:1/" }})
            end }}
            "#
        ),
    );
    let loaded = load(dir.path()).unwrap();
    let slow = lua_tool(&loaded, "slow");

    // Two slow calls overlap: about 0.4s in all, not 0.8s.
    let started = std::time::Instant::now();
    let (c1, c2) = (ctx("s1"), ctx("s2"));
    let (a, b) = tokio::join!(
        slow.call(json!({"tag": "a"}), &c1),
        slow.call(json!({"tag": "b"}), &c2)
    );
    assert_eq!((a.unwrap(), b.unwrap()), ("a 0".into(), "b 0".into()));
    assert!(
        started.elapsed() < Duration::from_millis(700),
        "{:?}",
        started.elapsed()
    );

    let fetch = lua_tool(&loaded, "fetch");
    assert_eq!(
        fetch.call(json!({"url": url}), &ctx("s")).await.unwrap(),
        "201 hello"
    );
    let stdin = lua_tool(&loaded, "stdin");
    assert_eq!(stdin.call(json!({}), &ctx("s")).await.unwrap(), "ABC true");
    // Connection errors raise in Lua, which the tool reports.
    let bad = lua_tool(&loaded, "bad");
    assert!(
        bad.call(json!({}), &ctx("s"))
            .await
            .unwrap_err()
            .contains("127.0.0.1:1")
    );

    // Cancelling the session stops its command at once; others carry on.
    let started = std::time::Instant::now();
    let s = loaded.scripting.clone();
    let cancelled = tokio::spawn({
        let slow = slow.clone();
        async move { slow.call(json!({"tag": "c"}), &ctx("s3")).await }
    });
    let other = tokio::spawn({
        let slow = slow.clone();
        async move { slow.call(json!({"tag": "d"}), &ctx("s4")).await }
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    s.cancel_session("s3");
    assert_eq!(cancelled.await.unwrap().unwrap(), "cancelled");
    assert!(started.elapsed() < Duration::from_millis(300));
    assert_eq!(other.await.unwrap().unwrap(), "d 0");
}

/// Serve canned SSE bodies, one per request, recording request bodies.
async fn fake_sse(bodies: Vec<String>) -> (String, Arc<std::sync::Mutex<Vec<Json>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen: Arc<std::sync::Mutex<Vec<Json>>> = Default::default();
    let seen2 = seen.clone();
    tokio::spawn(async move {
        for body in bodies {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map(|v| v.trim().parse().unwrap())
                        .unwrap_or(0);
                    while buf.len() < i + 4 + len {
                        let n = sock.read(&mut chunk).await.unwrap();
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    assert!(head.contains("x-api-key: k"), "{head}");
                    seen2
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&buf[i + 4..i + 4 + len]).unwrap());
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
    (url, seen)
}

fn anthropic_sse(events: &[Json]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn lua_provider_streams_and_calls_tools() {
    use crate::Core;
    use bone_proto::methods::*;
    let (url, seen) = fake_sse(vec![
        anthropic_sse(&[
            json!({"type":"message_start","message":{"usage":{"input_tokens":12}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"stamp it"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"stamp"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"word\":"}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"hi\"}"}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_delta","usage":{"output_tokens":7}}),
            json!({"type":"message_stop"}),
        ]),
        anthropic_sse(&[
            json!({"type":"message_start","message":{"usage":{"input_tokens":30}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Stamped "}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"HI."}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","usage":{"output_tokens":3}}),
        ]),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("plugins/anthropic");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/anthropic/core.lua"),
        plugin.join("core.lua"),
    )
    .unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            bone.config.providers.claude = {{ type = "anthropic", model = "claude-test", api_key = "k", base_url = "{url}" }}
            bone.config.system_prompt = "Be brief."
            bone.tool.register {{ name = "stamp", run = function(a) return a.word:upper() end }}
            "#
        ),
    );
    let loaded = load(dir.path()).unwrap();
    assert_eq!(loaded.config.provider.kind.as_deref(), Some("anthropic"));
    let core = Core::from_loaded(loaded);
    let mut events = core.subscribe();
    let call = |m: &'static str, p: Json| {
        let core = &core;
        async move { core.handle(m, Some(p)).await.unwrap() }
    };
    let work = tempfile::tempdir().unwrap();
    let info = call(SessionCreate::METHOD, json!({ "cwd": work.path() })).await;
    let sid = info["session_id"].as_str().unwrap().to_owned();
    call(
        TurnStart::METHOD,
        json!({ "session_id": sid, "text": "stamp hi" }),
    )
    .await;

    let mut deltas = String::new();
    let outcome = loop {
        let e = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("event")
            .unwrap();
        if e.method == MessageDelta::METHOD {
            deltas.push_str(e.params["text"].as_str().unwrap());
        }
        if e.method == TurnFinished::METHOD {
            break e.params["outcome"].clone();
        }
    };
    assert_eq!(outcome["status"], "completed", "{outcome}");
    assert_eq!(deltas, "stamp itStamped HI.");

    let msgs = call(SessionMessages::METHOD, json!({ "session_id": sid })).await;
    let msgs = msgs["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["tool_calls"][0]["arguments"], r#"{"word":"hi"}"#);
    assert_eq!(msgs[1]["reasoning"], "stamp it");
    assert_eq!(msgs[2]["content"], "HI");
    assert_eq!(msgs[3]["content"], "Stamped HI.");

    // What the API received: the system prompt apart, the tool round trip
    // as tool_use and tool_result blocks.
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0]["model"], "claude-test");
    assert!(seen[0]["system"].as_str().unwrap().starts_with("Be brief."));
    assert_eq!(seen[0]["messages"][0]["content"][0]["text"], "stamp hi");
    assert_eq!(
        seen[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["name"] == "stamp")
            .count(),
        1
    );
    let m = &seen[1]["messages"];
    assert_eq!(m[1]["role"], "assistant");
    assert_eq!(m[1]["content"][0]["type"], "tool_use");
    assert_eq!(m[1]["content"][0]["input"]["word"], "hi");
    assert_eq!(m[2]["content"][0]["type"], "tool_result");
    assert_eq!(m[2]["content"][0]["content"], "HI");
}

#[test]
fn unknown_lua_provider_type_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        r#"bone.config.providers.x = { type = "nope", model = "m" }"#,
    );
    let e = load(dir.path()).err().unwrap();
    assert!(e.contains("no Lua provider of type \"nope\""), "{e}");
}

async fn next_core_event(
    events: &mut tokio::sync::broadcast::Receiver<crate::Event>,
) -> crate::Event {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("event")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_stops_a_lua_provider_mid_stream() {
    use crate::Core;
    use bone_proto::methods::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // A server that sends one event and then hangs.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 8192];
        let _ = sock.read(&mut buf).await;
        sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: one\n\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            bone.provider.register("hang", {{ complete = function(req, emit)
              local s = bone.http_stream({{ url = "{url}" }})
              for data in s:events() do emit({{ text = data }}) end
              return {{ content = "never" }}
            end }})
            bone.config.providers.h = {{ type = "hang", model = "m" }}
            "#
        ),
    );
    let core = Core::from_loaded(load(dir.path()).unwrap());
    let mut events = core.subscribe();
    let info = core
        .handle(SessionCreate::METHOD, Some(json!({ "cwd": "/" })))
        .await
        .unwrap();
    let sid = info["session_id"].clone();
    core.handle(
        TurnStart::METHOD,
        Some(json!({ "session_id": sid, "text": "go" })),
    )
    .await
    .unwrap();
    loop {
        let e = next_core_event(&mut events).await;
        if e.method == MessageDelta::METHOD {
            assert_eq!(e.params["text"], "one");
            break;
        }
    }
    let started = std::time::Instant::now();
    core.handle(TurnCancel::METHOD, Some(json!({ "session_id": sid })))
        .await
        .unwrap();
    loop {
        let e = next_core_event(&mut events).await;
        if e.method == TurnFinished::METHOD {
            assert_eq!(e.params["outcome"]["status"], "cancelled");
            break;
        }
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    // The Lua thread is free: another job runs at once.
    let s = core.inner.runtime().scripting.clone().unwrap();
    let out = tokio::time::timeout(Duration::from_secs(2), s.hooks("x", json!({})))
        .await
        .expect("Lua thread free");
    assert_eq!(out.deny, None);
}

/// Serve canned OpenAI SSE bodies, one per request, recording the
/// authorization header and request body of each.
async fn fake_openai(bodies: Vec<String>) -> (String, Arc<std::sync::Mutex<Vec<(String, Json)>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen: Arc<std::sync::Mutex<Vec<(String, Json)>>> = Default::default();
    let seen2 = seen.clone();
    tokio::spawn(async move {
        for body in bodies {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    assert!(head.starts_with("post /v1/chat/completions"), "{head}");
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map(|v| v.trim().parse().unwrap())
                        .unwrap_or(0);
                    while buf.len() < i + 4 + len {
                        let n = sock.read(&mut chunk).await.unwrap();
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let auth = head
                        .lines()
                        .find_map(|l| l.strip_prefix("authorization:"))
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    seen2.lock().unwrap().push((
                        auth,
                        serde_json::from_slice(&buf[i + 4..i + 4 + len]).unwrap(),
                    ));
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
    (url, seen)
}

fn openai_sse(chunks: &[Json]) -> String {
    let mut s: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    s.push_str("data: [DONE]\n\n");
    s
}

/// examples/plugins/switch: a Lua router over OpenAI-compatible entries,
/// switched through the shared state between turns.
#[tokio::test(flavor = "multi_thread")]
async fn switch_plugin_routes_to_the_chosen_provider() {
    use crate::Core;
    use bone_proto::methods::*;
    let delta = |d: Json| json!({ "choices": [{ "index": 0, "delta": d }] });
    let (url, seen) = fake_openai(vec![
        openai_sse(&[
            delta(json!({ "reasoning_content": "use the tool" })),
            delta(json!({ "tool_calls": [{ "index": 0, "id": "c1", "function": { "name": "stamp", "arguments": "{\"word\":" } }] })),
            delta(json!({ "tool_calls": [{ "index": 0, "function": { "arguments": "\"hi\"}" } }] })),
            json!({ "choices": [], "usage": { "prompt_tokens": 9, "completion_tokens": 4 } }),
        ]),
        openai_sse(&[delta(json!({ "content": "Stamped " })), delta(json!({ "content": "HI." }))]),
        openai_sse(&[delta(json!({ "content": "from b" }))]),
    ])
    .await;

    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("plugins/switch");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/switch/core.lua"),
        plugin.join("core.lua"),
    )
    .unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            bone.config.providers.a = {{ base_url = "{url}", model = "m1", api_key = "ka", stream_usage = true }}
            bone.config.providers.b = {{ base_url = "{url}/", model = "m2", api_key = "kb" }}
            bone.config.providers.switch = {{ type = "switch", model = "switch", default = "a" }}
            bone.config.provider = "switch"
            bone.tool.register {{ name = "stamp", run = function(a) return a.word:upper() end }}
            "#
        ),
    );
    let loaded = load(dir.path()).unwrap();
    // on_ready published the choices for the TUI half.
    let published =
        std::fs::read_to_string(dir.path().join("state/shared/switch-providers.json")).unwrap();
    let published: Json = serde_json::from_str(&published).unwrap();
    assert_eq!(published["default"], "a");
    assert_eq!(published["providers"][1]["name"], "b");
    assert_eq!(published["providers"][1]["model"], "m2");

    let core = Core::from_loaded(loaded);
    let mut events = core.subscribe();
    let call = |m: &'static str, p: Json| {
        let core = &core;
        async move { core.handle(m, Some(p)).await.unwrap() }
    };
    let work = tempfile::tempdir().unwrap();
    let info = call(SessionCreate::METHOD, json!({ "cwd": work.path() })).await;
    let sid = info["session_id"].as_str().unwrap().to_owned();
    let mut turn = async |text: &str| {
        call(
            TurnStart::METHOD,
            json!({ "session_id": sid, "text": text }),
        )
        .await;
        let mut deltas = String::new();
        loop {
            let e = next_core_event(&mut events).await;
            if e.method == MessageDelta::METHOD {
                deltas.push_str(e.params["text"].as_str().unwrap());
            }
            if e.method == TurnFinished::METHOD {
                assert_eq!(e.params["outcome"]["status"], "completed", "{}", e.params);
                return deltas;
            }
        }
    };
    assert_eq!(turn("stamp hi").await, "use the toolStamped HI.");
    std::fs::write(
        dir.path().join("state/shared/switch.json"),
        r#"{ "current": "b" }"#,
    )
    .unwrap();
    assert_eq!(turn("again").await, "from b");

    let msgs = call(SessionMessages::METHOD, json!({ "session_id": sid })).await;
    let msgs = msgs["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["tool_calls"][0]["arguments"], r#"{"word":"hi"}"#);
    assert_eq!(msgs[2]["content"], "HI");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].0, "bearer ka");
    assert_eq!(seen[0].1["model"], "m1");
    assert_eq!(seen[0].1["stream_options"]["include_usage"], true);
    assert!(
        seen[0].1["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "stamp")
    );
    // The tool round trip in OpenAI's shape.
    let m = &seen[1].1["messages"];
    let n = m.as_array().unwrap().len();
    assert_eq!(m[n - 2]["tool_calls"][0]["function"]["name"], "stamp");
    assert_eq!(m[n - 1]["role"], "tool");
    assert_eq!(m[n - 1]["tool_call_id"], "c1");
    assert_eq!(m[n - 1]["content"], "HI");
    // Switched: the other entry, its own key, no usage option.
    assert_eq!(seen[2].0, "bearer kb");
    assert_eq!(seen[2].1["model"], "m2");
    assert!(seen[2].1.get("stream_options").is_none());
}

/// `model/complete` against a named OpenAI-compatible entry, with options.
#[tokio::test(flavor = "multi_thread")]
async fn model_calls_reach_named_providers_with_options() {
    use crate::Core;
    use bone_proto::methods::*;
    let delta = |d: Json| json!({ "choices": [{ "index": 0, "delta": d }] });
    let (url, seen) = fake_openai(vec![openai_sse(&[delta(json!({ "content": "titled" }))])]).await;
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            bone.config.providers.main = {{ base_url = "{url}", model = "big" }}
            bone.config.providers.cheap = {{ base_url = "{url}", model = "small", api_key = "kc" }}
            bone.config.provider = "main"
            "#
        ),
    );
    let core = Core::from_loaded(load(dir.path()).unwrap());
    let mut events = core.subscribe();
    let list = core.handle(ModelList::METHOD, None).await.unwrap();
    assert_eq!(list[0]["name"], "cheap");
    assert_eq!(list[1]["current"], true);
    let req = core
        .handle(
            ModelComplete::METHOD,
            Some(json!({
                "provider": "cheap",
                "messages": [{ "role": "user", "content": "title this" }],
                "options": { "model": "tiny" },
            })),
        )
        .await
        .unwrap();
    let done = loop {
        let e = next_core_event(&mut events).await;
        if e.method == ModelCompleted::METHOD {
            break e.params;
        }
    };
    assert_eq!(done["request_id"], req["request_id"]);
    assert_eq!(done["message"]["content"], "titled");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0].0, "bearer kc");
    assert_eq!(seen[0].1["model"], "tiny");
}

/// A request_error hook moving a failed call to another provider.
#[tokio::test(flavor = "multi_thread")]
async fn request_error_hooks_can_fall_back_to_another_provider() {
    use crate::Core;
    use bone_proto::methods::*;
    let delta = |d: Json| json!({ "choices": [{ "index": 0, "delta": d }] });
    let (url, seen) = fake_openai(vec![openai_sse(&[delta(
        json!({ "content": "from backup" }),
    )])])
    .await;
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        &format!(
            r#"
            -- Nothing listens here, so every call to main fails at once.
            bone.config.providers.main = {{ base_url = "http://127.0.0.1:9/v1", model = "big" }}
            bone.config.providers.backup = {{ base_url = "{url}", model = "spare" }}
            bone.config.provider = "main"
            bone.hook("request_error", function(ev)
              if ev.attempt == 1 then return {{ retry = 1, provider = "backup" }} end
            end)
            "#
        ),
    );
    let core = Core::from_loaded(load(dir.path()).unwrap());
    let mut events = core.subscribe();
    let work = tempfile::tempdir().unwrap();
    let info = core
        .handle(SessionCreate::METHOD, Some(json!({ "cwd": work.path() })))
        .await
        .unwrap();
    core.handle(
        TurnStart::METHOD,
        Some(json!({ "session_id": info["session_id"], "text": "hi" })),
    )
    .await
    .unwrap();
    let outcome = loop {
        let e = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("event")
            .unwrap();
        if e.method == TurnFinished::METHOD {
            break e.params["outcome"].clone();
        }
    };
    assert_eq!(outcome["status"], "completed", "{outcome}");
    assert_eq!(seen.lock().unwrap()[0].1["model"], "spare");
}
