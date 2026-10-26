# Review fixes: GUI regressions and transcript-tail safety

Date: 2026-10-26. Scope: the working-tree review and subsequent sequential UX
verification, not the separate GUI cleanup/performance plan.

**Status: all seven fixes are implemented** (working tree, 2026-10-26). Each
was checked through the headless GUI's normal egui pass or the core's RPC
harness with throwaway checks, not committed regression tests. The six visible
UX scenarios have not been re-run against a new release binary.

## Verification baseline

Seven separate subagents ran **one at a time** using **grok / grok-4.7**,
confirmed from their session usage records. Six experienced the reported bug in
the visible desktop GUI. The zero-tail panic has an automated backend
reproduction but no normal UI path.

GUI tests used `target/release/bone`, version
`3.0.3 (870d5ff1-dirty)`, built after the reviewed source changes. Agents used
real mouse, keyboard, wheel, and screenshots in isolated applications. Where
necessary, disposable Lua configurations supplied a model response or exposed
custom-node API behavior; the implementation under test was not patched.

All disposable GUI processes were stopped and the temporary Grok subagent
provider setting was restored. Test configurations remain under
`/home/vincent/.cache/bone-ux-review/`; these are local replay aids, not portable
regression tests. Provider evidence is recorded locally in
`/home/vincent/.cache/bone-review/ux-provider-evidence.json`.

## Fix checklist

| Done | # | Fix | Verification |
|---|---|---|---|
| [x] | 1 | Handle `session/messages` with `tail: 0` without panicking | Backend reproduced; not reachable through normal UI |
| [x] | 2 | Keep Markdown tables visible while streaming | Experienced in GUI; plain-text control passed |
| [x] | 3 | Honor consumed mouse events before node/native interaction | Experienced in GUI with event counters |
| [x] | 4 | Accept explicit `list.chat = false` | Experienced in GUI; omitted-field control passed |
| [x] | 5 | Remove duplicate embedded runtime paths | Experienced through `/runtime reset all` |
| [x] | 6 | Dispatch `on_scroll` for horizontal-only wheel input | Experienced in GUI; horizontal input delivery independently verified |
| [x] | 7 | Resize the sidebar using sidebar-relative coordinates | Experienced with a left panel; no-panel control checked |

Line references below describe the reviewed working tree and may shift.

## 1. Zero transcript tail panics and poisons the session

**Priority:** high backend/API correctness. The original review classified the
panic as critical; UX verification did not establish an ordinary user-facing
trigger. Do not describe this as a reproduced GUI crash.

**Relevant code:**

- `crates/bone-core/src/lib.rs:1084–1090`, `Core::session_messages`.
- `crates/bone-proto/src/methods.rs`, `SessionMessagesParams`.

For a nonempty transcript and `tail = 0`, `at = len - tail` equals `len`.
The inclusive slice `s.messages[..=at]` then panics while the session mutex is
held. A subsequent normal request also panics because the mutex is poisoned.
An automated check reproduced both failures with a one-message session.

**UX result:** not reproducible through the UI by its nature. Built-in GUI
callers request `Some(60)` or `None`; TUI callers request `None`. No normal
history-size control generates zero. Injecting arbitrary RPC through Lua would
be an API test, not a normal UI reproduction. No live user session was poisoned.

**Repair:** explicitly handle zero before calculating or slicing the tail.
Choose and document the contract: return an empty message tail with consistent
metadata, or reject zero with `INVALID_PARAMS`. Do not rely on UI callers always
using positive values.

**Acceptance / regression tests:**

- A zero-tail request against a nonempty session returns the chosen documented
  result or a normal RPC error, never a panic.
- A subsequent full-transcript request still succeeds on the same session.
- Cover empty transcripts, tails of 1, exactly the transcript length, and larger
  than the transcript; retain turn-boundary and tool-call/result behavior.
- Document the chosen zero behavior in the protocol/API reference.

## 2. Streaming Markdown tables disappear

**Priority:** high user-visible rendering regression.

**Relevant code:**

- `runtime/lua/bone/desktop/markdown.lua:88–109`: table construction.
- `runtime/lua/bone/desktop/markdown.lua:140`: `M.spans_of`.
- `runtime/lua/bone/desktop/views.lua:97–99`: streaming indicator.

Tables now produce a column containing rows containing text cells.
`spans_of()` only checks a node and its first child, so it returns nil for the
new table structure. The assistant view then appends to nil and the list-item
error handler replaces the answer with an empty node.

**UX result:** experienced. A deterministic local provider emitted:

