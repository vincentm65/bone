use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::net::unix::pipe;
use tokio::time::{Instant, sleep_until};

use super::{Tool, ToolContext, ToolResult, ToolSpec, truncate_middle, typed_args};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;
/// How long to keep reading after the shell exits, for output still in flight
/// from background processes that inherited the pipe.
const EXIT_GRACE: Duration = Duration::from_millis(200);
/// Stop buffering past this; the model only sees head and tail anyway.
const MAX_CAPTURE: usize = 4 << 20;
const SHOW_HEAD: usize = 10_000;
const SHOW_TAIL: usize = 20_000;

pub struct Shell(ToolSpec);

impl Shell {
    pub fn new() -> Self {
        Shell(ToolSpec {
            name: "shell".into(),
            description: "Run a bash command in the working directory. stdout and stderr are \
                          combined. Not interactive: stdin is empty. Long output keeps only its \
                          start and end."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The bash command line." },
                    "timeout_secs": { "type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": "Kill after this many seconds. Default 120." }
                },
                "required": ["command"]
            }),
        })
    }
}

#[derive(Deserialize)]
struct Args {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

impl Tool for Shell {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: Args = typed_args(args)?;
            let timeout = args
                .timeout_secs
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, MAX_TIMEOUT_SECS);
            run(&args.command, Duration::from_secs(timeout), ctx)
                .await
                .map_err(|e| format!("cannot run command: {e}"))?
        })
    }
}

async fn run(command: &str, timeout: Duration, ctx: &ToolContext) -> std::io::Result<ToolResult> {
    // One pipe for both streams keeps their interleaving.
    let (reader, writer) = std::io::pipe()?;
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c")
        .arg(command)
        .current_dir(&ctx.cwd)
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer)
        .process_group(0)
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    // Close our copies of the write end so EOF is reachable.
    drop(cmd);
    let pgid = child.id().map(|id| id as i32);
    let mut rx = pipe::Receiver::from_owned_fd(reader.into())?;

    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    let mut status = None;
    let mut grace_until: Option<Instant> = None;

    let killed = loop {
        tokio::select! {
            n = rx.read(&mut buf) => match n {
                Ok(0) | Err(_) => break None,
                Ok(n) => {
                    let room = MAX_CAPTURE.saturating_sub(out.len());
                    out.extend_from_slice(&buf[..n.min(room)]);
                }
            },
            s = child.wait(), if status.is_none() => {
                status = Some(s?);
                grace_until = Some(Instant::now() + EXIT_GRACE);
            }
            _ = sleep_until(grace_until.unwrap_or(deadline)), if grace_until.is_some() => break None,
            _ = sleep_until(deadline) => break Some(format!("timed out after {}s", timeout.as_secs())),
            _ = ctx.cancel.cancelled() => break Some("cancelled".to_owned()),
        }
    };

    if killed.is_none() && status.is_none() {
        // Output closed but the shell is still running.
        tokio::select! {
            s = child.wait() => status = Some(s?),
            _ = sleep_until(deadline) => {}
            _ = ctx.cancel.cancelled() => {}
        }
    }
    if (killed.is_some() || status.is_none())
        && let Some(pgid) = pgid
    {
        // SAFETY: plain syscall; a negative pid targets the process group
        // created by `process_group(0)`.
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
    }

    let text = String::from_utf8_lossy(&out);
    let mut result = if text.trim().is_empty() {
        "(no output)".to_owned()
    } else {
        truncate_middle(text.trim_end(), SHOW_HEAD, SHOW_TAIL)
    };
    match (killed, status) {
        (Some(why), _) => {
            result.push_str(&format!("\n[{why}; process killed]"));
            return Ok(Err(result));
        }
        (None, Some(s)) => match (s.code(), s.signal()) {
            (Some(code), _) => result.push_str(&format!("\n[exit code: {code}]")),
            (None, Some(sig)) => result.push_str(&format!("\n[killed by signal {sig}]")),
            _ => {}
        },
        (None, None) => {
            result.push_str(&format!(
                "\n[timed out after {}s; process killed]",
                timeout.as_secs()
            ));
            return Ok(Err(result));
        }
    }
    Ok(Ok(result))
}
