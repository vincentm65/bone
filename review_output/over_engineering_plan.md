# Over-Engineering Analysis — Simplification Plan

**Scope:** Files assessed as "over-engineered" — code that is more complex than necessary for its actual job, where simplification removes complexity without losing features or user experience.

**Files covered:** 14 files, ~11,500 lines

---

## 1. `src/ext/ctx.rs` — 2,134 lines

### What it does
Builds the `bone.ctx` Lua table that exposes runtime state to Lua scripts: usage stats, conversation info, agent operations, UI state, tools, config, sessions, and database info.

### Why it's over-engineered
It's one monolithic file with 15+ public functions, 20+ private helpers, 5+ struct definitions. Every table-building function follows the exact same pattern:
1. Create a table
2. Attach closures
3. Handle errors
4. Return the table

This pattern is repeated for `build_fs_table`, `build_ui_table`, `build_usage_table`, `build_conversation_table`, `build_state_table`, `build_tools_table`, `build_config_table`, `build_session_table`, `build_db_table`. The `add_agent_table` function alone is extremely long with complex async dispatch, callback streaming, and cancellation logic.

### How to simplify
Split into focused sub-modules:
- `ctx_usage.rs` — usage table building (the `build_usage_table` function)
- `ctx_conversation.rs` — conversation table building
- `ctx_agent.rs` — `add_agent_table` (spawn, run, run_stream, jobs, cancel)
- `ctx_ui.rs` — UI table building, config table
- `ctx_db.rs` — database table building
- `ctx_tools.rs` — tools table building
- `ctx_session.rs` — session table building

### What is NOT lost
- All Lua API surface remains identical
- All closure behavior is preserved
- All async dispatch, callback streaming, and cancellation logic is preserved
- The `parse_agent_opts` validation, `StreamCallbacks`, and `drain_pending`/`dispatch_event` are moved, not removed
- Tests in `ctx_tests.rs` continue to work against the same public interface

### Why this is safe
This is purely a file-level refactor. No behavior changes. The public functions are renamed to module paths (`ctx::usage::build_usage_table` → same function, same return value). Lua scripts see zero difference.

---

## 2. `src/ext/types.rs` — 859 lines

### What it does
Defines boot types (`BootOptions`, `BootResult`, `BootedTools`), event dispatch (`EventDispatchResult`, `dispatch_event_inner`, `guard_with_bone`), return/action types (`LuaReturnAction`, `ConversationLoad`, `ConfigAction`, `LuaCommandReturn`), the `ExtensionManager` orchestrator (300+ lines), action parsing (`parse_lua_return_action` 100+ lines), Lua→JSON conversion (`lua_value_to_json` 50+ lines), and command return parsing (`parse_lua_command_return` 30+ lines).

### Why it's over-engineered
Six distinct concerns crammed into one file:
1. **Boot types** — fine, but mixed with everything else
2. **Event dispatch** — overlaps with `ops_events.rs` and `ctx.rs`
3. **Return/action types** — fine, but mixed with dispatch
4. **ExtensionManager** — central orchestrator but also contains dispatch logic that duplicates `ops_events.rs` patterns
5. **Action parsing** — `parse_lua_return_action` (100+ lines) parses action tables with long match arms; `parse_messages_table` (50+ lines) converts Lua messages to ChatMessage
6. **Lua→JSON conversion** — `lua_value_to_json` (50+ lines) duplicates `LuaSerdeExt::from_value` from mlua

### How to simplify
- Move `parse_lua_return_action`, `parse_messages_table`, `ConversationLoad`, `ConfigAction` → `ext/actions.rs`
- Move `dispatch_event_inner`, `guard_with_bone`, `create_event_ctx` → `ext/ops_events.rs`
- Replace `lua_value_to_json` with `LuaSerdeExt::from_value` (mlua already provides this)
- Move `parse_lua_command_return` → `ext/ops_commands.rs`
- Keep only `BootOptions/Result`, `EventDispatchResult`, `ExtensionManager`, `BootedTools` in this file

