# Bone core state management review

**Date**: 2026-07-15  
**Mode**: Full architecture review of `core/` (working tree clean — not a diff review)  
**Focus**: State ownership, concurrency, persistence, conversation lifecycle  
**Issue counts**: 6 bugs, 4 suggestions, 2 nits

---

## Summary

Bone core’s turn-truth model is largely intentional and well-shaped: `RuntimeSession` owns the long-lived conversation, `Driver` is the single agent loop, daemon turns use `NullSessionSink` + atomic `apply_outcome`/`append_turn_with_checkpoint`, panic recovery snapshots pre-turn reclaimable state, and extension reload carefully preserves session-scoped fields via `adopt_session_state_from`. The dominant risk is **split state authority** outside the transcript/DB path: tool host state lives both in `ToolHandler.state_map` and a process-wide `ctx.state` map, conversation lifecycle commands do not reset tool/snapshot state, and interactive cancel cannot interrupt approval/key waits. Those gaps matter more than style — they cause cross-conversation leakage, multi-session interference under `bone serve`, and wedged turns.

---

## Architecture (what works)

| Layer | Role |
|-------|------|
| `RuntimeSession` | Authoritative transcript, token stats, tools, DB, turn_nudge |
| `Driver` | Single turn loop; returns reclaimable `DriverOutcome` |
| `LocalConn` / daemon | Pumps turn; routes approval/key/cancel; folds outcome back |
| `SessionDb` | Atomic turn append + optional compaction checkpoint |
| `Hub` | Multi-client fan-out of `RuntimeEvent` / merge of commands |

Frontend-as-client (Neovim model) is clear: the TUI never holds turn-truth.

---

## Issues

### Issue 1 -- Severity: bug

- **File**: `core/src/ext/ctx.rs:72-89`
- **Description**: Host tool state is split across two stores. `ToolHandler.state_map` is session-scoped, adopted after turns (`RuntimeSession::apply_outcome`), and preserved across `ReloadExtensions`, but the live path for tools like `task_list` reads/writes `process_shared_state()` via `ctx.state` — a process-global `OnceLock<Arc<Mutex<HashMap>>>`. `ToolExecutionContext.session_state` is populated in `ToolHandler::execute_*` (`registry.rs:373-380`, `405-415`, `472-475`) yet never read by any tool implementation. Result: the carefully maintained `state_map` is a shadow copy; real checklist (and any other `ctx.state` consumer) state is process-wide, not conversation-scoped, and not part of panic/`apply_outcome` adoption.
- **Suggestion**: Make one store authoritative. Prefer session-scoped state on `RuntimeSession`/`ToolHandler` (or a conversation-keyed map), inject prior state into Lua as `ctx.state` (or seed from `session_state`), and stop using a process-global singleton. Either delete the unused `session_state` plumbing or make it the sole feed for host-stateful tools.
- **Status**: open

### Issue 2 -- Severity: bug

- **File**: `core/src/rpc/mod.rs:799-828`
- **Description**: Conversation lifecycle does not reset tool/host/snapshot state. `NewConversation` clears `transcript`/`token_stats` and mints a DB row, but leaves `tools.state_map`, `tools.snapshots`, and `process_shared_state` untouched. `LoadConversation` (`832-881`) swaps transcript/usage/provider but keeps the previous conversation’s in-memory tool state and file snapshot store. `ClearConversation` (`904-912`) only clears transcript + token stats. Combined with Issue 1, `/new`, `/clear`, and history load leak `task_list` panes/state and `read_file`/`edit_file` snapshot identity across conversations — including concurrent actors under `SessionManager` that all share one process `ctx.state`.
- **Suggestion**: On new/load/clear, explicitly reset conversation-scoped tool state: `state_map.clear()`, `snapshots.write().clear()`, and clear or re-key `process_shared_state` (ideally replace it with session-owned state). For load, optionally restore persisted host state if/when it is durable.
- **Status**: open

### Issue 3 -- Severity: bug

- **File**: `core/src/runtime/event.rs:193-241`
- **Description**: Cancel cannot interrupt a blocked approval decision. `ChannelApprovalGate::decide` registers a oneshot and `await`s `reply_rx` with no select on the turn cancel flag. `LocalConn::send(Cancel)` only sets `AtomicBool` (`conn.rs:136`); the driver observes cancel between stream chunks and tool batches, not while parked in the gate. Same pattern for `ctx.ui.key` (`ext/ctx.rs:734-746`), which blocks on a oneshot with no cancel race. User Esc/`Cancel` during an approval or key prompt leaves the turn wedged until a reply arrives or the frontend detaches (and even detach only helps if the reply sender is dropped — registry entries keep senders alive).
- **Suggestion**: Thread `cancel` into the gate (and key wait): `select!` on cancel vs reply; on cancel resolve/deny, remove registry entry, and return a denied/cancelled outcome so the driver loop can exit. On turn end, drain unresolved registry entries.
- **Status**: open

