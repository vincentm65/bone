//! End-to-end: client <-> in-process transport <-> server <-> core.

use std::sync::Arc;
use std::time::Duration;

use bone_client::{Client, ClientError};
use bone_core::Core;
use bone_core::config::{CoreConfig, ProviderConfig};
use bone_proto::methods::{Echo, EchoParams, Initialize, InitializeParams};
use bone_proto::{PROTOCOL_VERSION, RpcError};
use bone_server::Server;
use tokio::time::timeout;

fn server() -> Server {
    server_at("http://127.0.0.1:9")
}

fn server_at(base_url: &str) -> Server {
    // Leaked so the directory outlives the server task.
    let data = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    Server::new(Arc::new(Core::new(CoreConfig {
        provider: ProviderConfig {
            base_url: base_url.into(),
            model: "m".into(),
            ..Default::default()
        },
        system_prompt: None,
        data_dir: data.path().to_owned(),
        parallel_tools: true,
        compact: Default::default(),
    })))
}

fn connect() -> (Client, bone_client::EventStream) {
    let server = server();
    Client::new(server.connect_in_process())
}

fn rpc_code(err: ClientError) -> i64 {
    match err {
        ClientError::Rpc(e) => e.code,
        other => panic!("expected rpc error, got {other:?}"),
    }
}

#[tokio::test]
async fn echo_round_trip_with_event() {
    let (client, _events) = connect();

    let init = client.initialize("test").await.unwrap();
    assert_eq!(init.protocol_version, PROTOCOL_VERSION);
    assert_eq!(init.server_name, "bone");

    let text = "hello bone3".to_owned();
    let reply = client
        .request::<Echo>(EchoParams { text: text.clone() })
        .await
        .unwrap();
    assert_eq!(reply.text, text);
}

#[tokio::test]
async fn handshake_errors() {
    // 1. Any request before initialize is rejected.
    let (client, _events) = connect();
    let err = client
        .request::<Echo>(EchoParams { text: "x".into() })
        .await
        .unwrap_err();
    assert_eq!(rpc_code(err), RpcError::NOT_INITIALIZED);

    // 2. A version mismatch is rejected, but the connection stays usable.
    let (client, _events) = connect();
    let err = client
        .request::<Initialize>(InitializeParams {
            protocol_version: PROTOCOL_VERSION + 1,
            client_name: "future".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_code(err), RpcError::VERSION_MISMATCH);
    client.initialize("test").await.unwrap();

    // 3. Initializing twice on the same connection is rejected.
    let err = client.initialize("test").await.unwrap_err();
    assert_eq!(rpc_code(err), RpcError::INVALID_REQUEST);
}

#[tokio::test]
async fn shutdown_closes_connection() {
    let (client, mut events) = connect();
    client.initialize("test").await.unwrap();
    client.shutdown().await.unwrap();

    assert!(
        timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .is_none()
    );
    let err = client
        .request::<Echo>(EchoParams { text: "x".into() })
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Disconnected), "{err:?}");
}

/// Every method in the protocol list reaches a handler: none is
/// "method not found", even if the empty params are rejected.
#[tokio::test]
async fn every_protocol_method_is_handled() {
    let (client, _events) = connect();
    client.initialize("test").await.unwrap();
    for method in bone_proto::methods::METHODS {
        if matches!(*method, "initialize" | "shutdown") {
            continue;
        }
        if let Err(ClientError::Rpc(e)) = client.request_raw(method, serde_json::json!({})).await {
            assert_ne!(
                e.code,
                RpcError::METHOD_NOT_FOUND,
                "{method} has no handler"
            );
        }
    }
    let err = client
        .request_raw("nope", serde_json::json!({}))
        .await
        .unwrap_err();
    assert_eq!(rpc_code(err), RpcError::METHOD_NOT_FOUND);
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_slow_request_does_not_hold_up_others() {
    use bone_proto::methods::{Empty, HealthCheck};
    // A provider address that accepts connections and never answers, so the
    // health check waits for its timeout.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    let (client, _events) =
        Client::new(server_at(&format!("http://{addr}/v1")).connect_in_process());
    client.initialize("test").await.unwrap();
    let client = Arc::new(client);
    let slow = tokio::spawn({
        let c = client.clone();
        async move { c.request::<HealthCheck>(Empty {}).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let reply = timeout(
        Duration::from_secs(1),
        client.request::<Echo>(EchoParams {
            text: "quick".into(),
        }),
    )
    .await
    .expect("echo answers while the health check waits")
    .unwrap();
    assert_eq!(reply.text, "quick");
    assert!(!slow.is_finished());
}
