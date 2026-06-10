# Extension System Streamlining Review

Review of `src/ext/` and the Rust code surrounding it, focused on removing
redundancy while keeping (or improving) current functionality. Items are
ordered roughly by impact. Estimated net reduction across all items:
**~550–650 lines** with no functionality loss and three real bug fixes.

Current inventory (`src/ext/`):

| File              | Lines | Role |
|-------------------|------:|------|
| ctx.rs            | 1354  | `ctx` table passed to tools/commands |
| lua_tool.rs       | 459   | `Tool` impl for Lua-registered tools |
| loader.rs         | 285   | boot, collect tools/commands/snapshots |
| ops_plugins.rs    | 263   | `bone.plugin.*` |
| types.rs          | 258   | ExtensionManager, BootResult, event dispatch |
| snapshots.rs      | 224   | config/theme/keymap snapshots |
| engine.rs         | 180   | VM creation, sandbox, cjson |
| mod.rs            | 119   | seeding, `boot_lua`, `run_lua_files` |
| ops_commands.rs   |  79   | `bone.register_command` |
| ops_tools.rs      |  71   | `bone.register_tool` |
| ops_events.rs     |  44   | `bone.on` |
| event.rs          |  10   | `EventDispatchResult` enum |

---

## 1. Collapse the three `LuaTool` execute paths into one (~−130 lines)

**Problem.** `src/ext/lua_tool.rs` implements all three `Tool` trait methods
(`execute`, `execute_output`, `execute_output_live`), and each one repeats the
same body: fetch the execute fn from the registry key, convert args, build a
13-field `CtxConfig`, call, convert the result (lua_tool.rs:136–316). Nothing
in the codebase ever calls `execute`/`execute_output` on a `LuaTool` —
`ToolRegistry::execute_live` (src/tools/registry.rs:50) only calls
`execute_output_live`, and the trait already provides default fallthroughs
(src/tools/types.rs:107–118).

**Also dead in this file:**
- `LuaExecution` enum has a single variant (lua_tool.rs:18–20).
- `lua_value_to_execution` (lua_tool.rs:319) checks the runtime-op marker and
  then does the *same thing in both branches* — the comment even says
  "Runtime op markers are no longer handled".
- `RUNTIME_OP_KEY` / `runtime_op_key()` in ctx.rs:17–27 exist only to feed
  that dead check.

