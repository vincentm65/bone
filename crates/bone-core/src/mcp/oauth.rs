//! OAuth 2.1 for remote (HTTP) MCP servers: discovery, dynamic client
//! registration, authorization code + PKCE, and token refresh.
//!
//! Tokens live in `mcp-auth.json` next to settings.json, readable only by
//! the owner, keyed by server name. A server whose config carries its own
//! `Authorization` header never uses this.
//!
//! Signing in works over SSH too: the browser is usually on another machine,
//! so the redirect to `http://127.0.0.1:<port>/callback` may land nowhere.
//! [`Flow::finish_with`] accepts the address the browser ended up on (or just
//! the code), pasted back.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// What an error starts with when the server wants (new) credentials; the
/// supervisor shows the server as needing authentication instead of retrying.
pub const AUTH_REQUIRED: &str = "authentication required";

static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where `mcp-auth.json` goes (the config dir).
pub fn set_dir(dir: PathBuf) {
    *DIR.lock().unwrap() = Some(dir);
}

fn path() -> Option<PathBuf> {
    DIR.lock().unwrap().as_ref().map(|d| d.join("mcp-auth.json"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entry {
    /// The endpoint these tokens are for; a changed URL drops them.
    pub url: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub token_endpoint: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

fn load() -> BTreeMap<String, Entry> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(all: &BTreeMap<String, Entry>) -> Result<(), String> {
    let path = path().ok_or("no config directory to save sign-in in")?;
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(all).map_err(|e| e.to_string())? + "\n";
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// The saved sign-in for `name`, if it is for `url`.
pub fn entry(name: &str, url: &str) -> Option<Entry> {
    load().remove(name).filter(|e| e.url == url)
}

pub fn signed_in(name: &str, url: &str) -> bool {
    entry(name, url).is_some()
}

/// Forget the sign-in of `name`.
pub fn sign_out(name: &str) -> Result<(), String> {
    let mut all = load();
    if all.remove(name).is_some() {
        save(&all)?;
    }
    Ok(())
}

fn store(name: &str, e: Entry) -> Result<(), String> {
    let mut all = load();
    all.insert(name.to_owned(), e);
    save(&all)
}

/// The bearer token to send, if signed in.
pub fn access_token(name: &str, url: &str) -> Option<String> {
    entry(name, url)
        .map(|e| e.access_token)
        .filter(|t| !t.is_empty())
}

/// True when the token is gone or about to expire (and a refresh could help).
pub fn needs_refresh(name: &str, url: &str) -> bool {
    entry(name, url).is_some_and(|e| {
        e.refresh_token.is_some() && e.expires_at.is_some_and(|t| t <= now() + 60)
    })
}

fn form(pairs: &[(&str, &str)]) -> String {
    let mut u = Url::parse("http://x/").unwrap();
    u.query_pairs_mut().extend_pairs(pairs.iter().copied());
    u.query().unwrap_or_default().to_owned()
}

fn b64url(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if c.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if c.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

fn random(n: usize) -> Vec<u8> {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_err()
    {
        // No /dev/urandom (Windows): uuid's random bits will do.
        let mut bytes = Vec::new();
        while bytes.len() < n {
            bytes.extend_from_slice(uuid::Uuid::now_v7().as_bytes());
        }
        buf.copy_from_slice(&bytes[..n]);
    }
    buf
}

#[derive(Debug, Clone)]
struct Meta {
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
}

async fn get_json(http: &reqwest::Client, url: &str) -> Option<Value> {
    let r = http
        .get(url)
        .header("accept", "application/json")
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}

/// `resource_metadata="…"` out of a `WWW-Authenticate` header.
fn resource_metadata(www: &str) -> Option<String> {
    let i = www.find("resource_metadata=")? + "resource_metadata=".len();
    let rest = &www[i..];
    let rest = rest.trim_start_matches('"');
    let end = rest.find(['"', ',', ' ']).unwrap_or(rest.len());
    Some(rest[..end].to_owned())
}

fn origin(u: &Url) -> String {
    u.origin().ascii_serialization()
}

/// RFC 9728 then RFC 8414 (falling back to the conventional paths).
async fn discover(http: &reqwest::Client, server_url: &str, www: Option<&str>) -> Result<Meta, String> {
    let server = Url::parse(server_url).map_err(|e| format!("{server_url}: {e}"))?;
    let mut candidates = Vec::new();
    if let Some(u) = www.and_then(resource_metadata) {
        candidates.push(u);
    }
    let path = server.path().trim_end_matches('/');
    if !path.is_empty() {
        candidates.push(format!("{}/.well-known/oauth-protected-resource{path}", origin(&server)));
    }
    candidates.push(format!("{}/.well-known/oauth-protected-resource", origin(&server)));
    let mut issuer = None;
    for c in candidates {
        if let Some(doc) = get_json(http, &c).await
            && let Some(a) = doc["authorization_servers"][0].as_str()
        {
            issuer = Some(a.to_owned());
            break;
        }
    }
    let issuer = Url::parse(issuer.as_deref().unwrap_or(&origin(&server)))
        .map_err(|e| format!("authorization server: {e}"))?;
    let ipath = issuer.path().trim_end_matches('/');
    let mut docs = Vec::new();
    if !ipath.is_empty() {
        docs.push(format!("{}/.well-known/oauth-authorization-server{ipath}", origin(&issuer)));
        docs.push(format!("{}/.well-known/openid-configuration{ipath}", origin(&issuer)));
    }
    docs.push(format!("{}/.well-known/oauth-authorization-server", origin(&issuer)));
    docs.push(format!("{}/.well-known/openid-configuration", origin(&issuer)));
    for d in docs {
        if let Some(doc) = get_json(http, &d).await
            && let (Some(a), Some(t)) = (
                doc["authorization_endpoint"].as_str(),
                doc["token_endpoint"].as_str(),
            )
        {
            return Ok(Meta {
                authorization_endpoint: a.to_owned(),
                token_endpoint: t.to_owned(),
                registration_endpoint: doc["registration_endpoint"].as_str().map(str::to_owned),
            });
        }
    }
    // Servers without metadata use these paths (the MCP spec's fallback).
    let base = origin(&issuer);
    Ok(Meta {
        authorization_endpoint: format!("{base}/authorize"),
        token_endpoint: format!("{base}/token"),
        registration_endpoint: Some(format!("{base}/register")),
    })
}

/// A sign-in in progress: the address to open, and what completes it.
pub struct Flow {
    pub url: String,
    pub redirect_uri: String,
    name: String,
    server_url: String,
    meta: Meta,
    client_id: String,
    client_secret: Option<String>,
    verifier: String,
    state: String,
    listener: Option<tokio::net::TcpListener>,
}

impl Flow {
    /// Discover, register a client and build the authorization address.
    pub async fn start(name: &str, server_url: &str, client_id: Option<String>) -> Result<Flow, String> {
        let http = reqwest::Client::new();
        // An unauthenticated request shows where the server says to look.
        let www = http
            .post(server_url)
            .header("accept", "application/json, text/event-stream")
            .json(&json!({ "jsonrpc": "2.0", "id": 0, "method": "ping" }))
            .send()
            .await
            .ok()
            .and_then(|r| {
                r.headers()
                    .get("www-authenticate")
                    .and_then(|h| h.to_str().ok().map(str::to_owned))
            });
        let meta = discover(&http, server_url, www.as_deref()).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("cannot listen for the sign-in redirect: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let (client_id, client_secret) = match client_id {
            Some(id) => (id, None),
            None => {
                let reg = meta.registration_endpoint.as_deref().ok_or(
                    "the server does not allow registering a client; set client_id in its config",
                )?;
                let res = http
                    .post(reg)
                    .json(&json!({
                        "client_name": "bone",
                        "redirect_uris": [redirect_uri],
                        "grant_types": ["authorization_code", "refresh_token"],
                        "response_types": ["code"],
                        "token_endpoint_auth_method": "none",
                    }))
                    .send()
                    .await
                    .map_err(|e| format!("registering a client: {e}"))?;
                if !res.status().is_success() {
                    let s = res.status();
                    let t = res.text().await.unwrap_or_default();
                    return Err(format!("registering a client: HTTP {s}: {}", t.trim()));
                }
                let doc: Value = res.json().await.map_err(|e| e.to_string())?;
                let id = doc["client_id"]
                    .as_str()
                    .ok_or("client registration returned no client_id")?
                    .to_owned();
                (id, doc["client_secret"].as_str().map(str::to_owned))
            }
        };
        let verifier = b64url(&random(32));
        let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
        let state = b64url(&random(16));
        let mut u = Url::parse(&meta.authorization_endpoint)
            .map_err(|e| format!("{}: {e}", meta.authorization_endpoint))?;
        u.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("resource", server_url),
        ]);
        Ok(Flow {
            url: u.to_string(),
            redirect_uri,
            name: name.to_owned(),
            server_url: server_url.to_owned(),
            meta,
            client_id,
            client_secret,
            verifier,
            state,
            listener: Some(listener),
        })
    }

    /// Wait for the browser to be redirected here; the code.
    pub async fn wait_for_redirect(
        &self,
        listener: tokio::net::TcpListener,
    ) -> Result<String, String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let (mut sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let mut buf = vec![0u8; 8192];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let target = req
                .lines()
                .next()
                .and_then(|l| l.split(' ').nth(1))
                .unwrap_or("/");
            let result = self.code_from(&format!("http://127.0.0.1{target}"));
            let (status, body) = match &result {
                Ok(_) => ("200 OK", "Signed in. You can close this tab and return to bone."),
                Err(_) if !target.starts_with("/callback") => {
                    // A favicon request or similar: keep waiting.
                    let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
                    continue;
                }
                Err(_) => ("400 Bad Request", "Sign-in failed. Return to bone for details."),
            };
            let page = format!(
                "<!doctype html><meta charset=utf-8><title>bone</title><body style=\"font:16px system-ui;margin:3em\">{body}"
            );
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}",
                        page.len()
                    )
                    .as_bytes(),
                )
                .await;
            return result;
        }
    }

    /// The socket the redirect arrives on (once).
    pub fn take_listener(&mut self) -> Option<tokio::net::TcpListener> {
        self.listener.take()
    }

    /// The code out of a redirect address (or a bare code), checking `state`.
    pub fn code_from(&self, pasted: &str) -> Result<String, String> {
        let pasted = pasted.trim();
        let Ok(u) = Url::parse(pasted) else {
            return if pasted.is_empty() {
                Err("nothing was pasted".into())
            } else {
                Ok(pasted.to_owned())
            };
        };
        let q: BTreeMap<_, _> = u.query_pairs().into_owned().collect();
        if let Some(e) = q.get("error") {
            return Err(format!(
                "the server refused: {e}{}",
                q.get("error_description")
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            ));
        }
        if q.get("state").is_some_and(|s| *s != self.state) {
            return Err("the redirect belongs to a different sign-in".into());
        }
        q.get("code")
            .cloned()
            .ok_or_else(|| "the address has no code".into())
    }

    /// Trade the code for tokens and save them.
    pub async fn exchange(&self, code: &str) -> Result<(), String> {
        let mut pairs = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("client_id", self.client_id.as_str()),
            ("code_verifier", self.verifier.as_str()),
            ("resource", self.server_url.as_str()),
        ];
        if let Some(s) = &self.client_secret {
            pairs.push(("client_secret", s));
        }
        let doc = token_request(&self.meta.token_endpoint, &pairs).await?;
        let entry = entry_from(&doc, &self.server_url, &self.client_id, self.client_secret.clone(), &self.meta.token_endpoint, None)?;
        store(&self.name, entry)
    }
}

