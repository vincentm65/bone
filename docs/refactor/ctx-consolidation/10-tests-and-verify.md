# Step 10 — Tests and end-to-end verification

**Goal:** Update tests to the new surface, add coverage for the generalized primitives, and
verify end to end.

## Test migration

### `src/ext/ctx_tests.rs`
- Remove/replace any `ctx.session.*`, `ctx.fs.exists/is_file/is_dir/metadata`,
  `ctx.config.get_table`, `ctx.shell_streaming`, `ctx.agent.run_stream`, `ctx.emit_pane`,
  `ctx.ui.notify/status` cases.
- The `conversation.current` / `conversation.history` tests (ctx_tests.rs:421-434) stay green.

### `tests/lua_api_test.rs`
- `ctx.ui.notify` (lines 483, 484) → `ctx.log.*`.
- `ctx.agent.run` cases (332-343) stay; ensure no `run_stream`/`shell_streaming` assertions
  remain.

### `tests/compact_test.rs`
- `ctx.ui.notify` (line 229) → `ctx.log.*`.
- `ctx.conversation.*` (102, 104, 117, 119, 267, 271) unchanged.

## New focused coverage (add to `ctx_tests.rs`)

- `ctx.shell("...", { on_line = cb })` invokes `cb` per line and returns `{stdout,...}`.
- `ctx.fs.stat` returns a table for an existing path and `nil` for a missing one.
- `ctx.config.get(section)` (no key) returns the whole section table; with key returns value.
- `ctx.db.query` reproduces the old `session.messages` result for a seeded conversation
  (guards the `lib/history` migration).

## End-to-end verification

1. `cargo build` then `cargo test` — all green; focus on `ctx_tests`, `lua_api_test`,
   `compact_test`.
2. TUI smoke (`cargo run`):
   - `/history` — picker lists conversations and loads a transcript (`lib/history` +
     `ctx.ui.pane` + `ctx.log`).
   - `/compact` — runs to completion (`agent.run`, `conversation.history`, `config.get`,
     `ctx.log`).
   - `/customize` — exercises merged `ctx.shell` streaming (`on_line`) + `ctx.log`.
   - Trigger `conversation_history` tool and an `ask_user` flow.
3. Final grep (must be empty):
   ```
   grep -rnE "ctx\.(session\.|shell_streaming|emit_pane|ui\.notify|ui\.status|agent\.run_stream|config\.get_table|fs\.(exists|is_file|is_dir|metadata))" defaults src tests
   ```
