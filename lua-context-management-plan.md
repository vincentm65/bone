# Lua-Defined Context Management Plan

## Summary

Implement manual `/compact` and automatic context reduction as Lua-owned behavior. Rust must not add any compaction-specific command, type, function, config key, branch, or policy. Rust only gains generic Lua extension capabilities:

- A generic active-conversation snapshot API for Lua.
- A generic conversation replacement action returned by Lua commands/hooks.
- A generic `before_turn` Lua hook that can return actions before the provider request is built.

The default `/compact` command and auto behavior live in `defaults/lua/commands/compact.lua`. Users can remove or edit that Lua file to disable or customize behavior.

## Key API Changes

Add generic Lua APIs, with no Rust-side compaction naming:

- `ctx.conversation.current() -> table|nil`
  - Returns `{ id, provider, model }` for the active conversation.
  - Mirrors `ctx.session.current()` but belongs to the active runtime conversation surface.

- `ctx.conversation.history() -> array`
  - Returns the current in-memory provider transcript, not the SQLite history.
  - Each item is a plain Lua table: `{ role = "user"|"assistant"|"tool", content = string, name?, tool_call_id? }`.
  - The system prompt is not included because Rust already prepends it in `build_chat_history`.

- Lua command/hook return action:
  - A returned table may include `action = "conversation.replace"`.
  - It must include `messages = { ... }`, using the same message shape as `ctx.conversation.history()`.
  - Rust validates roles and content, replaces the active transcript, recomputes the context estimate, and leaves the SQLite session history intact.
  - Optional `display`/`reply`/`content` still shows a UI message.
  - Optional `submit` keeps current command semantics.

- New generic hook event:
  - `bone.on("before_turn", function(event, ctx) ... end)`
  - Runs after the user message is appended and before provider history is built.
  - Receives full command/tool-style ctx, including `usage`, `state`, `agent`, `tools`, `config`, and `conversation`.
  - May return `nil` or an action table.
  - If multiple handlers return actions, apply them in registration order, rebuilding ctx history between handlers if practical; otherwise document that each handler sees the initial snapshot and actions apply sequentially.

## Implementation Details

- Extend `CtxConfig` with an optional active conversation snapshot:
  - `conversation_id`, `provider`, and `model` can reuse existing fields.
  - Add `conversation_history: Option<Vec<ChatMessage>>`.
  - `create_ctx_table` adds `ctx.conversation` only when a snapshot is provided.

- Add a generic Lua result parser in the UI command path:
  - Parse string/nil/table as today.
  - For tables, additionally parse `action` and `messages`.
  - Return an internal generic result struct such as `{ display, submit, action }`.
  - Do not name this struct or any Rust method after compaction.

- Add a generic action applier on `App`:
  - Support only `conversation.replace` for v1.
  - Validate all replacement messages before mutating state.
  - Replace `self.transcript` with validated `ChatMessage`s.
  - Replace visible chat messages with a concise system/display message only if Lua supplied one; do not try to recreate the full prior UI.
  - Recompute `token_stats.context_length` using `App::estimate_context_chars(build_chat_history(&self.transcript, None), &self.tools.definitions())`.
  - Do not alter cumulative sent/received/cost/request counts.

- Add `before_turn` support:
  - Add `"before_turn"` to the Lua event registry.
  - Implement a new dispatch path that can create full ctx and collect return values.
  - In `submit_user_turn`, call this hook after user message persistence and before `build_chat_history`.
  - Apply returned generic actions before the provider request starts.
  - Keep existing `message` event behavior unchanged.

- Add default Lua command file `defaults/lua/commands/compact.lua`:
  - Register `/compact`.
  - Register a `before_turn` handler for auto behavior.
  - Use a local config table, overridable from `bone.config.context` or similar Lua-owned config:
    - `auto_tokens = 8000`
    - `keep_messages = 12`
    - `summary_target_tokens = 1200`
  - Manual `/compact` always runs unless history is already small.
  - Auto handler runs only when `ctx.usage.snapshot().context_length >= auto_tokens`.
  - Use `ctx.agent.run()` to summarize older messages into a compact summary.
  - Return `action = "conversation.replace"` with:
    - One synthetic user message containing the summary.
    - The last `keep_messages` user/assistant messages, filtering tool messages for v1 safety.
  - Use `ctx.state` to avoid repeated auto runs on the same approximate history size.

- Update docs:
  - Document `ctx.conversation`.
  - Document command/hook return actions.
  - Document `before_turn`.
  - Document that `/compact` and auto context management are implemented entirely in Lua and can be customized or removed from `lua/commands/compact.lua`.

## Test Plan

- Lua API tests:
  - Default Lua commands include `compact`.
  - `ctx.conversation.history()` is available in command ctx and returns active transcript messages.
  - `ctx.conversation` is absent or inert where no active conversation snapshot exists.

- Command action tests:
  - A Lua command returning `action = "conversation.replace"` replaces `App.transcript`.
  - Invalid replacement messages produce a visible error and leave transcript unchanged.
  - Existing command return modes still work: string submit, table display with `submit=false`, nil no-op.

- Hook tests:
  - `before_turn` runs before provider history is built.
  - A hook returning `conversation.replace` changes the history observed by a mock provider.
  - Existing `message` events still fire as before.

- Default Lua behavior tests:
  - `/compact` returns a generic replacement action when history is large.
  - Auto handler does nothing below threshold.
  - Auto handler triggers above threshold and keeps the configured number of recent messages.
  - The default Lua file handles unavailable usage or agent failure by returning a display message without replacing history.

- Regression checks:
  - Protected built-ins remain unchanged; `/compact` remains Lua-overridable.
  - No new Rust command named `compact`.
  - No Rust compaction policy or Rust compaction config is introduced.

## Assumptions

- "Rust should have no mention of compaction" means no new compaction-specific Rust behavior or symbols. Existing unrelated uses of words like `compact_number` in stats code are left alone.
- SQLite conversation history remains append-only for audit/history; context replacement affects only the live provider transcript.
- The default summarizer can use `ctx.agent.run()` from Lua. If that fails, Lua reports the failure and does not mutate context.
- V1 replacement messages preserve plain `user` and `assistant` messages only. Tool-call chain preservation can be added later through the same generic API if needed.
