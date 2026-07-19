use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn publisher_does_not_keep_command_channel_open() {
    let (hub, mut commands_rx) = Hub::new();
    let publisher = hub.publisher();

    drop(hub);

    let received = tokio::time::timeout(std::time::Duration::from_secs(1), commands_rx.recv())
        .await
        .expect("command receiver stayed open");
    assert!(received.is_none());

    // The runtime-facing half remains usable without retaining a command
    // sender, even when there are no event subscribers.
    publisher.publish(RuntimeEvent::Status {
        message: "no listeners".into(),
    });
}

#[tokio::test]
async fn grouped_hubs_broadcast_global_events() {
    let group = HubGroup::default();
    let (hub_a, _commands_a) = Hub::new_grouped(group.clone());
    let (hub_b, _commands_b) = Hub::new_grouped(group);
    let mut events_a = hub_a.subscribe();
    let mut events_b = hub_b.subscribe();

    hub_a.publisher().publish_global(RuntimeEvent::Status {
        message: "global".into(),
    });

    assert!(
        matches!(events_a.recv().await.unwrap(), RuntimeEvent::Status { message } if message == "global")
    );
    assert!(
        matches!(events_b.recv().await.unwrap(), RuntimeEvent::Status { message } if message == "global")
    );
}

#[tokio::test]
async fn dropping_remote_client_closes_its_transport() {
    let (client_io, mut peer_io) = tokio::io::duplex(4096);
    let (read_half, write_half) = tokio::io::split(client_io);
    let client = RemoteClient::connect(read_half, write_half);

    drop(client);

    let mut byte = [0_u8; 1];
    let read = tokio::time::timeout(std::time::Duration::from_secs(1), peer_io.read(&mut byte))
        .await
        .expect("remote bridge kept the transport open")
        .unwrap();
    assert_eq!(read, 0, "peer should observe EOF after client drop");
}

#[tokio::test]
async fn hub_fans_out_events_and_merges_commands() {
    let (hub, mut commands_rx) = Hub::new();

    // Two clients connected by in-memory duplex pipes.
    let (client_a, server_a) = tokio::io::duplex(4096);
    let (client_b, server_b) = tokio::io::duplex(4096);
    tokio::spawn(serve_connection(server_a, hub.clone(), vec![]));
    tokio::spawn(serve_connection(
        server_b,
        hub.clone(),
        vec![RuntimeEvent::Status {
            message: "welcome".into(),
        }],
    ));

    // Give the writer tasks a moment to subscribe before broadcasting.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(hub.client_count(), 2);

    // Broadcast an event; both clients receive it.
    hub.publish(RuntimeEvent::Finished {
        content: "done".into(),
    });

    let mut ra = codec::MessageReader::new(tokio::io::split(client_a).0);
    let ev_a: RuntimeEvent = ra.read().await.unwrap().unwrap();
    assert!(matches!(ev_a, RuntimeEvent::Finished { content } if content == "done"));

    // Client B saw its initial welcome first, then the broadcast.
    let mut rb = codec::MessageReader::new(tokio::io::split(client_b).0);
    let ev_b0: RuntimeEvent = rb.read().await.unwrap().unwrap();
    assert!(matches!(ev_b0, RuntimeEvent::Status { message } if message == "welcome"));
    let ev_b1: RuntimeEvent = rb.read().await.unwrap().unwrap();
    assert!(matches!(ev_b1, RuntimeEvent::Finished { .. }));

    // A client writes a command; the hub surfaces it on the merged stream.
    let (client_c, server_c) = tokio::io::duplex(4096);
    tokio::spawn(serve_connection(server_c, hub.clone(), vec![]));
    let mut wc = tokio::io::split(client_c).1;
    codec::write_message(
        &mut wc,
        &RuntimeCommand::SubmitPrompt {
            text: "hi".into(),
            images: vec![],
        },
    )
    .await
    .unwrap();

    let cmd = commands_rx.recv().await.unwrap();
    assert!(matches!(cmd, RuntimeCommand::SubmitPrompt { text, .. } if text == "hi"));
}

#[tokio::test]
async fn malformed_frame_is_skipped_not_fatal() {
    let (hub, mut commands_rx) = Hub::new();
    let (client, server) = tokio::io::duplex(4096);
    tokio::spawn(serve_connection(server, hub.clone(), vec![]));

    let mut w = tokio::io::split(client).1;
    // Garbage line, then a valid command on the next line.
    w.write_all(b"{not valid json}\n").await.unwrap();
    codec::write_message(&mut w, &RuntimeCommand::Cancel)
        .await
        .unwrap();

    let cmd = commands_rx.recv().await.unwrap();
    assert!(matches!(cmd, RuntimeCommand::Cancel));
}

