# ctx API Consolidation — Overview

## Context

The Lua `ctx` table (`src/ext/ctx.rs`) has accumulated narrow, single-purpose helpers
that duplicate a more general primitive or each other. The clearest example:
`ctx.session.list()` / `ctx.session.messages()` are two hard-coded SELECTs that the
general `ctx.db.query()` primitive already covers.

This refactor collapses ~12 narrow API entries into a smaller set of broad primitives,
following the **neovim model**: Rust is the stable backbone of broad primitives; Lua is
the fully customizable layer where specifics live. Specifics that were hard-coded in Rust
(like "list conversations") move into an overridable Lua stdlib module.

**Decisions (confirmed with user):** breaking changes OK (no back-compat shims); full
sweep; bundled defaults + tests rewritten in the same change.

## Consolidations

| # | Remove | Keep / generalize |
|---|--------|-------------------|
| 1 | whole `ctx.session.*` (`list`, `messages`, `current`) | `ctx.db.query` for history; `ctx.conversation.current()` is the identical "current" |
| 2 | `ctx.shell_streaming` | `ctx.shell(cmd, opts?)` gains optional `opts.on_line` callback |
| 3 | `ctx.agent.run_stream` | `ctx.agent.run(prompt, opts?)` accepts `on_*` callbacks; streams when present |
| 4 | `ctx.config.get_table` | `ctx.config.get(section, key?)` — whole-section table when `key` omitted |
| 5 | `ctx.emit_pane` | `ctx.ui.pane(table)` (already the superset) |
| 6 | `ctx.ui.notify`, `ctx.ui.status` | `ctx.log.{debug,info,warn,error}` as the single logger |
| 7 | `ctx.fs.exists/is_file/is_dir/metadata` | one `ctx.fs.stat(path)` → table or **nil** when missing |

## Feature impact — only bundled Lua breaks

The Rust core, TUI, and session DB do **not** depend on these helpers; only `.lua` files do.

| Tool / command | Removed API used | Step |
|---|---|---|
| `/history` (`commands/history.lua`) | `session.list/messages`, `ui.notify` | 08 |
| `conversation_history` tool | `session.list/messages` | 08 |
| `/compact` (`commands/compact.lua`) | `ui.notify` | 08 |
| `ask_user` tool | `ui.notify` | 08 |
| `/customize` (`commands/customize.lua`) | `shell_streaming`, `ui.notify/status` | 08 |
| `/memory` (`commands/memory.lua`) | `fs.is_file` | 08 |
| `/review`, `web_search`, `cron` | plain `ctx.shell` only | no change |

Unavoidable: user-written Lua calling old names breaks (accepted "breaking OK" tradeoff).

## Step files (execute in order)

| Step | File | What |
|---|---|---|
| 01 | `01-lib-history-module.md` | New `defaults/lua/lib/history.lua` stdlib over `ctx.db.query` |
| 02 | `02-rust-session-removal.md` | Remove `ctx.session.*` from `ctx.rs` |
| 03 | `03-rust-shell-merge.md` | Merge `shell_streaming` into `shell` |
| 04 | `04-rust-agent-run-merge.md` | Merge `agent.run_stream` into `agent.run` |
| 05 | `05-rust-config-merge.md` | Merge `config.get_table` into `config.get` |
| 06 | `06-rust-pane-and-logging.md` | Remove `emit_pane`; unify logging to `ctx.log` (+ event ctx) |
| 07 | `07-rust-fs-stat.md` | Replace 4 fs helpers with `fs.stat` |
| 08 | `08-defaults-migration.md` | Rewrite the 6 affected bundled tools/commands |
| 09 | `09-agents-docs.md` | Rewrite `defaults/AGENTS.md` API reference |
| 10 | `10-tests-and-verify.md` | Update tests; end-to-end verification |

## Recommended ordering

Do **01** first (the stdlib the defaults will need). Then the Rust steps **02–07** (each
compiles independently; the removed APIs just become unused). Then **08–09** to migrate
defaults/docs to the new surface. Then **10** to update tests and verify. A single
`cargo build` after 02–07 and `cargo test` after 10.

## Final verification (after all steps)

```
grep -rnE "ctx\.(session\.|shell_streaming|emit_pane|ui\.notify|ui\.status|agent\.run_stream|config\.get_table|fs\.(exists|is_file|is_dir|metadata))" defaults src tests
```
must return nothing. Then `cargo test` green, and manual TUI smoke of `/history`,
`/compact`, `/customize`.
