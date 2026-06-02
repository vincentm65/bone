# Refactor Plan: `src/tools/dynamic.rs` → `src/tools/dynamic/`

## Problem

`src/tools/dynamic.rs` is a 654-line file that owns five unrelated concerns:

1. **YAML schema types** — structs for deserializing tool YAML files
2. **Wire-format types** — structs for deserializing what bash scripts emit
3. **Environment marshalling** — converting tool arguments into env vars
4. **Execution** — running scripts, streaming JSONL, handling contexts
5. **Output parsing** — three different output format parsers, event parsing, pane rendering

Anyone reading the code must hold all five contexts in their head. Anyone editing one risks breaking another. There are no doc comments explaining the contract between bash scripts and Rust.

## Proposed Structure

```
src/tools/dynamic/
├── mod.rs        # Public surface: DynamicTool, Tool impl, load_from_dir
├── schema.rs     # YAML-facing types (what the user writes)
├── wire.rs       # Wire-format types (what bash scripts emit)
├── env.rs        # Argument → env var conversion
├── runner.rs     # Script execution (sync + streaming)
└── parse.rs      # Output format parsers (json, line, jsonl)
```

Each file has one responsibility, clear doc comments, and a bounded scope.

---

## File-by-File Specification

### `schema.rs` — YAML-Facing Types

**Purpose:** Types that describe what a user writes in a `tools/*.yaml` file.

**Contents (~70 lines):**

| Item | Notes |
|---|---|
| `pub struct DynamicTool` | `name`, `version`, `description`, `args`, `script`, `interaction`, `output`, `safety`, `display` |
| `pub struct ToolArg` | `name`, `arg_type`, `description`, `required` |
| `pub enum InteractionType` | `Select` |
| `pub struct OutputConfig` | `kind: OutputKind` |
| `pub enum OutputKind` | `JsonEnvelope`, `LineEnvelope`, `JsonlEvents` |
| `impl DynamicTool` (schema methods) | `validate()`, `build_schema()` |

**Doc comment requirements:**
- Module-level doc explaining the YAML schema users write
- Each struct field gets a `///` doc comment explaining its purpose and YAML key name

**Current lines:** 16–224 (schema + validate + build_schema) → extracted as-is

---

### `wire.rs` — Wire-Format Types

**Purpose:** Types that define the contract between bash script output and Rust. These are the JSON shapes a custom tool script must emit.

**Contents (~120 lines):**

| Item | Notes |
|---|---|
| `struct JsonEnvelope` | `{ content, pane?, state? }` — top-level JSON output |
| `struct PaneEnvelope` | `{ source, title, lines, visible_rows, scroll }` — pane metadata from script |
| `enum PaneLineDef` | `Text(String)` or `Styled { spans: Vec<PaneSpanDef> }` |
| `struct PaneSpanDef` | `{ text, fg?, modifiers[] }` — a single styled span |
| `fn parse_color(name: &str) -> Option<Color>` | Named-color lookup |
| `fn default_pane_rows() -> usize` | Default constant |
| `impl PaneLineDef` | `into_line() → Line<'static>` |
| `impl PaneSpanDef` | `style() → Style` |

**Doc comment requirements:**
- Module-level doc explaining the wire contract: what JSON shapes scripts emit, how they map to UI
- `JsonEnvelope` gets a doc with a full JSON example
- `PaneEnvelope` gets a doc with a full JSON example
- `PaneSpanDef` documents supported color names and modifier strings

**Current lines:** 54–141 (wire types + pane helpers) → extracted as-is

---

### `env.rs` — Environment Marshalling

**Purpose:** Convert tool call arguments into shell environment variables.

**Contents (~60 lines):**

| Item | Notes |
|---|---|
| `pub fn arg_to_env_name(name: &str) -> String` | `question` → `TOOL_QUESTION` |
| `pub fn env_value(value: &Value) -> String` | JSON value → shell string |
| `fn push_array_env(env, name, value)` | `TOOL_FOO_0`, `TOOL_FOO_1`, `TOOL_FOO_COUNT` |
| `pub fn build_env(args: &[ToolArg], arguments: &Value, context: Option<&ToolExecutionContext>) -> Vec<(String, String)>` | Assembles full env var list |

**Doc comment requirements:**
- Module-level doc explaining the env var convention (`TOOL_<UPPERCASE_NAME>`, `_JSON` variants, `_COUNT` + `_0`/`_1` for arrays, `TOOL_CALL_ID`, `TOOL_SESSION_STATE`, `BONE_PID`)
- Each function gets a `///` doc with an example

**Current lines:** `arg_to_env_name` (228–233), `env_value` (335–347), `push_array_env` (349–357), `build_env` (299–333) → consolidated

**Change note:** `build_env` currently takes `&self`; it will be refactored to take `args: &[ToolArg]` as a parameter so it doesn't need `self`.

---

### `runner.rs` — Script Execution

**Purpose:** Run bash scripts with proper env vars, timeouts, and streaming support.

**Contents (~130 lines):**

| Item | Notes |
|---|---|
| `pub async fn run(args: &RunArgs) -> Result<ScriptOutput, String>` | Simple script execution |
| `pub async fn run_jsonl(args: &RunArgs, on_line: F) -> Result<ScriptOutput, String>` | Streaming execution with line callback |
| `pub struct RunArgs` | `command`, `env`, `timeout_ms` — replaces the closure-over-self pattern |

