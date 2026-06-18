# src/ — Worst Code Review & Refactoring Plan

All 10 source files reviewed in parallel. Ranked by severity.

---

## P0 — Must fix first

### 1. `ext/ctx.rs` — `create_ctx_table` (755-line God function)
Lines 63–817. One function builds the entire Lua `ctx` table with ~25 inline closures covering shell, FS, session, agent, config, SQL, UI. **Impossible to test or modify safely.**
- Split into `add_fs_table`, `add_shell_table`, `add_agent_table`, `add_config_table`, `add_session_table`, `add_usage_table`, `add_conversation_table`, `add_db_table`, `add_ui_table`. Each takes `&Lua` + the subset of `cfg` it needs.

### 2. `ext/ctx.rs` — SQL injection + unsound borrows
Lines 1093–1115. `ctx.db.query` checks `starts_with("select")` (trivially bypassed: `SELECT 1; DROP TABLE...; --`), and `db.conn_ref()` returns a borrowed `Connection` whose lifetime is unsound when `stmt`/`rows` outlive the function.
- Open a local `Connection` instead of borrowing `SessionDb`. Add a proper SQL validator or safe query builder.

### 3. `ext/ctx.rs` — `shell_streaming` manual thread spawning
Lines 248–371. Reimplements `run_script` from scratch with `std::thread::spawn` for stdout/stderr. Stderr thread has no timeout → deadlock risk. No policy checks. Two code paths for shell execution.
- Factor out a shared `spawn_bash` helper or make `run_script` support streaming mode.

### 4. `ui/app/mod.rs` — God object `App` (34 fields, 8 concerns)
Lines 37–108. Chat state, LLM config, UI layout, session persistence, timers, Lua integration, approval, subagent state — all in one struct.
- Extract `SessionManager`, `TurnTimer`, `PaneManager`, `LuaUiState`. `App` composes these.

### 5. `ui/app/stream/mod.rs` — `submit_user_turn` (203-line god function)
Lines 255–458. Orchestrates DB writes, extension dispatch, render management, driver setup, event-loop pumping, cancellation, cleanup — all in one function.
- Split into `prepare_turn`, `run_driver`, `reabsorb_outcome`, `cleanup_turn`.

### 6. `ui/app/mod.rs` — `prompt_and_wait` blocking event loop inside async
Lines 1500–1580. Spins its own `loop { event::poll(...) }` while holding `&mut self`. Blocks other tasks, reimplements the outer event loop.
- Make prompting state a mode in the main loop, not a nested loop.

### 7. `ext/ctx.rs` — `row_to_lua_value` dead code (NULL → 0)
Lines 1974–1977. Checks `i64` before `Option<i64>`, so SQL NULL becomes `0` instead of `Nil`. The `Option` checks are unreachable dead code.
- Swap the check order.

### 8. `ext/ctx.rs` — `yaml_to_lua` silently drops numeric keys
Lines 1849–1850. `serde_yaml::Value::Number(_) => continue` drops numeric keys with no warning.
- Convert to string or log a warning.

---

## P1 — High impact refactors

### 9. `ui/app/stream/mod.rs` — 8-parameter `drain_keys`
Lines 878–971. Eight mutable borrows. These fields should be a `StreamState` struct.
- Group into `StreamState`, reduce to `&mut StreamState, &mut KeySink`.

### 10. `ui/app/stream/mod.rs` — two identical event loops
Lines 315–395 and 756–775 share ~80% structure. One processes `RuntimeEvent`, the other `ToolLiveEvent`.
- Extract a generic `run_event_loop` parameterized over event type.

### 11. `ui/app/mod.rs` — `handle_key` ~130 lines
Lines 1150–1300. Pane nav, prompt key, autocomplete, Lua dispatch, paste detection, all `InputAction` variants.
- Extract into a chain of handlers or dedicated dispatch struct.

### 12. `session_db.rs` — 15 monolithic queries in `usage_stats_snapshot`
Lines 530–557. Fetches 15 SQL buckets every time, even if the UI only needs one.
- Compute lazily per requested time window.

### 13. `session_db.rs` — 6 duplicated query methods
Lines 580–710. Six methods with identical CTE → aggregate → LEFT JOIN → project pattern.
- Build a single parameterized time-series query builder.

