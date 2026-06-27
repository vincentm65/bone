# TUI-as-Client Refactor Plan

**Goal:** Make the built-in TUI a true wire client of the headless core
(Neovim model), without a broken window. Evolve the single existing TUI
through the protocol it already speaks.

**Approach:** Incremental. `LocalConn` is the safety net — every phase keeps
the TUI functional. The final `LocalConn` → `SocketConn` flip is small because
both already satisfy `RuntimeConn`.

**Why not one-shot:** The infrastructure (RuntimeConn, LocalConn, SocketConn,
run_daemon, Hub) is ~80% built and was designed to be crossed incrementally.
One-shot redoes finished work and creates a multi-day broken window.

---

## Status (2026-06-26)

- **Phase 0–2: DONE.** Workspace split; in-process daemon owns the session; TUI
  is a pure event consumer.
- **Boot-dedup Step A: DONE** (reload no longer dual-boots). Step B: premature
  (skipped, see below). Step C: blocked (architectural).
- **Phase 3-bridge: DONE**, pending interactive verification — `bone --connect
  <addr>` attaches the TUI to a remote `bone serve`. In-process default
  unchanged.
- **Phase 3-pure: BLOCKED** on unifying `drive_live` interactive slash commands
  with the daemon turn path (the TUI's local Lua VM can't be dropped until
  then). The next real piece of work if the refactor continues.
- **Phase 4 (protocol crate): DONE** (extracted early during Phase 0; `protocol/` member, depended on by both `core` and `tui`).

Everything implementable without a multi-session architectural change (the
`drive_live` unification) and without interactive terminal verification has
landed and is covered by `cargo test --workspace` (38 binaries green) + clippy.

**Phase 2 loose end:** one bootstrap read remains at `mod.rs:253`
(`runtime.lock().unwrap().conversation_id`) to seed the conversation ID before
the daemon task spawns. Not in the turn loop; harmless.

---

## What already works (don't redo this)

- `RuntimeConn` trait: `send(cmd)` + `next_event()` — frontend-neutral.
- `LocalConn` (in-process): owns the Driver future, polls on the TUI's task.
- `SocketConn` (remote): same protocol over JSONL to `bone serve`.
- TUI render loop already pulls events via `LocalConn::next_event()`.
- `run_daemon` owns `RuntimeSession` for remote clients and drives turns.
- `Hub` does multi-client fan-out + late-joiner state sync.
- Headless core compiles: `cargo check --lib --no-default-features`.

## What still needs doing

~~The remaining gaps are **ownership**, not plumbing.~~ **(All done — Phase 1–3 resolved items 1–4.)**

1. ~~App owns `RuntimeSession`, `Arc<dyn LlmProvider>`, `extensions`.~~ → Daemon owns, App uses Hub channels.
2. ~~App calls `build_driver` (1 site), `build_chat_history` (3 sites), `boot_with_tools` (2 sites) directly.~~ → Daemon handles all.
3. ~~App reads `runtime.transcript` directly for stats / rebuild-chat.~~ → Events only (Phase 2); one bootstrap read for conversation_id remains.
4. ~~App uses `LocalConn` — needs the flip to `SocketConn`.~~ → `DaemonSource::Remote` via `RemoteClient` (Phase 3-bridge).

---

## Phases

### Phase 0 — Workspace split (organizational, zero behavior change)

Separate the folder structure into two crates. No rewiring yet.

- Add `[workspace]` to root `Cargo.toml`: `members = ["protocol", "core", "tui"]`. (The `protocol` crate was extracted early — Phase 4.)
- `protocol/` = shared types: `RuntimeEvent`, `RuntimeCommand`, `ChatMessage`, etc. Depended on by both `core` and `tui`.
- `core/` = current `src/` minus `src/ui/`, minus the `ui` feature gate.
  Crate name `bone-core` (`bone_core` lib).
- `tui/` = current `src/ui/` + `main.rs` + a `lib.rs` re-export. Depends on `bone-core = { path = "../core", features = ["tui"] }` and `bone-protocol = { path = "../protocol" }`.
- Verify: `cargo build` (both), `cargo check --lib --no-default-features`
  (core alone), `cargo test`.
- **Risk:** low. Pure move + import path rewrite (`crate::` → `bone::`).
- **Shippable:** yes, identical behavior.

**Decision point:** does `tui` depend on the full `bone` lib or only a
`bone-protocol` crate (RuntimeEvent/RuntimeCommand types)? Start with full lib
dependency — pragmatic for a Rust client. Extract a protocol crate later only
if a non-Rust client is wanted.

### Phase 1 — Move session ownership into an in-process daemon

The biggest decoupling. App stops owning `RuntimeSession`/`llm`/`extensions`.

- Reuse `run_daemon` (the same one `bone serve` uses), spawned as a tokio task
  in-process. The `mlua` "send" feature makes this safe.
