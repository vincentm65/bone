# Interactive Panes — Improvement Plan v1

## Goal

Refactor the pane subsystem so the background code is customizable,
maintainable, and expandable — **without changing anything the user sees or
any current Lua API behavior.** Every phase ships to a fully usable state and
is independently revertable. UX parity is the acceptance gate for each phase.

This is a refactor, not a feature drop. New user-facing capabilities
(per-pane keys, real floats, splits, `ctx.keymap`) are *unblocked* by v1 but
deliberately out of scope.

---

## The Problem Today

There are **three** Lua entry points that mutate panes and **two** Rust
transports that carry the mutations, all converging on one render target:

| Entry point (Lua)            | Transport (Rust)                  | Applies via            |
|------------------------------|-----------------------------------|------------------------|
| `ctx.ui.pane(opts)`          | `ToolLiveEvent::Pane(PaneContent)`→ mpsc channel | `apply_tool_live_event` |
| `ctx.emit_pane(table)`       | same channel (third alias)        | same                   |
| `bone.api.ui.open_float(...)`| `ViewDiff` → `UiState` (Lua app_data) | `apply_view_diffs` (render-tick `try_lock`) |

Both apply into `App::pages: Vec<PanePage>`. The split exists for a real
reason — see "Why two transports" below — but today it means:

- Two pane mutation **types** (`PaneContent` vs `ViewDiff`/`Component::Float`)
  that mean almost the same thing but drift (`Float` has no `scroll`;
  `PaneContent` has no `anchor`/`z`/`border`).
- Two **apply functions** on `App` doing near-identical upsert/remove logic.
- `menu.lua` is a 250-line monolith hand-rolling `ctx.ui.pane` + `ctx.ui.key`
  event loops. Every new interactive pane reinvents it.
- Adding a new component kind (a list, a real split, a border) means touching
  three event types and two apply paths.

### Why two transports (do not "fix" this in v1)

`ctx.ui.pane` runs *inside* a tool's Lua callback. While that callback blocks
on `ctx.ui.key()`, the **VM mutex is held**. `drain_view_diffs()` drains
`UiState` via `try_lock` on the VM — which **fails** while a tool is blocked,
so diffs queued there would never render during a key-wait. The mpsc
`ToolLiveEvent` channel is the escape hatch: pane updates leave the locked VM
and are applied on the TUI thread directly. `bone.api.ui.*` uses `UiState`
because it runs from non-blocking contexts (init.lua, autocmds, status lines).

**v1 keeps both transports. It unifies the type and the apply logic.**
Collapsing to one transport is the v2 capstone (Phase 4) and is safe to defer.

---

## Target Architecture (v1 end state)

```
Lua entry points                        Transport               Apply
───────────────────                     ─────────                ─────
ctx.ui.pane(opts)  ─┐
ctx.emit_pane(tbl) ─┼─► ViewDiff ─► ToolLiveEvent channel ─► App::apply_view_diff
                    │   (Upsert/                                    ▲
                    │    Remove)                                    │
                    │                                          one function
                    │                                               │
bone.api.ui.*      ─┴─► ViewDiff ─► UiState (app_data) ─► drain ─► App::apply_view_diff
```

- **One mutation type**: `ViewDiff` (already exists in `runtime/view.rs`).
- **One apply function**: `App::apply_view_diff(&mut self, diff) -> bool`,
  shared by both transports.
- **Two transports kept**, for the reason above.
- **One Lua abstraction**: `ui.pane` module; `menu.lua` becomes its first
  consumer.

Net result: adding a new component kind is one new `Component` variant + one
arm in `apply_view_diff` + one `as_pane_content` mapping. Building a new
interactive pane is ~30 lines of Lua on top of `ui.pane`.

---

## Phases

Each phase is shippable on its own and leaves the app fully usable.

### Phase 0 — Make `Component::Float` a strict superset of `PaneContent`

Tiny, safe prerequisite so the type unification loses no capability.

**`src/runtime/view.rs`**
- Add `scroll: usize` (default 0 via `#[serde(default)]`) to `Component::Float`.
- `as_pane_content()` maps `scroll` into the returned `PaneContent`.

**Acceptance**
- All existing tests pass; new round-trip test for `scroll`.
- `visible_rows`/`scroll` rendered identically for every current emitter
  (all of which today send `scroll: 0`, so output is byte-identical).

---

### Phase 1 — Unify the Rust pane event type on `ViewDiff`

The core refactor. After this, there is one pane-mutation type and one apply
function; the channel still exists and still carries the blocking-tool case.

**`src/tools/types.rs`**
- `ToolLiveEvent::Pane(PaneContent)` → `ToolLiveEvent::ViewDiff(ViewDiff)`.
- Keep `ToolLiveEvent::Key(KeyRequest)` as-is.

