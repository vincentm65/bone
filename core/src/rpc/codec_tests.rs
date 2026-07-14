use super::*;
use crate::runtime::{RuntimeCommand, RuntimeEvent};

#[tokio::test]
async fn round_trips_event_over_duplex() {
    // Use whole duplex ends: dropping `b` fully signals EOF to `a`'s
    // reader. (A `split` write-half drop alone never closes the stream.)
    let (a, mut b) = tokio::io::duplex(1024);

    write_message(&mut b, &RuntimeEvent::TextDelta { text: "hi".into() })
        .await
        .unwrap();
    drop(b);

    let mut reader = MessageReader::new(a);
    let ev: RuntimeEvent = reader.read().await.unwrap().unwrap();
    assert!(matches!(ev, RuntimeEvent::TextDelta { text } if text == "hi"));
    assert!(
        reader.read::<RuntimeEvent>().await.is_none(),
        "EOF after one"
    );
}

#[tokio::test]
async fn decode_error_is_recoverable() {
    let (a, mut b) = tokio::io::duplex(1024);

    b.write_all(b"garbage\n").await.unwrap();
    write_message(&mut b, &RuntimeCommand::Cancel)
        .await
        .unwrap();
    drop(b);

    let mut reader = MessageReader::new(a);
    assert!(matches!(
        reader.read::<RuntimeCommand>().await,
        Some(Err(ReadError::Decode(_)))
    ));
    assert!(matches!(
        reader.read::<RuntimeCommand>().await,
        Some(Ok(RuntimeCommand::Cancel))
    ));
}
