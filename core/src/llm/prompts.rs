//! System-prompt assembly for Bone and delegated agents.

use crate::config::bone_dir;

/// Bone's built-in system prompt with runtime context.
pub fn system_prompt() -> String {
    system_prompt_with_base(None)
}

/// System prompt injected at the start of a normal conversation.
///
/// A configured base replaces Bone's built-in base prompt, while runtime
/// configuration-directory and working-directory context is always appended.
pub fn system_prompt_with_base(configured_base: Option<&str>) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let bone = bone_dir().display().to_string();
    let base = configured_base.unwrap_or(SYSTEM_PROMPT);
    let separator = if base.ends_with('\n') { "" } else { "\n\n" };
    format!(
        "{base}{separator}Resolved config directory: {bone}\nCurrent working directory: {cwd}\n"
    )
}

/// System prompt for any headless delegated agent (`ctx.agent.run`/`spawn` at
/// depth > 0) — not specific to the `subagent` tool: `compact` and `shotgun`
/// runs get the same contract. A fixed environment/tool scaffold
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
         - For file contents, use read_file to read, create_file only when the path does not exist, and edit_file to modify an existing file. Never delete an existing file merely to make create_file applicable. Prefer these dedicated tools over shell commands such as cat, head, tail, sed, tee, printf, or redirection. Use shell for file contents only when a file tool explicitly recommends it, the operation spans many files, or a dedicated tool cannot perform the operation. If a file tool fails, follow its error instead of immediately retrying the same operation through shell.\n\
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
- For file contents, use read_file to read, create_file only when the path does not exist, and edit_file to modify an existing file. Never delete an existing file merely to make create_file applicable. These dedicated tools are the default and preferred interface.
- Do not use shell commands such as cat, head, tail, sed, tee, printf, or redirection for operations supported by the file tools.
- Use shell for file contents only when a file tool explicitly recommends it, the operation spans many files, or a dedicated file tool cannot perform the operation. If a file tool fails, follow its error instead of immediately retrying the same operation through shell.
- Be concise. No emoji, no filler, no preamble.
- Do exactly what was asked, nothing extra.
- Write minimal code that solves the exact problem. 
- Always work in the current working directory. Do not search or modify files in other projects or directories unless explicitly instructed.
- Never modify your own `.bone-rust` files (config, tools, plugins, AGENTS.md, etc.) unless the user explicitly asks you to.

Config:
- The bone config directory is printed below as \"Resolved config directory\".
- For core architecture, configuration, extension, agent, UI, and development docs, read the generated AGENTS.md index and the relevant topic files in the resolved config directory: docs/architecture.md, docs/configuration.md, docs/extension-api.md, docs/agents.md, docs/ui.md, and docs/development.md.
- After directly editing config.yaml, providers.yaml, subagents.yaml, extensions.yaml, or command-policy.yaml, tell the user to restart.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prompt_includes_builtin_base_and_runtime_context() {
        let prompt = system_prompt();
        assert!(prompt.starts_with(SYSTEM_PROMPT));
        assert!(prompt.contains("Resolved config directory: "));
        assert!(prompt.contains("Current working directory: "));
    }

    #[test]
    fn configured_prompt_replaces_only_the_builtin_base() {
        let prompt = system_prompt_with_base(Some("Custom main-agent instructions."));
        assert!(prompt.starts_with("Custom main-agent instructions.\n\n"));
        assert!(!prompt.contains("You are bone, a coding assistant"));
        assert!(prompt.contains("Resolved config directory: "));
        assert!(prompt.contains("Current working directory: "));
    }
}
