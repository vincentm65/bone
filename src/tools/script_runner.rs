use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{Duration, timeout};

pub struct ScriptRequest {
    pub command: String,
    pub env: Vec<(String, String)>,
    pub timeout_ms: u64,
}

pub struct ScriptOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Returns the shell program, its argument flag, and a label for descriptions.
/// On Windows, prefers `pwsh` (PowerShell Core) and falls back to `powershell`
/// (Windows PowerShell 5.x built into Windows).
pub fn shell_command() -> (&'static str, &'static str, &'static str) {
    if cfg!(windows) {
        if which("pwsh") {
            ("pwsh", "-Command", "pwsh -Command")
        } else {
            ("powershell", "-Command", "powershell -Command")
        }
    } else {
        ("bash", "-lc", "bash -lc")
    }
}

fn which(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("-Version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

pub async fn run_script(request: ScriptRequest) -> Result<ScriptOutput, String> {
    let timeout_ms = request.timeout_ms.clamp(1_000, 300_000);
    let (shell, shell_arg, _) = shell_command();
    let mut child = Command::new(shell)
        .arg(shell_arg)
        .arg(&request.command)
        .envs(request.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;

    let mut stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("failed to capture stderr")?;

    let wait = async {
        let status_fut = async { child.wait().await.map_err(|e| e.to_string()) };
        let out_fut = async {
            let mut out = Vec::new();
            stdout
                .read_to_end(&mut out)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(out)
        };
        let err_fut = async {
            let mut err = Vec::new();
            stderr
                .read_to_end(&mut err)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(err)
        };
        let (status, out, err) = tokio::try_join!(status_fut, out_fut, err_fut)?;
        Ok::<_, String>((status, out, err))
    };

    let (status, out, err) = match timeout(Duration::from_millis(timeout_ms), wait).await {
        Ok(result) => result?,
        Err(_) => return Err(format!("command timed out after {timeout_ms}ms")),
    };

    Ok(ScriptOutput {
        exit_code: status.code(),
        stdout: truncate_output(&String::from_utf8_lossy(&out), 500),
        stderr: truncate_output(&String::from_utf8_lossy(&err), 100),
    })
}

pub async fn run_script_jsonl<F>(
    request: ScriptRequest,
    mut on_line: F,
) -> Result<ScriptOutput, String>
where
    F: FnMut(String) + Send,
{
    let timeout_ms = request.timeout_ms.clamp(1_000, 300_000);
    let (shell, shell_arg, _) = shell_command();
    let mut child = Command::new(shell)
        .arg(shell_arg)
        .arg(&request.command)
        .envs(request.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;

    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("failed to capture stderr")?;

    let wait = async {
        let status_fut = async { child.wait().await.map_err(|e| e.to_string()) };
        let out_fut = async {
            let mut out = String::new();
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        on_line(line.clone());
                        out.push_str(&line);
                        out.push('\n');
                    }
                    Ok(None) => break,
                    Err(err) => return Err(err.to_string()),
                }
            }
            Ok::<_, String>(out)
        };
        let err_fut = async {
            let mut err = Vec::new();
            stderr
                .read_to_end(&mut err)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(err)
        };
        let (status, out, err) = tokio::try_join!(status_fut, out_fut, err_fut)?;
        Ok::<_, String>((status, out, err))
    };

    let (status, out, err) = match timeout(Duration::from_millis(timeout_ms), wait).await {
        Ok(result) => result?,
        Err(_) => return Err(format!("command timed out after {timeout_ms}ms")),
    };

    Ok(ScriptOutput {
        exit_code: status.code(),
        stdout: out,
        stderr: truncate_output(&String::from_utf8_lossy(&err), 100),
    })
}

/// Truncate output to `max_lines`, keeping the first half and last half with a
/// marker showing how many lines were omitted.
pub fn truncate_output(output: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= max_lines {
        return output.to_string();
    }
    let head = max_lines / 2;
    let tail = max_lines - head;
    let mut out: Vec<&str> = lines[..head].to_vec();
    let truncated = format!("... {} lines truncated ...", lines.len() - max_lines);
    out.push(&truncated);
    out.extend_from_slice(&lines[lines.len() - tail..]);
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn jsonl_runner_drains_stderr_while_reading_stdout() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&lines);
        let output = run_script_jsonl(
            ScriptRequest {
                command: r#"i=0; while [ "$i" -lt 20000 ]; do echo stderr-line >&2; i=$((i+1)); done; echo '{"type":"finished","content":"ok"}'"#.to_string(),
                env: Vec::new(),
                timeout_ms: 2_000,
            },
            move |line| {
                seen.lock().unwrap().push(line);
            },
        )
        .await
        .expect("jsonl runner should not deadlock on large stderr output");

        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.contains(r#""content":"ok""#));
        assert!(!output.stderr.is_empty());
        assert_eq!(lines.lock().unwrap().len(), 1);
    }
}
