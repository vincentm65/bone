# Plan: Remove the old (non-Driver) turn path

## Context

The TUI now runs turns through the core `Driver` by default. The previous duplicated non-Driver TUI turn loop has been deleted, leaving one shared turn loop for TUI and headless paths.

**Guiding principle:** let the compiler find dead code. After the old turn body was removed, `cargo build` surfaced the functions/types that only the old path referenced. Those were deleted in small batches and the build/test suite was kept green.

## What was old-path-only and deleted

- Old `submit_user_turn` body and environment toggle.
- `consume_stream`, `wait_for_stream`.
- `handle_tool_calls`, `execute_tools_responsive`, `prepare_tool_call`, `show_immediate_tool_rows`, `wait_for_tool_future_live`.
- `dispatch_before_turn_responsive`.
- `mark_cancelled`.
- Old-only helpers/types/consts: `StreamFailure`, `PendingTool`, `INITIAL_RESPONSE_TIMEOUT`, `STREAM_IDLE_TIMEOUT`, `MAX_PROVIDER_ATTEMPTS`, `call_row_shown_during_prepare`, `show_immediate_tool_row`, `timeout_message`, `assistant_message`.

## What remains shared

- `drive_live` for slash commands, plus live-event helpers `apply_tool_live_event`, `apply_and_track`.
- `prompt_and_wait`, `drain_keys`.
- Streaming/scrollback rendering: `flush_streaming_message`, `finalize_streaming_message`, `flush_new_to_scrollback`, `flush_separator`.
- `build_tool_row`, `tool_error`, `run_inline_command`.
- all `pump_*` methods (the Driver-backed TUI path).

## Steps

### Phase A — Make Driver the default, drop the toggle ✅
`src/ui/app/stream/mod.rs`:
- Deleted the environment-gated branch and the entire old `submit_user_turn` body.
- Renamed the Driver-backed submission method to `submit_user_turn` (kept the same signature; call sites in `app/mod.rs` unchanged).
- No legacy escape hatch.

### Phase B — Compiler-driven dead-code sweep ✅
Deleted old-path-only code (563 LOC), `stream/mod.rs` reduced from ~1699 to ~846 lines:
- Methods: `mark_cancelled`, `consume_stream`, `redraw_streaming_tokens`, `handle_tool_calls`, `show_immediate_tool_rows`, `dispatch_before_turn_responsive`, `execute_tools_responsive`, `wait_for_tool_future_live`, `prepare_tool_call`, `wait_for_stream`.
- Helpers: `call_row_shown_during_prepare`, `show_immediate_tool_row`, `timeout_message`, `assistant_message`.
- Types/consts: `StreamFailure` (+impl), `PendingTool`, `INITIAL_RESPONSE_TIMEOUT`, `STREAM_IDLE_TIMEOUT`, `MAX_PROVIDER_ATTEMPTS`.
- Imports: `EventDispatchResult`, `CommandSafety`, `LlmError`, `LlmErrorKind`, `StatusInfo`, `StreamExt`, `Decision`, `Reasoning` (kept `Tool` trait import — needed for `ShellTool.execute()`).

### Phase C — Verify ✅
- `cargo build` — green.
- `cargo test` — green.
- Test files cleaned: `tests/stream_test.rs` reduced to the `tool_error` test; `tests/stream_tools_test.rs` deleted the `StreamFailure`/`timeout_message` tests and imports.
- Ratatui-free core gate: `cargo check --lib --no-default-features` — green, with only pre-existing unrelated warnings when present.
- `cargo clippy --all-targets` — no new findings (only pre-existing style nits when present).
- Interactive smoke: user verified the main parity matrix; input/status flicker was found and fixed by removing the full bottom-pane clear on spinner redraw.

### Phase D — Collapse duplicate event types ✅
- `Driver` now emits `RuntimeEvent` directly into both the rich frontend stream and the headless event sender path.
- Removed the separate in-process `AgentEvent` enum.
- Kept `AgentRunEvent` as a compatibility type alias to `RuntimeEvent` for existing Lua/subagent/tests APIs.
- Preserved headless `bone run --events` JSONL output shape in `agent::emit_event` by encoding the relevant `RuntimeEvent` variants to the legacy JSONL records.
- Removed the `RuntimeEvent: From<AgentRunEvent>` bridge because the two names now refer to the same event type.

### Phase E — Docs & memory cleanup ✅
- Updated `docs/refactor/tui-driver-cutover.md` to mark the cutover complete and remove stale toggle instructions.
- Updated this plan doc to describe the completed state.
- Updated memory to note that the TUI now consumes the Driver and the old loop is gone.

## Deferred follow-ups

- Fully remove the `AgentRunEvent` compatibility alias once public Lua/subagent/test APIs are migrated to the `RuntimeEvent` name.
- Decide whether/how to display `RuntimeEvent::ReasoningDelta` in the TUI. Reasoning is retained in Driver transcript today, but not rendered as a visible pane.
- Continue future runtime/RPC work from `docs/refactor/tui-runtime-decoupling.md`.

## Outcome

One turn loop (the `Driver`) shared by the TUI and headless `run_agent`; old TUI duplication removed; event plumbing consolidated around `RuntimeEvent`; text streaming remains through `RuntimeEvent::TextDelta`.