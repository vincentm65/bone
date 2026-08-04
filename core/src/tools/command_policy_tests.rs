use super::{CommandSafety, classify_command};

#[test]
fn classification_observes_policy_in_current_bone_dir() {
    let _guard = crate::util::test_env_lock();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(
        first.path().join("command-policy.yaml"),
        "read_only: [first-only]\n",
    )
    .unwrap();
    std::fs::write(
        second.path().join("command-policy.yaml"),
        "read_only: [second-only]\n",
    )
    .unwrap();
    let old_bone_dir = std::env::var_os("BONE_DIR");

    let result = std::panic::catch_unwind(|| {
        // SAFETY: held under test_env_lock; restored below.
        unsafe { std::env::set_var("BONE_DIR", first.path()) };
        assert_eq!(classify_command("first-only"), CommandSafety::ReadOnly);
        assert_eq!(classify_command("second-only"), CommandSafety::Danger);

        unsafe { std::env::set_var("BONE_DIR", second.path()) };
        assert_eq!(classify_command("first-only"), CommandSafety::Danger);
        assert_eq!(classify_command("second-only"), CommandSafety::ReadOnly);
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
fn malformed_policy_falls_back_to_danger() {
    let _guard = crate::util::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("command-policy.yaml"), "read_only: [").unwrap();
    let old_bone_dir = std::env::var_os("BONE_DIR");

    let result = std::panic::catch_unwind(|| {
        // SAFETY: held under test_env_lock; restored below.
        unsafe { std::env::set_var("BONE_DIR", dir.path()) };
        assert_eq!(
            classify_command("custom-read-command"),
            CommandSafety::Danger
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