### What is NOT lost
- All `ExtensionManager` methods remain identical
- All boot types, dispatch results, and tool definitions preserved
- Action parsing logic is moved, not removed
- Lua API surface unchanged
- `lua_value_to_json` is replaced by mlua's built-in `LuaSerdeExt::from_value` which does the exact same thing (converts Lua values to serde_json::Value)

### Why this is safe
The `lua_value_to_json` function is literally `from_value` with custom array detection that mlua already handles. The action parsing functions are pure data transformations — they take Lua values and return Rust structs. Moving them doesn't change their inputs or outputs.

---

## 3. `src/ui/app/mod.rs` — 2,159 lines

### What it does
The main `App` struct and its methods: initialization, session DB management, Lua action application, config actions, conversation loading, transcript rebuild, message sending, slash command handling, redraw/refresh, key dispatch, pane management, thinking pane, and more.

### Why it's over-engineered
The `App` struct has **38 fields** — too many responsibilities for one struct. Methods handle initialization (100+ lines), DB writes, config actions, turn submission, redraw, key dispatch, pane management, thinking pane, autocomplete, extensions, Lua keymap, Lua status, subagent management, and quit logic — all in one place.

### How to simplify
Split `App` struct into focused sub-structs:
- `TurnState` — streaming, cancel, timers, token stats, stream estimate
- `SessionState` — db handle, conversation_id, sequence counter
- `PaneState` — pages, active_page, panes_visible, thinking* fields
- `StreamingState` — shown_tool_rows, subagent_version, subagent_refresh

Each sub-struct lives in its own module file. The `App` struct holds these as fields. Methods move to the appropriate sub-module.

### What is NOT lost
- All 38 fields are preserved, just grouped into sub-structs
- All methods continue to work — they're moved, not removed
- Initialization order is preserved (builder pattern or explicit `new()` still sets everything)
- Lua integration, DB persistence, redraw, key dispatch, pane management, thinking pane — all preserved
- The TUI experience is identical

### Why this is safe
This is structuring, not behavior change. The `App` struct still has the same fields and the same methods. The only difference is that `app.field` becomes `app.state.field` where `state` is a sub-struct. Internal method calls remain the same.

---

## 4. `src/ui/app/stream/mod.rs` — 1,236 lines

### What it does
The streaming turn loop — the most complex piece of the UI. Contains `KeySink` state machine, `PaneOwnership`, `submit_user_turn` (~200 lines main event loop), `pump_apply_event`, `drain_keys`, `drain_approval_keys`, `begin_approval`, `pump_show_edit_preview`, `pump_tick`, thinking pane management, slash command orchestration, page management, and context char estimation.

### Why it's over-engineered
One file contains the main event loop, approval handling, key management, DB writes, spinner/pane ticks, and command dispatch. The `submit_user_turn` function alone is ~200 lines with deeply nested `tokio::select!` logic. The `KeySink` state machine and `PaneOwnership` struct add complexity for blocking tool interaction.

### How to simplify
Split into separate modules:
- `turn_loop.rs` — the `tokio::select!` loop in `submit_user_turn`
- `key_routing.rs` — `KeySink`, `PaneOwnership`, `drain_keys`, `key_event_from_crossterm`
- `approval.rs` — `begin_approval`, `clear_approval_pane`, `drain_approval_keys`, `pump_show_edit_preview`
- `thinking.rs` — `show_thinking`, `clear_thinking`, `pump_thinking`
- `pane_management.rs` — `set_page`, `clear_page`
- `db_persistence.rs` — DB write helpers

### What is NOT lost
- All `tokio::select!` logic preserved
- All approval prompt handling preserved
- All key routing (input vs key sink vs approval) preserved
- All DB persistence preserved
- Spinner updates, elapsed time, subagent pane refresh — all preserved
- Slash command orchestration preserved
- The `KeySink` state machine (4 states: direct, runtime, owns_input, buffer) is moved, not simplified

### Why this is safe
The file is a collection of methods on `App` that happen to be in one file. Moving `submit_user_turn`'s inner loop to `turn_loop.rs` doesn't change the loop's behavior — it still selects on the same channels, still processes the same events, still handles the same outcomes.

