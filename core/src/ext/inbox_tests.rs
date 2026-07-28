use super::*;

#[test]
fn push_then_drain_is_fifo_and_empties() {
    let inbox = SubmitInbox::default();
    inbox.push("first".into());
    inbox.push("second".into());
    assert_eq!(
        inbox.drain(),
        vec!["first".to_string(), "second".to_string()]
    );
    assert!(inbox.drain().is_empty(), "drain clears the inbox");
}

#[test]
fn push_is_bounded_and_drops_oldest() {
    let inbox = SubmitInbox::default();
    for i in 0..(MAX_INBOX + 5) {
        inbox.push(i.to_string());
    }
    let got = inbox.drain();
    assert_eq!(got.len(), MAX_INBOX);
    // Oldest five were dropped; the cap's first surviving entry follows.
    assert_eq!(got.first().map(String::as_str), Some("5"));
    assert_eq!(
        got.last().map(String::as_str),
        Some((MAX_INBOX + 4).to_string()).as_deref()
    );
}

#[test]
fn independent_inboxes_do_not_cross() {
    let first = SubmitInbox::default();
    let second = SubmitInbox::default();

    first.push("first-a".into());
    second.push("second-a".into());
    first.push("first-b".into());
    second.push("second-b".into());

    assert_eq!(second.drain(), vec!["second-a", "second-b"]);
    assert_eq!(first.drain(), vec!["first-a", "first-b"]);
}
