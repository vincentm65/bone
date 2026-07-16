//! Headless single-turn run: sends one prompt through the agent and prints the result.

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
    if let Some(prompt) = expand_lua_command(
        &request.prompt,
        &config_dir,
        request.approval_mode,
        request.provider.clone(),
        request.model.clone(),
    )
    .await
    {
        return agent::run_agent(AgentRequest {
            prompt,
            approval_mode: request.approval_mode,
            provider: request.provider,
            model: request.model,
            system_prompt: request.system_prompt,
            events: request.events,
            event_sender: None,
            agent_depth: 0,
            on_token_usage: None,
            activity: None,
            llm: None,
            session_sink: None,
            tool_allowlist: None,
            max_tokens: None,
            approval_gate: None,
            transcript: None,
            cancel: None,
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
        agent_depth: 0,
        on_token_usage: None,
        activity: None,
        llm: None,
        session_sink: None,
        tool_allowlist: None,
        max_tokens: None,
        approval_gate: None,
        transcript: None,
        cancel: None,
    })
    .await
}

/// Try to expand a prompt as a Lua command.
/// Returns the rendered prompt if the command exists and executes successfully.
async fn expand_lua_command(
    prompt: &str,
    config_dir: &std::path::Path,
    approval_mode: crate::tools::ApprovalMode,
    provider: Option<String>,
    model: Option<String>,
) -> Option<String> {
    let trimmed = prompt.trim();
    let command = trimmed.strip_prefix('/')?;
    let mut parts = command.splitn(2, char::is_whitespace);
    let name = parts.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    let args = parts.next().unwrap_or("").trim_start();

    // Clone owned values for spawn_blocking.
    let name_owned = name.clone();
    let args_owned = args.to_string();
    let config_dir_owned = config_dir.to_path_buf();

    // Run blocking Lua execution on a separate thread to avoid blocking the tokio worker.
    tokio::task::spawn_blocking(move || {
        // Boot extensions for command lookup only — no config sync/persist.
        let booted = ext::boot_with_tools(
            &config_dir_owned,
            &std::env::current_dir().unwrap_or_default(),
            &mut crate::config::custom::CustomConfigs::default(),
            false,
            ext::BootOptions {
                agent_depth: 0,
                headless: true,
                model: model.clone().unwrap_or_default(),
                provider: provider.clone().unwrap_or_default(),
                tool_allowlist: None,
            },
            &model.clone().unwrap_or_default(),
            &provider.clone().unwrap_or_default(),
        );
        let lua = booted.manager.lua_handle();
        let lua = lua.lock().unwrap_or_else(|e| e.into_inner());

        // Find the command handler.
        let handler = ext::ops_commands::find_handler(&lua, &name_owned)?;

        // Create ctx table.
        let config_dir_str = config_dir_owned.to_string_lossy().to_string();
        let shared_state = booted.tools.shared_state.clone();
        let mut ctx_cfg = crate::ext::ctx::CtxConfig::new(config_dir_str, shared_state);
        ctx_cfg.tool_handler = Some(booted.tools);
        ctx_cfg.approval_mode = approval_mode;
        ctx_cfg.provider = provider;
        ctx_cfg.model = model;
        let ctx_table = crate::ext::ctx::create_ctx_table(&lua, &ctx_cfg).ok()?;

        // Release the project Lua mutex before calling into Lua: a nested
        // LuaTool invocation via ctx.tools.call runs inline on this thread
        // and must re-acquire it (std::sync::Mutex is not reentrant).
        drop(lua);

        // Call handler(args, ctx).
        let result: Result<String, mlua::Error> = handler.call((args_owned, ctx_table));
        result.ok()
    })
    .await
    .ok()?
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