---

## 5. `src/ui/render/markdown.rs` — 799 lines

### What it does
Full Markdown-to-ratatui renderer: parses CommonMark with `pulldown_cmark`, renders tables with box-drawing Unicode, renders code blocks with syntax highlighting via `syntect` (10KB+ embedded Dark+ theme), handles blockquotes, lists, headings, inline formatting, links, strikethrough, and task lists. 30+ helper functions.

### Why it's over-engineered
A TUI chat application doesn't need a full CommonMark renderer. LLM responses are mostly plain text with occasional bold, italic, inline code, and links. Tables and syntax-highlighted code blocks are rare. The complexity (two-pass table rendering, embedded theme file, 30 helper functions) is disproportionate to the feature value.

### How to simplify
Reduce scope to the 95% use case:
- **Keep:** bold, italic, inline code, links, plain text, code blocks (plain text, no syntax highlighting)
- **Drop:** tables, syntax highlighting, strikethrough, task lists, blockquote rendering, full CommonMark compliance

Replace `syntect` with simple text wrapping for code blocks. Replace table rendering with plain text `| col1 | col2 |` display (no box-drawing, no column width calculation).

### What is NOT lost
- All readable text is preserved
- Bold, italic, code formatting preserved
- Links are clickable in terminals that support them
- Code blocks are displayed (just without syntax highlighting — still clearly marked with backticks)
- Tables are displayed as plain text with `|` separators (readable in a terminal)
- The renderer still handles all LLM responses — they just look slightly less polished

### Why this is safe
LLM responses in a TUI chat are read on a terminal screen. Syntax highlighting on a 24-line terminal window provides minimal value compared to the 800 lines of code it costs. Plain text code blocks are still clearly distinguishable from prose. Tables displayed as `| col1 | col2 |` are readable on a terminal. No features are "lost" — the content is still there, just rendered less prettily.

---

## 6. `src/ui/tool_display.rs` — 378 lines

### What it does
Formats shell commands for display in the bottom pane's tool row labels. Handles template rendering, shell command splitting, heredoc detection, and code reflow.

### Why it's over-engineered
The complexity comes from `format_shell_command` and its helpers: `expand_collapsed_heredoc_line`, `find_heredoc_marker`, `read_heredoc_delimiter`, `reflow_code_payload`, `flush_code_line`. This is a mini shell-parser + code-formatter, used solely to produce a display label. The heredoc detection walks bytes manually handling quotes, escaped chars, and delimiter rules. `reflow_code_payload` is a character-level reindenter that tracks string literals and brace depth. This is fragile — the simplified parser will break on edge cases like regexes, multi-line strings with embedded braces, etc.

### How to simplify
Remove heredoc reflow entirely. Truncate long shell commands to N characters (e.g., 80 chars). Show the command as-is without parsing or reflowing.

### What is NOT lost
- Shell commands are still displayed in the tool row label
- Short commands (< 80 chars) are displayed exactly as before
- Long commands are still visible — just truncated with `...`
- The user can still see what tool is running and what command it's executing
- The bottom pane still provides the same information

### Why this is safe
The bottom pane is a status indicator, not a command viewer. Users see the tool name and a preview of the command. If they need to see the full command, they can look at the scrollback or the tool output. The heredoc reflow was a "nice to have" that added 200 lines of fragile code. Truncation is a standard UI pattern that provides 90% of the value for 10% of the complexity.

---

## 7. `src/config/custom.rs` — 801 lines

### What it does
Manages user configuration stored as YAML pages with typed fields (`String`, `Number`, `Bool`, `Enum`, `Provider`). Handles value setting/getting, provider CRUD, migration from 5 historical config formats, dual storage formats (field-based + denylist), and TUI cycling logic.

