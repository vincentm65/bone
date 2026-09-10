//! Client-local features that never touch the daemon's authority: the
//! `/update` self-update action, the `:`/`!` inline shell, and their shared
//! output formatting.
//!
//! These mirror the TUI's client-local behavior (`tui/src/ui/app/stream/mod.rs`
//! `run_inline_command`, `tui/src/ui/app/mod.rs` `open_update`) without pulling
//! in `bone-core`. The inline shell runs on the client machine; only the folded
//! transcript text crosses the wire via `RuntimeCommand::AppendMessage`.
//!
//! Long-running work runs on a detached worker thread and reports back through a
//! [`LocalResult`] channel so the GUI never blocks.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use eframe::egui;

/// Timeout for an inline shell command, matching the TUI.
pub const SHELL_TIMEOUT_MS: u64 = 60_000;
/// Lines of `$ cmd\n<output>` folded into the daemon transcript for later turns.
pub const TRANSCRIPT_MAX_LINES: usize = 200;
/// Per-line character cap when folding output, mirroring `core`'s tool output.
const MAX_TOOL_LINE_CHARS: usize = 2000;
/// Cap on captured stdout/stderr per stream (bytes).
const CAPTURE_BYTES: usize = 2 * 1024 * 1024;
/// Do not let a descendant that escaped the shell's process group keep the
/// GUI worker blocked forever while its inherited pipe remains open.
const READER_JOIN_TIMEOUT: Duration = Duration::from_secs(1);

/// Result of a client-local background task, delivered to the owning tab.
pub enum LocalResult {
    /// `/update` finished; `reply` is the user-facing status line.
    Update { tab_id: u64, reply: String },
    /// An inline shell command finished; `output` is the formatted result.
    Shell {
        tab_id: u64,
        command: String,
        output: String,
        is_error: bool,
    },
}

/// User-facing reply for a finished `bone update`, matching the TUI's exit-code
/// mapping (`0` applied, `2` no changes, anything else failed). `None` means the
/// update process could not be started (or no `bone` binary was found).
pub fn update_reply(exit_code: Option<i32>) -> String {
    match exit_code {
        Some(0) => "Update applied. Restart bone to use the new binary.".to_string(),
        Some(2) => "Update: no changes.".to_string(),
        Some(_) => "Update failed.".to_string(),
        None => "Run `bone update` from your shell to update bone.".to_string(),
    }
}

/// Heading for an inline-shell transcript row, mirroring the TUI's
/// `format_shell_label` (`shell <first line>`).
pub fn shell_label(command: &str) -> String {
    let first = command.lines().next().unwrap_or(command);
    format!("shell {first}")
}

/// The `$ cmd\n<output>` text folded into the daemon transcript, truncated to
/// [`TRANSCRIPT_MAX_LINES`] lines.
pub fn shell_transcript_text(command: &str, output: &str) -> String {
    truncate_output(&format!("$ {command}\n{output}"), TRANSCRIPT_MAX_LINES)
}

/// Truncate a single over-long line, mirroring `core::tools::truncate_line`.
fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_TOOL_LINE_CHARS {
        return line.to_string();
    }
    let end = line
        .char_indices()
        .nth(MAX_TOOL_LINE_CHARS)
        .map(|(offset, _)| offset)
        .unwrap_or(line.len());
    let mut out = line[..end].to_string();
    out.push_str("…[truncated]");
    out
}

/// Truncate `output` to `max_lines`, keeping the first half and last half with a
/// marker showing how many lines were omitted. Mirrors `core::tools::shell`.
pub fn truncate_output(output: &str, max_lines: usize) -> String {
    let mut lines = Vec::new();
    let mut line_truncated = false;
    for line in output.lines() {
        let truncated = truncate_line(line);
        line_truncated |= truncated.len() != line.len();
        lines.push(truncated);
    }
    if lines.len() <= max_lines {
        return if line_truncated {
            lines.join("\n")
        } else {
            output.to_string()
        };
    }
    let head = max_lines / 2;
    let tail = max_lines - head;
    let marker = format!("... {} lines truncated ...", lines.len() - max_lines);
    let mut out = Vec::with_capacity(max_lines + 1);
    out.extend(lines.drain(..head));
    out.push(marker);
    let keep_from = lines.len().saturating_sub(tail);
    out.extend(lines.into_iter().skip(keep_from));
    out.join("\n")
}

/// Run `command` in a shell with [`SHELL_TIMEOUT_MS`], returning the formatted
/// output and whether it should render as an error. Blocking; call on a worker.
pub fn run_shell(command: &str) -> (String, bool) {
    run_shell_with_timeout(command, Duration::from_millis(SHELL_TIMEOUT_MS))
}

fn run_shell_with_timeout(command: &str, timeout: Duration) -> (String, bool) {
    let mut child = match shell_command(command).spawn() {
        Ok(child) => child,
        Err(error) => return (format!("[error: {error}]"), true),
    };
    let out_handle = child.stdout.take().map(spawn_reader);
    let err_handle = child.stderr.take().map(spawn_reader);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    terminate_child_tree(&mut child);
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                terminate_child_tree(&mut child);
                break None;
            }
        }
    };

    // A shell can exit while a background child inherits its stdout/stderr.
    // Clean up the shell's process tree before joining the reader threads so
    // those descendants cannot keep this worker alive indefinitely.
    if status.is_some() {
        terminate_child_tree(&mut child);
    }

    let stdout = join_reader(out_handle);
    let stderr = join_reader(err_handle);
    let mut result = if timed_out {
        format!(
            "[timed out after {}ms; partial output]\nstdout:\n{stdout}",
            timeout.as_millis()
        )
    } else {
        let label = status
            .and_then(|status| status.code())
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_string());
        format!("exit code: {label}\nstdout:\n{stdout}")
    };
    if !stderr.is_empty() {
        result.push_str(&format!("\nstderr:\n{stderr}"));
    }
    let is_error = timed_out
        || status
            .and_then(|status| status.code())
            .map(|code| code != 0)
            .unwrap_or(true);
    (result, is_error)
}

