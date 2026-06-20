# Batch 2 Review — `/home/vincent/projects/bone/src/ext/`

---

## /home/vincent/projects/bone/src/ext/mod.rs
- **Lines:** 368
- **Assessment:** mostly good
- **Notes:** Well-organized module root that declares sub-modules, includes generated code, and exposes public utility functions (`extract_description`, `catalog`, `seed_default_lua_*`, `run_lua_files_*`). The three `seed_default_lua_*` functions are nearly identical (only the constant array varies) — they could be consolidated into a single `seed_from_bundled(items, dir, allow)` helper, but the duplication is acceptable for clarity. `run_lua_files_filtered` and `run_lua_files_selected` are clean. The inline `#[cfg(test)] mod seed_tests` tests are focused. No significant over-engineering.

---

## /home/vincent/projects/bone/src/ext/api.rs
- **Lines:** 277
- **Assessment:** mostly good
- **Notes:** Clean, well-documented module implementing `bone.api.*` (autocmd, emit, submit, keymap, config). The pattern of `ensure_subtable` is repeated (once here, once in `api_ui.rs`) — minor duplication. The `keymap.set`/`del`/`get` functions follow identical boilerplate (get bone, get keymap, ensure mode table). Could be DRY'd with a helper but the API surface is small enough that it's fine. `from_lua` helper in `api_ui.rs` duplicates JSON serde roundtrip logic that could be shared. Test coverage is solid.

---

## /home/vincent/projects/bone/src/ext/api_ui.rs
- **Lines:** 353
- **Assessment:** can be simplified
- **Notes:** The `from_lua` function deserializes Lua values via a JSON serde roundtrip (`lua.from_value` → `serde_json::from_value`). This works around mlua's untagged enum limitations but is fragile and adds overhead. The `lock` / `lock_shared` split is redundant — both call `ui.lock().unwrap_or_else(|e| e.into_inner())`. Could be a single `fn lock(ui: &SharedUi)`. The `ensure_subtable` pattern is duplicated from `api.rs` — a shared utility in `mod.rs` or a helper module would reduce repetition. The number of closures (open_float, set_lines, close, set_statusline, set_highlight, term_width) is appropriate for the API surface size.

---

## /home/vincent/projects/bone/src/ext/ctx.rs
- **Lines:** 2134
- **Assessment:** over-engineered
- **Notes:** This is the largest file in the `ext/` directory by a wide margin and the most complex. Key issues:

  - **Massive size (2134 lines):** Contains 15+ public functions, 20+ private helpers, closures, and 5+ struct definitions. Should be split into focused sub-modules (e.g., `ctx_usage.rs`, `ctx_conversation.rs`, `ctx_agent.rs`, `ctx_ui.rs`, `ctx_db.rs`).
  - **Duplicated table-building pattern:** `build_fs_table`, `build_ui_table`, `build_usage_table`, `build_conversation_table`, `build_state_table`, `build_tools_table`, `build_config_table`, `build_session_table`, `build_db_table` all follow the same pattern (create table, attach closures, return). Each replicates error handling and closure creation boilerplate.
  - **`add_agent_table` is extremely long** — it registers `ctx.agent.spawn`, `ctx.agent.run`, `ctx.agent.run_stream`, `ctx.agent.jobs`, `ctx.agent.cancel` with complex async dispatch, callback streaming, and cancellation logic. This should be its own module.
  - **`parse_agent_opts`** accepts allowed_keys as a slice but the validation (`warn_unknown_opts`) is done manually — could use a builder pattern or derive-based parser.
  - **`StreamCallbacks` struct + `drain_pending`/`dispatch_event`** adds significant complexity for streaming agent results. The event dispatch match arms are verbose.
  - **Duplicate `log` table** — `ctx.log` is built here but `engine.rs` also builds `bone.log`. Two different log tables with slightly different shapes.
  - The `build_config_table` reads YAML config from disk on each call to `get()` — this is a disk read inside a Lua closure, which is unusual.
  - Many `opt_*` helper functions (`opt_str`, `opt_u64`, `opt_usize`, `opts_cb`, `extract_tool_allowlist`, `build_current_fn`) are defined here and also replicated in `ctx_tests.rs` test helpers — the test helpers shadow the real ones with slightly different implementations.