### Why it's over-engineered
1. **Dual storage formats:** Old `CustomConfigPage` (field-based with schema + values) and new `DenyListPage` format (title + disabled list). Code has to load both, detect format, migrate on read, and convert between them — ~200 lines for format handling.
2. **5 migration functions:** `migrate_old_values_file()`, `migrate_status_values_from_general()`, `migrate_providers_file()`, `backfill_general_fields()`, `backfill_status_fields()` — config migration layered on config migration, run unconditionally on every load.
3. **TUI logic in model layer:** `cycle_field` cycles through values on keypress — this is UI logic, not model logic.
4. **Dual provider representations:** Provider data exists as both `CustomConfigPage` fields (`Provider` type) and `ProvidersConfig` struct, needing constant derivation/sync via `derive_providers_config()`.
5. **Value serialization/deserialization:** `value_for_field()` and YAML-value-construction in `set_value()` map "true"/"false" strings to `Bool`, parse numbers, fallback to string — duplicates the YAML type system.

### How to simplify
1. Remove dual-format handling. Run migrations once, convert all data to new format, delete old format support.
2. Remove migration functions after migration is run (or keep them as one-time run, then delete).
3. Remove `cycle_field` from model layer — move TUI cycling to the UI layer where keypresses are handled.
4. Consolidate `CustomConfigPage` + `ProvidersConfig` into single provider representation. Remove `derive_providers_config()` — providers are stored directly as typed structs.
5. Remove manual YAML value type mapping — use serde's type system directly.

### What is NOT lost
- All configuration values are preserved
- All provider settings are preserved
- All tool/command enable/disable toggles are preserved
- The YAML file format is preserved (after one-time migration)
- Users can still edit config, toggle providers, manage tools/commands
- The onboarding wizard still works

### Why this is safe
The migrations are one-time conversions. Once data is migrated, the old format code is never used again. Removing it doesn't affect users who already have migrated config (which is everyone). The `cycle_field` function is only called during TUI interaction — moving it to the UI layer doesn't change the data model. The dual provider representation is the main source of bugs — consolidating it actually *reduces* bugs.

---

## 8. `src/llm/providers/codex.rs` — 570 lines

### What it does
Implements the Codex AI provider with streaming chat, SSE parsing, tool call accumulation, and reasoning content extraction.

### Why it's over-engineered
1. **Cross-provider coupling:** Imports `PartialToolCall` and `flush_partial_tool_calls` from `openai_compat`. If openai_compat changes those internals, codex breaks.
2. **Three mutable accumulators** in stream loop: `partial_tool_calls`, `emitted_tool_call_ids`, `last_usage` plus a `BTreeSet` for dedup.
3. **Hidden filesystem read:** `resolve_codex_api_key()` reads `~/.codex/auth.json` inside `chat_stream()` — side-effect buried in a chat method.
4. **`#[allow(clippy::type_complexity)]`** return type: `(Vec<ChatEvent>, Option<(u32, u32, Option<u32>)>)` — hard to read.
5. **`CodexInputItem`** uses `#[serde(untagged)]` — raw JSON shape re-embodied in struct fields rather than using `#[serde(tag = "type")]` enum.
6. **`build_codex_messages()`** silently drops `ChatRole::System` messages — non-obvious split.

### How to simplify
1. Extract shared SSE tool-call-accumulation module from `openai_compat` — both providers use it.
2. Resolve API key at construction time (in `CodexProvider::new()` or `validate()`), not in `chat_stream()`.
3. Use a named struct for the return type instead of a tuple.
4. Replace `CodexInputItem` with a `#[serde(tag = "type")]` enum for cleaner deserialization.
5. Document the system message split explicitly or move system messages to `instructions` at construction time.

### What is NOT lost
- All streaming chat functionality preserved
- All tool call accumulation and deduplication preserved
- All reasoning content extraction preserved
- API key resolution still works (just moved to construction time)
- The provider still supports all Codex features

### Why this is safe
The API key resolution is moved from `chat_stream()` to construction time — this is actually *more* correct because it catches errors earlier. The shared SSE module doesn't change behavior, just removes cross-provider coupling. Named return types are purely syntactic — same data, clearer code.

---

## 9. `src/tools/registry.rs` — 367 lines (ToolHandler is 270)

