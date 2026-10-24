# The GUI's own view: a node tree in Lua, no text grid

## Goal

The GUI stops drawing a character grid. Lua describes the window as a tree of
nodes with CSS-like styles (flex rows and columns, padding, borders, radii,
pixel sizes, fonts, images), and Rust lays it out and paints it with egui.
Everything is customizable from Lua, with no hardcoded chrome.

The TUI and the GUI share the core, the protocol, and the *behavior* layer:
commands, tools, keybindings (keymaps), options, events, plugins, and the data
Lua reads (chats, processes, usage). They do not share a layout or a view
format. The TUI is unchanged throughout.

## Principles

- **Rust draws, Lua decides**, as before, but Lua decides in nodes, not cells.
- **Opt-in until it replaces the grid.** `bone.gui.view(fn)` turns the node
  renderer on; without it the old grid screen is drawn, so every phase leaves
  a working GUI.
- **The layout solver is pure Rust** (no egui), testable with fake text
  metrics.
- **Behavior stays shared.** Keys, commands and options keep working through
  `bone-ui`; the node view only changes how things are laid out and drawn.
- **No cells in the new path.** Sizes are logical pixels, `%`, or flex.

## The node model (fixed in phase 1)

A node is a Lua table. Its array part is its children. A node with `text` or
`spans` is a text node; otherwise it is a box.

```lua
{ direction = "row", gap = 8, padding = { 8, 12 }, bg = "#1c1c1c",
  radius = 10, border = 1, border_color = "#2c2c2c",
  { text = "bone", size = 15, bold = true, color = "Accent" },
  { grow = 1 },
  { text = "ready", color = "#888" },
}
```

| Group | Fields |
|---|---|
| Box | `direction` (`row`/`column`, default `column`), `gap`, `align` (cross axis: `stretch` default, `start`, `center`, `end`), `justify` (`start`, `center`, `end`, `between`) |
| Size | `width`, `height`, `min_width`, `max_width`, `min_height`, `max_height` (px number or `"50%"`), `grow`, `shrink` |
| Space | `padding`, `margin` (n, `{v, h}` or `{t, r, b, l}`) |
| Paint | `bg`, `border` (width), `border_color`, `radius`, `overflow` (`visible`/`hidden`) |
| Text | `text` or `spans = { { text, color, bold, italic, size, font } }`, `color`, `size`, `font` (`sans`/`mono`), `bold`, `italic`, `text_align`, `wrap` |

Colors are `#rgb`/`#rrggbb`, or a highlight group name (its background for
`bg`, its foreground elsewhere).

## Phases

### Phase 1: node model, solver, painter

- `node.rs` parses a Lua table into a typed tree and resolves colors.
- `flex.rs` is the solver: intrinsic sizes, grow/shrink, gaps, alignment,
  min/max, percentages, over a `Metrics` trait for text.
- `paint.rs` measures text with egui and paints boxes, borders, radii, text
  and clipping.
- `bone.gui.view(fn)` registers the root function (`fn(ctx)` with `ctx =
  { width, height }` in pixels returns the root node); `Gui::ui` draws it
  instead of the grid when set. The whole tree is rebuilt each frame.
**Status:** done (`node.rs`, `flex.rs`, `nodeview.rs`).

- Exit: a Lua view lays out and paints in the headless GUI; solver unit tests
  pass; the grid path is untouched when no view is set.

### Phase 2: events and input

**Status:** done. A node with `on_key`, `on_click` or `focusable` takes the
keyboard focus (tab, ctrl+p, a press, `autofocus`); a `block_input` box keeps
it; the keymaps take what the node does not.

- Hit testing by node `id`; `on_click`, `on_hover`, `on_scroll`, `on_key`
  handlers as Lua callbacks; `cursor`, `focusable`, a focus ring.
- `overflow = "scroll"` with wheel and scrollbars.
- An `input` node bound to the prompt editor (cursor, selection, IME, paste,
  images) so the composer can be a node.
- Keymaps keep running through `bone-ui`; a node can own a keymap context.

### Phase 3: retained updates and virtual lists

**Status:** done. Lua builds the tree only when dirty; `list` nodes make and
measure only the items in view (10 000 items: under 200 made, none while still).
The chat list (`chat = true`) keeps each item's node across rebuilds until
the shared render cache says the item changed, so typing makes none.

- Stop rebuilding everything each frame: Lua returns a new tree only when
  state changed (`bone.gui.view` re-run on `changed`/events), keyed nodes,
  cached layout and galleys per subtree.
- A `list` node that virtualizes a long sequence (the transcript), with
  item heights measured on demand and scroll anchoring.
- Budgets: 10k-item transcript streaming at 60 fps, as the grid does today.

### Phase 4: the transcript in nodes

**Status:** done. `bone.nodes` turns views' `block`s into nodes (and views may
return `node`); text selects across the items in view. Images open on click.

- Chat views return nodes (`bone.gui.views.<kind>`) instead of `lines` and
  `block`: user bubbles, Markdown (shared parser from `bone-ui`), code
  blocks, tool cards, reasoning, images, errors, with text selection and
  copy across items.
- Scroll, follow-the-end, jump to item, search hooks.

### Phase 5: the rest of the window

**Status:** done as `bone.nodes.window`: composer, `/` menu, tray, status bar,
welcome, jump-to-latest, toast, the ask dialog, docked `blocks` panels (the
sessions sidebar). Settings, help and health stay windows (`blocks` popups).
Overlays are absolute nodes with `z` and `block_input`, rather than a separate
`bone.gui.overlay`.

- Composer, sidebar, tray, status bar, welcome, settings, help, health as
  nodes; popups, panels, menus, the `/` suggestions and the ask dialog as
  overlays (`bone.gui.overlay`), with focus and dismissal.
- Port the `desktop` library fully; keep `standard` and `bare` working
  through the grid until they are removed.

### Phase 6: retire the grid

**Status:** done. `screen_ui` and the grid leaves are gone; `desktop` and
`standard` are the node window and `bare` the same without item views. What
remains of cells is a unit: Lua's windows (`bone.ui.popup`), which plugins
open from `ui.lua` for both frontends, are sized in monospace cells and
placed in pixels; a press on one is the shared `mouse` event, sent by its
node. No grid is laid over the window. `ui.layout` and
`ui.panels` are no longer GUI capabilities; the four cell-layout conformance
scripts run only in the TUI. The views, help, settings and health build nodes
directly; windows and panels take `node = fn` in their spec, kept by the
GUI's Rust host (`bone._gui_blocks` and `bone.nodes.block` are gone). The GUI's chats keep no
rows (`ChatBuffer<()>`: the shared cache only says which items changed), and
the sessions sidebar takes the keyboard as nodes, not through a panel. Not
done: docked panels of `lines` (a `/term` panel) are not drawn in the GUI.

- The GUI no longer calls `bone.ui.layout`, `lines` views, `bone.ui.panel`
  or `popup` in the cell sense; those ops remain for the TUI. Removed
  `screen.rs`, `block.rs`, `rich.rs` and the `bone._gui_blocks` patch.
- `bone.gui` API docs, `customizing-gui.md` rewritten, GUI conformance and
  headless tests moved to the node view, `docs/architecture.md` updated.
- Plugins: `gui.lua` gets the node API; `ui.lua` is behavior only.

## Risks

- **Per-frame Lua cost** on large trees: addressed in phase 3; phase 1 only
  proves the model on small views.
- **Text selection and bidirectional text** remain egui limits.
- **API size**: CSS has a lot of surface. Add fields only when a screen needs
  them.
