//! An MCP (Model Context Protocol) client: tools from MCP servers.
//!
//! Nothing here runs unless core Lua configures a server (`bone.mcp.add`).
//! The [`McpManager`] lives as long as the core, across reloads: applying a
//! new configuration stops servers that went away or changed, starts new
//! ones, and leaves the rest running. Each server is supervised: connect,
//! `initialize`, list its tools, follow `tools/list_changed`, and restart
//! with backoff if it dies.
//!
//! Transports: stdio (a child process speaking newline-delimited JSON-RPC,
//! in its own process group) and Streamable HTTP (each message POSTed; the
//! answer comes back as JSON or as server-sent events).
//!
//! Its tools join every model request as `<server>_<tool>` and run through
//! the same hooks as other tools.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::tools::{Tool, ToolContext, ToolResult, ToolSpec};

pub mod oauth;
use oauth::AUTH_REQUIRED;

const PROTOCOL_VERSION: &str = "2025-06-18";
/// A tool call's default limit; `timeout` (ms) in the server's config
/// changes it.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Connecting, `initialize` and the first tool list.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Restarts after a crash before giving up (until the next reload).
const MAX_RESTARTS: u32 = 5;
/// Lines of a stdio server's stderr kept for error messages.
const STDERR_LINES: usize = 20;

/// One server, as `bone.mcp.add` describes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ServerConfig {
    pub name: String,
    /// stdio: the program and its arguments.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Streamable HTTP: the endpoint.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Only these tools (by the server's names); `None` is all of them.
    #[serde(default)]
    pub allow: Option<Vec<String>>,
    #[serde(default)]
    pub deny: Vec<String>,
    /// Start on first use instead of at once.
    #[serde(default)]
    pub lazy: bool,
    /// Per-call limit in ms.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// OAuth: a client registered by hand, for servers that do not allow
    /// registering one on the fly.
    #[serde(default)]
    pub client_id: Option<String>,
}

impl ServerConfig {
    /// From Lua, where empty lists arrive as `{}`.
    pub fn from_lua_json(mut v: Value) -> Result<Self, String> {
        for k in ["args", "allow", "deny"] {
            if let Some(x) = v.get(k) {
                v[k] = crate::agent::list(x);
            }
        }
        let c: ServerConfig = serde_json::from_value(v).map_err(|e| e.to_string())?;
        match (&c.command, &c.url) {
            (Some(_), None) | (None, Some(_)) => Ok(c),
            _ => Err(format!(
                "MCP server {}: give either command (stdio) or url (HTTP)",
                c.name
            )),
        }
    }

    fn wants(&self, tool: &str) -> bool {
        self.allow
            .as_ref()
            .is_none_or(|a| a.iter().any(|t| t == tool))
            && !self.deny.iter().any(|t| t == tool)
    }

    fn call_timeout(&self) -> Duration {
        self.timeout
            .map(Duration::from_millis)
            .unwrap_or(CALL_TIMEOUT)
    }
}

/// A tool as the server describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Value,
}

/// The name the model sees: `<server>_<tool>`, in the characters tool
/// names allow, at most 64 long.
pub fn tool_name(server: &str, tool: &str) -> String {
    let mut n: String = format!("{server}_{tool}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    n.truncate(64);
    n
}

// ---- connections -----------------------------------------------------------

/// What a connection tells its supervisor.
#[derive(Debug, PartialEq)]
pub enum ConnEvent {
    ToolsChanged,
    Closed(String),
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A connection speaking newline-delimited JSON-RPC: a child's stdio, or
/// (tests) anything readable and writable.
pub struct LineConn {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next: AtomicU64,
    /// Keeps the process (group) alive while the connection is.
    _process: Option<ProcessGuard>,
}

impl LineConn {
    /// Run the connection over `reader`/`writer`. Returns it and the events
    /// its supervisor should watch.
    pub fn new(
        reader: impl AsyncRead + Unpin + Send + 'static,
        writer: impl AsyncWrite + Unpin + Send + 'static,
    ) -> (Self, mpsc::UnboundedReceiver<ConnEvent>) {
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let pending: Pending = Default::default();
        let mut writer = writer;
        tokio::spawn(async move {
            while let Some(line) = out_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err()
                    || writer.write_all(b"\n").await.is_err()
                    || writer.flush().await.is_err()
                {
                    return;
                }
            }
        });
        let pending2 = pending.clone();
        let replies = out.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            let why = loop {
                let line = match lines.next_line().await {
                    Ok(Some(l)) => l,
                    Ok(None) => break "the server closed the connection".to_owned(),
                    Err(e) => break format!("reading from the server: {e}"),
                };
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue; // not JSON-RPC (a log line); ignore it
                };
                handle_incoming(msg, &pending2, &ev_tx, |reply| {
                    let _ = replies.send(reply.to_string());
                });
            };
            for (_, tx) in pending2.lock().unwrap().drain() {
                let _ = tx.send(Err(why.clone()));
            }
            let _ = ev_tx.send(ConnEvent::Closed(why));
        });
        (
            LineConn {
                out,
                pending,
                next: AtomicU64::new(0),
                _process: None,
            },
            ev_rx,
        )
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if self.out.send(msg.to_string()).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err("the server is gone".into());
        }
        // If the caller gives up, tell the server.
        let mut guard = CancelOnDrop {
            conn: self,
            id,
            armed: true,
        };
        let r = rx
            .await
            .unwrap_or_else(|_| Err("the server is gone".into()));
        guard.armed = false;
        r
    }

    fn notify(&self, method: &str, params: Value) {
        let msg = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.out.send(msg.to_string());
    }
}

