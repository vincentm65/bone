use bone::tools::seed_default_tools;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("bone-tools-mod-{label}-{suffix}"));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn seed_default_tools_migrates_old_task_list_kill_pane() {
    let dir = temp_dir("task-list-migration");
    let path = dir.join("task_list.yaml");
    fs::write(
        &path,
        r#"name: task_list
description: old task list
script: |
  print(json.dumps({"content": "Task list killed.", "pane": None}))
"#,
    )
    .unwrap();

    seed_default_tools(&dir);

    let updated = fs::read_to_string(&path).unwrap();
    assert!(updated.contains(r#""source": "task_list""#));
    assert!(updated.contains(r#""lines": []"#));
    assert!(!updated.contains(r#""pane": None"#));

    let _ = fs::remove_dir_all(dir);
}
