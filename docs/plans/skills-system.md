# Skills System

## Status: Planned

## Objective

Add reusable, shareable slash-command workflows that users and the agent can author without compiling Rust code. A skill remains a prompt template with an optional executable script; it is not exposed as an LLM tool.

## Preserved User Features

- YAML skills stored in the Bone configuration directory under `skills/*.yaml`.
- Invocation through slash commands such as `/x rust opinions`.
- Prompt-only, scripted-prompt, and script-only skills.
- Template variables `{{args}}` and `{{script_output}}`.
- `/skills list`, `/skills enable <name>`, `/skills disable <name>`, and `/skills reload`.
- The agent can discover, read, create, and use skill YAML files with existing tools when the user requests a skill conversationally rather than entering its slash command.
- Example skills for Hacker News lookup, commit-message generation, and email drafting.

## Configuration Location

Do not hard-code `~/.bone-rust`. Extend `src/config/mod.rs` with:

```rust
pub fn skills_dir() -> PathBuf {
    bone_dir().join("skills")
}
```

This preserves the application's current `XDG_CONFIG_HOME` and home-directory fallback behavior.

## Skill Format

```yaml
name: x
description: "Search X.com and build a report"
enabled: true
script: |
  curl -s "https://nitter.net/search?q=${BONE_ARGS}" | head -200
prompt: |
  Based on these results, build a report about: {{args}}
  Data:
  {{script_output}}
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    pub prompt: Option<String>,
    pub script: Option<String>,
}
```

### Validation

- `name` must match `[A-Za-z0-9_][A-Za-z0-9_-]*`.
- Names colliding with builtin commands (`help`, `clear`, `new`, `model`, `provider`, `tools`, `config`, `skills`, `edit`, `quit`, `exit`) are rejected with a warning.
- At least one of `prompt` or `script` must be present.
- Invalid YAML is reported and skipped without preventing other skills from loading.
- Duplicate skill names are rejected; report both paths rather than depending on directory iteration order.

## Invocation Behavior

| Skill shape | Slash invocation behavior |
|---|---|
| `prompt` only | Render `{{args}}` and submit the rendered user message to the LLM. |
| `script` and `prompt` | Obtain execution approval, run script with `BONE_ARGS`, render prompt with output, then submit the rendered user message to the LLM. |
| `script` only | Obtain execution approval, run script with `BONE_ARGS`, then submit raw stdout as the user message. |

Template rendering is single-pass. User input or script output containing `{{args}}` or `{{script_output}}` is inserted as data and is not expanded again.

## Execution And Approval

Scripted skills are initiated by the user, but they still execute code. They must not run directly from `SkillStore::invoke()`.

Introduce a shared process runner used by `ShellTool`, scripted skills, and later custom tools:

```text
src/tools/script_runner.rs
  ScriptRequest {
    command: String,
    env: Vec<(String, String)>,
    timeout_ms: u64,
  }
  ScriptOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
  }
  async fn run_script(request) -> Result<ScriptOutput, String>
```

- It owns process spawning, stdin isolation, timeout, cancellation behavior, and stdout/stderr truncation.
- It uses the same platform dispatch as the existing shell tool; if skills intentionally require POSIX shell syntax, explicitly mark them unsupported on Windows when loading or invoking.
- For skill scripts, use a synthetic `ToolCall` named `shell` whose `command` is the skill script for deterministic classification by `CommandSafety::for_call`.
- Display approval as `skill: <name>` with the script available in peek mode; approval must use the script content, not only the skill name.
- Respect `ApprovalMode`: read-only scripts may run automatically in `Safe`, edit scripts in `Edits`, danger scripts only in `Danger` or after explicit acceptance.
- Non-zero exit is an invocation failure for skills. Show truncated stderr and do not submit a prompt turn unless a later product decision adds an explicit "continue with failed output" action.

This shares the existing policy behavior instead of claiming equivalent safety while bypassing it.

## Conversation Flow

Current slash commands display system replies and return; ordinary user messages alone start a provider turn. Refactor the ordinary submission body into a reusable method:

```rust
async fn submit_user_turn(
    &mut self,
    text: String,
    display_text: Option<String>,
    term: &mut BoneTerminal,
) -> io::Result<()>
```

- Normal typed messages call `submit_user_turn(text, None, term)`.
- A successfully invoked skill calls `submit_user_turn(rendered, Some("/<name> <args>"), term)`.
- The transcript contains the rendered user message actually seen by the LLM.
- The UI should show that the turn originated from a skill without duplicating large script output in scrollback unnecessarily.
- A failed, disabled, or rejected skill produces a visible system/error message and does not start an LLM turn.

## Architecture

