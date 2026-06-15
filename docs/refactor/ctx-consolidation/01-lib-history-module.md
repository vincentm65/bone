# Step 01 — Lua stdlib: `lib/history.lua`

**Goal:** Move the hard-coded conversation-history SQL out of Rust into an overridable Lua
module, so `ctx.session.*` can be deleted while keeping the convenience for consumers.

**Why neovim-style:** the module ships as a default but lives in `<config_dir>/lua/lib/`,
so users can override it like any other Lua file. `require` resolves it with no loader
change — `package.path` already includes `<config_dir>/lua/?.lua` (`src/ext/engine.rs:58`).

## New file: `defaults/lua/lib/history.lua`

Helpers take `ctx` as an argument because `ctx` is per-call, not a global.

```lua
-- lib/history — conversation history helpers over the ctx.db.query primitive.
-- Replaces the former ctx.session.list / ctx.session.messages Rust helpers.
local M = {}

-- List recent conversations, newest first.
-- Returns array of { id, provider, model, started_at, ended_at }.
function M.list(ctx, limit)
  limit = math.max(1, math.min(limit or 20, 100))
  return ctx.db.query(
    "SELECT id, provider, model, started_at, ended_at " ..
    "FROM conversations ORDER BY id DESC LIMIT ?",
    { limit }
  ) or {}
end

-- Messages for a conversation in chronological order.
-- Returns array of { seq, role, content, tool_name, tool_call_id, tool_calls }.
-- Note: tool_calls is the raw JSON string from the DB; decode with cjson if needed.
function M.messages(ctx, id, limit)
  limit = math.max(1, math.min(limit or 200, 1000))
  return ctx.db.query(
    "SELECT seq, role, content, tool_name, tool_call_id, tool_calls " ..
    "FROM messages WHERE conversation_id = ? ORDER BY seq ASC LIMIT ?",
    { id, limit }
  ) or {}
end

return M
```

## Schema reference (from `src/session_db.rs:209-225`)

- `conversations(id, started_at, ended_at, provider, model)`
- `messages(conversation_id, role, content, tool_name, tool_call_id, tool_calls, seq)`

These match the columns the old `list_conversations` / `list_messages` (session_db.rs:779,
801) selected, so consumers get the same shapes — **except** `tool_calls` is returned as
the raw JSON string (the old Rust `session.messages` pre-decoded it into a Lua array). The
one consumer that read messages (`conversation_history.lua`) only uses `role/content/
tool_name/tool_call_id`, so this is safe. If a future consumer needs decoded tool_calls,
`cjson.decode(row.tool_calls)`.

## Verify

- `defaults/lua/lib/history.lua` exists and `require("lib.history")` works from a default
  (verified indirectly by Step 08's consumers; a quick check: add a temporary
  `local h = require("lib.history")` in a scratch command and confirm no load error).