struct CancelOnDrop<'a> {
    conn: &'a LineConn,
    id: u64,
    armed: bool,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.conn.pending.lock().unwrap().remove(&self.id);
            self.conn.notify(
                "notifications/cancelled",
                json!({ "requestId": self.id, "reason": "cancelled" }),
            );
        }
    }
}

/// A response, a request from the server, or a notification.
fn handle_incoming(
    msg: Value,
    pending: &Pending,
    events: &mpsc::UnboundedSender<ConnEvent>,
    reply: impl Fn(Value),
) {
    let id = msg.get("id").cloned();
    match msg.get("method").and_then(Value::as_str) {
        None => {
            let Some(id) = id.as_ref().and_then(Value::as_u64) else {
                return;
            };
            let Some(tx) = pending.lock().unwrap().remove(&id) else {
                return;
            };
            let r = match msg.get("error") {
                Some(e) => Err(e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_owned()),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(r);
        }
        Some("notifications/tools/list_changed") => {
            let _ = events.send(ConnEvent::ToolsChanged);
        }
        Some(method) => {
            // A request from the server: bone offers no client features
            // besides answering pings.
            let Some(id) = id else { return };
            reply(if method == "ping" {
                json!({ "jsonrpc": "2.0", "id": id, "result": {} })
            } else {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("bone does not support {method}") } })
            });
        }
    }
}