- App holds a `Hub` client handle: `subscribe()` receiver + `command_sender()`.
- App's turn loop sends `SubmitPrompt` over the command sender, receives events
  via the broadcast receiver, renders. Identical to the socket path.
- Remove `App.runtime`, `App.llm`, `App.extensions`, the `build_driver` call,
  `boot_with_tools`, `build_chat_history` calls from the TUI — the daemon owns
  all of these now.
- **Risk:** medium. This is the real rewiring. Keep `LocalConn` reachable as a
  fallback flag (`--local-conn`) during stabilization so a regression doesn't
  block shipping.
- **Shippable:** yes — behavior identical, just the server moved into a task.

**Substep 1a:** Audit the 3 `build_chat_history` + 2 `boot_with_tools` call
sites. Some may be rebuild-chat (provider switch) or stats — those must move
to events (`TokenUsage`) or daemon commands, not direct transcript reads.

### Phase 2 — Make the TUI a pure event consumer

Eliminate every direct `runtime.transcript` / `runtime.token_stats` read.
The App holds only a view-model built from accumulated `RuntimeEvent`s.

- Token totals: accumulate `TokenUsage` events instead of reading
  `runtime.token_stats`.
- Message list: built from `TextDelta` / `ToolCall` / `ToolResult` events,
  not `runtime.transcript`.
- Stats pane: sourced from accumulated events, not session reads.
- **Risk:** low-medium. Mechanical, but touches the stats/history code paths.
- **Shippable:** yes.
- **Exit criterion:** `tui/` imports nothing from `bone_core::runtime` except
  protocol types (`RuntimeEvent`, `RuntimeCommand`) and pure helpers.
  (Met; one bootstrap `conversation_id` read remains in the constructor,
  before the daemon task spawns. Not in the turn loop.)

### Phase 3 — Flip `LocalConn` → `SocketConn`

The switch that makes it a true separate client.

- App opens a socket to `bone serve` (or spawns `bone serve` as a subprocess
  and connects to it), uses `SocketConn` (already implemented).
- Remove the in-process daemon spawn from Phase 1.
- Add a startup mode: `bone` runs the TUI + auto-spawns `bone serve`;
  `bone --connect <addr>` attaches to an existing daemon.
- **Risk:** low. `SocketConn` satisfies the same `RuntimeConn` trait the TUI
  already uses. The render loop is unchanged.
- **Shippable:** yes — the TUI is now a real client.

> **Update (investigation):** the App does *not* currently use a `RuntimeConn`;
> it talks to the in-process daemon over `Hub` channels (`command_tx`
> `UnboundedSender<RuntimeCommand>` + `events_rx` `broadcast::Receiver`). A
> remote attach can reuse that exact interface via a small bridge (socket→
> broadcast, mpsc→socket) — no render-loop change. The remaining blocker is
> **interactive slash commands** running against the TUI's local Lua VM via
> `drive_live`; see Boot-dedup Step C. Until those run in the daemon, the TUI
> cannot drop its in-process VM. So Phase 3 splits into: **3-bridge** (remote
> attach over a socket, in-process VM retained — safe, shippable) and **3-pure**
> (drop the in-process VM — blocked on Step C).

#### Phase 3-bridge — remote attach (DONE, pending interactive verification)

`bone --connect <addr>` runs the full TUI against a separate `bone serve`
daemon. The local App keeps its own Lua VM (display + interactive slash
commands); turns run on the remote daemon.

What landed:
- `core/src/rpc/mod.rs`: `RemoteClient` — client-side counterpart to `Hub`.
  Wraps a `SocketConn`, exposes the same `command_sender()` / `subscribe()`
  interface; a forwarder task relays `next_event()` into a broadcast channel.
  The App's event loop is unchanged.
- `tui/src/ui/app/mod.rs`: `DaemonSource::{InProcess, Remote}`; `App::new`
  delegates to `App::with_daemon`. The daemon block is a match producing the
  same `(command_tx, events_rx, conversation_id, reload_inbox)` tuple either
  way. `reload_inbox` is now `Option` (`None` remote — the remote daemon
  disk-boots on `ReloadExtensions`, no shared VM). `reload_extensions` skips the
  inbox when `None`.
- `tui/src/main.rs`: `--connect <addr>` flag; `bone serve` now sends an
  on-connect `StateSnapshot` (shared the session `Arc` with the accept loop) so
  a fresh client renders conversation id / totals immediately.
- Tests (`core/tests/rpc_daemon_test.rs`):
  `remote_client_bridges_commands_and_events` (prompt round-trips daemon→
  broadcast) and `remote_client_receives_initial_state_replay` (on-connect
  full-state sync). Smoke-tested `bone serve` + `bone connect` over a socket.
