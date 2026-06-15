# Step 03 — Merge `shell_streaming` into `shell` (Rust)

**Goal:** One `ctx.shell(cmd, opts?)`; streaming becomes an optional `opts.on_line` callback.

**File:** `src/ext/ctx.rs`

## Changes

1. Rewrite `shell_fn` (ctx.rs:246) to accept `(command, opts)` where `opts` may contain:
   - `timeout_ms` (existing)
   - `on_line` : `Option<mlua::Function>` (new)

2. Branch on `on_line`:
   - **absent** → current `run_script` path (ctx.rs:256-273). Default timeout 120s.
   - **present** → the line-by-line reader-thread logic currently in `shell_streaming`
     (ctx.rs:290-385): spawn bash, reader thread over stdout, `recv_timeout` loop calling
     `on_line(line)` per line, background stderr drain, kill on timeout. Returns the same
     `{stdout, stderr, exit_code}`.

3. Unify the timeout default at 120s, clamp 1s–300s for both paths (was 120s plain /
   300s streaming). Keep the clamp helper inline.

4. Delete the separate `shell_streaming` registration (ctx.rs:280-387) and its
   `ctx.set("shell_streaming", ...)`.

## Notes

- The reader-thread code uses `std::process::Command` + threads + `mpsc`; lift it verbatim
  into the `on_line` branch. The `callback.call::<()>(line)` becomes `on_line.call::<()>(line)`.
- New API shape: `ctx.shell(cmd, { timeout_ms = 60000, on_line = function(l) ... end })`.

## Verify

- `cargo build` succeeds.
- `grep -n "shell_streaming" src/ext/ctx.rs` → nothing.
- Plain `ctx.shell("echo hi")` returns `{stdout="hi\n", exit_code=0}`.
- `ctx.shell("printf 'a\\nb\\n'", { on_line = cb })` invokes `cb` twice.
