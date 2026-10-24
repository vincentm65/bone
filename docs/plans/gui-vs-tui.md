# GUI and TUI: what they share, and where the GUI's layout falls short

Written after reviewing whether the GUI is an equal citizen of the core with
full Lua customization. The architecture goal is met; the customization goal
is met for behaviour and only partly for looks. This note says why, and what
to change.

## What they share

| Layer | Shared? | Where |
|---|---|---|
| Core, sessions, turns, tools, hooks, asks | Yes, one core; neither frontend knows the other | `bone-core`, `bone-server` |
| Wire protocol, requests, events, reconnect | Yes | `bone-proto`, `bone-client` |
| Client-side state: chat items and reducer, composer/editor, keymaps and key sequences, commands, options, popups and panels (state), jobs, processes, terminal emulation, themes, markdown parsing, layout tree and solver | Yes, in cells | `bone-ui` |
| Lua UI API (`bone.ui`, `bone.keymap`, `bone.o`, `bone.on`, `bone.cmd`, `bone.chat`, `bone.prompt`, `bone.job`, plugins, libraries) | Yes: about 160 ops, one definition, behind the `Host` trait | `bone_ui::lua`, `runtime/ui/api.lua` |
| Behaviour modules in Lua: commands, `/` menu, keys, setup, config data, `bone.chats`, `bone.background`, `bone.usage`, `bone.timing` | Yes | `runtime/lua/bone/` |
| Conformance scripts | Yes, run in both | `tests/ui/*.lua` |
| Plugin `ui.lua` | Yes | |

| Layer | TUI only | GUI only |
|---|---|---|
| Application struct, `Host` impl | `bone_tui::App` | `bone_gui::Gui` (about 880 lines of accessor plumbing each) |
| Drawing | ratatui cell buffer; terminal escape handling | egui; proportional text, rounded boxes, images, shadows |
| Input | crossterm keys and mouse; terminal clipboard helper | winit/egui keys, native text editing, IME, drag and drop |
| Own Lua ops | `bone.tui` | `bone.gui`: window, notifications, file dialogs, library switch |
| Own Lua files | `tui.lua`, `runtime/tui/` | `gui.lua`, `runtime/gui/` |
| Looks (default library) | `runtime/lua/bone/ui/` ("standard") | `runtime/lua/bone/desktop/` ("desktop"), separate colorscheme and views |
| Library choice | `/tui` | `/gui` |
| Rich text | none: `lines` only | `block` views, `blocks` for panels and windows |
| Embedded terminals | `/term` panels over local PTYs (Unix) | PTY panels via `bone.job`; no interactive terminal widget of its own |
| Asks | drawn by a plugin handler (`ask/requested` + popup) | a node dialog in `bone.nodes.window`; Rust's dialog is the fallback when no `ask/requested` handler is loaded |

Not shared by design: a plugin's `tui.lua` in the GUI, drafts, scroll
positions and open windows between clients.

## The layout and customization issue (resolved)

This section describes the problem as found. The GUI now draws a node tree
(`docs/plans/gui-native-view.md`), which removes points 1 to 4.

Customization through Lua is complete for **behaviour**: keys, commands,
options, events, panels, popups, views, libraries, plugins. What is not
complete is **geometry and chrome**. Four causes:

1. **The whole screen is a character grid.** `screen_ui` turns the window
   into `cols x rows` and places every leaf, panel, popup, separator and the
   prompt at integer cell rects (`Cells::rect`). Anything between cells cannot
   be said: a 12 px gap, a 260 px sidebar, a card that is 1.5 rows tall.
   Leftover pixels at the edges are wasted, and a sidebar's width in pixels
   changes with `font_size`.
2. **Pixel units were planned and never built.** The roadmap promised
   `{ px = 240 }` behind `gui.pixels`. There is no such capability. Mouse
   events carry cell `x`/`y` only (no `px`/`py`).
3. **Two drawing paths, and only one is a layout.** The grid path
   (`lines`, shared, cell-exact) and the `block` path (GUI-only, proportional,
   pixel-sized inside). Blocks are not layout nodes. A chat view may return a
   `block`; a panel or popup gets `blocks` through a wrapper that
   `runtime/gui/api.lua` patches over `bone.ui.panel.open`, `popup`, `win` and
   `update`, keeping the spec in a side table, `bone._gui_blocks`. The prompt,
   statusline, divider and regions cannot use blocks at all, so the desktop
   library fakes a card with a rounded border drawn on a cell rect. A block
   also takes whole chat rows, so scrolling stays in rows.
4. **Chrome is hardcoded in Rust, with no Lua hook.** Window margin
   (24 x 16 in `view.rs`), the panel fill (`lifted(canvas)`), popup radius,
   padding and shadow (the old grid painter, since removed), scrollbar widths, widget corner radii, the
   1-cell separator lines, message-row padding, and fallback colors (`0x16`,
   `0xee` and so on) when a colorscheme leaves a group unset. A library can
   recolor these through groups but cannot reshape them.

The cost of this is the duality tax: a view that serves both frontends
returns `block` and `lines`, and the GUI's best looks are exactly the ones
that bypass the shared API (point 3). That is why the claim "one API, two
frontends" is true for behaviour and weaker for looks.

## Options, cheapest first

1. **Chrome as options and groups.** Make the window margin, panel fill,
   popup radius/shadow/padding, scrollbar and separator style `bone.o` options
   or highlight-group fields in `gui`. Small, removes point 4, no layout change.
2. **Blocks as a first-class field.** Put `blocks` in the shared spec
   validation (ignored by the TUI) instead of a Lua monkeypatch, and accept it
   on the prompt, statusline and regions. Removes the side table in point 3.
3. **Pixel units.** Implement `size = { px = N }` in the layout solver as a
   GUI-only unit: the solver works in cells, so the GUI converts pixels to
   cells for the grid and keeps the remainder as a pixel offset or inset.
   Add `px`/`py` to the `mouse` event. This is the real fix for points 1 and 2
   and the largest change; it touches `bone-ui::layout`.
4. **Decide the principle.** Either the GUI's layout stays cell-based and
   blocks are the pixel escape hatch (then do 1 and 2 and document it), or the
   solver gains pixel units (3). Doing 1 and 2 first costs little and keeps 3
   open.
5. **Smaller:** share the `Host` accessor plumbing between frontends with a
   struct both embed, so the two 880-line `lua.rs` files stop drifting.
