# Plan: Get bone under 5,000 lines (excluding tests)

**Goal:** Every line of test code removed from source files. Non-test code ≤ 5,000 lines.
**Current:** 5,374 lines in 48 `.rs` files (excluding `*tests.rs` files and `tests/` directory).

---

## Phase 1 — Extract all inline test code (~75 lines saved)

### 1a. `src/tools/handler.rs` — move inline test module (~59 lines)
The only substantial inline test block in the codebase. It's a 59-line
`#[cfg(test)] mod tests { ... }` (lines 23–81). Only uses public API
(`ToolHandler::new`, `ToolRegistry::new`, `execute_all`), so it can move
straight to `tests/integration_test.rs`.

**Action:**
- Cut lines 23–81 from `handler.rs`
- Append the test (adapted for crate-level access) to `tests/integration_test.rs`

### 1b. Remove `#[cfg(test)] mod tests;` declarations (~16 lines)
8 files have a 2-line `#[cfg(test)] mod tests;` at the bottom. These are
glue that pulls in separate `tests.rs` files that access private internals
(`use super::*`, `use super::...` with private fns/structs).

To remove them, the corresponding `tests.rs` files must become integration
tests. This requires making their accessed internals `pub(crate)`:

| Test file | Private items used |
|-----------|-------------------|
| `tools/edit_file/tests.rs` | `sha256_hex` (private fn) |
| `llm/providers/openai_compat/tests.rs` | `use super::*` (many internals) |
| `tools/command_policy/tests.rs` | `classify_command` (private fn) |
| `llm/token_tracker/tests.rs` | `use super::*` (struct fields) |
| `tools/write_file/tests.rs` | `WriteFileTool` (already pub) |
| `tools/approval/tests.rs` | `ApprovalMode` (already pub) |
| `llm/providers/codex/codex_provider/tests.rs` | `use super::*` |
| `ui/render/wrap/tests.rs` | `use super::*` |

**Action:**
- Make needed items `pub(crate)` in their parent modules
- Move each `tests.rs` content into `tests/` directory
- Delete the 2-line `#[cfg(test)] mod tests;` from each source file
- Delete the old `tests.rs` files

---

## Phase 2 — Consolidate overly-split modules (~60 lines saved)

### 2a. Merge command files into `commands/mod.rs` (~25 lines)
7 files for ~174 lines total. Each command is tiny; the file-per-command
split adds module declarations and use-statement overhead.

**Files to merge:**
- `clear.rs` (37) — `/clear` handler
- `context.rs` (13) — `/context` handler
- `help.rs` (11) — `/help` handler
- `model.rs` (3) — `/model` handler
- `quit.rs` (3) — placeholder comment
- `provider_switch.rs` (47) — `/provider` handler
- `mod.rs` (60) — dispatch + types

**Action:** Move all function bodies into `mod.rs`, delete sub-files,
remove the `mod` declarations. Estimated result: ~150 lines in one file.

### 2b. Merge config sub-files into `config/mod.rs` (~15 lines)
`paths.rs` (25) + `app_config.rs` (31) + `seed.rs` (41) are small helpers.
They can live in `mod.rs` alongside the `load_yaml` helper.

**Action:** Move contents into `mod.rs`, flatten `pub use` to inline definitions,
delete sub-files. Estimated result: ~100 lines.

### 2c. Inline `codex/auth.rs` into `codex_provider.rs` (~5 lines)
`auth.rs` (30) is only used by `codex_provider.rs`. Move the two functions
to the bottom of `codex_provider.rs`, delete `auth.rs`, remove `mod auth;` + `use super::auth`.

### 2d. Merge `tools/approval/mod.rs` into `tools/mod.rs` (~15 lines)
`approval/mod.rs` (47) is the `ApprovalMode` enum. Move it into `tools/mod.rs`
or `tools/types.rs`, flatten the directory.

---

## Phase 3 — Tighten verbose code (~130 lines)

### 3a. `tools/edit_file/mod.rs` (559 → ~530) (~30 lines)
The `ToolDefinition` has a ~45-line JSON schema with very verbose
`description` fields. Tighten prose without losing LLM-required semantics.

