# TUI → Driver Cutover (Step 3) — complete

Goal: the live TUI runs the core `Driver` instead of its own duplicated loop. This is complete: the Driver path is now the only TUI turn path, and the old loop has been deleted.

## Done & verified
- [x] Driver emits rich `RuntimeEvent` stream (TextDelta/Reasoning/tool/usage)
- [x] Driver forwards live tool panes + `ctx.ui.interact` (via `ReplyRegistry`)
- [x] `ChannelApprovalGate` interactive approval (headless tests)
- [x] Driver cancellation + `DriverOutcome` (state hand-back)
- [x] `App.llm` migrated to `Arc<dyn LlmProvider>` (shareable with Driver)
- [x] Exposed public `SessionWriter` for future RPC use
- [x] Decision: Driver uses `NullSessionSink`; App persists in the pump via its own DB helpers, avoiding session-seq collisions with the live conversation
- [x] Added `arguments: Value` to `RuntimeEvent::ToolCall`
- [x] Added `content: String` to `RuntimeEvent::ToolResult`
- [x] Pump builds Driver from clones (tools/extensions) + Arc llm/session + channels
- [x] Pump `select!` loop renders events, handles approval via channel, panes, Ctrl+C cancel
- [x] Pump reabsorbs `DriverOutcome` (transcript, token_stats, tools)
- [x] `pump_apply_event` helper + trailing-event drain (final text/Finished)
- [x] `pump_tick` redraw never touches the Lua VM (no apply_view_diffs)
- [x] Fix: Esc lag — drain keys at top of loop + persistent `ticker` interval; Ctrl+C cancels streaming
- [x] Fix: `ctx.ui.interact` (ask_user) — real `PanePage::from_interact` pane + `pending_interacts` reply routing via `ReplyRegistry`
- [x] Fix: edit_file diff preview (`pump_show_edit_preview` in approval branch, shown in Safe + Danger; deduped against later ToolResult row)
- [x] Fix: double blank line after user message on tool-first turns (`pump_ensure_assistant` creates the Assistant placeholder before tool rows)
- [x] Per-turn DB persistence (user at start; assistant/tool diff of the Driver's returned transcript at end, via `append_assistant_to_db`/`append_tool_result_to_db`)
- [x] Fix: Driver records the final assistant message in its transcript so `/history` includes complete turns
- [x] Driver path made default by deleting the old toggle and renaming the Driver submission method to `submit_user_turn`
- [x] Deleted duplicated loop: `consume_stream`/`handle_tool_calls`/`execute_tools_responsive`/`dispatch_before_turn_responsive`/`wait_for_stream` and the old `submit_user_turn` body
- [x] Collapsed `AgentEvent` out of the Driver path; headless event senders now use `RuntimeEvent` (`AgentRunEvent` remains a compatibility type alias)
- [x] TUI handles live `RuntimeEvent::TokenUsage`; text streaming remains through `RuntimeEvent::TextDelta`
- [x] Fixed bottom-pane flicker caused by clearing the whole inline viewport on each spinner redraw
- [x] `cargo build` green
- [x] `cargo test` green
- [x] Ratatui-free core gate green (only pre-existing unrelated warnings when present)

## Deferred follow-ups
- Fully remove the `AgentRunEvent` compatibility alias once Lua/subagent public APIs are migrated to the `RuntimeEvent` name.
- Decide whether/how to surface `RuntimeEvent::ReasoningDelta` visibly in the TUI; reasoning is preserved in the Driver transcript today.
- Continue future runtime/RPC work from `docs/refactor/tui-runtime-decoupling.md`.