- **Not yet done:** driving the interactive `bone --connect` TUI in a real
  terminal (can't be done headlessly). Default `bone` (in-process) is untouched.
  Known limitation: stats reads use the *local* session DB, so attaching to a
  daemon on another host shows local stats (fine for the localhost daemon case).

### Phase 4 (optional) — Separate binary / protocol crate **→ DONE**

- Extract `RuntimeEvent`/`RuntimeCommand` into a tiny `bone-protocol` crate. ✅
- `tui/` depends only on `bone-protocol` — no longer links the full core. ✅
- Enables non-Rust clients (the npm/web path).
- **Risk:** low, optional, no deadline.

Landed:
- `protocol/` workspace member with `bone-protocol` crate (`bone_protocol` lib).
- 7 modules: `event.rs`, `input.rs`, `message.rs`, `session.rs`, `tokens.rs`, `tools.rs`, `view.rs`.
- Both `core` and `tui` depend on `bone-protocol = { path = "../protocol" }`.
- Extracted early during Phase 0 to share types cleanly between crates.

---

## Risk register

| Risk | Mitigation |
|---|---|
| TUI broken mid-refactor | `LocalConn` fallback flag during Phase 1 |
| `!Send` Lua VM across tasks | `mlua` "send" feature already enabled; `run_daemon` already proves spawn works |
| Lost stats/history fidelity | Phase 2 is explicitly about verifying event-derived state matches |
| Rendering helpers (`format_tokens`, `shell_split`, `classify_command`) pull core back in | Acceptable as core dep for Rust TUI; extract only if needed |

## Ordering rationale

Phase 0 before 1: draw the boundary before rewiring across it.
Phase 1 before 2: ownership must move before reads can be eliminated.
Phase 2 before 3: can't flip to socket while direct reads exist (they'd have no
source).
Phase 3 is the payoff — small because 1 and 2 did the work.

Each phase is independently shippable. The TUI never stops working.

---

## Boot-dedup track (between Phase 2 and Phase 3)

A self-contained track that removes extension/tool **boot duplication** between
the TUI and the in-process daemon. Independent of the Phase-3 socket flip but a
useful precursor: it forces the question "who owns the Lua VM?" to be answered
in code before the process boundary is drawn. Numbered A/B/C to avoid colliding
with the plan's Phase numbers.

While the TUI runs the daemon in-process (pre socket-flip), the two already
share one Lua VM (`extensions.clone()` at startup). The remaining duplication
was on **reload**, plus the open question of whether the TUI needs its own VM at
all once the daemon owns turns.

### Step A — Eliminate dual-boot on reload (DONE)

`/tools reload` + post-`/catalog` hot-reload. Before: the TUI re-booted
extensions from disk, then `ReloadExtensions` made the daemon independently
re-boot a *second* VM from disk (twice the I/O, drift risk).

Now: a shared `Arc<Mutex<Option<BootedTools>>>` "reload inbox". The TUI boots
once, drops a clone (`manager.clone()` shares the `Arc<Mutex<Lua>>`) into the
inbox, then sends `ReloadExtensions`. `run_daemon` adopts the inbox payload if
present, else boots from disk (`bone serve` passes `None`, unchanged). This also
fixes a latent inconsistency: the daemon's reload used to re-boot with
`headless: true` while startup shared the TUI's `headless: false` VM — now reload
matches startup.

Touched: `core/src/rpc/mod.rs` (signature + `ReloadExtensions` handler),
`tui/src/ui/app/mod.rs` (inbox field + `reload_extensions`), call sites in
`tui/src/main.rs` and `core/tests/rpc_daemon_test.rs` (pass `None`).

Covered by `reload_extensions_adopts_inbox_without_disk_boot`
(`core/tests/rpc_daemon_test.rs`): proves the daemon adopts the inbox's tool set
(not a disk-boot count), drains the inbox, and swaps `session.tools`.

### Step B — Tool definitions sourced from events (DEFERRED)

Add `tool_definitions` to `StateSnapshot` so the TUI reads enabled tool defs
from events instead of its own `ToolHandler` copy. Shrinks the TUI's reasons to
hold tool state. Lower priority than the socket flip; revisit if the TUI's
`self.tools` proves to be the last direct core dependency.

### Step C — Full move: daemon owns everything (DEFERRED, blocker identified)

Daemon owns the only Lua VM; the TUI sends command invocations and renders
results.

What's already solved: `ctx.ui.key()` **during a model turn** round-trips over
the protocol today — a blocked tool emits `RuntimeEvent::KeyRequest{id}`, the
frontend replies `RuntimeCommand::KeyReply{id,key}` (`KeyReplyRegistry` splits
the live `oneshot` from the serializable command). This is the `bone connect`
path; see `core/src/rpc/mod.rs:137` and `driver.rs:828`.

The actual blocker: **interactive slash commands** (`/config`, `/usage`, …) run
through the TUI's `drive_live` against its *local* Lua VM, not through the
daemon's turn loop. They render panes/menus and block on keys synchronously in
the render thread. To make the TUI a pure remote client these must instead run
*in the daemon* and drive the UI over the same event/command protocol (pane
diffs as events, keys as `KeyRequest`/`KeyReply`) — i.e. unify the `drive_live`
path with the daemon turn path. That unification is the real Phase 3 prerequisite.
