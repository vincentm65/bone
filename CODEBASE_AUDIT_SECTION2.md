# Section 2 — Runtime, session orchestration, and RPC

**Review goal:** debug, simplify, clean up, reduce LOC, and improve architectural documentation.  
**Scope:** `core/src/runtime/**`, `core/src/rpc/**`, `core/src/session_sink.rs`, and the named section-2 integration tests.  
**Mode:** investigation only; no production code was intentionally changed by this review.

> Working-tree note: the RPC codec files changed while this review was running. The first pass found an unhandled `ReadError::TooLong` compile error in `rpc/mod.rs`; the current tree contains the missing arms and builds. Findings below describe the final tree that was validated.

---

## Architecture and turn lifecycle

```text
RuntimeCommand
├── in-process: Hub → run_daemon → DaemonCtx
└── socket: MessageReader → serve_* → Hub/SessionManager → DaemonCtx

DaemonCtx::run_turn
├── snapshots RuntimeSession into Driver
├── LocalConn pumps Driver events while routing replies/cancel/steer
├── Driver owns one turn's transcript/tools/token state
├── DriverOutcome returns explicit messages + usage + replacement state
└── RuntimeSession::apply_outcome adopts state and commits the turn

Interactive replies
├── ApprovalRequest → ApprovalReplyRegistry → ChannelApprovalGate
└── KeyRequest → KeyReplyRegistry → live tool callback

Managed serve
└── SessionManager owns one !Send daemon future per conversation
```

### State ownership

- `RuntimeSession` is the cross-turn authority for transcript, token totals, tools, conversation id, and DB sequence.
- `Driver` owns a cloned per-turn working copy and returns it in `DriverOutcome`.
- `DaemonCtx` owns session-wide reply registries and serializes turns for one conversation.
- `run_session_manager` multiplexes independent conversation actors on one local task.
- The daemon path uses `NullSessionSink` during the turn and persists `DriverOutcome` atomically afterward. Headless/sub-agent paths persist through `SessionSink` during the turn.

### Required invariants

1. Cancel must unblock every phase of a turn, including approval and interactive-key waits.
2. Reply registry entries must be removed when resolved, cancelled, disconnected, or abandoned.
3. A panic in one managed conversation must not terminate unrelated conversations.
4. A transport error must either consume input or terminate the connection; retry loops must make progress.
5. Durable-write failures must be visible; in-memory success must not silently imply persisted success.
6. Terminal RPC states must emit at most one terminal notice and then close/return.

---

## Findings

### 1. [High] Cancel does not unblock a pending approval and leaves an orphaned waiter

**Confidence:** verified from control flow  
**Evidence:** `core/src/runtime/driver.rs:871-881`, `core/src/runtime/driver.rs:1084`, `core/src/runtime/event.rs:127-147`, `core/src/runtime/event.rs:201-238`, `core/src/runtime/conn.rs:130-136`, `core/src/rpc/mod.rs:1315-1320`

**Scenario:**

1. The model requests a tool that needs frontend approval.
2. `ChannelApprovalGate::decide` inserts a sender into `ApprovalReplyRegistry` and waits on `reply_rx.await`.
3. The user sends `Cancel` without also sending an `ApprovalReply`.
4. `LocalConn` only sets the atomic cancel flag.
5. The driver is awaiting `execute_tool_calls(...).await` directly; this await is not raced against `await_cancel()`.

The turn remains blocked until some client resolves the approval. The registry entry remains pending. The same registry is session-lived, so abandoned senders accumulate.

There is a second leak in the detached-frontend path: `ChannelApprovalGate` registers the sender before publishing the request, but if `events.send(event)` fails it returns the fallback decision without removing the registered sender (`event.rs:201-227`).

**Root cause:** registry ownership is one-way: `register` and `resolve` exist, but there is no cancellation/unregister guard. Cancellation is threaded into streams, hooks, retries, and tools, but not the approval wait.

**Impact:** Cancel can appear frozen at the approval prompt; stale pending entries remain for the session; diagnostics and timer state can drift.

**Smallest fix:**

- Add `remove(id)` or an RAII registration guard to `ApprovalReplyRegistry`.
- Make `ChannelApprovalGate` cancellation-aware and `select!` between reply and cancel.
- Ensure every exit path removes the id and balances `WorkTimer::pause/resume`.
- Alternatively, add a turn-scoped `cancel_all` that resolves all outstanding approvals as denied, but scope it to the active turn rather than the entire process.

**Regression tests:**

- Start a tool approval, send `Cancel`, assert the driver completes promptly and `pending_count() == 0`.
- Drop the event receiver before `decide`, assert fallback is returned and `pending_count() == 0`.

---

### 2. [High] One managed conversation panic terminates the whole session manager

**Confidence:** verified from control flow  
**Evidence:** `core/src/rpc/mod.rs:119-130`, `core/src/rpc/mod.rs:180-192`, `core/src/rpc/mod.rs:228`, `core/src/rpc/mod.rs:245-249`

