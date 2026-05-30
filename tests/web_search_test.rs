// Integration tests for the web_search dynamic tool.
// HTTP tests hit the real DuckDuckGo API via `uv run --with ddgs`, so they require
// network access and a working `uv` installation. Tagged with #[ignore] so
// `cargo test` doesn't fail offline; run with `cargo test --test web_search_test -- --ignored`.

use bone::tools::dynamic::{DynamicTool, load_from_dir};
use bone::tools::script_runner::{ScriptRequest, run_script};
use std::path::Path;

fn load_web_search_tool() -> DynamicTool {
    let defaults_dir = Path::new("defaults/tools");
    let tools = load_from_dir(defaults_dir);
    tools
        .into_iter()
        .find(|t| t.name == "web_search")
        .expect("web_search tool should exist in defaults/tools")
}

#[tokio::test]
#[ignore]
async fn web_search_returns_json_results_for_real_query() {
    let tool = load_web_search_tool();
    let script = tool.script.expect("web_search should have a script");

    let output = run_script(ScriptRequest {
        command: script,
        env: vec![
            (
                "TOOL_QUERY".to_string(),
                "rust programming language".to_string(),
            ),
            ("TOOL_NUM_RESULTS".to_string(), "3".to_string()),
        ],
        timeout_ms: 30_000,
    })
    .await
    .expect("web_search script should succeed");

    assert_eq!(output.exit_code, Some(0), "stderr: {}", output.stderr);
    assert!(!output.stdout.is_empty(), "should have output");

    // Each line should be valid JSON with title, href, body
    let lines: Vec<&str> = output.stdout.lines().collect();
    assert!(
        lines.len() >= 1,
        "should have at least 1 result, got: {:?}",
        output.stdout
    );

    for line in &lines {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line is not valid JSON: {line}\n{e}"));
        assert!(
            parsed["title"].is_string() || parsed["href"].is_string(),
            "result should have title or href: {line}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn web_search_respects_num_results() {
    let tool = load_web_search_tool();
    let script = tool.script.expect("web_search should have a script");

    let output = run_script(ScriptRequest {
        command: script,
        env: vec![
            ("TOOL_QUERY".to_string(), "tokio async runtime".to_string()),
            ("TOOL_NUM_RESULTS".to_string(), "2".to_string()),
        ],
        timeout_ms: 30_000,
    })
    .await
    .expect("web_search script should succeed");

    let lines: Vec<&str> = output.stdout.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        lines.len() <= 2,
        "expected at most 2 results, got {}",
        lines.len()
    );
}

#[tokio::test]
#[ignore]
async fn web_search_handles_empty_results_gracefully() {
    let tool = load_web_search_tool();
    let script = tool.script.expect("web_search should have a script");

    let output = run_script(ScriptRequest {
        command: script,
        env: vec![
            // Nonsensical query unlikely to return results
            (
                "TOOL_QUERY".to_string(),
                "zzzzzxxxxxqqqqqNoSuchThing12345".to_string(),
            ),
            ("TOOL_NUM_RESULTS".to_string(), "1".to_string()),
        ],
        timeout_ms: 30_000,
    })
    .await
    .expect("web_search script should succeed");

    // Should succeed (exit 0) even with no results
    assert_eq!(output.exit_code, Some(0), "stderr: {}", output.stderr);
}

#[test]
fn web_search_tool_yaml_loads_correctly() {
    let tool = load_web_search_tool();

    assert_eq!(tool.name, "web_search");
    assert!(tool.description.contains("DuckDuckGo"));
    assert!(tool.script.is_some());

    let query_arg = tool.args.iter().find(|a| a.name == "query").unwrap();
    assert!(query_arg.required);
    assert_eq!(query_arg.arg_type, "string");

    let num_arg = tool.args.iter().find(|a| a.name == "num_results").unwrap();
    assert!(!num_arg.required);
    assert_eq!(num_arg.arg_type, "integer");
}

#[test]
fn web_search_script_references_ddgs() {
    let tool = load_web_search_tool();
    let script = tool.script.unwrap();
    assert!(script.contains("ddgs"), "script should import ddgs library");
    assert!(script.contains("uv run"), "script should use uv run");
    assert!(
        script.contains("python3 -c"),
        "script should invoke python3 -c"
    );
}