/// Kills a stdio server's whole process group when the connection goes.
struct ProcessGuard {
    _child: tokio::process::Child,
    pgid: Option<u32>,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pgid {
            // SAFETY: kill(2) on our own child's process group.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

/// Streamable HTTP: every message is a POST.
pub struct HttpConn {
    http: reqwest::Client,
    /// The server's name, for its saved sign-in.
    name: String,
    url: String,
    headers: BTreeMap<String, String>,
    session: Mutex<Option<String>>,
    next: AtomicU64,
    events: mpsc::UnboundedSender<ConnEvent>,
}

impl HttpConn {
    fn post(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut b = self
            .http
            .post(&self.url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL_VERSION)
            .json(body);
        for (k, v) in &self.headers {
            b = b.header(k, v);
        }
        if !self.has_own_auth()
            && let Some(t) = oauth::access_token(&self.name, &self.url)
        {
            b = b.bearer_auth(t);
        }
        if let Some(s) = self.session.lock().unwrap().as_ref() {
            b = b.header("mcp-session-id", s);
        }
        b
    }

    /// The config carries its own `Authorization` header.
    fn has_own_auth(&self) -> bool {
        self.headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("authorization"))
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if !self.has_own_auth() && oauth::needs_refresh(&self.name, &self.url) {
            // Expired (or nearly): a failure here shows up as a 401 below.
            let _ = oauth::refresh(&self.name, &self.url).await;
        }
        let mut res = self
            .post(&body)
            .send()
            .await
            .map_err(|e| format!("{}: {e}", self.url))?;
        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            if self.has_own_auth() {
                return Err(format!(
                    "{AUTH_REQUIRED}: the server rejected the Authorization header in its config (HTTP 401)"
                ));
            }
            // One refresh and retry before asking the user to sign in again.
            if oauth::refresh(&self.name, &self.url).await.is_err() {
                return Err(format!(
                    "{AUTH_REQUIRED}: sign in to {} (HTTP 401)",
                    self.name
                ));
            }
            res = self
                .post(&body)
                .send()
                .await
                .map_err(|e| format!("{}: {e}", self.url))?;
            if res.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(format!(
                    "{AUTH_REQUIRED}: sign in to {} (HTTP 401)",
                    self.name
                ));
            }
        }
        if let Some(s) = res.headers().get("mcp-session-id")
            && let Ok(s) = s.to_str()
        {
            *self.session.lock().unwrap() = Some(s.to_owned());
        }
        if !res.status().is_success() {
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            return Err(format!("HTTP {status}: {}", text.trim()));
        }
        let sse = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("text/event-stream"));
        let pending: Pending = Default::default();
        let (tx, mut rx) = oneshot::channel();
        pending.lock().unwrap().insert(id, tx);
        let handle = |msg: Value| handle_incoming(msg, &pending, &self.events, |_| {});
        if !sse {
            let msg: Value = res.json().await.map_err(|e| e.to_string())?;
            handle(msg);
        } else {
            let mut parser = crate::provider::sse::SseParser::default();
            'read: loop {
                let chunk = res.chunk().await.map_err(|e| e.to_string())?;
                let datas = match &chunk {
                    Some(bytes) => parser.push(bytes),
                    None => parser.finish().into_iter().collect(),
                };
                for d in datas {
                    if let Ok(msg) = serde_json::from_str(&d) {
                        handle(msg);
                    }
                    if !pending.lock().unwrap().contains_key(&id) {
                        break 'read;
                    }
                }
                if chunk.is_none() {
                    break;
                }
            }
        }
        rx.try_recv()
            .unwrap_or_else(|_| Err("the server sent no answer".into()))
    }

    async fn notify(&self, method: &str, params: Value) {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.post(&body).send().await;
    }
}

pub enum Conn {
    Lines(LineConn),
    Http(HttpConn),
}

impl Conn {
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        match self {
            Conn::Lines(c) => c.request(method, params).await,
            Conn::Http(c) => c.request(method, params).await,
        }
    }

    async fn notify(&self, method: &str, params: Value) {
        match self {
            Conn::Lines(c) => c.notify(method, params),
            Conn::Http(c) => c.notify(method, params).await,
        }
    }

    /// Handshake, as the protocol requires before anything else.
    async fn initialize(&self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "bone", "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await;
        Ok(())
    }

    async fn list_tools(&self) -> Result<Vec<RemoteTool>, String> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let r = self.request("tools/list", params).await?;
            for t in r["tools"].as_array().into_iter().flatten() {
                let Some(name) = t["name"].as_str() else {
                    continue;
                };
                out.push(RemoteTool {
                    name: name.to_owned(),
                    description: t["description"].as_str().unwrap_or_default().to_owned(),
                    input_schema: match &t["inputSchema"] {
                        Value::Null => json!({ "type": "object", "properties": {} }),
                        s => s.clone(),
                    },
                    annotations: t["annotations"].clone(),
                });
            }
            match r["nextCursor"].as_str() {
                Some(c) if !c.is_empty() => cursor = Some(c.to_owned()),
                _ => return Ok(out),
            }
        }
    }

    /// The tool's result as text for the model, and whether it is an error.
    async fn call_tool(&self, name: &str, args: Value) -> Result<(String, bool), String> {
        let r = self
            .request("tools/call", json!({ "name": name, "arguments": args }))
            .await?;
        Ok((result_text(&r), r["isError"].as_bool().unwrap_or(false)))
    }
}

/// An MCP tool result as text: its text parts, a note for each other part,
/// or the structured content when there is no text.
fn result_text(r: &Value) -> String {
    let mut parts = Vec::new();
    for c in r["content"].as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("text") => parts.push(c["text"].as_str().unwrap_or_default().to_owned()),
            Some("resource") => parts.push(
                c["resource"]["text"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("[resource {}]", c["resource"]["uri"])),
            ),
            Some(kind) => parts.push(format!(
                "[{kind}{}]",
                c["mimeType"]
                    .as_str()
                    .map(|m| format!(" {m}"))
                    .unwrap_or_default()
            )),
            None => {}
        }
    }
    if parts.is_empty() && !r["structuredContent"].is_null() {
        return r["structuredContent"].to_string();
    }
    parts.join("\n")
}

