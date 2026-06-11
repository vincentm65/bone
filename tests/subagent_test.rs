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

// ── 1. Registration + dynamic description ───────────────────────────────────

/// Two sub-agents registered in init.lua.
const TWO_AGENTS_INIT: &str = r#"
bone.register_subagent({
    name = "researcher",
    description = "Searches the web and summarizes findings",
    system_prompt = "You are a researcher.",
})

bone.register_subagent({
    name = "coder",
    description = "Writes and fixes code",
    system_prompt = "You are a coder.",
})
"#;

#[test]
fn two_agents_registered_and_listed_in_tool() {
    let config_dir = common::temp_dir("subagent-two-agents");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("init.lua"), TWO_AGENTS_INIT).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, false);

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

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, false);

    let defs = booted.tools.definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(
        !names.contains(&"subagent"),
        "subagent tool should NOT be registered when no agents exist; got: {names:?}",
    );

    std::fs::remove_dir_all(&config_dir).ok();
}

// ── 2. Spawn lifecycle (no provider → Error) ────────────────────────────────

#[test]
fn spawn_lifecycle_no_provider() {
    let config_dir = common::temp_dir("subagent-lifecycle");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("init.lua"), TWO_AGENTS_INIT).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, false);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // Dispatch a task via the subagent tool.
    let call = ToolCall {
        id: "call-lifecycle".into(),
        name: "subagent".into(),
        arguments: serde_json::json!({
            "action": "dispatch",
            "tasks": [{ "agent": "researcher", "task": "Find information about Rust" }],
        }),
    };

    let results = rt.block_on(async {
        tokio::time::timeout(
            Duration::from_secs(15),
            booted.tools.execute_all(vec![call], 0),
        )
        .await
    });
    rt.shutdown_timeout(Duration::from_secs(1));

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
                if job["status"].as_str() == Some("error") {
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

    // take_finished_unconsumed should return at least this job.
    // (Registry is process-global, so other unconsumed jobs may exist from prior tests.)
    let taken = registry.take_finished_unconsumed();
    let my_job = taken.iter().find(|j| j.id == id);
    assert!(
        my_job.is_some(),
        "take_finished_unconsumed should include job-{}; got jobs: {:?}",
        id,
        taken,
    );
    assert_eq!(my_job.unwrap().status, bone::ext::jobs::JobStatus::Error);
    assert!(!my_job.unwrap().result.as_ref().unwrap().is_empty());

    // Second take: no more unconsumed jobs (all taken above).
    let taken2 = registry.take_finished_unconsumed();
    assert!(taken2.is_empty());

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
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("depth_guard.lua"), SPAWN_AT_DEPTH).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, false);

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
fn render_subagent_pane_returns_valid_panepage() {
    let config_dir = common::temp_dir("subagent-render");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("init.lua"), TWO_AGENTS_INIT).unwrap();

    let mut custom = bone::config::custom::CustomConfigs::default();
    let booted = bone::ext::boot_with_tools(&config_dir, &config_dir, &mut custom, false);

    // Create some fake jobs in the registry.
    let registry = bone::ext::jobs::registry();
    let id1 = registry.create("researcher".into(), "search query".into());
    let id2 = registry.create("coder".into(), "fix bug in module".into());
    registry.complete(&id1, Ok("found 3 relevant papers".into()));
    registry.complete(&id2, Err("timeout".into()));

    let snap = registry.snapshot();

    // Call render_subagent_pane through the extension manager.
    let pane = booted.manager.render_subagent_pane(&snap);
    assert!(
        pane.is_some(),
        "render_subagent_pane should return Some; got None",
    );

    let pane = pane.unwrap();

    // Convert to PanePage to verify it's valid.
    let page = bone::ui::pane_page::PanePage::from_json(&pane);
    assert!(
        page.is_ok(),
        "rendered pane should parse as PanePage; got error: {}",
        page.unwrap_err(),
    );

    let page = page.unwrap();
    assert_eq!(page.source, "subagents");
    assert!(page.title.contains("Agents"));
    assert_eq!(page.content.len(), 2); // one line per agent

    std::fs::remove_dir_all(&config_dir).ok();
}
