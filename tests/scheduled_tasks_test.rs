use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use bone::cron::{CronJob, build_cron_line, parse_cron_line, sh_escape};
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

#[tokio::test]
async fn expands_prompt_only_skill() {
    let dir = temp_dir("skill-expand");
    fs::write(
        dir.join("clean.yaml"),
        "name: clean\ndescription: Clean file\nprompt: 'Clean {{args}}'\nenabled: true\n",
    )
    .unwrap();
    let store = SkillStore::load_from_dir(&dir, false).unwrap();
    let rendered = expand_skill_command(&store, "/clean src/main.rs", false, ApprovalMode::Safe)
        .await
        .unwrap();
    assert_eq!(rendered, "Clean src/main.rs");
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn rejects_scripted_skill_without_flag() {
    let dir = temp_dir("skill-script-reject");
    fs::write(
        dir.join("scripted.yaml"),
        "name: scripted\ndescription: Scripted\nscript: 'printf hi'\nsafety: read_only\nprompt: 'Output {{script_output}}'\nenabled: true\n",
    )
    .unwrap();
    let store = SkillStore::load_from_dir(&dir, false).unwrap();
    let err = expand_skill_command(&store, "/scripted", false, ApprovalMode::Danger)
        .await
        .unwrap_err();
    assert!(err.contains("--allow-skill-scripts"));
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn expands_scripted_skill_with_flag() {
    let dir = temp_dir("skill-script-expand");
    fs::write(
        dir.join("scripted.yaml"),
        r#"name: scripted
description: Scripted
script: 'printf "arg=%s" "$BONE_ARGS"'
safety: read_only
prompt: 'Output {{script_output}}'
enabled: true
"#,
    )
    .unwrap();
    let store = SkillStore::load_from_dir(&dir, false).unwrap();
    let rendered = expand_skill_command(&store, "/scripted abc", true, ApprovalMode::Danger)
        .await
        .unwrap();
    assert_eq!(rendered, "Output arg=abc");
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn rejects_scripted_skill_when_approval_too_low() {
    let dir = temp_dir("skill-script-approval");
    fs::write(
        dir.join("scripted.yaml"),
        "name: scripted\ndescription: Scripted\nscript: 'printf hi'\nprompt: 'Output {{script_output}}'\nenabled: true\n",
    )
    .unwrap();
    let store = SkillStore::load_from_dir(&dir, false).unwrap();
    let err = expand_skill_command(&store, "/scripted", true, ApprovalMode::Safe)
        .await
        .unwrap_err();
    assert!(err.contains("requires Danger approval"));
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn scripted_skill_declared_read_only_still_requires_danger() {
    let dir = temp_dir("skill-script-readonly");
    fs::write(
        dir.join("scripted.yaml"),
        r#"name: scripted
description: Scripted
script: 'printf ok'
safety: read_only
prompt: 'Output {{script_output}}'
enabled: true
"#,
    )
    .unwrap();
    let store = SkillStore::load_from_dir(&dir, false).unwrap();
    assert_eq!(
        store.get_enabled("scripted").unwrap().effective_safety(),
        CommandSafety::Danger
    );
    let err = expand_skill_command(&store, "/scripted", true, ApprovalMode::Safe)
        .await
        .unwrap_err();
    assert!(err.contains("requires Danger approval"));
    let rendered = expand_skill_command(&store, "/scripted", true, ApprovalMode::Danger)
        .await
        .unwrap();
    assert_eq!(rendered, "Output ok");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rejects_empty_script_field() {
    let dir = temp_dir("skill-empty-script");
    fs::write(
        dir.join("scripted.yaml"),
        "name: scripted\ndescription: Scripted\nprompt: 'Output {{script_output}}'\nscript: '   '\nenabled: true\n",
    )
    .unwrap();
    let store = SkillStore::load_from_dir(&dir, false).unwrap();
    assert!(store.get_enabled("scripted").is_none());
    assert!(
        store
            .warnings()
            .iter()
            .any(|warning| warning.contains("empty script"))
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shell_escape_quotes() {
    assert_eq!(sh_escape("a'b"), "'a'\\''b'");
}

#[test]
fn cron_line_round_trip_with_delimiter_like_prompt() {
    let job = CronJob {
        name: "daily-clean".to_string(),
        minute: 5,
        hour: 9,
        approval: ApprovalMode::Edits,
        cwd: PathBuf::from("/tmp/a' && echo wrong"),
        prompt: "/clean tricky ' >> /tmp/evil and don't break it".to_string(),
        log_path: PathBuf::from("/tmp/daily-clean.log"),
        allow_skill_scripts: true,
    };
    let line = build_cron_line(&job).unwrap();
    assert!(line.contains("--allow-skill-scripts"));
    let parsed = parse_cron_line(&line).unwrap();
    assert_eq!(parsed, job);
}

#[test]
fn cron_line_round_trip() {
    let job = CronJob {
        name: "daily-clean".to_string(),
        minute: 5,
        hour: 9,
        approval: ApprovalMode::Edits,
        cwd: PathBuf::from("/tmp/a'b"),
        prompt: "/clean src/main.rs and don't break it".to_string(),
        log_path: PathBuf::from("/tmp/daily-clean.log"),
        allow_skill_scripts: false,
    };
    let line = build_cron_line(&job).unwrap();
    let parsed = parse_cron_line(&line).unwrap();
    assert_eq!(parsed.name, job.name);
    assert_eq!(parsed.minute, job.minute);
    assert_eq!(parsed.hour, job.hour);
    assert_eq!(parsed.approval, job.approval);
    assert_eq!(parsed.cwd, job.cwd);
    assert_eq!(parsed.prompt, job.prompt);
    assert_eq!(parsed.log_path, job.log_path);
    assert_eq!(parsed.allow_skill_scripts, job.allow_skill_scripts);
}
