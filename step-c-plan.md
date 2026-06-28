# Step C — Daemon owns the only Lua VM

**Goal:** Delete the TUI's local Lua VM (`App.extensions`) so the daemon is the
sole owner. Every slash command, hook, and pane runs in the daemon and reaches
the TUI purely over `RuntimeEvent`/`RuntimeCommand`. The TUI becomes a protocol
client; Phase 3-pure (default `bone` = TUI + auto-spawned local `bone serve`)
becomes a small flip.

**Why now:** it is the last seam between TUI and core, and the prerequisite for
a web client. Tools are already 100% reusable; commands and hooks are not,
because they execute against a VM that can't cross a process boundary (`mlua::Lua`
is `!Send`). Once only the daemon has a VM, *any* transport client (socket TUI,
web client) gets every command and hook for free.

---

## Why we are doing this

### The problem
`mlua::Lua` is `!Send`: it can live on one tokio task but can't move between
tasks. So the TUI and the daemon can't *share* one VM across a socket — each
side needs its own. Today the in-process TUI keeps its own `ExtensionManager`
(`App.extensions`, `tui/src/ui/app/mod.rs:117`) and runs slash commands against
it locally via `drive_live` (`tui/src/ui/app/stream/mod.rs:1092`), which blocks
on crossterm key reads in the render thread.

That local VM is the **last piece of core-owned, mutable, non-serializable
state** living in the TUI. Everything else (`RuntimeSession`, `Arc<dyn
LlmProvider>`, the `Driver`) already moved to the daemon in Phase 1.

### The mechanism already exists
The remote path is built and shipped (`bone --connect <addr>`):

- **Daemon side** — `run_interactive_command` (`core/src/rpc/mod.rs:215`): runs
  the Lua handler in `spawn_blocking`, pumps `KeyRequest`/`ViewDiff`/`Cancel`
  over the protocol exactly like a turn.
- **Client side** — `run_remote_command` (`tui/src/ui/app/stream/mod.rs:457`):
  sends `RunCommand`, renders `ViewDiff` events, answers `KeyRequest` with
  `KeyReply`, handles `ApprovalRequest` mid-command.

These two are a structural twin of the local path. Step C is **not "build a new
mechanism"** — it is *delete the local path, make the remote path the only path.*

### The payoff
1. TUI boots no VM; imports only protocol types + pure helpers.
2. Phase 3-pure becomes a flip, not a rewrite.
3. A web client implements `run_remote_command` in JS and gets every slash
   command, hook, and interactive pane for free.

---

## The full local-VM surface (what retires)

`App.extensions` is touched in six ways. Step C must retire all of them, not
just the command path:

| # | Site | What it does | Remote equivalent |
|---|------|--------------|-------------------|
| 1 | `run_lua_command` local branch (`mod.rs:2063-2125`) | Runs slash commands via `drive_live` | `run_remote_command` — **done** |
| 2 | `drive_live` (`stream/mod.rs:1092`) + `drain_keys` (`stream/mod.rs:1174`) | crossterm key loop for commands | `run_remote_command`'s `KeyRequest` arm — **done** |
| 3 | `dispatch_session_end` (`mod.rs:424`) + `mode_change` dispatch (`mod.rs:685`) | Fires Lua hooks | Needs a daemon command (new) |
| 4 | terminal-width sync (`mod.rs:812`): `ui_handle().lock().terminal_width = …` | Lets Lua panes wrap to live width | Needs width in `StateSnapshot` or a `SetTerminalWidth` command (new) |
| 5 | command listing for autocomplete (`mod.rs:1351`, `1952`): `extensions.commands()` | Populates `/help` + autocomplete | Step B: `tool_definitions`/command list in `StateSnapshot` |
| 6 | reload (`mod.rs:2278`, `2291`): `self.extensions = booted.manager` + inbox | Re-boots VM on `/tools reload` | Daemon owns reload; TUI sends `ReloadExtensions` only |

Sites 1–2 are already solved by the remote path. Sites 3–4 are new wiring.
Site 5 is the deferred Step B (optional precursor; can stub with an empty list
and ship). Site 6 simplifies away.

---

## Procedure

Each step is independently shippable. Do not delete the local path until step 4.
`is_remote` stays the gate throughout; step 5 flips the default and deletes the
dead branch.

### Step 1 — Move Lua hook dispatch onto the protocol (sites 3)

**Why first:** hooks fire outside the command loop (`session_end` on quit,
`mode_change` on Shift+Tab), so they need their own command path independent of
`RunCommand`.

- Add `RuntimeCommand::DispatchHook { name: String, payload: serde_json::Value }`.
- Handle it in `run_daemon` (`core/src/rpc/mod.rs`): call
  `extensions.dispatch_simple(&name, payload)`. Non-blocking; no reply needed
  (hooks are fire-and-forget today).
- In the TUI, replace the two `self.extensions.dispatch_simple(...)` calls with
  `self.command_tx.send(RuntimeCommand::DispatchHook { .. })`.
- Gate behind `is_remote` for now (local path keeps dispatching directly until
  step 5 deletes it).

**Verify:** `mode_change` still updates the daemon's `SharedApprovalMode` (it
already does via `SetApprovalMode` at `mod.rs:685` — confirm the hook fires on
the daemon after this change). Add a test that `DispatchHook` reaches the
daemon's `ExtensionManager`.

### Step 2 — Move terminal-width sync onto the protocol (site 4)

**Why:** Lua panes (`ctx.ui.width`) wrap to the live terminal width, which only
the TUI knows. Today it writes directly into the shared `UiState`.

