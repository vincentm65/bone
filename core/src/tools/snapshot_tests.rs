use super::*;

fn store_with(path: &str, text: &str) -> (SnapshotStore, String) {
    let mut store = SnapshotStore::default();
    let tag = store.record(path, text, None);
    (store, tag)
}

#[test]
fn tag_is_4_hex_uppercase_and_stable() {
    let (_, tag) = store_with("a.txt", "hello\n");
    assert_eq!(tag.len(), 4);
    assert!(
        tag.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    );
    let (_, tag2) = store_with("a.txt", "hello\n");
    assert_eq!(tag, tag2);
}

#[test]
fn different_content_yields_different_tag() {
    let (_, a) = store_with("a.txt", "hello\n");
    let (_, b) = store_with("a.txt", "world\n");
    assert_ne!(a, b);
}

#[test]
fn head_returns_latest_snapshot() {
    let (mut store, tag) = store_with("a.txt", "alpha\n");
    assert_eq!(store.head("a.txt").unwrap().tag, tag);
    assert!(store.head("missing.txt").is_none());

    store.record("a.txt", "beta\n", None);
    assert_eq!(store.head("a.txt").unwrap().text, "beta\n");
}

#[test]
fn repeated_read_merges_seen_lines() {
    let mut store = SnapshotStore::default();
    store.record("a.txt", "value\n", Some(&[1]));
    store.record("a.txt", "value\n", Some(&[2]));
    assert_eq!(
        store
            .head("a.txt")
            .unwrap()
            .seen_lines
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn normalize_strips_bom_and_crlf() {
    assert_eq!(normalize_text("\u{feff}a\r\nb\rc"), "a\nb\nc");
    assert_eq!(normalize_text("plain\n"), "plain\n");
}

#[test]
fn compute_tag_matches_across_line_endings() {
    // CRLF and LF of the same logical content must hash identically.
    assert_eq!(compute_tag(&normalize_text("a\r\nb")), compute_tag("a\nb"));
}

#[test]
fn snapshot_keeps_full_digest_separate_from_display_tag() {
    let mut store = SnapshotStore::default();
    let first = "version = 18\n";
    let second = "version = 93\n";
    assert_eq!(compute_tag(first), compute_tag(second));
    assert_ne!(compute_digest(first), compute_digest(second));

    store.record("a.txt", first, Some(&[1]));
    assert_eq!(store.head("a.txt").unwrap().digest, compute_digest(first));
}

#[test]
fn config_dir_paths_anchor_to_the_resolved_config_directory() {
    let _guard = crate::util::test_env_lock();
    let bone = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let old_bone_dir = std::env::var_os("BONE_DIR");

    let result = std::panic::catch_unwind(|| {
        // SAFETY: held under test_env_lock; restored below.
        unsafe { std::env::set_var("BONE_DIR", bone.path()) };
        assert_eq!(
            resolve_path(".bone-rust/AGENTS.md", Some(project.path())).unwrap(),
            bone.path().join("AGENTS.md")
        );
        assert_eq!(
            resolve_path("./.bone-rust/AGENTS.md", Some(project.path())).unwrap(),
            bone.path().join("AGENTS.md")
        );
        assert_eq!(
            resolve_path(".bone-rust", Some(project.path())).unwrap(),
            bone.path()
        );
    });

    match old_bone_dir {
        Some(value) => unsafe { std::env::set_var("BONE_DIR", value) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn project_local_config_dir_wins_over_the_config_redirect() {
    let _guard = crate::util::test_env_lock();
    let bone = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let local = project.path().join(".bone-rust");
    std::fs::create_dir(&local).unwrap();
    let old_bone_dir = std::env::var_os("BONE_DIR");

    let result = std::panic::catch_unwind(|| {
        // SAFETY: held under test_env_lock; restored below.
        unsafe { std::env::set_var("BONE_DIR", bone.path()) };
        assert_eq!(
            resolve_path(".bone-rust/AGENTS.md", Some(project.path())).unwrap(),
            local.join("AGENTS.md")
        );
    });

    match old_bone_dir {
        Some(value) => unsafe { std::env::set_var("BONE_DIR", value) },
        None => unsafe { std::env::remove_var("BONE_DIR") },
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[test]
fn other_relative_paths_stay_in_the_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_path("core/src/lib.rs", Some(dir.path())).unwrap(),
        dir.path().join("core/src/lib.rs")
    );
    assert_eq!(
        resolve_path("../outside.txt", Some(dir.path())).unwrap(),
        dir.path().join("../outside.txt")
    );
    assert_eq!(
        resolve_path("/etc/hosts", Some(dir.path())).unwrap(),
        PathBuf::from("/etc/hosts")
    );
    assert!(resolve_path("   ", Some(dir.path())).is_err());
}
