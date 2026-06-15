# Step 02 — Remove `ctx.session.*` (Rust)

**Goal:** Delete the `ctx.session` namespace; `ctx.db.query` + `lib/history` (Step 01) and
`ctx.conversation.current()` replace it.

**File:** `src/ext/ctx.rs`

## Changes

1. Delete the entire `ctx.session` block (`ctx.rs:922-1026`):
   - `session_table` creation
   - `session_current_fn` (ctx.rs:929) — redundant with `conversation.current` (ctx.rs:616),
     which returns the byte-identical `{id, provider, model}`
   - `session_list_fn` (ctx.rs:946)
   - `session_messages_fn` (ctx.rs:975)
   - the `ctx.set("session", session_table)?` line (ctx.rs:1026)

2. Keep `ctx.db.query` (ctx.rs:1029) and its helpers `row_to_lua_value`,
   `tostring_lua_value` (ctx.rs:1871-1915) unchanged.

3. After deletion, `cfg.session_id` / `cfg.provider` / `cfg.model` are still used by
   `ctx.conversation.*` and `ctx.usage` — do not remove those CtxConfig fields.

## Verify

- `cargo build` succeeds.
- `grep -n "ctx.set(\"session\"" src/ext/ctx.rs` → nothing.
- `ctx.conversation.current()` still returns `{id, provider, model}` (unchanged code path).