fn fake_managed_runtime(
    id: i64,
    active: Arc<std::sync::atomic::AtomicUsize>,
    max_active: Arc<std::sync::atomic::AtomicUsize>,
) -> ManagedRuntime {
    use std::sync::atomic::Ordering;

    let (hub, mut commands) = Hub::new();
    let publisher = hub.publisher();
    let initial = Arc::new(move || {
        let snapshot = bone_protocol::SessionSnapshot {
            conversation_id: Some(id),
            ..Default::default()
        };
        vec![RuntimeEvent::ConversationLoaded {
            messages: Vec::new(),
            snapshot,
        }]
    });
    let task = Box::pin(async move {
        while let Some(command) = commands.recv().await {
            if let RuntimeCommand::SubmitPrompt { text, .. } = command {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                publisher.publish(RuntimeEvent::Started {
                    approval: "safe".into(),
                    task: String::new(),
                    model: "test".into(),
                    display: None,
                });
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                publisher.publish(RuntimeEvent::Finished {
                    content: format!("session-{id}:{text}"),
                });
                publisher.publish(RuntimeEvent::TurnComplete);
                active.fetch_sub(1, Ordering::SeqCst);
            }
        }
    });
    ManagedRuntime {
        conversation_id: id,
        hub,
        initial,
        task,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn managed_connections_isolate_events_and_run_concurrently() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (manager, receiver) = SessionManager::new();
            let factory_active = active.clone();
            let factory_max = max_active.clone();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, move |target| {
                let id = match target {
                    SessionTarget::Latest => 1,
                    SessionTarget::New => 3,
                    SessionTarget::Conversation(id) => id,
                };
                Ok(fake_managed_runtime(
                    id,
                    factory_active.clone(),
                    factory_max.clone(),
                ))
            }));

            let (client_a, server_a) = tokio::io::duplex(4096);
            let (client_b, server_b) = tokio::io::duplex(4096);
            let serve_a = tokio::task::spawn_local(serve_managed_connection(
                server_a,
                manager.clone(),
                SessionTarget::Latest,
            ));
            let serve_b = tokio::task::spawn_local(serve_managed_connection(
                server_b,
                manager,
                SessionTarget::Latest,
            ));
            let (read_a, mut write_a) = tokio::io::split(client_a);
            let (read_b, mut write_b) = tokio::io::split(client_b);
            let mut read_a = codec::MessageReader::new(read_a);
            let mut read_b = codec::MessageReader::new(read_b);

            // Both initially attach to actor 1. Move only B to actor 2.
            let _: RuntimeEvent = read_a.read().await.unwrap().unwrap();
            let _: RuntimeEvent = read_b.read().await.unwrap().unwrap();
            codec::write_message(&mut write_b, &RuntimeCommand::LoadConversation { id: 2 })
                .await
                .unwrap();
            let switched: RuntimeEvent = read_b.read().await.unwrap().unwrap();
            assert!(matches!(
                switched,
                RuntimeEvent::ConversationLoaded { snapshot, .. }
                    if snapshot.conversation_id == Some(2)
            ));

            codec::write_message(
                &mut write_a,
                &RuntimeCommand::SubmitPrompt {
                    text: "alpha".into(),
                    images: vec![],
                },
            )
            .await
            .unwrap();
            codec::write_message(
                &mut write_b,
                &RuntimeCommand::SubmitPrompt {
                    text: "beta".into(),
                    images: vec![],
                },
            )
            .await
            .unwrap();

            async fn finished<R: AsyncRead + Unpin>(
                reader: &mut codec::MessageReader<R>,
            ) -> String {
                loop {
                    match reader.read::<RuntimeEvent>().await.unwrap().unwrap() {
                        RuntimeEvent::Finished { content } => return content,
                        _ => continue,
                    }
                }
            }
            let (a, b) = tokio::join!(finished(&mut read_a), finished(&mut read_b));
            assert_eq!(a, "session-1:alpha");
            assert_eq!(b, "session-2:beta");
            assert_eq!(
                max_active.load(std::sync::atomic::Ordering::SeqCst),
                2,
                "different conversation actors should overlap"
            );

            serve_a.abort();
            serve_b.abort();
            runner.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn managed_load_failure_is_correlated() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (manager, receiver) = SessionManager::new();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, move |target| {
                let id = match target {
                    SessionTarget::Conversation(404) => return Err("conversation not found".into()),
                    SessionTarget::Conversation(id) => id,
                    SessionTarget::Latest => 1,
                    SessionTarget::New => 2,
                };
                Ok(fake_managed_runtime(id, active.clone(), max_active.clone()))
            }));

            let (client, server) = tokio::io::duplex(4096);
            let serve = tokio::task::spawn_local(serve_managed_connection(
                server,
                manager,
                SessionTarget::Latest,
            ));
            let (read, mut write) = tokio::io::split(client);
            let mut read = codec::MessageReader::new(read);
            let _: RuntimeEvent = read.read().await.unwrap().unwrap();

            codec::write_message(&mut write, &RuntimeCommand::LoadConversation { id: 404 })
                .await
                .unwrap();
            let failed: RuntimeEvent =
                tokio::time::timeout(std::time::Duration::from_secs(1), read.read())
                    .await
                    .expect("load failure response timed out")
                    .unwrap()
                    .unwrap();
            assert!(matches!(
                failed,
                RuntimeEvent::ConversationLoadFailed { id: 404, message }
                    if message == "conversation not found"
            ));

            serve.abort();
            runner.abort();
        })
        .await;
}

