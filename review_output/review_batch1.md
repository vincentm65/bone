# Batch 1 Review — `/home/vincent/projects/bone/src/`

## 1. `/home/vincent/projects/bone/src/agent.rs`
- **Lines:** 557
- **Assessment:** can be simplified
- **Notes:**
  - `SessionWriter` implements `SessionSink` by manually delegating each method body to the free functions, which themselves repeat the same `SessionDb::open()` + `match` pattern in every method. There are 5 methods each with the same 7-line error-handling block. This is a lot of repetitive boilerplate for what could be a single `SessionDb::open()` call at the call-site or a `db: SessionDb` field on `SessionWriter` reused across calls.
  - `SessionWriter` opens a new DB connection on every single append/record/end call (calls `SessionDb::open`). This is expensive and seems like an oversight — the struct could hold a `SessionDb` directly (or a `Connection`). The comment says "stores only Send data so headless agent futures can run concurrently", but the trait methods take `&self` anyway, so a connection could be wrapped in `Arc<Mutex<>>` or use a connection pool.
  - `emit_event` is 75 lines of pattern-matching that builds JSON structs by hand. Every variant rebuilds a `serde_json::json!({...})` literal. If `RuntimeEvent` already derives `Serialize`, a single `serde_json::to_string(&event)` or a custom `#[serde(tag="type")]` approach would eliminate all this manual mapping. Some variants are skipped (`TextDelta`, `ReasoningDelta`, `KeyRequest`) — this logic split is brittle.
  - `resolve_provider` mixes config mutation (setting last_provider, overriding model) with provider construction. The function both reads config and writes it (via `custom.set_last_provider`), which is a side-effect buried inside a "resolve" function.
  - `AgentSetup` struct has 9 fields that are immediately destructured and handed to `Driver`. Could `Driver::new(...)` accept `AgentRequest` directly and call the setup internally, keeping the noise out of agent.rs.
  - `summarize_call_args` has a hard-coded match on tool names that duplicates knowledge from the tools registry. A generic "best field" heuristic (e.g. first string field) would avoid coupling to tool names.

## 2. `/home/vincent/projects/bone/src/chat.rs`
- **Lines:** 89
- **Assessment:** mostly good
- **Notes:**
  - Clean, focused file. `build_chat_history` is a straightforward prepend of system prompt.
  - `Message` struct with helpers is well-factored.
  - `ToolDisplay` is unused in this file (referenced only in TUI code). Could move closer to where it's consumed, but not a big deal.
  - No over-engineering here.

## 3. `/home/vincent/projects/bone/src/lib.rs`
- **Lines:** 15
- **Assessment:** mostly good
- **Notes:**
  - Minimal module re-export file. Nothing to simplify. The only note is that `pane_content` and `session_sink` are public modules, while `session_db` and `shell_split` are also public — could audit which truly need `pub`.

## 4. `/home/vincent/projects/bone/src/main.rs`
- **Lines:** 475
- **Assessment:** can be simplified
- **Notes:**
  - `DEPS` auto-install logic adds ~100 lines for a feature (one-time dependency installation) that could be a shell script or a note in README. The `try_install` function handles 4 Linux package managers, macOS Homebrew, Windows winget, and a uv fallback installer. This is significant maintenance burden for a helper that runs once per user.
  - `ensure_deps` uses a sentinel file (`~/.bone-rust/.deps-warned`) to run only once, which is a reasonable optimization — but if this is truly a one-time concern, it doesn't need to live in the Rust binary at all.
  - `install_dir` + `do_install` (~60 lines) duplicates what `cargo install` already does. The user can symlink manually.
  - `run_serve` and `run_connect` (~70 lines combined) are server-mode entry points. They duplicate the config-loading logic from the TUI path (`CustomConfigs::load()`, `derive_providers_config()`, provider creation). There is a general pattern of config-loading being duplicated across `main.rs`, `agent.rs` (in `agent_setup`), and `run.rs`.
  - `parse_cli_options` is a hand-rolled argument parser (14 lines) — fine for 2 flags, but the pattern repeats in `run.rs`'s `parse_run_args` (which handles 6 flags). Could use `clap` or `bpaf` to eliminate manual index tracking and error messages.
  - The top-level `main` function is a single long sequence of `if` blocks that dispatch to different modes. This is clear but would benefit from a `match` on `args.first()` early, exiting early per branch, to reduce nesting.

