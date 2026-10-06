//! The MCP client against a fake server on an in-memory pipe, and one real
//! stdio server (a bash script).

use std::sync::atomic::AtomicUsize;

use super::*;

/// What the fake server saw and does.
#[derive(Default)]
struct Fake {
    connections: AtomicUsize,
    changed: std::sync::atomic::AtomicBool,
    cancelled: Mutex<Vec<Value>>,
}

fn tool(name: &str, annotations: Value) -> Value {
    json!({ "name": name, "description": format!("the {name} tool"), "inputSchema": { "type": "object" }, "annotations": annotations })
}

/// Answer one connection like a small MCP server would.
async fn serve(fake: Arc<Fake>, io: tokio::io::DuplexStream) {
    fake.connections.fetch_add(1, Ordering::SeqCst);
    let (r, mut w) = tokio::io::split(io);
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let msg: Value = serde_json::from_str(&line).unwrap();
        let id = msg["id"].clone();
        let result = match msg["method"].as_str().unwrap_or_default() {
            "initialize" => {
                json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": { "tools": { "listChanged": true } }, "serverInfo": { "name": "fake", "version": "1" } })
            }
            "tools/list" if msg["params"]["cursor"].is_null() => {
                json!({ "tools": [tool("echo", json!({ "readOnlyHint": true }))], "nextCursor": "p2" })
            }
            "tools/list" => {
                let mut tools = vec![
                    tool("fail", Value::Null),
                    tool("crash", Value::Null),
                    tool("change", Value::Null),
                    tool("slow", Value::Null),
                ];
                if fake.changed.load(Ordering::SeqCst) {
                    tools.push(tool("extra", Value::Null));
                }
                json!({ "tools": tools })
            }
            "tools/call" => match msg["params"]["name"].as_str().unwrap() {
                "echo" => {
                    json!({ "content": [{ "type": "text", "text": format!("echo: {}", msg["params"]["arguments"]["text"].as_str().unwrap_or("")) }] })
                }
                "fail" => {
                    json!({ "content": [{ "type": "text", "text": "boom" }], "isError": true })
                }
                "crash" => return,
                "change" => {
                    fake.changed.store(true, Ordering::SeqCst);
                    let note =
                        json!({ "jsonrpc": "2.0", "method": "notifications/tools/list_changed" });
                    w.write_all(format!("{note}\n").as_bytes()).await.unwrap();
                    json!({ "content": [{ "type": "text", "text": "changed" }] })
                }
                _ => continue, // slow: never answers
            },
            "notifications/cancelled" => {
                fake.cancelled.lock().unwrap().push(msg["params"].clone());
                continue;
            }
            _ => continue,
        };
        let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        w.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
    }
}

fn fake_connector(fake: Arc<Fake>) -> Connector {
    Arc::new(move |_config: &ServerConfig| {
        let fake = fake.clone();
        Box::pin(async move {
            let (ours, theirs) = tokio::io::duplex(1 << 16);
            tokio::spawn(serve(fake, theirs));
            let (r, w) = tokio::io::split(ours);
            let (conn, events) = LineConn::new(r, w);
            Ok((Conn::Lines(conn), events))
        })
    })
}

fn config(name: &str) -> ServerConfig {
    ServerConfig::from_lua_json(json!({ "name": name, "command": "unused", "args": {} })).unwrap()
}

fn manager() -> (McpManager, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let m = McpManager::default();
    m.set_connector(fake_connector(fake.clone()));
    (m, fake)
}

fn names(m: &McpManager) -> Vec<String> {
    m.tool_specs().into_iter().map(|t| t.name).collect()
}