```markdown
| A | B |
|---|---|
| x | y |
```

It then held the turn open for 15 seconds. The answer was blank while streaming,
with a red error reporting `views.lua:99: attempt to get length of local 'last'
(a nil value)`. The table appeared after completion. A plain-text reply streamed
normally in the same window.

**Repair:** make streaming-indicator insertion support non-text container
nodes. A small safe approach is to append a separate indicator text node when
`spans_of()` returns nil. If changing traversal instead, avoid placing the
indicator in an arbitrary table header cell.

**Acceptance / regression tests:**

- Render an assistant item ending in a table with `streaming = true`: no Lua
  error, and both the table and a streaming indication remain visible.
- Complete that item: retain the table and remove the indicator.
- Retain behavior for plain paragraphs, code bands, and empty streaming output.
- Repeat the visible streaming check, not just a final completed-message check.

Local fixture: `table-stream/` under the UX cache directory.

## 3. Consumed mouse events still activate underlying nodes

**Priority:** high input correctness; intercepted gestures can activate controls.

**Relevant code:**

- `crates/bone-gui/src/keys.rs:352–367`: pointer event routing.
- `crates/bone-gui/src/view.rs`: `Gui::ui` routing order.
- `crates/bone-gui/src/nodeview/paint.rs`: egui responses and node callbacks.

Removing handled pointer events from `InputState.events` is too late: egui has
already derived pointer state and interaction results before `route_keys()`
runs. The return value for intercepted drag events is also ignored.

**UX result:** experienced with a custom-node fixture. A control click with
consumption disabled incremented click/press/release counters to 1. After F6
enabled a `bone.on("mouse", ...)` handler returning true, a second real click
was seen as down/up by that handler, but all three node counters increased to 2.
Expected: the node counters stay at 1.

**Repair:** consume events before egui derives interactions, or explicitly
suppress native/node interaction for consumed gestures. Filtering the event
vector alone is not sufficient. Preserve coherent press/drag/release state when
an intercepted gesture starts or ends.

**Acceptance / regression tests:**

- With consumption disabled, normal node click/press/release behavior still works.
- With consumption enabled for down/up, the mouse handler receives those events
  but the underlying node does not activate.
- Cover consumed drags and verify no stuck pressed/capture state after release.
- Check relevant native text-editor pointer interactions as well as node buttons.
- Tests must run through egui's normal input pass, not only call the shared mouse
  dispatch function directly.

Local fixture: `mouse-intercept/`.

## 4. Explicit `list.chat = false` breaks the entire view

**Priority:** normal; custom-node API compatibility regression.

**Relevant code:** `crates/bone-gui/src/node.rs:1259–1267`.

The list's `chat` boolean expression recognizes false as disabled, but the new
`buffer` match omits `Value::Boolean(false)` and rejects it as an invalid type.

**UX result:** experienced with a generic-list fixture. Omitting `chat` displayed
four rows. F6 set it explicitly to false and the whole view became:

```text
bone has no working view
child 8: list: chat must be true or a chat number
```

A second F6 omitted the field again and restored the list. This is a real GUI
failure through the custom-node API, not a standard chat setting.

**Repair:** accept `Value::Boolean(false)` in the buffer arm returning `None`,
while retaining `chat = false` as a generic list rather than a chat list.

**Acceptance / regression tests:**

- Omitted, nil, and false all parse and render as generic lists.
- True and valid numeric chat selections still work.
- Invalid types still report an appropriate error.
- Switching between omitted and false must not replace the GUI with its fallback.

Local fixture: `list-false/`.

## 5. Duplicate embedded paths abort `/runtime reset all`

**Priority:** normal; user-facing reset command performs only part of its work.

**Relevant code:**

- `crates/bone-lua/src/lib.rs:64–74`: embedded runtime path list.
- Same file: `runtime_overrides` and `reset_runtime_file`.
- `runtime/lua/bone/commands.lua`: `/runtime reset all` loop.

The embedded list includes each of `style`, `widgets`, `composer`, `statusbar`,
and `overlays` twice. Override enumeration retains duplicates. Resetting the
first copy moves the file, then the duplicate reset fails and aborts the loop.

**UX result:** experienced. With identical style and overlays overrides in a
disposable config, `/runtime` listed each twice. `/runtime reset all` moved style
to the backup directory, then failed with:

```text
lua/bone/nodes/style.lua is not overridden
```

Overlays remained overridden. With default autoreload, a subsequent
`Lua configuration reloaded` status masked the command error; disabling
autoreload in the fixture made it visible. The partial reset occurred in both
cases. Error-toast masking is an observation here, not an additional confirmed
fix requirement.

