use super::*;

// The inbox is a process-global singleton, so tests touching it must run
// one at a time — otherwise their pushes/drains interleave.
static TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn push_then_drain_is_fifo_and_empties() {
    let _guard = TEST_LOCK.lock().unwrap();
    push("first".into());
    push("second".into());
    assert_eq!(drain(), vec!["first".to_string(), "second".to_string()]);
    assert!(drain().is_empty(), "drain clears the inbox");
}

#[test]
fn push_is_bounded_and_drops_oldest() {
    let _guard = TEST_LOCK.lock().unwrap();
    for i in 0..(MAX_INBOX + 5) {
        push(i.to_string());
    }
    let got = drain();
    assert_eq!(got.len(), MAX_INBOX);
    // Oldest five were dropped; the cap's first surviving entry follows.
    assert_eq!(got.first().map(String::as_str), Some("5"));
    assert_eq!(
        got.last().map(String::as_str),
        Some((MAX_INBOX + 4).to_string()).as_deref()
    );
}
