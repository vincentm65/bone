# Plan: Remove the old (non-Driver) turn path

## Context

The TUI now runs turns through the core `Driver` via `submit_user_turn_via_driver`,
gated behind `BONE_DRIVER=1`. The new path has been soaked and verified at parity
(streaming, approval accept/advise/deny, edit previews, panes, `ask_user`, cancel
via Ctrl+C, DB persistence, `/history`, message spacing). This plan flips the
Driver path to default and deletes the now-duplicated old loop (~500 LOC), leaving
one turn loop in the codebase.

**Guiding principle:** let the compiler find dead code. After the old
`submit_user_turn` body is gone, `cargo build` emits `never used` warnings for
exactly the functions/types that only the old path referenced. Delete those,
rebuild, repeat — the build breaks immediately if anything shared is touched.

## What is old-path-only vs shared (verified by call-site audit)

**Old-path-only — safe to delete** (each called only from the old loop):
- `submit_user_turn` old body (the loop after the `BONE_DRIVER` check)
- `consume_stream`, `wait_for_stream`
- `handle_tool_calls`, `execute_tools_responsive`, `prepare_tool_call`,
  `show_immediate_tool_rows`, `wait_for_tool_future_live`
- `dispatch_before_turn_responsive`
- `mark_cancelled`
- Old-only helper types/consts: `StreamFailure`, `PendingTool`,
  `MAX_PROVIDER_ATTEMPTS` (confirm via compiler — delete only if unused).

**Shared — MUST KEEP** (used by the new pump and/or the command path `drive_live`):
- `drive_live` (used by `run_lua_command` for slash commands), and its live-event
  helpers `apply_tool_live_event`, `apply_and_track`
- `prompt_and_wait` (new pump approval branch), `drain_keys` (pump + drive_live)
- `redraw_streaming_tokens`, `redraw_streaming_message`,
  `finalize_streaming_message`, `flush_new_to_scrollback`, `end_turn_separator`
- `build_tool_row`, `tool_error`, `assistant_message`, `run_inline_command`
- all `pump_*` methods (the new path)

## Steps

### Phase A — Make Driver the default, drop the toggle
`src/ui/app/stream/mod.rs`:
- Delete the `if std::env::var_os("BONE_DRIVER")…` check **and** the entire old
  `submit_user_turn` body below it.
- Rename `submit_user_turn_via_driver` → `submit_user_turn` (keep the same
  signature and the two call sites in `app/mod.rs:632,1557` unchanged).
- `src/main.rs`: no change (no `BONE_DRIVER` reference there).
- Decision: **no `BONE_LEGACY` escape hatch** — we're deleting the old code, so
  there's nothing to fall back to. (If a safety net is wanted, do Phase A alone
  first — default flip with the old body still present behind `BONE_LEGACY` — and
  delete in a follow-up once confident. Default recommendation: delete now, since
  it's already soaked.)

### Phase B — Compiler-driven dead-code sweep
- `cargo build` → collect every `function/method 'X' is never used` warning.
- Delete the flagged old-path-only items from the **KEEP-list-excluded** set
  above. Work in small batches; rebuild after each so new warnings surface
  (e.g. deleting `handle_tool_calls` makes `prepare_tool_call` /
  `execute_tools_responsive` / `show_immediate_tool_rows` newly dead).
- Delete any now-unused helper types/consts/imports the warnings point to.
- Stop when `cargo build` is warning-clean. **Never silence a warning for a
  KEEP-list function — that means a real wiring bug.**

### Phase C — Verify (no regressions)
- `cargo build` — zero warnings.
- `cargo test` — green (the `subagent_pane` test is a known parallel flake; re-run
  it alone if it trips).
- Ratatui-free core gate: `cargo check --lib --no-default-features` — green.
- `cargo clippy --all-targets` — no new dead-code/unused findings.
- **Interactive smoke (with the user, in the terminal):** normal turn streams;
  tool call → approve / advise / deny; `edit_file` shows a diff preview;
  `ask_user` picker works; Ctrl+C cancels mid-stream and mid-tool; `ctx.ui.pane`
  panes render; single blank-line spacing holds; `/history` (save + recall),
  `/config`, `/clear`, model/provider switch all still work.

### Phase D — Collapse the duplicate event types (separate, optional)
Independent of A–C; can be its own commit.
- Fold `AgentRunEvent` / `AgentEvent` / `emit_event` (`src/agent.rs`) into the one
  `RuntimeEvent` (`src/runtime/event.rs`).
- Caveat: the headless JSONL path (`bone run --events`, `agent.rs::emit_event`)
  still serializes `AgentRunEvent` to stdout. Either (a) keep a thin JSONL encoder
  over `RuntimeEvent`, or (b) leave `AgentRunEvent` solely as the JSONL DTO and
  drop only the in-process `AgentRunEvent` channel. Pick when doing this phase.
- Update `Driver` to emit only `RuntimeEvent`; `run_agent` maps to JSONL.

### Phase E — Docs & memory cleanup
- Remove `BONE_DRIVER` mentions from `docs/refactor/tui-driver-cutover.md`
  (mark Step 3 complete) and any READMEs.
- Update memory `neovim-refactor`: the TUI now consumes the `Driver`; the old loop
  is gone; note remaining deferred items (RPC `--remote` TUI client, live
  `bone.api.ui` into the running render, `ApiCall` over RPC, Phase 7 menu ports).

## Rollback
Phases A–B are a single mechanical change on a branch. If the interactive smoke
(Phase C) surfaces a problem, `git revert`/reset the branch — the old path returns
intact. Do not merge to `main` until the smoke matrix passes.

## Outcome
One turn loop (the `Driver`) shared by the TUI and headless `run_agent`; ~500 LOC
of duplication removed; `stream/mod.rs` reduced to the pump + shared
rendering/approval/command helpers.