### Issue 4 -- Severity: bug

- **File**: `core/src/runtime/event.rs:201-228`
- **Description**: Approval registry leak when the event stream is already closed. `decide` always `register`s the oneshot (`202`), then if `events.send` fails (`225-228`) returns the non-interactive fallback **without removing** the pending entry. The map retains a oneshot sender whose receiver was dropped on return, so `pending_count` grows and ids never resolve. There is no turn-end cleanup of `ApprovalReplyRegistry` / `KeyReplyRegistry` in `run_turn` either.
- **Suggestion**: On send failure, remove the id from the registry (or register only after a successful send). Add `clear()` / `abort_all()` called from `run_turn` after the connection drains (and on cancel) so cancelled mid-wait tools cannot leak entries across turns.
- **Status**: open

### Issue 5 -- Severity: bug

- **File**: `core/src/runtime/session.rs:317-347`
- **Description**: `apply_outcome` always adopts in-memory transcript/token_stats/`state_map`, then persists with a silent `if let Ok(next) = db.append_turn_with_checkpoint(...)`. On DB failure, memory advances while durable history does not; `session_seq` is left stale relative to the adopted transcript. User messages are already written pre-turn via `append_user_to_db` (`rpc/mod.rs:790`), so a failed turn commit yields a DB that has the user line but missing assistant/tool rows — resume via `load_effective_transcript` then diverges from what the live session just showed. No warning is surfaced (unlike headless `SessionWriter::persist_failures`).
- **Suggestion**: Propagate or log persistence errors from `apply_outcome` (status event + counter). Consider treating failed commit as a hard turn-persistence error while still keeping in-memory continuity, or retry once. Align user-message durability with the same atomic turn transaction where possible so partial user-only rows are intentional and documented.
- **Status**: open

### Issue 6 -- Severity: bug

- **File**: `core/src/runtime/driver.rs:192-220`
- **Description**: Panic recovery is only partially consistent. `run_to_outcome` clones `transcript`/`token_stats`/`tools` before the turn; on panic those pre-turn values are returned and `apply_outcome` reverts `state_map`. But (a) `tools.snapshots` is an `Arc` shared with the live turn — file snapshot mutations survive the “rollback”; (b) `process_shared_state` is process-global and is never rolled back; (c) pre-turn user rows already committed via `append_user_to_db` remain in SQLite while assistant progress is dropped. After a panicking turn the session can show the old transcript while task list / file-edit guards still reflect partial work.
- **Suggestion**: Document the intentional “DB may be ahead” note more broadly for shared Arcs and `ctx.state`, or snapshot/restore those stores too. Prefer conversation-owned state that can be rolled back with the panic outcome. Avoid relying on deep-clone of `ToolHandler` as the sole recovery mechanism when several fields are shared by Arc or global singleton.
- **Status**: open

### Issue 7 -- Severity: suggestion

- **File**: `core/src/rpc/mod.rs:323-324`
- **Description**: Hub and managed-connection event pumps treat `broadcast::RecvError::Lagged` as `continue` (also `474`), silently dropping events. Capacity is 1024 (`Hub::new` at line 67). Under multi-client load or slow consumers, a client can miss `StateSnapshot`, `ToolResult`, `Finished`, or `ConversationLoaded` and desync its view-model from `RuntimeSession` turn-truth with no recovery signal.
- **Suggestion**: On lag, publish a resync signal (e.g. force `StateSnapshot` + optional full transcript reload) or surface a `Status`/protocol event so the client knows to resubscribe/resync rather than rendering a silently incomplete turn.
- **Status**: open

### Issue 8 -- Severity: suggestion

- **File**: `core/src/rpc/mod.rs:562`
- **Description**: `DaemonCtx` / `run_turn` use `self.session.lock().unwrap()` throughout (dozens of sites; e.g. 782, 1148, 1236). Registries and `SessionWriter` deliberately recover poison with `unwrap_or_else(|e| e.into_inner())` (`agent.rs:24-26`, `event.rs:48`). A panic while holding the session mutex (or nested `turn_nudge` lock at `driver.rs:395` / `conn.rs:143`) permanently wedges the daemon’s ability to handle further commands even if panics are otherwise caught at the turn boundary.
- **Suggestion**: Use poison recovery for `RuntimeSession` and `turn_nudge` locks consistently with other core locks, or abort the process on session-mutex poison if partial recovery is unsafe — but do not leave a half-dead daemon on `unwrap`.
- **Status**: open

