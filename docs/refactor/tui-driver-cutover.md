# TUI → Driver Cutover (Step 3) — remaining checklist

Goal: the live TUI runs the core `Driver` instead of its own loop, gated behind
`BONE_DRIVER=1` so the proven path stays default until verified. Then delete the
~500 LOC of duplicated loop.

## Already done & verified (context)
- [x] Driver emits rich `RuntimeEvent` stream (TextDelta/Reasoning/tool/usage)
- [x] Driver forwards live tool panes + `ctx.ui.interact` (via `ReplyRegistry`)
- [x] `ChannelApprovalGate` interactive approval (headless tests)
- [x] Driver cancellation + `DriverOutcome` (state hand-back)
- [x] `App.llm` migrated to `Arc<dyn LlmProvider>` (shareable with Driver)

## Remaining

### 1. App `SessionSink` — DONE
- [x] Expose a public `SessionWriter` (for future RPC use)
- [x] Decision: Driver uses `NullSessionSink`; App persists in the pump via its
      own DB helpers, avoiding session-seq collisions with the live conversation

### 2. Protocol enrichment (for faithful tool-row rendering) — DONE
- [x] Add `arguments: Value` to `RuntimeEvent::ToolCall`
- [x] Add `content: String` to `RuntimeEvent::ToolResult`
- [x] Update Driver emissions + `From<AgentRunEvent>` + round-trip test
- [x] build + `cargo test` green

### 3. The pump — `submit_user_turn_via_driver` (gated) — CODE DONE
- [x] Build Driver from clones (tools/extensions) + Arc llm/session + channels
- [x] `select!` loop: render events, approval via channel, panes, Esc→cancel
- [x] Reabsorb `DriverOutcome` (transcript, token_stats, tools)
- [x] Toggle in `submit_user_turn`: `BONE_DRIVER` set → new path, else old
- [x] `pump_apply_event` helper + trailing-event drain (final text/Finished)
- [x] `pump_tick` redraw that never touches the Lua VM (no apply_view_diffs)
- [x] build + `cargo test` green
- [x] Fix: Esc lag — drain keys at top of loop + persistent `ticker` interval;
      immediate break/cancel on Esc (was: recreated sleep starved by fast stream)
- [x] Fix: `ctx.ui.interact` (ask_user) — real `PanePage::from_interact` pane +
      `pending_interacts` reply routing via `ReplyRegistry` (was: silent cancel)
- [x] Fix: edit_file diff preview (`pump_show_edit_preview` in approval branch,
      shown in Safe + Danger; deduped against the later ToolResult row)
- [x] Fix: double blank line after user message on tool-first turns
      (`pump_ensure_assistant` creates the Assistant placeholder before tool rows)
- [x] **USER verified**: edit preview shows; single blank after user message;
      ask_user picker works; approve/advise/deny
      (accepted: Esc doesn't stop stream — Ctrl+C does)

### 4. Toward default + delete duplication
- [x] Per-turn DB persistence (user at start; assistant/tool diff of the Driver's
      returned transcript at end, via `append_assistant_to_db`/`append_tool_result_to_db`)
- [x] Fix: Driver now records the final assistant message in its transcript
      (was lost → /history showed only user messages)
- [x] **USER verified** `/history` shows a `BONE_DRIVER` turn after restart
      (+ history.lua timestamps converted UTC→local)
- [ ] Soak: keep using `BONE_DRIVER=1` for real work to build confidence
- [ ] Make Driver path default (flip toggle; add `BONE_LEGACY` escape hatch)
- [ ] Delete duplicated loop: `consume_stream`/`handle_tool_calls`/
      `execute_tools_responsive`/`dispatch_before_turn_responsive`/`wait_for_stream`
      (~500 LOC) + the old `submit_user_turn` body
- [ ] Collapse `AgentRunEvent`/`AgentEvent`/`emit_event` into `RuntimeEvent`
- [ ] build + `cargo test` green; ratatui-free core gate green
