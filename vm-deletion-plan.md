# Cleanup plan — delete the TUI client's duplicate Lua VM

## Context / why

Default `bone` now spawns a `bone serve` subprocess and attaches as a protocol
client, but the TUI **still boots its own Lua VM** (`App.extensions`) alongside
the daemon's. Same `init.lua`, booted twice: two VM boots at startup, double the
extension-state memory, and two copies that must stay in agreement.

After the wire work (committed: `f8f0a31`, `29a2fc1`, `0a67c3b`) every piece of
display state the client needs is now sent over the protocol in
`RuntimeEvent::FrontendState` (theme, keymap, banner, commands, **tool defs +
display configs**) and consumed by the client — *except tool rendering, which
still reads the local VM's `self.tools`.* So the client VM is now almost pure
duplication.

**Goal:** delete the client VM so the daemon is the sole VM owner; the TUI
becomes a Rust-only frontend. No new capability (a web client is already
unblocked by the wire work) — this is duplication removal: one source of truth,
~half the startup Lua work, less memory, no divergence risk.

**Cost accepted:** the in-process runtime path and its fallback go away, so
`bone` will *require* a spawnable local daemon (the `--connect` and default
spawn paths). Sandboxes that can't spawn subprocesses lose the fallback.

**Testing reality:** the TUI can't be driven headlessly here and this sandbox
kills `bone serve` subprocesses, so Phases must be **smoke-tested by the user in
a real terminal**. Phase 1 is a safe checkpoint; Phase 2 is the irreversible cut.

---

## What the client VM provides today, and its wire replacement

| Client-VM use | Site(s) | Replacement | Status |
|---|---|---|---|
| theme/keymap/banner/config at boot | `with_daemon` | `FrontendState` | consumed ✓ |
| command list (autocomplete) | `collect_commands` | `wire_commands` | consumed ✓ |
| command routing gate (is it a Lua cmd?) | `handle_command` (`extensions.commands()`/`is_available()`) | `wire_commands` | **to do** |
| tool defs/display (render rows, estimate ctx) | `self.tools` (`display_for_call` ×3, `definitions()` ×1) | `FrontendState.tool_defs/tool_display` | on wire ✓, **client to do** |
| run slash commands | `run_lua_command` `is_remote==false` branch | `run_remote_command` | remote path exists ✓ |
| drain Lua UI diffs | `extensions.drain_view_diffs()` | wire `ViewDiff`s | remote path exists ✓ |
| hooks (`session_end`, `mode_change`) | `dispatch_*` | `DispatchHook` | done ✓ |
| terminal width | `ensure_viewport_and_draw` | `SetTerminalWidth` | done ✓ |
| reload | `reload_extensions` + `reload_inbox` | `ReloadExtensions` command | command exists ✓ |
| ctx for local commands | `app_ctx_state` | (dies with local command path) | — |
| tool UI state | `self.tools.state_map.clear()` | drop (client doesn't read it) | — |

---

## Phase 1 — Client renders tools from the wire (VM still present; safe checkpoint)

Make the render/estimate path stop reading the local `ToolHandler`, so the only
remaining client-VM consumer is gone *before* we delete the VM.

1. **New lightweight type** `WireTools { defs: Vec<ToolDefinition>, display:
   HashMap<String, ToolDisplayConfig> }` with `display_for_call(&ToolCall) ->
   Option<&ToolDisplayConfig>` and `definitions() -> &[ToolDefinition]`.
   - File: `tui/src/ui/app/mod.rs` (or a small new module).
2. Replace the App field `tools: ToolHandler` → `tools: WireTools`.
   - Update the 4 read sites: `display_for_call` at `stream/mod.rs:822`, `:976`,
     `mod.rs:682`; `definitions()` at `mod.rs:611` (context estimate).
   - Drop `state_map.clear()` at `mod.rs:706` (only the daemon needs tool state).
3. Feed `WireTools`:
   - **Remote:** extend `apply_frontend_state` to deserialize `tool_defs` +
     `tool_display` and populate `self.tools`.
   - **In-process (transitional bridge):** in the `InProcess` arm of
     `with_daemon`, populate `WireTools` from `booted.tools` (`definitions()` +
     `display_map()`) so this path keeps rendering tools until Phase 2 deletes it.
4. Keep `self.extensions` for now (still feeds nothing the renderer reads, but
   stays so the in-process path and `is_remote==false` branches still compile).

**Build:** `cargo build`. **Smoke test (user):** run `bone`; confirm tool rows
(esp. `subagent`/`task_list`/`ask_user`/`browser`) render with their custom
display, and the context-size estimate looks right.

---

## Phase 2 — Delete the VM, the in-process path, and the dead command path (the cut)

Once Phase 1 is confirmed, remove the now-unused machinery. One commit, since
the pieces are entangled (they share `is_remote`/`extensions`/`DaemonSource`).

Delete / collapse:

1. **`App.extensions`** field + the `boot_with_tools` call in `with_daemon`; the
   boot-time `theme/keymap/config/banner` reads (now from `FrontendState`).
   - Banner timing: the boot path can no longer build the banner locally. Insert
     the banner into scrollback when `FrontendState` arrives (in
     `apply_frontend_state`), guarded so it shows once.
2. **In-process path:** `DaemonSource::InProcess`, `App::new`, the in-process
   `run_daemon` spawn, and `reload_inbox` (field + param threading). `with_daemon`
   keeps only the `Remote` arm and boots no VM.
3. **`main.rs` fallback:** default path always spawns + connects; if spawn/connect
   fails, return an error instead of falling back to in-process.
4. **Local command path:** the `is_remote==false` branch of `run_lua_command`,
   `App::drive_live` (`stream/mod.rs`), the `KeySink::Direct` variant (rename
   `Daemon` → the only variant), and `app_ctx_state`.
5. **`is_remote`** flag: remove the field; every `if self.is_remote { … } else {
   … }` collapses to the remote branch (`dispatch_session_end`, `mode_change`,
   width sync, `run_lua_command`, `open_stats_dashboard`).
6. **`reload_extensions`:** drop the local re-boot/inbox handoff; just send
   `RuntimeCommand::ReloadExtensions` and render the daemon's resulting
   `Status` + `FrontendState`.
7. **`handle_command` routing gates:** replace `extensions.commands()` /
   `is_available()` checks with `wire_commands` membership.

Then prune now-dead imports (`crate::ext::boot_with_tools`, `ExtensionManager`,
`ToolHandler`) so the TUI's core deps are protocol/data types + pure helpers only.

**Build:** `cargo build` + `cargo test --workspace`. **Smoke test (user, the big
one — no fallback now):**
- `bone` launches, banner shows, theme/keymap correct, `/`-autocomplete lists
  catalog commands.
- Send a turn; tool approval (accept + deny); a picker (`/themes`, `/config`)
  nav/select/Esc; `/usage`; `/tools reload` (reload path); resize; Ctrl+C ×2
  quits with **no orphaned `bone serve`** (`pgrep -af 'bone serve'`).
- `bone --connect <remote>` against a separate host still works.

---

## Rollback

Each phase is its own commit on `step-c-remote-parity`. If Phase 2 misbehaves in
the real terminal, `git revert` that commit restores the dual-VM (working) state;
the wire layer (Phases 1 / 2.5 / earlier) stays intact and useful regardless.

## Out of scope (kept as-is)

- `/config` *mutations* over RPC (already deferred; daemon notes "not applied").
- `self.user_config` / renderer plumbing beyond what `apply_frontend_state`
  already feeds.
- The `SetTerminalWidth`-per-frame debounce (separate minor cleanup).