#[tokio::test]
async fn socket_conn_skips_decode_errors_then_reads_next_event() {
    use crate::runtime::{RuntimeConn, SocketConn};

    let (read_side, mut peer) = tokio::io::duplex(4096);
    let mut conn = SocketConn::new(read_side, tokio::io::sink());
    peer.write_all(b"not json\n").await.unwrap();
    codec::write_message(
        &mut peer,
        &RuntimeEvent::Status {
            message: "healthy".into(),
        },
    )
    .await
    .unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_secs(1), conn.next_event())
        .await
        .expect("socket read timed out");
    assert!(matches!(event, Some(RuntimeEvent::Status { message }) if message == "healthy"));
}

#[tokio::test]
async fn socket_conn_terminates_on_oversized_frame() {
    use crate::runtime::{RuntimeConn, SocketConn};

    let input = std::io::Cursor::new(vec![b'x'; codec::MAX_LINE_BYTES + 1]);
    let mut conn = SocketConn::new(input, tokio::io::sink());

    let event = tokio::time::timeout(std::time::Duration::from_secs(5), conn.next_event())
        .await
        .expect("oversized frame caused a retry loop");
    assert!(event.is_none());
}

#[tokio::test]
async fn socket_conn_terminates_on_io_error() {
    use crate::runtime::{RuntimeConn, SocketConn};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    struct ErrorReader;
    impl AsyncRead for ErrorReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::other("read failed")))
        }
    }

    let mut conn = SocketConn::new(ErrorReader, tokio::io::sink());
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), conn.next_event())
        .await
        .expect("I/O error caused a retry loop");
    assert!(event.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn managed_actor_panic_does_not_stop_other_sessions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (manager, receiver) = SessionManager::new();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, move |target| {
                let id = match target {
                    SessionTarget::Conversation(id) => id,
                    _ => return Err("explicit conversation required".into()),
                };
                if id == 1 {
                    let (hub, _commands) = Hub::new();
                    Ok(ManagedRuntime {
                        conversation_id: id,
                        hub,
                        initial: Arc::new(Vec::new),
                        task: Box::pin(async { panic!("actor boom") }),
                    })
                } else {
                    Ok(fake_managed_runtime(id, active.clone(), max_active.clone()))
                }
            }));

            let mut failed = manager
                .attach(SessionTarget::Conversation(1))
                .await
                .unwrap();
            let panic_status =
                tokio::time::timeout(std::time::Duration::from_secs(1), failed.events.recv())
                    .await
                    .expect("panic status timed out")
                    .unwrap();
            assert!(matches!(
                panic_status,
                RuntimeEvent::Status { message } if message.contains("actor boom")
            ));

            let mut healthy = manager
                .attach(SessionTarget::Conversation(2))
                .await
                .expect("manager stopped after another actor panicked");
            healthy
                .commands
                .send(RuntimeCommand::SubmitPrompt {
                    text: "still alive".into(),
                    images: vec![],
                })
                .unwrap();
            let content = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    if let RuntimeEvent::Finished { content } = healthy.events.recv().await.unwrap()
                    {
                        break content;
                    }
                }
            })
            .await
            .expect("healthy actor did not respond");
            assert_eq!(content, "session-2:still alive");

            runner.abort();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn managed_event_channel_closure_writes_one_status_then_eof() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (manager, receiver) = SessionManager::new();
            let runner = tokio::task::spawn_local(run_session_manager(receiver, |_| {
                let (hub, _commands) = Hub::new();
                Ok(ManagedRuntime {
                    conversation_id: 1,
                    hub,
                    initial: Arc::new(Vec::new),
                    task: Box::pin(async {}),
                })
            }));
            let (client, server) = tokio::io::duplex(4096);
            let serve = tokio::task::spawn_local(serve_managed_connection(
                server,
                manager,
                SessionTarget::Latest,
            ));
            let mut reader = codec::MessageReader::new(client);

            let terminal = tokio::time::timeout(std::time::Duration::from_secs(1), reader.read())
                .await
                .expect("terminal status timed out")
                .unwrap()
                .unwrap();
            assert!(matches!(
                terminal,
                RuntimeEvent::Status { message } if message == "conversation runtime stopped"
            ));
            let eof = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                reader.read::<RuntimeEvent>(),
            )
            .await
            .expect("managed connection did not close after terminal status");
            assert!(eof.is_none());
            serve.await.unwrap().unwrap();

            runner.abort();
        })
        .await;
}
