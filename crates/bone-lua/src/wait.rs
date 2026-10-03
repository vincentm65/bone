//! Background work for Lua (`bone.system`, `bone.sleep`, `bone.http`), on
//! both sides. The core runs it while a coroutine waits; the TUI runs it and
//! then calls a callback. Dropping the future kills the process or request.

use std::time::Duration;

use serde_json::{Value as Json, json};
use tokio::io::AsyncWriteExt;

/// Run one wait spec: `{ system = cmd, cwd?, stdin?, timeout? }`,
/// `{ sleep = ms }` or `{ http = { url, method?, headers?, body?, timeout? } }`.
pub async fn run(spec: Json) -> Result<Json, String> {
    if let Some(cmd) = spec["system"].as_str() {
        return system(cmd, &spec).await;
    }
    if let Some(ms) = spec["sleep"].as_u64() {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        return Ok(json!(true));
    }
    if spec["http"].is_object() {
        return http(&spec["http"]).await;
    }
    Err(format!("unknown wait: {spec}"))
}

/// `{ code, stdout, stderr }`; on timeout the process is killed and
/// `timed_out = true` (code nil).
async fn system(cmd: &str, spec: &Json) -> Result<Json, String> {
    let mut c = tokio::process::Command::new("bash");
    c.arg("-c")
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = spec["cwd"].as_str() {
        c.current_dir(cwd);
    }
    let stdin = spec["stdin"].as_str().map(str::to_owned);
    c.stdin(if stdin.is_some() {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    });
    let mut child = c.spawn().map_err(|e| format!("cannot run {cmd:?}: {e}"))?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = pipe.write_all(data.as_bytes()).await;
        });
    }
    let out = child.wait_with_output();
    let out = match spec["timeout"].as_u64() {
        Some(ms) => match tokio::time::timeout(Duration::from_millis(ms), out).await {
            Ok(r) => r,
            Err(_) => return Ok(json!({ "timed_out": true, "stdout": "", "stderr": "" })),
        },
        None => out.await,
    }
    .map_err(|e| format!("{cmd:?}: {e}"))?;
    Ok(json!({
        "code": out.status.code(),
        "stdout": String::from_utf8_lossy(&out.stdout),
        "stderr": String::from_utf8_lossy(&out.stderr),
    }))
}

/// `{ status, headers, body }`. Errors (no connection, timeout) are
/// returned to Lua as errors; HTTP error statuses are not.
async fn http(req: &Json) -> Result<Json, String> {
    let url = req["url"].as_str().ok_or("bone.http needs a url")?;
    let method = req["method"].as_str().unwrap_or("GET");
    let method = reqwest::Method::from_bytes(method.to_uppercase().as_bytes())
        .map_err(|_| format!("bad HTTP method {method:?}"))?;
    let mut b = reqwest::Client::new().request(method, url);
    if let Some(h) = req["headers"].as_object() {
        for (k, v) in h {
            b = b.header(k, v.as_str().unwrap_or_default());
        }
    }
    match &req["body"] {
        Json::Null => {}
        Json::String(s) => b = b.body(s.clone()),
        other => b = b.json(other),
    }
    if let Some(ms) = req["timeout"].as_u64() {
        b = b.timeout(Duration::from_millis(ms));
    }
    let res = b.send().await.map_err(|e| format!("{url}: {e}"))?;
    let status = res.status().as_u16();
    let headers: serde_json::Map<String, Json> = res
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), json!(v.to_str().unwrap_or_default())))
        .collect();
    let body = res.text().await.map_err(|e| format!("{url}: {e}"))?;
    Ok(json!({ "status": status, "headers": headers, "body": body }))
}
