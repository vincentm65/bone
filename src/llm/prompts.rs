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

static SYSTEM_PROMPT: &str = "\
You are bone, a coding assistant running in the user's terminal.

Rules:
- Use tools for all file and system operations.
- Be concise. No emoji, no filler, no preamble.
- Create exactly what was asked, nothing extra.
- Write minimal code that solves the exact problem. 

Config:
- The bone config directory is printed below as \"Resolved config directory\".
- For tool, command, and Lua API docs, read AGENTS.md in that directory.
- After editing providers.yaml or command-policy.yaml, tell the user to restart.
";
