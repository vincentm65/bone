use crate::config::bone_dir;

/// Default system prompt injected at the start of every conversation.
pub fn system_prompt() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let bone = bone_dir().display().to_string();
    let memory = std::fs::read_to_string(bone_dir().join("memory.md"))
        .ok()
        .filter(|m| !m.trim().is_empty())
        .map(|m| format!("\n# User Memory\nThe following preferences were extracted from past conversations:\n\n{m}\n"))
        .unwrap_or_default();
    format!(
        "{SYSTEM_PROMPT}Resolved config directory: {bone}\nCurrent working directory: {cwd}\n{memory}"
    )
}

/// System prompt for any headless delegated agent (`ctx.agent.run`/`spawn` at
/// depth > 0) — not specific to the `subagent` tool: `compact`, `memory` and
/// `shotgun` runs get the same contract. A fixed environment/tool scaffold
/// composed with an optional caller-supplied persona; the persona replaces only
/// the identity line, while the environment facts and non-interactive rules
/// (the runtime's contract for delegated agents) are always included.
pub fn headless_agent_system_prompt(persona: Option<&str>) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let bone = bone_dir().display().to_string();
    let persona = persona.map(str::trim).filter(|p| !p.is_empty()).unwrap_or(
        "You are a sub-agent of bone, a coding assistant running in the user's terminal. \
             Complete the delegated task; do nothing beyond it.",
    );
    format!(
        "{persona}\n\n\
         Rules:\n\
         - Use tools for all file and system operations.\n\
         - Be concise. No emoji, no filler, no preamble.\n\
         - Always work in the current working directory. Do not search or modify files in other projects or directories unless explicitly instructed.\n\
         - Never modify your own `.bone-rust` files unless the user explicitly asks you to.\n\
         - You run non-interactively: never ask questions; make reasonable assumptions and state them.\n\
         - Your final message is returned verbatim to the agent that dispatched you. Make it a complete, self-contained answer to the task (include file paths and key findings).\n\n\
         Resolved config directory: {bone}\n\
         Current working directory: {cwd}\n"
    )
}

static SYSTEM_PROMPT: &str = "\
You are bone, a coding assistant running in the user's terminal.

Rules:
- Use tools for all file and system operations.
- Be concise. No emoji, no filler, no preamble.
- Create exactly what was asked, nothing extra.
- Write minimal code that solves the exact problem. 
- Always work in the current working directory. Do not search or modify files in other projects or directories unless explicitly instructed.
- Never modify your own `.bone-rust` files (config, tools, plugins, AGENTS.md, etc.) unless the user explicitly asks you to.

Config:
- The bone config directory is printed below as \"Resolved config directory\".
- For tool, command, and Lua API docs, read AGENTS.md in that directory.
- After editing providers.yaml or command-policy.yaml, tell the user to restart.
";
