//! Lua API tests — sandbox, depth limits, event ctx, reload.
//!
//! Covers cleanup plan items 7–8:
//!   7. Reload re-boots with fresh VM and picks up tools/commands/hooks/snapshots.
//!   8a. Sandbox blocks dangerous APIs (os.execute, io.open, dofile, loadfile, package.loadlib).
//!   8b. Default Lua tools/commands boot without sandbox violations.
//!   8c. ctx.tools.call enforces MAX_TOOL_CALL_DEPTH.
//!   8d. ctx.agent.run enforces MAX_AGENT_DEPTH.
//!   8e. Event handler ctx has ui.notify but not tools/agent/shell.

mod common;

use std::time::Duration;

use bone::tools::types::ToolCall;

// ── 8a. Sandbox blocks dangerous APIs ───────────────────────────────────────

/// A Lua tool that attempts each sandboxed API and returns a summary table.
const SANDBOX_PROBE_TOOL: &str = r#"
bone.register_tool({
  name = "sandbox_probe",
  description = "probes sandboxed APIs",
  safety = "danger",
  parameters = { type = "object", properties = {} },
  execute = function(args, ctx)
    local results = {}

    local function probe(label, fn)
      local ok, err = pcall(fn)
      if ok then
        table.insert(results, label .. ":UNBLOCKED")
      else
        table.insert(results, label .. ":BLOCKED")
      end
    end

    probe("os.execute", function() os.execute("true") end)
    probe("os.exit",    function() os.exit(0) end)
    probe("os.remove",  function() os.remove("/tmp/__bone_sandbox_test__") end)
    probe("os.rename",  function() os.rename("/tmp/a","/tmp/b") end)
    probe("io.open",    function() io.open("/dev/null") end)
    probe("io.popen",   function() io.popen("true") end)
    probe("io.tmpfile", function() io.tmpfile() end)
    probe("dofile",     function() dofile("/dev/null") end)
    probe("loadfile",   function() loadfile("/dev/null") end)
    probe("package.loadlib", function() package.loadlib("x","y") end)

    return table.concat(results, ",")
  end,
})
"#;

