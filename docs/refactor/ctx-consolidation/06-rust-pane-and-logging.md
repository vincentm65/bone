# Step 06 — Remove `emit_pane`; unify logging to `ctx.log` (Rust)

**Goal:** One pane emitter (`ctx.ui.pane`) and one logging surface (`ctx.log.*`).

**Files:** `src/ext/ctx.rs`, `src/ext/types.rs`

## Part A — pane (ctx.rs)

1. Delete `ctx.emit_pane` (ctx.rs:1088-1100). `ctx.ui.pane` (ctx.rs:451) is the superset:
   both build a `PanePage` from the table and send `ToolLiveEvent::Pane` over the same
   `pane_sender`. `ctx.ui.pane` already falls back to `(false, "pane unavailable")` when no
   sender, so behavior is preserved everywhere.
2. Keep `cfg.call_id` / `ctx.call_id` (ctx.rs:1085) — unrelated to emit_pane.

## Part B — logging (ctx.rs)

1. Delete `ui.notify` (ctx.rs:434-442) and `ui.status` (ctx.rs:445-449) from `ui_table`.
2. `ctx.log` (ctx.rs:164-174) is the single logger and is already built unconditionally.
   Confirm its levels map: `warn`/`error` → stderr; `debug`/`info` → stderr (current
   behavior prints all four with a `[level]` prefix). Keep as-is.
3. `ui_table` retains only `pane` and `interact`.

## Part C — event-handler ctx (types.rs)

The event/`before_turn` minimal ctx is built separately by `create_event_ctx`
(`src/ext/types.rs:612`), which currently installs only `ui.notify`. Replace it with a
`ctx.log` table so event handlers keep a logger:

1. In `create_event_ctx` (types.rs:612-628), drop the `ui.notify` function and the `ui` table.
2. Add a `log` table with `debug/info/warn/error` functions mirroring ctx.rs:164-174
   (warn/error → `eprintln!` with prefix; debug/info likewise). Set `ctx.set("log", log)`.
3. Update the doc comment at types.rs:611 ("...with ui.notify" → "...with ctx.log").

## Verify

- `cargo build` succeeds.
- `grep -rnE "emit_pane|ui\.(notify|status)" src/ext` → nothing.
- Event handler can call `ctx.log.warn("x")` without error.
- A tool can still `ctx.ui.pane{...}` and `ctx.ui.interact{...}`.

## Docs availability note (for Step 09)

The Context-Availability table in `AGENTS.md` lists `ui.notify` as the only logger available
to event handlers. After this step the row becomes `ctx.log` (yes for events), and
`ui.notify`/`ui.status`/`emit_pane` rows are removed.
