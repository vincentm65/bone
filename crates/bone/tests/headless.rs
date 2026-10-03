//! The `bone` binary in headless mode, over stdio and a Unix socket.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use bone_client::{Client, connect_unix, spawn};
use bone_proto::methods::{Echo, EchoParams, Echoed, SessionCreate, SessionList};
use tokio::process::Command;
use tokio::time::{sleep, timeout};

fn bone(data: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bone"));
    cmd.env("BONE_BASE_URL", "http://127.0.0.1:9")
        .env("BONE_MODEL", "m")
        .env("BONE_DATA_DIR", data)
        // Never read the developer's real ~/.bone.
        .env("BONE_CONFIG_DIR", data)
        .kill_on_drop(true);
    cmd
}

async fn wait_for(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("{} never appeared", path.display());
}

#[tokio::test]
async fn stdio_session_and_shutdown() {
    let data = tempfile::tempdir().unwrap();
    let mut cmd = bone(data.path());
    cmd.arg("--headless");
    let (conn, mut child) = spawn(cmd).unwrap();
    let (client, _events) = Client::new(conn);

    let init = client.initialize("test").await.unwrap();
    assert_eq!(init.server_name, "bone");
    let info = client
        .request::<SessionCreate>(Default::default())
        .await
        .unwrap();
    let list = client
        .request::<SessionList>(Default::default())
        .await
        .unwrap();
    assert_eq!(list[0].session_id, info.session_id);

    client.shutdown().await.unwrap();
    let status = timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status}");
}

#[tokio::test]
async fn stdio_exits_when_stdin_closes() {
    let data = tempfile::tempdir().unwrap();
    let mut child = bone(data.path())
        .arg("--headless")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let status = timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status}");
}

#[tokio::test]
async fn socket_serves_many_clients_and_cleans_up() {
    let data = tempfile::tempdir().unwrap();
    let sock = data.path().join("run/bone.sock");
    let mut server = bone(data.path())
        .args(["--headless", "--listen"])
        .arg(&sock)
        .spawn()
        .unwrap();
    wait_for(&sock).await;

    let (a, _a_events) = Client::new(connect_unix(&sock).await.unwrap());
    let (b, mut b_events) = Client::new(connect_unix(&sock).await.unwrap());
    a.initialize("a").await.unwrap();
    b.initialize("b").await.unwrap();
    a.request::<Echo>(EchoParams { text: "hi".into() })
        .await
        .unwrap();
    let event = timeout(Duration::from_secs(5), b_events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.parse::<Echoed>().unwrap().unwrap().text, "hi");

    // `shutdown` closes one connection, not the server.
    a.shutdown().await.unwrap();
    b.request::<Echo>(EchoParams {
        text: "still here".into(),
    })
    .await
    .unwrap();

    // A second server on the same live socket refuses to start.
    let second = bone(data.path())
        .args(["--headless", "--listen"])
        .arg(&sock)
        .stderr(Stdio::null())
        .status()
        .await
        .unwrap();
    assert!(!second.success());
    assert!(sock.exists());

    // SAFETY: plain syscall on our own child.
    unsafe { libc::kill(server.id().unwrap() as i32, libc::SIGTERM) };
    let status = timeout(Duration::from_secs(5), server.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status}");
    assert!(!sock.exists());
}

#[tokio::test]
async fn stale_socket_is_replaced() {
    let data = tempfile::tempdir().unwrap();
    let sock = data.path().join("bone.sock");
    // Bound and dropped: the file stays but nothing listens.
    drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());

    let _server = bone(data.path())
        .args(["--headless", "--listen"])
        .arg(&sock)
        .spawn()
        .unwrap();
    let conn = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(c) = connect_unix(&sock).await {
                return c;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let (client, _events) = Client::new(conn);
    client.initialize("test").await.unwrap();
}