#[test]
fn sandbox_blocks_dangerous_apis() {
    let config_dir = common::temp_dir("sandbox-probe");
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("probe.lua"), SANDBOX_PROBE_TOOL).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, false, bone::ext::BootOptions::default());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let call = ToolCall {
        id: "call-sandbox".to_string(),
        name: "sandbox_probe".to_string(),
        arguments: serde_json::json!({}),
    };

    let results = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(10),
            booted.tools.execute_all(vec![call], 0),
        )
        .await
    });
    rt.shutdown_timeout(Duration::from_secs(1));

    let results = results.expect("sandbox probe timed out");
    assert_eq!(results.len(), 1);
    assert!(
        !results[0].is_error,
        "sandbox probe errored: {}",
        results[0].content
    );

    let content = &results[0].content;
    for api in &[
        "os.execute",
        "os.exit",
        "os.remove",
        "os.rename",
        "io.open",
        "io.popen",
        "io.tmpfile",
        "dofile",
        "loadfile",
        "package.loadlib",
    ] {
        let expected = format!("{api}:BLOCKED");
        assert!(
            content.contains(&expected),
            "expected {api}:BLOCKED in output, got: {content}",
        );
    }
    assert!(
        !content.contains("UNBLOCKED"),
        "no API should be unblocked, got: {content}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 8b. Default tools boot without sandbox violations ───────────────────────

#[test]
fn default_tools_boot_cleanly() {
    let config_dir = common::temp_dir("defaults-boot");
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, true, bone::ext::BootOptions::default());

    // The boot itself should succeed. ExtensionManager should report available.
    assert!(
        booted.manager.is_available(),
        "extension manager should be available after boot",
    );

    // Verify default tools are registered (at least the ones we ship).
    let defs = booted.tools.definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();

    for expected in &["ask_user", "web_search", "task_list"] {
        assert!(
            names.contains(expected),
            "default tool '{expected}' not found; registered: {names:?}",
        );
    }

    // Commands should also be present.
    let cmd_names: Vec<&str> = booted
        .manager
        .commands()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        cmd_names.contains(&"usage"),
        "default command 'usage' not found; commands: {cmd_names:?}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 8c. ctx.tools.call depth limit ──────────────────────────────────────────

/// A tool that calls itself recursively via ctx.tools.call until depth is exhausted.
const DEPTH_COUNTER_TOOL: &str = r#"
bone.register_tool({
  name = "depth_counter",
  description = "calls itself recursively via ctx.tools.call",
  safety = "read_only",
  parameters = {
    type = "object",
    properties = {
      depth = { type = "number" },
    },
  },
  execute = function(args, ctx)
    local d = (args.depth or 0) + 1
    local r = ctx.tools.call("depth_counter", { depth = d }, { approval = "safe" })
    if r.is_error then
      return "depth=" .. tostring(d) .. " stopped:" .. tostring(r.content)
    end
    return tostring(r.content)
  end,
})
"#;

#[test]
fn tools_call_depth_limit_enforced() {
    let config_dir = common::temp_dir("tools-call-depth");
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("depth.lua"), DEPTH_COUNTER_TOOL).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, false, bone::ext::BootOptions::default());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let call = ToolCall {
        id: "call-depth".to_string(),
        name: "depth_counter".to_string(),
        arguments: serde_json::json!({ "depth": 0 }),
    };

    let results = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(30),
            booted.tools.execute_all(vec![call], 0),
        )
        .await
    });
    rt.shutdown_timeout(Duration::from_secs(1));

    let results = results.expect("depth counter timed out");
    assert_eq!(results.len(), 1);
    assert!(
        !results[0].is_error,
        "depth counter errored: {}",
        results[0].content,
    );

    let content = &results[0].content;
    // The tool should have been stopped by the depth limiter.
    assert!(
        content.contains("stopped:max tool call depth exceeded"),
        "expected depth limit message, got: {content}",
    );
    // It should have reached at least depth 4 (MAX_TOOL_CALL_DEPTH is 4).
    assert!(
        content.contains("depth=4") || content.contains("depth=5"),
        "expected at least depth 4 before stop, got: {content}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 8d. ctx.agent.run depth limit ───────────────────────────────────────────

/// A tool that calls ctx.agent.run. When executed at max agent depth,
/// it should get the depth-exceeded error without touching an LLM.
const AGENT_DEPTH_TOOL: &str = r#"
bone.register_tool({
  name = "agent_depth_probe",
  description = "probes ctx.agent.run depth",
  safety = "read_only",
  parameters = { type = "object", properties = {} },
  execute = function(args, ctx)
    local r = ctx.agent.run("hello", { approval = "safe", timeout_ms = 1000 })
    if r.error and string.find(r.error, "max agent depth") then
      return "depth_exceeded"
    end
    return "ok:" .. tostring(r.ok) .. " err:" .. tostring(r.error)
  end,
})
"#;

#[test]
fn agent_run_depth_limit_enforced() {
    let config_dir = common::temp_dir("agent-depth");
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("agent_depth.lua"), AGENT_DEPTH_TOOL).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, false, bone::ext::BootOptions::default());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // Execute at agent_depth = 3 (MAX_AGENT_DEPTH). The tool should
    // immediately receive "max agent depth exceeded" without calling any LLM.
    let call = ToolCall {
        id: "call-agent-depth".to_string(),
        name: "agent_depth_probe".to_string(),
        arguments: serde_json::json!({}),
    };

    let results = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(10),
            booted.tools.execute_all(vec![call], 3),
        )
        .await
    });
    rt.shutdown_timeout(Duration::from_secs(1));

    let results = results.expect("agent depth probe timed out");
    assert_eq!(results.len(), 1);
    assert!(
        !results[0].is_error,
        "agent depth probe errored: {}",
        results[0].content,
    );
    assert_eq!(
        results[0].content, "depth_exceeded",
        "expected depth_exceeded, got: {}",
        results[0].content,
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 8e. Event ctx fields ────────────────────────────────────────────────────

/// An event handler that stores results in a _G global.
/// We'll read it via the Lua VM directly.
const EVENT_CTX_PROBE_V2: &str = r#"
bone.on("session_start", function(event, ctx)
  local parts = {}

  local function check(label, tbl, key)
    if tbl and type(tbl) == "table" then
      local v = rawget(tbl, key)
      if v ~= nil then
        table.insert(parts, label .. "." .. key .. "=yes")
      else
        table.insert(parts, label .. "." .. key .. "=no")
      end
    else
      table.insert(parts, label .. "=missing")
    end
  end

  check("ctx", ctx, "ui")
  check("ctx", ctx, "tools")
  check("ctx", ctx, "agent")
  check("ctx", ctx, "shell")
  check("ctx", ctx, "fs")

  if ctx and type(ctx) == "table" and type(ctx.ui) == "table" then
    check("ctx.ui", ctx.ui, "notify")
  end

  _EVENT_CTX_PROBE_RESULT = table.concat(parts, ",")
end)
"#;

#[test]
fn event_ctx_has_ui_notify_but_not_tools_agent_shell() {
    let config_dir = common::temp_dir("event-ctx");
    let lua_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&lua_dir).unwrap();
    // Write init.lua with the event handler.
    std::fs::write(config_dir.join("init.lua"), EVENT_CTX_PROBE_V2).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, false, bone::ext::BootOptions::default());

    // Dispatch a session_start event.
    booted
        .manager
        .dispatch_simple("session_start", serde_json::json!({}));

    // Read the probe result from the Lua VM.
    let lua_arc = booted.manager.lua_arc();
    let lua = lua_arc.lock().unwrap();
    let result: String = lua
        .globals()
        .get::<Option<String>>("_EVENT_CTX_PROBE_RESULT")
        .ok()
        .flatten()
        .unwrap_or_default();
    drop(lua);

    assert!(!result.is_empty(), "event handler did not set probe result");

    // Event ctx should have ui and ui.notify.
    assert!(
        result.contains("ctx.ui=yes"),
        "expected ctx.ui=yes in: {result}",
    );
    assert!(
        result.contains("ctx.ui.notify=yes"),
        "expected ctx.ui.notify=yes in: {result}",
    );

    // Event ctx should NOT have tools, agent, shell, or fs.
    for forbidden in &[
        "ctx.tools=yes",
        "ctx.agent=yes",
        "ctx.shell=yes",
        "ctx.fs=yes",
    ] {
        assert!(
            !result.contains(forbidden),
            "event ctx should not have {forbidden}, got: {result}",
        );
    }

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 7. Reload picks up changes ──────────────────────────────────────────────

