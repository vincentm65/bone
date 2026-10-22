# Bone GUI roadmap

## Goal

Build a desktop GUI for bone that is a **first-class citizen, not a
second-class port of the TUI**, while leaving the current TUI working exactly
as it does today.

1. **First-class GUI.** The GUI is as capable and as customizable as the TUI,
   in Rust and in Lua. Anything you can change in the TUI from Lua — layout,
   views, panels, popups, prompt, keymaps, commands, mouse handling, themes,
   plugins, libraries — you can change in the GUI the same way. It is a native
   desktop client, not a terminal emulator around the TUI and not built on
   `bone-tui::App`.
2. **The current TUI is not changed.** TUI behavior, its Lua API, its
   configuration files, and its tests stay as they are. Code may move out of
   `bone-tui` into a shared crate so both frontends use one implementation,
   but every such move is behavior-neutral and the existing TUI tests pass
   unchanged as the proof. Anything new for the TUI is additive and opt-in.
3. **Side by side.** The TUI and GUI can run at the same time, connect to one
   core, open the same sessions, and receive the same events.

"First-class" is measured, not hoped for: every shared Lua API has one
definition, one conformance test, and passes in both frontends. A shared API
that behaves differently in the GUI is a bug.

## Non-negotiable principles

- **The core is the authority.** Sessions, transcripts, turns, queues, tools,
  questions, settings, and event ordering remain in `bone-core`/`bone-server`.
- **Both frontends are clients.** `bone-tui` and `bone-gui` use
  `bone-client`/`bone-proto`; neither talks directly to the other.
- **One implementation of client-side state.** Anything that is not drawing —
  the transcript reducer, markdown parsing, prompt editing, keymaps, commands,
  options, highlight groups — lives in a shared crate used by both frontends.
  Frontends own rendering, input translation, and windowing only.
- **GUI is not built on `bone-tui::App`.** It gets its own application state,
  renderer, Lua dispatcher, and lifecycle, built on the shared crate.
- **Lua is a product API.** Shared UI behavior has a documented contract and a
  conformance suite; frontend-specific features are exposed through
  capabilities.
- **Extraction is a move, not a rewrite.** Code moved out of `bone-tui` keeps
  its behavior; existing TUI tests pass unchanged.
- **GPU acceleration is an implementation detail.** Prefer an iGPU through a
  `wgpu`-backed renderer, but retain a software fallback and never require a
  specific GPU.

## Where the code is today

What already exists and should be reused, not redesigned:

- **Multi-client delivery.** `bone-server` broadcasts every event to every
  connection. `turn/started` carries the user message for other clients,
  `ask/resolved` fires when any client answers, `ask/list` recovers missed
  questions, and `session/messages` returns `active_turn`, `queue`, and
  `queue_paused` — a complete resync snapshot.
- **Concurrency rules.** The first `ask/respond` wins; a second answer is an
  `INVALID_PARAMS` error (covered in `agent_tests.rs`). `turn/start` on a
  session with a running turn fails with `BUSY`; `queue/add` starts or queues.
- **Lag recovery.** A connection whose broadcast receiver falls behind gets
  `session/updated` with `reason: "lagged"` and an empty `session_id`; the TUI
  reloads every open chat (`App::reload_chat`). The GUI must do the same.
- **Reconnect.** The TUI's `--connect` mode reconnects, re-fetches settings and
  open asks, and fires `server/reconnected` (`bone-tui/src/lib.rs`,
  `App::reconnected`).
- **Spawn-or-connect.** `crates/bone/src/main.rs` resolves the bind/connect
  race for the default socket.
- **Sides.** `bone_lua::Side` (`Core`, `Tui`) already drives capabilities and
  state paths (`state/<side>/`, `state/shared/`).
- **Shared Lua UI.** The standard UI (`runtime/lua/bone/ui/`: views, layout,
  statusline, sessions, tray) and modules such as `menu`, `help`, `setup`,
  `config`, and `commands` are Lua, not Rust.
- **Libraries.** `~/.bone/library` selects `standard`, `bare`, or a custom
  copy in `~/.bone/libraries/<name>/`, whose files override runtime files by
  relative path (`bone_lua::tui_library`). Bare skips the user's and project's
  `tui.lua`. Plugins must behave the same on every library.
- **Headless driver.** `bone-tui/src/headless.rs` runs the real TUI against an
  in-memory buffer; the conformance suite builds on it.

What is terminal-shaped and needs a decision:

