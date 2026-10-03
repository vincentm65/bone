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
    let lua = setup(dir.path()).unwrap();
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
    let ex = extract(&setup(dir.path()).unwrap()).unwrap();
    let err = resolve(dir.path(), &ex, &no_env).unwrap_err();
    assert!(err.contains("no model provider configured"), "{err}");
    let env = |k: &str| match k {
        "BONE_BASE_URL" => Some("http://x".into()),
        "BONE_MODEL" => Some("m".into()),
        _ => None,
    };
    assert_eq!(resolve(dir.path(), &ex, &env).unwrap().data_dir, dir.path());

    write(dir.path(), r#"bone.config.provider = "nope""#);
    let ex = extract(&setup(dir.path()).unwrap()).unwrap();
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
    let tool = std::sync::Arc::new(LuaTool::new(spec, s.clone()));

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
    s.cancel_asks("s1");
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
    let tool = LuaTool::new(spec, loaded.scripting.clone());
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
