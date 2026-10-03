//! `health/check`: the core's own checks, then those core Lua registered
//! with `bone.health(name, fn)`.

use std::time::Duration;

use bone_proto::methods::{HealthItem, HealthStatus};

use crate::Inner;
use crate::tools::Registry;

fn item(name: &str, status: HealthStatus, message: impl Into<String>) -> HealthItem {
    HealthItem {
        name: name.into(),
        status,
        message: message.into(),
    }
}

pub(crate) async fn check(inner: &Inner) -> Vec<HealthItem> {
    use HealthStatus::*;
    let rt = inner.runtime();
    let p = &rt.config.provider;
    let mut out = vec![item(
        "provider",
        Ok,
        format!("{} at {}", p.model, p.base_url),
    )];

    let local = ["://localhost", "://127.", "://[::1]", "://0.0.0.0"]
        .iter()
        .any(|h| p.base_url.contains(h));
    out.push(match (&p.api_key, local) {
        (Some(_), _) => item("api key", Ok, "set"),
        (None, true) => item("api key", Ok, "none (local server)"),
        (None, false) => item(
            "api key",
            Warn,
            "none set; a remote provider usually needs one (api_key in core.lua or BONE_API_KEY)",
        ),
    });
    out.push(match &p.kind {
        Some(kind) => item(
            "reachable",
            Ok,
            format!(
                "not checked: the {kind} provider is Lua (a plugin can add a bone.health check)"
            ),
        ),
        None => reachable(p).await,
    });

    let dir = &inner.data_dir;
    let probe = dir.join(".bone-health");
    out.push(
        match std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&probe, b"")) {
            Result::Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                item("sessions", Ok, format!("{} is writable", dir.display()))
            }
            Err(e) => item(
                "sessions",
                Error,
                format!("cannot write {}: {e}", dir.display()),
            ),
        },
    );

    let lua_tools: Vec<&str> = rt
        .tools
        .specs()
        .iter()
        .filter(|s| Registry::builtin().get(&s.name).is_none())
        .map(|s| s.name.as_str())
        .collect();
    for server in inner.mcp.list() {
        out.push(match server.state.as_str() {
            "ready" => item(
                &format!("mcp {}", server.name),
                Ok,
                format!("{} tools", server.tools.len()),
            ),
            "idle" => item(&format!("mcp {}", server.name), Ok, "starts on first use"),
            "starting" => item(
                &format!("mcp {}", server.name),
                Warn,
                match &server.error {
                    Some(e) => format!("restarting after: {e}"),
                    None => "starting".into(),
                },
            ),
            _ => item(
                &format!("mcp {}", server.name),
                Error,
                server.error.clone().unwrap_or_else(|| "failed".into()),
            ),
        });
    }
    match &rt.scripting {
        Some(s) => {
            let tools = if lua_tools.is_empty() {
                "no Lua tools".to_owned()
            } else {
                format!("Lua tools: {}", lua_tools.join(", "))
            };
            out.push(item("core lua", Ok, format!("loaded; {tools}")));
            out.extend(s.health().await);
        }
        None => out.push(item("core lua", Warn, "not loaded (no core.lua scripting)")),
    }
    out
}

/// Ask the provider for its model list: proves the address and key work.
async fn reachable(p: &crate::config::ProviderConfig) -> HealthItem {
    use HealthStatus::*;
    let url = format!("{}/models", p.base_url.trim_end_matches('/'));
    let mut req = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5));
    if let Some(key) = &p.api_key {
        req = req.bearer_auth(key);
    }
    match req.send().await {
        Result::Ok(r) if r.status().is_success() => {
            item("reachable", Ok, format!("{url} answered"))
        }
        Result::Ok(r) if matches!(r.status().as_u16(), 401 | 403) => item(
            "reachable",
            Error,
            format!("{url}: {} (is the API key right?)", r.status()),
        ),
        Result::Ok(r) => item(
            "reachable",
            Warn,
            format!("{url}: {} (some servers have no model list)", r.status()),
        ),
        Err(e) => item("reachable", Error, format!("{url}: {e}")),
    }
}