- The Lua UI contract (`runtime/tui/api.lua`, `bone-tui/src/lua.rs`,
  `docs/lua.md`) is expressed in screen cells: mouse `x`/`y`,
  `bone.ui.geometry()` rects, panel and layout `size`.
- Lua bindings run inside `App::with_api`, which lends the whole `App` to Lua
  through `lua.scope`. Every binding is written against `App` directly.
- Highlight groups resolve to `ratatui::style::Style`; keys are crossterm
  `KeyEvent`s; `ChatBuffer` caches `ratatui::text::Line`, and `chat::bare`
  renders to `Line`s.
- `process/*` exposes PTY output with escape codes and `process/resize` in
  `cols`/`rows`. The standard UI's tray (`runtime/lua/bone/ui/tray.lua`) draws
  process output with `bone.processes.screen`, a `vt100` emulation in the TUI.

Dependencies of the candidate modules for extraction:

| Module | Depends on | Extraction |
| --- | --- | --- |
| `markdown.rs`, `text.rs`, `editor.rs`, `options.rs` | nothing in the crate | move |
| `layout.rs` | ratatui `Rect` (in `Placed` only) | move the ids and `Window`; `Placed` stays in the TUI |
| `theme.rs` | ratatui `Style`/`Color` | own style types, ratatui conversion in the TUI |
| `keys.rs` | crossterm `KeyEvent` | own key type, crossterm conversion in the TUI |
| `keymap.rs` | `keys` | move after `keys` |
| `composer.rs` | `editor`, `layout` | move after `layout` |
| `ui.rs` | ratatui `Line`, `App`, `Theme`, `chat::bare` | split: the Lua UI model (`LayoutNode`, `Size`, `Item`, `PromptSpec`, `Border`, `Spinner`, `parse_items`/`parse_lines`) moves; `render_items` and the `impl App` block stay |
| `chat.rs` | `data`, `text`, ratatui `Line` | keep the line cache and `bare` in the TUI, move the reducer |
| `term.rs` | `vt100`, ratatui `Style`, `ui::Item` | move after `theme` and `ui`; returns `bone-ui` items |
| `commands.rs`, `data.rs` | `impl App` blocks | split the pure parts out; the rest stays per frontend |
| `panel.rs`, `jobs.rs` | `impl App`, ratatui `Frame` | panel/job ops become shared behind `Host`; drawing stays per frontend |
| `selection.rs`, `render.rs`, `terminal.rs`, `clipboard.rs` | ratatui, crossterm | TUI only |

`ui.rs` is the Lua UI model itself, so it is the extraction that makes
layouts, panels, and styled lines shareable; everything in Phase 3 and 4
depends on it.

## Target architecture

```text
bone-core ── bone-server ──┬── bone-tui ── ratatui/crossterm adapter + TUI Lua host
                           └── bone-gui ── desktop adapter + GUI Lua host
                                   │
                    both depend on bone-ui (shared client state, no rendering)

bone-proto / bone-client: wire contract, transport, spawn-or-connect, reconnect
bone-lua: Lua state setup, sides, capabilities, plugins, state, docs
```

Crates:

- `bone-ui`: transcript reducer, markdown, composer, keymap engine, commands,
  options, backend-neutral styles (`Color`, `Style`, highlight groups), key and
  pointer event types, layout tree and solver over abstract units, terminal
  emulation for process output (`vt100` is a pure parser), and the shared Lua
  API bindings. No `ratatui`, crossterm, PTY, or windowing dependency.
- `bone-tui`: terminal frontend; converts `bone-ui` values to ratatui and
  crossterm events to `bone-ui` events.
- `bone-gui`: desktop frontend; same, for the chosen GUI stack.
- `bone-client`: gains the reconnect logic from `bone-tui`, so both
  frontends share it. Spawn-or-connect stays in the `bone` binary, which
  launches either frontend.

## Lua/configuration model

