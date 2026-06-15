# Step 08 — Migrate bundled defaults to the new surface

**Goal:** Update the 6 bundled tools/commands that use removed APIs. Depends on Steps 01-07.

## `defaults/lua/tools/conversation_history.lua`

- Add at top: `local history = require("lib.history")`.
- `load_messages` (line 34): `ctx.session.messages(id, {limit=1000})` →
  `history.messages(ctx, id, 1000)`.
- `first_user_summary` (line 63): `ctx.session.messages(id, {limit=80})` →
  `history.messages(ctx, id, 80)`.
- `execute` (line 87): drop the `ctx.session` availability guard (line 88); replace
  `ctx.session.list({limit=100})` (line 92) → `history.list(ctx, 100)`.
- `ctx.ui.pane` (line 125) stays.
- Note: rows now have `tool_name`/`tool_call_id` columns (same as before); the message shape
  built at line 39 is unchanged.

## `defaults/lua/commands/history.lua`

- Add `local history = require("lib.history")`.
- `ctx.session.list` (line 71) → `history.list(ctx, ...)`; `ctx.session.messages` (86, 125)
  → `history.messages(ctx, ...)`.
- `ctx.ui.notify(msg, level)` (lines 73, 78, 107, 111, 115, 121, 127, 140) →
  `ctx.log.info/warn/error(msg)` per level.
- `ctx.ui.pane` (line 104) stays.

## `defaults/lua/commands/compact.lua`

- `ctx.ui.notify` (lines 206, 212, 294) → `ctx.log.*`.
- `ctx.conversation.history` / `ctx.agent.run` / `ctx.config.get` unchanged (config.get with
  a key still works after Step 05).

## `defaults/lua/tools/ask_user.lua`

- `ctx.ui.notify` (lines 226, 244) → `ctx.log.*`.
- `ctx.ui.pane` (225, 233) stays.

## `defaults/lua/commands/customize.lua`

- `ctx.shell_streaming(cmd, cb, opts)` (line 89) → `ctx.shell(cmd, { on_line = cb, <opts> })`.
- `ctx.ui.notify` / `ctx.ui.status` (line 97) → `ctx.log.*`.
- `ctx.agent.run` (92) unchanged.

## `defaults/lua/commands/memory.lua`

- `ctx.fs.is_file(p)` (lines 9, 38) → `local s = ctx.fs.stat(p); ... s and s.kind == "file"`
  (see Step 07 idioms). `ctx.shell` calls unchanged.

## No change

`review.lua`, `web_search.lua`, `cron.lua` — only plain `ctx.shell`.

## Verify

- `grep -rnE "ctx\.(session\.|shell_streaming|ui\.notify|ui\.status|fs\.is_file)" defaults/lua`
  → nothing.
- Manual TUI smoke covered in Step 10.
