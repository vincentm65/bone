use super::*;

// ── Helper functions (extracted from inline test module) ────────────────────

fn opt_get<T: mlua::FromLua>(opts: &Option<mlua::Table>, key: &str) -> Option<T> {
    let opts = opts.as_ref()?;
    opts.get::<T>(key).ok()
}

fn tool_call_result(
    lua: &mlua::Lua,
    ok: bool,
    name: Option<String>,
    call_id: Option<String>,
    content: &str,
) -> mlua::Result<mlua::Value> {
    let tbl = lua.create_table()?;
    tbl.set("ok", ok)?;
    tbl.set("is_error", !ok)?;
    if let Some(ref n) = name {
        tbl.set("name", n.clone())?;
    } else {
        tbl.set("name", mlua::Value::Nil)?;
    }
    if let Some(ref c) = call_id {
        tbl.set("call_id", c.clone())?;
    }
    tbl.set("content", content)?;
    Ok(mlua::Value::Table(tbl))
}

fn make_session_current(
    lua: &mlua::Lua,
    id: Option<i32>,
    provider: Option<String>,
    model: Option<String>,
) -> mlua::Result<mlua::Function> {
    let has_session = id.is_some();
    let id_val = id
        .map(|i| mlua::Value::Integer(i as i64))
        .unwrap_or(mlua::Value::Nil);
    let provider_clone = provider.clone();
    let model_clone = model.clone();
    lua.create_function(move |lua, ()| {
        if !has_session {
            return Ok(mlua::Value::Nil);
        }
        let tbl = lua.create_table()?;
        tbl.set("id", id_val.clone())?;
        tbl.set("provider", provider_clone.as_deref())?;
        tbl.set("model", model_clone.as_deref())?;
        Ok(mlua::Value::Table(tbl))
    })
}

fn agent_err_table(lua: &mlua::Lua, error: String) -> mlua::Result<mlua::Value> {
    let tbl = lua.create_table()?;
    tbl.set("ok", false)?;
    tbl.set("content", "")?;
    tbl.set("error", error)?;
    Ok(mlua::Value::Table(tbl))
}

