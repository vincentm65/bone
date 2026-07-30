use super::*;

/// Create a CtxConfig with a minimal test ConfigStore and schema so that
/// `create_ctx_table` does not reject the config as missing its daemon store.
fn test_ctx_config() -> CtxConfig {
    let shared = new_shared_state();
    let store = crate::config::store::ConfigStore::for_test();
    let mut cfg = CtxConfig::new("/tmp".to_string(), shared);
    cfg.config_schema = Some(store.schema());
    cfg.config_store = Some(store);
    cfg
}

#[test]
fn canonical_config_pages_and_mutations_use_the_daemon_store() {
    let _guard = crate::util::test_env_lock();
    let old_bone = std::env::var_os("BONE_DIR");
    let temp = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", temp.path()) };

    let store =
        crate::config::store::ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
    let lua = Lua::new();
    let mut cfg = CtxConfig::new(
        temp.path().to_string_lossy().into_owned(),
        new_shared_state(),
    );
    cfg.config_store = Some(store.clone());
    cfg.config_schema = Some(store.schema_for(
        &["shell".into(), "worker".into()],
        &["config".into(), "history".into()],
    ));
    let config = build_config_table(&lua, &cfg).unwrap();

    let get_pages: mlua::Function = config.get("get_pages").unwrap();
    let pages: serde_json::Value = lua
        .from_value(get_pages.call::<Value>(()).unwrap())
        .unwrap();
    let namespaces = pages
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|page| page["namespace"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        namespaces,
        ["general", "providers", "tools", "commands", "status"]
    );
    assert_eq!(pages[2]["fields"].as_array().unwrap().len(), 2);
    assert_eq!(pages[3]["fields"].as_array().unwrap().len(), 1);
    assert_eq!(pages[3]["fields"][0]["key"], "history");
    assert_eq!(pages[4]["fields"].as_array().unwrap().len(), 15);

    let set_value: mlua::Function = config.get("set_value").unwrap();
    assert!(
        set_value
            .call::<bool>(("general", "show_reasoning", true))
            .unwrap()
    );
    assert_eq!(store.snapshot().values["general"]["show_reasoning"], true);
    assert!(set_value.call::<bool>(("tools", "shell", false)).unwrap());
    assert_eq!(store.snapshot().disabled_tools, ["shell"]);
    assert!(
        set_value
            .call::<bool>(("status", "spinner_speed", 125_i64))
            .unwrap()
    );
    assert_eq!(store.snapshot().values["ui"]["spinner_speed"], 125);

    let upsert_subagent: mlua::Function = config.get("upsert_subagent").unwrap();
    let agent = lua.create_table().unwrap();
    agent.set("name", "reviewer").unwrap();
    agent.set("description", "Reviews changes").unwrap();
    assert!(upsert_subagent.call::<bool>(agent).unwrap());
    assert_eq!(
        store.snapshot().values["subagents"]["reviewer"]["enabled"],
        true
    );

    let set_subagent_enabled: mlua::Function = config.get("set_subagent_enabled").unwrap();
    assert!(
        set_subagent_enabled
            .call::<bool>(("reviewer", false))
            .unwrap()
    );
    assert_eq!(
        store.snapshot().values["subagents"]["reviewer"]["enabled"],
        false
    );

    let delete_subagent: mlua::Function = config.get("delete_subagent").unwrap();
    assert!(delete_subagent.call::<bool>("reviewer").unwrap());
    assert!(store.snapshot().values["subagents"]["reviewer"].is_null());

    unsafe {
        match old_bone {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

#[test]
fn config_get_uses_canonical_store_instead_of_filesystem() {
    let lua = Lua::new();
    let store = crate::config::store::ConfigStore::for_test();
    let mut cfg = CtxConfig::new("/tmp".into(), new_shared_state());
    cfg.config_schema = Some(store.schema_for(&[], &[]));
    cfg.config_store = Some(store);
    let config = build_config_table(&lua, &cfg).unwrap();
    let get: mlua::Function = config.get("get").unwrap();

    assert_eq!(get.call::<String>(("general", "approval")).unwrap(), "safe");
}

#[test]
fn config_get_table_exposes_canonical_enablement() {
    let _guard = crate::util::test_env_lock();
    let previous = std::env::var_os("BONE_DIR");
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", dir.path()) };

    let lua = Lua::new();
    let store =
        crate::config::store::ConfigStore::new(crate::ext::ExtensionManager::unloaded()).unwrap();
    let schema = store.schema_for(&["cron".into()], &["compact".into()]);
    let revision = store.snapshot().revision;
    store
        .set_enabled("commands", "compact", false, revision)
        .unwrap();
    let revision = store.snapshot().revision;
    store.set_enabled("tools", "cron", false, revision).unwrap();
    let mut cfg = CtxConfig::new("/tmp".into(), new_shared_state());
    cfg.config_schema = Some(schema);
    cfg.config_store = Some(store);
    let config = build_config_table(&lua, &cfg).unwrap();
    let get_table: mlua::Function = config.get("get_table").unwrap();

    let commands: mlua::Table = get_table.call("commands").unwrap();
    let tools: mlua::Table = get_table.call("tools").unwrap();
    assert!(!commands.get::<bool>("compact").unwrap());
    assert!(!tools.get::<bool>("cron").unwrap());

    unsafe {
        match previous {
            Some(value) => std::env::set_var("BONE_DIR", value),
            None => std::env::remove_var("BONE_DIR"),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn agent_opts_do_not_inherit_model_when_provider_changes() {
    let lua = Lua::new();
    let opts = lua.create_table().unwrap();
    opts.set("provider", "openrouter").unwrap();

    let (_, provider, model, _, _) = parse_agent_opts(
        &Some(opts),
        crate::tools::ApprovalMode::Safe,
        &Some("local".to_string()),
        &Some("local".to_string()),
        &["provider", "model"],
    )
    .unwrap();

    assert_eq!(provider.as_deref(), Some("openrouter"));
    assert_eq!(model, None);
}

#[test]
fn agent_opts_inherit_model_when_provider_is_inherited() {
    let lua = Lua::new();
    let opts = lua.create_table().unwrap();

    let (_, provider, model, _, _) = parse_agent_opts(
        &Some(opts),
        crate::tools::ApprovalMode::Safe,
        &Some("local".to_string()),
        &Some("local".to_string()),
        &["provider", "model"],
    )
    .unwrap();

    assert_eq!(provider.as_deref(), Some("local"));
    assert_eq!(model.as_deref(), Some("local"));
}

#[test]
fn agent_depth_exceeded_shape() {
    // A depth/opts error from the dispatch closures is rendered through
    // agent_result_to_lua as { ok=false, content="", error=<msg> }.
    let lua = Lua::new();
    let result = agent_result_to_lua(&lua, Err("max agent depth exceeded".to_string())).unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert_eq!(tbl["ok"], false);
    assert_eq!(tbl["content"], "");
    assert_eq!(tbl["error"], "max agent depth exceeded");
}

#[test]
fn usage_context_serializes_with_correct_keys() {
    let usage = UsageContext {
        request_count: 5,
        sent: 1000,
        received: 500,
        cached: 200,
        cost: 0.0123,
        context_length: 4096,
        tool_count: 3,
        tool_schema_chars: 256,
        tool_schema_tokens: 64,
        system_prompt_chars: 128,
        system_prompt_tokens: 32,
        by_provider: vec![UsageProviderContext {
            provider: "openrouter".into(),
            model: "gemini".into(),
            prompt_tokens: 100,
            completion_tokens: 50,
            cached_tokens: 20,
            cost: 0.005,
            request_count: 2,
        }],
    };
    let lua = Lua::new();
    let result = lua.to_value(&usage).unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();

    assert_eq!(tbl["request_count"], 5);
    assert_eq!(tbl["sent"], 1000);
    assert_eq!(tbl["received"], 500);
    assert_eq!(tbl["cached"], 200);
    assert_eq!(tbl["cost"], 0.0123);
    assert_eq!(tbl["context_length"], 4096);
    assert_eq!(tbl["tool_count"], 3);
    assert_eq!(tbl["tool_schema_chars"], 256);
    assert_eq!(tbl["tool_schema_tokens"], 64);
    assert_eq!(tbl["system_prompt_chars"], 128);
    assert_eq!(tbl["system_prompt_tokens"], 32);

    let bp = &tbl["by_provider"];
    assert!(bp.is_array());
    assert_eq!(bp.as_array().unwrap().len(), 1);
    let row = &bp[0];
    assert_eq!(row["provider"], "openrouter");
    assert_eq!(row["model"], "gemini");
}

#[test]
fn usage_provider_context_serializes_correctly() {
    let provider = UsageProviderContext {
        provider: "anthropic".into(),
        model: "claude-sonnet".into(),
        prompt_tokens: 300,
        completion_tokens: 150,
        cached_tokens: 50,
        cost: 0.008,
        request_count: 1,
    };
    let lua = Lua::new();
    let result = lua.to_value(&provider).unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert_eq!(tbl["provider"], "anthropic");
    assert_eq!(tbl["model"], "claude-sonnet");
    assert_eq!(tbl["prompt_tokens"], 300);
    assert_eq!(tbl["completion_tokens"], 150);
    assert_eq!(tbl["cached_tokens"], 50);
    assert_eq!(tbl["cost"], 0.008);
    assert_eq!(tbl["request_count"], 1);
}

#[test]
fn tool_definition_serializes_correctly() {
    let def = crate::tools::ToolDefinition {
        name: "read_file".into(),
        description: "Read a file".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            }
        }),
    };
    let lua = Lua::new();
    let result = lua.to_value(&def).unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert_eq!(tbl["name"], "read_file");
    assert_eq!(tbl["description"], "Read a file");
    assert!(tbl["input_schema"].is_object());
}

#[test]
fn tool_definition_array_serializes_correctly() {
    let defs = vec![
        crate::tools::ToolDefinition {
            name: "read_file".into(),
            description: "Read".into(),
            input_schema: serde_json::json!({}),
        },
        crate::tools::ToolDefinition {
            name: "write_file".into(),
            description: "Write".into(),
            input_schema: serde_json::json!({}),
        },
    ];
    let lua = Lua::new();
    let result = lua.to_value(&defs).unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert!(tbl.is_array());
    assert_eq!(tbl.as_array().unwrap().len(), 2);
}

// ── ui.status / ui.notify emit RuntimeEvent::Status (compaction feedback) ────

/// When `runtime_status` is set (the interactive Driver path), `ctx.ui.status`
/// and info-level `ctx.ui.notify` surface to the frontend as a `Status` event.
/// This is the channel auto-compaction uses to announce progress + savings.
#[test]
fn ui_status_and_info_notify_emit_runtime_status() {
    use crate::runtime::RuntimeEvent;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let mut cfg = test_ctx_config();
    cfg.runtime_status = Some(tx);

    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    lua.load("ctx.ui.status('Compacting context...')")
        .exec()
        .unwrap();
    lua.load("ctx.ui.notify('Compacted: 40 → 5 messages', 'info')")
        .exec()
        .unwrap();

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert_eq!(
        events.len(),
        2,
        "status + info notify should each emit one event"
    );
    match &events[0] {
        RuntimeEvent::Status { message } => assert_eq!(message, "Compacting context..."),
        other => panic!("first event should be Status, got {other:?}"),
    }
    match &events[1] {
        RuntimeEvent::Status { message } => assert_eq!(message, "Compacted: 40 → 5 messages"),
        other => panic!("second event should be Status, got {other:?}"),
    }
}

/// `ctx.ui.notice` emits a `Notice` event (persistent, kept in the transcript)
/// rather than a transient `Status`. This is how Lua marks a message as worth
/// surfacing without the host substring-matching the text — the seam that
/// removed the hardcoded `contains("compact")` check in the stream handler.
#[test]
fn ui_notice_emits_runtime_notice() {
    use crate::runtime::RuntimeEvent;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeEvent>();
    let mut cfg = test_ctx_config();
    cfg.runtime_status = Some(tx);

    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    lua.load("ctx.ui.notice('Compacted: saved 1234 tokens')")
        .exec()
        .unwrap();

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    assert_eq!(events.len(), 1, "notice should emit one event");
    match &events[0] {
        RuntimeEvent::Notice { message } => assert_eq!(message, "Compacted: saved 1234 tokens"),
        other => panic!("event should be Notice, got {other:?}"),
    }
}

// Without a frontend (headless before_turn), `ctx.ui.status` must not send and
// must not panic — it falls back to stderr.
#[test]
fn ui_status_without_frontend_is_inert() {
    let cfg = test_ctx_config();
    assert!(cfg.runtime_status.is_none());

    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();
    // Must not error.
    lua.load("ctx.ui.status('headless line')").exec().unwrap();
}

// ── AppCtxState parity (commands ⇆ tools share one ctx) ─────────────────────

fn sample_app_state() -> AppCtxState {
    let tools =
        crate::tools::registry::ToolHandler::new(crate::tools::registry::ToolRegistry::default());
    let stats = crate::llm::TokenStats {
        sent: 1234,
        ..Default::default()
    };
    let history = vec![
        crate::llm::ChatMessage::new(crate::llm::ChatRole::User, "hello"),
        crate::llm::ChatMessage::new(crate::llm::ChatRole::Assistant, "hi there"),
    ];
    let config_store = crate::config::store::ConfigStore::for_test();
    let config_schema = config_store.schema_for(&[], &[]);
    AppCtxState::new(
        &tools,
        &stats,
        &crate::tools::ApprovalMode::Danger,
        Some(42),
        "openrouter",
        "gemini",
        Some(131_072),
        None,
        Vec::new(),
        history,
        config_store,
        config_schema,
        None,
    )
}

fn cfg_from(state: &AppCtxState) -> CtxConfig {
    let shared: SharedState = Arc::new(Mutex::new(HashMap::new()));
    let mut cfg = CtxConfig::new("/tmp".to_string(), shared);
    state.apply_to(&mut cfg);
    cfg
}

// The single mapping (`apply_to`) populates every app-derived field. Both the
// command runner and the tool path route through it, so this is the parity
// guarantee at the CtxConfig level.
#[test]
fn app_ctx_state_apply_to_populates_all_app_fields() {
    let cfg = cfg_from(&sample_app_state());

    assert_eq!(cfg.session_id, Some(42));
    assert_eq!(cfg.provider.as_deref(), Some("openrouter"));
    assert_eq!(cfg.model.as_deref(), Some("gemini"));
    assert_eq!(cfg.context_window_tokens, Some(131_072));
    assert_eq!(cfg.approval_mode, crate::tools::ApprovalMode::Danger);
    assert!(cfg.tool_handler.is_some());
    assert_eq!(cfg.usage.as_ref().unwrap().sent, 1234);
    assert_eq!(cfg.conversation_history.as_ref().unwrap().len(), 2);
}

// The same fields are visible on the Lua `ctx` surface (what a command/tool
// handler actually reads).
#[test]
fn app_ctx_state_exposes_app_fields_through_lua_ctx() {
    let cfg = cfg_from(&sample_app_state());
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let current: Value = lua
        .load("return ctx.conversation.current()")
        .eval()
        .unwrap();
    let current: serde_json::Value = lua.from_value(current).unwrap();
    assert_eq!(current["id"], 42);
    assert_eq!(current["provider"], "openrouter");
    assert_eq!(current["model"], "gemini");

    let hist_len: usize = lua
        .load("return #ctx.conversation.history()")
        .eval()
        .unwrap();
    assert_eq!(hist_len, 2);

    let sent: u64 = lua.load("return ctx.usage.snapshot().sent").eval().unwrap();
    assert_eq!(sent, 1234);
    let capacity: u64 = lua
        .load("return ctx.model.context_window_tokens")
        .eval()
        .unwrap();
    assert_eq!(capacity, 131_072);
}

#[test]
fn agent_opts_use_explicit_model_when_provider_changes() {
    let lua = Lua::new();
    let opts = lua.create_table().unwrap();
    opts.set("provider", "openrouter").unwrap();
    opts.set("model", "google/gemini-3.1-flash-lite").unwrap();

    let (_, provider, model, _, _) = parse_agent_opts(
        &Some(opts),
        crate::tools::ApprovalMode::Safe,
        &Some("local".to_string()),
        &Some("local".to_string()),
        &["provider", "model"],
    )
    .unwrap();

    assert_eq!(provider.as_deref(), Some("openrouter"));
    assert_eq!(model.as_deref(), Some("google/gemini-3.1-flash-lite"));
}

// ── await_cancelled: the cancel-detection future shared by run/spawn ─────────

#[test]
fn await_cancelled_resolves_once_flag_is_set() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let setter = flag.clone();
    rt.spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        setter.store(true, Ordering::Relaxed);
    });
    rt.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            await_cancelled(&Some(flag)),
        )
        .await
        .expect("await_cancelled must resolve once the flag flips to true");
    });
}