### 3b. `tools/command_policy/mod.rs` (341 → ~316) (~25 lines)
51 lines of doc comments. Many are multi-line for simple functions.
Condense to one-liners where possible.

### 3c. `ui/render/banner.rs` (102 → ~87) (~15 lines)
The `lines()` function can be compacted — the two "row" blocks have
repeated structure. Extract a helper for the left/right padded line.

### 3d. `ui/input.rs` (319 → ~304) (~15 lines)
38 lines of comments. Section dividers like `// ── Unicode-safe cursor helpers ──`
and verbose method docs can be shortened.

### 3e. `ui/render/bottom_pane.rs` (219 → ~204) (~15 lines)
Cursor-splitting logic (`before`/`at_cursor`/`after`) is duplicated in
`desired_height` and `draw_bottom_pane_with_tick`. Extract a helper.

### 3f. `llm/providers/codex/codex_provider.rs` (417 → ~402) (~15 lines)
44 blank lines + verbose struct docs. Tighten.

### 3g. `llm/providers/openai_compat/mod.rs` (385 → ~370) (~15 lines)
48 blank lines + 35 comment lines. Tighten.

---

## Phase 4 — Structural merges (~60 lines)

### 4a. Merge `tools/registry.rs` + `tools/handler.rs` (~30 lines)
Handler is a thin wrapper around Registry (22 lines after test removal).
Move `ToolHandler` into `registry.rs` or vice versa. Delete one file.

### 4b. Flatten `llm/token_tracker/` → `llm/token_tracker.rs` (~10 lines)
It's 76 lines in its own directory. No sub-modules besides tests. Make it
a flat file.

### 4c. Flatten `tools/write_file/` → `tools/write_file.rs` (~10 lines)
Same pattern: 89 lines in a directory. Make it a flat file.

### 4d. Merge `ui/render/streaming.rs` (53) into `ui/render/mod.rs` (~10 lines)
The streaming helper is only used by the renderer. Inline it.

---

## Summary

| Phase | Description | Est. savings |
|-------|-------------|--------------|
| 1a | Extract handler.rs inline test | 59 |
| 1b | Remove `#[cfg(test)] mod tests;` (8 files) | 16 |
| 2a | Merge command files | 25 |
| 2b | Merge config files | 15 |
| 2c | Inline codex auth | 5 |
| 2d | Merge approval into tools | 15 |
| 3a | Compact edit_file description | 30 |
| 3b | Compact command_policy comments | 25 |
| 3c | Compact banner | 15 |
| 3d | Compact input comments | 15 |
| 3e | Extract bottom_pane cursor helper | 15 |
| 3f | Compact codex_provider | 15 |
| 3g | Compact openai_compat | 15 |
| 4a | Merge registry + handler | 30 |
| 4b | Flatten token_tracker | 10 |
| 4c | Flatten write_file | 10 |
| 4d | Merge streaming into render | 10 |
| **Total** | | **~325** |

**Projected result:** 5,374 − 325 ≈ **5,049 lines**

Still ~50 short of 5,000. A second pass of comment/whitespace tightening
across remaining midsize files (`ui/app/stream.rs`, `ui/app/mod.rs`,
`ui/render/messages.rs`, `ui/render/bottom_pane.rs`) should close the gap.

---

## Files that stay as-is

The following are already tight and well-factored — no changes needed:
- `src/main.rs` (27), `src/lib.rs` (5)
- `src/chat/message.rs` (55), `src/chat/history.rs` (13), `src/chat/mod.rs` (5)
- `src/llm/prompts.rs` (36), `src/llm/provider.rs` (211), `src/llm/mod.rs` (7)
- `src/tools/bash.rs` (124), `src/tools/read_file.rs` (68)
- `src/ui/prompt.rs` (56), `src/ui/theme.rs` (36), `src/ui/mod.rs` (7)
- `src/ui/render/wrap/mod.rs` (111)
- `src/ui/render/messages.rs` (187) — may need light comment trim only
