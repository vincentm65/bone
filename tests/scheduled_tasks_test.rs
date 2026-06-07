use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use bone::skills::{SkillStore, expand_skill_command};
use bone::tools::ApprovalMode;
use bone::tools::command_policy::CommandSafety;

fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("bone-{label}-{suffix}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn load_skills(dir: &PathBuf) -> SkillStore {
    let skills_dir = dir.join("bone-rust").join("skills");
    fs::create_dir_all(&skills_dir).unwrap();
    let prev_xdg = env::var("XDG_CONFIG_HOME").ok();
    unsafe { env::set_var("XDG_CONFIG_HOME", dir); }
    let store = SkillStore::load().unwrap();
    match prev_xdg {
        Some(v) => unsafe { env::set_var("XDG_CONFIG_HOME", v); },
        None => unsafe { env::remove_var("XDG_CONFIG_HOME"); },
    }
    store
}

#[tokio::test]
async fn expands_prompt_only_skill() {
    let dir = temp_dir("skill-expand");
    let skills_dir = dir.join("bone-rust").join("skills");
    fs::create_dir_all(&skills_dir).unwrap();
    fs::write(
        skills_dir.join("clean.yaml"),
        "name: clean\ndescription: Clean file\nprompt: 'Clean {{args}}'\nenabled: true\n",
    )
    .unwrap();
    let store = load_skills(&dir);
    let rendered = expand_skill_command(&store, "/clean src/main.rs", false, ApprovalMode::Safe)
        .await
        .unwrap();
    assert_eq!(rendered, "Clean src/main.rs");
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn rejects_scripted_skill_without_flag() {
    let dir = temp_dir("skill-script-reject");
    let skills_dir = dir.join("bone-rust").join("skills");
    fs::create_dir_all(&skills_dir).unwrap();
    fs::write(
        skills_dir.join("scripted.yaml"),
        "name: scripted\ndescription: Scripted\nscript: 'printf hi'\nsafety: read_only\nprompt: 'Output {{script_output}}'\nenabled: true\n",
    )
    .unwrap();
    let store = load_skills(&dir);
    let err = expand_skill_command(&store, "/scripted", false, ApprovalMode::Danger)
        .await
        .unwrap_err();
    assert!(err.contains("--allow-skill-scripts"));
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn expands_scripted_skill_with_flag() {
    let dir = temp_dir("skill-script-expand");
    let skills_dir = dir.join("bone-rust").join("skills");
    fs::create_dir_all(&skills_dir).unwrap();
    fs::write(
        skills_dir.join("scripted.yaml"),
        r#"name: scripted
description: Scripted
script: 'printf "arg=%s" "$BONE_ARGS"'
safety: read_only
prompt: 'Output {{script_output}}'
enabled: true
"#,
    )
    .unwrap();
    let store = load_skills(&dir);
    let rendered = expand_skill_command(&store, "/scripted abc", true, ApprovalMode::Danger)
        .await
        .unwrap();
    assert_eq!(rendered, "Output arg=abc");
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn rejects_scripted_skill_when_approval_too_low() {
    let dir = temp_dir("skill-script-approval");
    let skills_dir = dir.join("bone-rust").join("skills");
    fs::create_dir_all(&skills_dir).unwrap();
    fs::write(
        skills_dir.join("scripted.yaml"),
        "name: scripted\ndescription: Scripted\nscript: 'printf hi'\nprompt: 'Output {{script_output}}'\nenabled: true\n",
    )
    .unwrap();
    let store = load_skills(&dir);
    let err = expand_skill_command(&store, "/scripted", true, ApprovalMode::Safe)
        .await
        .unwrap_err();
    assert!(err.contains("requires Danger approval"));
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn scripted_skill_declared_read_only_uses_declared_safety() {
    let dir = temp_dir("skill-script-readonly");
    let skills_dir = dir.join("bone-rust").join("skills");
    fs::create_dir_all(&skills_dir).unwrap();
    fs::write(
        skills_dir.join("scripted.yaml"),
        r#"name: scripted
description: Scripted
script: 'printf ok'
safety: read_only
prompt: 'Output {{script_output}}'
enabled: true
"#,
    )
    .unwrap();
    let store = load_skills(&dir);
    assert_eq!(
        store.get_enabled("scripted").unwrap().effective_safety(),
        CommandSafety::ReadOnly
    );
    let rendered = expand_skill_command(&store, "/scripted", true, ApprovalMode::Safe)
        .await
        .unwrap();
    assert_eq!(rendered, "Output ok");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rejects_empty_script_field() {
    let dir = temp_dir("skill-empty-script");
    let skills_dir = dir.join("bone-rust").join("skills");
    fs::create_dir_all(&skills_dir).unwrap();
    fs::write(
        skills_dir.join("scripted.yaml"),
        "name: scripted\ndescription: Scripted\nprompt: 'Output {{script_output}}'\nscript: '   '\nenabled: true\n",
    )
    .unwrap();
    let store = load_skills(&dir);
    assert!(store.get_enabled("scripted").is_none());
    assert!(
        store
            .warnings()
            .iter()
            .any(|warning| warning.contains("empty script"))
    );
    fs::remove_dir_all(dir).unwrap();
}