#[test]
fn await_cancelled_stays_pending_when_unset_or_absent() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    // No flag at all → never resolves.
    let none = rt.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_millis(120),
            await_cancelled(&None),
        )
        .await
    });
    assert!(none.is_err(), "await_cancelled(None) must never resolve");

    // Flag present but still false → stays pending within the poll window.
    let flag = Arc::new(AtomicBool::new(false));
    let pending = rt.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_millis(120),
            await_cancelled(&Some(flag)),
        )
        .await
    });
    assert!(
        pending.is_err(),
        "await_cancelled must stay pending while the flag is false"
    );
}

// ── extract_tool_allowlist: per-agent tools={} parsing for ctx.agent.spawn ──

#[test]
fn extract_tool_allowlist_reads_named_tools_in_order() {
    let lua = Lua::new();
    let opts = lua.create_table().unwrap();
    let tools = lua.create_table().unwrap();
    tools.set(1, "read_file").unwrap();
    tools.set(2, "ls").unwrap();
    opts.set("tools", tools).unwrap();

    assert_eq!(
        extract_tool_allowlist(&Some(opts)),
        Some(vec!["read_file".to_string(), "ls".to_string()]),
    );
}

#[test]
fn extract_tool_allowlist_none_when_key_absent() {
    let lua = Lua::new();
    let opts = lua.create_table().unwrap();
    assert_eq!(extract_tool_allowlist(&Some(opts)), None);
    assert_eq!(extract_tool_allowlist(&None), None);
}

