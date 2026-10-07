# Adding a panel

A panel is a persistent area docked beside, above or below the chat. It takes
room from the chat (unlike `bone.ui.win`, which floats over it), keeps its
scroll position while you do other things, can be hidden and shown again, and
can take the keyboard. Lua supplies all of its content; Rust sizes and places
it, scrolls it, and routes keys and the mouse wheel to it.

A fixed part of the screen (a file list column, a notes column) that is there
all the time is a region in `bone.ui.layout` instead (see the TUI guide).
Panels are for things you open and close at runtime.

Reference: `docs/lua.md` "Panels: docked areas you own". Source:
`crates/bone-tui/src/panel.rs` (sizing, focus, keys) and `lua.rs` (the API).
The [Bone catalog](https://github.com/vincentm65/bone-catalog) includes `review` and `mcp` packages with panels.

## Opening one

```lua
local todo = { "write tests", "update docs" }
local p = bone.ui.panel.open({
  id = "todo",                  -- default "panel<N>"; letters, digits, _ - .
  dock = "right",               -- "right" (default), "left", "top", "bottom"
  size = 32,                    -- see below
  title = "Todo",               -- a row above the content that does not scroll
  render = function(ctx)        -- every frame; or lines = { ... }
    local out = {}
    for i, t in ipairs(todo) do
      out[i] = { { (ctx.focused and "• " or "  ") .. t, "Normal" } }
    end
    return out
  end,
  keys = {
    d = function(panel) table.remove(todo, 1) end,  -- done with the first
  },
  on_key = function(key, panel) return false end,  -- true if handled
  on_close = function(panel) bone.notify("bye") end,
})
bone.keymap.set("f3", function() p:focus() end)
```

`bone.ui.panel.open(spec)` (or `bone.ui.panel(spec)`) returns a handle.

## Sizing and placement

- `size`: columns for `left`/`right`, rows for `top`/`bottom`. A number of
  cells, a fraction of the room (`0.3`), or `"auto"` to fit the content up to
  `max` (default 40 columns or 10 rows). The default is 30 columns beside the
  chat and `"auto"` above or below it. The chat always keeps 20 columns and 3
  rows; a panel that would get less than `min` (default 1) is not drawn that
  frame (`info().visible` is false).
- `left` and `right` panels take full-height columns (after any `left`/`right`
  regions), then `top` and `bottom` panels split the chat column. Among panels
  on the same side, lower `order` (default 0) is placed nearer the edge, then
  older first. Side panels get a `WinSeparator` bar next to the chat.

## Content

`render(ctx)` returns every line (or nil for none), with
`ctx = { id, dock, width, height, focused, top, title }`; for `"auto"` it gets
the most room it could have. `lines` is a fixed list instead (a table you keep
changing is fine, it is read every frame). Rust shows the rows from `top`;
`follow = true` keeps the end in view as content grows, until it is scrolled
away from it. A render error is reported once and leaves the panel empty until
`update`/`set_lines` gives it new content.

Lines are as everywhere: a string, `{ "text", "Group" }`, or
`{ fill = "─", hl = "Group" }`.

## The keyboard

`focus = true` (or `p:focus()`, `bone.ui.panel.focus(id)`, a click on it,
`focus_next`) gives a `focusable` panel the keyboard. Its `keys` run first,
then `on_key(key, panel)`, then the `panel` keymap context (or the panel's own
`context`, a named context; give it `fallback = "panel"` to keep the scroll
keys). The defaults map the arrows, page keys, `home`/`end` and the wheel to
scrolling, `esc` back to the prompt, `tab`/`shift+tab` to the next/previous
panel and `ctrl+c` to `interrupt`. Text that is not mapped is ignored. The
wheel over any panel scrolls that panel.

## The handle

| Call | Does |
|---|---|
| `p:update(spec)` | change any field above (`false` resets `size`, `max`, `title`, `context`); `focus = true/false`. Returns `false` if it is closed |
| `p:set_lines(lines)` | replace the content (a list, or a render function) |
| `p:scroll(n)`, `p:scroll("top")`, `p:scroll("bottom")` | scroll by rows or to an end |
| `p:hide()`, `p:show()`, `p:toggle()` | take it off the screen and back, keeping its state |
| `p:focus()`, `p:close()`, `p:is_open()` | |
| `p:info()` | `{ id, kind, dock, size, order, title, hidden, focusable, focus, follow, top, rows, visible, width, height, row, col }` (the last four from the last frame, while visible) |

`bone.ui.panel.get(id)` returns a handle or nil, `bone.ui.panel.list()` every
panel's info in placement order, `bone.ui.panel.focused()` the focused panel's
id, and `bone.ui.panel.update(id, spec)`, `close(id)` and `focus(id)` (nil for
the prompt) work by id.

## Lifecycle

`panel/opened`, `panel/updated` and `panel/closed` events (`kind = "panel"`),
and `on_close(panel)` when it closes. Hiding or closing the focused panel
gives the keyboard back to the prompt. Panels stay open across sessions until
closed — but the TUI's Lua reload drops them: on a `ready` event with
`reload = true`, `ev.panels` lists the ids that were open, so a plugin can
reopen its panel:

```lua
bone.on("ready", function(ev)
  if ev.reload and ev.panels and ev.panels["todo"] then p:update({}) end
end)
```

## Worked example: a job viewer

```lua
local panel
local function draw(ctx)
  local out = {}
  for _, j in ipairs(bone.job.list()) do
    local mark = j.state == "running" and "•" or "·"
    out[#out + 1] = { { mark .. " ", "Accent" }, { j.name .. "  ", "Normal" },
                      { j.state, "Dim" } }
  end
  return out
end
panel = bone.ui.panel.open({ id = "jobs", dock = "bottom", size = "auto",
                             max = 8, title = "Jobs", render = draw })
bone.keymap.set("f4", function() panel:toggle() end)
-- the panel content is read every frame, so it updates as jobs start and end
```