# Customizing the GUI's look and behavior

Where: `~/.bone/gui.lua` (the GUI only) or `~/.bone/ui.lua` (the TUI and the
GUI); a plugin's `gui.lua` and `ui.lua` work the same. The GUI is `bone --gui`
(a build with the `gui` feature). It reloads when a file changes, and a Lua
error never crashes it: it is shown once and listed by `/health`.

The GUI runs the same Lua UI API as the TUI (keymaps, commands, options,
events, views, layout, windows, panels), so `customizing-tui.md`,
`customizing-panels.md`, `customizing-popups.md` and `customizing-chat.md`
apply to it too; lay things out in character units. This guide is what is
only the GUI's. Reference: `docs/lua.md` "Frontends: the shared UI API".
Source: `crates/bone-gui/src/`, the built-in library in
`runtime/gui/desktop.lua` and `runtime/lua/bone/desktop/`.

## Libraries

The GUI's library is chosen apart from the TUI's (`/tui`):

```
/gui                     list them (● marks the one in use)
/gui use NAME            standard | bare | desktop | a copy of yours; reloads
/gui create NAME [base]  copy the standard UI (base: bare or desktop) to edit
/gui delete NAME         move a copy not in use aside
```

`desktop` is the default: graphite colors, bubbles for your messages,
proportional Markdown answers, tool calls as cards, a card composer.
`standard` draws what the TUI draws; `bare` is plain text. A copy lives in
`~/.bone/libraries/NAME/`; its `gui/defaults.lua` is the entry point, and any
file in it replaces the built-in one of the same path. For one change, a file
at the same path under `~/.bone/runtime/` does the same without a copy.

## Options

Set in `gui.lua`, or with `/config` (saved as `gui.<name>` in `settings.json`):

```lua
bone.o.font = "/path/Mono.ttf"        -- the monospace font (empty: built in)
bone.o.font_size = 14                 -- points; one cell is one advance wide
bone.o.font_sans = ""                 -- proportional text (empty: the system's)
bone.o.font_sans_bold = ""            -- its bold and italic faces
bone.o.font_sans_italic = ""
bone.o.queue_mode = "next"            -- as in the TUI: steer or next
```

## The window: `bone.gui`

```lua
bone.gui.window{ title = "bone", width = 1100, height = 760, maximized = false }
bone.gui.window_info()                -- { width, height, columns, fullscreen, maximized, focused }
bone.gui.notify("bone", "done")       -- a desktop notification (gui.notifications)
bone.gui.open_file({ title = "Pick", filters = { { "Images", { "png" } } } }, function(path) end)
bone.gui.save_file({ name = "out.txt" }, function(path) end)   -- gui.dialogs
```

Check `bone.has_capability("gui.window")` before using these from shared code.

## Rich text: blocks

A chat view may return `block` as well as `lines`. The GUI draws the block
(wrapped, proportional text in a box) and the TUI draws `lines`, so a view can
serve both:

```lua
bone.ui.views.notice = function(item)
  return {
    block = {
      paragraphs = { { spans = { { "Hello ", "Normal" }, { text = "world", bold = true } } } },
      size = 15, align = "right", max_width = 0.8,
      bg = "UserMessage", border = "PromptBorder", radius = 12,
      pad = { 8, 12 }, margin = { 6, 6 },
    },
    lines = { { { "Hello world", "Normal" } } },
  }
end
```

A span is `{ text, group }` or `{ text =, hl =, bold, italic, code, size }`; a
paragraph also takes `font`, `size`, `fg`, `gap`, `indent`, and `bg` with
`pad`. `fill = true` makes a box span the allowed width. Sizes are points.

A panel or window can show blocks too (`gui.blocks`): give `blocks =
function(ctx) return { { id = …, block = {…}, selected = bool } } end`, with
`ctx = { width, height }` in points, and `on_block(id)` for a click. Its keys,
focus and closing stay the panel's or window's.
