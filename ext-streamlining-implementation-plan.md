# Extension Streamlining Implementation Plan

This is the implementation plan for the review in [`ext-streamlining.md`](./ext-streamlining.md). Use that document as the source of truth for rationale, detailed findings, and original item numbering. This plan only defines a safe step-by-step execution order.

## Step 0: Baseline

Goal: make sure the current tree builds/tests before touching extension cleanup.

```bash
cargo build
cargo test
```

Capture current grep baseline for known issues:

```bash
grep -R "io.open" -n defaults/lua
grep -R "compact" -n src defaults/AGENTS.md
grep -R "boot_lua\|run_default_commands\|seed_default_lua_commands" -n src
```

## Step 1: Fix obvious user-visible bugs first

### 1A. Fix `/memory` sandbox breakage

Corresponds to `ext-streamlining.md` item 12.

Change `defaults/lua/commands/memory.lua`:

- Replace `io.open(db, "r")` existence check with `ctx.fs.is_file(db)`.
- Replace `io.open(state_file, "r")` with `ctx.read_file(state_file)` or equivalent existing API.
- Keep behavior otherwise unchanged.

Verification:

```bash
grep -R "io.open" -n defaults/lua/commands/memory.lua
cargo build
cargo test
```

Manual check:

```text
/memory
```

Expected: no sandbox `io.open` error.

### 1B. Remove ghost `/compact`

Corresponds to `ext-streamlining.md` item 8.

Single source of truth comes later, but first remove the broken command from current lists:

- `src/ui/autocomplete.rs`
- `src/ui/commands/mod.rs`
- `defaults/AGENTS.md` if it documents protected built-ins

Verification:

```bash
grep -R '"/compact"\|"compact"' -n src/ui defaults/AGENTS.md
cargo build
cargo test
```

Expected: no `/compact` entry in built-in command lists. Ignore unrelated `auto_compact_*` config and number formatting helpers.

## Step 2: Add `CtxConfig::new`

Corresponds to `ext-streamlining.md` item 6.

Goal: reduce risk before touching LuaTool and command dispatch.

In `src/ext/ctx.rs`, add a constructor that fills default/inert values for every field except `config_dir` and `shared_state`.

Then update call sites:

- `src/ext/lua_tool.rs`
- `src/run.rs`
- `src/ui/app/mod.rs`

Use field assignment for only the customized fields, such as `call_id`, `pane_sender`, `tool_handler`, `usage`, and depth fields.

Verification:

```bash
grep -R "CtxConfig {" -n src/ext src/run.rs src/ui/app/mod.rs
cargo build
cargo test
```

Expected: direct struct literals mostly gone or only inside the constructor.

## Step 3: Collapse `LuaTool` execution paths

Corresponds to `ext-streamlining.md` item 1.

Goal: one execution implementation.

In `src/ext/lua_tool.rs`:

- Keep `execute_output_live` as the real implementation.
- Make `execute` delegate through `execute_output_live` with an inert `ToolExecutionContext`.
- Either let the trait default handle `execute_output`, or implement `execute_output` as a direct live delegation if structured output should be preserved for non-live callers.
- Delete `LuaExecution`.
- Delete `lua_value_to_execution`.
- Replace it with a small value-to-text conversion followed by `parse_tool_output(&text)`.

In `src/ext/ctx.rs`:

- Delete `RUNTIME_OP_KEY`.
- Delete `runtime_op_key()`.

Verification:

```bash
grep -R "LuaExecution\|lua_value_to_execution\|RUNTIME_OP_KEY\|runtime_op_key" -n src
cargo build
cargo test
```

Manual checks:

```text
/usage
```

Also call a Lua tool such as `ask_user` or `task_list`.

## Step 4: Unify pane/color parsing

Corresponds to `ext-streamlining.md` item 2.

Goal: one parser for panes and colors.

Preferred shape:

- Move `pane_from_json` out of `ctx.rs` into `src/ui/pane_page.rs`, e.g. `PanePage::from_json`.
- Move shared color parsing somewhere reusable.
- Have `ctx.emit_pane` / `ctx.ui.pane` use the shared parser.
- Have `lua_tool.rs::parse_tool_output` use the shared parser for the `pane` field.
- Have `snapshots.rs` use the shared color parser.
- Delete `span_obj_to_style`.
- Delete duplicate `parse_hex_color`.