---

## /home/vincent/projects/bone/src/ext/ctx_tests.rs
- **Lines:** 596
- **Assessment:** mostly good
- **Notes:** Comprehensive test coverage for ctx table building, agent opts parsing, usage serialization, tool definition serialization, and AppCtxState parity. The test helper functions (`opt_get`, `tool_call_result`, `make_session_current`, `agent_err_table`, `spawn_err`) are clean. A few observations:
  - `agent_opts_do_not_inherit_model_when_provider_changes` and `agent_opts_inherit_model_when_provider_is_inherited` test the `parse_agent_opts` function which is itself well-covered here.
  - Tests like `usage_context_serializes_with_correct_keys` and `tool_definition_serializes_correctly` serialize Rust structs to Lua and then to JSON for assertion — this is thorough but verbose. Could use snapshot testing.
  - The `AppCtxState` parity tests (`app_ctx_state_apply_to_populates_all_app_fields`, `app_ctx_state_exposes_app_fields_through_lua_ctx`) are valuable regression guards.
  - No significant over-engineering — the test surface matches the complexity of the code under test.

---

## /home/vincent/projects/bone/src/ext/engine.rs
- **Lines:** 329
- **Assessment:** mostly good
- **Notes:** Clean separation: `create_engine` sets up the VM, `run_init` loads init.lua. The sandboxing logic (`sandbox_globals`, `sandbox_table`) is clean and well-documented. The `inject_cjson` function is separate from the engine concerns and could live in its own module (e.g., `ext/cjson.rs`). `create_log_table` duplicates the `log`-table pattern from `ctx.rs` — the two log tables have slightly different signatures (one takes `Value`, the other takes `String`). The `DEFAULT_INIT_LUA` string is large — could be loaded from a file at compile time (already done via `include!` for other Lua content). The `populated_init_lua` / `blank_init_lua` public API is clean.

---

## /home/vincent/projects/bone/src/ext/inbox.rs
- **Lines:** 74
- **Assessment:** mostly good
- **Notes:** Tight, focused module implementing a process-global FIFO submit inbox. Uses `OnceLock<Mutex<VecDeque<String>>>` — correct and simple. The `MAX_INBOX` bound of 256 prevents unbounded growth. `push` and `drain` are both 3-line functions. Can't meaningfully simplify further. Tests are clean and use a test mutex to serialize access to the global singleton.

---

## /home/vincent/projects/bone/src/ext/jobs.rs
- **Lines:** 437
- **Assessment:** can be simplified
- **Notes:** `complete` and `complete_with_tokens` are nearly identical — both compute `status`, `result`, `result_file`, and update the job entry. The only difference is `complete_with_tokens` also sets `token_sent`/`token_received` in the same lock. Could refactor to have `complete` delegate to `complete_with_tokens` with default tokens (or merge into one method). `running_ids` and `running_jobs` return the same filtered set — callers could just call `running_jobs` and extract IDs. The `version` bumping is done manually after every mutation — an inner helper on the locked data would be cleaner. The `wait_for` loop is correct but complex (re-checks cancellation every 100ms). The `Condvar` usage is appropriate. The `spill_result` function and `truncate_for_injection` are well-designed.

---

## /home/vincent/projects/bone/src/ext/jobs_tests.rs
- **Lines:** 371
- **Assessment:** mostly good
- **Notes:** Comprehensive coverage including creation, concurrency caps, cancel, complete, spill-to-file, version bumps, peek/mark-consumed, pruning, truncation, and wait_for (timeout, completion, cancellation, unknown IDs). The `fresh_registry` / `new_job` / `create_default` helpers keep tests concise. Some tests test internal invariants (version bumps on no-change) rather than external contracts. The pruning tests (`pruning_caps_registry_size`, `pruning_keeps_unconsumed_jobs`) are valuable. `wait_for` tests cover all edge cases well.