fn spawn_err(lua: &mlua::Lua, error: &str) -> mlua::Result<mlua::Value> {
    let tbl = lua.create_table()?;
    tbl.set("ok", false)?;
    tbl.set("error", error)?;
    Ok(mlua::Value::Table(tbl))
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
fn opt_get_none_opts_returns_none() {
    assert_eq!(opt_get::<String>(&None, "key"), None);
}

#[test]
fn opt_get_missing_key_returns_none() {
    let lua = Lua::new();
    let opts = lua.create_table().unwrap();
    assert_eq!(opt_get::<String>(&Some(opts), "missing"), None);
}

#[test]
fn opt_get_correct_type_returns_some() {
    let lua = Lua::new();
    let opts1 = lua.create_table().unwrap();
    opts1.set("str", "hello").unwrap();
    opts1.set("num", 42u64).unwrap();
    assert_eq!(
        opt_get::<String>(&Some(opts1), "str"),
        Some("hello".to_string())
    );

    let opts2 = lua.create_table().unwrap();
    opts2.set("num", 42u64).unwrap();
    assert_eq!(opt_get::<u64>(&Some(opts2), "num"), Some(42));
}

#[test]
fn opt_get_wrong_type_returns_none() {
    let lua = Lua::new();
    let nested1 = lua.create_table().unwrap();
    let opts1 = lua.create_table().unwrap();
    opts1.set("nested", nested1).unwrap();
    assert_eq!(opt_get::<String>(&Some(opts1), "nested"), None);

    let nested2 = lua.create_table().unwrap();
    let opts2 = lua.create_table().unwrap();
    opts2.set("nested", nested2).unwrap();
    assert_eq!(opt_get::<u64>(&Some(opts2), "nested"), None);
}

#[test]
fn tool_call_result_produces_correct_shape() {
    let lua = Lua::new();
    let result = tool_call_result(
        &lua,
        true,
        Some("ls".into()),
        Some("call-1".into()),
        "output",
    )
    .unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert_eq!(tbl["ok"], true);
    assert_eq!(tbl["is_error"], false);
    assert_eq!(tbl["name"], "ls");
    assert_eq!(tbl["call_id"], "call-1");
    assert_eq!(tbl["content"], "output");
}

#[test]
fn tool_call_result_is_error_inverts_ok() {
    let lua = Lua::new();
    let result = tool_call_result(&lua, false, Some("fail".into()), None, "error msg").unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert_eq!(tbl["ok"], false);
    assert_eq!(tbl["is_error"], true);
    assert!(tbl["call_id"].is_null());
}

#[test]
fn tool_call_result_nil_name_serialises_to_nil() {
    let lua = Lua::new();
    let result = tool_call_result(&lua, false, None, None, "tools unavailable").unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert!(tbl["name"].is_null());
}

#[test]
fn make_session_current_with_session() {
    let lua = Lua::new();
    let fn_ = make_session_current(
        &lua,
        Some(42),
        Some("openrouter".into()),
        Some("gemini".into()),
    )
    .unwrap();
    let result: Value = fn_.call(()).unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert_eq!(tbl["id"], 42);
    assert_eq!(tbl["provider"], "openrouter");
    assert_eq!(tbl["model"], "gemini");
}

#[test]
fn make_session_current_no_session_returns_nil() {
    let lua = Lua::new();
    let fn_ = make_session_current(&lua, None, None, None).unwrap();
    let result = fn_.call::<Value>(()).unwrap();
    assert_eq!(result, Value::Nil);
}

#[test]
fn make_session_current_optional_fields() {
    let lua = Lua::new();
    let fn_ = make_session_current(&lua, Some(1), None, None).unwrap();
    let result: Value = fn_.call(()).unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert_eq!(tbl["id"], 1);
    assert!(tbl.get("provider").is_none_or(|v| v.is_null()));
    assert!(tbl.get("model").is_none_or(|v| v.is_null()));
}

#[test]
fn agent_err_table_shape() {
    let lua = Lua::new();
    let result = agent_err_table(&lua, "something broke".into()).unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert_eq!(tbl["ok"], false);
    assert_eq!(tbl["content"], "");
    assert_eq!(tbl["error"], "something broke");
}

#[test]
fn spawn_err_omits_content_field() {
    let lua = Lua::new();
    let result = spawn_err(&lua, "sub-agents cannot spawn").unwrap();
    let tbl: serde_json::Value = lua.from_value(result).unwrap();
    assert_eq!(tbl["ok"], false);
    assert_eq!(tbl["error"], "sub-agents cannot spawn");
    assert!(tbl.get("content").is_none_or(|v| v.is_null()));
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
    let shared: SharedState = Arc::new(Mutex::new(HashMap::new()));
    let mut cfg = CtxConfig::new("/tmp".to_string(), shared);
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
    let shared: SharedState = Arc::new(Mutex::new(HashMap::new()));
    let mut cfg = CtxConfig::new("/tmp".to_string(), shared);
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
    let shared: SharedState = Arc::new(Mutex::new(HashMap::new()));
    let cfg = CtxConfig::new("/tmp".to_string(), shared);
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
    AppCtxState::new(
        &tools,
        &stats,
        &crate::tools::ApprovalMode::Danger,
        Some(42),
        "openrouter",
        "gemini",
        None,
        Vec::new(),
        history,
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

// Regression: tools and before_turn must share one session-scoped map so a
// value written by a tool (e.g. task_list) is readable from before_turn.
// Passing the same Arc into both CtxConfigs is the host contract; separate
// `new_shared_state()` calls intentionally isolate conversations.
#[test]
fn ctx_state_is_shared_across_contexts() {
    let shared = new_shared_state();

    // Writer context (stands in for a tool invocation).
    let writer_cfg = CtxConfig::new("/tmp".to_string(), shared.clone());
    let lua_w = Lua::new();
    let ctx_w = create_ctx_table(&lua_w, &writer_cfg).unwrap();
    lua_w.globals().set("ctx", ctx_w).unwrap();
    lua_w
        .load(r#"ctx.state.set("task_list", "checklist")"#)
        .exec()
        .unwrap();

    // Reader context, built the same way the before_turn hook is.
    let reader_cfg = CtxConfig::new("/tmp".to_string(), shared);
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
    let cfg = CtxConfig::new("/tmp".to_string(), new_shared_state());
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
    let mut cfg = CtxConfig::new("/tmp".to_string(), new_shared_state());
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
