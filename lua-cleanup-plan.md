# Lua runtime cleanup pass

## Context

A review of the Lua extension system (`core/src/ext/`, ~8,850 lines) found it structurally healthy — no compiler-flagged dead code, no store/reseed remnants — but with a handful of legacy leftovers and duplications worth cleaning. Goal: reduce lines, remove misleading names from superseded designs, and dedup repeated logic, **without changing user-visible behavior or the Lua API** (one approved exception: removing the `ctx.emit_pane` alias).

User decisions (already made):
- Keep `ctx.rs` as a single file; dedup only, no submodule split.
- Remove the legacy `ctx.emit_pane` alias.
- Leave `should_refresh_seeded_lua` heuristics in `mod.rs` as-is.

Honest expectation: net LOC reduction is modest (~80–130 lines). The codebase is terse; the bigger win is removing the misleading "pane channel" naming and the legacy alias.

## Changes

### 1. Remove the legacy `ctx.emit_pane` alias
`ctx.emit_pane` is an identical second registration of the `ctx.ui.pane` closure (both built by `make_pane_emit_fn`). No bundled Lua uses it.
- `core/src/ext/ctx.rs:325-330` — delete the `emit_pane` registration in `create_ctx_table`; simplify `make_pane_emit_fn`'s doc comment (ctx.rs:335-337).
- Update stale comment references to `ctx.emit_pane`: `core/src/ext/api_ui.rs:16,55`, `core/src/ext/types.rs:178,257`, `core/src/ext/ctx.rs:115`, `tui/src/ui/app/mod.rs:907`.
- Update the seeded Lua-authoring doc `core/defaults/AGENTS.md` (lines ~108, ~156, ~509): remove the `emit_pane` rows/alias mentions. Note: seeded AGENTS.md on user machines refreshes per the normal seeding rules; content change here is doc-only.

### 2. Retire the misnamed `ToolLiveEvent` / `pane_sender` (v1 pane-channel remnant)
`ToolLiveEvent` (`core/src/tools/types.rs:45-47`) now has a single variant, `Key(KeyRequest)` — pane content moved to `SharedUi` ("v2"). The channel only carries key requests.
- Delete the `ToolLiveEvent` enum; make the channels carry `crate::pane_content::KeyRequest` directly.
- Rename `pane_sender` → `key_sender` (or `key_events`) on `CtxConfig` (`ctx.rs:113`) and thread the rename/type change through:
  - `core/src/ext/ctx.rs:672-693` (ctx.ui.key) and `ctx.rs:848,909` (nested `ctx.tools.call` pass-through)
  - `core/src/ext/lua_tool.rs:198`
  - `core/src/tools/registry.rs` (`execute_all_live` signature / pass-through)
  - `core/src/runtime/driver.rs:1032` and `core/src/rpc/mod.rs:704,735` — drop the now-pointless single-variant destructuring
  - tests: `core/tests/driver_turn_test.rs:537`, `core/tests/stream_tools_test.rs`
- Behavior identical; this is purely naming/type simplification.

### 3. `ctx.rs` dedups (single file, no split)
- **YAML load duplication**: `ctx.config.get` and `ctx.config.get_table` (ctx.rs:1110-1166) both do path-join → read → `serde_yaml::from_str`. Extract a `load_section_yaml(config_dir: &str, section: &str) -> Result<Option<serde_yaml::Value>, mlua::Error>` helper.
- **Bypassed `block_on` helper**: `ctx.ui.key` (ctx.rs:682) inlines `tokio::task::block_in_place(|| Handle::current().block_on(rx))`; use the existing `block_on` helper (ctx.rs:24). Same inline pattern exists twice in `ops_plugins.rs` (see change 4).
- **Unavailable-stub branches**: the `else` arms for `pane`/`width`/`key`/`usage.snapshot`/`conversation` stubs are small; only consolidate if a helper is a clear win — do not force it.

### 4. `ops_plugins.rs`: extract git helper
`install` (git clone, lines 138-157) and `update` (git pull, lines 247-265) duplicate the block_in_place + `tokio::process::Command` + status-match plumbing. Extract:
```rust
fn run_git(args: &[&str], cwd: Option<&Path>, verb: &str) -> Result<(), mlua::Error>
```
using the shared `block_on` (move it to a shared location, e.g. `pub(crate)` in `ctx.rs`, or duplicate-free via `crate::ext::ctx::block_on`). Also fold the two identical "plugin already exists" checks in `install` into one check after `name`/`dest` are resolved.

### 5. `snapshots.rs`: merge preset parsers
`parse_spinner_presets` and `parse_text_presets` (snapshots.rs:28-87) share the iterate-pairs → require-`name` → collect-string-array skeleton. Extract a generic helper, e.g.:
```rust
fn parse_presets<T>(table: &mlua::Table, kind: &str, build: impl Fn(String, &mlua::Table) -> Option<T>) -> Vec<T>
```
keeping the per-kind warnings ("missing name", "no frames") intact.

### 6. `lua_tool.rs`: annotate migration shim
`normalize_json_schema` (lua_tool.rs:249-266) strips stale boolean `required` fields — versioning code. Add one doc-comment line stating the condition under which it can be deleted (all seeded/catalogue tools migrated). No behavior change.

## Explicitly out of scope
- Splitting `ctx.rs` into submodules (rejected: LOC goal).
- `should_refresh_seeded_lua` manifest rewrite (deferred).
- `ctx.session.current` / `ctx.conversation.current` overlap — user-visible Lua API, keep both.
- `tool_err` / `agent_err` merge — different field sets, semantic separation is fine.
- ops_* registration macro — the per-domain validation differences make a macro a net loss.

## Verification
1. `cargo check --workspace --all-features` and `cargo clippy --workspace --all-features` — must stay warning-free.
2. `cargo test --workspace` — especially `core/tests/lua_api_test.rs`, `lua_tool_nested_test.rs`, `driver_turn_test.rs`, `stream_tools_test.rs`, and the in-file `ctx_tests.rs` / `jobs_tests.rs` / seed tests.
3. Manual smoke via the TUI (`cargo run`): open an interactive pane tool (e.g. `/config` or `/catalogue`) to confirm `ctx.ui.pane` + `ctx.ui.key` still render and accept keys (exercises the renamed key channel and pane path end-to-end).
4. Grep for leftovers: `grep -rn "emit_pane\|ToolLiveEvent\|pane_sender" core/ tui/` should return nothing.