---

## /home/vincent/projects/bone/src/ext/loader.rs
- **Lines:** 360
- **Assessment:** can be simplified
- **Notes:** `collect_tools`, `collect_commands`, `collect_config_snapshot`, `collect_theme_snapshot`, `collect_keymap_snapshot`, and `collect_subagent_names` all follow the same pattern: (1) lock the Lua mutex, (2) get the `bone` table, (3) get a sub-table from it, (4) iterate/parse entries. This could be unified into a generic `with_bone<F>(lua_arc, f)` helper + parallel `collect_*` functions that share the mutex acquisition logic. The `get_bone` helper already exists but is only used by `collect_*` callers — the error handling (mutex poisoned → return empty) is repeated 6 times. The `boot` function orchestrates everything cleanly but is long — the seeding + snapshot collection sequence could be factored.

---

## /home/vincent/projects/bone/src/ext/loader_tests.rs
- **Lines:** 41
- **Assessment:** mostly good
- **Notes:** Minimal — just 4 tests covering `get_bone` (absent/present) and `collect_subagent_names` (no table/empty list). The rest of the loader's functionality (tool collection, command collection, snapshot collection) is untested at the unit level. Given the boot path is integration-tested elsewhere, this is acceptable but thin. No over-engineering.

---

## /home/vincent/projects/bone/src/ext/lua_tool.rs
- **Lines:** 340
- **Assessment:** can be simplified
- **Notes:** The `run_execute` method has complex mutex management (drop the outer lock before calling Lua, re-acquire after) documented thoroughly. This complexity is inherent to the threading model (non-reentrant `std::sync::Mutex` + reentrant mlua VM mutex). The `execute_output_live` method has two code paths (nested vs top-level) with lengthy comments — the inline-execution path for nested calls could be a separate method. `parse_tool_output` is clean. `normalize_json_schema` is a tiny recursive helper that could be inlined or moved to a utility crate. The `Tool` trait implementation is clean. The `from_entry` builder method is well-structured.

---

## /home/vincent/projects/bone/src/ext/ops_commands.rs
- **Lines:** 114
- **Assessment:** mostly good
- **Notes:** Concise module for `bone.register_command`. The `setup_register_command` function validates arguments (short form: function, long form: table with handler/description) and stores entries. `find_handler` linearly scans `bone._commands` each time — could be a `HashMap<String, Function>` lookup for O(1) dispatch. For the expected small number of commands (tens, not thousands) the linear scan is acceptable. No over-engineering.

---

## /home/vincent/projects/bone/src/ext/ops_events.rs
- **Lines:** 55
- **Assessment:** mostly good
- **Notes:** Very concise. `setup_on` creates `bone._handlers` with 9 pre-seeded event arrays + `bone.on` function. Handles unknown event names by creating handler arrays on demand (autocmd pattern). No simplification needed — this is the right size for its job.

---

## /home/vincent/projects/bone/src/ext/ops_plugins.rs
- **Lines:** 275
- **Assessment:** can be simplified
- **Notes:** `load`, `install` (local + symlink), `install` (git clone), `remove`, `list`, `update` each independently fetch `config_dir` from the `bone` table. A small helper like `fn config_dir(lua, bone) -> String` shared across all closures would reduce repetition. The `install` function handles two completely different workflows (local symlink vs git clone) in one function — splitting into `install_local(path)` and `install_github(repo)` would be clearer. Platform-specific `symlink_plugin_dir` functions (unix/windows) are appropriate. The `update` function performs `git pull` with `block_in_place` — the same async-command pattern is used in multiple places and could be extracted into a utility. The `list` function sorts directory entries and reports `has_init`.