**Repair:** remove duplicate entries so every embedded runtime path occurs
exactly once. Add a uniqueness test to prevent recurrence.

**Acceptance / regression tests:**

- Assert uniqueness across all embedded runtime paths.
- Override at least style and overlays; enumeration lists each once.
- `/runtime reset all` moves both files to backup, leaves no overrides, and
  reports success without an intermediate duplicate-path error.
- Retain single-file reset and library-copy behavior.

Local fixture: `runtime-reset/`.

## 6. Horizontal-only wheel input does not call `on_scroll`

**Priority:** normal; horizontal input is silently lost by custom handlers.

**Relevant code:** `crates/bone-gui/src/nodeview/focus.rs:201–202`.

Callback dispatch tests only `on_dy != 0.0` even though it passes `dx` to the
handler. A horizontal-only event therefore never invokes `on_scroll`.

**UX result:** experienced. A vertical control gesture invoked 18 callbacks
(total dy −64, dx 0). After smoothing settled and F6 reset the counters,
horizontal gestures over the same node invoked zero callbacks. The same
left/right gestures moved a separate horizontal scroll rail from offset 0 to
64 and back to 0, independently confirming actual horizontal input delivery.
This was not shift-plus-vertical emulation or an input-tool limitation.

**Repair:** dispatch when either axis is nonzero, e.g.
`on_dy != 0.0 || dx != 0.0`. Preserve the existing ownership, modifiers, and
scroll-container routing behavior.

**Acceptance / regression tests:**

- Horizontal-only input invokes the handler with dy 0 and nonzero dx.
- Vertical-only and mixed-axis input still invoke the handler correctly.
- An idle zero-delta frame does not invoke it.
- Cover shift/modifier routing without conflating it with true horizontal input.
- Keep a visible horizontal-scroll positive control during UX re-verification.

Local fixture: `horizontal-wheel/`.

## 7. Sidebar resizing includes the left panel's width

**Priority:** normal; sidebar jumps in size and saves the incorrect width.

**Relevant code:** `runtime/lua/bone/desktop/sidebar.lua:524–530`.

The drag handler calculates width from `floor(pos.x + 3)`, but `on_drag` supplies
window-relative coordinates. A left-docked panel shifts the sidebar's origin
away from zero, and that displacement is incorrectly added to its width.

**UX result:** experienced in the standard GUI with a 20-cell left-docked panel.
The sidebar started at width 280, with its origin around x=193. A real 10-pixel
rightward drag changed its width to 482 and saved 482. Without the left panel,
the equivalent drag produced width 292; the small difference from 290 was
attributed to where the handle was grabbed, not a large jump.

**Repair:** calculate width in sidebar-relative coordinates, for example by
subtracting `bone.gui.rect("sessions").x`. Account for the handle's grab offset
if needed; do not simply subtract a hard-coded panel width.

**Acceptance / regression tests:**

- A small drag changes width only by the pointer's movement, with and without
  a left panel; the panel width is never added.
- Verify no jump at drag start when grabbing different points on the handle.
- Keep min/max width clamps and persistence correct.
- Check the narrow-window overlay sidebar and the ordinary docked sidebar.
- Repeat at the actual GUI coordinate scale; do not confuse screenshot pixels
  with window coordinates.

Local fixture: `sidebar-resize/`.

## Completion gate

Keep fixes focused and add regression coverage alongside each one. Existing
suites passed despite these findings, so rerunning them alone is insufficient.

Suggested targeted suites (plus any new regression tests):

```sh
cargo test -p bone-core --lib
cargo test -p bone-gui --test lua_host --test node_view --test conformance
cargo test -p bone-lua
cargo test -p bone-ui -p bone-tui -p bone-proto --lib
cargo test -p bone-proto --test golden
```

During the review, `/tmp` caused quota failures unrelated to the code; rerunning
with `TMPDIR` set to a writable directory resolved those failures. Keep it
outside `$HOME`: tests run in temporary working directories, and a `~/.bone/`
above one is found as an untrusted project config whose notice shifts the TUI
layout (`shared_api_in_the_tui` then fails). `/dev/shm` or `/var/tmp` work.

Before closing the checklist:

- All seven acceptance criteria have automated coverage where practical.
- Re-run the six visible UX scenarios against a newly built binary.
- Verify zero-tail safety on a disposable backend session, not a user's session.
- Confirm no regression in streaming paragraphs, ordinary mouse input, generic
  lists, runtime reset, vertical scrolling, or sidebar persistence.
