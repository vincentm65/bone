use super::*;

#[tokio::test]
async fn key_reply_registry_routes_reply_by_id() {
    let registry = KeyReplyRegistry::new();
    let (tx, rx) = oneshot::channel();
    let id = registry.register(KeyRequest { reply: tx });
    assert_eq!(registry.pending_count(), 1);

    // A reply for a wrong id does nothing.
    let key = KeyEvent {
        code: "Enter".into(),
        char: None,
        ctrl: false,
        alt: false,
        shift: false,
    };
    assert!(!registry.resolve(id.wrapping_add(99), key.clone()));
    assert_eq!(registry.pending_count(), 1);

    // The correct id delivers the value and clears the pending entry.
    assert!(registry.resolve(id, key.clone()));
    assert_eq!(registry.pending_count(), 0);

    let got = rx.await.expect("reply delivered");
    assert_eq!(got, key);
}

#[tokio::test]
async fn approval_preview_uses_daemon_session_working_dir() {
    let root = std::env::temp_dir().join(format!("bone-approval-preview-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("target.txt");
    std::fs::write(&path, "old\n").unwrap();

    let registry = ApprovalReplyRegistry::new();
    let (events, mut receiver) = mpsc::unbounded_channel();
    let gate = ChannelApprovalGate::new(events, registry.clone(), None, Some(root.clone()));
    let decision = tokio::spawn(async move {
        gate.decide(
            None,
            true,
            &ToolCall {
                id: "call-1".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({
                    "path": "target.txt",
                    "edits": [{ "at": format!("1#{}", crate::tools::edit_file::line_hash("old")), "text": "new" }]
                }),
            },
        )
        .await
    });

    let RuntimeEvent::ApprovalRequest { id, preview, .. } = receiver.recv().await.unwrap() else {
        panic!("expected approval request");
    };
    let preview = preview.expect("daemon preview");
    // Use the basename (stable across symlink-canonical temp roots) and a
    // fragment of the proposed diff body rather than the pre-canonical path.
    assert!(preview.contains("target.txt"), "{preview}");
    assert!(preview.contains("+ new"), "{preview}");
    assert!(registry.resolve(id, CallOutcome::Approve));
    assert_eq!(decision.await.unwrap(), CallOutcome::Approve);

    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn anchor_edit_approval_preview_uses_session_snapshots() {
    use crate::tools::edit_file::line_hash;
    use crate::tools::snapshot::Snapshots;

    let root = std::env::temp_dir().join(format!("bone-hashline-preview-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("target.txt");
    let original = "alpha\nbeta\ngamma\ndelta\nepsilon\n";
    std::fs::write(&path, original).unwrap();
    let canonical = std::fs::canonicalize(&path).unwrap();

    let snapshots: Snapshots = Default::default();
    {
        let mut store = snapshots.write().unwrap();
        store.set_hashline(true);
        store.record(&canonical.to_string_lossy(), original, None);
    }
    // The file shifts after the model read it; the anchor still names line 3.
    std::fs::write(&path, format!("// header\n{original}")).unwrap();

    let registry = ApprovalReplyRegistry::new();
    let (events, mut receiver) = mpsc::unbounded_channel();
    let gate = ChannelApprovalGate::new(events, registry.clone(), None, Some(root.clone()))
        .with_snapshots(snapshots);
    let anchor = format!("3#{}", line_hash("gamma"));
    let decision = tokio::spawn(async move {
        gate.decide(
            None,
            true,
            &ToolCall {
                id: "call-1".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({
                    "path": "target.txt",
                    "edits": [{"at": anchor, "text": "GAMMA"}]
                }),
            },
        )
        .await
    });

    let RuntimeEvent::ApprovalRequest { id, preview, .. } = receiver.recv().await.unwrap() else {
        panic!("expected approval request");
    };
    let preview = preview.expect("edit_file preview");
    assert!(!preview.starts_with("Cannot preview"), "{preview}");
    assert!(preview.contains("+ GAMMA"), "{preview}");
    assert!(preview.contains("- gamma"), "{preview}");
    assert!(registry.resolve(id, CallOutcome::Approve));
    assert_eq!(decision.await.unwrap(), CallOutcome::Approve);

    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn approval_preview_surfaces_edit_errors_without_changing_approval_policy() {
    let registry = ApprovalReplyRegistry::new();
    let (events, mut receiver) = mpsc::unbounded_channel();
    let gate = ChannelApprovalGate::new(events, registry.clone(), None, None);
    let decision = tokio::spawn(async move {
        gate.decide(
            Some("policy block".into()),
            false,
            &ToolCall {
                id: "bad-edit".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({"edits": []}),
            },
        )
        .await
    });
    let RuntimeEvent::ApprovalRequest {
        id,
        preview,
        blocked,
        auto_allows,
        ..
    } = receiver.recv().await.unwrap()
    else {
        panic!("expected approval request");
    };
    assert_eq!(
        preview.as_deref(),
        Some(
            "Cannot preview edit_file: edit_file is missing `path`; provide the file path with the edits"
        )
    );
    assert_eq!(blocked.as_deref(), Some("policy block"));
    assert!(!auto_allows);
    assert!(registry.resolve(id, CallOutcome::Denied));
    assert_eq!(decision.await.unwrap(), CallOutcome::Denied);
    assert_eq!(registry.pending_count(), 0);
}

#[tokio::test]
async fn approval_registry_cleans_up_when_frontend_is_gone() {
    let registry = ApprovalReplyRegistry::new();
    let (events, receiver) = mpsc::unbounded_channel();
    drop(receiver);
    let gate = ChannelApprovalGate::new(events, registry.clone(), None, None);

    let outcome = gate
        .decide(
            None,
            true,
            &ToolCall {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "missing"}),
            },
        )
        .await;

    assert_eq!(outcome, CallOutcome::Approve);
    assert_eq!(registry.pending_count(), 0);
}

#[tokio::test]
async fn cancelling_registries_unblocks_pending_replies() {
    let approvals = ApprovalReplyRegistry::new();
    let (approval_tx, approval_rx) = oneshot::channel();
    approvals.register(approval_tx);
    approvals.cancel_all();
    assert_eq!(approval_rx.await.unwrap(), CallOutcome::Denied);
    assert_eq!(approvals.pending_count(), 0);

    let keys = KeyReplyRegistry::new();
    let (key_tx, key_rx) = oneshot::channel();
    keys.register(KeyRequest { reply: key_tx });
    keys.cancel_all();
    assert!(key_rx.await.is_err());
    assert_eq!(keys.pending_count(), 0);
}
