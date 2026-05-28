# Custom Tools System

## Status: Planned

## Objective

Allow users and the agent to add model-callable tools through YAML definitions without editing or recompiling Rust. Custom tools remain real LLM tools: they have typed arguments, participate in the agentic tool loop, return tool results, and may provide UI-mediated interactions.

## Preserved User Features

- YAML tool definitions stored in the Bone configuration directory under `tools/*.yaml`.
- Typed argument definitions converted to provider-visible JSON Schema.
- Script-backed tools whose argument values are passed through `TOOL_<ARG_NAME>` environment variables.
- `interaction: select` tools that ask the user to choose an option and return it to the model.
- Seeded `grep`, `gh`, and `ask_user` definitions on first initialization.
- `/tools` management and `/tools reload`.
- The agent can create custom tool YAML files using existing file tools and instruct the user to reload.

## Required Foundations

This plan shares foundation work with the skills system:

- Public configuration path helpers rather than hard-coded `~/.bone-rust`.
- A shared `src/tools/script_runner.rs` for subprocess execution, timeout, output limits, and platform behavior.
- Explicit approval behavior for executable definitions.

The custom-tools implementation may be delivered independently, but it should not duplicate the script runner.

## Configuration Location

Extend `src/config/mod.rs`:

```rust
pub fn tools_dir() -> PathBuf {
    bone_dir().join("tools")
}
```

All discovery, seed files, reload behavior, and system-prompt documentation use this helper so `XDG_CONFIG_HOME` remains supported.

## Definition Format

### Script Tool

```yaml
name: grep
version: 1
description: "Search for a pattern in files"
execution:
  kind: script
  safety: read_only
args:
  - name: pattern
    type: string
    description: "Regex pattern to search for"
    required: true
  - name: path
    type: string
    description: "Directory or file to search in"
    required: false
script: |
  rg -- "$TOOL_PATTERN" "${TOOL_PATH:-.}"
```

### Selection Interaction Tool

```yaml
name: ask_user
version: 1
description: "Ask the user a question with selectable options or a custom answer"
interaction: select
args:
  - name: question
    type: string
    description: "The question to ask"
    required: true
  - name: options
    type: array
    items: string
    description: "Choices the user can select"
    required: true
  - name: allow_custom
    type: boolean
    description: "Whether the user can type their own answer"
    required: false
```

The `execution.kind` wrapper makes executable intent explicit and leaves room for future non-shell handlers. For backward compatibility with the initial proposed YAML shape, a definition with `script` and no `execution` is valid: load it as `kind: script` with conservative `safety: danger`. This preserves existing definitions while letting reviewed definitions opt into a less restrictive declared safety.