**Implementation.**
1. Delete the `execute` and `execute_output` bodies. Keep only
   `execute_output_live` plus a one-line `execute`:
   ```rust
   async fn execute(&self, arguments: Value) -> Result<String, String> {
       let ctx = ToolExecutionContext { call_id: String::new(), session_state: None,
           owner: String::new(), cancelled: None, agent_depth: 0,
           tool_call_depth: 0, tool_handler: None };
       self.execute_output_live(arguments, None, ctx).await.map(|o| o.content)
   }
   ```
   (`execute` must exist because it is the trait's required method; nothing
   else needs it for LuaTool, so the trivial delegation is correct. The
   trait's default `execute_output` then also routes through the live path.)
2. Delete `LuaExecution`; make the spawn_blocking closure return
   `Result<ToolOutput, String>` directly. Replace `lua_value_to_execution`
   with a small `lua_value_to_text(name, value) -> Result<String, String>`
   followed by `parse_tool_output(&text)`; a returned table just becomes the
   `format!("{v:?}")` text as today.
3. Delete `runtime_op_key()`/`RUNTIME_OP_KEY` from ctx.rs.

---

## 2. One pane parser, one color parser (~−130 lines, fixes an inconsistency)

**Problem.** The same pane-JSON format is parsed twice and colors are parsed
three times:
- `ctx.rs:1183 pane_from_json` (~100 lines) — used by `ctx.ui.pane` /
  `ctx.emit_pane`. Uses `parse_color` (ctx.rs:1328) which supports named,
  `light*`, and `#RRGGBB`.
- `lua_tool.rs:351 parse_tool_output` re-implements the pane/lines/spans
  walk inline (~60 lines) and uses its own `span_obj_to_style`
  (lua_tool.rs:426, ~34 lines) which supports **named colors only — no hex,
  no light variants**. So a tool that returns the JSON envelope gets fewer
  styling options than one that calls `ctx.emit_pane`. That's a bug.
- `snapshots.rs:151 parse_hex_color` (~32 lines) — uppercase named + hex.

**Implementation.**
1. Make `pane_from_json` `pub(crate)` in ctx.rs (or move it next to
   `PanePage` in `src/ui/pane_page.rs` as `PanePage::from_json` — preferred,
   since it is pure UI deserialization with no Lua dependency).
2. In `parse_tool_output`, replace the entire inline pane walk
   (lua_tool.rs:364–415) with
   `map.get("pane").and_then(|p| pane_from_json(p).ok())`.
3. Delete `span_obj_to_style`.
4. Replace `snapshots.rs::parse_hex_color` with the (extended) shared
   `parse_color`: lowercase the input, accept an optional `#` prefix, keep
   the `light*` names and 6-digit hex. Behavior is a strict superset of both
   current parsers.

---

## 3. Slim `BootResult` to `{ manager, tools }` (~−60 lines)

**Problem.** `BootResult` (types.rs:140–153) carries `commands`,
`config_snapshot`, `theme_snapshot`, `keymap_snapshot` — but
`ExtensionManager` already stores all four with accessors
(types.rs:63–80). `agent_setup` ignores them entirely (agent.rs:340–343),
`handle_tools_command` ignores them (ui/app/mod.rs:1417–1419), and `App::new`
could read them from the manager. `loader::boot` clones everything twice and
its error path builds two full sets of defaults (loader.rs:29–44, 83–98).

**Implementation.**
1. Reduce `BootResult` to `manager` + `tools`.
2. In `loader::boot`, drop the clones; pass values into
   `ExtensionManager::from_arc` once. The engine-failure early-return shrinks
   to `BootResult { manager: ExtensionManager::from_arc(..defaults..), tools: vec![] }`.
3. In `App::new` (ui/app/mod.rs:93–135), bind `let boot = ext::boot(..)` and
   use `boot.manager.theme_snapshot()`, `.config_snapshot()`,
   `.keymap_snapshot()` (clone the keymap into `lua_keymap`).
4. Update the two other destructure sites to `let BootResult { manager, tools } = ...`.

---

## 4. Deduplicate the boot/register/sync block (3 copies → 1) (~−55 lines, fixes a bug)

**Problem.** This identical ~30-line sequence appears three times:
`App::new` (ui/app/mod.rs:105–128), `agent_setup` (agent.rs:349–370), and
`handle_tools_command` "reload" (ui/app/mod.rs:1411–1441):
load_tools → boot → register_lua_tools → collect names →
`sync_tools_from_registry` → enabled fallback →
`ToolHandler::with_enabled_safety_and_display`.

**Bug.** `/tools reload` boots a *new* Lua VM for the tools but throws away
the new `ExtensionManager`, so `self.extensions` (event handlers, commands,
`run_lua_command`'s `lua_handle`) still points at the **old** VM while the
reloaded tools run on the new one. Two VMs stay alive and reloaded
commands/hooks are never picked up.

**Implementation.**
1. Add to `src/ext/mod.rs` (or `tools/mod.rs`):
   ```rust
   pub struct BootedTools { pub manager: ExtensionManager, pub tools: ToolHandler }
   pub fn boot_with_tools(custom: &mut CustomConfigs) -> BootedTools { /* the shared block */ }
   ```
2. Call it from all three sites. In `handle_tools_command`, also assign
   `self.extensions = booted.manager;` — this fixes the stale-VM bug and makes
   `/tools reload` actually reload commands and event handlers too (document
   this in the reply string).

---

## 5. Unify Lua command lookup/dispatch (~−60 lines)

**Problem.** Finding a command in `bone._commands` and resolving its handler
is implemented twice, nearly verbatim:
- `run.rs:164–186` (`expand_lua_command`)
- `ui/app/mod.rs:1102–1126` (`run_lua_command`)

Additionally `run.rs` boots Lua through a *third* boot path:
`ext::boot_lua` + `ext::seed_default_lua_commands` + `ext::run_default_commands`
(mod.rs:77–119), which is a hand-rolled subset of `loader::boot` — it skips
`lua/tools/*.lua`, so a headless `/command` that delegates to a Lua tool via
`ctx.tools.call` behaves differently than in the TUI.

**Implementation.**
1. Add to `ops_commands.rs`:
   ```rust
   pub(crate) fn find_handler(lua: &Lua, name: &str) -> Option<mlua::Function>
   ```
   (iterate `bone._commands`, match `name`, unwrap function-or-table handler —
   the exact logic currently duplicated).
2. Use it in both `run.rs` and `App::run_lua_command`.
3. In `expand_lua_command`, replace `boot_lua`/seed/`run_default_commands`
   with `let boot = ext::boot(&config_dir, &cwd);` and use
   `boot.manager.lua_handle()`. Then delete from `mod.rs`: `boot_lua`,
   `run_default_commands` (and `seed_default_lua_commands` becomes
   loader-internal). `run_lua_files` stays (loader uses it).

---

## 6. Add a `CtxConfig` constructor (~−45 lines)

**Problem.** The 13-field `CtxConfig` literal is written out five times, four
of them mostly `None`/zero: lua_tool.rs:157–171, 207–221, 277–291,
run.rs:192–206, ui/app/mod.rs:1131–1145.

**Implementation.** Add:
```rust
impl CtxConfig {
    pub(crate) fn new(config_dir: String, shared_state: SharedState) -> Self { /* all else None/0/Safe */ }
}
```
Call sites then set only the fields they actually customize
(`pane_sender`, `call_id`, `tool_handler`, `usage`, ...) via struct update or
field assignment. Combined with item 1 this removes most of the literals
outright.

---

## 7. Factor the `loader.rs` collect boilerplate (~−70 lines)

**Problem.** `collect_tools`, `collect_commands`, `collect_config_snapshot`,
`collect_theme_snapshot`, `collect_keymap_snapshot` (loader.rs:102–285) each
repeat: lock the mutex (with poison warning) → get `bone` global → get a
sub-table → default on failure. Five copies of the same scaffold.

**Implementation.** `boot()` already holds the un-wrapped `Lua` before it is
moved into the `Arc` (loader.rs:71). Collect *before* wrapping where possible,
or add one helper:
```rust
fn with_bone<T>(lua: &Lua, f: impl FnOnce(&Lua, &Table) -> T, default: T) -> T
```
Then each collector becomes ~8 lines. The three snapshot collectors are
identical modulo type — a small generic
`fn snapshot<T: Default>(lua: &Lua, key: &str, parse: impl Fn(&Lua, &Table) -> Result<T, String>) -> T`
collapses them to three one-liners. Note `collect_tools` is the only one that
genuinely needs the `Arc` (it clones it into each `LuaTool`), so keep its
signature.

---

## 8. Single source of truth for built-in commands (~−30 lines, fixes a ghost command)

**Problem.** Built-in slash commands are listed in three places that have
already drifted:
- `ui/autocomplete.rs:11 BUILTIN_COMMANDS` (names + descriptions)
- `ui/commands/mod.rs:18 is_protected_builtin` (names)
- `ui/commands/mod.rs:107 help()` (names + descriptions, hand-formatted)

Drift example: **`/compact` is autocompleted and protected but implemented
nowhere** — selecting it prints "Unknown command". Also
`AutocompleteState::combined` (autocomplete.rs:55–62) has zero callers.

**Implementation.**
1. In `ui/commands/mod.rs` define:
   ```rust
   pub const BUILTINS: &[(&str, &str)] = &[ ("clear", "clear chat history"), ... ];
   pub fn is_protected_builtin(cmd: &str) -> bool { BUILTINS.iter().any(|(n, _)| *n == cmd) }
   ```
2. `autocomplete::builtin_commands()` maps over `commands::BUILTINS`; delete
   the local const and the dead `combined()`.
3. `help()` generates its command section from `BUILTINS` (keep the
   shortcuts/pane sections as static text).
4. Remove `compact` from the list (or implement it — but removal matches
   current behavior; the `/recall`-style cleanup commit already removed its
   backend).

---

## 9. Delete dead wrappers and the duplicate `include!` (~−25 lines)

- `tools/mod.rs:17` — `include!(default_lua_tools.rs)` compiles
  `DEFAULT_LUA_TOOLS` a second time; nothing in `tools/` uses it (the only
  consumer is `ext/mod.rs:42`). Delete the include.
- `tools/mod.rs:59 seed_default_lua_tools` is a one-line wrapper; change its
  single caller (`config/mod.rs:160`) to
  `ext::seed_default_lua_tools(&config::bone_dir().join("lua/tools"))` and
  delete the wrapper **and** `ext::lua_tools_dir()` (mod.rs:53–55, also
  single-caller).
- `ExtensionManager::lua()` (types.rs:53–55) has zero callers — delete.
- `seed_default_lua_tools` / `seed_default_lua_commands` (mod.rs:37–72) are
  identical except for the const — fold into
  `fn seed_defaults(dir: &Path, files: &[(&str, &str)])` with two thin
  callers (or call it directly from `loader::boot` with the right const).
- `event.rs` is 10 lines for one enum; move `EventDispatchResult` into
  `types.rs` and delete the file (update `agent.rs:702` import path).

---

## 10. Trust one validation layer for tool registration (~−40 lines)

**Problem.** Tool entries are validated twice: `ops_tools.rs` checks
name/description/parameters/safety/execute at `bone.register_tool()` time, and
`LuaTool::from_entry` (lua_tool.rs:38–116) re-checks every field again at
collection time. Both run during the same boot; both produce stderr warnings.

**Implementation.** Shrink `register_tool` to push the table as-is (optionally
keep only the `name` check so warnings can name the tool), and let
`from_entry` be the single validator — it already produces good per-field
errors and is the layer that actually constructs the tool. Keep the
safety-string interpretation in `from_entry` exactly as-is (unknown →
`Danger`), which is fail-safe.

---

## 11. Derive `Default` in snapshots.rs (~−20 lines)

`LuaConfigSnapshot` (snapshots.rs:19–26) and `LuaThemeSnapshot`
(snapshots.rs:75–93) hand-write `Default` impls that are exactly what
`#[derive(Default)]` produces (all `None` / empty map). Replace both with the
derive, as `LuaKeymapSnapshot` already does.

---

## 12. Bug: the default `/memory` command is broken by the sandbox

`engine.rs:120–121` stubs `io.open` to raise
"not available in bone Lua sandbox; use ctx APIs instead", but the *bundled*
`defaults/lua/commands/memory.lua` calls `io.open` three times (db existence
check, `memory.last_run` read). Every `/memory` invocation errors out
immediately.

**Implementation (pick one):**
- Preferred: rewrite memory.lua to use the ctx APIs that already exist —
  `ctx.fs.is_file(db)` and `ctx.read_file(state_file)` — no Rust change, and
  it dogfoods the extension API the sandbox points users at.
- Alternative: allow read-mode `io.open`. More code, weaker sandbox; not
  recommended.

While editing memory.lua, also note it shells out to `sqlite3` via
`ctx.shell` even though `ctx.session.list/messages` (ctx.rs:675–760) expose
the same data — porting it to `ctx.session.*` removes the external `sqlite3`
dependency and the SQL-string building (it interpolates timestamps into SQL).
That makes the Lua file shorter too.

---

## 13. Small alignments (optional, low effort)

- **`create_event_ctx` vs `ctx.ui`** — event handlers get a minimal ctx with
  only `ui.notify` (types.rs:241–258), with *different* prefix formatting from
  the tool ctx's `ui.notify` (ctx.rs:349). Extract one
  `fn notify_fn(lua) -> LuaResult<Function>` used by both so messages format
  identically (~−10 lines).
- **`ctx.ui.status`** (ctx.rs:362) is `notify` minus the level — keep it (it's
  documented API) but implement it as a call into the same helper.
- **`mod.rs::boot`** is a pure pass-through to `loader::boot`; either inline
  the loader into `mod.rs` or `pub use loader::boot;` (−6 lines).
- **`/stats` (Rust, ui/stats.rs) vs `/usage` (Lua)** — both render token
  usage. Not redundant today (full-screen dashboard vs chat reply), but
  `ctx.usage.snapshot()` already exposes everything the Lua side needs; if you
  later add a Lua pane/full-screen primitive, `/stats` is the next candidate
  to migrate out of Rust. No action now.

---

## What NOT to remove

- **`ops_plugins.rs`** — overlaps nothing; it's the only install/update path.
- **`engine.rs` sandbox + cjson** — load-bearing for every default tool.
- **`ui/commands/mod.rs` handlers** (`model`, `provider`, `clear`) — these
  mutate `Box<dyn LlmProvider>`, the renderer, and persisted provider config;
  Lua's `ctx` has no access to any of that, so they cannot become Lua
  commands without first building a large host-control API (which would be a
  net *increase* in code).
- **The per-agent VM boot in `agent_setup`** — headless agents are separate
  processes/tasks; sharing the TUI VM across threads would require making the
  whole ctx Send-safe. Correct as-is.

## Suggested execution order

1. Items 9, 11, and the dead code in item 1 (pure deletions, zero risk).
2. Item 1 (LuaTool single path) + item 6 (CtxConfig::new) together.
3. Item 2 (pane/color unification) — verify with a Lua tool returning a JSON
   envelope with `fg = "#ff8800"` to confirm the fixed behavior.
4. Item 3 (BootResult) + item 4 (shared boot block, fixes `/tools reload`).
5. Item 5 (command dispatch unification).
6. Items 7, 8, 10.
7. Item 12 (memory.lua fix) — test `/memory` end to end.

After each step: `cargo build && cargo test`, then a manual TUI smoke test
(`/usage`, `/tools reload`, a Lua tool call that emits a pane).