Verification:

```bash
grep -R "span_obj_to_style\|parse_hex_color\|fn pane_from_json" -n src
cargo build
cargo test
```

Manual Lua tool check:

- Return a JSON envelope with a pane span using `fg = "#ff8800"`.
- Expected: color works through returned JSON envelope, not only through `ctx.emit_pane`.

## Step 5: Slim `BootResult`

Corresponds to `ext-streamlining.md` item 3.

Goal: remove duplicate snapshot/command carrying.

In `src/ext/types.rs`, reduce `BootResult` to:

- `manager`
- `tools`

Remove from `BootResult`:

- `commands`
- `config_snapshot`
- `theme_snapshot`
- `keymap_snapshot`

Update `src/ext/loader.rs` and call sites:

- `src/agent.rs`
- `src/ui/app/mod.rs`
- `/tools reload` path in `src/ui/app/mod.rs`

Use `ExtensionManager` accessors instead:

- `boot.manager.config_snapshot()`
- `boot.manager.theme_snapshot()`
- `boot.manager.keymap_snapshot()`

Verification:

```bash
grep -R "BootResult {" -n src
grep -R "config_snapshot.*BootResult\|theme_snapshot.*BootResult\|keymap_snapshot.*BootResult\|commands.*BootResult" -n src
cargo build
cargo test
```

## Step 6: Introduce shared boot/register/sync helper

Corresponds to `ext-streamlining.md` item 4.

Goal: fix stale VM on `/tools reload`.

Add a helper, probably in `src/ext/mod.rs` or `src/tools/mod.rs`, that performs the shared sequence:

1. Load configured tools.
2. Boot extensions.
3. Register Lua tools into the registry.
4. Collect registry names.
5. Sync custom tool config from registry.
6. Apply enabled fallback.
7. Construct `ToolHandler` with safety and display overrides.

Suggested shape:

```rust
pub struct BootedTools {
    pub manager: ExtensionManager,
    pub tools: ToolHandler,
}
```

Replace duplicated blocks in:

- `App::new`
- `agent_setup`
- `/tools reload`

Important reload fix:

```rust
self.extensions = booted.manager;
self.tools = booted.tools;
```

Update reload message to indicate commands/hooks are reloaded too.

Verification:

```bash
grep -R "sync_tools_from_registry" -n src
cargo build
cargo test
```

Expected: one shared path, not three copies.

Manual checks:

1. Change a Lua command implementation.
2. Run `/tools reload`.
3. Invoke the command.
4. Expected: new behavior is used.

Also test:

```text
/usage
/tools reload
/usage
```

## Step 7: Unify Lua command lookup/dispatch

Corresponds to `ext-streamlining.md` item 5.

Goal: remove duplicated command lookup and make TUI/headless behavior consistent.

In `src/ext/ops_commands.rs`, add a helper:

```rust
pub(crate) fn find_handler(lua: &Lua, name: &str) -> Option<mlua::Function> {
    // existing lookup behavior from run.rs and ui/app/mod.rs
}
```

Use it in:

- `expand_lua_command`
- `App::run_lua_command`

Then change `expand_lua_command` to use full extension boot instead of the hand-rolled command-only boot path.

After that, remove from `src/ext/mod.rs` if unused:

- `boot_lua`
- `run_default_commands`

Make `seed_default_lua_commands` loader-internal if nothing else uses it.

Verification:

```bash
grep -R "boot_lua\|run_default_commands\|find_handler" -n src
cargo build
cargo test
```

Manual checks:

- TUI `/usage`.
- Headless slash command path, if supported by CLI.
- A Lua command that calls a Lua tool through `ctx.tools.call`.

## Step 8: Factor loader collection boilerplate

Corresponds to `ext-streamlining.md` item 7.

Goal: cleanup after boot paths are stable.

In `src/ext/loader.rs`, add a helper for accessing the `bone` table and defaulting on failure.

Use it to reduce:

- `collect_commands`
- `collect_config_snapshot`
- `collect_theme_snapshot`
- `collect_keymap_snapshot`

Keep `collect_tools` separate if it needs the `Arc<Mutex<Lua>>` to create `LuaTool`.

Verification:

```bash
cargo build
cargo test
```

Manual TUI smoke test after this step.

## Step 9: Single source of truth for built-in commands

Corresponds to `ext-streamlining.md` item 8.

Goal: prevent future drift.

In `src/ui/commands/mod.rs`, define one built-in command list:

```rust
pub const BUILTINS: &[(&str, &str)] = &[
    ("help", "show help"),
    // ...
];
```

Then:

- `is_protected_builtin()` checks `BUILTINS`.
- `help()` generates the command list from `BUILTINS`.
- `src/ui/autocomplete.rs` maps from `commands::BUILTINS`.
- Delete local `BUILTIN_COMMANDS`.
- Delete unused `AutocompleteState::combined()` if still unused.

Verification:

```bash
grep -R "BUILTIN_COMMANDS\|combined(" -n src/ui
cargo build
cargo test
```

Manual check:

```text
/help
```

Expected: help and autocomplete agree.

## Step 10: Delete dead wrappers and duplicate includes

Corresponds to `ext-streamlining.md` item 9.

Remove if unused:

- Duplicate `include!(default_lua_tools.rs)` from `src/tools/mod.rs`.
- `tools::seed_default_lua_tools` wrapper.
- `ext::lua_tools_dir()`.
- `ExtensionManager::lua()`.
- Move `EventDispatchResult` from `event.rs` into `types.rs`; delete `event.rs`.

Optionally fold `seed_default_lua_tools` and `seed_default_lua_commands` into shared default-seeding logic.

Verification:

```bash
grep -R "default_lua_tools.rs\|seed_default_lua_tools\|lua_tools_dir\|fn lua(&self)\|EventDispatchResult" -n src
cargo build
cargo test
```

## Step 11: Trust one validation layer for Lua tools

Corresponds to `ext-streamlining.md` item 10.

Goal: reduce duplicated validation.

In `src/ext/ops_tools.rs`:

- Keep `_tools` array setup.
- Make `register_tool` push the table as-is.
- Optionally keep name extraction only for better warning messages.
- Let `LuaTool::from_entry` validate description, parameters, safety, and execute.

Verification:

```bash
cargo build
cargo test
```

Manual check:

- Temporarily create a malformed Lua tool.
- Confirm the warning is still clear.
- Remove the malformed tool.

## Step 12: Derive snapshot defaults

Corresponds to `ext-streamlining.md` item 11.

In `src/ext/snapshots.rs`, replace manual `Default` impls with `#[derive(Default)]` for:

- `LuaConfigSnapshot`
- `LuaThemeSnapshot`

Verification:

```bash
grep -R "impl Default for LuaConfigSnapshot\|impl Default for LuaThemeSnapshot" -n src/ext/snapshots.rs
cargo build
cargo test
```

## Step 13: Final cleanup and smoke test

Run:

```bash
cargo fmt
cargo build
cargo test
grep -R "LuaExecution\|RUNTIME_OP_KEY\|boot_lua\|run_default_commands\|BUILTIN_COMMANDS\|parse_hex_color\|span_obj_to_style\|io.open" -n src defaults/lua
```

Manual TUI smoke test:

```text
/usage
/help
/tools reload
/usage
/stats
/memory
```

Manual Lua behavior checks:

- A Lua command still runs.
- A Lua tool still runs.
- `/tools reload` picks up changed Lua command behavior.
- A Lua tool returning a JSON pane with hex color renders correctly.

## Suggested commit grouping

1. Fix `/memory` sandbox usage.
2. Remove ghost `/compact`.
3. Add `CtxConfig::new`.
4. Collapse `LuaTool` execution.
5. Unify pane/color parsing.
6. Slim `BootResult`.
7. Add shared boot-with-tools helper and fix `/tools reload`.
8. Unify Lua command lookup/headless boot.
9. Loader cleanup.
10. Built-in command single source of truth.
11. Dead wrapper/include cleanup.
12. Tool validation cleanup.
13. Snapshot default derives.