fn ctx() -> ToolContext {
    ToolContext {
        call_id: String::new(),
        cwd: ".".into(),
        session_id: "s".into(),
        cancel: Default::default(),
        views: Default::default(),
        jobs: Default::default(),
        output: None,
        processes: None,
    }
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..300 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

#[tokio::test]
async fn lists_and_calls_tools() {
    let (m, _) = manager();
    m.apply(vec![config("fk")]);
    m.ensure_ready(Duration::from_secs(5)).await;
    assert_eq!(
        names(&m),
        ["fk_echo", "fk_fail", "fk_crash", "fk_change", "fk_slow"]
    );
    let echo = m.tool("fk_echo").unwrap();
    assert_eq!(
        echo.call(json!({ "text": "hi" }), &ctx()).await,
        Ok("echo: hi".into())
    );
    assert_eq!(
        m.tool("fk_fail").unwrap().call(json!({}), &ctx()).await,
        Err("boom".into())
    );
    assert_eq!(
        m.describe("fk_echo").unwrap()["annotations"]["readOnlyHint"],
        true
    );
    assert_eq!(
        m.call("fk", "echo", json!({ "text": "x" })).await,
        Ok(("echo: x".into(), false))
    );
    assert!(m.tool("fk_nope").is_none());
    let list = m.list();
    assert_eq!((list[0].state.as_str(), list[0].tools.len()), ("ready", 5));
}

#[tokio::test]
async fn follows_tool_changes_and_restarts_after_a_crash() {
    let (m, fake) = manager();
    m.apply(vec![config("fk")]);
    m.ensure_ready(Duration::from_secs(5)).await;
    m.tool("fk_change")
        .unwrap()
        .call(json!({}), &ctx())
        .await
        .unwrap();
    eventually("the new tool shows", || {
        names(&m).contains(&"fk_extra".to_owned())
    })
    .await;

    let crashed = m.tool("fk_crash").unwrap().call(json!({}), &ctx()).await;
    assert!(crashed.unwrap_err().contains("closed"));
    eventually("it restarted", || {
        fake.connections.load(Ordering::SeqCst) == 2 && m.list()[0].state == "ready"
    })
    .await;
}

#[tokio::test]
async fn calls_can_be_cancelled_or_time_out() {
    let (m, fake) = manager();
    let mut quick = config("fk");
    quick.timeout = Some(100);
    m.apply(vec![quick]);
    m.ensure_ready(Duration::from_secs(5)).await;
    let slow = m.tool("fk_slow").unwrap();
    let r = slow.call(json!({}), &ctx()).await;
    assert!(r.unwrap_err().contains("did not answer in time"));
    let c = ctx();
    let cancel = c.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    });
    assert_eq!(slow.call(json!({}), &c).await, Err("cancelled".into()));
    // The server heard about both.
    eventually("cancel notifications", || {
        fake.cancelled.lock().unwrap().len() == 2
    })
    .await;
}

#[tokio::test]
async fn config_filters_laziness_and_reapplying() {
    let (m, fake) = manager();
    let mut c = config("fk");
    c.allow = Some(vec!["echo".into(), "fail".into()]);
    c.deny = vec!["fail".into()];
    c.lazy = true;
    m.apply(vec![c.clone()]);
    assert_eq!(m.list()[0].state, "idle");
    assert_eq!(fake.connections.load(Ordering::SeqCst), 0);
    m.ensure_ready(Duration::from_secs(5)).await;
    assert_eq!(names(&m), ["fk_echo"]);
    assert!(m.tool("fk_fail").is_none());
    // The same config keeps the running server; a changed one restarts it.
    m.apply(vec![c.clone()]);
    assert_eq!(m.list()[0].state, "ready");
    c.deny.clear();
    c.lazy = false;
    m.apply(vec![c]);
    m.ensure_ready(Duration::from_secs(5)).await;
    assert_eq!(fake.connections.load(Ordering::SeqCst), 2);
    assert_eq!(names(&m), ["fk_echo", "fk_fail"]);
    m.apply(vec![]);
    assert!(m.list().is_empty() && m.is_empty());
}

#[test]
fn results_become_text() {
    assert_eq!(
        result_text(&json!({ "content": [
            { "type": "text", "text": "a" },
            { "type": "image", "mimeType": "image/png", "data": "..." },
            { "type": "resource", "resource": { "uri": "file:///x", "text": "body" } },
        ] })),
        "a\n[image image/png]\nbody"
    );
    assert_eq!(
        result_text(&json!({ "content": [], "structuredContent": { "n": 1 } })),
        r#"{"n":1}"#
    );
    assert_eq!(tool_name("my server", "do.it"), "my_server_do_it");
    assert!(ServerConfig::from_lua_json(json!({ "name": "x" })).is_err());
}

