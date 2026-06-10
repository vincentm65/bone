use crate::agent::{self, AgentRequest, AgentResponse};
use crate::ext;
use crate::tools::ApprovalMode;

pub struct RunRequest {
    pub prompt: String,
    pub approval_mode: ApprovalMode,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub events: bool,
}

pub fn parse_run_args(args: &[String]) -> Result<RunRequest, String> {
    let mut prompt: Option<String> = None;
    let mut approval: Option<String> = None;
    let mut provider: Option<String> = None;
    let mut model: Option<String> = None;
    let mut system_prompt: Option<String> = None;
    let mut events = false;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--prompt" => {
                i += 1;
                prompt = Some(args.get(i).ok_or("--prompt requires a value")?.clone());
            }
            "--approval" => {
                i += 1;
                approval = Some(args.get(i).ok_or("--approval requires a value")?.clone());
            }
            "--provider" => {
                i += 1;
                provider = Some(args.get(i).ok_or("--provider requires a value")?.clone());
            }
            "--model" => {
                i += 1;
                model = Some(args.get(i).ok_or("--model requires a value")?.clone());
            }
            "--events" => {
                events = true;
            }
            "--system-prompt" => {
                i += 1;
                system_prompt = Some(
                    args.get(i)
                        .ok_or("--system-prompt requires a value")?
                        .clone(),
                );
            }
            "--help" | "-h" => return Err(run_usage()),
            other if other.starts_with("--") => {
                return Err(format!("unknown argument: {other}\n{}", run_usage()));
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    let prompt = prompt.unwrap_or_else(|| {
        if !positional.is_empty() {
            positional.join(" ")
        } else {
            use std::io::Read;
            let mut buf = String::new();
            let _ = std::io::stdin().read_to_string(&mut buf);
            buf.trim().to_string()
        }
    });

    if prompt.trim().is_empty() {
        return Err(
            "no prompt provided; use --prompt, positional text, or pipe to stdin".to_string(),
        );
    }

    Ok(RunRequest {
        prompt,
        approval_mode: parse_approval(approval.as_deref())?,
        provider,
        model,
        system_prompt,
        events,
    })
}

pub async fn run_headless(request: RunRequest) -> Result<AgentResponse, String> {
    let config_dir = crate::config::bone_dir();

    // Try Lua command expansion first.
    if let Some(prompt) = expand_lua_command(&request.prompt, &config_dir) {
        return agent::run_agent(AgentRequest {
            prompt,
            approval_mode: request.approval_mode,
            provider: request.provider,
            model: request.model,
            system_prompt: request.system_prompt,
            events: request.events,
            event_sender: None,
        })
        .await;
    }

    let prompt = request.prompt.clone();

    agent::run_agent(AgentRequest {
        prompt,
        approval_mode: request.approval_mode,
        provider: request.provider,
        model: request.model,
        system_prompt: request.system_prompt,
        events: request.events,
        event_sender: None,
    })
    .await
}

/// Try to expand a prompt as a Lua command.
/// Returns the rendered prompt if the command exists and executes successfully.
fn expand_lua_command(prompt: &str, config_dir: &std::path::Path) -> Option<String> {
    let trimmed = prompt.trim();
    let command = trimmed.strip_prefix('/')?;
    let mut parts = command.splitn(2, char::is_whitespace);
    let name = parts.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    let args = parts.next().unwrap_or("").trim_start();

    // Boot Lua.
    let (lua, _loaded) = ext::boot_lua(config_dir).ok()?;

    // Seed and run default commands.
    ext::seed_default_lua_commands(&config_dir.join("lua/commands"));
    if ext::run_default_commands(&lua, config_dir).is_err() {
        return None;
    }

    // Find the command in _commands.
    let bone_table = lua.globals().get::<mlua::Table>("bone").ok()?;
    let commands_table = bone_table.get::<mlua::Table>("_commands").ok()?;

    let mut found_entry: Option<mlua::Table> = None;
    for entry in commands_table.sequence_values::<mlua::Table>() {
        let entry = entry.ok()?;
        let cmd_name: String = entry.get("name").ok()?;
        if cmd_name == name {
            found_entry = Some(entry);
            break;
        }
    }

    let entry = found_entry?;

    // Get the handler.
    let handler: mlua::Value = entry.get("handler").ok()?;
    let handler = match handler {
        mlua::Value::Function(f) => f,
        mlua::Value::Table(t) => t.get("handler").ok()?,
        _ => return None,
    };

    // Create ctx table.
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let config_dir_str = config_dir.to_string_lossy().to_string();
    let shared_state: crate::ext::ctx::SharedState =
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let ctx_cfg = crate::ext::ctx::CtxConfig {
        cwd,
        config_dir: config_dir_str,
        shared_state,
        pane_sender: None,
            call_id: None,
        };
    let ctx_table = crate::ext::ctx::create_ctx_table(&lua, &ctx_cfg).ok()?;

    // Call handler(args, ctx).
    let result: Result<String, mlua::Error> = handler.call((args, ctx_table));
    result.ok()
}

pub(crate) fn parse_approval(value: Option<&str>) -> Result<ApprovalMode, String> {
    match value {
        Some("read_only") | Some("safe") => Ok(ApprovalMode::Safe),
        Some("danger") => Ok(ApprovalMode::Danger),
        None => Ok(ApprovalMode::Safe),
        Some(other) => Err(format!("unknown approval mode: {other}")),
    }
}

fn run_usage() -> String {
    "Usage: bone run [--approval safe|danger] [--events] [--provider <id>] [--model <name>] [--prompt <text>|<text>]".to_string()
}
