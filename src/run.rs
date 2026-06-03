use crate::agent::{self, AgentRequest, AgentResponse};
use crate::skills::{SkillStore, expand_skill_command};
use crate::tools::ApprovalMode;

pub struct RunRequest {
    pub prompt: String,
    pub approval_mode: ApprovalMode,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub allow_skill_scripts: bool,
}

pub fn parse_run_args(args: &[String]) -> Result<RunRequest, String> {
    let mut prompt: Option<String> = None;
    let mut approval: Option<String> = None;
    let mut provider: Option<String> = None;
    let mut model: Option<String> = None;
    let mut system_prompt: Option<String> = None;
    let mut allow_skill_scripts = false;
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
            "--allow-skill-scripts" => allow_skill_scripts = true,
            "--system-prompt" => {
                i += 1;
                system_prompt = Some(args.get(i).ok_or("--system-prompt requires a value")?.clone());
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
        allow_skill_scripts,
    })
}

pub async fn run_headless(request: RunRequest) -> Result<AgentResponse, String> {
    let store = SkillStore::load().map_err(|err| format!("failed to load skills: {err}"))?;
    let prompt = match expand_skill_command(
        &store,
        &request.prompt,
        request.allow_skill_scripts,
        request.approval_mode,
    )
    .await
    {
        Ok(rendered) => rendered,
        Err(err) if err == "not a skill invocation" || err.starts_with("unknown skill:") => {
            request.prompt.clone()
        }
        Err(err) => return Err(err),
    };

    agent::run_agent(AgentRequest {
        prompt,
        approval_mode: request.approval_mode,
        provider: request.provider,
        model: request.model,
        system_prompt: request.system_prompt,
        events: false,
    })
    .await
}

pub(crate) fn parse_approval(value: Option<&str>) -> Result<ApprovalMode, String> {
    match value {
        Some("read_only") | Some("safe") => Ok(ApprovalMode::Safe),
        Some("edit") | Some("edits") => Ok(ApprovalMode::Edits),
        Some("danger") => Ok(ApprovalMode::Danger),
        None => Ok(ApprovalMode::Safe),
        Some(other) => Err(format!("unknown approval mode: {other}")),
    }
}

fn run_usage() -> String {
    "Usage: bone run [--approval read_only|edit|danger] [--provider <id>] [--model <name>] [--allow-skill-scripts] [--prompt <text>|<text>]".to_string()
}