/// Opens a connection for a server (tests replace it).
pub type Connector = Arc<
    dyn Fn(
            &ServerConfig,
        )
            -> BoxFuture<'static, Result<(Conn, mpsc::UnboundedReceiver<ConnEvent>), String>>
        + Send
        + Sync,
>;

/// The real connector: spawn the command, or connect over HTTP.
pub fn connect(
    config: &ServerConfig,
) -> BoxFuture<'static, Result<(Conn, mpsc::UnboundedReceiver<ConnEvent>), String>> {
    let config = config.clone();
    Box::pin(async move {
        if let Some(url) = &config.url {
            let (events, rx) = mpsc::unbounded_channel();
            let conn = HttpConn {
                http: reqwest::Client::new(),
                name: config.name.clone(),
                url: url.clone(),
                headers: config.headers.clone(),
                session: Mutex::new(None),
                next: AtomicU64::new(0),
                events,
            };
            return Ok((Conn::Http(conn), rx));
        }
        let program = config.command.clone().unwrap_or_default();
        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&config.args)
            .envs(config.env.iter())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        if let Some(cwd) = &config.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot run {program}: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        // Keep the end of stderr for error messages.
        let tail: Arc<Mutex<VecDeque<String>>> = Default::default();
        if let Some(err) = child.stderr.take() {
            let tail = tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let mut t = tail.lock().unwrap();
                    if t.len() == STDERR_LINES {
                        t.pop_front();
                    }
                    t.push_back(l);
                }
            });
        }
        let pgid = child.id();
        let (mut conn, mut events) = LineConn::new(stdout, stdin);
        conn._process = Some(ProcessGuard {
            _child: child,
            pgid,
        });
        // Closing reports what the server last said on stderr.
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(ev) = events.recv().await {
                let ev = match ev {
                    ConnEvent::Closed(why) => {
                        // Give the stderr reader a moment to catch up.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        let t = tail.lock().unwrap();
                        match t.back() {
                            Some(last) => ConnEvent::Closed(format!("{why}: {last}")),
                            None => ConnEvent::Closed(why),
                        }
                    }
                    other => other,
                };
                let _ = tx.send(ev);
            }
        });
        Ok((Conn::Lines(conn), rx))
    })
}

// ---- servers -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Not started yet (lazy).
    Idle,
    Starting,
    Ready,
    /// The server wants a (new) sign-in; waits for `mcp/auth` or a reconnect.
    Auth,
    Failed,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Starting => "starting",
            Status::Ready => "ready",
            Status::Auth => "auth",
            Status::Failed => "failed",
        }
    }
}

struct State {
    status: Status,
    error: Option<String>,
    conn: Option<Arc<Conn>>,
    tools: Vec<RemoteTool>,
}

struct Server {
    config: ServerConfig,
    state: Mutex<State>,
    changed: Notify,
    task: Mutex<Option<tokio::task::AbortHandle>>,
}

/// A sign-in that is waiting for the browser.
struct PendingAuth {
    /// Pasted redirect addresses or codes.
    paste: mpsc::UnboundedSender<String>,
    task: tokio::task::AbortHandle,
}

impl Server {
    fn set(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap());
        self.changed.notify_waiters();
    }

    fn status(&self) -> Status {
        self.state.lock().unwrap().status
    }
}

impl Server {
    /// Stop supervising; the connection (and process) goes with the task.
    fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
    }
}