## 5. `/home/vincent/projects/bone/src/run.rs`
- **Lines:** 221
- **Assessment:** can be simplified
- **Notes:**
  - `run_headless` builds two identical `AgentRequest` structs (expanded Lua command path vs. direct path). The only difference is the `prompt` field. The two branches could be collapsed: compute `prompt` first, then build the request once.
  - `expand_lua_command` is a 50-line function that boots a full Lua VM (`boot_with_tools`) to check if a command exists. This is heavy for a simple "is there a `/command` handler?" check. Could use a lighter lookup (e.g. scan a commands directory or use a cached registry) and only boot Lua when the command actually runs.
  - `expand_lua_command` spawns `spawn_blocking` for Lua execution, which is good for not blocking tokio, but the function returns `Option<String>` while the blocking work is a tokio task that could fail silently. If the tokio task panics or is cancelled, the expansion silently returns `None` and falls through to direct execution — could cause confusing behavior.
  - `parse_run_args` is a hand-rolled parser for 6 flags. Same duplication concern as `main.rs::parse_cli_options`. A shared arg-parser module or a crate would simplify both.
  - `parse_approval` is simple and well-factored.

## 6. `/home/vincent/projects/bone/src/pane_content.rs`
- **Lines:** 217
- **Assessment:** mostly good
- **Notes:**
  - The `deserialize_vec_or_empty_map` deserializer handles the Lua `{}` ambiguity (empty table serializes as `{}` not `[]`). This is a real interoperability concern. Well-documented.
  - The leniency (silently skipping non-deserializable elements) is well-documented and matches previous behavior. Acceptable for a Lua-interop boundary.
  - Test coverage is thorough (5 test cases covering edge cases: numbers, bad types, empty objects, null, happy path).
  - `KeyRequest` with the `oneshot::Sender` is clean.
  - No over-engineering. Could potentially simplify by removing the `visible_rows`/`scroll` defaults and the `is_empty()` convenience method (since callers could check `lines.is_empty()` directly), but these are minor.

## 7. `/home/vincent/projects/bone/src/session_db.rs`
- **Lines:** 856
- **Assessment:** can be simplified
- **Notes:**
  - This is the largest file in the review. The schema migration logic (`setup_schema`) uses a step-by-step `if version == N { ... }` chain from v1 to v4. Migrations are manual ALTER TABLE statements. This is fine for now (4 versions), but does not scale well. Could use a migration library (`refinery`, `diesel_migrations`) or a simpler "drop and recreate" approach for non-production databases.
  - The `FULL_SCHEMA` constant (the full latest CREATE TABLE statements) is maintained in parallel with the migration steps. This duplication is a common source of drift — a new column could be added to `FULL_SCHEMA` without a corresponding `ALTER TABLE` in the migration chain (or vice versa). A single source of truth would be safer.
  - `usage_stats_snapshot()` fetches 14 independent queries in sequence (total, by_model_today, by_model_7d, by_model_4w, by_model_all, daily, weekly, monthly, all_time, hourly_today, hourly_7d, hourly_4w, hourly_all, daily_activity). Many of these are similar queries with different time windows. This works but is chatty — could batch or use a single query with time-window grouping.
  - The 4 `usage_bucket` query methods (`usage_today_by_hour`, `usage_recent_days`, `usage_recent_weeks`, `usage_buckets`) share almost identical structure (CTE generate series, LEFT JOIN with usage aggregation). Could be refactored into one parameterized method.
  - `UsageStatsSnapshot` struct has 16 fields — it's a view-model for the stats dashboard, which is appropriate, but it means any change to the stats UI requires modifying both the struct and the query methods.
  - `TimeWindow` enum with `clause()` method is clean. Good use of bound parameters to avoid SQL injection.
  - `ViewMode` enum is duplicated between `session_db.rs` and presumably the stats UI. Could live in a shared module.
  - The `civil_from_days` / `iso_from_unix_secs` helpers are a manual ISO date formatter (~20 lines). Rust's `chrono` crate would replace this with `chrono::DateTime::from_timestamp(secs, 0).unwrap().to_rfc3339()` which is standard and tested. The comment says "Howard Hinnant's civil-from-days algorithm" which is impressive but unnecessary when the ecosystem provides this.
  - Overall, the file is well-structured but carries a lot of query boilerplate.