### 14. `ext/ctx.rs` — `ctx.config.get` double YAML parse per lookup
Lines 826–848. Parses entire YAML file, iterates every field, on every call.
- Cache parsed config in `OnceLock` or `Arc<RwLock<>>`.

### 15. `ext/ctx.rs` — `await_cancelled` busy-poll at 50ms
Line 1823. `tokio::time::sleep(50ms)` in a loop instead of `tokio::sync::Notify`.
- Replace with `Notify` for O(1) wakeup.

### 16. `ui/app/mod.rs` — `run_lua_command` closure soup (~80 lines)
Lines 1715–1800. `spawn_blocking` inside closure inside closure, Lua mutex workarounds, scattered error handling.
- Split into named functions.

### 17. `ui/render/bottom_pane.rs` — `draw_bottom_pane_with_tick` (500 lines)
Lines 323–823. Input border, tab bar, prompt, input area, autocomplete, page with scroll, status bar — all in one function with a mutable `y` cursor incremented in 20 places.
- Split each region into its own method returning consumed rows.

### 18. `ui/render/markdown.rs` — `words_from_spans` allocates per character
Lines 452–469. Creates a `String` + `Span` for every single character. 1 KB paragraph = hundreds of allocations.
- Group consecutive styled chars into substrings, emit one `Span` per run.

### 19. `ui/render/markdown.rs` — `unwrap_markdown_table_fences` index soup
Lines 556–621. Manual index arithmetic, unclosed fence silently discards rest of document.
- Use pulldown-cmark's own fence handling or restructure as a proper state machine.

### 20. `config/custom.rs` — `cycle_field` skips first option
Lines 380–382. `position()` returns `None` → `unwrap_or(0)` → next is index 1. First option unreachable.
- Return `options[0]` when not found.

### 21. `config/custom.rs` — `read_denylist` writes to disk via `&self`
Lines 147–176. A `read_*` method mutates state. Violates least surprise.
- Separate migration write into explicit `load()` step.

### 22. `runtime/driver.rs` — `run_to_outcome` ~290 lines
Lines 72–444. Streaming, retry, token estimation, session recording, extension dispatch, tool execution.
- Extract stream consumption, token-usage recording, assistant message construction, tool-execution dispatch.

### 23. `ext/ctx.rs` — `shell_streaming` stderr thread hangs forever
Lines 313–317. `BufReader::read_to_string` with no timeout. Child hangs → thread never finishes → `join()` blocks forever.
- Use non-blocking stderr or a timeout on the join.

---

## P2 — Cleanups that prevent future bugs

- `ui/app/mod.rs` — `clear_chat` / `load_conversation` duplicated reset boilerplate → extract `reset_ui_state()`
- `ui/app/stream/mod.rs` — three cancellation flags (`cancel_streaming`, `cancel`, `cancel_token`) → single `CancellationToken`
- `ext/types.rs` — `lua_value_to_json` silently converts userdata/functions to `null` → use `lua.from_value`
- `session_db.rs` — hand-rolled datetime vs SQL `localtime` mismatch → add `chrono` or document the UTC/localtime gap
- `session_db.rs` — `search()` fallback matches SQLite error text → pre-validate query
- `ui/render/bottom_pane.rs` — `desired_height` re-derives layout math that `draw_bottom_pane_with_tick` also computes → single source of truth
- `ui/stats.rs` — `draw_daily_activity` 130-line monolith with hardcoded row indices → extract grid computation, rendering, annotations
- `ui/app/mod.rs` — `handle_key` inconsistent error returns (`return self.redraw(term)` vs `self.redraw(term)?; return Ok(())`) → standardize
- `ext/ctx.rs` — `ctx.config.get` O(n) per lookup with full YAML re-parse → cache
- `config/custom.rs` — `Vec<(String, X)>` used as a map → `HashMap`

---

## Top 3 files to refactor first

1. **`ext/ctx.rs`** — 5 P0 items (God function, SQL injection, unsound borrows, shell thread spam, NULL→0 bug, numeric key drop, busy-poll, double YAML parse)
2. **`ui/app/mod.rs` + `ui/app/stream/mod.rs`** — 4 P0 + 2 P1 (God object App, submit_user_turn, prompt_and_wait nested loop, handle_key, drain_keys 8 params, duplicate event loops)
3. **`session_db.rs`** — 2 P0 (15 eager queries, 6 duplicated query methods)
