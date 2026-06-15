# Step 04 — Merge `agent.run_stream` into `agent.run` (Rust)

**Goal:** One `ctx.agent.run(prompt, opts?)`; streaming happens when any `on_*` callback is
passed in `opts`.

**File:** `src/ext/ctx.rs` (`add_agent_table`, ctx.rs:1133)

## Changes

1. In `run_fn` (ctx.rs:1149), after parsing opts, extract the callbacks via `opts_cb`
   (ctx.rs:1633): `on_started, on_status, on_tool_call, on_tool_result, on_token_usage,
   on_finished, on_failed`.

2. If **all callbacks are nil** → keep the current plain path (ctx.rs:1171-1198):
   `event_sender: None`, blocking `run_agent` in `select!` with cancel + inactivity.

3. If **any callback present** → use the streaming path currently in `run_stream_fn`
   (ctx.rs:1241-1326): create the mpsc channel, `event_sender: Some(tx)`, the
   `tokio::select!` loop with `dispatch_event(...)` (ctx.rs:1639). Reuse `dispatch_event`
   unchanged.

4. Delete `run_stream_fn` registration (ctx.rs:1209-1328) and `agent_table.set("run_stream", ...)`.

5. Collapse the opt-key lists: replace `RUN_OPT_KEYS` (ctx.rs:1510) and `RUN_STREAM_OPT_KEYS`
   (ctx.rs:1517) with a single `RUN_OPT_KEYS` that includes the callback keys. Update both
   call sites of `parse_agent_opts` for run. `SPAWN_OPT_KEYS` stays.

6. Re-clone captured vars (`inherited_*`, `agent_depth`, `cancelled_flag`) as needed now that
   one closure does both jobs; the second set of `_s` clones (ctx.rs:1204-1208) is removed.

7. `spawn`, `jobs`, `wait` unchanged.

## Verify

- `cargo build` succeeds.
- `grep -n "run_stream" src/ext/ctx.rs` → nothing.
- `ctx.agent.run("hi")` returns `{ok, content, error}`.
- `ctx.agent.run("hi", { on_status = cb })` drives `cb` and still returns the final table.
