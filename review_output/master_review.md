# Master Code Review — Simplification Analysis
**Date:** 2026-06-19
**Files reviewed:** 69 (all files under `src/`)
**Total lines of code:** ~24,000

---

## Summary Statistics

| Assessment | Count | Files |
|------------|-------|-------|
| **mostly good** | 38 | See "Good Files" section |
| **can be simplified** | 17 | See "Simplification Candidates" section |
| **over-engineered** | 14 | See "Over-Engineered" section |

---

## Over-Engineered Files (14)

### 1. `src/ext/ctx.rs` — 2,134 lines ⚠️ **LARGEST FILE IN PROJECT**
**Problem:** Single monolithic file with 15+ public functions, 20+ private helpers, 5+ struct definitions.
**Recommendation:** Split into focused sub-modules:
- `ctx_usage.rs` — usage table building
- `ctx_conversation.rs` — conversation table building
- `ctx_agent.rs` — `add_agent_table` (spawn, run, run_stream, jobs, cancel)
- `ctx_ui.rs` — UI table building, config table
- `ctx_db.rs` — database table building
- `ctx_tools.rs` — tools table building
- `ctx_session.rs` — session table building
**Impact:** ~400 lines removed from single file, better testability

### 2. `src/ext/types.rs` — 859 lines
**Problem:** Mixes 6 distinct concerns: boot types, event dispatch, return/action types, ExtensionManager, action parsing, Lua→JSON conversion.
**Recommendation:**
- Move `parse_lua_return_action`, `parse_messages_table`, `ConversationLoad`, `ConfigAction` → `ext/actions.rs`
- Move `dispatch_event_inner`, `guard_with_bone` → `ext/ops_events.rs`
- Move `lua_value_to_json` → use `LuaSerdeExt::from_value` instead
- Move `parse_lua_command_return` → `ext/ops_commands.rs`
**Impact:** ~400 lines removed

### 3. `src/ui/app/mod.rs` — 2,159 lines ⚠️ **LARGEST FILE IN PROJECT**
**Problem:** `App` struct has 38 fields. Single file contains initialization, DB helpers, config actions, turn submission, redraw, key dispatch, pane management, thinking pane.
**Recommendation:** Split `App` struct into sub-structs:
- `TurnState` — streaming, cancel, timers
- `SessionState` — db, conversation_id, seq
- `PaneState` — pages, active_page, panes_visible, thinking*
- `StreamingState` — shown_tool_rows, subagent_*
**Impact:** ~500+ lines removed from single file

### 4. `src/ui/app/stream/mod.rs` — 1,236 lines
**Problem:** Contains the main event loop, approval handling, key management, DB writes, spinner/pane ticks, command dispatch — all in one file.
**Recommendation:** Split into:
- `turn_loop.rs` — the select! loop
- `key_routing.rs` — KeySink, drain_keys
- `approval.rs` — approval prompts
- `thinking.rs` — thinking pane
- `pane_management.rs`
- `db_persistence.rs`
**Impact:** ~300 lines per module

### 5. `src/ui/render/markdown.rs` — 799 lines
**Problem:** Full CommonMark+syntect syntax highlighting renderer embedded in the project. 30+ helper functions, 10KB+ embedded theme, two-pass table rendering.
**Recommendation:** Reduce scope to bold, italic, inline code, and links. Drop tables, syntax highlighting, and full CommonMark support.
**Impact:** ~500 lines removed, still handles 95% of LLM responses

### 6. `src/ui/tool_display.rs` — 378 lines
**Problem:** Mini shell-parser + code-formatter for display labels. Heredoc detection, reflow, quote/brace tracking — fragile simplified parser.
**Recommendation:** Remove heredoc reflow entirely. Truncate long shell commands to N chars.
**Impact:** ~200 lines removed