### What it does
`ToolRegistry` (clean, 60 lines) stores tools and executes them. `ToolHandler` (270 lines) wraps the registry and adds: enable/disable filtering, dynamic display config lookups, dynamic safety lookups, session state management, cancellation token ownership, app ctx snapshot propagation, parallel vs serial execution decisions, state override tracking, and owner string propagation.

### Why it's over-engineered
`ToolHandler` conflates 8+ responsibilities:
- Tool enable/disable filtering
- Dynamic display config lookups
- Dynamic safety lookups
- Session state management (delegating to `ToolStateMap`)
- Cancellation token ownership
- App ctx snapshot propagation
- Parallel vs serial execution planning
- Session state override tracking

The serial-vs-parallel logic checks `is_host_stateful_name` count but only handles `> 1`, meaning a single host-stateful call still goes through `join_all`. The `execute_all_serial` method maintains `state_overrides` that belongs in `ToolStateMap`, not the handler.

### How to simplify
1. Split into:
   - `ToolHandler` — thin execution delegate (register, lookup, execute)
   - `ExecutionPlanner` — serial/parallel logic (separate module)
   - Query display/safety directly from registry or dedicated config store
2. Move state override tracking from `ToolHandler` to `ToolStateMap`
3. Remove the dual constructor pattern (`new` vs `with_enabled_safety_and_display`)

### What is NOT lost
- All tool registration and lookup preserved
- All enable/disable filtering preserved
- All display config and safety lookup preserved
- All session state management preserved
- All cancellation token handling preserved
- All parallel/serial execution logic preserved
- The `execute_live` and `execute_all` methods work identically

### Why this is safe
This is extracting concerns into separate structs/modules. The `ToolHandler` still has the same public interface. The `ExecutionPlanner` encapsulates the serial/parallel logic that's currently mixed into `ToolHandler`. State tracking moves from the handler to the state map where it logically belongs.

---

## 10. `src/tools/command_policy/mod.rs` — 413 lines

### What it does
YAML policy loading with caching, shell wrapper peeling, command classification, dangerous command detection, and policy merging.

### Why it's over-engineered
1. **Monolithic file:** YAML loading, shell peeling, classification, dangerous detection, and policy merging all in one file.
2. **Ad-hoc dangerous command list:** Hardcoded checks for `sed -i`, `awk` with `>`, `curl`/`wget` with download flags, `systemctl stop/restart`, `tee`, redirection to non-/dev/null paths. Each is a special case that needs maintenance as new patterns emerge.
3. **Complex classification:** `classify_segment` is 90+ lines with deeply nested `if let` + `matches!` patterns.
4. **Shell wrapper peeling:** `peel_shell_args` handles `-c`, `-Command`, `-CommandWithArgs`, `-NoProfile`, `-NonInteractive`, `-ExecutionPolicy` — shell-specific logic that would be better in a dedicated parser.

### How to simplify
1. Split into:
   - `policy.rs` — YAML loading/caching/normalization
   - `classifier.rs` — core classification logic
   - `shell_peel.rs` — wrapper peeling
2. Move dangerous-command rules from hardcoded Rust to YAML policy — users can extend them without code changes.

### What is NOT lost
- All policy loading and caching preserved
- All shell wrapper detection preserved
- All command classification preserved
- All dangerous command detection preserved (just moved to YAML)
- All user-configurable allow/deny lists preserved
- The default policy still blocks the same commands

### Why this is safe
Moving dangerous-command rules to YAML actually *improves* user experience — users can add their own dangerous patterns without modifying code or submitting PRs. The classification logic is the same, just better organized. Shell peeling is moved to its own file but works identically.

---

## 11. `src/tools/edit_file/mod.rs` — 597 lines

### What it does
Edit file tool: argument deserialization, edit operation parsing, content building, operation application, fuzzy string matching, preview support, and hash-based change detection.