Add `Side::Gui`. Configuration layers, in load order per frontend (matching
today's `load_user_files`):

```text
runtime/<side>/api.lua, runtime/<side>/defaults.lua   built-in (library may override)
plugin/ui.lua, plugin/<side>.lua                      each enabled plugin
saved <side>.* and ui.* settings                      settings.json
~/.bone/ui.lua, ~/.bone/<side>.lua                    user (skipped by bare)
.bone/ui.lua, .bone/<side>.lua                        trusted project (skipped by bare)
ready event
```

where `<side>` is `tui` or `gui`. **Both frontends load `ui.lua`**; that is
what makes shared code shared. Today's `tui.lua` files keep working unchanged
in the TUI.

Libraries cover both frontends. The `~/.bone/library` choice is shared, and a
library overrides files by relative path as today, so a library may contain
`tui/…`, `gui/…`, and shared `lua/bone/ui/…` files. Files a library omits fall
back to the built-ins, so a TUI-only custom library still gets the standard
GUI. Bare exists for both frontends and keeps its rule: minimal, but every
feature reachable. Plugins stay library-agnostic on both frontends.

Plugin layout:

```text
plugin/
  core.lua              core/server side
  ui.lua                shared TUI/GUI code
  tui.lua               terminal-specific code
  gui.lua               desktop-specific code
  lua/                  shared modules
```

Capabilities follow the existing naming: shared ones under `ui.*`
(`ui.keymaps`, `ui.panels`, `ui.chat`, …), with `tui.*` and `gui.*` for
frontend-only features. During migration the TUI advertises both the old
`tui.*` names and the new `ui.*` names so existing plugins keep working; the
duplicated `tui.*` names are deprecated once Phase 5 ports the catalog, and
removed in the next major version.

State scopes: `state/core/`, `state/tui/`, `state/gui/`, `state/shared/`.
`settings.json` remains core-owned; clients use `settings/*` and
`settings/changed`, with `tui.*`, `gui.*`, and shared `ui.*` namespaces.

## Lua UI contract

### Units

Shared layouts must work in both frontends, so sizes are abstract:

- A number in `size`, panel widths, and similar fields is **character units**:
  terminal cells in the TUI, the base font's advance width (columns) or line
  height (rows) in the GUI.
- Fractions (`0.3`), `"auto"`, `min`, and `max` keep their current meaning.
- GUI-only code may also use logical pixels (`{ px = 240 }`) behind
  `gui.pixels`; the TUI rejects them.

### Pointer events

The shared `mouse` event keeps its hit information — `index`, `line`,
`region`, `row`, `col`, `panel`, `panel_line` — which is what handlers actually
use. Raw coordinates are frontend-specific: `x`/`y` cells in the TUI, `px`/`py`
logical pixels in the GUI. Hover and scroll are shared events; gestures are
GUI-only.

### Styles

Highlight groups and styled-text lines (`{ { "text", "Normal" }, … }`) are
shared and resolve to `bone-ui` styles. The GUI renders them as rich text, not
as a terminal grid. GUI-only style attributes (font family/size, padding,
rounded borders) are additive fields the TUI ignores.

### Classification

Every entry in `docs/lua.md` is classified as one of:

- **shared** — same meaning in both; moves to `ui.*` and the conformance suite;
- **shared with units** — shared once expressed in character units;
- **TUI-only** — terminal cell geometry, terminal-specific options such as
  synchronized updates;
- **GUI-only** — native menus, notifications, file dialogs, drag/drop, images
  and GPU content, accessibility roles, window state, DPI;
- **redesign** — APIs whose current shape cannot be shared without breaking it.

The classification is a table in `docs/lua.md`, published before GUI Lua work
starts.

### Text and wrapping

Lua wraps text itself: `bone.text.wrap`, `chat::bare(width)`, and render
callbacks all take a width in columns. A proportional font would not match
those widths. So the GUI draws Lua-produced lines in a **monospace font**:
one character unit is one advance width, and shared output is exact in both
frontends. Proportional text is a later GUI-only addition (for example
markdown body text that the GUI wraps itself), behind a capability.

### Host and dispatch

The Lua bindings are one string-op dispatcher (`dispatch(app, lua, op, args)`
in `lua.rs`, about 130 ops) lent through `with_api`. That is the seam:

- Each frontend implements a `Host` trait: accessors to the shared state
  (as `bone-ui` types it stores itself) and the services only it has
  (calling Lua callbacks, redraw, geometry, clipboard).
- `bone_ui::lua::dispatch(host, lua, op, args)` handles shared ops; unknown
  ops fall through to the frontend's own dispatcher.
- Ops move into the shared dispatcher one group at a time.

### Processes

Terminal emulation moves to `bone-ui`, so `bone.processes.screen` and
`job:screen` are shared: both return styled lines for a given rows × cols, and
the GUI draws them in a monospace font. That keeps the standard tray working in
the GUI. An interactive embedded terminal (keyboard input to the process) is a
later GUI-only capability (`gui.terminal`). `process/resize` stays in
cols/rows; the GUI computes them from the area and font metrics.

## Toolkit selection

The leading candidate is **egui** (eframe, `wgpu` with the `glow` backend as
fallback). The TUI already draws a retained Lua tree with an immediate-mode
renderer — ratatui redraws every frame, Lua output is cached by
`rev`/`item_rev` — so egui matches `render.rs` most closely, and it has
AccessKit and IME. `iced` is the alternative. The spike *is* the Phase 2
vertical slice, not a separate prototype; the main thing it must prove is
text selection across a long, virtualized transcript. Judge it against:

- driving a retained tree from Lua without re-running Lua callbacks every frame;
- text selection across a long, virtualized transcript;
- streaming deltas into a 10k-item transcript at 60 fps;
- IME and bidirectional text;
- accessibility (AccessKit or equivalent);
- software rendering fallback and adapter diagnostics;
- Linux (X11 and Wayland), macOS, and Windows.

The toolkit draws `bone-ui` values; it never owns layout semantics. Keep the
adapter thin enough to replace.

## Testing

- **Conformance suite.** Lua test scripts under `tests/ui/` exercise each
  shared API and assert on output through a frontend-neutral harness: the
  existing headless TUI and a headless GUI that renders to an in-memory scene.
  Every shared API needs a conformance test before it is marked shared.
- **Multi-client tests.** One core, one TUI client, one GUI client:
  1. both attach to the same session;
  2. a turn started by one is visible in the other;
  3. tool and message events arrive in order;
  4. both can submit to the same session;
  5. a question answered in one disappears from the other;
  6. reconnecting the GUI restores the transcript, running turn, queue, and
     open questions;
  7. a GUI that lags the broadcast recovers through the `"lagged"` reload.
- **Existing TUI tests** keep their assertions through every extraction.
  Path-only edits (`app.options` → `app.ui.options`, moved `use` paths) are
  allowed; a changed assertion is not.

## Phased implementation

Phases 0 and 1 run together: the cheap moves in Phase 1 need no decisions,
and the Lua API is classified group by group as bindings migrate (Phase 3),
not all up front.

### Phase 0: contract and decisions

- Document the multi-client guarantees that already exist in
  `docs/protocol.md`: broadcast delivery, ask ownership (first answer wins,
  everyone hears `ask/resolved`), concurrent submits (`turn/start` is `BUSY`,
  `queue/add` queues), lag recovery (`session/updated` with `"lagged"`), and
  the resync snapshot.
- Write the `Host`/shared-dispatch design (see "Host and
  dispatch").
- Decide units, the pointer event shape, and the `ui.*` capability names.
- Set up the conformance harness against the headless TUI.

**Exit condition:** protocol guarantees documented, the dispatch design
written, and the conformance harness runs.

**Status:** done except the units, pointer-event, and capability-name
decisions, which land with the binding groups that need them. The
guarantees are in `docs/protocol.md` ("Multiple clients"); the design is
`docs/plans/gui-lua-host.md`. The harness is `crates/bone/tests/conformance.rs`:
it runs each `tests/ui/*.lua` against a real core in the headless TUI, through
a `UiDriver` trait the headless GUI will implement too. A script is a chunk,
or returns a list of steps run with events handled between them; `eq(got,
want, what)` is its one helper.

### Phase 1: extract `bone-ui`

Small commits, with TUI test assertions unchanged, in dependency order (see
the table above). `bone-tui` re-exports moved modules under their old paths so
call sites do not change:

1. Pure moves: `markdown.rs`, `text.rs`, `editor.rs`, `options.rs`, and the
   ids and `Window` from `layout.rs`.
2. Own `Color`/`Style` types and highlight groups from `theme.rs`; own key
   type from `keys.rs`; ratatui and crossterm conversions stay in `bone-tui`.
3. `keymap.rs`; `composer.rs`.
4. The Lua UI model from `ui.rs`.
5. The `chat.rs` reducer, leaving the `Line` cache and `bare` in `bone-tui`.
6. The pure parts of `commands.rs` and `data.rs`, out of their `impl App`
   blocks.
7. `vt100` screen emulation (`term.rs`) behind a frontend-neutral
   styled-lines result.
8. Reconnect logic into `bone-client` (`bone_client::reconnect`).
   Spawn-or-connect (`bind_or_connect`) stays in `crates/bone`: binding the
   socket is the server's side (`bone_server::Listener`), and the GUI shares
   it by starting through the same binary.

Steps 2–6 are refactors, not moves; each lands with no behavior change and
the TUI tests as the check.

**Exit condition:** `bone-ui` builds without ratatui or crossterm, and the TUI
uses it with no behavior change.

**Status:** done. `bone-ui` holds `text`, `markdown`, `editor`, `options`,
`layout` (ids, `Window`), `style`, `theme`, `keys`, `keymap`, `composer`,
`model` (the Lua UI model from `ui.rs`), `chat` (`ChatBuffer<L>`, generic
over the frontend's line type), `data`, `commands` (argument specs), and
`term`. Its dependencies are `bone-proto`, `bone-media`, `mlua`, `vt100`,
`bitflags`, `serde_json`, and `unicode-width`. The TUI re-exports each under
its old path; ratatui and crossterm conversions live in `bone-tui`'s
`theme.rs` and `keys.rs`.

### Phase 2: GUI vertical slice (and toolkit spike)

Start once Phase 1 step 5 (the chat reducer) lands. Build on egui first; if
the spike criteria fail, switch to `iced` before going further. Record the
decision here.

Create `crates/bone-gui`:

- window, event loop, and renderer with the chosen toolkit;
- connect, spawn-or-connect, and reconnect through `bone-client`;
- `initialize`, session list and selector, `session/messages`, event stream;
- transcript driven by the shared reducer, prompt via the shared composer,
  submit, streaming output, questions/approvals;
- `session/updated` handling, including the `"lagged"` reload of every open
  chat;
- the bare visual style: transcript, prompt, minimal status, no forced
  dashboard chrome.

**Exit condition:** the multi-client tests pass with one TUI and one GUI.

**Status:** done. `crates/bone-gui` (egui/eframe 0.36, Rust 1.95 for that
crate only), opened with `bone --gui` from a binary built with
`--features gui`; the default build does not include it.
`crates/bone-gui/tests/multi_client.rs` covers multi-client cases 1–7 (6
through a proxy that drops the GUI's connection while the core keeps
running). Along the way, `bone_ui::chat::apply_event` and `bare_text`
became shared, so both frontends apply transcript events the same way.

**Toolkit decision: egui.** Against the spike criteria:

| Criterion | Result |
| --- | --- |
| Retained tree, Lua not re-run per frame | Yes: rows are cached per item by `rev` in `ChatBuffer<L>`, as in the TUI; frames draw cached rows. |
| Streaming into a 10k-item transcript at 60 fps | Yes: 10k items, release build, headless (layout, no GPU paint): first frame 10 ms (wraps everything), then 0.6 ms per frame idle or streaming a delta. |
| Text selection across a long, virtualized transcript | Partly: rows are selectable labels and egui selects across labels, but rows scrolled out of `show_rows` drop out of a selection. Needs a transcript widget that keeps selection by item and offset (Phase 4). |
| IME | Through winit (untested here: no display). |
| Bidirectional text | Not supported by egui's text layout; would need `parley` or `cosmic-text` (Phase 4 decision). |
| Accessibility | AccessKit, on by default in eframe. |
| Software fallback, diagnostics | wgpu with a low-power adapter by default; wgpu's GL backend covers Mesa's software rasterizer, and eframe's glow renderer is tried if wgpu fails. The renderer is shown in the status line; adapter details are Phase 4. |
| Linux (X11, Wayland), macOS, Windows | Supported by eframe; only Linux builds have been run. |

Bidirectional text and cross-scroll selection are the two known gaps; both
sit behind the transcript widget, which the adapter keeps replaceable.

**Found along the way:** the TUI applies replies and server events from two
channels (`tokio::select!`), so a `session/messages` reply can overtake
older events still queued and those events are applied on top of the
snapshot (for example a `message/completed` shown twice after attaching
while far behind). The GUI uses one ordered channel and does not have this.
Not fixed: the TUI is unchanged in this work.

### Phase 3: GUI Lua host

- Add `Side::Gui`, GUI capabilities, and the `ui.lua`/`gui.lua` loader for
  plugins, user, and project config; add `ui.lua` loading to the TUI.
- Add `runtime/gui/api.lua` and `runtime/gui/defaults.lua`; the GUI defaults
  call the same `require("bone.ui").setup(opts)` as the TUI. Add the GUI bare
  library.
- Bind the shared Lua API once in `bone-ui` through the shared dispatcher
  over `Host`, keeping the `with_api`/`lua.scope` lending pattern. Migrate
  `lua.rs` one op group at a time (text, options, keymap, prompt, chat data,
  commands, …); each group is classified in `docs/lua.md` and gets its
  conformance tests when it moves.
- Move the shared parts of `runtime/tui/api.lua` into a shared
  `runtime/ui/api.lua` that both sides load.
- Lua reload with the TUI's safety rule: build a fresh state, keep the old one
  on failure, clean up owned callbacks and jobs.
- `bone.api_info()` and capability tests for GUI Lua.

**Exit condition:** the standard and bare libraries run in the GUI with no
GUI-specific changes to `runtime/lua/bone/ui/`, and the conformance suite
passes in both frontends for everything classified shared so far.

**Status:** done. The shared dispatcher is `bone_ui::lua` over a `Host`
trait (`docs/plans/gui-lua-host.md`; it replaced the `UiState` split):
options, events, commands (and running them, `/model` included), keymaps
(set/del/context/list/current), highlights and colorschemes, chat data,
core requests and background work, and the small frontend services
(session, prompt text, processes, chat view, refresh). The Lua state setup,
the API lending, and drawing chat items through Lua views (`render_chat`,
`guarded`, `layout_items`) are shared too. `runtime/tui/api.lua` became
`runtime/ui/api.lua`, loaded by both; `runtime/tui/api.lua` and
`runtime/gui/api.lua` are each side's own (empty for now).

The GUI loads `ui/api.lua`, `gui/api.lua`, `gui/defaults.lua` (the same
`require("bone.ui").setup()`) or `gui/bare.lua`, plugins' and the user's
`ui.lua` and `gui.lua`, fires `ready`, draws its transcript through the
standard views, and reloads with the TUI's rule. Every `tests/ui/*.lua`
script passes in both frontends through one runner (`tests/ui/runner.rs`);
`ui.*` capabilities name the shared groups (both frontends; the TUI keeps
its `tui.*` names), classified in `docs/lua.md`.

The one change under `runtime/lua/bone/ui/` was frontend-neutral: the
`tool_detail` option the views read moved from `tui/defaults.lua` into
`views.lua`.

Still per frontend, and not yet in the GUI (each raises "`<op>`: not
available in the GUI yet" there): popups and windows, panels, regions,
layout, the statusline and prompt drawing, prompt editing, jobs, plugin
lifecycle and projects, libraries, key focus and raw interception, and
cell geometry. Those are Phase 4 (layout, panels, popups, input) and
Phase 5 (plugins, projects).

### Phase 4: layout, panels, popups, and native input

- Shared layout tree and solver over character units in `bone-ui`; the GUI
  resolves units to logical pixels with DPI scaling.
- Panels, popups, regions, views, and `bone.ui.geometry()` in both frontends.
- Shared pointer events with hit information; GUI hover, selection,
  clipboard, drag/drop, and IME.
- Font selection, shaping, and wrapping through the toolkit's text stack.
- Renderer adapter selection with iGPU preference and software fallback,
  shown in diagnostics.

**Exit condition:** a `ui.lua` that defines a layout, a panel, a popup, a view,
keymaps, and a mouse handler behaves the same in both frontends, verified by
the conformance suite.

**Status:** done; `tests/ui/frontend.lua` is the exit check (the runner can
click: a step sets `__click = { x, y }`). Shared in `bone-ui`: the layout
solver (`bone_ui::layout::place` over a `Measure` trait, character units),
reading `bone.ui` (layout, regions, statusline, prompt spec, popup lines),
the prompt box's geometry, `bone.ui.geometry`, windows (`bone_ui::popup`:
state, placement, ops), panels (`bone_ui::panel` and `lua::panels`: state,
docking, measuring, per-frame content, ops), the key engine
(`lua::input`: windows', panels', raw and keymap keys, sequences,
`bone.ui.actions`, contexts), and the `mouse` event with what is under it.
The TUI draws and handles input exactly as before through these.

The GUI lays its window out with `bone.ui.layout` and draws every leaf,
window and docked panel; the standard sessions panel (`ctrl+o`), the slash
commands and the `/` menu run there, so its own sidebar is gone. Keys go to
the shared engine when it has something to do with them and otherwise to
egui's text box (input methods, selection and native editing); Lua's
`bone.ui.actions` see editing keys first. Pointer presses, drags and the
wheel become the same `mouse` events by character cell. `bone.o.font` and
`font_size` set the font; the status line names the renderer and adapter
and warns about a software one.

Moved from `tui/defaults.lua` into the standard library (frontend-neutral,
the TUI unchanged): the standard key bindings (`bone/keys.lua`), and
`bone.ui.sessions`, `help` and `suggestions` (into `bone.ui.setup`).

Left for later: the GUI's prompt cursor and range editing ops (its text box
keeps its own cursor), prompt images and history, chat scrolling from keys,
jobs, and an AccessKit pass over the Lua-drawn rows.

### Phase 5: plugin and configuration ecosystem

- GUI support in plugin manifests and catalog metadata.
- Port built-in and catalog plugins from `tui.lua` to `ui.lua` where they are
  shared; add `gui.lua` halves where they are not.
- GUI-only capabilities: native menus, notifications, file dialogs, images,
  accessibility labels, window state, and the `gui.terminal` widget.
- `/health` for frontend, renderer, connection, Lua, and plugin capabilities.
- GUI config watcher and project trust flow.
- Document writing plugins that support both frontends.

**Exit condition:** a plugin can cleanly support core-only, TUI-only, GUI-only,
or shared UI behavior, and the catalog says which.

**Status:** done. Plugin lifecycle is shared (`bone_ui::lua::plugins`): a
plugin's files run as their owner in each frontend (`ui.lua`, then
`tui.lua` or `gui.lua`), unloading removes what it made, and a trusted
project's `.bone/` runs the same way (trust is kept once for both).
`bone.plugin.list()` reports `core` and `frontends` per plugin, and the
catalog shows `both`/`tui`/`gui`/`core` from a package's files or a
`frontends` field in `catalog.json`, and loads this frontend's half on
install (`tests/ui/plugins.lua`, `tests/ui/catalog.lua`). `/health` is the
same page in both, with each frontend's checks (the GUI's: renderer and
adapter, connection, Lua, plugins, font). The GUI's own API is `bone.gui`
(`gui.window`), reported only there; copying to the
clipboard is shared. `docs/customizing/customizing-plugins.md` explains
writing a plugin for both frontends.