`ManagedRuntime::task` is a plain `LocalBoxFuture`. `run_session_manager` pushes `async move { (id, runtime.task.await) }` into `FuturesUnordered` without `catch_unwind`. A panic therefore unwinds `run_session_manager`; it does not produce the `(id, ())` item needed by the eviction branch.

The comment at `rpc/mod.rs:190-191` says the entry is evicted when an actor exits by “panic, command channel closed, etc.” The panic claim is false in the current implementation.

**Impact:** one conversation's unexpected panic drops the manager and all other active conversation actors/connections. This defeats session isolation.

**Smallest fix:** wrap each actor future in `AssertUnwindSafe(...).catch_unwind()`, log/publish the panic, and always return its conversation id so the map entry is evicted. Keep the manager alive.

**Regression test:** run two fake managed actors, panic one, then submit to or reattach the other and assert it still responds.

---

### 3. [Medium] Managed RPC loops forever after its event channel closes

**Confidence:** verified  
**Evidence:** `core/src/rpc/mod.rs:330-339`; contrast `core/src/rpc/mod.rs:471-480`

When `attachment.events.recv()` returns `Closed`, `serve_managed_connection` writes one `"conversation runtime stopped"` status and then continues the outer loop. A closed broadcast receiver returns `Closed` immediately forever, so the server repeatedly writes the same status until socket backpressure or disconnect stops it.

The single-hub `serve_connection` path correctly returns when its broadcast closes.

**Impact:** repeated terminal events, avoidable CPU/network use, and a connection that never reaches EOF cleanly.

**Smallest fix:** `return Ok(())` immediately after the one terminal status write. If the desired behavior is transparent actor recreation, implement that explicitly in `SessionManager`; do not busy-loop a dead attachment.

**Regression test:** close the managed actor's event channel, assert exactly one terminal status followed by EOF.

---

### 4. [Medium] `SocketConn` can spin forever on an oversized frame or I/O error

**Confidence:** verified  
**Evidence:** `core/src/runtime/conn.rs:250-256`, `core/src/rpc/codec.rs:88-125`

`SocketConn::next_event` retries every `ReadError` with `Some(Err(_)) => continue`. This is valid only for `Decode`.

For `TooLong` detected before a newline (`codec.rs:103-104`), the oversized buffer is retained. The next call sees the same buffer and returns `TooLong` immediately, creating a CPU loop. Retrying `Io` is also wrong: transport errors should terminate the connection and may repeat indefinitely.

**Impact:** a malformed/buggy remote can wedge the client bridge and consume a core; genuine socket failures are hidden as a hung connection.

**Smallest fix:** skip only `ReadError::Decode`; return `None` for `Io` and `TooLong`. Clearing the codec buffer is optional if `TooLong` is uniformly documented and treated as fatal.

**Regression tests:**

- Feed `SocketConn` more than `MAX_LINE_BYTES` without a newline and assert `next_event` terminates.
- Use a reader that returns an I/O error and assert no retry loop.

---

### 5. [Medium] Daemon persistence failures are silently discarded

**Confidence:** verified  
**Evidence:** `core/src/runtime/session.rs:316-351`, especially `core/src/runtime/session.rs:339-350`

`RuntimeSession::apply_outcome` adopts the new transcript/token/tool state first. It then calls `append_turn_with_checkpoint` inside `if let Ok(next) = ...`; an error is ignored. The frontend receives the turn result and updated in-memory snapshot with no warning that history was not saved.

This conflicts with the documented `SessionSink::persist_failures` goal (`core/src/session_sink.rs:60-65`), but the interactive daemon uses `NullSessionSink`, so that counter cannot report this transaction failure.

**Impact:** users can continue chatting while one or more turns are permanently absent from durable history. Debugging a flaky/full/read-only DB is difficult because success is reported normally.

**Smallest fix:** make `apply_outcome` return both the model result and an optional persistence error, or store a session persistence warning/failure count. Publish one actionable `RuntimeEvent::Status`/warning while retaining the in-memory turn.

**Regression test:** force `append_turn_with_checkpoint` to fail after session initialization; assert state remains usable and a persistence failure is surfaced.

---

### 6. [Low] Key/approval registries have no abandoned-request cleanup contract

**Confidence:** verified design gap; key cancellation behavior depends on the tool  
**Evidence:** `core/src/runtime/event.rs:24-100`, `core/src/runtime/event.rs:103-158`, `core/src/runtime/driver.rs:1118-1141`

Both registries expose only `register`, `resolve`, and `pending_count`. They cannot unregister a dropped receiver or drain a completed/cancelled turn. The key forwarder also ignores failure from `events_out.send(RuntimeEvent::KeyRequest { id })` after registration.

A cancelled tool may drop its key receiver, but the sender remains stored in the session registry until a late reply happens. For key requests this can also leave `WorkTimer` paused because resume occurs only when `resolve` removes the last entry.

**Smallest fix:** one generic turn-scoped reply registry with registration guards would remove duplicated lifecycle logic and guarantee cleanup. If keeping two types, add `remove`/`drain` and invoke them on event-send failure and turn completion.

**Regression tests:** dropped key receiver, failed event publish, and cancelled turn all restore pending count and timer state.

---

## Simplification and LOC reduction opportunities

