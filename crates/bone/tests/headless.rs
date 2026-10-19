//! The `bone` binary in headless mode, over stdio and a Unix socket.
#![cfg_attr(not(unix), allow(unused_imports))]

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use bone_client::{Client, connect, spawn};
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

#[cfg(unix)]
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

#[cfg(unix)]
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

    let (a, _a_events) = Client::new(connect(&sock).await.unwrap());
    let (b, mut b_events) = Client::new(connect(&sock).await.unwrap());
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

#[cfg(unix)]
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
            if let Ok(c) = connect(&sock).await {
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

#[tokio::test]
async fn explicit_connect_failure_does_not_load_core_or_start_one() {
    let data = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join("core.lua"), "error('CORE_MUST_NOT_LOAD')").unwrap();
    let sock = data.path().join("missing.sock");
    let output = timeout(
        Duration::from_secs(5),
        bone(data.path()).arg("--connect").arg(&sock).output(),
    )
    .await
    .unwrap()
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("cannot connect to"), "{stderr}");
    assert!(!stderr.contains("CORE_MUST_NOT_LOAD"), "{stderr}");
    assert!(!sock.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn attaching_skips_core_config_even_with_empty_overrides() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".bone");
    std::fs::create_dir_all(&config).unwrap();
    // Both implicit and explicit attach must ignore even a broken core config.
    std::fs::write(config.join("core.lua"), "error('CORE_MUST_NOT_LOAD')").unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let sock = runtime.path().join("bone3/bone.sock");
    let server_data = tempfile::tempdir().unwrap();
    let _server = bone(server_data.path())
        .args(["--headless", "--listen"])
        .arg(&sock)
        .spawn()
        .unwrap();
    wait_for(&sock).await;

    for explicit in [false, true] {
        let mut cmd = bone(&config);
        cmd.env("HOME", home.path())
            .env("XDG_RUNTIME_DIR", runtime.path())
            .stdin(Stdio::null());
        for name in [
            "BONE_CONFIG_DIR",
            "BONE_DATA_DIR",
            "BONE_MODEL",
            "BONE_BASE_URL",
            "BONE_API_KEY",
            "BONE_REASONING_EFFORT",
            "BONE_SYSTEM_PROMPT",
            "BONE_APPROVAL",
            "BONE_DEBUG_HTTP",
        ] {
            cmd.env(name, "");
        }
        if explicit {
            cmd.arg("--connect").arg(&sock);
        }
        let output = timeout(Duration::from_secs(10), cmd.output())
            .await
            .unwrap()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        // No terminal is available, so the TUI exits after its successful attach.
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(!stderr.contains("CORE_MUST_NOT_LOAD"), "{stderr}");
        assert!(!stderr.contains("cannot connect"), "{stderr}");
        assert!(sock.exists());
    }
}

#[tokio::test]
async fn core_config_errors_keep_exit_code_two() {
    let data = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join("core.lua"), "error('BROKEN_CORE')").unwrap();
    let output = timeout(
        Duration::from_secs(5),
        bone(data.path()).arg("--headless").output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("BROKEN_CORE"));
}
