use bone::tools::dynamic::{DynamicTool, InteractionType};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("bone-tools-{label}-{suffix}"));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn default_ask_user_parses_select_interaction() {
    let yaml = include_str!("../defaults/tools/ask_user.yaml");
    let tool: DynamicTool = serde_yaml::from_str(yaml).unwrap();

    assert_eq!(tool.name, "ask_user");
    assert!(matches!(tool.interaction, Some(InteractionType::Select)));
}

#[test]
fn script_tool_can_omit_args() {
    let tool: DynamicTool = serde_yaml::from_str(
        "name: status\ndescription: show status\nscript: |\n  git status --short\n",
    )
    .unwrap();

    assert!(tool.args.is_empty());
}

#[test]
fn interaction_tool_with_script_is_rejected_during_load() {
    let dir = temp_dir("hybrid");
    fs::write(
        dir.join("select_branch.yaml"),
        "name: select_branch\ndescription: pick branch\ninteraction: select\nscript: echo main\n",
    )
    .unwrap();

    assert!(bone::tools::dynamic::load_from_dir(&dir).is_empty());
    let _ = fs::remove_dir_all(dir);
}