---

## /home/vincent/projects/bone/src/ext/ops_tools.rs
- **Lines:** 82
- **Assessment:** mostly good
- **Notes:** `setup_register_tool` and `setup_register_subagent` follow nearly identical patterns: create storage table, create register function with validation, store on `bone`. These could be merged into a single `setup_register_generic(name, storage_key)` helper that takes the validation function as a parameter. The validation in `setup_register_subagent` is more thorough (checks for empty name, duplicate names). The duplicate-name check iterates all existing entries every time — O(n²) for n registrations. A set of names would be O(1). For the expected small numbers this is fine.

---

## /home/vincent/projects/bone/src/ext/snapshots.rs
- **Lines:** 244
- **Assessment:** can be simplified
- **Notes:** `parse_spinner_presets` and `parse_text_presets` have nearly identical structure (iterate table pairs, get name, get inner table, parse items, skip malformed). Could be unified into a generic `parse_presets<T>(table, parse_fn)` or a single function with a flag. The `collect_presets` function calls `require("ui.spinners")` at boot time — if the module is missing, it silently returns empty vecs (fine). `LuaConfigSnapshot::from_lua_table` reads `approval_mode` and `status_show` but not `spinners`/`texts` (those are injected after construction in `loader.rs`). This split is confusing — either `from_lua_table` should include spinner/text parsing, or `collect_presets` should be inlined at the call site. `LuaThemeSnapshot` and `LuaKeymapSnapshot` are clean. The `LuaKeyBinding` struct could be a tuple `(String, String)` but the named fields improve clarity.

---

## /home/vincent/projects/bone/src/ext/types.rs
- **Lines:** 859
- **Assessment:** over-engineered
- **Notes:** This file mixes too many concerns:

  - **Boot types:** `BootOptions`, `BootResult`, `BootedTools` — fine.
  - **Event dispatch:** `EventDispatchResult`, `dispatch_event_inner`, `guard_with_bone` — overlaps with `ops_events.rs` and `ctx.rs`.
  - **Return/action types:** `LuaReturnAction`, `ConversationLoad`, `ConfigAction`, `LuaCommandReturn` — fine but could be in their own module.
  - **ExtensionManager (300+ lines):** 18 public methods including `dispatch_simple`, `dispatch_tool_call`, `dispatch_tool_result`, `dispatch_before_turn` — this is the central orchestrator but also contains dispatch logic that duplicates patterns in `ops_events.rs`.
  - **`parse_lua_return_action` (100+ lines):** Parses action tables with a long match on action names. The `conversation.replace` and `conversation.load` branches are nearly identical (both parse messages from a `messages` field).
  - **`parse_messages_table` (50+ lines):** Converts Lua message arrays to `ChatMessage` — shared by two action branches but defined only here.
  - **`lua_value_to_json` (50+ lines):** Manual Lua→JSON conversion. mlua already has `LuaSerdeExt::from_value` — this duplicates that functionality with custom array detection logic.
  - **`parse_lua_command_return` (30+ lines):** Normalizes command handler return values — fine but could be in `ops_commands.rs`.
  - The `#[cfg(test)] mod tests` section adds another ~100 lines of tests directly in the file.

  Recommended refactor:
  1. Move `parse_lua_return_action`, `parse_messages_table`, `ConversationLoad`, `ConfigAction` into a new `ext/actions.rs`.
  2. Move `dispatch_event_inner`, `guard_with_bone`, `create_event_ctx` into `ext/ops_events.rs` (which already owns event dispatch).
  3. Move `lua_value_to_json` into a shared utility (or use `LuaSerdeExt::from_value` instead).
  4. Move `parse_lua_command_return` into `ext/ops_commands.rs`.
  5. Keep only `BootOptions/Result`, `EventDispatchResult`, `ExtensionManager`, and `BootedTools` in this file (would cut ~400 lines).
