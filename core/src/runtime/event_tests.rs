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