### A. Make reply waiter lifecycle one reusable primitive

`KeyReplyRegistry` and `ApprovalReplyRegistry` duplicate id allocation, poisoned-lock recovery, hash-map insertion/removal, and pending diagnostics. Their payloads differ, and key replies add timer accounting, but the lifecycle invariant is the same.

A small generic `ReplyRegistry<T>` plus an RAII `PendingReply<T>` would:

- remove duplicate map/id/resolve code;
- make unregister-on-drop automatic;
- provide one tested cancellation contract;
- fix findings 1 and 6 rather than adding ad hoc cleanup at every caller.

### B. Centralize fatal codec-error conversion

`serve_connection`, `serve_managed_connection`, and `SocketConn` each decide independently which `ReadError` values are recoverable. This already drifted when `TooLong` was introduced.

Give `ReadError` a clear API such as `is_recoverable()` / `into_io_error()`, or add one reader helper that skips only decode failures. This is a small LOC reduction and prevents another non-exhaustive or wildcard retry bug.

### C. Do not maintain two RPC terminal-state implementations

`serve_connection` and `serve_managed_connection` duplicate framing and terminal handling but disagree on `Closed`. If both remain product paths, factor the common read/error/close behavior. If section 1's local-first direction wins, mark the single-hub helper test-only or remove the unused product path instead of abstracting it.

### D. Make persistence ownership explicit before deleting sink code

The current split is intentional but expensive:

- daemon: `DriverOutcome.persist_messages` → one atomic `RuntimeSession` transaction;
- headless/sub-agent: seven direct `SessionSink` call sites inside `Driver`.

Do not add a third path. The cleanest long-term shape is for `Driver` to produce effects and for a caller-owned adapter to persist them. That could remove persistence side effects from the model loop and reduce panic/partial-write ambiguity. Before changing it, decide whether headless runs require per-message crash durability; otherwise a blind merge could be a reliability regression.

### E. Reduce transcript cloning only with a clear panic contract

The daemon clones the transcript into `Driver` (`runtime/session.rs:301-302`), then `Driver::run_to_outcome` clones it again for panic recovery (`runtime/driver.rs:191-197`). This is O(history) twice per turn.

The second clone protects conversation recovery after a panic, so deleting it is not a free cleanup. A lower-copy design requires shared immutable pre-turn state or restructuring the driver so unwind recovery retains ownership. Treat this as a measured performance item, not a style cleanup.

---

## Coverage gaps

1. Cancel while waiting for `ApprovalReply`.
2. Registry cleanup after event-send failure, receiver drop, and turn cancellation.
3. Panic isolation between managed conversation actors.
4. Managed event-channel closure emits one terminal event and returns.
5. Oversized and I/O-error behavior through `SocketConn`, not only `MessageReader`.
6. Interactive DB transaction failure visibility.
7. Session-manager reattach racing actor completion; current map eviction is eventually correct, but simultaneous attach/completion is not tested.
8. Backpressure policy for unbounded command/event channels is undocumented and untested under sustained producers.

---

## Clean areas verified

- `Driver::run_to_outcome` catches turn panics and preserves a pre-turn state snapshot rather than wiping the conversation (`runtime/driver.rs:178-217`).
- Provider streaming, hook waits, retry sleeps, and repeated-tool-error backoff explicitly race the cancel flag.
- `RuntimeSession::apply_outcome` uses one transactional append for interactive turn messages and usage when the DB succeeds.
- `LocalConn::next_event` drains events emitted before a completed `DriverOutcome`, preserving event-before-terminal ordering.
- Different managed conversation actors can run concurrently; the existing isolation test covers normal operation.
- RPC JSONL framing now has a 16 MiB cap and the current server reader loops handle `TooLong` as fatal.
- `SocketConn::Drop` aborts its writer task, and `RemoteClient::Drop` aborts its forwarder.
- View diffs have focused unit coverage and preserve replacement order/removal behavior.
- `NullSessionSink` and `UsageOnlySessionSink` have clear, current documentation; usage-only sinks correctly avoid ending the parent conversation.

---

## Validation

Commands run against the final observed working tree:

```text
cargo check -p bone-core --lib
  PASS

cargo test -p bone-core --lib runtime:: -- --test-threads=1
  PASS — 12 passed

cargo test -p bone-core \
  --test driver_turn_test \
  --test rpc_daemon_test \
  --test interactive_esc_test \
  --test stream_tools_test
  PASS — 48 passed total
```

No test failures were observed after the in-progress RPC codec changes reached their final state.

---

## Recommended remediation order

1. **Fix approval cancellation and registry cleanup together** — user-visible freeze plus leak; use one lifecycle design.
2. **Catch managed actor panics** — restore actual conversation isolation and correct the misleading comment.
3. **Terminate managed connections on `Closed`** — one-line behavior fix plus regression test.
4. **Make `SocketConn` retry only decode errors** — one-line core fix plus oversized/I/O tests.
5. **Surface daemon persistence errors** — preserve in-memory success but stop silent history loss.
6. **Then simplify:** generic reply registry, centralized codec error handling, and an explicit decision on dual RPC/persistence paths.
