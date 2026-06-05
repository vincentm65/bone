use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use bone::skills::SkillStore;
use bone::skills::render_skill;
use bone::skills::types::Skill;
use bone::tools::script_runner::{ScriptRequest, run_script};

fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("bone-skills-{label}-{suffix}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn write_skill(dir: &std::path::Path, filename: &str, contents: &str) {
    fs::write(dir.join(filename), contents).unwrap();
}

#[test]
fn loads_valid_skills_and_reports_invalid_or_duplicate_entries() {
    let dir = temp_dir("load");
    write_skill(
        &dir,
        "a.yaml",
        "name: report\ndescription: first\nprompt: 'Report {{args}}'\n",
    );
    write_skill(
        &dir,
        "b.yaml",
        "name: report\ndescription: second\nprompt: duplicate\n",
    );
    write_skill(
        &dir,
        "c.yaml",
        "name: tools\ndescription: collision\nprompt: no\n",
    );

    let store = SkillStore::load_from_dir(&dir).unwrap();

    assert_eq!(store.list().count(), 1);
    assert!(store.get_enabled("report").is_some());
    assert_eq!(store.warnings().len(), 2);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn template_interpolation_is_single_pass() {
    let skill = Skill {
        name: "report".to_string(),
        description: "Report".to_string(),
        enabled: true,
        prompt: Some("Args: {{args}}\nOutput: {{script_output}}".to_string()),
        script: Some("true".to_string()),
        safety: None,
    };

    let rendered = render_skill(&skill, "{{script_output}}", Some("{{args}}")).unwrap();

    assert_eq!(rendered, "Args: {{script_output}}\nOutput: {{args}}");
}

#[tokio::test]
async fn script_runner_passes_skill_arguments_as_environment() {
    let output = run_script(ScriptRequest {
        command: "printf '%s' \"$BONE_ARGS\"".to_string(),
        env: vec![("BONE_ARGS".to_string(), "topic value".to_string())],
        timeout_ms: 1_000,
    })
    .await
    .unwrap();

    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "topic value");
}