## 8. `/home/vincent/projects/bone/src/session_db_tests.rs`
- **Lines:** 223
- **Assessment:** mostly good
- **Notes:**
  - Tests are thorough: migration preservation, legacy version rejection, fresh database init, seq tracking, date formatting, tool_calls round-trip.
  - Test for `legacy_unversioned_database_is_not_stamped_current` is solid — validates that the safety check works.
  - `tool_calls_roundtrip` test proves the v3→v4 migration works end-to-end.
  - No simplification needed. Could add more edge cases (empty conversation, multiple conversations) but the current coverage is reasonable.

## 9. `/home/vincent/projects/bone/src/session_sink.rs`
- **Lines:** 92
- **Assessment:** mostly good
- **Notes:**
  - Defines the `SessionSink` trait (5 methods) and `NullSessionSink` no-op implementation.
  - Well-documented, clear purpose (injection seam for testability).
  - `NullSessionSink` is 60 lines of trivial no-op implementations. Since all methods are no-ops (they return `None` or do nothing), a macro could reduce boilerplate, though at this file size it's not critical.
  - No over-engineering. The trait is the right abstraction level.

## 10. `/home/vincent/projects/bone/src/shell_split.rs`
- **Lines:** 111
- **Assessment:** mostly good
- **Notes:**
  - Single public function `shell_split` with an options struct. Clean design.
  - Hand-written parser for quoting, escaping, and shell separators. This is appropriate for a focused utility.
  - Comment stripping via `#` at word-boundary is well-implemented (handles quote context).
  - The `push_segment` helper is clear.
  - No over-engineering. Could potentially be simplified by removing the `keep_separators` option if callers only use one mode, but having both is reasonable.

## 11. `/home/vincent/projects/bone/src/shell_split_tests.rs`
- **Lines:** 34
- **Assessment:** mostly good
- **Notes:**
  - Three test cases covering policy style (strip comments + newlines), display style (keep separators), and quoted separators (not split).
  - Coverage is adequate for a utility function of this size. Could add edge cases (e.g. consecutive separators, empty input, only comments), but the existing tests cover the main paths.
  - No simplification needed.

## 12. `/home/vincent/projects/bone/src/config/mod.rs`
- **Lines:** 371
- **Assessment:** can be simplified
- **Notes:**
  - `UserConfig` struct has 13 fields, mostly for spinner/theming configuration. Many of these (`spinner_style`, `spinner_text`, `spinner_speed`, `spinner_text_rotate`, `spinner_text_speed`, `spinner_text_custom`) are consumed only by the TUI status bar. They pollute the core config module. Could move into a `ui::config` submodule.
  - `from_custom_configs` + `apply_custom_configs` pattern: the constructor calls `default()` then `apply_custom_configs()`. The need for both exists because callers sometimes need to apply configs after construction. But `from_custom_configs` currently calls `apply_custom_configs` internally, which works. The separation seems unnecessary — could fold into a single constructor.
  - `SetupSelection`, `load_setup_selection`, `save_setup_selection`, `needs_onboarding`, `seed_base`, `seed_all`, `seed_all_with`, `seed_all_with_persisted`, `apply_onboarding` — these form a mini setup/wizard framework (~130 lines). This is substantial logic for config seeding. Could be extracted into a `config/setup.rs` module.
  - `warn_if_no_api_key_for` is a good user-facing helper. The `is_local_base_url` and `has_codex_auth_token` helpers are specific but reasonable.
  - `STATUS_TOGGLE_KEYS` as a const array is clean. Used in both `UserConfig` and `custom.rs` for migration, which is appropriate.
  - Overall: the spinner config bloats the struct; the onboarding logic could be extracted.

