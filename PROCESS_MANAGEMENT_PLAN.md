# Bone Core Process-Management Plan

## Goal

Make it clear why a background process stopped. One small change: record the termination reason and timestamps the shell runner already computes, and show them in the processes pane.

No new services, no disk persistence, no event streams, no change to cancellation semantics. The process manager stays generic — no domain-specific behavior.

## Current state

`core/src/processes.rs` + `core/src/tools/shell.rs` already provide:

- process IDs, owner scopes, scoped cancellation;
- live bounded stdout/stderr capture (64 KB, truncated marker);
- process-group cleanup through the shell runner;
- detached lifetime: processes run in their registry task and outlive the spawning tool call.

The gap: `run_script_stream` returns `ProcessOutput { cancelled, timed_out, exit_code, signal, ... }` — it already knows why the process stopped — but the registry discards `cancelled`/`timed_out` and stores only exit code/signal. A cancelled process is stored identically to a normal exit (distinguishable only by "signal 9"). There are no timestamps.

## The change

### 1. Record termination reason and times

Add to `ProcessSnapshot`:

- `state`: `Running | Exited | TimedOut | Cancelled`;
- `started_at`, `finished_at`.

The registry's spawn task already receives `ProcessOutput`; store its flags instead of dropping them.

### 2. Show it in the TUI

`execute_action` list/status output and the processes pane show state, reason, and elapsed time. No new keys, no new panes.

### 3. Tests

- normal exit → `Exited` with code;
- failing exit → `Exited` with non-zero code;
- timeout → `TimedOut`;
- cancellation → `Cancelled` with partial output;
- existing scoped ownership and cancellation behavior unchanged.

## Out of scope (confirmed)

- No disk persistence or run directories — no disk growth.
- No event streams, no new supervisor service, no ownership hierarchy.
- No cancellation semantics change: direct SIGKILL to the process group and a non-blocking grab of buffered output stay as-is — waiting for pipe EOF after a kill would hang on escaped descendants holding the write end.
- No graceful SIGTERM→grace→SIGKILL two-stage kill.

## Invariants

- The existing shell runner is the single owner of process execution.
- Every finished run records why it stopped.
- Frontends render registry state; they never own processes.
- No process state is written to disk.