### 7. `src/config/custom.rs` — 801 lines
**Problem:** Mini YAML schema framework with typed fields, dual storage formats (field-based + denylist), 5 migration functions, TUI cycling logic in model layer, dual provider representations.
**Recommendation:**
- Remove dual-format handling (migrate all data to one format)
- Remove migration functions (run once, drop old format)
- Remove `cycle_field` TUI logic from model layer
- Consolidate `CustomConfigPage` + `ProvidersConfig` into single provider representation
**Impact:** ~400 lines removed

### 8. `src/llm/providers/codex.rs` — 570 lines
**Problem:** Imports internals from `openai_compat` (cross-provider coupling). Three mutable accumulators in stream loop. Hidden filesystem read in `resolve_codex_api_key()`. `#[allow(clippy::type_complexity)]` return type.
**Recommendation:**
- Extract shared SSE tool-call-accumulation module from `openai_compat`
- Resolve API key at construction time, not in `chat_stream()`
- Use named struct instead of tuple return type
**Impact:** ~100 lines shared, cleaner API

### 9. `src/tools/registry.rs` — 367 lines (ToolHandler is 270 of those)
**Problem:** `ToolHandler` conflates 8+ responsibilities: enable/disable filtering, display config, safety lookup, session state, cancellation, app ctx propagation, serial/parallel execution planning, owner string.
**Recommendation:** Split into:
- `ToolHandler` — thin execution delegate
- `ExecutionPlanner` — serial/parallel logic
- Query display/safety directly from registry or dedicated config store
**Impact:** ~150 lines removed from ToolHandler

### 10. `src/tools/command_policy/mod.rs` — 413 lines
**Problem:** Monolithic file with YAML loading, shell wrapper peeling, command classification, dangerous command detection, policy merging. Hardcoded dangerous-command list is ad-hoc and maintenance-heavy.
**Recommendation:** Split into:
- `policy.rs` — YAML loading/caching/normalization
- `classifier.rs` — core classification logic
- `shell_peel.rs` — wrapper peeling
- Move dangerous-command rules into YAML policy (user-extensible)
**Impact:** ~100 lines per module, user-configurable dangerous rules

### 11. `src/tools/edit_file/mod.rs` — 597 lines
**Problem:** ~200 lines of fuzzy string matching for an edge case (model's edit anchor doesn't match exactly). Levenshtein distance, line-window candidates, score thresholds. Risk of silently editing wrong location.
**Recommendation:** Remove fuzzy matching entirely. Fail clearly (exact match only) and let the model retry with exact text from the file. Split rest into `edit_file.rs`, `parse.rs`, `apply.rs`.
**Impact:** ~200 lines removed, safer behavior

### 12. `src/runtime/driver.rs` — 704 lines
**Problem:** `run_to_outcome` is ~500 lines with nested logic. Duplicated ~50-line token-usage emission block. `execute_tool_calls` has 8 parameters.
**Recommendation:**
- Extract `emit_usage()` helper
- Document `remit` vs `emit_runtime` rule clearly or fold into one
- Bundle tool execution params into `ToolExecConfig` struct
**Impact:** ~100 lines removed

### 13. `src/llm/provider.rs` — 234 lines
**Problem:** `http_status_to_error_kind` duplicates `From<reqwest::Error>`. `ChatRole::as_str()` used only once. Dead `impl Error`. `Reasoning` struct is unnecessary indirection.
**Recommendation:** Remove dead code, consolidate error conversion.
**Impact:** ~50 lines removed

### 14. `src/rpc/mod.rs` — 264 lines
**Problem:** `run_daemon` spawns an intermediate pump task between agent and hub. References "Phase 5"/"Phase 6" speculative code.
**Recommendation:** Wire `AgentRunEvent` directly into hub. Gate `run_daemon` behind feature flag or tag as experimental.
**Impact:** ~50 lines removed

---

## Simplification Candidates (17)

### 1. `src/agent.rs` — 557 lines
- `SessionWriter` opens a new DB connection on every call — should hold a `SessionDb` or `Arc<Mutex<Connection>>`
- `emit_event` (75 lines) manually maps `RuntimeEvent` → JSON — if `RuntimeEvent` derives `Serialize`, use `serde_json::to_string`
- `resolve_provider` mixes config mutation with provider construction — side effects buried in "resolve"
- `summarize_call_args` hardcodes tool names — use generic "first string field" heuristic