**Doc comment requirements:**
- Module-level doc explaining the execution model (bash -lc, timeout clamping, exit code handling)
- `RunArgs` documents each field
- Each function documents error cases (non-zero exit, timeout, spawn failure)

**Current lines:** `run` (359–376), `run_with_context` (378–413), `run_jsonl_events` (253–290), `run_jsonl_events_live` (292–328) → consolidated

**Change note:** These currently live as `impl DynamicTool` methods. They will become free functions that take a `RunArgs` struct. The `DynamicTool` `Tool` impl in `mod.rs` will call these functions, passing the constructed args.

---

### `parse.rs` — Output Format Parsers

**Purpose:** Parse script stdout into `ToolOutput`, supporting three formats.

**Contents (~180 lines):**

| Item | Notes |
|---|---|
| `pub fn parse_json_envelope(stdout: &str) -> Result<ToolOutput, String>` | JSON output format |
| `pub fn parse_line_envelope(stdout: &str) -> Result<ToolOutput, String>` | Line-delimited output format |
| `pub fn parse_jsonl_events(stdout: &str) -> Result<ToolOutput, String>` | JSONL batch output format |
| `pub fn parse_live_event(line: &str) -> Option<ToolLiveEvent>` | Single JSONL line → live event |
| `pub fn pane_page_from_value(value: &Value) -> Option<PanePage>` | JSON value → PanePage |
| `pub fn parse_output(kind: Option<&OutputKind>, stdout: &str) -> Result<ToolOutput, String>` | Dispatcher that picks the right parser |

**Doc comment requirements:**
- Module-level doc explaining the three output formats with examples:
  - `json_envelope`: single JSON object with `content` and optional `pane`
  - `line_envelope`: `@@content@@`/`@@pane@@` delimited text
  - `jsonl_events`: one JSON event per line (`pane`, `text_delta`, `finished`, `failed`)
- Each parser function gets a doc with input/output examples

**Current lines:** 450–610 → extracted as-is

---

### `mod.rs` — Public Surface

**Purpose:** The `DynamicTool` struct and its `Tool` trait implementation. Wires the other modules together. Owns no parsing or execution logic.

**Contents (~150 lines):**

| Item | Notes |
|---|---|
| `pub use schema::*` | Re-export public types |
| `pub use wire::*` | Re-export wire types (needed by parse) |
| `impl Tool for DynamicTool` | `definition()`, `execute()`, `execute_output()`, `execute_output_live()` — delegates to runner/parse |
| `pub fn load_from_dir(dir: &Path) -> Vec<DynamicTool>` | YAML loading + validation |

**The `Tool` impl becomes a thin orchestrator:**
```
execute(arguments)           → runner::run() → parse::parse_output()
execute_output(arguments)    → runner::run() → parse::parse_output()
execute_output_live(...)     → runner::run_jsonl() → parse::parse_output()
                                  or runner::run() → parse::parse_output()
```

**Current lines:** scattered across 157–448 → consolidated into thin delegation

---

## Dependency Graph

```
schema.rs ──────┐
                │
wire.rs ────────┤
                ├──→ mod.rs (DynamicTool + Tool impl) ──→ external
env.rs ─────────┤
                │
runner.rs ──────┤
                │
parse.rs ───────┘   (depends on wire.rs for deserialization types)
```

- `schema.rs` — no internal deps (only serde + serde_json)
- `wire.rs` — no internal deps (only ratatui + serde)
- `env.rs` — no internal deps (only serde_json + types::ToolExecutionContext)
- `runner.rs` — depends on `env.rs` (to build env vars) and `script_runner` (to execute)
- `parse.rs` — depends on `wire.rs` (for deserialization types) and `types` (for ToolOutput, ToolLiveEvent)
- `mod.rs` — depends on all five

No circular dependencies.

---

## Execution Order

| Step | Action | Risk |
|---|---|---|
| 1 | Create `src/tools/dynamic/` directory | None |
| 2 | Move `schema.rs` types out | Pure extraction, no logic change |
| 3 | Move `wire.rs` types out | Pure extraction, no logic change |
| 4 | Move `env.rs` functions out | Refactor `build_env` to not need `&self` |
| 5 | Move `parse.rs` functions out | Pure extraction, no logic change |
| 6 | Move `runner.rs` functions out | Refactor from methods to free functions |
| 7 | Write `mod.rs` as thin orchestrator | Glue code, no new logic |
| 8 | Add doc comments to every module | Documentation only |
| 9 | `cargo check` | Verify no regressions |
| 10 | Delete old `src/tools/dynamic.rs` | Cleanup |

Steps 2–3 and 4–6 can each be done as atomic commits. Step 7 is the only "risky" step since it rewrites the `Tool` impl to delegate instead of own logic.

---

## What Does NOT Change

- The `Tool` trait in `types.rs`
- `ToolRegistry` and `ToolHandler` in `registry.rs`
- `script_runner.rs`
- The YAML schema users write
- The wire format bash scripts emit
- Any external behavior — this is purely internal reorganization

## Lines of Code Estimate

| File | Lines |
|---|---|
| `schema.rs` | ~90 |
| `wire.rs` | ~120 |
| `env.rs` | ~70 |
| `runner.rs` | ~130 |
| `parse.rs` | ~180 |
| `mod.rs` | ~160 |
| **Total** | **~750** (slight increase from doc comments + imports) |

Current: 654 lines in one file. The ~100-line increase is doc comments and `use` statements — the actual logic is identical.
