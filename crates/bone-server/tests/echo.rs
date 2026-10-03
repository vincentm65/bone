//! End-to-end: client <-> in-process transport <-> server <-> core.

use std::sync::Arc;
use std::time::Duration;

use bone_client::{Client, ClientError};
use bone_core::Core;
use bone_core::config::{CoreConfig, ProviderConfig};
use bone_proto::methods::{Echo, EchoParams, Echoed, Initialize, InitializeParams};
use bone_proto::{PROTOCOL_VERSION, RpcError};
use bone_server::Server;
use tokio::time::timeout;

fn server() -> Server {
    // Leaked so the directory outlives the server task.
    let data = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    Server::new(Arc::new(Core::new(CoreConfig {
        provider: ProviderConfig {
            kind: None,
            options: serde_json::Value::Null,
            base_url: "http://127.0.0.1:9".into(),
            model: "m".into(),
            api_key: None,
            reasoning_effort: None,
            stream_usage: true,
        },
        system_prompt: None,
        data_dir: data.path().to_owned(),
        parallel_tools: true,
        max_tool_output: bone_core::config::DEFAULT_MAX_TOOL_OUTPUT,
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
    let (client, mut events) = connect();

    let init = client.initialize("test").await.unwrap();
    assert_eq!(init.protocol_version, PROTOCOL_VERSION);
    assert_eq!(init.server_name, "bone");

    let text = "hello bone3".to_owned();
    let reply = client
        .request::<Echo>(EchoParams { text: text.clone() })
        .await
        .unwrap();
    assert_eq!(reply.text, text);

    let event = timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.parse::<Echoed>().unwrap().unwrap().text, text);
}

#[tokio::test]
async fn requests_before_initialize_are_rejected() {
    let (client, _events) = connect();
    let err = client
        .request::<Echo>(EchoParams { text: "x".into() })
        .await
        .unwrap_err();
    assert_eq!(rpc_code(err), RpcError::NOT_INITIALIZED);
}

#[tokio::test]
async fn version_mismatch_is_rejected() {
    let (client, _events) = connect();
    let err = client
        .request::<Initialize>(InitializeParams {
            protocol_version: PROTOCOL_VERSION + 1,
            client_name: "future".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_code(err), RpcError::VERSION_MISMATCH);

    // A failed handshake leaves the connection uninitialized but usable.
    client.initialize("test").await.unwrap();
}

#[tokio::test]
async fn double_initialize_is_rejected() {
    let (client, _events) = connect();
    client.initialize("test").await.unwrap();
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

#[tokio::test]
async fn events_reach_every_client() {
    let server = server();
    let (a, _a_events) = Client::new(server.connect_in_process());
    let (b, mut b_events) = Client::new(server.connect_in_process());
    a.initialize("a").await.unwrap();
    b.initialize("b").await.unwrap();

    a.request::<Echo>(EchoParams {
        text: "from a".into(),
    })
    .await
    .unwrap();
    let event = timeout(Duration::from_secs(1), b_events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.parse::<Echoed>().unwrap().unwrap().text, "from a");
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
