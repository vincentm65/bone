//! Regression test: a LuaTool calling another LuaTool via ctx.tools.call
//! must not deadlock.
//!
//! Previously, nested LuaTool invocations went through spawn_blocking and
//! tried to access the Lua VM from a different thread while the calling
//! thread still held mlua's internal (per-thread reentrant) VM mutex,
//! deadlocking on the very first LuaTool -> LuaTool call. Nested calls now
//! execute inline on the calling thread.

mod common;

use std::time::Duration;

use bone_core::tools::types::ToolCall;

const OUTER_TOOL: &str = r#"
bone.register_tool({
  name = "outer_caller",
  description = "calls inner_echo via ctx.tools.call",
  safety = "read_only",
  parameters = { type = "object", properties = {} },
  execute = function(args, ctx)
    local r = ctx.tools.call("inner_echo", { msg = "hi" }, { approval = "safe" })
    if r.is_error then
      return "error:" .. tostring(r.content)
    end
    return "outer:" .. tostring(r.content)
  end,
})
"#;

const INNER_TOOL: &str = r#"
bone.register_tool({
  name = "inner_echo",
  description = "echoes its msg argument",
  safety = "read_only",
  parameters = { type = "object", properties = { msg = { type = "string" } } },
  execute = function(args, ctx)
    return "inner:" .. tostring(args.msg)
  end,
})
"#;

#[test]
fn nested_lua_tool_call_does_not_deadlock() {
    let config_dir = common::temp_dir("lua-nested");
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("outer.lua"), OUTER_TOOL).unwrap();
    std::fs::write(tools_dir.join("inner.lua"), INNER_TOOL).unwrap();

    let mut custom = bone_core::config::custom::CustomConfigs::default();
    let booted = bone_core::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone_core::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );
    let tools = booted.tools;

    // ctx.tools.call uses block_in_place, which requires a multi-thread runtime.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let call = ToolCall {
        id: "call-1".to_string(),
        name: "outer_caller".to_string(),
        arguments: serde_json::json!({}),
    };

    let results = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), tools.execute_all(vec![call], 0)).await
    });

    // Shut down without waiting on blocking threads so a regression fails
    // the test instead of hanging it forever.
    rt.shutdown_timeout(Duration::from_secs(1));

    let results = results.expect("nested lua tool call deadlocked (timed out)");
    assert_eq!(results.len(), 1);
    assert!(
        !results[0].is_error,
        "nested call errored: {}",
        results[0].content
    );
    assert_eq!(results[0].content, "outer:inner:hi");

    std::fs::remove_dir_all(&config_dir).ok();
}
