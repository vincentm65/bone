# Batch 6 Review: `runtime/` and `rpc/` modules

---

## /home/vincent/projects/bone/src/runtime/mod.rs
- **Lines:** 20
- **Assessment:** mostly good
- **Notes:** Minimal re-export facade. Clean module documentation explaining the architecture (Driver owns provider/tools/extensions/sink; `run_agent` is a thin wrapper). No over-engineering. Only suggestion: the doc comment says `agent::run_agent` is a "thin wrapper" — if that wrapper is truly one line, consider inlining and removing the indirection. But this file itself is fine.

---

## /home/vincent/projects/bone/src/runtime/event.rs
- **Lines:** 343
- **Assessment:** mostly good
- **Notes:** Well-documented protocol boundary types (`RuntimeEvent`, `RuntimeCommand`) with serde round-trip tests. The `KeyReplyRegistry` is a standard id-keyed oneshot table — fine. `ChannelApprovalGate` cleanly bridges async approval requests. The `ApprovalRequest` struct is only used inside `ChannelApprovalGate`; consider merging the two (have `ChannelApprovalGate` own its own oneshot internally rather than exposing a public struct with a live sender). The comment about "Phase 5" and "Phase 6" on `ApiCall` is speculative future-ware — that variant adds complexity for something that doesn't exist yet. Minor: `remit` vs `emit` naming in driver.rs suggests the two-step dispatch (JSONL + rich events) could be unified rather than split across files.

---

## /home/vincent/projects/bone/src/runtime/driver.rs
- **Lines:** 704
- **Assessment:** can be simplified
- **Notes:** The largest and most complex file in this batch. The `Driver` struct has 19 fields — that's a lot of configuration threaded through. `run_to_outcome` is a single ~500-line function with deeply nested logic (retry loop, stream consumption, before_turn hook dispatch, usage estimation fallback, tool execution). Specific simplification opportunities:

  1. **Duplicated token-usage emission block** (~50 lines) — the `if !had_usage && !stream_error` block is almost identical to the `ChatEvent::TokenUsage` handler inside the stream loop. Extract into a helper: `fn emit_usage(...)` called from both places.

  2. **`remit` vs `emit_runtime` split** — two closures that do nearly the same thing (one skips the JSONL `emit_event` path). This is fragile; one caller uses `remit` (TextDelta, ReasoningDelta) while another uses `emit_runtime` (everything else). The distinction exists because `remit` is called in the hot stream loop and avoids redundant serde, but the inconsistency is error-prone. Either document the rule clearly or fold `remit` into `emit_runtime` with a flag.

  3. **before_turn hook machinery** (lines ~110-200) — `spawn_blocking` with clone of `ExtensionManager`, then iterating actions for conversation_replace, system_prompt_append, tool_filter. This is heavy. The `spawn_blocking` justification (Lua `ctx.agent.run` blocks) is valid, but the post-processing loop iterates actions three times (checking for replace, appends, filter). Could be a single pass.

  4. **`execute_tool_calls` function** (bottom of file) — marked `#[allow(clippy::too_many_arguments)]` with 8 parameters. Several of these (agent_depth, runtime_events, key_reply_registry) are pass-throughs from `Driver`. Consider bundling into a `ToolExecConfig` struct.

  5. **`UsageRecord` and `DriverOutcome`** — `UsageRecord` duplicates fields that exist in `TokenStats` and the session sink. The `usage: Vec<UsageRecord>` in `DriverOutcome` is only read by the TUI for its own session_seq persistence. Consider whether the TUI could read from session sink instead.

---

## /home/vincent/projects/bone/src/runtime/view.rs
- **Lines:** 377
- **Assessment:** mostly good
- **Notes:** Clean data-oriented design. `ViewModel` as a reducer with `apply()` is well done. `Component` enum with serde tagging is appropriate for the RPC use case. Tests cover upsert-in-place, remove, highlights, round-trip serde, and Lua-style JSON parsing. Minor over-engineering notes:

  1. **`view_diff_from_pane_content`** — a standalone function that converts `PaneContent` → `ViewDiff`. It's only used in one place (the channel transport path). Could be a method on `PaneContent` or inlined at the call site.

  2. **`float_from_pane_content` / `as_pane_content`** — round-trip conversion between `Component::Float` and `PaneContent`. The fact that these two types coexist with overlapping fields (both have `source`/`id`, `title`, `lines`, `scroll`) suggests they could be unified. `PaneContent` is the older type; `Component::Float` is the newer one. Consider deprecating `PaneContent` and using `Component::Float` everywhere.

  3. **`FloatRect` defaults** — `anchor` defaults to `Center`, `col`/`row` default to 0. These serde defaults are fine but the struct currently derives nothing that uses them except deserialization. Minor.

---

## /home/vincent/projects/bone/src/rpc/mod.rs
- **Lines:** 264
- **Assessment:** can be simplified
- **Notes:** The `Hub` is clean — broadcast for events, mpsc for merged commands, no nonsense. `serve_connection` is well-structured (split read/write tasks). However:

  1. **`run_daemon`** is over-engineered for what it does. It only handles `SubmitPrompt` (by spawning `agent::run_agent`), `Cancel` (status event), and a catch-all status. The `pump` task that forwards events from `run_agent`'s channel to the hub is an extra hop — the agent's `event_sender` could be wired directly into the hub's publish mechanism if `AgentRunEvent` is truly a type alias for `RuntimeEvent` (as the comment claims). Remove the intermediate channel and spawn.

  2. **Comments reference "Phase 5" and "Phase 6"** — speculative future planning embedded in production code. The `run_daemon` doc says "intentionally minimal" and "proves the end-to-end RPC path" — if this is a proof-of-concept, tag it as experimental or gate it behind a feature flag rather than shipping it as stable.

  3. **`initial: Vec<RuntimeEvent>` in `serve_connection`** — late-joiner state sync. Currently only used in tests (passing `vec![RuntimeEvent::Status { message: "welcome".into() }]`). The actual daemon doesn't use it. Worth keeping the plumbing but the test usage is trivial.

  4. **`Hub::client_count()`** delegates to `receiver_count()` which counts lagged receivers too. Not a bug but could be misleading under backpressure.

---

## /home/vincent/projects/bone/src/rpc/codec.rs
- **Lines:** 114
- **Assessment:** mostly good
- **Notes:** Simple, clean, minimal — exactly what a codec should be. JSONL framing with `MessageReader` and `write_message`. Blank-line skipping and decode-error recovery are good defensive choices. `ReadError` distinguishes IO (fatal) from decode (skip). Tests cover round-trip and decode recovery. No over-engineering. Only minor observation: `write_message` converts serde error to `io::Error` via `InvalidData` — this loses the original error message detail. Consider using `io::Error::new(ErrorKind::InvalidData, e)` which preserves the inner error via `source()`.
