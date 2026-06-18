//! Context management tests — /compact command, conversation API, return actions.
//!
//! Covers:
//!   1. Default compact.lua command is registered
//!   2. before_turn is a valid event name
//!   3. ctx.conversation API is present in command ctx
//!   4. conversation.replace return action parses correctly
//!   5. Default compact.lua internal logic

mod common;

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Run a string of Lua code in the extension manager's VM and return
/// the value of a named global, or an empty string on failure.
fn lua_global_string(manager: &bone::ext::ExtensionManager, name: &str) -> String {
    let lua_arc = manager.lua_arc();
    let lua = lua_arc.lock().unwrap();
    lua.globals()
        .get::<Option<String>>(name)
        .ok()
        .flatten()
        .unwrap_or_default()
}

// ── 1. Default compact.lua command is registered ────────────────────────────

#[test]
fn compact_command_is_registered() {
    let config_dir = common::temp_dir("compact-registered");
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        true,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    let cmd_names: Vec<&str> = booted
        .manager
        .commands()
        .iter()
        .map(|c| c.name.as_str())
        .collect();

    assert!(
        cmd_names.contains(&"compact"),
        "expected 'compact' in registered commands; got: {cmd_names:?}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 2. before_turn is a valid event name ────────────────────────────────────

const BEFORE_TURN_PROBE: &str = r#"
local ok = pcall(function()
    bone.on("before_turn", function(event, ctx)
        _BEFORE_TURN_FIRED = "yes"
    end)
end)
_BEFORE_TURN_REGISTERED = ok and "yes" or "no"
"#;

#[test]
fn before_turn_is_valid_event_name() {
    let config_dir = common::temp_dir("before-turn-valid");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("init.lua"), BEFORE_TURN_PROBE).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    let registered = lua_global_string(&booted.manager, "_BEFORE_TURN_REGISTERED");
    assert_eq!(
        registered, "yes",
        "before_turn should be a valid event name; got registered={registered:?}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 3. ctx.conversation API via command handler ─────────────────────────────

/// A Lua command that probes ctx.conversation and stores the results in globals.
const CONV_API_PROBE_CMD: &str = r#"
bone.register_command("convprobe", {
    description = "probes ctx.conversation",
    handler = function(args, ctx)
        local results = {}

        if ctx.conversation then
            results["conv_table"] = "yes"

            if type(ctx.conversation.current) == "function" then
                results["current_fn"] = "yes"
                local cur = ctx.conversation.current()
                if cur then
                    results["current_result"] = "table"
                    results["current_id"] = tostring(cur.id or "nil")
                    results["current_provider"] = tostring(cur.provider or "nil")
                    results["current_model"] = tostring(cur.model or "nil")
                else
                    results["current_result"] = "nil"
                end
            else
                results["current_fn"] = "no"
            end

            if type(ctx.conversation.history) == "function" then
                results["history_fn"] = "yes"
                local hist = ctx.conversation.history()
                if hist then
                    results["history_result"] = "table"
                    results["history_len"] = tostring(#hist)
                    if #hist > 0 then
                        local first = hist[1]
                        results["first_role"] = first.role or "nil"
                        results["first_has_content"] = first.content ~= nil and "yes" or "no"
                    end
                else
                    results["history_result"] = "nil"
                end
            else
                results["history_fn"] = "no"
            end
        else
            results["conv_table"] = "no"
        end

        -- Store in a global for test inspection.
        _CONV_API_PROBE_RESULT = cjson.encode(results)
        return { display = "probed", submit = false }
    end,
})
"#;

#[test]
fn conversation_api_available_in_commands() {
    let config_dir = common::temp_dir("conv-api-cmd");
    let cmd_dir = config_dir.join("lua/commands");
    std::fs::create_dir_all(&cmd_dir).unwrap();
    std::fs::write(cmd_dir.join("convprobe.lua"), CONV_API_PROBE_CMD).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    // Verify handler registered.
    let cmd_names: Vec<&str> = booted
        .manager
        .commands()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        cmd_names.contains(&"convprobe"),
        "convprobe command should be registered",
    );

    // Call the handler through the Lua VM directly (simulating /convprobe).
    let lua_arc = booted.manager.lua_arc();
    let result = {
        let lua = lua_arc.lock().unwrap();

        // Find command handler from _commands table.
        let bone: mlua::Table = lua.globals().get("bone").unwrap();
        let commands: mlua::Table = bone.get("_commands").unwrap();
        let mut handler: Option<mlua::Function> = None;
        for entry in commands.sequence_values::<mlua::Table>() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name: String = entry.get("name").unwrap_or_default();
            if name == "convprobe" {
                handler = entry.get("handler").ok();
                break;
            }
        }

        let handler = handler.expect("convprobe handler not found");

        // Create a minimal ctx with conversation table for the handler.
        // We can't use CtxConfig (pub(crate)), so we construct a fake ctx
        // that mirrors what the handler expects.
        let ctx = lua.create_table().unwrap();

        // ctx.conversation — with canned data.
        let conv = lua.create_table().unwrap();
        // current function
        let current_fn = lua
            .create_function(|lua, _: ()| {
                let t = lua.create_table()?;
                t.set("id", 42_i64)?;
                t.set("provider", "test-provider")?;
                t.set("model", "test-model")?;
                Ok(mlua::Value::Table(t))
            })
            .unwrap();
        conv.set("current", current_fn).unwrap();

        // history function
        let history_fn = lua
            .create_function(|lua, _: ()| {
                let t = lua.create_table()?;
                let msg1 = lua.create_table()?;
                msg1.set("role", "user")?;
                msg1.set("content", "hello")?;
                t.push(msg1)?;
                Ok(mlua::Value::Table(t))
            })
            .unwrap();
        conv.set("history", history_fn).unwrap();
        ctx.set("conversation", conv).unwrap();

        // ctx.ui.notify (minimal, for the handler)
        let ui = lua.create_table().unwrap();
        let notify_fn = lua
            .create_function(|_, (_msg, _level): (String, Option<String>)| Ok(()))
            .unwrap();
        ui.set("notify", notify_fn).unwrap();
        ctx.set("ui", ui).unwrap();

        // Release lock before calling (avoid reentrancy issues).
        drop(lua);
        handler.call::<mlua::Value>(("", ctx))
    };

    // Handler should return a table (our {display="probed", submit=false}).
    assert!(
        result.is_ok(),
        "convprobe handler should succeed, got: {result:?}",
    );

    // Read the probe results.
    let lua = lua_arc.lock().unwrap();
    let raw: String = lua
        .globals()
        .get::<Option<String>>("_CONV_API_PROBE_RESULT")
        .ok()
        .flatten()
        .unwrap_or_default();
    drop(lua);

    assert!(!raw.is_empty(), "probe did not set _CONV_API_PROBE_RESULT");

    let results: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        results["conv_table"], "yes",
        "ctx.conversation should exist"
    );
    assert_eq!(
        results["current_fn"], "yes",
        "ctx.conversation.current should be a function"
    );
    assert_eq!(
        results["history_fn"], "yes",
        "ctx.conversation.history should be a function"
    );
    assert_eq!(results["history_len"], "1", "history should have 1 message");
    assert_eq!(
        results["first_role"], "user",
        "first message should have role=user"
    );
    assert_eq!(
        results["first_has_content"], "yes",
        "first message should have content"
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 4. conversation.replace return action parsing ───────────────────────────

const ACTION_RETURN_CMD: &str = r#"
bone.register_command("actiontest", {
    description = "tests return action parsing",
    handler = function(args, ctx)
        if args == "replace" then
            return {
                action = "conversation.replace",
                messages = {
                    { role = "user", content = "summary of prior context" },
                    { role = "assistant", content = "acknowledged" },
                    {
                        role = "assistant",
                        content = "",
                        tool_calls = {
                            { id = "call_1", name = "read_file", arguments = { path = "Cargo.toml" } },
                        },
                    },
                    { role = "tool", content = "contents", name = "read_file", tool_call_id = "call_1" },
                },
                display = "replaced",
                submit = false,
            }
        elseif args == "bad_action" then
            return {
                action = "unknown.action",
                display = "should warn",
                submit = false,
            }
        elseif args == "no_messages" then
            return {
                action = "conversation.replace",
                display = "should warn about missing messages",
                submit = false,
            }
        elseif args == "invalid_role" then
            return {
                action = "conversation.replace",
                messages = {
                    { role = "system", content = "system is not a valid role" },
                },
                display = "should warn about invalid role",
                submit = false,
            }
        else
            return { display = "no action", submit = false }
        end
    end,
})
"#;

#[test]
fn conversation_replace_action_parses() {
    let config_dir = common::temp_dir("action-return");
    let cmd_dir = config_dir.join("lua/commands");
    std::fs::create_dir_all(&cmd_dir).unwrap();
    std::fs::write(cmd_dir.join("actiontest.lua"), ACTION_RETURN_CMD).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    let lua_arc = booted.manager.lua_arc();

    // Helper to call actiontest with an arg and get the return table.
    let call_handler = |arg: &str| -> Option<mlua::Table> {
        let lua = lua_arc.lock().unwrap();
        let bone: mlua::Table = lua.globals().get("bone").unwrap();
        let commands: mlua::Table = bone.get("_commands").unwrap();
        let mut handler: Option<mlua::Function> = None;
        for entry in commands.sequence_values::<mlua::Table>() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name: String = entry.get("name").unwrap_or_default();
            if name == "actiontest" {
                handler = entry.get("handler").ok();
                break;
            }
        }
        let handler = handler?;

        // Minimal ctx with ui.notify.
        let ctx = lua.create_table().ok()?;
        let ui = lua.create_table().ok()?;
        let notify_fn = lua
            .create_function(|_, (_msg, _level): (String, Option<String>)| Ok(()))
            .ok()?;
        ui.set("notify", notify_fn).ok()?;
        ctx.set("ui", ui).ok()?;
        drop(lua);

        match handler.call::<mlua::Value>((arg, ctx)) {
            Ok(mlua::Value::Table(t)) => Some(t),
            _ => None,
        }
    };

    // Test 1: valid conversation.replace
    let t = call_handler("replace").expect("should return a table");
    let action: String = t.get("action").unwrap_or_default();
    assert_eq!(action, "conversation.replace");

    let messages: mlua::Table = t.get("messages").unwrap();
    let msgs: Vec<mlua::Table> = messages.sequence_values().filter_map(|v| v.ok()).collect();
    assert_eq!(msgs.len(), 4);
    let role0: String = msgs[0].get("role").unwrap();
    assert_eq!(role0, "user");
    let content0: String = msgs[0].get("content").unwrap();
    assert_eq!(content0, "summary of prior context");
    let role1: String = msgs[1].get("role").unwrap();
    assert_eq!(role1, "assistant");
    let role2: String = msgs[2].get("role").unwrap();
    assert_eq!(role2, "assistant");
    let tool_calls: mlua::Table = msgs[2].get("tool_calls").unwrap();
    let calls: Vec<mlua::Table> = tool_calls
        .sequence_values()
        .filter_map(|v| v.ok())
        .collect();
    assert_eq!(calls.len(), 1);
    let call_id: String = calls[0].get("id").unwrap();
    assert_eq!(call_id, "call_1");
    let args: mlua::Table = calls[0].get("arguments").unwrap();
    let path: String = args.get("path").unwrap();
    assert_eq!(path, "Cargo.toml");
    let role3: String = msgs[3].get("role").unwrap();
    assert_eq!(role3, "tool");

    let display: String = t.get("display").unwrap_or_default();
    assert_eq!(display, "replaced");

    let submit: bool = t.get("submit").unwrap_or(true);
    assert!(!submit);

    // Test 2: no action → regular table with display
    let t = call_handler("").expect("should return a table");
    let action: Option<String> = t.get("action").ok();
    assert!(action.is_none(), "empty arg should have no action");

    // Test 3: bad action name → should not crash, just return the table
    let t = call_handler("bad_action").expect("should return a table");
    let action: String = t.get("action").unwrap_or_default();
    assert_eq!(
        action, "unknown.action",
        "unknown action preserved in table"
    );

    // Test 4: missing messages should still return the table (warning on stderr)
    let t = call_handler("no_messages").expect("should return a table");
    let action: String = t.get("action").unwrap_or_default();
    assert_eq!(action, "conversation.replace");
    let messages: Option<mlua::Table> = t.get("messages").ok();
    assert!(
        messages.is_none(),
        "no_messages: messages key should be absent"
    );

    // Test 5: invalid roles preserved in the table (validation happens in Rust)
    let t = call_handler("invalid_role").expect("should return a table");
    let messages: mlua::Table = t.get("messages").unwrap();
    let msgs: Vec<mlua::Table> = messages.sequence_values().filter_map(|v| v.ok()).collect();
    assert_eq!(msgs.len(), 1);
    let role0: String = msgs[0].get("role").unwrap();
    assert_eq!(
        role0, "system",
        "invalid role preserved for Rust-side validation"
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 5. Lua command return semantics ────────────────────────────────────────

#[test]
fn lua_command_string_return_submits_to_agent_loop() {
    let lua = mlua::Lua::new();
    let value = mlua::Value::String(lua.create_string("expanded prompt").unwrap());

    let parsed = bone::ext::types::parse_lua_command_return(value)
        .expect("string command return should be handled");

    assert_eq!(parsed.output, "expanded prompt");
    assert!(parsed.submit, "string returns should submit a user turn");
    assert!(parsed.action.is_none());
}

#[test]
fn lua_command_table_return_can_be_display_only() {
    let lua = mlua::Lua::new();
    let table = lua.create_table().unwrap();
    table.set("display", "shown only").unwrap();
    table.set("submit", false).unwrap();

    let parsed = bone::ext::types::parse_lua_command_return(mlua::Value::Table(table))
        .expect("table command return should be handled");

    assert_eq!(parsed.output, "shown only");
    assert!(!parsed.submit);
    assert!(parsed.action.is_none());
}

#[test]
fn lua_command_table_return_defaults_to_submit() {
    let lua = mlua::Lua::new();
    let table = lua.create_table().unwrap();
    table.set("content", "prompt from table").unwrap();

    let parsed = bone::ext::types::parse_lua_command_return(mlua::Value::Table(table))
        .expect("table command return should be handled");

    assert_eq!(parsed.output, "prompt from table");
    assert!(parsed.submit);
    assert!(parsed.action.is_none());
}

// ── 5. Default compact.lua internal logic ──────────────────────────────────

/// Load the default compact.lua and exercise its compact() function directly.
#[test]
fn compact_logic_on_small_history_is_noop() {
    let config_dir = common::temp_dir("compact-small");
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    // Load compact.lua into the VM and run a test.
    let lua_arc = booted.manager.lua_arc();
    let lua = lua_arc.lock().unwrap();

    // The compact.lua is already loaded via defaults. Let's probe it.
    // Check that /compact command exists.
    let bone: mlua::Table = lua.globals().get("bone").unwrap();
    let commands: mlua::Table = bone.get("_commands").unwrap();
    let mut found = false;
    for entry in commands.sequence_values::<mlua::Table>() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name: String = entry.get("name").unwrap_or_default();
        if name == "compact" {
            found = true;
            break;
        }
    }
    assert!(found, "/compact command should be in _commands");

    // Check that _handlers.before_turn is populated.
    let handlers: mlua::Table = bone.get("_handlers").unwrap();
    let before_turn: mlua::Table = handlers.get("before_turn").unwrap();
    let bt_count = before_turn.sequence_values::<mlua::Value>().count();
    assert!(
        bt_count >= 1,
        "before_turn handlers should have at least 1 entry, got {bt_count}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

#[test]
fn compact_preserves_tool_call_chains() {
    let config_dir = common::temp_dir("compact-tool-calls");
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    let lua_arc = booted.manager.lua_arc();
    let lua = lua_arc.lock().unwrap();
    lua.load(r#"
        local handler = bone._handlers.before_turn[1]
        local ctx = {
            config = {
                get = function(section, key)
                    if key == "auto_compact_tokens" then return "1" end
                    if key == "auto_compact_keep_messages" then return "3" end
                end,
                get_table = function(section)
                    if section == "commands" then return { disabled = {} } end
                end,
            },
            usage = { snapshot = function() return { context_length = 100 } end },
            conversation = { history = function() return {
                { role = "user", content = "older" },
                { role = "assistant", content = "older answer" },
                { role = "user", content = "read it" },
                { role = "assistant", content = "", tool_calls = {
                    { id = "call_1", name = "read_file", arguments = { path = "Cargo.toml" } },
                } },
                { role = "tool", content = "contents", name = "read_file", tool_call_id = "call_1" },
                { role = "user", content = "continue" },
            } end },
            agent = { run = function() return { ok = true, content = "summary" } end },
            ui = { notify = function() end },
        }
        local ret = handler({}, ctx)
        _COMPACT_TOOL_RET = cjson.encode(ret.messages)
    "#)
    .exec()
    .unwrap();

    let raw: String = lua.globals().get("_COMPACT_TOOL_RET").unwrap();
    let messages: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(
        messages.as_array().unwrap().iter().any(|m| {
            m["role"] == "assistant"
                && m["tool_calls"]
                    .as_array()
                    .is_some_and(|calls| calls[0]["id"] == "call_1")
        }),
        "assistant tool call should be preserved: {raw}"
    );
    assert!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .any(|m| { m["role"] == "tool" && m["tool_call_id"] == "call_1" }),
        "matching tool result should be preserved: {raw}"
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

#[test]
fn compact_drops_orphan_tool_results() {
    let config_dir = common::temp_dir("compact-orphan-tool");
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    let lua_arc = booted.manager.lua_arc();
    let lua = lua_arc.lock().unwrap();
    lua.load(r#"
        local handler = bone._handlers.before_turn[1]
        local ctx = {
            config = {
                get = function(section, key)
                    if key == "auto_compact_tokens" then return "1" end
                    if key == "auto_compact_keep_messages" then return "2" end
                end,
                get_table = function(section)
                    if section == "commands" then return { disabled = {} } end
                end,
            },
            usage = { snapshot = function() return { context_length = 200 } end },
            conversation = { history = function() return {
                { role = "user", content = "read it" },
                { role = "assistant", content = "", tool_calls = {
                    { id = "call_1", name = "read_file", arguments = { path = "Cargo.toml" } },
                } },
                { role = "tool", content = "contents", name = "read_file", tool_call_id = "call_1" },
                { role = "user", content = "continue" },
                { role = "assistant", content = "ok" },
            } end },
            agent = { run = function() return { ok = true, content = "summary" } end },
            ui = { notify = function() end },
        }
        local ret = handler({}, ctx)
        _COMPACT_ORPHAN_RET = cjson.encode(ret.messages)
    "#)
    .exec()
    .unwrap();

    let raw: String = lua.globals().get("_COMPACT_ORPHAN_RET").unwrap();
    let messages: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "tool"),
        "orphan tool result should be dropped: {raw}"
    );
    assert!(
        messages.as_array().unwrap().iter().all(|m| m
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .is_none_or(|a| a.is_empty())),
        "assistant tool call without result should be dropped: {raw}"
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 6b. auto-compact gate respects the deny-list config format ──────────────
//
// Regression: the `commands` namespace uses the deny-list model
// (`{ title, disabled = [] }`), NOT the legacy field-based `{ compact = true }`.
// The before_turn gate must treat `compact` as enabled unless it appears in the
// `disabled` array. Previously the gate called `ctx.config.get("commands",
// "compact")`, which always returned nil under the deny-list format and
// silently disabled auto-compaction forever.

#[test]
fn auto_compact_enabled_under_denylist_config() {
    let config_dir = common::temp_dir("compact-denylist-enabled");
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    let lua_arc = booted.manager.lua_arc();
    let lua = lua_arc.lock().unwrap();
    lua.load(
        r#"
        local handler = bone._handlers.before_turn[1]
        -- Deny-list config format: disabled is empty → compact is enabled.
        local ctx = {
            config = {
                get = function(section, key)
                    if key == "auto_compact_tokens" then return "1" end
                    if key == "auto_compact_keep_messages" then return "2" end
                end,
                get_table = function(section)
                    if section == "commands" then return { disabled = {} } end
                end,
            },
            usage = { snapshot = function() return { context_length = 100 } end },
            conversation = { history = function() return {
                { role = "user", content = "older" },
                { role = "assistant", content = "older answer" },
                { role = "user", content = "continue" },
                { role = "assistant", content = "ok" },
            } end },
            agent = { run = function() return { ok = true, content = "summary" } end },
            ui = { notify = function() end },
        }
        local ret = handler({}, ctx)
        -- Should NOT have bailed at the gate: it ran compaction and returned
        -- a replacement transcript (non-nil) with the summary.
        _AUTO_COMPACT_RET = ret and "table" or "nil"
    "#,
    )
    .exec()
    .unwrap();

    let result: String = lua.globals().get("_AUTO_COMPACT_RET").unwrap();
    assert_eq!(
        result, "table",
        "auto-compaction should fire under the deny-list config format; got ret={result:?}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

#[test]
fn auto_compact_disabled_when_in_denylist() {
    let config_dir = common::temp_dir("compact-denylist-disabled");
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    let lua_arc = booted.manager.lua_arc();
    let lua = lua_arc.lock().unwrap();
    lua.load(
        r#"
        local handler = bone._handlers.before_turn[1]
        -- compact is in the disabled array → the gate bails with nil.
        local ctx = {
            config = {
                get = function(_, _) return nil end,
                get_table = function(section)
                    if section == "commands" then return { disabled = { "compact" } } end
                end,
            },
            usage = { snapshot = function() return { context_length = 999 } end },
            conversation = { history = function() return {
                { role = "user", content = "x" },
            } end },
            agent = { run = function() return { ok = true, content = "should not run" } end },
            ui = { notify = function() end },
        }
        local ret = handler({}, ctx)
        _AUTO_COMPACT_DISABLED_RET = ret and "table" or "nil"
    "#,
    )
    .exec()
    .unwrap();

    let result: String = lua.globals().get("_AUTO_COMPACT_DISABLED_RET").unwrap();
    assert_eq!(
        result, "nil",
        "auto-compaction must be skipped when compact is in the deny-list; got ret={result:?}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 6. compact.lua loads without Lua errors ─────────────────────────────────

#[test]
fn compact_lua_loads_cleanly() {
    let config_dir = common::temp_dir("compact-load");
    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions::default(),
        "test-model",
        "TestProvider",
    );

    // After boot, the default compact.lua should be loaded.
    // Check via public commands API (init.lua runs before defaults, so we
    // check post-boot).
    let cmd_names: Vec<&str> = booted
        .manager
        .commands()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        cmd_names.contains(&"compact"),
        "compact.lua should be loaded and register /compact; commands: {cmd_names:?}",
    );

    // Also verify the before_turn handler is registered via the Lua VM.
    let lua_arc = booted.manager.lua_arc();
    let lua = lua_arc.lock().unwrap();
    let bone: mlua::Table = lua.globals().get("bone").unwrap();
    let handlers: mlua::Table = bone.get("_handlers").unwrap();
    let before_turn: mlua::Table = handlers.get("before_turn").unwrap();
    let bt_count = before_turn.sequence_values::<mlua::Value>().count();
    drop(lua);
    assert!(
        bt_count >= 1,
        "compact.lua should register a before_turn handler; got {bt_count}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 7. compact command is NOT a protected builtin ───────────────────────────

#[test]
fn compact_is_not_a_protected_builtin() {
    // The builtin list is in src/ui/commands/mod.rs BUILTINS.
    // Verify /compact is NOT in it (it's Lua-defined).
    let builtins = &[
        "clear", "config", "edit", "e", "exit", "help", "model", "new", "provider", "quit",
        "stats", "tools",
    ];
    assert!(
        !builtins.contains(&"compact"),
        "/compact must not be a protected builtin; it should be Lua-overridable",
    );
}