fn entry_from(
    doc: &Value,
    url: &str,
    client_id: &str,
    client_secret: Option<String>,
    token_endpoint: &str,
    old_refresh: Option<String>,
) -> Result<Entry, String> {
    let access = doc["access_token"]
        .as_str()
        .ok_or("the token response has no access_token")?;
    Ok(Entry {
        url: url.to_owned(),
        client_id: client_id.to_owned(),
        client_secret,
        token_endpoint: token_endpoint.to_owned(),
        access_token: access.to_owned(),
        refresh_token: doc["refresh_token"].as_str().map(str::to_owned).or(old_refresh),
        expires_at: doc["expires_in"].as_u64().map(|s| now() + s),
    })
}

async fn token_request(endpoint: &str, pairs: &[(&str, &str)]) -> Result<Value, String> {
    let res = reqwest::Client::new()
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(form(pairs))
        .send()
        .await
        .map_err(|e| format!("{endpoint}: {e}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        let why = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|d| {
                d["error_description"]
                    .as_str()
                    .or(d["error"].as_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| text.trim().to_owned());
        return Err(format!("token request: HTTP {status}: {why}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("token response: {e}"))
}

/// Use the refresh token for a new access token. On failure the saved
/// sign-in is kept (the caller asks for a new one).
pub async fn refresh(name: &str, url: &str) -> Result<(), String> {
    let e = entry(name, url).ok_or(format!("{AUTH_REQUIRED}: not signed in"))?;
    let rt = e
        .refresh_token
        .clone()
        .ok_or(format!("{AUTH_REQUIRED}: the sign-in expired"))?;
    let mut pairs = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", rt.as_str()),
        ("client_id", e.client_id.as_str()),
        ("resource", url),
    ];
    if let Some(s) = &e.client_secret {
        pairs.push(("client_secret", s));
    }
    let doc = token_request(&e.token_endpoint, &pairs)
        .await
        .map_err(|why| format!("{AUTH_REQUIRED}: {why}"))?;
    let next = entry_from(&doc, url, &e.client_id, e.client_secret.clone(), &e.token_endpoint, Some(rt))?;
    store(name, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_the_rfc_vectors() {
        assert_eq!(b64url(b""), "");
        assert_eq!(b64url(b"f"), "Zg");
        assert_eq!(b64url(b"fo"), "Zm8");
        assert_eq!(b64url(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn pkce_challenge_matches_the_rfc_example() {
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            b64url(&Sha256::digest(v.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn finds_the_resource_metadata_hint() {
        let w = r#"Bearer realm="mcp", resource_metadata="https://x.test/.well-known/oauth-protected-resource""#;
        assert_eq!(
            resource_metadata(w).as_deref(),
            Some("https://x.test/.well-known/oauth-protected-resource")
        );
        assert_eq!(resource_metadata("Bearer"), None);
    }

    #[test]
    fn redirects_are_checked_against_the_state() {
        let flow = Flow {
            url: String::new(),
            redirect_uri: String::new(),
            name: "x".into(),
            server_url: "https://x.test/mcp".into(),
            meta: Meta {
                authorization_endpoint: String::new(),
                token_endpoint: String::new(),
                registration_endpoint: None,
            },
            client_id: "c".into(),
            client_secret: None,
            verifier: String::new(),
            state: "s1".into(),
            listener: None,
        };
        assert_eq!(
            flow.code_from("http://127.0.0.1:9/callback?code=abc&state=s1").unwrap(),
            "abc"
        );
        assert!(flow.code_from("http://127.0.0.1:9/callback?code=abc&state=zz").is_err());
        assert!(flow.code_from("http://127.0.0.1:9/callback?error=access_denied").is_err());
        assert_eq!(flow.code_from("rawcode").unwrap(), "rawcode");
    }
}
