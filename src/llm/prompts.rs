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
- Use tools for all file and system operations. Never claim completion without a tool confirming it.
- Be concise. No emoji, no filler, no preamble.
- Create exactly what was asked, nothing extra. No README, pyproject.toml, setup.py, __init__.py, LICENSE, or package structures unless explicitly requested.
- Write minimal code that solves the exact problem. Prefer single files over multi-file packages.

Config:
- The bone config directory is printed below as \"Resolved config directory\".
- For config schema and tool/skill docs, read AGENTS.md in that directory.
- Edit settings in config/*.yaml. After editing providers.yaml or command-policy.yaml, tell the user to restart. After editing a skill or tool YAML, tell the user to run /skills reload or /tools reload.

";

/// System prompt for the compaction summary LLM call.
/// Sent along with the older messages that are about to be discarded,
/// instructing the LLM to produce a concise but information-preserving summary.
pub fn compact_summary_prompt() -> &'static str {
    COMPACT_SUMMARY_PROMPT
}

static COMPACT_SUMMARY_PROMPT: &str = "\
Summarize the following conversation between a user and an AI coding assistant. \
Preserve all information the assistant would need to continue helping effectively:\n\n\
- Key facts, decisions, and outcomes discussed\n\
- Files and directories that were read, created, or modified (include paths)\n\
- Commands that were run and their results (include relevant output)\n\
- Current state of any in-progress tasks\n\
- Unresolved issues or open questions\n\
- Any preferences or context the user has expressed\n\n\
Be thorough and specific. Preserve file paths, command outputs, and technical details.\n\
Do not add commentary. Output only the summary.\n";