**`src/ext/ctx.rs`** (`ctx.ui.pane`)
- Build a `Component::Float` from the opts table (`source`→`id`,
  `lines`, `visible_rows`→`rect.height`, `scroll`) and send
  `ToolLiveEvent::ViewDiff(ViewDiff::Upsert { component })`.
- `ctx.emit_pane` becomes a one-line alias to `ctx.ui.pane` (kept for
  back-compat; logged as deprecated). No behavior change.

**`src/runtime/event.rs` / `driver.rs`**
- `RuntimeEvent::Pane { pane: PaneContent }` → `RuntimeEvent::ViewDiff { diff }`.
- Driver emits `RuntimeEvent::ViewDiff` when a `ToolLiveEvent::ViewDiff`
  arrives on its event channel.

**`src/agent.rs`**
- Headless path ignores `RuntimeEvent::ViewDiff` exactly as it ignores
  `RuntimeEvent::Pane` today.

**`src/ui/app/mod.rs`**
- Extract `pub(crate) fn apply_view_diff(&mut self, diff: ViewDiff) -> bool`
  from the body of the existing `apply_view_diffs` loop (one diff → one call).
- `apply_view_diffs` (UiState drain) becomes: drain → `for d { apply_view_diff(d) }`.

**`src/ui/app/stream/mod.rs`**
- `apply_tool_live_event` / `apply_and_track`: handle
  `ToolLiveEvent::ViewDiff(d)` by calling `self.apply_view_diff(d)`.
- Cancel-cleanup (`live_sources`) now tracks the float `id` from each
  `ViewDiff::Upsert` and removes it on cancel — same semantics, sourced from
  the unified type. (For `bone.api.ui`-originated floats this is already
  best-effort; channel-originated floats get exact cleanup as today.)

**Acceptance (UX parity gate — must all pass before merging)**
- `/config` tabbed editor: opens, navigates, edits, applies — identical.
- `ask_user` (single + multi-question + custom text): identical rendering & keys.
- `task_list` pane: create/complete/kill render identically; clears on done.
- `menu.select` / `multi_select` / `text_input`: scroll, action keys, tab nav,
  Esc/Enter — identical.
- Any `bone.api.ui.set_statusline` / `set_highlight` usage: identical.
- `drive_live` cancel (Esc) still removes the tool's pane.

These are pure type-routing changes; rendering output is byte-identical
because both paths already funnel through `PanePage::from_content`.

---

### Phase 2 — Lua `ui.pane` abstraction module

Pure Lua, zero Rust risk. This is the big *expandability* win: new
interactive panes stop reinventing the event loop.

**New `defaults/lua/lib/ui/pane.lua`** — a `Pane` object:

```lua
local Pane = require("ui.pane")

-- open + own a pane sourced from the channel transport
local p = Pane.new(ctx, { id = "files", title = "Files", visible_rows = 12 })

p:set_lines(lines)          -- incremental re-render (re-emits via ctx.ui.pane)
p:append(line)              -- convenience
p:close()                   -- emits empty lines → removal
p:wait_key()                -- nil-safe wrapper over ctx.ui.key
p:key_loop(function(key)    -- read/dispatch until the fn returns truthy
  if key.code == "Esc" then return "cancel" end
end)

-- shared styled-line helpers (moved out of menu.lua so all panes share them)
Pane.span(text, fg, mods)
Pane.line(...)
```

Design notes:
- `Pane.new` calls `ctx.ui.pane` (channel transport) so it works during
  blocking tools — the whole point.
- `wait_key` returns `nil` (not an error) when `ctx.ui.key` is unavailable,
  matching current `menu.lua` `next_key` behavior.
- No new user-facing API beyond the module; existing `ctx.ui.pane` /
  `ctx.ui.key` are unchanged.

**Rewrite `defaults/lua/lib/ui/menu.lua` on top of `Pane`:**
- Move `span`/`line`/`clamp`/`split_leading_circle`/`next_key`/`is_text_key`
  into (or shared from) `ui.pane`.
- `select_loop` / `text_input` keep their exact rendering and key handling —
  they just emit through `p:set_lines(lines)` and `p:wait_key()` instead of
  raw `ctx.ui.pane` / `ctx.ui.key`.
- Result: same pixels, same keys, ~80 fewer lines, one reusable foundation.

**Acceptance**
- `/config`, `ask_user`, any menu-driven command: byte-identical output and
  identical key behavior (a screenshot/asciinema diff is the gate).
- A throwaway example pane (e.g. a 15-line file picker) can be written using
  only `ui.pane` + `ui.menu` primitives — proving expandability.

---

### Phase 3 — DRY the key-delivery & cancel-cleanup plumbing (optional polish)

Lower priority; only after Phase 1+2 are stable. Pure internal cleanup, zero
UX surface.

