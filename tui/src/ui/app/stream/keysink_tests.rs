use super::KeySink;
use crate::pane_content::KeyEvent;
use crate::runtime::RuntimeCommand;
use tokio::sync::mpsc::{self, UnboundedReceiver};

fn key(c: &str) -> KeyEvent {
    KeyEvent {
        code: c.to_string(),
        char: Some(c.to_string()),
        ctrl: false,
        alt: false,
        shift: false,
    }
}

/// Pull the `(id, key)` from the next `KeyReply` the sink emitted.
fn next_reply(rx: &mut UnboundedReceiver<RuntimeCommand>) -> (u64, KeyEvent) {
    match rx.try_recv().expect("a KeyReply command") {
        RuntimeCommand::KeyReply { id, key } => (id, key),
        other => panic!("expected KeyReply, got {other:?}"),
    }
}

#[test]
fn key_routes_to_armed_reply_slot() {
    let mut sink = KeySink::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeCommand>();
    sink.set_daemon(1, tx);
    assert!(sink.wants_key());
    assert!(sink.deliver(key("a")));
    assert_eq!(next_reply(&mut rx), (1, key("a")));
}

#[test]
fn owner_buffers_keys_between_requests() {
    // Between a tool's successive requests the slot is empty but the tool
    // still owns input, so keys buffer rather than leaking to chat.
    let mut sink = KeySink::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeCommand>();
    sink.set_daemon(1, tx);
    sink.deliver(key("a")); // resolves the slot, owns_input stays latched
    assert_eq!(next_reply(&mut rx), (1, key("a")));
    assert!(sink.wants_key()); // still owned
    assert!(sink.deliver(key("b"))); // buffered, consumed (not leaked)

    // Next request drains the buffered key instead of blocking.
    let (tx2, mut rx2) = mpsc::unbounded_channel::<RuntimeCommand>();
    sink.set_daemon(2, tx2);
    assert_eq!(next_reply(&mut rx2), (2, key("b")));
}

#[test]
fn clear_owner_releases_input_to_chat() {
    // After the owning tool finishes, keys must fall through to chat input
    // instead of staying latched/buffered for the rest of the turn.
    let mut sink = KeySink::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeCommand>();
    sink.set_daemon(1, tx);
    sink.deliver(key("a"));
    assert_eq!(next_reply(&mut rx), (1, key("a")));

    sink.clear_owner();
    assert!(!sink.wants_key());
    assert!(!sink.deliver(key("b"))); // falls through to chat
}

#[test]
fn clear_owner_drops_stale_buffer() {
    // Buffered keys belong to the finished tool and must not bleed into a
    // later tool's first key request.
    let mut sink = KeySink::new();
    let (tx, mut rx) = mpsc::unbounded_channel::<RuntimeCommand>();
    sink.set_daemon(1, tx);
    sink.deliver(key("a"));
    let _ = next_reply(&mut rx);
    sink.deliver(key("buffered")); // buffered for next request
    sink.clear_owner();

    let (tx2, mut rx2) = mpsc::unbounded_channel::<RuntimeCommand>();
    sink.set_daemon(2, tx2);
    sink.deliver(key("fresh"));
    assert_eq!(next_reply(&mut rx2), (2, key("fresh")));
}