#[test]
fn reload_picks_up_new_tools_and_commands() {
    let config_dir = common::temp_dir("reload");
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();

    // Boot once — no custom tools.
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted1 = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, true, bone::ext::BootOptions::default());
    let names1: Vec<String> = booted1
        .tools
        .definitions()
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert!(
        !names1.iter().any(|n| n == "reload_test_tool"),
        "reload_test_tool should not exist on first boot",
    );

    // Write a new tool and a new command.
    std::fs::write(
        tools_dir.join("reload_test.lua"),
        r#"
bone.register_tool({
  name = "reload_test_tool",
  description = "tool added after initial boot",
  safety = "read_only",
  parameters = { type = "object", properties = {} },
  execute = function() return "reloaded" end,
})
"#,
    )
    .unwrap();
    let cmd_dir = config_dir.join("lua/commands");
    std::fs::create_dir_all(&cmd_dir).unwrap();
    std::fs::write(
        cmd_dir.join("reload_cmd.lua"),
        r#"
bone.register_command("reload_test_cmd", {
  description = "command added after initial boot",
  handler = function() return { display = "ok", submit = false } end,
})
"#,
    )
    .unwrap();

    // Simulate /tools reload: boot a fresh VM.
    let booted2 = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, true, bone::ext::BootOptions::default());
    let names2: Vec<String> = booted2
        .tools
        .definitions()
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert!(
        names2.iter().any(|n| n == "reload_test_tool"),
        "reload_test_tool should appear after reload, got: {names2:?}",
    );

    let cmd_names: Vec<String> = booted2
        .manager
        .commands()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    assert!(
        cmd_names.iter().any(|n| n == "reload_test_cmd"),
        "reload_test_cmd should appear after reload, got: {cmd_names:?}",
    );

    // Execute the new tool to prove the new VM works.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let call = ToolCall {
        id: "call-reload".to_string(),
        name: "reload_test_tool".to_string(),
        arguments: serde_json::json!({}),
    };

    let results = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(10),
            booted2.tools.execute_all(vec![call], 0),
        )
        .await
    });
    rt.shutdown_timeout(Duration::from_secs(1));

    let results = results.expect("reload tool timed out");
    assert_eq!(results.len(), 1);
    assert!(
        !results[0].is_error,
        "reload tool errored: {}",
        results[0].content
    );
    assert_eq!(results[0].content, "reloaded");

    std::fs::remove_dir_all(&config_dir).ok();
}

#[test]
fn reload_snapshots_come_from_same_fresh_vm() {
    let config_dir = common::temp_dir("reload-snapshots");
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();

    // Boot without any theme config.
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted1 = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, true, bone::ext::BootOptions::default());
    assert!(
        booted1.manager.theme_snapshot().user_msg.is_none(),
        "no theme should be set on first boot",
    );

    // Add a theme in init.lua.
    std::fs::write(
        config_dir.join("init.lua"),
        r##"
bone.theme = bone.theme or {}
bone.theme.user_msg = "#ff0000"
"##,
    )
    .unwrap();

    // Reboot — snapshots should reflect the new theme.
    let booted2 = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, true, bone::ext::BootOptions::default());
    assert!(
        booted2.manager.theme_snapshot().user_msg.is_some(),
        "theme snapshot should have user_msg after reload",
    );

    // The VM should be a different instance (fresh boot), verified by the
    // fact that the old snapshot was None and the new one is Some.
    std::fs::remove_dir_all(&config_dir).ok();
}