#[test]
fn extract_tool_allowlist_empty_table_means_zero_tools() {
    let lua = Lua::new();
    let opts = lua.create_table().unwrap();
    let tools = lua.create_table().unwrap();
    opts.set("tools", tools).unwrap();
    assert_eq!(extract_tool_allowlist(&Some(opts)), Some(vec![]));
}

// ── wall_elapsed / wall_timeout_ms ────────────────────────────────────────

#[test]
fn wall_elapsed_some_completes() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(2), wall_elapsed(Some(10)))
            .await
            .expect("wall_elapsed(Some(10)) must complete quickly");
    });
}

#[test]
fn wall_elapsed_none_stays_pending() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(async {
        tokio::time::timeout(std::time::Duration::from_millis(100), wall_elapsed(None)).await
    });
    assert!(
        result.is_err(),
        "wall_elapsed(None) must never resolve (timeout expected)"
    );
}

#[test]
fn provider_slot_wait_counts_toward_wall_timeout() {
    let _guard = crate::util::test_env_lock();
    let old_bone = std::env::var_os("BONE_DIR");
    let temp = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BONE_DIR", temp.path()) };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _held = crate::ext::provider_slots::acquire("wall-test", Some(1), None)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let error = acquire_provider_slot("wall-test", Some(1), None, Some(20))
            .await
            .err()
            .unwrap();
        assert!(error.contains("wall-clock limit"));
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    });
    match old_bone {
        Some(value) => unsafe { std::env::set_var("BONE_DIR", value) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
}

// Regression: tools and before_turn must share one session-scoped map so a
// value written by a tool (e.g. task_list) is readable from before_turn.
// Passing the same Arc into both CtxConfigs is the host contract; separate
// `new_shared_state()` calls intentionally isolate conversations.
#[test]
fn ctx_state_is_shared_across_contexts() {
    let shared = new_shared_state();

    // Writer context (stands in for a tool invocation).
    let mut writer_cfg = test_ctx_config();
    writer_cfg.shared_state = shared.clone();
    let lua_w = Lua::new();
    let ctx_w = create_ctx_table(&lua_w, &writer_cfg).unwrap();
    lua_w.globals().set("ctx", ctx_w).unwrap();
    lua_w
        .load(r#"ctx.state.set("task_list", "checklist")"#)
        .exec()
        .unwrap();

    // Reader context, built the same way the before_turn hook is.
    let mut reader_cfg = test_ctx_config();
    reader_cfg.shared_state = shared;
    let lua_r = Lua::new();
    let ctx_r = create_ctx_table(&lua_r, &reader_cfg).unwrap();
    lua_r.globals().set("ctx", ctx_r).unwrap();
    let got: String = lua_r
        .load(r#"return ctx.state.get("task_list")"#)
        .eval()
        .unwrap();

    assert_eq!(
        got, "checklist",
        "value set in one ctx.state must be visible from another (shared map)"
    );
}

#[test]
fn ctx_state_is_isolated_across_fresh_maps() {
    let a = new_shared_state();
    let b = new_shared_state();
    a.lock()
        .unwrap()
        .insert("task_list".into(), "parent".into());
    assert!(
        b.lock().unwrap().get("task_list").is_none(),
        "fresh shared_state must not see another conversation's keys"
    );
}

#[test]
fn extension_shell_primitives_enforce_safe_mode() {
    let _guard = crate::util::test_env_lock();
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    for expression in [
        r#"ctx.shell("rm /tmp/bone-approval-test")"#,
        r#"ctx.shell_streaming("rm /tmp/bone-approval-test", function() end)"#,
        r#"ctx.process.spawn("rm /tmp/bone-approval-test")"#,
    ] {
        let allowed: bool = lua
            .load(format!("return pcall(function() {expression} end)"))
            .eval()
            .unwrap();
        assert!(!allowed, "dangerous extension shell call was not denied");
    }
}

struct BlockingGate;

#[async_trait::async_trait]
impl crate::tools::ApprovalGate for BlockingGate {
    async fn decide(
        &self,
        _blocked: Option<String>,
        _auto_allows: bool,
        _call: &ToolCall,
    ) -> bone_protocol::CallOutcome {
        bone_protocol::CallOutcome::Blocked("blocked by test gate".into())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_shell_primitives_use_approval_gate() {
    let _guard = crate::util::test_env_lock();
    let mut cfg = test_ctx_config();
    cfg.approval_mode = crate::tools::ApprovalMode::Danger;
    cfg.approval_gate = Some(crate::tools::SharedGate(Arc::new(BlockingGate)));
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    for expression in [
        r#"ctx.shell("echo ok")"#,
        r#"ctx.shell_streaming("echo ok", function() end)"#,
        r#"ctx.process.spawn("echo ok")"#,
    ] {
        let (allowed, error): (bool, String) = lua
            .load(format!(
                "local ok, err = pcall(function() {expression} end); return ok, tostring(err)"
            ))
            .eval()
            .unwrap();
        assert!(!allowed);
        assert!(error.contains("blocked by test gate"), "{error}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_shell_primitives_honor_cancellation() {
    let _guard = crate::util::test_env_lock();
    for expression in [
        r#"ctx.shell("sleep 30 & wait")"#,
        r#"ctx.shell_streaming("sleep 30 & wait", function() end)"#,
    ] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut cfg = test_ctx_config();
        cfg.approval_mode = crate::tools::ApprovalMode::Danger;
        cfg.cancelled = Some(cancelled.clone());
        let lua = Lua::new();
        let ctx = create_ctx_table(&lua, &cfg).unwrap();
        lua.globals().set("ctx", ctx).unwrap();
        let cancel_thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            cancelled.store(true, Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let (ok, error): (bool, String) = lua
            .load(format!(
                "local ok, err = pcall(function() {expression} end); return ok, tostring(err)"
            ))
            .eval()
            .unwrap();
        cancel_thread.join().unwrap();
        assert!(!ok, "{expression} unexpectedly succeeded");
        assert!(error.contains("cancelled by user"), "{expression}: {error}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "{expression} did not cancel promptly"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shell_streaming_callback_error_reaps_process_tree() {
    let _guard = crate::util::test_env_lock();
    let mut cfg = test_ctx_config();
    cfg.approval_mode = crate::tools::ApprovalMode::Danger;
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();
    #[cfg(unix)]
    let command = "echo first; sleep 30 & wait";
    #[cfg(windows)]
    let command = "Write-Output first; $p = Start-Process powershell -ArgumentList @('-NoProfile', '-Command', 'Start-Sleep -Seconds 30') -PassThru; $p.WaitForExit()";
    lua.globals().set("shell_command", command).unwrap();
    let started = std::time::Instant::now();
    let (ok, error): (bool, String) = lua
        .load(
            r#"
            local ok, err = pcall(function()
                ctx.shell_streaming(shell_command, function()
                    error("callback failed")
                end)
            end)
            return ok, tostring(err)
            "#,
        )
        .eval()
        .unwrap();
    assert!(!ok);
    assert!(error.contains("callback failed"), "{error}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_processes_are_conversation_scoped() {
    let _guard = crate::util::test_env_lock();
    let mut cfg_a = test_ctx_config();
    cfg_a.approval_mode = crate::tools::ApprovalMode::Danger;
    cfg_a.session_id = Some(101);
    let lua_a = Lua::new();
    let ctx_a = create_ctx_table(&lua_a, &cfg_a).unwrap();
    lua_a.globals().set("ctx", ctx_a).unwrap();
    let id: String = lua_a
        .load(r#"return ctx.process.spawn("sleep 30", { owner = "conversation:202" }).id"#)
        .eval()
        .unwrap();

    let mut cfg_b = test_ctx_config();
    cfg_b.session_id = Some(202);
    let lua_b = Lua::new();
    let ctx_b = create_ctx_table(&lua_b, &cfg_b).unwrap();
    lua_b.globals().set("ctx", ctx_b).unwrap();
    lua_b.globals().set("foreign_id", id.clone()).unwrap();
    let (status_hidden, output_hidden, kill_denied, listed): (bool, bool, bool, bool) = lua_b
        .load(
            r#"
            local listed = false
            for _, process in ipairs(ctx.process.list()) do
                if process.id == foreign_id then listed = true end
            end
            return ctx.process.status(foreign_id) == nil,
                   ctx.process.output(foreign_id) == nil,
                   ctx.process.kill(foreign_id),
                   listed
            "#,
        )
        .eval()
        .unwrap();
    assert!(status_hidden);
    assert!(output_hidden);
    assert!(!kill_denied);
    assert!(!listed);

    lua_a.globals().set("own_id", id).unwrap();
    assert!(
        lua_a
            .load("return ctx.process.kill(own_id)")
            .eval::<bool>()
            .unwrap()
    );
}

#[test]
fn ctx_exec_and_codec_are_available_and_binary_safe() {
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();
    let encoded: String = lua
        .load("return ctx.codec.base64_encode('\\0\\255')")
        .eval()
        .unwrap();
    assert_eq!(encoded, "AP8=");
    let hash: String = lua.load("return ctx.codec.sha256('abc')").eval().unwrap();
    assert_eq!(
        hash,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let random: String = lua.load("return ctx.codec.random_hex(16)").eval().unwrap();
    assert_eq!(random.len(), 32);
    assert!(random.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let available: bool = lua
        .load("return type(ctx.exec) == 'function' and type(ctx.codec.base64_encode) == 'function' and type(ctx.codec.sha256) == 'function'")
        .eval()
        .unwrap();
    assert!(available);
}

fn test_png(width: u32, height: u32, color: png::ColorType, pixels: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(pixels).unwrap();
    }
    output
}

fn test_rgba_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    test_png(width, height, png::ColorType::Rgba, pixels)
}

fn test_png_crc(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn append_test_png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(kind.len() + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    output.extend_from_slice(&test_png_crc(&crc_input).to_be_bytes());
}

/// Construct a syntactically valid PNG header without allocating its pixels.
/// `decode_png_rgba` checks the declared dimensions before decoding IDAT.
fn test_png_header(width: u32, height: u32) -> Vec<u8> {
    let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    append_test_png_chunk(&mut output, b"IHDR", &ihdr);
    append_test_png_chunk(&mut output, b"IDAT", &[]);
    append_test_png_chunk(&mut output, b"IEND", &[]);
    output
}

#[test]
fn ctx_time_is_monotonic_and_sleep_is_native() {
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let (before, after): (u64, u64) = lua
        .load(
            r#"
            local before = ctx.time.monotonic_ms()
            assert(ctx.time.sleep_ms(2))
            return before, ctx.time.monotonic_ms()
            "#,
        )
        .eval()
        .unwrap();
    assert!(after >= before);
}

#[test]
fn ctx_time_sleep_honors_cancellation_and_bounds() {
    let cancelled = Arc::new(AtomicBool::new(true));
    let mut cfg = test_ctx_config();
    cfg.cancelled = Some(cancelled.clone());
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let already_cancelled = lua
        .load("return ctx.time.sleep_ms(1000)")
        .eval::<bool>()
        .unwrap_err()
        .to_string();
    assert!(already_cancelled.contains("timer cancelled"));

    cancelled.store(false, Ordering::Relaxed);
    let cancel_from_thread = cancelled.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        cancel_from_thread.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let mid_sleep_cancelled = lua
        .load("return ctx.time.sleep_ms(5000)")
        .eval::<bool>()
        .unwrap_err()
        .to_string();
    canceller.join().unwrap();
    assert!(mid_sleep_cancelled.contains("timer cancelled"));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "cancelled timer did not return promptly"
    );

    cancelled.store(false, Ordering::Relaxed);
    let (zero_succeeds, over_limit_fails): (bool, bool) = lua
        .load(
            r#"
            local zero_succeeds = ctx.time.sleep_ms(0)
            local over_limit_succeeds = pcall(function()
                ctx.time.sleep_ms(60001)
            end)
            return zero_succeeds, not over_limit_succeeds
            "#,
        )
        .eval()
        .unwrap();
    assert!(zero_succeeds);
    assert!(over_limit_fails);
}

#[test]
fn ctx_codec_random_hex_is_bounded_and_fresh() {
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let (default_token, first, second, maximum, below_fails, above_fails): (
        String,
        String,
        String,
        String,
        bool,
        bool,
    ) = lua
        .load(
            r#"
            local default_token = ctx.codec.random_hex()
            local first = ctx.codec.random_hex(8)
            local second = ctx.codec.random_hex(8)
            local maximum = ctx.codec.random_hex(64)
            local below_succeeds = pcall(function() ctx.codec.random_hex(7) end)
            local above_succeeds = pcall(function() ctx.codec.random_hex(65) end)
            return default_token, first, second, maximum,
                   not below_succeeds, not above_succeeds
            "#,
        )
        .eval()
        .unwrap();
    assert_eq!(default_token.len(), 32);
    assert_eq!(first.len(), 16);
    assert_eq!(second.len(), 16);
    assert_ne!(first, second);
    assert_eq!(maximum.len(), 128);
    assert!(
        [default_token, first, second, maximum]
            .into_iter()
            .all(|token| token.bytes().all(|byte| byte.is_ascii_hexdigit()))
    );
    assert!(below_fails);
    assert!(above_fails);
}

#[test]
fn ctx_png_tiles_and_diff_are_binary_safe() {
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let before = test_rgba_png(
        2,
        2,
        &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
    );
    let after = test_rgba_png(
        2,
        2,
        &[0, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
    );
    lua.globals()
        .set("before_png", lua.create_string(&before).unwrap())
        .unwrap();
    lua.globals()
        .set("after_png", lua.create_string(&after).unwrap())
        .unwrap();

    let (before_hash, after_hash, changed, x, y): (String, String, u64, u32, u32) = lua
        .load(
            r#"
            local before_tiles = ctx.codec.png_tiles(before_png, 2, 2)
            local after_tiles = ctx.codec.png_tiles(after_png, 2, 2)
            local diff = ctx.codec.png_diff(before_png, after_png)
            return before_tiles.hashes[2], after_tiles.hashes[2],
                   diff.changed_pixels, diff.bounds.x, diff.bounds.y
            "#,
        )
        .eval()
        .unwrap();
    assert_ne!(before_hash, after_hash);
    assert_eq!(changed, 1);
    assert_eq!((x, y), (1, 0));
}

#[test]
fn ctx_png_codecs_normalize_color_formats_to_rgba() {
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let rgb = test_png(2, 1, png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6]);
    let rgba = test_rgba_png(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]);
    let grayscale_alpha = test_png(2, 1, png::ColorType::GrayscaleAlpha, &[42, 17, 200, 255]);
    let grayscale_rgba = test_rgba_png(2, 1, &[42, 42, 42, 17, 200, 200, 200, 255]);
    for (name, value) in [
        ("rgb_png", rgb),
        ("rgba_png", rgba),
        ("grayscale_alpha_png", grayscale_alpha),
        ("grayscale_rgba_png", grayscale_rgba),
    ] {
        lua.globals()
            .set(name, lua.create_string(&value).unwrap())
            .unwrap();
    }

    let (rgb_equal, gray_equal, rgb_hashes_match, gray_hashes_match): (bool, bool, bool, bool) =
        lua.load(
            r#"
            local rgb_diff = ctx.codec.png_diff(rgb_png, rgba_png)
            local gray_diff = ctx.codec.png_diff(
                grayscale_alpha_png,
                grayscale_rgba_png
            )
            local rgb_tiles = ctx.codec.png_tiles(rgb_png, 2, 1)
            local rgba_tiles = ctx.codec.png_tiles(rgba_png, 2, 1)
            local gray_tiles = ctx.codec.png_tiles(grayscale_alpha_png, 2, 1)
            local gray_rgba_tiles = ctx.codec.png_tiles(grayscale_rgba_png, 2, 1)
            return rgb_diff.equal and rgb_diff.bounds == nil,
                   gray_diff.equal and gray_diff.bounds == nil,
                   rgb_tiles.hashes[1] == rgba_tiles.hashes[1]
                       and rgb_tiles.hashes[2] == rgba_tiles.hashes[2],
                   gray_tiles.hashes[1] == gray_rgba_tiles.hashes[1]
                       and gray_tiles.hashes[2] == gray_rgba_tiles.hashes[2]
            "#,
        )
        .eval()
        .unwrap();
    assert!(rgb_equal);
    assert!(gray_equal);
    assert!(rgb_hashes_match);
    assert!(gray_hashes_match);
}

#[test]
fn ctx_png_resize_preserves_aspect_ratio_and_never_upscales() {
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let wide = test_rgba_png(4, 2, &[10, 20, 30, 255].repeat(8));
    let tall = test_rgba_png(2, 4, &[40, 50, 60, 255].repeat(8));
    let small = test_png(2, 1, png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6]);
    for (name, value) in [
        ("wide_png", wide),
        ("tall_png", tall),
        ("small_png", small.clone()),
    ] {
        lua.globals()
            .set(name, lua.create_string(&value).unwrap())
            .unwrap();
    }

    let wide_result: Table = lua
        .load("return ctx.codec.png_resize(wide_png, 2, 100)")
        .eval()
        .unwrap();
    let wide_output: mlua::String = wide_result.get("png").unwrap();
    let wide_decoded = decode_png_rgba(&wide_output.as_bytes()).unwrap();
    assert_eq!(
        (
            wide_result.get::<u32>("width").unwrap(),
            wide_result.get::<u32>("height").unwrap(),
            wide_result.get::<bool>("resized").unwrap(),
        ),
        (2, 1, true)
    );
    assert_eq!((wide_decoded.width, wide_decoded.height), (2, 1));

    let tall_result: Table = lua
        .load("return ctx.codec.png_resize(tall_png, 100, 2)")
        .eval()
        .unwrap();
    let tall_output: mlua::String = tall_result.get("png").unwrap();
    let tall_decoded = decode_png_rgba(&tall_output.as_bytes()).unwrap();
    assert_eq!(
        (
            tall_result.get::<u32>("width").unwrap(),
            tall_result.get::<u32>("height").unwrap(),
            tall_result.get::<bool>("resized").unwrap(),
        ),
        (1, 2, true)
    );
    assert_eq!((tall_decoded.width, tall_decoded.height), (1, 2));

    let small_result: Table = lua
        .load("return ctx.codec.png_resize(small_png, 20, 20)")
        .eval()
        .unwrap();
    let small_output: mlua::String = small_result.get("png").unwrap();
    assert_eq!(small_output.as_bytes().as_ref(), small.as_slice());
    assert_eq!(
        (
            small_result.get::<u32>("width").unwrap(),
            small_result.get::<u32>("height").unwrap(),
            small_result.get::<bool>("resized").unwrap(),
        ),
        (2, 1, false)
    );
}

#[test]
fn ctx_png_resize_filters_in_premultiplied_alpha_space() {
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    // Transparent red next to opaque blue should downsample to translucent
    // blue, not purple. This catches straight-alpha color bleeding.
    let alpha = test_rgba_png(2, 1, &[255, 0, 0, 0, 0, 0, 255, 255]);
    lua.globals()
        .set("alpha_png", lua.create_string(&alpha).unwrap())
        .unwrap();
    let output: mlua::String = lua
        .load("return ctx.codec.png_resize(alpha_png, 1, 1).png")
        .eval()
        .unwrap();
    let decoded = decode_png_rgba(&output.as_bytes()).unwrap();
    assert_eq!((decoded.width, decoded.height), (1, 1));
    assert_eq!(decoded.rgba, [0, 0, 255, 128]);
}

#[test]
fn ctx_png_region_sha256_is_normalized_and_localized() {
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let base = test_rgba_png(
        3,
        2,
        &[
            1, 1, 1, 255, 10, 20, 30, 255, 40, 50, 60, 255, 2, 2, 2, 255, 70, 80, 90, 255, 100,
            110, 120, 255,
        ],
    );
    let outside_change = test_rgba_png(
        3,
        2,
        &[
            9, 9, 9, 255, 10, 20, 30, 255, 40, 50, 60, 255, 8, 8, 8, 255, 70, 80, 90, 255, 100,
            110, 120, 255,
        ],
    );
    let inside_change = test_rgba_png(
        3,
        2,
        &[
            1, 1, 1, 255, 11, 20, 30, 255, 40, 50, 60, 255, 2, 2, 2, 255, 70, 80, 90, 255, 100,
            110, 120, 255,
        ],
    );
    let normalized_rgb = test_png(2, 1, png::ColorType::Rgb, &[10, 20, 30, 40, 50, 60]);
    let normalized_rgba = test_rgba_png(2, 1, &[10, 20, 30, 255, 40, 50, 60, 255]);
    for (name, value) in [
        ("base_png", base),
        ("outside_png", outside_change),
        ("inside_png", inside_change),
        ("normalized_rgb_png", normalized_rgb),
        ("normalized_rgba_png", normalized_rgba),
    ] {
        lua.globals()
            .set(name, lua.create_string(&value).unwrap())
            .unwrap();
    }

    let (base_hash, outside_hash, inside_hash, width, height): (String, String, String, u32, u32) =
        lua.load(
            r#"
            local base = ctx.codec.png_region_sha256(base_png, 1, 0, 2, 2)
            local outside = ctx.codec.png_region_sha256(
                outside_png, 1, 0, 2, 2
            )
            local inside = ctx.codec.png_region_sha256(
                inside_png, 1, 0, 2, 2
            )
            return base.sha256, outside.sha256, inside.sha256,
                   base.width, base.height
            "#,
        )
        .eval()
        .unwrap();
    assert_eq!(base_hash.len(), 64);
    assert_eq!(base_hash, outside_hash);
    assert_ne!(base_hash, inside_hash);
    assert_eq!((width, height), (2, 2));
    let normalized_match: bool = lua
        .load(
            r#"
            return ctx.codec.png_region_sha256(
                       normalized_rgb_png, 0, 0, 2, 1
                   ).sha256
                == ctx.codec.png_region_sha256(
                       normalized_rgba_png, 0, 0, 2, 1
                   ).sha256
            "#,
        )
        .eval()
        .unwrap();
    assert!(normalized_match);
}

#[test]
fn ctx_png_codecs_reject_bad_bounds_limits_and_cancellation() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut cfg = test_ctx_config();
    cfg.cancelled = Some(cancelled.clone());
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let valid = test_rgba_png(2, 2, &[0; 16]);
    let oversized_header = test_png_header(40_000_001, 1);
    lua.globals()
        .set("valid_png", lua.create_string(&valid).unwrap())
        .unwrap();
    lua.globals()
        .set(
            "oversized_png",
            lua.create_string(&oversized_header).unwrap(),
        )
        .unwrap();

    let (zero_width, zero_height, zero_region, outside_region, oversized): (
        bool,
        bool,
        bool,
        bool,
        bool,
    ) = lua
        .load(
            r#"
            local zero_width = pcall(function()
                ctx.codec.png_resize(valid_png, 0, 1)
            end)
            local zero_height = pcall(function()
                ctx.codec.png_resize(valid_png, 1, 0)
            end)
            local zero_region = pcall(function()
                ctx.codec.png_region_sha256(valid_png, 0, 0, 0, 1)
            end)
            local outside_region = pcall(function()
                ctx.codec.png_region_sha256(valid_png, 1, 1, 2, 1)
            end)
            local oversized = pcall(function()
                ctx.codec.png_resize(oversized_png, 1, 1)
            end)
            return not zero_width, not zero_height, not zero_region,
                   not outside_region, not oversized
            "#,
        )
        .eval()
        .unwrap();
    assert!(zero_width);
    assert!(zero_height);
    assert!(zero_region);
    assert!(outside_region);
    assert!(oversized);

    cancelled.store(true, Ordering::Relaxed);
    let (resize_cancelled, region_cancelled, tiles_cancelled, diff_cancelled): (
        bool,
        bool,
        bool,
        bool,
    ) = lua
        .load(
            r#"
            local resize_ok, resize_error = pcall(function()
                ctx.codec.png_resize(valid_png, 1, 1)
            end)
            local region_ok, region_error = pcall(function()
                ctx.codec.png_region_sha256(valid_png, 0, 0, 1, 1)
            end)
            local tiles_ok, tiles_error = pcall(function()
                ctx.codec.png_tiles(valid_png, 1, 1)
            end)
            local diff_ok, diff_error = pcall(function()
                ctx.codec.png_diff(valid_png, valid_png)
            end)
            return not resize_ok
                       and tostring(resize_error):find(
                           "PNG operation cancelled", 1, true
                       ) ~= nil,
                   not region_ok
                       and tostring(region_error):find(
                           "PNG operation cancelled", 1, true
                       ) ~= nil,
                   not tiles_ok
                       and tostring(tiles_error):find(
                           "PNG operation cancelled", 1, true
                       ) ~= nil,
                   not diff_ok
                       and tostring(diff_error):find(
                           "PNG operation cancelled", 1, true
                       ) ~= nil
            "#,
        )
        .eval()
        .unwrap();
    assert!(resize_cancelled);
    assert!(region_cancelled);
    assert!(tiles_cancelled);
    assert!(diff_cancelled);
}

#[test]
fn ctx_png_codecs_reject_invalid_inputs_and_resource_limits() {
    let cfg = test_ctx_config();
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let valid = test_rgba_png(2, 2, &[0; 16]);
    let different_dimensions = test_rgba_png(1, 1, &[0; 4]);
    let oversized_header = test_png_header(40_000_001, 1);
    for (name, value) in [
        ("valid_png", valid),
        ("different_dimensions_png", different_dimensions),
        ("oversized_png", oversized_header),
    ] {
        lua.globals()
            .set(name, lua.create_string(&value).unwrap())
            .unwrap();
    }

    let (invalid_fails, zero_grid_fails, large_grid_fails, dimensions_fail, oversized_fails): (
        bool,
        bool,
        bool,
        bool,
        bool,
    ) = lua
        .load(
            r#"
            local invalid_ok = pcall(function()
                ctx.codec.png_tiles("not a png", 1, 1)
            end)
            local zero_grid_ok = pcall(function()
                ctx.codec.png_tiles(valid_png, 0, 1)
            end)
            local large_grid_ok = pcall(function()
                ctx.codec.png_tiles(valid_png, 3, 1)
            end)
            local dimensions_ok = pcall(function()
                ctx.codec.png_diff(valid_png, different_dimensions_png)
            end)
            local oversized_ok, oversized_error = pcall(function()
                ctx.codec.png_tiles(oversized_png, 1, 1)
            end)
            return not invalid_ok, not zero_grid_ok, not large_grid_ok,
                   not dimensions_ok,
                   not oversized_ok
                       and tostring(oversized_error):find(
                           "PNG dimensions are too large",
                           1,
                           true
                       ) ~= nil
            "#,
        )
        .eval()
        .unwrap();
    assert!(invalid_fails);
    assert!(zero_grid_fails);
    assert!(large_grid_fails);
    assert!(dimensions_fail);
    assert!(oversized_fails);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctx_exec_passes_metacharacters_as_literal_argv() {
    let _guard = crate::util::test_env_lock();
    let mut cfg = test_ctx_config();
    cfg.approval_mode = crate::tools::ApprovalMode::Danger;
    let lua = Lua::new();
    lua.globals()
        .set("ctx", create_ctx_table(&lua, &cfg).unwrap())
        .unwrap();
    let output: String = lua
        .load(r#"return ctx.exec("printf", {"%s", "$(not-a-command); *"}).stdout"#)
        .eval()
        .unwrap();
    assert_eq!(output, "$(not-a-command); *");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ctx_exec_reports_timeout_and_output_limit() {
    let _guard = crate::util::test_env_lock();
    let mut cfg = test_ctx_config();
    cfg.approval_mode = crate::tools::ApprovalMode::Danger;
    let lua = Lua::new();
    lua.globals()
        .set("ctx", create_ctx_table(&lua, &cfg).unwrap())
        .unwrap();
    let missing: (bool, bool) = lua
        .load(
            r#"local r=ctx.exec("/definitely/not/a/bone-command", {}); return r.spawned,type(r.error)=="string""#,
        )
        .eval()
        .unwrap();
    assert_eq!(missing, (false, true));
    let timed_out: (bool, bool) = lua
        .load(r#"local r=ctx.exec("sleep", {"2"}, {timeout_ms=20}); return r.spawned,r.timed_out"#)
        .eval()
        .unwrap();
    assert_eq!(timed_out, (true, true));
    let started = std::time::Instant::now();
    let descendant_timed_out: bool = lua
        .load(r#"return ctx.exec("sh", {"-c", "sleep 2 &"}, {timeout_ms=20}).timed_out"#)
        .eval()
        .unwrap();
    assert!(descendant_timed_out);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    let limited: bool = lua
        .load(r#"return ctx.exec("printf", {"123456789"}, {max_output_bytes=4}).output_limit_exceeded"#)
        .eval()
        .unwrap();
    assert!(limited);
}

struct CaptureExecGate(Arc<Mutex<String>>);

#[async_trait::async_trait]
impl crate::tools::ApprovalGate for CaptureExecGate {
    async fn decide(
        &self,
        _: Option<String>,
        _: bool,
        call: &ToolCall,
    ) -> bone_protocol::CallOutcome {
        *self.0.lock().unwrap() = call.arguments.to_string();
        bone_protocol::CallOutcome::Blocked("blocked".into())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ctx_exec_redacted_approval_does_not_expose_arguments() {
    let _guard = crate::util::test_env_lock();
    let seen = Arc::new(Mutex::new(String::new()));
    let mut cfg = test_ctx_config();
    cfg.approval_mode = crate::tools::ApprovalMode::Danger;
    cfg.approval_gate = Some(crate::tools::SharedGate(Arc::new(CaptureExecGate(
        seen.clone(),
    ))));
    let lua = Lua::new();
    lua.globals()
        .set("ctx", create_ctx_table(&lua, &cfg).unwrap())
        .unwrap();
    let _: (bool, String) = lua.load(r#"local ok,e=pcall(function() ctx.exec("echo", {"SECRET-ARG"}, {redact_args=true}) end); return ok,tostring(e)"#).eval().unwrap();
    let preview = seen.lock().unwrap().clone();
    assert!(
        !preview.contains("SECRET-ARG"),
        "redacted approval leaked argv: {preview}"
    );
}

#[test]
fn runtime_info_exposes_read_only_execution_metadata() {
    let mut cfg = test_ctx_config();
    cfg.session_id = Some(42);
    cfg.provider = Some("openrouter".into());
    cfg.model = Some("test-model".into());
    cfg.agent_depth = 2;
    cfg.approval_mode = crate::tools::ApprovalMode::Danger;
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    let info: serde_json::Value = lua
        .from_value(lua.load("return ctx.runtime.info()").eval().unwrap())
        .unwrap();
    assert_eq!(info["session_id"], 42);
    assert_eq!(info["provider"], "openrouter");
    assert_eq!(info["model"], "test-model");
    assert_eq!(info["agent_depth"], 2);
    assert_eq!(info["approval_mode"], "danger");
    assert_eq!(info["execution"]["kind"], "agent");
    assert_eq!(info["execution"]["depth"], 2);
}

#[test]
fn conversation_submit_and_load_queue_generic_operations() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut cfg = test_ctx_config();
    cfg.conversation_operations = Some(tx);
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    lua.load(
        r#"
        assert(ctx.conversation.submit("continue this") == true)
        assert(ctx.conversation.load(17) == true)
        "#,
    )
    .exec()
    .unwrap();

    assert_eq!(
        crate::ext::inbox::for_lua(&lua).drain(),
        vec!["continue this"]
    );
    assert_eq!(rx.try_recv().unwrap(), ConversationOperation::Load(17));
}

#[test]
fn ui_apply_accepts_protocol_view_diffs() {
    let ui = crate::ext::api_ui::new_shared();
    let mut cfg = test_ctx_config();
    cfg.ui = Some(ui.clone());
    let lua = Lua::new();
    let ctx = create_ctx_table(&lua, &cfg).unwrap();
    lua.globals().set("ctx", ctx).unwrap();

    lua.load(
        r##"assert(ctx.ui.apply({ kind = "set_highlight", name = "thinking", fg = "#abcdef" }))"##,
    )
    .exec()
    .unwrap();

    assert_eq!(
        crate::ext::api_ui::snapshot(&ui)
            .highlights
            .get("thinking")
            .map(String::as_str),
        Some("#abcdef")
    );
    assert!(matches!(
        crate::ext::api_ui::drain_diffs(&ui).as_slice(),
        [crate::runtime::view::ViewDiff::SetHighlight { name, .. }] if name == "thinking"
    ));
}

#[test]
fn db_query_prefix_allows_select_and_with() {
    assert!(is_allowed_db_query_prefix("SELECT 1"));
    assert!(is_allowed_db_query_prefix("  select 1"));
    assert!(is_allowed_db_query_prefix(
        "WITH recent AS (SELECT 1) SELECT * FROM recent"
    ));
    assert!(is_allowed_db_query_prefix(
        "\n  with recent as (select 1 as id) select * from recent"
    ));
    assert!(!is_allowed_db_query_prefix("INSERT INTO t VALUES (1)"));
    assert!(!is_allowed_db_query_prefix("DELETE FROM t"));
    assert!(!is_allowed_db_query_prefix("UPDATE t SET x = 1"));
    assert!(!is_allowed_db_query_prefix("PRAGMA table_info(t)"));
}