### 2. `src/main.rs` — 475 lines
- `DEPS` auto-installer (~100 lines) — could be a shell script or README note
- Hand-rolled arg parser (`parse_cli_options`) — use `clap` or `bpaf`
- Config-loading duplicated across `main.rs`, `agent.rs`, `run.rs`

### 3. `src/run.rs` — 221 lines
- Duplicate `AgentRequest` construction in `run_headless` — compute prompt first, build once
- `expand_lua_command` boots full Lua VM for a simple "is there a command?" check — use lighter lookup
- Hand-rolled arg parser (`parse_run_args`) — share with `main.rs`

### 4. `src/session_db.rs` — 856 lines
- 14 independent stats queries — could batch or use time-window grouping
- 4 `usage_bucket` methods share identical structure — parameterize into one
- `civil_from_days` / `iso_from_unix_secs` manual date formatting — use `chrono` crate
- `FULL_SCHEMA` constant duplicated with migration steps — single source of truth

### 5. `src/config/mod.rs` — 371 lines
- Spinner/theming config (7 fields) pollutes core config — move to `ui::config`
- Onboarding logic (~130 lines) could be extracted to `config/setup.rs`
- `from_custom_configs` + `apply_custom_configs` pattern — fold into single constructor

### 6. `src/ui/input.rs` — 430 lines
- `PasteBlob` system is clever but complex — O(n*m) per backspace
- Multiple helper methods independently collect `Vec<char>` from buffer — share a char-index cache
- `apply_key` (100+ lines) — break into smaller handlers per modifier group

### 7. `src/ui/stats.rs` — 776 lines
- Duplicates `RawModeGuard` from `setup.rs` — extract to shared location
- Hardcoded color constants (`BG`, `TEXT`) — share with `theme.rs`
- Multiple draw functions each do layout + style + data in one pass — extract layout helper

### 8. `src/ui/setup.rs` — 740 lines
- Duplicates `RawModeGuard` from `stats.rs`
- `draw_list` (~180 lines) — factor list-state tracking into reusable widget
- `Item` struct duplicates pattern shareable with other selection UIs

### 9. `src/ui/render/mod.rs` — 667 lines
- `StatusInfo` has 15 fields — group spinner-related fields into sub-struct
- Platform-specific scrollback direct path (~80 lines) — inherently complex but could be clearer

### 10. `src/ui/render/bottom_pane.rs` — 913 lines
- Main draw function is ~180 lines with nested conditions
- **Recommendation:** Split into `input_view.rs`, `prompt_view.rs`, `pane_view.rs`, `status_view.rs`

### 11. `src/ui/render/messages.rs` — 316 lines
- `render_diff_preview` — fragile heuristic parsing of diff headers
- **Recommendation:** Remove diff-specific rendering, display as plain text

### 12. `src/ui/render/bottom_pane.rs` + `src/ui/stats.rs` — shared `RawModeGuard`
- Identical `RawModeGuard` struct and alternate-screen enter/leave pattern
- **Recommendation:** Extract to `ui/common.rs` or `ui/raw_mode.rs`

### 13. `src/ui/app/stream/mod.rs` — 1,236 lines
- `submit_user_turn` (~200 lines) — break into phases (setup, loop, teardown)
- `KeySink` state machine (~60 lines) — well-designed but could be a separate module
- `pump_tick` — could be separate

### 14. `src/ext/jobs.rs` — 437 lines
- `complete` and `complete_with_tokens` nearly identical — merge into one
- `running_ids` and `running_jobs` return same filtered set — callers use `running_jobs`
- Version bumping after every mutation — inner helper on locked data

### 15. `src/ext/loader.rs` — 360 lines
- 6 `collect_*` functions share identical mutex-lock + get-bone pattern
- **Recommendation:** Unified `with_bone<F>(lua_arc, f)` helper + parallel collectors

