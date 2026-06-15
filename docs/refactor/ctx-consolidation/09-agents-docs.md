# Step 09 — Rewrite `defaults/AGENTS.md` API reference

**Goal:** `AGENTS.md` is the user-facing source of truth for the `ctx` API. Make it match the
new surface. Depends on Steps 02-07.

**File:** `defaults/AGENTS.md`

## API reference table (lines 76-131)

- **`ctx.log.*`** (81-85): keep. Note it is now available in event handlers too.
- **`ctx.fs.*`** (86-91): replace the 4 rows (`exists`, `is_file`, `is_dir`, `metadata`) with
  one `ctx.fs.stat(path)` → `{path, kind, len, readonly}` or `nil`. Keep `read_dir`.
- **Shell** (92-94): collapse to one row `ctx.shell(cmd, opts?)` → `{stdout, stderr, exit_code}`;
  document `opts = { timeout_ms?, on_line? }` where `on_line(line)` is called per stdout line.
  Remove the `shell_streaming` row.
- **`ctx.ui.*`** (99-103): remove `ui.notify` and `ui.status` rows. Keep `ui.pane`, `ui.interact`.
- **Live events** (104-105): remove the `emit_pane` row (folded into `ui.pane`).
- **`ctx.agent.*`** (115-120): remove `run_stream` row; document `ctx.agent.run(prompt, opts?)`
  accepting the `on_*` callbacks (list them) as the streaming form.
- **`ctx.config.*`** (121-124): replace two rows with `ctx.config.get(section, key?)` →
  value when `key` given, whole-section table when omitted. Remove `get_table`.
- **`ctx.session.*`** (125-128): delete the whole block. Add a short note pointing to
  `ctx.db.query` and the `lib/history` helper for conversation history.
- **`ctx.conversation.*`** (129-131): keep — now the single "current conversation" accessor.

## Context Availability table (lines 137-157)

- Merge `ui.notify` / `ui.status` / `emit_pane` rows: row `ctx.log` = yes for tool, command,
  **and event** handlers; `ui.pane` = tool + command (sender-gated).
- Remove the `session` row; keep `conversation`.
- Update the closing paragraph (157): event handlers get `config_dir`, `ctx.log`, `config.dir`.

## Prose sections

- `ctx.conversation` (161-176): unchanged content, but confirm no `session` cross-reference.
- Shell Options (178-184): retitle to cover the single `ctx.shell`; document `on_line` and the
  unified 120s default (clamp 1s-300s).
- Search the rest of the file for every stale reference and update inline examples:
  `ctx.session.*`, `ctx.shell_streaming`, `ctx.agent.run_stream`, `ctx.emit_pane`,
  `ctx.ui.notify/status`, `ctx.config.get_table`, `ctx.fs.exists/is_file/is_dir/metadata`
  (the overview grep lists their line numbers).

## Verify

- `grep -nE "session\.|shell_streaming|run_stream|emit_pane|ui\.notify|ui\.status|get_table|fs\.(exists|is_file|is_dir|metadata)" defaults/AGENTS.md`
  → nothing.