/// Supervise one server: connect, list tools, follow changes, restart.
async fn supervise(server: Arc<Server>, connector: Connector) {
    let mut restarts = 0;
    loop {
        server.set(|s| s.status = Status::Starting);
        let started = tokio::time::timeout(START_TIMEOUT, async {
            let (conn, events) = connector(&server.config).await?;
            conn.initialize().await?;
            let tools = conn.list_tools().await?;
            Ok::<_, String>((Arc::new(conn), events, tools))
        })
        .await
        .unwrap_or_else(|_| Err("did not start in time".into()));
        let why = match started {
            Ok((conn, mut events, tools)) => {
                restarts = 0;
                server.set(|s| {
                    s.status = Status::Ready;
                    s.error = None;
                    s.conn = Some(conn.clone());
                    s.tools = tools;
                });
                loop {
                    match events.recv().await {
                        Some(ConnEvent::ToolsChanged) => {
                            if let Ok(tools) = conn.list_tools().await {
                                server.set(|s| s.tools = tools);
                            }
                        }
                        Some(ConnEvent::Closed(why)) => break why,
                        // HTTP has no event stream: stay as is.
                        None => std::future::pending::<()>().await,
                    }
                }
            }
            Err(why) => why,
        };
        if why.starts_with(AUTH_REQUIRED) {
            // Retrying cannot help: wait to be signed in (or reconnected).
            server.set(|s| {
                s.conn = None;
                s.tools.clear();
                s.error = Some(why.clone());
                s.status = Status::Auth;
            });
            std::future::pending::<()>().await;
        }
        restarts += 1;
        let give_up = restarts > MAX_RESTARTS;
        server.set(|s| {
            s.conn = None;
            s.tools.clear();
            s.error = Some(why.clone());
            s.status = if give_up {
                Status::Failed
            } else {
                Status::Starting
            };
        });
        if give_up {
            return;
        }
        // 1, 2, 4, 8, 16 seconds.
        tokio::time::sleep(Duration::from_secs(1u64 << (restarts - 1).min(4))).await;
    }
}

/// Every configured server, for the life of the core.
pub struct McpManager {
    servers: Mutex<BTreeMap<String, Arc<Server>>>,
    connector: Mutex<Connector>,
    auths: Mutex<HashMap<String, PendingAuth>>,
}

impl Default for McpManager {
    fn default() -> Self {
        McpManager {
            servers: Default::default(),
            connector: Mutex::new(Arc::new(connect)),
            auths: Default::default(),
        }
    }
}

impl McpManager {
    /// Use `connector` to open connections (tests).
    pub fn set_connector(&self, connector: Connector) {
        *self.connector.lock().unwrap() = connector;
    }

    /// Switch to `configs`: stop servers that are gone or changed, start new
    /// ones (unless lazy), keep the rest.
    pub fn apply(&self, configs: Vec<ServerConfig>) {
        let mut servers = self.servers.lock().unwrap();
        servers.retain(|name, s| {
            let keep = configs.iter().any(|c| &c.name == name && *c == s.config);
            if !keep {
                s.stop();
            }
            keep
        });
        for config in configs {
            if servers.contains_key(&config.name) {
                continue;
            }
            let server = Arc::new(Server {
                state: Mutex::new(State {
                    status: Status::Idle,
                    error: None,
                    conn: None,
                    tools: Vec::new(),
                }),
                changed: Notify::new(),
                task: Mutex::new(None),
                config,
            });
            if !server.config.lazy {
                self.start(&server);
            }
            servers.insert(server.config.name.clone(), server);
        }
    }

    fn start(&self, server: &Arc<Server>) {
        let connector = self.connector.lock().unwrap().clone();
        spawn_supervisor(server, connector, false);
    }