Not done here: porting catalog packages from `tui.lua` to `ui.lua` happens
in the bone-catalog repository; native OS notifications and file dialogs
need new dependencies (for example `notify-rust`, `rfd`) and wait for that
decision; the `gui.terminal` widget, images, and an AccessKit pass over
Lua-drawn rows are left for Phase 6.

### Phase 6: production hardening

- Reconnect and server restart tests; Lua reload and plugin unload tests in
  both frontends.
- DPI, resize, text shaping, image, clipboard, and software-renderer tests.
- GPU adapter loss and fallback.
- Performance profiling for long transcripts, streaming deltas, and Lua render
  callbacks.
- Linux, macOS, and Windows packaging.
- Architecture, Lua, plugin, and protocol documentation.

**Status:** mostly done.
- **Restart:** `bone-gui/tests/multi_client.rs` runs a TUI and a GUI on one
  server, and restarts the server under a GUI that reconnects and gets its
  session back. Lua reload and plugin unload were covered in Phases 3 and 5,
  in both frontends.
- **Resize and HiDPI:** the GUI fires `resize` when its cell grid changes.
  `lua_host.rs` checks that a resized window and a 2x scale give the right
  columns and rows.
- **Renderer:** wgpu is tried first, then Glow, and the adapter is reported
  in `/health` with a warning for a software adapter. egui-wgpu already
  reconfigures an outdated surface and recreates a lost one. eframe 0.36
  does not expose device loss, so losing the device is fatal and the next
  launch picks a renderer again.