- Add `width: u16` to `SessionSnapshot` (`protocol/src/session.rs`), **or** add a
  lightweight `RuntimeCommand::SetTerminalWidth(u16)`. Prefer the command —
  width changes on resize, not on every turn, and avoids bloating the snapshot.
- Daemon applies it to its `ExtensionManager`'s `UiState` handle.
- TUI sends width on startup and on every resize (the existing render frame at
  `mod.rs:808` already re-reads `terminal.size()` — send there).
- Gate behind `is_remote`.

**Verify:** resize a `/config` picker pane while attached remotely; wrapping
tracks. Can't be automated (needs a real TTY) — manual check, log it.

### Step 3 — Close the local-stats-read gap (prerequisite correctness)

**Why:** once remote is the only path, the TUI can't read the local session DB
for the stats dashboard (`mod.rs:1596`, `2234`) — it would show the wrong host's
stats, the known `--connect` limitation. Either source stats from events or
accept the limitation and document it.

- Option A (full): add a `RuntimeCommand::QueryStats` + `RuntimeEvent::StatsResult`
  round-trip; the dashboard reads from the daemon's DB.
- Option B (ship now): keep the local read but gate the dashboard behind
  `is_remote == false`; remote clients get a "stats unavailable remotely" notice.

Pick B for Step C (it's a display-only pane); do A later if demanded. The
`token_stats` mirror fields (`mod.rs:94`) are **already** event-sourced via
`StateSnapshot` — those are fine.

### Step 4 — Interactive verification of the remote command path (the gate)

**Why:** this is the step the plan flags as un-automatable. Every interactive
command must behave identically over `run_remote_command` as over `drive_live`
before the local path is deleted.

Run `bone serve` + `bone --connect <addr>` in a real terminal and exercise,
comparing against in-process `bone`:

- `/config` — full text entry (api_key field), picker navigation, bracketed paste
- `/usage` / any stats pane
- any command that opens a picker pane and reads `ctx.ui.key()`
- Esc-cancel mid-command — confirm `PaneOwnership::drain_for_cancel` cleans up
  panes identically (`run_remote_command` has its own copy at `stream/mod.rs:498`)
- mid-command tool approval (`/shotgun`-style commands that call tools) — the
  `ApprovalRequest` arm at `stream/mod.rs:534`
- `ViewDiff` ordering/dropping under fast typing

Fix parity gaps against `drive_live` as found. **Do not proceed to step 5 with
open gaps.** Log each fix.

### Step 5 — Flip the default; delete the local VM (sites 1, 2, 6)

**Why:** the payoff step. Now that remote is verified for commands, hooks,
width, and stats, the local VM is dead weight.

- Default `bone` spawns a local `bone serve` and connects to it (the
  `DaemonSource::Remote` path with `127.0.0.1`), instead of `InProcess`.
- Delete `App.extensions` (`mod.rs:117`) and `App::boot_with_tools` call.
- Delete the `is_remote == false` branch of `run_lua_command` (`mod.rs:2063`);
  `run_remote_command` becomes the only path. `is_remote` itself goes away.
- Delete `App::drive_live` (`stream/mod.rs:1092`) and the `KeySink::Direct`
  variant (`stream/mod.rs:109`) — only `Daemon` remains, rename it.
- Delete the reload inbox handoff (`mod.rs:2278`, `2291`): the TUI sends
  `ReloadExtensions` and renders the result; no local re-boot.
- Command listing (site 5): if Step B isn't done, temporarily source
  autocomplete from an empty list (commands still run via `RunCommand`, just not
  autocompleted). Land Step B next to restore it.

**Verify:** `cargo test --workspace` green. Full `bone` session against the
local daemon: turn, tools, `/config`, `/usage`, reload, resize, cancel. Confirm
`bone --connect` to a *remote* host still works (the path is now the default).

---

## What lands when Step C is done

- `App.extensions` and the local `ExtensionManager` are gone from the TUI.
- `drive_live`, the local `run_lua_command` branch, `KeySink::Direct`, the reload
  inbox — all deleted.
- The TUI's remaining `bone_core` imports are protocol types + pure helpers only.
  `boot_with_tools` (the last logic call site) is gone.
- Phase 3-pure is a no-op flip (it's now the default).
- A web client gets every slash command, hook, and pane by implementing
  `run_remote_command` in JS — zero new server-side command machinery.

---

## Risk register

| Risk | Mitigation |
|---|---|
| Remote command path has parity gaps vs `drive_live` (paste, pane cleanup, key timing) | Step 4 is an explicit gate; no deletion until verified |
| `ctx.ui.width` wrapping regresses | Step 2 lands width-over-protocol before the flip |
| Stats dashboard reads wrong host | Step 3 picks B (gate the pane); document it |
| Hooks (`session_end`, `mode_change`) stop firing | Step 1 adds `DispatchHook`; test it reaches the VM |
| Command autocomplete breaks | Step 5 ships with empty list if Step B deferred; non-fatal |
| Can't automate interactive verification | Step 4 is manual by necessity; log every exercised path |

## Ordering rationale

Step 1 (hooks) and Step 2 (width) before Step 5 because they're the non-command
VM touchpoints that would break if deleted along with the command path. Step 3
(stats) before Step 5 because the dashboard would silently show wrong data.
Step 4 (verification) is the hard gate — it's where the real risk lives, and the
one step that needs a human at a terminal. Step 5 is mechanical deletion once
1–4 are green.
