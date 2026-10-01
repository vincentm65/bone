use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn ssh_args_run_bone_stdio_in_batch_mode() {
    let args = ssh_args("devbox", "bone").unwrap();
    assert_eq!(args.first().map(String::as_str), Some("-T"));
    assert!(args.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
    assert_eq!(&args[args.len() - 4..], ["devbox", "--", "bone", "stdio"]);
    assert_eq!(
        &ssh_args(" user@host ", "/opt/bone").unwrap()[args.len() - 4..],
        ["user@host", "--", "/opt/bone", "stdio"]
    );
}

#[test]
fn ssh_args_accept_a_copyable_host_and_port() {
    let args = ssh_args("me@office.example:2222", "bone").unwrap();
    assert!(args.windows(2).any(|pair| pair == ["-p", "2222"]));
    assert_eq!(
        &args[args.len() - 4..],
        ["me@office.example", "--", "bone", "stdio"]
    );

    let ipv6 = ssh_args("me@[2001:db8::1]:2222", "bone").unwrap();
    assert!(ipv6.windows(2).any(|pair| pair == ["-p", "2222"]));
}

#[test]
fn ssh_args_reject_hosts_that_could_be_options() {
    for host in ["", "  ", "-oProxyCommand=touch /tmp/x", "-p22", "a b"] {
        assert!(ssh_args(host, "bone").is_err(), "{host:?}");
    }
}

#[tokio::test]
async fn stdio_bridge_copies_both_ways_and_ends_when_the_daemon_closes() {
    let (daemon_side, bridge_side) = tokio::io::duplex(1024);
    let (input_reader, mut input_writer) = tokio::io::duplex(1024);
    let (output_writer, mut output_reader) = tokio::io::duplex(1024);
    let bridge = tokio::spawn(stdio_bridge(bridge_side, input_reader, output_writer));

    let (mut daemon_read, mut daemon_write) = tokio::io::split(daemon_side);
    input_writer.write_all(b"{\"cmd\":1}\n").await.unwrap();
    let mut got = [0u8; 10];
    daemon_read.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"{\"cmd\":1}\n");

    daemon_write.write_all(b"{\"ev\":2}\n").await.unwrap();
    let mut got = [0u8; 9];
    output_reader.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"{\"ev\":2}\n");

    drop((daemon_read, daemon_write));
    tokio::time::timeout(std::time::Duration::from_secs(2), bridge)
        .await
        .expect("bridge ends when the daemon closes")
        .unwrap()
        .unwrap();
}

#[cfg(unix)]
fn fake_ssh(script: &str) -> (tempfile::TempDir, String) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ssh");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = path.to_string_lossy().into_owned();
    (dir, path)
}

#[cfg(unix)]
#[tokio::test]
async fn ssh_session_pipes_the_remote_command_stdio() {
    // Stands in for `ssh host -- bone stdio`: skip to the remote command and
    // echo stdin back, proving the halves are wired the right way round.
    let (_dir, program) = fake_ssh(r#"while [ "$1" != "--" ]; do shift; done; exec cat"#);
    let (mut read, mut write, _session) = spawn_ssh(&program, "devbox", "bone").unwrap();
    write.write_all(b"ping\n").await.unwrap();
    let mut got = [0u8; 5];
    read.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"ping\n");
}

#[cfg(unix)]
#[tokio::test]
async fn ssh_failure_reports_the_last_stderr_line() {
    let (_dir, program) = fake_ssh(
        "echo 'debug noise' >&2; echo 'devbox: Permission denied (publickey).' >&2; exit 255",
    );
    let (mut read, _write, session) = spawn_ssh(&program, "devbox", "bone").unwrap();
    let mut rest = Vec::new();
    read.read_to_end(&mut rest).await.unwrap();
    let failure = session.failure().await;
    assert!(
        failure.ends_with("devbox: Permission denied (publickey)."),
        "{failure}"
    );
    assert!(failure.contains("255"), "{failure}");
}