- **Performance:** measured headless with the standard Lua views on 4000
  items: the first frame takes 385 ms, idle frames 0.48 ms, and frames while
  streaming 1.37 ms. The first frame is slow because every item renders
  through Lua once. The TUI does the same, so rendering only the visible
  items would help both frontends.
- **Packaging:** `scripts/dist.sh --gui` builds
  `dist/bone-gui-<version>-<target>.tar.gz`, and `.github/workflows/gui-build.yml`
  builds and tests the GUI binary on Linux, macOS and Windows as artifacts.
  The npm packages keep the TUI-only binary.
- **Docs:** `docs/architecture.md` describes `bone-ui`, `bone-gui` and the
  `Host` design.

Since then: the chat renders the visible items first and the rest within a
25 ms frame budget (first frame 27 ms for 4000 items); a lost GPU device
reopens the window with the same session; wide, fallback and combining
characters keep the cell grid; the prompt takes images (`/attach`, dropped
files, alt+v or `/paste`; egui keeps ctrl+v for text), and a row naming an
image previews it on hover and opens it on click.

Since then (Phases 7 and 8): `bone.job` and the process tracking moved to
`bone-ui`; `/term` opens terminals in panels; AccessKit labels cover the
rows Lua paints; notifications and file dialogs are in (`notify-rust`,
`rfd` through the XDG portal).