```text
src/config/mod.rs            - `skills_dir()` path helper
src/tools/script_runner.rs    - shared process execution primitive
src/skills/types.rs           - `Skill`, parsing and name validation
src/skills/mod.rs             - `SkillStore`, render/invocation preparation
src/lib.rs                    - export `skills`
src/ui/app/mod.rs             - `App.skills`, skill management commands
src/ui/app/stream.rs          - reusable `submit_user_turn`, approved invocation flow
src/ui/commands/mod.rs        - help text and non-interactive `/skills` command responses
src/llm/prompts.rs            - concise discovery/authoring guidance
defaults/skills/*.yaml        - optional example source files
```

`SkillStore` owns loaded definitions, but execution and UI approval stay on `App` because they require terminal interaction and access to the normal turn loop.

## Implementation Steps

### Phase 1: Shared Runtime Foundation

1. Add `config::skills_dir()` and use it for discovery, examples, and prompt documentation.
2. Extract the subprocess execution behavior from `ShellTool` into `src/tools/script_runner.rs`.
3. Preserve current shell output formatting through `ShellTool`; define skill invocation failures separately from shell result formatting.
4. Add tests proving shell behavior and approval classification do not regress after extraction.

### Phase 2: Skill Storage And Validation

1. Add `src/skills/types.rs` and `src/skills/mod.rs`.
2. Implement `SkillStore::load()`, `reload()`, `list()`, `get_enabled()`, and `set_enabled()`.
3. Update YAML atomically when enabling or disabling; use the existing atomic-write helper or an equivalent shared config writer.
4. Sort files and listing output for deterministic behavior.
5. Load examples only through an explicit seeding policy: seed missing example files on first initialization of an empty skills directory; do not recreate user-deleted examples thereafter.

### Phase 3: Slash Commands And LLM Turn Submission

1. Add `skills: SkillStore` to `App`.
2. Support `/skills`, `/skills list`, `/skills enable <name>`, `/skills disable <name>`, and `/skills reload`.
3. For unknown slash commands, resolve enabled skills after builtin command dispatch.
4. Refactor normal message handling into `submit_user_turn()`.
5. Implement prompt-only skill invocation using the reused submission method.

### Phase 4: Scripted Skills With Approval

1. Add scripted invocation preparation that passes `BONE_ARGS` through a process environment variable, never textual shell interpolation.
2. Classify the actual configured script through the shell command policy.
3. Reuse/extend the approval UI so the user sees the skill name and can inspect the script being executed.
4. Run the script only after approval rules permit it, render the result, and enter the ordinary LLM turn.
5. Handle rejection, timeout, non-zero exit, and invalid UTF-8/lossy output consistently.

### Phase 5: Agent Discoverability And Examples

1. Add a short system-prompt section identifying `skills_dir()`, YAML format, and `/skills reload`.
2. Tell the agent that `/name ...` is the canonical user invocation, but when a user asks for a skill conversationally it may read that skill and execute any configured script through the existing `shell` tool so normal approval applies.
3. Seed `hn.yaml`, `commit.yaml`, and `email.yaml` examples, keeping network/script behavior explicit.

## Security Requirements

- Creating or editing a skill is a file mutation and follows existing `write_file`/`edit_file` approval behavior.
- Executing a script is a separate action; prior approval to write the YAML does not authorize later execution.
- Always classify and display the real script content before execution.
- Pass arguments via `BONE_ARGS`; do not interpolate arguments into shell source in Rust.
- Do not include full skill contents in every provider prompt. Include format guidance only; let the agent read an individual YAML file when relevant.
- Cap script output before inserting it into conversation history to prevent unbounded context growth.

## Tests

- Parse valid skill shapes and reject missing-action, invalid-name, duplicate-name, and builtin-collision files.
- Verify `enabled` defaults to true and toggling persists atomically.
- Verify prompt-only rendering and single-pass interpolation.
- Verify config discovery honors `XDG_CONFIG_HOME`.
- Verify a slash skill produces a normal user turn and enters the LLM flow.
- Verify the system guidance permits conversational skill use only through existing approved tools.
- Verify disabled skills do not invoke.
- Verify scripted skill approval uses classification of its script, including read-only, edit, and danger cases.
- Verify rejection, timeout, non-zero exit, and output truncation do not submit an LLM turn.
- Verify examples seed once without recreating deleted user-owned files.

## Completion Criteria

- Users can manage and invoke all three skill forms from slash commands.
- The agent can author skills and tell the user to reload them.
- Scripted skills are subject to the same deterministic approval policy as equivalent shell commands.
- Skill-invoked text follows the same transcript/provider processing path as ordinary user input.
- No hard-coded config location or duplicated unsafe process runner is introduced.
