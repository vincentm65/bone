//! Sub-agent feature tests — registration, spawn lifecycle, depth guard, render.
//!
//! Covers phase 5 of the sub-agent implementation plan:
//!   1. Registration + dynamic description
//!   2. Spawn lifecycle (no provider → job ends Error)
//!   3. Depth guard
//!   4. Render path

mod common;

use std::time::Duration;

use bone::tools::types::ToolCall;

/// Build a default `NewJob` (cap 1, no parent, fresh cancel flag) for tests
/// that seed the registry directly.
fn test_job(agent: &str, task: &str) -> bone::ext::jobs::NewJob {
    bone::ext::jobs::NewJob {
        agent: agent.into(),
        task: task.into(),
        title: String::new(),
        max_concurrency: 1,
        scope: None,
        cancel_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
}

// ── 1. Registration + dynamic description ───────────────────────────────────

/// Two sub-agents registered in init.lua.
const TWO_AGENTS_INIT: &str = r#"
bone.register_subagent({
    name = "researcher",
    description = "Searches the web and summarizes findings",
    system_prompt = "You are a researcher.",
    timeout_ms = 1000,
})

bone.register_subagent({
    name = "coder",
    description = "Writes and fixes code",
    system_prompt = "You are a coder.",
    timeout_ms = 1000,
})
"#;

#[test]
fn two_agents_registered_and_listed_in_tool() {
    let config_dir = common::temp_dir("subagent-two-agents");
    common::seed_catalog_into(&config_dir);
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("init.lua"), TWO_AGENTS_INIT).unwrap();

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

    // The subagent tool should be registered.
    let defs = booted.tools.definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"subagent"),
        "subagent tool should be registered when agents exist; got: {names:?}",
    );

    // Its description should list both agents.
    let subagent_def = defs.iter().find(|d| d.name == "subagent").unwrap();
    assert!(
        subagent_def.description.contains("researcher"),
        "description should mention 'researcher'; got: {}",
        subagent_def.description,
    );
    assert!(
        subagent_def.description.contains("coder"),
        "description should mention 'coder'; got: {}",
        subagent_def.description,
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

/// No init.lua → no sub-agents → no subagent tool.
#[test]
fn no_agents_registered_no_tool() {
    let config_dir = common::temp_dir("subagent-no-agents");
    common::seed_catalog_into(&config_dir);

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

    let defs = booted.tools.definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(
        !names.contains(&"subagent"),
        "subagent tool should NOT be registered when no agents exist; got: {names:?}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

/// A `tool_allowlist` on the boot options narrows the exposed tools to the
/// intersection with the globally-enabled set. Guards the per-agent allowlist
/// wiring (it was previously a dead field).
#[test]
fn tool_allowlist_narrows_exposed_tools() {
    let config_dir = common::temp_dir("subagent-tool-allowlist");
    common::seed_catalog_into(&config_dir);

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone::ext::BootOptions {
            tool_allowlist: Some(vec!["read_file".to_string()]),
            ..Default::default()
        },
        "test-model",
        "TestProvider",
    );

    let defs = booted.tools.definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"read_file"),
        "allowlisted tool should remain; got: {names:?}",
    );
    assert!(
        !names.contains(&"write_file"),
        "non-allowlisted tool should be filtered out; got: {names:?}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 2. Spawn lifecycle (no provider → Error) ────────────────────────────────

#[test]
fn spawn_lifecycle_no_provider() {
    let config_dir = common::temp_dir("subagent-lifecycle");
    common::seed_catalog_into(&config_dir);
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("init.lua"), TWO_AGENTS_INIT).unwrap();

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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let task_marker = "unique-task-spawn-lifecycle-no-provider";

    // Dispatch a task via the subagent tool.
    let call = ToolCall {
        id: "call-lifecycle".into(),
        name: "subagent".into(),
        arguments: serde_json::json!({
            "action": "dispatch",
            "tasks": [{ "agent": "researcher", "task": task_marker }],
        }),
    };

    let results = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(15),
            booted.tools.execute_all(vec![call], 0),
        )
        .await
    });

    let results = results.expect("subagent dispatch timed out");
    assert_eq!(results.len(), 1);

    // The subagent tool returns a JSON envelope with content and pane.
    let content = &results[0].content;
    assert!(
        content.contains("Dispatched"),
        "expected 'Dispatched' in content, got: {}",
        content,
    );

    // The job was created — find it in the registry.
    let registry = bone::ext::jobs::registry();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);

    let mut job_id: Option<String> = None;
    loop {
        let snap = registry.snapshot();
        if let Some(arr) = snap.as_array() {
            for job in arr {
                if job["task"].as_str() == Some(task_marker)
                    && job["status"].as_str() != Some("running")
                {
                    job_id = Some(job["id"].as_str().unwrap().to_string());
                    break;
                }
            }
        }
        if job_id.is_some() {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("no job completed within timeout");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let id = job_id.unwrap();
    assert!(id.starts_with("job-"));

    // Finished jobs are first peeked, then explicitly marked consumed after
    // delivery.
    // (Registry is process-global, so other unconsumed jobs may exist from prior tests.)
    let taken = registry.peek_finished_unconsumed();
    let my_job = taken.iter().find(|j| j.id == id);
    assert!(
        my_job.is_some(),
        "peek_finished_unconsumed should include {}; got jobs: {:?}",
        id,
        taken,
    );
    assert_eq!(my_job.unwrap().status, bone::ext::jobs::JobStatus::Error);
    assert!(!my_job.unwrap().result.as_ref().unwrap().is_empty());
    registry.mark_consumed(std::slice::from_ref(&id));

    // Second peek: this job is no longer unconsumed.
    let taken2 = registry.peek_finished_unconsumed();
    assert!(!taken2.iter().any(|j| j.id == id));

    rt.shutdown_timeout(Duration::from_secs(1));
    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 2b. Wait paths ──────────────────────────────────────────────────────────

/// dispatch with wait=true blocks and returns the job results inline.
#[test]
fn dispatch_with_wait_returns_results_inline() {
    let config_dir = common::temp_dir("subagent-dispatch-wait");
    common::seed_catalog_into(&config_dir);
    std::fs::create_dir_all(&config_dir).unwrap();
    // Unique agent name: the busy-agent check is global by name, so avoid
    // colliding with other tests dispatching in parallel.
    std::fs::write(
        config_dir.join("init.lua"),
        r#"bone.register_subagent({
            name = "waiter-inline",
            description = "test agent",
            system_prompt = "You are a test agent.",
            timeout_ms = 1000,
        })"#,
    )
    .unwrap();

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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let call = ToolCall {
        id: "call-dispatch-wait".into(),
        name: "subagent".into(),
        arguments: serde_json::json!({
            "action": "dispatch",
            "wait": true,
            "tasks": [{ "agent": "waiter-inline", "task": "unique-task-dispatch-wait-inline" }],
        }),
    };

    let results = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(30),
            booted.tools.execute_all(vec![call], 0),
        )
        .await
    });
    rt.shutdown_timeout(Duration::from_secs(1));

    let results = results.expect("dispatch with wait timed out");
    let content = &results[0].content;

    // The dispatch report and the inline job result (no provider → ERROR).
    assert!(
        content.contains("Dispatched 1"),
        "expected dispatch report; got: {content}",
    );
    // The job finished (done or error, depending on provider availability)
    // and its result was returned inline by the same tool call.
    assert!(
        content.contains("## waiter-inline (job-"),
        "expected inline job result section; got: {content}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

/// The standalone wait action returns results for previously dispatched jobs
/// and marks them consumed (no later auto-injection).
#[test]
fn wait_action_collects_dispatched_job() {
    let config_dir = common::temp_dir("subagent-wait-action");
    common::seed_catalog_into(&config_dir);
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("init.lua"),
        r#"bone.register_subagent({
            name = "waiter-collect",
            description = "test agent",
            system_prompt = "You are a test agent.",
        })"#,
    )
    .unwrap();

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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // Create a known finished job directly so this test only exercises the
    // subagent wait action and does not race another real background run.
    let registry = bone::ext::jobs::registry();
    let job_id = registry
        .create(test_job(
            "waiter-collect",
            "unique-task-wait-action-collect",
        ))
        .expect("wait-action job should be created");
    registry.complete(&job_id, Ok("collected job result".into()));

    // Wait on it via the tool.
    let wait = ToolCall {
        id: "call-wait".into(),
        name: "subagent".into(),
        arguments: serde_json::json!({
            "action": "wait",
            "ids": [job_id.clone()],
        }),
    };
    let wait_results = rt
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(30),
                booted.tools.execute_all(vec![wait], 0),
            )
            .await
        })
        .expect("wait timed out");
    rt.shutdown_timeout(Duration::from_secs(1));

    let content = &wait_results[0].content;
    assert!(
        content.contains(&job_id),
        "wait result should reference the job id; got: {content}",
    );

    // Consumed: not delivered again via auto-injection.
    let taken = registry.peek_finished_unconsumed();
    assert!(
        !taken.iter().any(|j| j.id == job_id),
        "waited job must be consumed and not auto-injected",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 3. Depth guard ──────────────────────────────────────────────────────────

/// A tool that tries to spawn a sub-agent job.
const SPAWN_AT_DEPTH: &str = r#"
bone.register_tool({
    name = "spawn_at_depth",
    description = "attempts ctx.agent.spawn at current depth",
    safety = "read_only",
    parameters = { type = "object", properties = {} },
    execute = function(args, ctx)
        local spawn_result = ctx.agent.spawn("test task", {})
        if spawn_result and spawn_result.ok then
            return "ok:id=" .. spawn_result.id
        end
        return "error:" .. (spawn_result and spawn_result.error or "unknown")
    end,
})
"#;

#[test]
fn depth_guard_rejects_spawn_at_depth_1() {
    let config_dir = common::temp_dir("subagent-depth-guard");
    common::seed_catalog_into(&config_dir);
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("depth_guard.lua"), SPAWN_AT_DEPTH).unwrap();

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

    // Verify the tool is registered.
    let defs = booted.tools.definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(
        names.contains(&"spawn_at_depth"),
        "spawn_at_depth tool should be registered; got: {:?}",
        names,
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // Execute at agent_depth = 1.
    let call = ToolCall {
        id: "call-depth-guard".into(),
        name: "spawn_at_depth".into(),
        arguments: serde_json::json!({}),
    };

    let results = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(10),
            booted.tools.execute_all(vec![call], 1),
        )
        .await
    });
    rt.shutdown_timeout(Duration::from_secs(1));

    let results = results.expect("spawn at depth timed out");
    assert_eq!(results.len(), 1);

    // The tool should return a plain text result with ok=false when agent_depth > 0.
    let content = &results[0].content;
    assert!(
        content.starts_with("error:"),
        "spawn should be rejected at depth 1; got: {content}",
    );
    assert!(
        content.contains("sub-agents cannot spawn"),
        "error message should mention the depth guard; got: {}",
        content,
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 4. Render path ──────────────────────────────────────────────────────────

#[test]
fn rust_jobs_pane_returns_valid_panepage() {
    // The pane is driven purely by the job registry — no registered-agent list
    // is consulted, so the agent labels come from the jobs themselves.
    let registry = bone::ext::jobs::registry();
    let id1 = registry
        .create(test_job("render-researcher", "search query"))
        .expect("render-researcher job should be created");
    let _id2 = registry
        .create(test_job("render-coder", "fix bug in module"))
        .expect("render-coder job should be created");
    // Complete one job so only one is running (pane should still show).
    registry.complete(&id1, Ok("found 3 relevant papers".into()));

    // Call the Rust-side pane renderer directly.
    let pane = bone::ui::jobs_pane::render(&registry.all_jobs());
    assert!(
        pane.is_some(),
        "jobs pane renderer should return Some for running jobs; got None",
    );

    let pane = pane.unwrap();
    assert_eq!(pane.source, "jobs");
    assert!(pane.title.contains("Agents"));
    assert!(pane.content.len() >= 2); // at least one line for running agent + separator
}

// ── 5. Cancel through the Lua tool (ctx.agent.cancel → flag) ─────────────────

/// A Lua tool that delegates to `ctx.agent.cancel(id)` and reports the boolean
/// `ok` it returns. Exercises the Lua→registry→cancel-flag wiring that the
/// `spawn` watchdog observes.
const CANCEL_TOOL_LUA: &str = r#"
bone.register_tool({
    name = "lua_cancel",
    description = "cancels a job id via ctx.agent.cancel and reports ok",
    safety = "read_only",
    parameters = {
        type = "object",
        properties = { id = { type = "string" } },
        required = { "id" },
    },
    execute = function(args, ctx)
        local res = ctx.agent.cancel(args.id)
        return "ok=" .. tostring(res and res.ok or false)
    end,
})
"#;

fn lua_cancel_call(marker: &str, id: &str) -> ToolCall {
    ToolCall {
        id: marker.into(),
        name: "lua_cancel".into(),
        arguments: serde_json::json!({ "id": id }),
    }
}

/// `ctx.agent.cancel(id)` on a running job sets its cancel flag (ok=true);
/// on a missing or already-finished job it is a no-op (ok=false). Driven
/// end-to-end through a registered Lua tool at depth 0.
#[test]
fn cancel_running_job_via_lua_tool() {
    let config_dir = common::temp_dir("subagent-cancel-lua");
    common::seed_catalog_into(&config_dir);
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("cancel.lua"), CANCEL_TOOL_LUA).unwrap();

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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // A running job whose cancel flag we can inspect afterwards.
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let registry = bone::ext::jobs::registry();
    let job_id = registry
        .create(bone::ext::jobs::NewJob {
            agent: "cancel-target".into(),
            task: "unique-task-cancel-via-lua".into(),
            title: String::new(),
            max_concurrency: 1,
            scope: None,
            cancel_flag: cancel_flag.clone(),
        })
        .expect("cancel-target job should be created");

    // 1. Cancel the running job through the Lua tool at depth 0.
    let results = rt
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(15),
                booted
                    .tools
                    .execute_all(vec![lua_cancel_call("call-cancel", &job_id)], 0),
            )
            .await
        })
        .expect("cancel tool timed out");
    assert_eq!(results.len(), 1);
    assert!(
        results[0].content.contains("ok=true"),
        "cancelling a running job should report ok=true; got: {}",
        results[0].content,
    );
    assert!(
        cancel_flag.load(std::sync::atomic::Ordering::Relaxed),
        "ctx.agent.cancel must set the job's cancel flag",
    );

    // 2. Cancelling a non-existent id is a no-op (ok=false).
    let miss = rt
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(15),
                booted
                    .tools
                    .execute_all(vec![lua_cancel_call("call-miss", "job-does-not-exist")], 0),
            )
            .await
        })
        .expect("miss tool timed out");
    assert!(
        miss[0].content.contains("ok=false"),
        "cancelling a missing id should report ok=false; got: {}",
        miss[0].content,
    );

    // 3. Cancelling a finished job is a no-op (ok=false).
    registry.complete(&job_id, Ok("done".into()));
    let after = rt
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(15),
                booted
                    .tools
                    .execute_all(vec![lua_cancel_call("call-after", &job_id)], 0),
            )
            .await
        })
        .expect("after tool timed out");
    assert!(
        after[0].content.contains("ok=false"),
        "cancelling a finished job should report ok=false; got: {}",
        after[0].content,
    );

    rt.shutdown_timeout(Duration::from_secs(1));
    std::fs::remove_dir_all(&config_dir).ok();
}