### Phase 7: parity with the TUI

The GUI does what the TUI does, with the shared defaults. Every op the Lua
API calls exists in the GUI: sessions open in chats of their own (each with
its draft); the core's processes (the tray), `bone.job`, the library and
runtime ops are shared code in `bone-ui`; the prompt's cursor, selection,
editing ops, history and paste placeholders match; Lua can scroll the chat.
`tests/ui/` covers it in both frontends (`sessions`, `prompt`,
`scrolling`, `libraries`, `jobs`); the runner can click, paste and wait,
and fails a script whose steps go with a reload. A TUI library without a
`gui/` folder no longer replaces the GUI's UI.

**Status:** done.

### Phase 8: the desktop library

A GUI library chosen apart from the TUI's (`/gui use`, `gui-library`), and
a built-in one, `desktop`, that draws the standard UI like the Android app:
graphite colors, bubbles for your messages, proportional Markdown answers,
monospace tool rows, a card composer. It is Lua
(`runtime/gui/desktop.lua`, `runtime/lua/bone/desktop/`) over one new
building block: a view may return a `block` of rich text the GUI wraps and
boxes itself, with `lines` for a frontend that does not draw blocks.

**Status:** done, and the GUI's default: with no `/gui use` choice the
GUI draws `desktop` (`/gui use standard` is the TUI's look). Real bold and
italic come from the system's sans faces (or `font_sans*`); block text is
selectable and previews its images; the sessions sidebar and tray take the
panel surfaces, and an answer that finishes in the background notifies.

### Phase 9: the GUI's looks apart from the TUI's

`desktop` no longer starts from the TUI's standard UI: it has its own
colorscheme (`graphite`), views for every kind of item (tool calls as
cards), layout, composer, status bar, sessions sidebar, tray and `/`
suggestions (`runtime/lua/bone/desktop/`). The two share behaviour only:
the commands, keys and `/` menu, and data modules taken out of the TUI's
drawing code — `bone.chats` (the sidebar's), `bone.background` (the
tray's), `bone.usage` (the status line's), `bone.timing` (turn footers).
The TUI draws from the same modules as before. A test checks none of the
TUI's drawing modules load in the GUI. Looks for one frontend belong in
its own file (`tui.lua`, `gui.lua`); `ui.lua` is for both.

**Status:** done.

## Explicit non-goals for the first milestone

- Rewriting `bone-tui`; extraction is moves only.
- Making every existing `tui.lua` plugin work in the GUI automatically.
- Sharing prompt drafts, scroll positions, or open windows between clients.
- Replacing the core protocol with direct shared-memory communication.
- Requiring a discrete GPU.
- Building a widget toolkit before a working chat client exists.
- Protocol sequence numbers; resync from snapshots instead, until a test shows
  they are needed.

## Acceptance criteria

- TUI and GUI run simultaneously against one core and one data directory, and
  the multi-client tests pass.
- A GUI reconnect fully resynchronizes after missed events.
- Every API classified shared passes the conformance suite in both frontends.
- The standard UI and a shared `ui.lua` run unmodified in both frontends.
- GUI layout, views, panels, prompt, keymaps, mouse handling, and popups are
  customizable in Lua.
- `bone-ui` and `bone-gui` do not depend on `bone-tui`, ratatui, or crossterm.
- The renderer uses the iGPU when available and has a working software
  fallback.
- Existing TUI tests pass unchanged.

## Risks

- **GUI API lags the TUI.** The main risk to "first-class". Mitigated by one
  shared binding in `bone-ui`, the conformance suite, and the rule that new
  shared APIs land in both frontends at once.
- **Binding migration size.** `lua.rs` (~2,800 lines) and
  `runtime/tui/api.lua` (~1,500) are written against `App`. Migrating them in
  binding groups behind a host trait keeps each step small and testable;
  a big-bang port would stall the GUI and risk TUI regressions.
- **Cell-shaped APIs.** Some current APIs may not translate to character
  units; they go in the redesign bucket in Phase 0, not in a GUI workaround.
- **Toolkit mismatch.** An immediate-mode toolkit fights a retained Lua tree;
  the spike decides, and the adapter stays thin.
- **Lua render cost.** Render callbacks must be cached by revision (as
  `ChatBuffer` already does with `rev`/`item_rev`), not re-run per frame.
- **Plugin fragmentation.** Mitigated by `ui.lua` plus explicit `tui.lua` and
  `gui.lua`, rather than silently running incompatible code.
- **Text rendering complexity.** Use the toolkit's text stack; postpone a
  custom GPU text renderer until profiling justifies it.
- **GPU availability.** Treat `wgpu` acceleration as opportunistic and show a
  diagnostic when the fallback is active.