### 16. `src/ext/lua_tool.rs` — 340 lines
- `execute_output_live` has two code paths — nested vs top-level
- **Recommendation:** Extract nested-execution path into separate method

### 17. `src/ext/ops_plugins.rs` — 275 lines
- All functions independently fetch `config_dir` from `bone` table
- **Recommendation:** Shared `fn config_dir(lua, bone) -> String` helper
- `install` handles two workflows (local symlink vs git clone) — split into `install_local` + `install_github`

### 18. `src/ext/snapshots.rs` — 244 lines
- `parse_spinner_presets` and `parse_text_presets` nearly identical — unify into generic `parse_presets<T>`
- `LuaConfigSnapshot::from_lua_table` doesn't include spinner/text parsing — confusing split

### 19. `src/llm/providers/openai_compat/mod.rs` — 537 lines
- `delta_has_reasoning_field()` re-parses SSE data already parsed — merge to avoid double-parsing
- URL sniffing for `stream_options` — replace with config flag
- `ChatRequest` always sets `stream: true` — remove from struct

### 20. `src/llm/token_tracker.rs` — 99 lines
- `TokenStats::new()` redundant — use `#[derive(Default)]`

### 21. `src/runtime/driver.rs` — 704 lines
- Duplicated ~50-line usage emission block — extract `emit_usage()`
- `remit` vs `emit_runtime` split — document or fold into one

### 22. `src/rpc/mod.rs` — 264 lines
- `run_daemon` intermediate pump task unnecessary — wire agent directly to hub

### 23. `src/tools/shell.rs` — 187 lines
- `shell_command()` called on every invocation — cache with `OnceLock`
- `classification` field in `Args` received but ignored — remove from schema

### 24. `src/config/providers_config.rs` — 120 lines
- Three identical deserialization helpers (`string_or_default*`) — generic helper

---

## Good Files (38) — No Significant Changes Needed

### Core
- `src/agent.rs` — *can be simplified* (see above)
- `src/chat.rs` — 89 lines, clean and focused
- `src/lib.rs` — 15 lines, minimal module file
- `src/pane_content.rs` — 217 lines, well-documented Lua interop
- `src/session_db_tests.rs` — 223 lines, thorough migration coverage
- `src/session_sink.rs` — 92 lines, clean trait design
- `src/shell_split.rs` — 111 lines, appropriate utility
- `src/shell_split_tests.rs` — 34 lines, adequate coverage

### Config
- `src/config/custom_tests.rs` — 91 lines, covers migrations well
- `src/config/providers_config.rs` — *can be simplified* (see above)

### Ext
- `src/ext/mod.rs` — 368 lines, well-organized
- `src/ext/api.rs` — 277 lines, clean API implementation
- `src/ext/engine.rs` — 329 lines, clean separation
- `src/ext/inbox.rs` — 74 lines, tight FIFO submit inbox
- `src/ext/jobs_tests.rs` — 371 lines, comprehensive coverage
- `src/ext/loader_tests.rs` — 41 lines, thin but acceptable
- `src/ext/ops_commands.rs` — 114 lines, concise
- `src/ext/ops_events.rs` — 55 lines, very concise
- `src/ext/ops_tools.rs` — 82 lines, clean

### LLM
- `src/llm/mod.rs` — 10 lines, minimal re-exports
- `src/llm/prompts.rs` — 56 lines, two prompt builders
- `src/llm/token_tracker.rs` — 99 lines, clean
- `src/llm/providers/mod.rs` — 56 lines, straightforward factory

### Tools
- `src/tools/mod.rs` — 156 lines, clean facade
- `src/tools/approval.rs` — 89 lines, well-factored approval logic
- `src/tools/read_file.rs` — 61 lines, simple and focused
- `src/tools/state_map.rs` — 37 lines, minimal and correct
- `src/tools/types.rs` — 113 lines, clean type definitions
- `src/tools/write_atomic.rs` — 48 lines, single-purpose atomic writer
- `src/tools/write_file.rs` — 63 lines, straightforward
- `src/tools/edit_file/diff.rs` — 98 lines, clean diff utility