- The pending-key plumbing (`PendingKeyReply`, the `pending_key` slots in both
  `submit_user_turn` and `drive_live`, `KeyReplyRegistry`) is extracted into a
  single `KeySink` so there's one place that resolves a `KeyEvent` to a waiter.
- Cancel-cleanup tracking generalized to a small `PaneOwnership` set on `App`
  fed by *both* transports, so the cleanup rule ("remove floats a tool opened
  if it's cancelled mid-block") is stated once.

**Acceptance**: identical behavior; the two `pending_key` sites collapse to one.

---

## Non-Goals (explicitly deferred to v2+)

These become *easy* after v1 but are not part of it (they'd change UX):

- **Real floating-window rendering** in the TUI (overlays positioned by
  `FloatRect`, borders, z-order). Today floats render as bottom-pane tabs;
  v1 keeps that.
- **Layout/split system** (`bone.api.ui.layout{...}`).
- **Per-pane key routing** (multiple panes reading keys concurrently). v1
  keeps the single-interactive-pane-at-a-time model.
- **Pane lifecycle events** (`pane_open` / `pane_close` / `pane_focus`).
- **`ctx.keymap`** (Lua driving the input buffer / submit / cancel). The
  `keymap_ctx_plan.md` design stands; v1 doesn't expose it.
- **Collapsing to one transport** — done in Phase 4 (v2).

---

## Phase 4 (v2 capstone — complete)

Collapsed the two transports into one by moving `UiState` out of Lua app-data
into a standalone `Arc<Mutex<UiState>>`:

- `ctx.ui.pane` and `bone.api.ui.*` both push `ViewDiff`s into the shared
  handle (`SharedUi`), captured by Rust closures (no `app_data` lookup).
- The TUI drains it on every tick **without touching the VM mutex** (separate
  mutex), so panes render even while a tool blocks on `ctx.ui.key()`.
- `ToolLiveEvent::ViewDiff` channel variant is retired; only
  `ToolLiveEvent::Key` remains on the channel.
- `RuntimeEvent::ViewDiff` is retired — pane updates never round-trip through
  the runtime event stream; they're drained straight from `SharedUi`.

This removes the last wart (two transports) and makes pane updates strictly
more responsive (the old `try_lock`-on-VM skip is gone; a plain blocking lock
on the standalone mutex always succeeds).

---

## Risk & Rollback

- **Phase 0**: additive field with serde default. Zero risk.
- **Phase 1**: type-routing refactor across ~6 files. Risk = a missed call
  site. Mitigation: compiler-driven (exhaustive matches on `ToolLiveEvent` /
  `RuntimeEvent` / `ViewDiff` force every site to update). Rollback = single
  revert; no on-disk format changes.
- **Phase 2**: Lua-only. Rollback = restore `menu.lua`. No Rust rebuild needed.
- **Phase 3**: internal; gated behind Phase 1+2 stability.

UX-parity checks (the commands/behaviors listed under each phase's Acceptance)
are run before and after every phase. Any divergence blocks the phase.

---

## File Change Summary

| Phase | File | Change |
|-------|------|--------|
| 0 | `src/runtime/view.rs` | add `scroll` to `Float`; map in `as_pane_content` |
| 1 | `src/tools/types.rs` | `ToolLiveEvent::Pane` → `ViewDiff(ViewDiff)` |
| 1 | `src/ext/ctx.rs` | `ctx.ui.pane` emits `ViewDiff::Upsert(Float)`; alias `ctx.emit_pane` |
| 1 | `src/runtime/event.rs` | `RuntimeEvent::Pane` → `ViewDiff { diff }` |
| 1 | `src/runtime/driver.rs` | emit `RuntimeEvent::ViewDiff` |
| 1 | `src/agent.rs` | ignore `RuntimeEvent::ViewDiff` (headless) |
| 1 | `src/ui/app/mod.rs` | extract `apply_view_diff`; both paths call it |
| 1 | `src/ui/app/stream/mod.rs` | handle `ToolLiveEvent::ViewDiff`; cleanup from float ids |
| 2 | `defaults/lua/lib/ui/pane.lua` | **new** — reusable `Pane` module |
| 2 | `defaults/lua/lib/ui/menu.lua` | rewritten on top of `ui.pane` |
| 3 | `src/ui/app/stream/mod.rs`, `mod.rs` | `KeySink` + `PaneOwnership` consolidation |

## Success Criteria for v1

1. Zero observable change for any existing user Lua, tool, or command.
2. One pane-mutation type (`ViewDiff`) and one apply function
   (`App::apply_view_diff`) — verifiable by grep.
3. A new interactive pane can be built in `ui.pane` without touching Rust and
   without copy-pasting `menu.lua`'s event loop.
4. Adding a new `Component` variant touches exactly: the enum, `as_pane_content`,
   and `apply_view_diff` — nothing else.