## 13. `/home/vincent/projects/bone/src/config/custom.rs`
- **Lines:** 801
- **Assessment:** over-engineered
- **Notes:**
  - This file implements a mini framework for config pages: a schema-based YAML page system with typed fields (`String`, `Number`, `Bool`, `Enum`, `Provider`), value serialization, denylist migration, and 5 migration functions. This is the most over-engineered file in the project.
  - **Two storage formats coexist**: the old `CustomConfigPage` (field-based with schema + values inline) and the new `DenyListPage` format (title + disabled list). The code has to load both, detect format, migrate on read (`read_denylist`), and convert between them. The denylist format was introduced for tools/commands pages but the field format is still kept for everything else. This dual-format approach adds ~200 lines to handle the migration path.
  - **5 migration functions**: `migrate_old_values_file()`, `migrate_status_values_from_general()`, `migrate_providers_file()`, `backfill_general_fields()`, `backfill_status_fields()` — each handles a different historical state of the config files. This is config migration layering on config migration. These are run unconditionally on every `CustomConfigs::load()` call.
  - `value_for_field()` and the YAML-value-construction in `set_value()` (mapping "true"/"false" strings to `Bool`, parsing numbers, fallback to string) duplicates the YAML type system. If the YAML serde layer handles this, why re-parse manually?
  - `EnabledNames()` gets names by scanning all fields and checking `get_value(namespace, &f.key) == "true" || val.is_empty()`. The `is_empty()` fallback means "default = true" is implicit. This is fragile — a field whose value is accidentally empty string becomes enabled.
  - The `cycle_field` function is TUI logic (cycling through values on keypress) inside the model layer. This belongs in the UI event handler, not the config model.
  - `scan_lua_dir()` does filesystem traversal to discover Lua tools/commands. This couples config to the filesystem layout of Lua scripts. A registry-based approach (e.g. Lua scripts self-register on boot) would be cleaner.
  - The reload-and-revert-on-failure pattern in `set_value` / `set_provider_entry` (write YAML, then if save fails, revert the in-memory value) is defensive but adds complexity. If write failures are rare, a simpler "try to save" with an error log would suffice.
  - `get_provider_entry` / `set_provider_entry` operate on the providers page, which duplicates what `ProvidersConfig` already handles. There are two parallel provider representations (the YAML page field and the `ProvidersConfig` struct) that must be kept in sync.
  - `derive_providers_config()` iterates over the providers page fields and constructs a `ProvidersConfig`. This conversion happens on every config load and also when providers change. The dual representation of provider data (in `CustomConfigPage` as `Provider` fields, and in `ProvidersConfig` as a typed struct) is a source of bugs.

## 14. `/home/vincent/projects/bone/src/config/custom_tests.rs`
- **Lines:** 91
- **Assessment:** mostly good
- **Notes:**
  - Tests use `with_temp_config_home` to sandbox env var changes. Good practice.
  - Three tests: migration of old values file, backfill of new seed fields, migration of status toggles from general page.
  - Covers the migration paths well. No tests for `set_value`/`get_value` round-trips, `cycle_field`, or provider CRUD — these are exercised only through the UI tests, if at all.
  - The `ENV_LOCK` mutex prevents parallel test runs from clobbering each other's `XDG_CONFIG_HOME` — necessary and clean.
  - No simplification needed.

## 15. `/home/vincent/projects/bone/src/config/providers_config.rs`
- **Lines:** 120
- **Assessment:** mostly good
- **Notes:**
  - `ProviderEntry` struct with custom deserializers for default values (`string_or_default`, `string_or_default_endpoint`, `string_or_default_handler`). These are three nearly identical deserialization functions. Could be refactored into one generic helper: `string_or_default_with(deserializer, || "/chat/completions".to_string())`.
  - `from_nested()` manually extracts fields from a `serde_yaml::Value::Mapping`. This is needed because the provider is stored as a nested YAML map inside a `CustomConfigPage` field. It works but duplicates the struct definition. Could deserialize directly with `serde_yaml::from_value::<ProviderEntry>(val.clone())` — the struct already derives `Deserialize`.
  - `ProviderEntry::label` is noted as "Human-readable label shown in the status bar" but is not actually used in the TUI status bar in the code I see.
  - Overall this file is well-structured and focused. The `last_provider` + `providers` HashMap flattening via `#[serde(flatten)]` is clean.
