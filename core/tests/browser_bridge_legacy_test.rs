//! Regression coverage for the pre-consolidation browser observation tool.

mod common;

use bone_core::tools::types::ToolCall;

const LEGACY_OBSERVE_TOOL: &str = r#"
local function invoke_bridge(request)
    return cjson.encode(request)
end

local function call(params, _ctx)
    return invoke_bridge(params)
end

bone.tool.register({
    name = "browser_bridge_observe",
    description = "Legacy Firefox observation adapter",
    parameters = { type = "object", properties = {}, additionalProperties = false },
    safety = "read_only",
    execute = call,
})
"#;

#[test]
fn legacy_observe_injects_action_before_invoking_bridge() {
    let config_dir = common::temp_dir("legacy-browser-observe");
    let tools_dir = config_dir.join("lua/tools");
    std::fs::create_dir_all(&tools_dir).unwrap();
    std::fs::write(tools_dir.join("browser_bridge.lua"), LEGACY_OBSERVE_TOOL).unwrap();

    let mut custom = bone_core::config::custom::CustomConfigs::default();
    let booted = bone_core::ext::boot_with_tools(
        &config_dir,
        &config_dir,
        &mut custom,
        false,
        bone_core::ext::BootOptions::default(),
        "test-model",
        "test-provider",
    );
    let call = ToolCall {
        id: "call-1".into(),
        name: "browser_bridge_observe".into(),
        arguments: serde_json::json!({}),
    };

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let results = runtime.block_on(booted.tools.execute_all(vec![call], 0));

    assert_eq!(results.len(), 1);
    assert!(!results[0].is_error, "{}", results[0].content);
    let request: serde_json::Value = serde_json::from_str(&results[0].content).unwrap();
    assert_eq!(request["action"], "observe");

    std::fs::remove_dir_all(config_dir).ok();
}