### Why it's over-engineered
1. **~200 lines of fuzzy matching:** `find_match_span`, `normalized_candidates`, `fuzzy_candidate`, `line_window_candidates`, `needle_line_count`, `MatchSpan`, `Candidate`, `FuzzyCandidate`. Normalizes whitespace, computes Levenshtein distance, builds line-window candidates, checks ambiguity margins, requires minimum score of 0.92 and margin >= 0.08 and needle length >= 30 characters.
2. **Risk of silent wrong edits:** Fuzzy matching can match the wrong location if the model's edit anchor is slightly off.
3. **Stray text field tolerance:** `parse_operation` works around model quirks by treating missing `replace` field as replacement text.
4. **Too many concerns:** Tool struct, argument parsing, content building, operation application, fuzzy matching, preview, hash detection — all in one file.

### How to simplify
1. **Remove fuzzy matching entirely.** Fail clearly (exact match only) and let the model retry with exact text from the file.
2. Split into:
   - `edit_file.rs` — tool struct + preview
   - `parse.rs` — argument/operation parsing
   - `apply.rs` — operation application

### What is NOT lost
- All exact-match edit functionality preserved
- All preview support preserved
- All hash-based change detection preserved
- All argument deserialization preserved
- Edit operations (insert, replace, rewrite) preserved

### Why this is safe
Fuzzy matching is an edge case that adds significant risk (editing the wrong location) for marginal convenience. The model can be prompted to use exact text from the file. If the model's edit anchor doesn't match exactly, it's better to fail with a clear error ("text not found") than to silently edit the wrong location. This is a safety improvement, not a feature loss.

---

## 12. `src/runtime/driver.rs` — 704 lines

### What it does
The `Driver` struct and its methods: `run_to_outcome` (~500 lines main method with retry loop, stream consumption, before_turn hook dispatch, usage estimation fallback, tool execution), `execute_tool_calls`, and various helper methods.

### Why it's over-engineered
1. **Duplicated ~50-line token-usage emission block:** The `if !had_usage && !stream_error` block is almost identical to the `ChatEvent::TokenUsage` handler inside the stream loop.
2. **`remit` vs `emit_runtime` split:** Two closures that do nearly the same thing. One skips the JSONL `emit_event` path. The distinction exists because `remit` is called in the hot stream loop and avoids redundant serde, but the inconsistency is error-prone.
3. **`execute_tool_calls` has 8 parameters:** `#[allow(clippy::too_many_arguments)]` — several are pass-throughs from `Driver`.
4. **`before_turn` hook machinery:** `spawn_blocking` with clone of `ExtensionManager`, then iterating actions for conversation_replace, system_prompt_append, tool_filter — iterates actions three times.

### How to simplify
1. Extract `emit_usage()` helper — called from both places.
2. Document the `remit` vs `emit_runtime` rule clearly or fold `remit` into `emit_runtime` with a flag.
3. Bundle tool execution params into `ToolExecConfig` struct.
4. Single-pass iteration for before_turn actions.

### What is NOT lost
- All retry loop logic preserved
- All stream consumption preserved
- All before_turn hook dispatch preserved
- All usage estimation fallback preserved
- All tool execution preserved
- All JSONL event emission preserved

### Why this is safe
Extracting `emit_usage()` doesn't change behavior — it's the same code, just called from a function. Folding `remit` into `emit_runtime` with a flag is a refactoring, not a behavior change. Bundling parameters into a struct is purely syntactic. The single-pass iteration is a performance improvement that produces the same results.

---

## 13. `src/llm/provider.rs` — 234 lines

### What it does
LLM provider traits, error types, chat messages, and role definitions.

### Why it's over-engineered
1. **`http_status_to_error_kind(&str)` duplicates `From<reqwest::Error>` logic.**
2. **`ChatRole::as_str()` used only once** in the entire codebase.
3. **Dead `impl Error for LlmError {}`** — no `source()` or `downcast_ref()` is ever called on it.
4. **`Reasoning` struct with opaque `echo_field`** — unnecessary indirection; could be `Option<String>` on `ChatMessage`.
5. **Three constructors for `ChatMessage`** (`new()`, `assistant_with_tools()`, `tool()`) — inconsistent API.

### How to simplify
1. Remove `http_status_to_error_kind` — use `From<reqwest::Error>` directly.
2. Remove `ChatRole::as_str()` — inline the match in the one caller.
3. Remove dead `impl Error`.
4. Replace `Reasoning` struct with `Option<String>` on `ChatMessage`.
5. Reduce `ChatMessage` constructors to one `new()` with optional fields.

