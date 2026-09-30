//! System-prompt assembly for Bone and delegated agents.

use crate::config::bone_dir;

/// Fixed tool-usage guidance appended to every main-agent system prompt.
/// Lives in code rather than `core/defaults/config.yaml` because seeding is
/// create-if-missing, so a YAML prompt change would not reach existing
/// installs.
const TOOL_USAGE_GUIDANCE: &str = "\
Tool usage:\n\
- Batch independent tool calls in one turn.\n\
- edit_file replaces lines by `LINE#HASH` anchors shown by read_file; pass several disjoint edits in one call via edits.\n\
- Do not re-read a file you just changed unless the edit reported the file changed.\n\
- shell/grep/rg output has no `LINE#HASH` anchors. Never edit from it; read_file the range first, then edit with those anchors.";

/// Environment facts appended to every prompt. The guide path is absolute
/// because a bare `AGENTS.md` resolves against the working directory, where it
/// does not exist.
fn runtime_context() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let bone = std::env::current_dir().map_or_else(|_| bone_dir(), |cwd| cwd.join(bone_dir()));
    format!(
        "Resolved config directory: {}\nBone guide: {} (read before changing Bone itself; not a project AGENTS.md)\nCurrent working directory: {cwd}\n",
        bone.display(),
        bone.join("AGENTS.md").display()
    )
}

/// System prompt injected at the start of a normal conversation.
///
/// Runtime configuration-directory and working-directory context is always
/// appended to the configured base prompt, preceded by the fixed
/// [`TOOL_USAGE_GUIDANCE`] block.
pub fn system_prompt(base: &str) -> String {
    let separator = if base.ends_with('\n') { "" } else { "\n\n" };
    format!(
        "{base}{separator}{TOOL_USAGE_GUIDANCE}\n\n{}",
        runtime_context()
    )
}

/// System prompt for any headless delegated agent (`ctx.agent.run`/`spawn` at
/// depth > 0) — not specific to the `subagent` tool: `compact` and `shotgun`
/// runs get the same contract. A fixed environment/tool scaffold
/// composed with an optional caller-supplied persona; the persona replaces only
/// the identity line, while the environment facts and non-interactive rules
/// (the runtime's contract for delegated agents) are always included.
pub fn headless_agent_system_prompt(persona: Option<&str>) -> String {
    let persona = persona.map(str::trim).filter(|p| !p.is_empty()).unwrap_or(
        "You are a sub-agent of bone, a coding assistant running in the user's terminal. \
             Complete the delegated task; do nothing beyond it.",
    );
    format!(
        "{persona}\n\n\
         Rules:\n\
         - Use tools for all file and system operations.\n\
         - For files, use read_file, create_file (only if the path does not exist) and edit_file; never delete a file just to use create_file. Use shell for file contents only when a file tool recommends it, the operation spans many files, or no dedicated tool fits. If a file tool fails, follow its error instead of retrying through shell.\n\
         - Be concise. No emoji, no filler.\n\
         - Always work in the current working directory. Do not search or modify files in other projects or directories unless explicitly instructed.\n\
         - Never modify your own `.bone-rust` files unless the user explicitly asks you to.\n\
         - You run non-interactively: never ask questions; make reasonable assumptions and state them.\n\
         - Your final message is returned verbatim to the agent that dispatched you. Make it a complete, self-contained answer to the task (include file paths and key findings).\n\n{}",
        runtime_context()
    )
}

#[cfg(test)]
#[path = "prompts_tests.rs"]
mod tests;
