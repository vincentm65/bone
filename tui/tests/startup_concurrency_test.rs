mod common;

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bone::runtime::RuntimeEvent;

struct Server {
    child: Child,
    pid: u32,
}

struct ServerResult {
    pid: u32,
    conversation_id: i64,
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn unused_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn start_server(exe: &str, repo: &Path, bone_dir: &Path, addr: SocketAddr) -> Server {
    let child = Command::new(exe)
        .current_dir(repo)
        .env("BONE_DIR", bone_dir)
        .args([
            "serve",
            "--listen",
            &addr.to_string(),
            "--provider",
            "local",
            "--model",
            "local",
            "--shutdown-on-stdin-eof",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    Server { child, pid }
}

fn connect_and_read_conversation(addr: SocketAddr) -> Result<i64, String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let stream = loop {
        match TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
            Ok(stream) => break stream,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(format!("cannot connect to {addr}: {error}")),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| format!("cannot read from {addr}: {error}"))?;
        if read == 0 {
            return Err(format!("server {addr} closed before publishing a snapshot"));
        }
        let event: RuntimeEvent = serde_json::from_str(&line)
            .map_err(|error| format!("invalid event from {addr}: {error}: {line}"))?;
        if let RuntimeEvent::StateSnapshot { snapshot } = event
            && let Some(id) = snapshot.conversation_id
        {
            return Ok(id);
        }
    }
}

fn finish_server(mut server: Server, conversation_id: i64) -> ServerResult {
    drop(server.child.stdin.take());
    let output = server.child.wait_with_output().unwrap();
    ServerResult {
        pid: server.pid,
        conversation_id,
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn two_executables_start_concurrently_with_a_fresh_shared_bone_dir() {
    let exe = env!("CARGO_BIN_EXE_bone");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let bone_dir = common::temp_dir("executable-concurrent-startup");
    let first_addr = unused_addr();
    let second_addr = loop {
        let addr = unused_addr();
        if addr != first_addr {
            break addr;
        }
    };
    let mut first = start_server(exe, repo, &bone_dir, first_addr);
    let mut second = start_server(exe, repo, &bone_dir, second_addr);

    let first_client = std::thread::spawn(move || connect_and_read_conversation(first_addr));
    let second_client = std::thread::spawn(move || connect_and_read_conversation(second_addr));
    let first_id = first_client.join().unwrap();
    let second_id = second_client.join().unwrap();

    if first_id.is_err() || second_id.is_err() {
        let _ = first.child.kill();
        let _ = second.child.kill();
    }
    let first_result = finish_server(first, first_id.unwrap_or(-1));
    let second_result = finish_server(second, second_id.unwrap_or(-1));

    eprintln!("executable={exe} bone_dir={}", bone_dir.display());
    for result in [&first_result, &second_result] {
        eprintln!(
            "pid={} conversation_id={} status={} stdout={:?} stderr={:?}",
            result.pid, result.conversation_id, result.status, result.stdout, result.stderr
        );
        assert!(
            result.status.success() && result.conversation_id > 0,
            "pid={} conversation_id={} status={} stdout={:?} stderr={:?}",
            result.pid,
            result.conversation_id,
            result.status,
            result.stdout,
            result.stderr
        );
        assert!(
            !result.stderr.contains("fatal:") && !result.stderr.contains("session database"),
            "pid={} stderr={:?}",
            result.pid,
            result.stderr
        );
    }

    let _ = std::fs::remove_dir_all(bone_dir);
}
