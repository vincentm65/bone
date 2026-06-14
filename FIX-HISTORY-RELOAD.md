# Fix Plan: `/history` reload correctness

Two issues from the v2.0.8 regression review. Fix both, then build + test.

## Problem 1: `tool_calls` dropped on reload

`tool_calls` is written (JSON via `serde_json::to_string(&Vec<ToolCall>)` at
stream/mod.rs:291 and agent.rs:633) but never read back. `list_messages`
omits the column and `StoredMessage` has no field for it. Result: reloading a
tool-using conversation orphans the `tool` result messages and breaks the LLM
context round-trip.

### 1.1 `StoredMessage` — add the field
`src/session_db.rs:28`
- Add `pub tool_calls: Option<String>` (raw JSON string, parse in Lua layer).

### 1.2 `list_messages` — select the column
`src/session_db.rs:770`
- Change SELECT to include `tool_calls` (6th column), set it on the row.
- Keep query shape otherwise identical.

### 1.3 Surface in Lua `ctx.session.messages`
`src/ext/ctx.rs:974` (`session_messages_fn`)
- After the existing `tool_call_id` set, if `msg.tool_calls` is `Some(json)`:
  parse it to a Lua table and `t.set("tool_calls", table)`.
- Shape must match what `conversation.replace`/`conversation.load` parsing in
  `src/ext/types.rs` (`parse_messages_table`) expects: an array of
  `{ id=, name=, arguments=<json value> }`. Parse the JSON string with
  `serde_json::from_str` → convert via the existing `json_to_lua`/value
  conversion used elsewhere in the file.

### 1.4 Verify history.lua needs no change
`defaults/lua/commands/history.lua` `valid_message` already copies
`msg.tool_calls` through when present — so once 1.3 surfaces it, it flows
into the `conversation.load` payload unchanged. Confirm only.

### 1.5 Test
`src/session_db_tests.rs`
- Extend `max_message_seq_tracks_highest_seq` (or add a new test) to store an
  assistant message with a `tool_calls` JSON string and assert
  `list_messages` returns it.

---

## Problem 2: `load_conversation` / `clear_chat` leak stale state

`queue` and `active_prompt` aren't cleared, so queued input or a stale
approval prompt from the prior conversation carries into the loaded one.

### 2.1 `load_conversation`
`src/ui/app/mod.rs:~373` (right after the existing clears)
- Add `self.queue.clear();`
- Add `self.active_prompt = None;`

### 2.2 `clear_chat`
`src/ui/app/mod.rs:460` (same place, mirror it)
- Add `self.queue.clear();`
- Add `self.active_prompt = None;`

Note: `shown_tool_rows` auto-clears at next turn start (stream/mod.rs:163) and
`autocomplete` is cosmetic — skip both, keep the fix minimal.

---

## Order of work

1. Problem 1 (1.1 → 1.2 → 1.3 → 1.5), then verify 1.4.
2. Problem 2 (2.1 → 2.2).
3. `cargo build` + `cargo test --lib`.
4. Stash WIP first so the run reflects only these changes; restore after.

## Files touched
- src/session_db.rs
- src/session_db_tests.rs
- src/ext/ctx.rs
- src/ui/app/mod.rs