/// A real stdio server: bash answering with canned lines.
pub const BASH_SERVER: &str = r#"
[ -n "$STARTS" ] && echo started >> "$STARTS"
echo "a log line on stderr" >&2
while IFS= read -r line; do
  id=
  [[ $line =~ \"id\":([0-9]+) ]] && id=${BASH_REMATCH[1]}
  case $line in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"sh","version":"1"}}}\n' "$id";;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"hello","description":"says hi","inputSchema":{"type":"object","properties":{}}}]}}\n' "$id";;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"hi from bash"}]}}\n' "$id";;
  esac
done
"#;

#[tokio::test]
async fn talks_to_a_real_stdio_server() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("server.sh");
    std::fs::write(&script, BASH_SERVER).unwrap();
    let m = McpManager::default();
    m.apply(vec![
        ServerConfig::from_lua_json(json!({
            "name": "sh", "command": "bash", "args": [script.to_string_lossy()],
        }))
        .unwrap(),
    ]);
    m.ensure_ready(Duration::from_secs(10)).await;
    assert_eq!(names(&m), ["sh_hello"], "{:?}", m.list());
    assert_eq!(
        m.tool("sh_hello").unwrap().call(json!({}), &ctx()).await,
        Ok("hi from bash".into())
    );
    // A server that cannot start says why.
    let m = McpManager::default();
    m.apply(vec![
        ServerConfig::from_lua_json(json!({ "name": "no", "command": "/nonexistent/mcp" }))
            .unwrap(),
    ]);
    m.ensure_ready(Duration::from_secs(5)).await;
    eventually("an error", || m.list()[0].error.is_some()).await;
    assert!(m.list()[0].error.as_ref().unwrap().contains("cannot run"));
}

/// Streamable HTTP: JSON and SSE answers, and the session id round trip.
#[tokio::test]
async fn talks_to_an_http_server() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<(String, Value)>>> = Default::default();
    let seen2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let (head, body) = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map(|v| v.trim().parse().unwrap())
                    .unwrap_or(0);
                while buf.len() < i + 4 + len {
                    let n = sock.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                }
                break (
                    head,
                    serde_json::from_slice::<Value>(&buf[i + 4..i + 4 + len]).unwrap(),
                );
            };
            seen2.lock().unwrap().push((head, body.clone()));
            let id = body["id"].clone();
            let reply =
                |result: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
            let resp = match body["method"].as_str().unwrap_or_default() {
                "initialize" => {
                    let b = reply(
                        json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "serverInfo": { "name": "web", "version": "1" } }),
                    );
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nmcp-session-id: sess-1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}",
                        b.len()
                    )
                }
                "tools/list" => {
                    // As an event stream, with a notification before the answer.
                    let note = json!({ "jsonrpc": "2.0", "method": "notifications/message", "params": {} });
                    let b = reply(
                        json!({ "tools": [{ "name": "search", "inputSchema": { "type": "object" } }] }),
                    );
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {note}\n\ndata: {b}\n\n"
                    )
                }
                "tools/call" => {
                    let b = reply(json!({ "content": [{ "type": "text", "text": "found it" }] }));
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{b}",
                        b.len()
                    )
                }
                _ => "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_owned(),
            };
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.shutdown().await.ok();
        }
    });
    let m = McpManager::default();
    m.apply(vec![
        ServerConfig::from_lua_json(json!({
            "name": "web", "url": url, "headers": { "authorization": "Bearer t" },
        }))
        .unwrap(),
    ]);
    m.ensure_ready(Duration::from_secs(10)).await;
    assert_eq!(names(&m), ["web_search"], "{:?}", m.list());
    assert_eq!(
        m.tool("web_search").unwrap().call(json!({}), &ctx()).await,
        Ok("found it".into())
    );
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .all(|(h, _)| h.contains("authorization: bearer t"))
    );
    assert!(!seen[0].0.contains("mcp-session-id"));
    // Every message after initialize carries the session.
    assert!(
        seen[1..]
            .iter()
            .all(|(h, _)| h.contains("mcp-session-id: sess-1"))
    );
    assert_eq!(seen[1].1["method"], "notifications/initialized");
}