fn terminate_child_tree(child: &mut Child) {
    let pid = child.id();

    #[cfg(unix)]
    {
        // `shell_command` puts the shell in a new process group. A negative
        // PID targets that group, including descendants that inherited the
        // shell's output pipes.
        let _ = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    }

    #[cfg(windows)]
    {
        // `taskkill /T` is the standard Windows process-tree equivalent.
        // Suppress its output because this is a background GUI operation.
        let pid = pid.to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    // Keep the direct kill as a fallback if process-group/tree cleanup is not
    // available or did not find the process.
    let _ = child.kill();
    let _ = child.wait();
}

/// Spawn `bone update` on a worker thread and deliver [`LocalResult::Update`].
pub fn spawn_update(
    ctx: egui::Context,
    tx: Sender<LocalResult>,
    tab_id: u64,
    binary: std::path::PathBuf,
) {
    std::thread::spawn(move || {
        let reply = match Command::new(&binary).arg("update").status() {
            Ok(status) => update_reply(status.code()),
            Err(_) => update_reply(None),
        };
        let _ = tx.send(LocalResult::Update { tab_id, reply });
        ctx.request_repaint();
    });
}

/// Spawn an inline shell command on a worker thread and deliver
/// [`LocalResult::Shell`].
pub fn spawn_shell(ctx: egui::Context, tx: Sender<LocalResult>, tab_id: u64, command: String) {
    std::thread::spawn(move || {
        let (output, is_error) = run_shell(&command);
        let _ = tx.send(LocalResult::Shell {
            tab_id,
            command,
            output,
            is_error,
        });
        ctx.request_repaint();
    });
}

fn join_reader(handle: Option<std::thread::JoinHandle<String>>) -> String {
    let Some(handle) = handle else {
        return String::new();
    };
    let deadline = Instant::now() + READER_JOIN_TIMEOUT;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            // Dropping a JoinHandle detaches the reader. The shell worker can
            // return its partial output even if an escaped descendant still
            // owns the pipe; the reader will terminate when that pipe closes.
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.join().unwrap_or_default()
}

fn spawn_reader<R: Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() < CAPTURE_BYTES {
                        let take = n.min(CAPTURE_BYTES - buf.len());
                        buf.extend_from_slice(&chunk[..take]);
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    cmd
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_reply_matches_tui_exit_codes() {
        assert_eq!(
            update_reply(Some(0)),
            "Update applied. Restart bone to use the new binary."
        );
        assert_eq!(update_reply(Some(2)), "Update: no changes.");
        assert_eq!(update_reply(Some(1)), "Update failed.");
        assert_eq!(
            update_reply(None),
            "Run `bone update` from your shell to update bone."
        );
    }

    #[test]
    fn shell_label_uses_first_line() {
        assert_eq!(shell_label("echo hi"), "shell echo hi");
        assert_eq!(shell_label("ls\npwd"), "shell ls");
    }

    #[test]
    fn truncate_output_keeps_head_and_tail() {
        let text = (1..=10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let truncated = truncate_output(&text, 4);
        assert_eq!(
            truncated, "1\n2\n... 6 lines truncated ...\n9\n10",
            "head + marker + tail"
        );
        assert_eq!(truncate_output(&text, 100), text, "under limit unchanged");
    }

    #[test]
    fn transcript_text_prefixes_command() {
        let folded = shell_transcript_text("echo hi", "exit code: 0\nstdout:\nhi");
        assert!(folded.starts_with("$ echo hi\n"));
    }

    #[test]
    fn run_shell_reports_success_and_failure() {
        let (output, is_error) = run_shell("printf hello");
        assert!(output.contains("exit code: 0"), "{output}");
        assert!(output.contains("stdout:\nhello"), "{output}");
        assert!(!is_error, "{output}");

        let (output, is_error) = run_shell("exit 3");
        assert!(output.contains("exit code: 3"), "{output}");
        assert!(is_error, "{output}");
    }

    #[cfg(unix)]
    #[test]
    fn run_shell_does_not_wait_for_background_pipe_holders() {
        let started = Instant::now();
        let (output, is_error) =
            run_shell_with_timeout("sleep 30 & printf parent", Duration::from_millis(100));

        assert!(started.elapsed() < Duration::from_secs(2), "{output}");
        assert!(output.contains("stdout:\nparent"), "{output}");
        assert!(!is_error, "{output}");
    }

    #[cfg(unix)]
    #[test]
    fn run_shell_timeout_kills_the_process_group() {
        let started = Instant::now();
        let (output, is_error) = run_shell_with_timeout("sleep 30", Duration::from_millis(100));

        assert!(started.elapsed() < Duration::from_secs(2), "{output}");
        assert!(output.contains("timed out after 100ms"), "{output}");
        assert!(is_error, "{output}");
    }
}