## Types And Validation

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicToolConfig {
    pub name: String,
    pub version: u32,
    pub description: String,
    #[serde(default)]
    pub args: Vec<ToolArg>,
    pub script: Option<String>,
    pub interaction: Option<InteractionType>,
    pub execution: Option<ExecutionConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolArg {
    pub name: String,
    #[serde(rename = "type")]
    pub value_type: ArgType,
    pub items: Option<ArgType>,
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgType {
    String,
    Number,
    Boolean,
    Array,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionType {
    Select,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    pub kind: ExecutionKind,
    pub safety: DeclaredSafety,
}
```

### Load-Time Validation

- Tool name must match `[A-Za-z_][A-Za-z0-9_]*`.
- Argument names must match `[A-Za-z_][A-Za-z0-9_]*`.
- Reject argument environment-name collisions after normalization, e.g. `foo-bar` and `foo_bar` both mapping to `TOOL_FOO_BAR`.
- Reject a definition containing both `interaction` and `script`.
- A script tool must have a non-empty script. If safety is omitted, it defaults to `danger`.
- `interaction: select` must define required `question: string` and `options: array` with string items.
- Reject unknown argument types and unsupported schema combinations.
- Reject names colliding with builtin tools; reject duplicate custom names and report both source paths.
- Parse and validation failures produce visible reload/startup warnings and skip the affected file.

## Tool Definition Ownership Refactor

Dynamic names and descriptions require owned values:

```rust
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}
```

Update builtin tools and provider adapters accordingly. While doing this, remove `Box::leak` in `ShellTool::definition()` by returning owned formatted strings.

Refactor registry insertion so collisions cannot silently replace existing tools:

```rust
pub fn try_register<T: Tool + 'static>(&mut self, tool: T) -> Result<(), String>
```

or a consuming equivalent. Builtins register first; any duplicate dynamic tool is rejected.

## Loading, Seeding, And Enablement

### Seeding

Seed embedded defaults only when `tools_dir()` is first initialized and contains no YAML tool definitions:

- `grep.yaml`
- `gh.yaml`
- `ask_user.yaml`

Write a marker such as `.defaults-initialized` after seeding. This prevents deleted user-owned defaults from reappearing if the user later removes every tool file.

### Enablement

Current tool enablement is a separate allowlist stored in `UserConfig`, and `/tools` lists a hard-coded builtin list. Replace that split behavior with registry-driven management:

- The registry exposes definitions and origin metadata (`Builtin` or `Custom { path }`).
- `/tools` displays every registered tool, origin, and enabled status.
- `/tools reload` replaces the dynamic portion of the registry and refreshes the picker immediately.
- Builtin defaults retain existing enablement behavior.
- Seeded defaults may start enabled as part of first initialization to preserve the feature's out-of-box behavior.
- Newly created or newly discovered non-seeded custom tools start disabled unless the user explicitly enables them through `/tools`; do not allow an agent-authored executable tool to become available automatically merely by writing a YAML file.
- Enablement is controlled by application configuration, not by a YAML `enabled` field.
- Persist enabled tool names and an approved content fingerprint for enabled executable custom definitions. If a custom tool's executable definition changes on reload, disable it until the user reviews and enables the new content.
- Handle removed and re-added tools deterministically; re-adding a name does not restore executable enablement unless its approved fingerprint still matches.

This preserves seeded default usability while putting newly authored executable capabilities behind an explicit user action.

## Script Tools And Approval

### Execution

`DynamicTool::execute()` validates runtime arguments and invokes the shared script runner:

- Set one environment variable per supplied argument: `TOOL_<NORMALIZED_NAME>`.
- Strings are passed unchanged as environment values.
- Numbers and booleans are converted to JSON-compatible text.
- Arrays are serialized as JSON text unless a later version explicitly supports another representation.
- Missing required args or unexpected value types return a tool error before process creation.
- Enforce a timeout and output truncation using the shared runner.
- Non-zero exits return a tool error containing bounded diagnostic output.

### Approval Model

Custom tool execution is not equivalent to executing its displayed name. A YAML tool's script is the code that must be trusted.

- Store an approved `DeclaredSafety` (`read_only`, `edit`, or `danger`) in executable definitions.
- Backward-compatible flat `script` definitions with no declared safety receive `danger`, never an inferred lower category.
- A custom tool is never treated as safer than the declared category.
- On each call, `ApprovalMode` checks the dynamic tool's declared safety rather than treating all custom tool names as unclassified.
- Display `custom tool: <name> [<safety>]` during approvals, with a peek action exposing the actual configured script and effective arguments/environment names.
- The user enabling a newly discovered executable custom tool must be shown its name, declaration, and script and confirm enablement. This is separate from any per-call prompt required by the active approval mode.
- The system cannot prove an arbitrary script's declared safety. A malicious or mistaken declaration is therefore a trust decision made visible at enablement and when calls require approval.
- `danger` custom tools require explicit approval in `Safe` and `Edits`; `Danger` mode remains an explicit global opt-in consistent with existing application semantics.

Implementation options:

1. Extend `CommandSafety::for_call` with registry-provided dynamic metadata in `App::prepare_tool_call`.
2. Introduce a `PreparedToolCall { call, safety, preview }` calculated before approval.

Prefer option 2: policy evaluation then has enough context for builtins, custom tools, and scripted skills without making global command policy depend on registry state.

## Interaction Tools

`interaction: select` is a model-callable tool with no executable script. It does not use script execution approval, but it must be enabled like any other tool because it controls when conversation flow pauses for user input.

The existing approval `Prompt` renderer is reusable, but `Decision` is not: it maps choices to `Accept`, `Advise`, and `Cancel`. Add a dedicated method:

```rust
fn select_and_wait(
    &mut self,
    title: String,
    options: Vec<String>,
    term: &mut BoneTerminal,
) -> io::Result<Option<String>>
```

Flow:

1. The LLM calls `ask_user` with `question` and `options`.
2. `prepare_tool_call()` finds registered dynamic metadata and identifies `InteractionType::Select`.
3. Validate arguments before presenting the UI; invalid arguments return a tool error.
4. `select_and_wait()` displays the options and returns the selected text, or `None` on cancellation.
5. The selected text becomes a successful tool result; cancellation becomes an error result and cancels or continues the current agent loop according to a documented policy.
6. `DynamicTool::execute()` rejects interaction definitions defensively because they must be intercepted before ordinary execution.

## Architecture

```text
src/config/mod.rs              - `tools_dir()` path helper
src/tools/types.rs             - owned `ToolDefinition`
src/tools/registry.rs          - checked registration, origin metadata, reload support
src/tools/script_runner.rs      - shared subprocess runtime
src/tools/dynamic.rs            - parsing, validation, schema creation, execution metadata
src/tools/mod.rs                - builtin and dynamic registry construction
src/ui/app/mod.rs               - registry-driven `/tools`, enable confirmation, selection UI
src/ui/app/stream.rs            - prepared calls, safety approval, interaction interception
src/llm/prompts.rs              - concise authoring/reload guidance
defaults/tools/*.yaml           - seeded builtin custom definitions
```

## Implementation Steps

### Phase A: Runtime And Registry Foundations

1. Add `config::tools_dir()`.
2. Change `ToolDefinition.name` and `.description` to `String`; update providers, builtins, and tests.
3. Remove leaked shell description strings by returning owned values.
4. Add checked registry registration and origin metadata.
5. Extract or reuse the shared script runner defined by the skills plan.

### Phase B: Dynamic Definition Loading

1. Add typed YAML parsing and validation in `src/tools/dynamic.rs`.
2. Generate strict JSON Schema with `required` and `additionalProperties: false`.
3. Implement deterministic directory scanning, collision reporting, and warning collection.
4. Implement first-initialization seed files and marker behavior.
5. Register valid loaded tools after builtins using checked insertion.

### Phase C: Tools UI And Reload

1. Replace the `/tools` picker hard-coded list with registry-driven rows.
2. Display origin (`builtin`/`custom`) and enabled state.
3. Implement `/tools reload`, preserving enabled state for known definitions and disabling newly authored custom tools pending user enablement.
4. Add enable confirmation that displays the configured script and declared safety for executable custom tools.
5. Persist changes in `UserConfig`.

### Phase D: Tool Execution And Approval

1. Implement `DynamicTool::execute()` for script definitions using environment arguments and the shared runner.
2. Add prepared-call metadata so approval checks declared safety and approval UI can preview scripts.
3. Preserve `ApprovalMode` behavior for builtins.
4. Handle timeouts, non-zero exits, malformed arguments, and truncated output as tool errors.
5. Verify multiple executable tool calls maintain result ordering under existing concurrent execution behavior.

### Phase E: Interaction Tools

1. Add a separate option-selection UI API returning the selected value.
2. Intercept `interaction: select` before ordinary approval/execution.
3. Return selections as tool results and handle cancellation predictably.
4. Seed and verify `ask_user.yaml`.

### Phase F: Agent Authoring Guidance

1. Add concise system-prompt guidance describing the YAML location, script environment variables, explicit safety declaration, and `/tools reload`.
2. State that newly agent-created tools are disabled until the user enables them.
3. Avoid embedding full default definitions in every system prompt; the agent can inspect individual examples on demand.

## Security Requirements

- Newly created executable custom tools cannot be enabled silently by file creation or reload.
- Modified executable custom tools cannot retain enablement without a matching user-approved definition fingerprint.
- Script source and declared safety are visible during enablement and relevant approval prompts.
- Never splice LLM argument values into script source; pass them only through environment variables.
- Dynamic definitions cannot overwrite builtin tools.
- Interaction tools never execute scripts.
- Output is bounded before becoming tool result content.
- Definitions use the application's config directory resolution and do not assume a specific home path.

## Tests

- `ToolDefinition` owned-string provider serialization and builtin regressions.
- Checked registration rejects builtin/custom and custom/custom collisions.
- Dynamic YAML parse, name validation, argument validation, environment-name collision, and schema generation.
- Seeding occurs once and does not recreate deleted user-owned files.
- `XDG_CONFIG_HOME` discovery and reload behavior.
- `/tools` displays dynamic tools and persists enablement.
- Newly authored executable definitions remain disabled until confirmed.
- Modified executable definitions are disabled until their new fingerprint is confirmed.
- Script calls validate types, provide environment variables, enforce output bounds, and surface non-zero exits/timeouts.
- Approval evaluates dynamic safety metadata and displays script previews.
- Selection interactions return selected values rather than approval decisions and reject invalid argument shapes.
- Concurrent dynamic tool execution retains result ordering.

## Completion Criteria

- Valid custom YAML tools are advertised to providers and callable in the ordinary tool loop.
- Seeded default custom tools are available on initial setup and remain user-owned thereafter.
- Users can reload, view, enable, and disable both builtin and custom tools.
- Interaction tools return actual user selections to the model.
- Executable tools cannot overwrite builtins or silently become active when authored by an agent.
- Custom script execution uses a shared bounded runtime and an explicit, visible approval model.