### Issue 9 -- Severity: suggestion

- **File**: `core/src/tools/registry.rs:360-369`
- **Description**: Host-stateful serialization only triggers when **more than one** host-stateful name appears in the batch. With a single host-stateful call, that call runs concurrently with other tools via `join_all`. Because real state is in the process-wide `ctx.state` mutex (Issue 1), concurrent non-stateful tools that also touch `ctx.state` can interleave; `state_map` updates still only land after the whole batch in the driver (`driver.rs:857-872`). The mid-batch `state_overrides` path (`registry.rs:401-428`) is ineffective while tools ignore `session_state`. Architecture intends “stateful tools see prior results”; implementation only half-delivers.
- **Suggestion**: After unifying state (Issue 1), either always serialize host-stateful tools against any writers of the same key, or document that only multi-call same-batch host-stateful tools are ordered. Wire `session_state` into the tool ctx so serial overrides actually change what the next call sees without depending on a global map.
- **Status**: open

### Issue 10 -- Severity: suggestion

- **File**: `core/src/agent.rs:16-134`
- **Description**: Dual persistence models remain: headless `SessionWriter` appends messages and usage **mid-turn** via `SessionSink` (non-atomic, partial turns on crash — noted in `driver.rs:188-189`), while the interactive/daemon path uses `NullSessionSink` and batches in `append_turn_with_checkpoint`. The Driver still calls both `session.append_message`/`record_usage` and accumulates `persist_messages`/`usage` for `apply_outcome`. Correct for the two entry points today, but the dual writer design is easy to re-break if a future path attaches a real sink *and* calls `apply_outcome` (double rows) or neither (silent loss).
- **Suggestion**: Keep the invariant explicit in types (e.g. sink mode enum: `Incremental` vs `Deferred`) or stop calling sink append when the outcome will batch-persist. Add a test that a daemon turn never writes via `SessionSink` and a headless turn never depends on `apply_outcome`.
- **Status**: open

### Issue 11 -- Severity: nit

- **File**: `core/src/runtime/session.rs:331`
- **Description**: `apply_outcome` adopts only `tools.state_map` from the outcome `ToolHandler`, not other turn-local fields (`cancel_token`, `approval_gate`). That is correct today (turn-local wiring must not stick), but combined with `ToolHandler: Clone` it is easy for future code to mutate registry/enabled maps on the clone and expect them to return. Snapshots rely on Arc sharing instead of explicit adopt — asymmetric with `state_map`.
- **Suggestion**: Prefer an explicit `SessionToolState { state_map, snapshots }` return on `DriverOutcome` rather than a full `ToolHandler`, so adoption boundaries stay obvious.
- **Status**: open

### Issue 12 -- Severity: nit

- **File**: `core/src/ext/ctx.rs:1901-1919`
- **Description**: Subagents built via `build_agent_request` get `session_sink: None` (→ `NullSessionSink` for depth>0), empty `transcript` (except followup), and a freshly booted tool handler — good isolation for SQLite and file snapshots. They still share `process_shared_state` and the global job registry (scoped by conversation_id for cancel/inject). A subagent `task_list` write therefore mutates the parent conversation’s visible checklist.
- **Suggestion**: Once host state is session-scoped (Issue 1), give subagents either an isolated state map or an explicit inherit flag.
- **Status**: open

---

## Priority fix order

1. **Unify host state** (Issues 1, 2, 9) — biggest architectural hole  
2. **Cancel + registry hygiene** (Issues 3, 4) — interactive correctness  
3. **Persistence visibility** (Issue 5) — silent memory/DB drift  
4. **Hub lag resync + lock poison** (Issues 7, 8) — multi-client robustness  

---

## Scope reviewed

Key areas examined:

- `core/src/runtime/{session,driver,conn,event,view}.rs`
- `core/src/rpc/mod.rs` (`run_daemon`, lifecycle commands, outcome folding)
- `core/src/tools/{registry,state_map,types,snapshot}.rs`
- `core/src/agent.rs`, `session_db.rs`, `session_sink.rs`
- `core/src/ext/{ctx,loader}.rs` and `defaults/lua/tools/task_list.lua`
- Related unit/integration tests for session apply and driver panic paths