    fn server(&self, name: &str) -> Result<Arc<Server>, String> {
        self.servers
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| format!("no MCP server {name}"))
    }

    /// Drop the server's connection and start it again now, with a fresh
    /// restart budget.
    pub fn reconnect(&self, name: &str) -> Result<(), String> {
        let server = self.server(name)?;
        let connector = self.connector.lock().unwrap().clone();
        spawn_supervisor(&server, connector, true);
        Ok(())
    }

    /// Begin signing in to an HTTP server. The address goes to the user's
    /// browser; the server reconnects by itself once the browser comes back
    /// here or `auth_code` is given what it was redirected to.
    pub async fn auth_begin(&self, name: &str) -> Result<(String, String), String> {
        let server = self.server(name)?;
        let url = server
            .config
            .url
            .clone()
            .ok_or_else(|| format!("{name} is a stdio server; it has no sign-in"))?;
        let mut flow = oauth::Flow::start(name, &url, server.config.client_id.clone()).await?;
        let listener = flow.take_listener().ok_or("no redirect socket")?;
        let (shown, redirect) = (flow.url.clone(), flow.redirect_uri.clone());
        let flow = Arc::new(flow);
        let (paste, mut pasted) = mpsc::unbounded_channel::<String>();
        let connector = self.connector.lock().unwrap().clone();
        let task_server = server.clone();
        let handle = tokio::spawn(async move {
            let outcome = async {
                let redirected = flow.wait_for_redirect(listener);
                tokio::pin!(redirected);
                let code = loop {
                    tokio::select! {
                        r = &mut redirected => break r?,
                        p = pasted.recv() => {
                            let p = p.ok_or("cancelled")?;
                            match flow.code_from(&p) {
                                Ok(code) => break code,
                                Err(e) => task_server.set(|s| s.error = Some(format!("{AUTH_REQUIRED}: {e}"))),
                            }
                        }
                    }
                };
                flow.exchange(&code).await
            };
            match tokio::time::timeout(Duration::from_secs(600), outcome).await {
                Ok(Ok(())) => spawn_supervisor(&task_server, connector, true),
                Ok(Err(e)) => task_server.set(|s| {
                    s.status = Status::Auth;
                    s.error = Some(format!("{AUTH_REQUIRED}: sign-in failed: {e}"));
                }),
                Err(_) => task_server.set(|s| {
                    s.status = Status::Auth;
                    s.error = Some(format!("{AUTH_REQUIRED}: the sign-in timed out"));
                }),
            }
        });
        let old = self.auths.lock().unwrap().insert(
            name.to_owned(),
            PendingAuth {
                paste,
                task: handle.abort_handle(),
            },
        );
        if let Some(old) = old {
            old.task.abort();
        }
        Ok((shown, redirect))
    }

    /// Finish a sign-in from the address the browser was redirected to.
    pub fn auth_code(&self, name: &str, pasted: &str) -> Result<(), String> {
        let auths = self.auths.lock().unwrap();
        let pending = auths
            .get(name)
            .ok_or_else(|| format!("no sign-in is waiting for {name}; start one first"))?;
        pending
            .paste
            .send(pasted.to_owned())
            .map_err(|_| "that sign-in is over; start a new one".to_owned())
    }

    /// Forget the sign-in and disconnect.
    pub fn sign_out(&self, name: &str) -> Result<(), String> {
        let server = self.server(name)?;
        if let Some(p) = self.auths.lock().unwrap().remove(name) {
            p.task.abort();
        }
        oauth::sign_out(name)?;
        let connector = self.connector.lock().unwrap().clone();
        spawn_supervisor(&server, connector, true);
        Ok(())
    }

    fn all(&self) -> Vec<Arc<Server>> {
        self.servers.lock().unwrap().values().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.servers.lock().unwrap().is_empty()
    }

    /// Start lazy servers and wait (up to `limit`) for those starting for
    /// the first time. A server that failed and is waiting to restart is not
    /// waited for: its tools are just missing until it is back.
    pub async fn ensure_ready(&self, limit: Duration) {
        let servers = self.all();
        for s in &servers {
            if s.status() == Status::Idle {
                self.start(s);
            }
        }
        let _ = tokio::time::timeout(limit, async {
            for s in &servers {
                loop {
                    let changed = s.changed.notified();
                    {
                        let st = s.state.lock().unwrap();
                        if st.status != Status::Starting || st.error.is_some() {
                            break;
                        }
                    }
                    changed.await;
                }
            }
        })
        .await;
    }

    /// The tools of every ready server, as the model sees them.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        let mut out = Vec::new();
        for s in self.all() {
            let st = s.state.lock().unwrap();
            if st.status != Status::Ready {
                continue;
            }
            for t in st.tools.iter().filter(|t| s.config.wants(&t.name)) {
                out.push(ToolSpec {
                    name: tool_name(&s.config.name, &t.name),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                });
            }
        }
        out
    }

    /// The tool the model calls `name`, if a ready server has it.
    pub fn tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        for s in self.all() {
            let st = s.state.lock().unwrap();
            let Some(t) = st
                .tools
                .iter()
                .find(|t| s.config.wants(&t.name) && tool_name(&s.config.name, &t.name) == name)
            else {
                continue;
            };
            return Some(Arc::new(McpTool {
                read_only: t.annotations["readOnlyHint"] == true,
                spec: ToolSpec {
                    name: name.to_owned(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
                remote: t.name.clone(),
                server: s.clone(),
            }));
        }
        None
    }

    /// `{ server, annotations }` of an MCP tool, for hooks.
    pub fn describe(&self, name: &str) -> Option<Value> {
        for s in self.all() {
            let st = s.state.lock().unwrap();
            if let Some(t) = st
                .tools
                .iter()
                .find(|t| tool_name(&s.config.name, &t.name) == name)
            {
                return Some(
                    json!({ "server": s.config.name, "tool": t.name, "annotations": t.annotations }),
                );
            }
        }
        None
    }

    /// Call a server's tool by its own name (`bone.mcp.call`).
    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args: Value,
    ) -> Result<(String, bool), String> {
        let s = self
            .servers
            .lock()
            .unwrap()
            .get(server)
            .cloned()
            .ok_or_else(|| format!("no MCP server {server}"))?;
        if s.status() == Status::Idle {
            self.start(&s);
        }
        call_on(&s, tool, args).await
    }

    /// Every server: `{ name, state, error, tools }`.
    pub fn list(&self) -> Vec<bone_proto::methods::McpServerInfo> {
        self.all()
            .iter()
            .map(|s| {
                let st = s.state.lock().unwrap();
                let shown: Vec<&RemoteTool> =
                    st.tools.iter().filter(|t| s.config.wants(&t.name)).collect();
                bone_proto::methods::McpServerInfo {
                    name: s.config.name.clone(),
                    state: st.status.name().into(),
                    error: st.error.clone(),
                    tools: shown
                        .iter()
                        .map(|t| tool_name(&s.config.name, &t.name))
                        .collect(),
                    url: s.config.url.clone(),
                    command: s.config.command.as_ref().map(|c| {
                        std::iter::once(c.as_str())
                            .chain(s.config.args.iter().map(String::as_str))
                            .collect::<Vec<_>>()
                            .join(" ")
                    }),
                    lazy: s.config.lazy,
                    signed_in: s
                        .config
                        .url
                        .as_deref()
                        .is_some_and(|u| oauth::signed_in(&s.config.name, u)),
                    tool_info: shown
                        .iter()
                        .map(|t| bone_proto::methods::McpToolInfo {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            read_only: t.annotations["readOnlyHint"] == true,
                        })
                        .collect(),
                }
            })
            .collect()
    }
}

