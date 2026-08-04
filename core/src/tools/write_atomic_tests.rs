use super::*;

fn test_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bone-{name}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

#[test]
fn synchronous_write_replaces_file_with_exact_bytes() {
    let path = test_path("atomic-sync-test");
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, b"old").unwrap();

    write_atomic_sync(&path, &[0, 1, 2, 255], None).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), [0, 1, 2, 255]);

    let _ = std::fs::remove_file(&path);
}

#[cfg(unix)]
#[test]
fn synchronous_write_applies_requested_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let path = test_path("atomic-sync-permissions-test");
    let _ = std::fs::remove_file(&path);
    let permissions = std::fs::Permissions::from_mode(0o640);

    write_atomic_sync(&path, b"content", Some(permissions)).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );

    let _ = std::fs::remove_file(&path);
}
