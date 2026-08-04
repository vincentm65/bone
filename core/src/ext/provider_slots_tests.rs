use super::*;

fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[tokio::test]
async fn caps_one_provider_and_releases_on_drop() {
    let root = temp_dir();
    let first = acquire_in(root.path(), "local", Some(1), None)
        .await
        .unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let waiting = tokio::spawn({
        let path = root.path().to_path_buf();
        let cancelled = cancelled.clone();
        async move { acquire_in(&path, "local", Some(1), Some(&cancelled)).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(!waiting.is_finished());
    drop(first);
    assert!(waiting.await.unwrap().is_ok());
}

#[tokio::test]
async fn missing_limit_is_unlimited() {
    let root = temp_dir();
    let _first = acquire_in(root.path(), "local", None, None).await.unwrap();
    assert!(acquire_in(root.path(), "local", None, None).await.is_ok());
    assert!(!root.path().join(provider_key("local")).exists());
}

#[tokio::test]
async fn different_providers_have_independent_slots() {
    let root = temp_dir();
    let _first = acquire_in(root.path(), "local-a", Some(1), None)
        .await
        .unwrap();
    assert!(
        acquire_in(root.path(), "local-b", Some(1), None)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn waiting_is_cancellable() {
    let root = temp_dir();
    let _first = acquire_in(root.path(), "local", Some(1), None)
        .await
        .unwrap();
    let cancelled = Arc::new(AtomicBool::new(true));
    let error = acquire_in(root.path(), "local", Some(1), Some(&cancelled))
        .await
        .err()
        .unwrap();
    assert!(error.contains("cancelled"));
}