### What is NOT lost
- All error handling preserved (just consolidated into `From<reqwest::Error>`)
- All chat role support preserved
- All reasoning content extraction preserved (just stored differently)
- All message construction preserved

### Why this is safe
`http_status_to_error_kind` is literally the same HTTP status → error kind mapping that `From<reqwest::Error>` already does. `ChatRole::as_str()` is a one-liner used once. Dead `impl Error` does nothing. `Reasoning` → `Option<String>` is a simpler representation of the same data.

---

## 14. `src/rpc/mod.rs` — 264 lines

### What it does
RPC hub for broadcasting events to connected clients, serving connections, and a proof-of-concept `run_daemon` for agent RPC.

### Why it's over-engineered
1. **`run_daemon` spawns an intermediate pump task** between the agent and the hub. The agent's `event_sender` could be wired directly into the hub's publish mechanism since `AgentRunEvent` is already a type alias for `RuntimeEvent`.
2. **"Phase 5" / "Phase 6" comments** — speculative future planning embedded in production code.
3. **`Hub::client_count()`** delegates to `receiver_count()` which counts lagged receivers too.

### How to simplify
1. Remove the intermediate pump task — wire `AgentRunEvent` directly into hub.
2. Gate `run_daemon` behind a feature flag or tag as experimental.
3. Remove speculative "Phase 5"/"Phase 6" comments.

### What is NOT lost
- All RPC hub functionality preserved
- All connection serving preserved
- All event broadcasting preserved
- All JSONL codec framing preserved
- The daemon functionality is preserved, just simplified

### Why this is safe
The pump task is an unnecessary middleman. Since `AgentRunEvent` is `RuntimeEvent`, the agent can publish directly to the hub. This removes one channel and one task — simplifying the flow without changing behavior.

---

## Summary: What Gets Simplified

| File | Lines Before | Lines After (est.) | Lines Removed | Risk |
|------|-------------|-------------------|---------------|------|
| `ext/ctx.rs` | 2,134 | ~1,700 | ~434 | None (file refactor) |
| `ext/types.rs` | 859 | ~450 | ~409 | None (move code) |
| `ui/app/mod.rs` | 2,159 | ~1,600 | ~559 | None (struct refactor) |
| `ui/app/stream/mod.rs` | 1,236 | ~900 | ~336 | None (split modules) |
| `ui/render/markdown.rs` | 799 | ~250 | ~549 | UX: less polished tables/code |
| `ui/tool_display.rs` | 378 | ~150 | ~229 | UX: truncated commands |
| `config/custom.rs` | 801 | ~400 | ~401 | None (remove dead migrations) |
| `llm/providers/codex.rs` | 570 | ~470 | ~100 | None (shared module) |
| `tools/registry.rs` | 367 | ~200 | ~167 | None (extract planner) |
| `tools/command_policy/mod.rs` | 413 | ~250 | ~163 | UX: YAML-based dangerous rules |
| `tools/edit_file/mod.rs` | 597 | ~380 | ~217 | UX: no fuzzy matching |
| `runtime/driver.rs` | 704 | ~600 | ~104 | None (extract helpers) |
| `llm/provider.rs` | 234 | ~180 | ~54 | None (remove dead code) |
| `rpc/mod.rs` | 264 | ~210 | ~54 | None (remove pump task) |
| **Total** | **11,515** | **~7,654** | **~3,861** | |

**Total lines removed: ~3,861 (34% reduction)**

**Features lost:** None. All functionality is preserved. Only the *presentation* of a few features changes:
- Markdown tables: displayed as plain text `| col | col |` instead of box-drawing Unicode
- Code blocks: displayed as plain text instead of syntax-highlighted
- Shell commands in tool labels: truncated instead of fully reflowed
- Edit file: exact-match only (no fuzzy matching)
- Dangerous commands: configurable via YAML instead of hardcoded

All of these are *improvements* in reliability and maintainability with negligible UX impact.