/// Wait (briefly) for the server, then call its tool.
async fn call_on(server: &Server, tool: &str, args: Value) -> Result<(String, bool), String> {
    let conn = tokio::time::timeout(START_TIMEOUT, async {
        loop {
            let changed = server.changed.notified();
            {
                let st = server.state.lock().unwrap();
                match st.status {
                    Status::Ready => return st.conn.clone(),
                    Status::Failed | Status::Auth => return None,
                    _ => {}
                }
            }
            changed.await;
        }
    })
    .await
    .ok()
    .flatten();
    let Some(conn) = conn else {
        let why = server.state.lock().unwrap().error.clone();
        return Err(format!(
            "MCP server {} is not running{}",
            server.config.name,
            why.map(|w| format!(": {w}")).unwrap_or_default()
        ));
    };
    let result = tokio::time::timeout(server.config.call_timeout(), conn.call_tool(tool, args))
        .await
        .map_err(|_| format!("{tool} did not answer in time"))?;
    if let Err(e) = &result
        && e.starts_with(AUTH_REQUIRED)
    {
        // The token stopped working mid-session: show it and stop offering
        // the tools until the user signs in again.
        server.set(|s| {
            s.conn = None;
            s.tools.clear();
            s.error = Some(e.clone());
            s.status = Status::Auth;
        });
    }
    result
}

/// (Re)start supervising `server`. Unless `force`, a running one is left.
fn spawn_supervisor(server: &Arc<Server>, connector: Connector, force: bool) {
    let mut task = server.task.lock().unwrap();
    if let Some(t) = task.as_ref() {
        if !force {
            return;
        }
        t.abort();
    }
    server.set(|s| {
        s.status = Status::Starting;
        s.error = None;
        s.conn = None;
        s.tools.clear();
    });
    let handle = tokio::spawn(supervise(server.clone(), connector));
    *task = Some(handle.abort_handle());
}

/// An MCP tool in the agent's hands.
struct McpTool {
    spec: ToolSpec,
    /// The server says it only reads, so it may run alongside others.
    read_only: bool,
    remote: String,
    server: Arc<Server>,
}

impl Tool for McpTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn parallel(&self) -> bool {
        self.read_only
    }

    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            tokio::select! {
                r = call_on(&self.server, &self.remote, args) => match r? {
                    (text, false) => Ok(text),
                    (text, true) => Err(text),
                },
                _ = ctx.cancel.cancelled() => Err("cancelled".into()),
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