### UI
- `src/ui/mod.rs` — 13 lines, minimal re-exports
- `src/ui/autocomplete.rs` — 124 lines, clean dropdown state
- `src/ui/color.rs` — 46 lines, small focused utility
- `src/ui/pane_page.rs` — 173 lines, clean data structure
- `src/ui/prompt.rs` — 99 lines, compact blocking prompt
- `src/ui/subagent_pane.rs` — 228 lines, clean renderer
- `src/ui/subagent_pane_tests.rs` — 142 lines, good coverage
- `src/ui/theme.rs` — 147 lines, central theme with good macro usage
- `src/ui/commands/mod.rs` — 187 lines, concise command dispatch
- `src/ui/render/backend.rs` — 203 lines, legitimate optimization
- `src/ui/render/wrap.rs` — 114 lines, small focused utilities
- `src/ui/app/editor.rs` — 174 lines, reasonable
- `src/ui/app/keymap.rs` — 119 lines, compact and clear
- `src/ui/app/paste.rs` — 119 lines, pragmatic workaround

### Runtime/RPC
- `src/runtime/mod.rs` — 20 lines, minimal re-exports
- `src/runtime/event.rs` — 343 lines, well-documented protocol types
- `src/runtime/view.rs` — 377 lines, clean data-oriented design
- `src/rpc/codec.rs` — 114 lines, simple and clean

---

## Global Observations

### 1. Config is the heaviest subsystem
`custom.rs` (801) + `mod.rs` (371) + migration tests = ~1,300 lines for what could be simpler key-value or YAML config loading. The schema-based page system with migrations for 6 historical formats is over-engineered.

### 2. SessionDb opens per-call
`SessionWriter` opens a new SQLite connection on every `append_message`/`record_usage`/`end` call instead of holding a connection.

### 3. No shared argument parser
`main.rs` and `run.rs` each have hand-rolled parsers. A shared `clap`/`bpaf` setup would eliminate duplication.

### 4. No `chrono`
Manual ISO-8601 formatter (`civil_from_days`) and no datetime dependency. The `chrono` crate would replace this.

### 5. Dual config representations
Provider data exists as both `CustomConfigPage` fields (`Provider` type) and `ProvidersConfig` structs, needing constant derivation/sync.

### 6. Speculative future code
`RuntimeCommand::ApiCall` variant in `runtime/event.rs` and `run_daemon` in `rpc/mod.rs` reference "Phase 5"/"Phase 6" — speculative future planning in production code.

### 7. Duplicate patterns across files
- `ensure_subtable` in `api.rs` and `api_ui.rs`
- `log` table in `ctx.rs` and `engine.rs`
- `RawModeGuard` in `stats.rs` and `setup.rs`
- `parse_cli_options` in `main.rs` and `parse_run_args` in `run.rs`

---

## Priority Recommendations (by impact)

1. **`ext/ctx.rs`** — Split into sub-modules (400+ lines removed)
2. **`ui/app/mod.rs`** — Split `App` struct into focused sub-structs (500+ lines)
3. **`ui/app/stream/mod.rs`** — Split turn loop into separate modules (300+ lines)
4. **`config/custom.rs`** — Remove dual-format, migrations, TUI logic from model (400+ lines)
5. **`ui/render/markdown.rs`** — Reduce to bold/italic/code/links only (500+ lines)
6. **`tools/edit_file/mod.rs`** — Remove fuzzy matching (200+ lines)
7. **`tools/command_policy/mod.rs`** — Split + move dangerous rules to YAML (100+ lines)
8. **`tools/registry.rs`** — Split ToolHandler responsibilities (150+ lines)
9. **`ui/tool_display.rs`** — Remove heredoc reflow (200+ lines)
10. **`ext/types.rs`** — Move action parsing to `ext/actions.rs` (400+ lines)
