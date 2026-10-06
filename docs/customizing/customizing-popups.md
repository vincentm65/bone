# Popups, pickers, pagers and notifications

A popup is a window drawn over the screen that can take the keyboard. Lua
draws every cell; Rust only places it and clears what is behind it. Use a
panel (the other guide) for a docked area that takes room from the chat; use a
popup for transient, floating things: confirmations, pickers, a pager, a
streaming answer.

Reference: `docs/lua.md` "UI API". Source: `crates/bone-tui/src/ui.rs`
(windows, pickers, pagers) and `lua.rs`. Complete examples:
`examples/plugins/approve/` (confirmation popups), `ask-model/` (a pager
filled by a streaming model call), `switch/` (a provider picker).

## A focused window

```lua
local w = bone.ui.win{
  lines = { { "choose one", "PopupTitle" }, "  a", "  b" },
  -- or lines = function(ctx) return { ... } end  (called every frame;
  -- ctx = { width, height } the most room it has)
  width = 24, height = 4,        -- default: fitting the lines
  anchor = "screen",             -- "screen" (default), "chat", or "prompt"
  row = -1, col = 0,             -- in the anchor; default centered; 0 is the
                                 -- top/left edge; negative counts from the
                                 -- bottom/right (-1 touches it)
  z = 0,                         -- stacking order (newer on top among equals)
  focus = true,                  -- take the keyboard (default false)
  keys = { esc = function() bone.ui.close(w) end },
  on_key = function(name) return false end,  -- the rest; true if handled
  guard = 100,                   -- ms to ignore keys after opening
}
```

A focused window gets keys first: its `keys` map, then `on_key(name)`, then
the `popup` keymap context (`ctrl+c` cancels the turn), and the rest are
ignored. `bone.ui.popup{ ... }` is `bone.ui.win` with `focus = true`.

Managing windows:

```lua
bone.ui.update(id, { lines = ..., row = 0 })  -- change any field in place
bone.ui.close(id)
bone.ui.is_open(id)
```

A line is a list of items: a string, `{ "text", "Group" }`, or
`{ fill = "─", hl = "Group" }`. `bone.ui.box(lines, { title, title_hl,
border_hl, width, pad, chars })` puts a rounded border around lines
(`PopupBorder`/`PopupTitle` by default) — only used if you call it.

## A picker

```lua
local h = bone.ui.select(items, {
  prompt = "provider",
  format = function(item) return item.name end,   -- what typing filters
  on_choice = function(item, index)
    if not item then return end                    -- esc
    bone.notify("picked " .. item.name)
  end,
  loading = false,      -- show "loading…" until set_items
  empty = "none",       -- shown when the list is empty
  footer = nil, width = nil, height = nil,
})
h:set_items(more)       -- fill it later
h:close()
```

Typing filters (matching `format(item)`), `up`/`down` (or `ctrl+p`/`ctrl+n`,
the wheel) move, `enter` calls `on_choice(item, index)`, `esc` calls
`on_choice(nil)`. `bone.ui.sessions()` is the built-in session picker (`ctrl+o`);
define your own with `bone.ui.select` to change how sessions are listed.

## A pager

```lua
local h = bone.ui.pager(content, { title = "log", width = 80, height = 24 })
h:set(new_content)      -- replace
h:close()
```

`content` is a string (wrapped; `#` lines are headings) or a list of lines.
`up`/`down`/wheel, `pageup`/`pagedown`, `home`/`end`, `esc` or `q` closes.

Streaming into a pager (from `ask-model`):

```lua
local answer, pager = "", bone.ui.pager("…", { title = "ask" })
bone.model.complete({ system = "Answer briefly.", prompt = question },
  function(d) if d.text then answer = answer .. d.text; pager:set(answer) end end,
  function(r, err) pager:set(r and r.content or ("error: " .. tostring(err))) end)
```

## Notifications

```lua
bone.notify("saved")            -- "info" (default)
bone.notify("it failed", "error")
```

`print(...)` is `bone.notify`. Without a `message` region in the layout,
notifications take rows at the bottom.

## Confirming something from the core

A core hook or tool can pause and ask the user (`bone.ask`); the TUI shows the
question however it likes and answers it back. The `approve` plugin is a
complete example; the shape is:

```lua
bone.on("ask/requested", function(ev)
  -- ev: { ask_id, question = ... }  (question is whatever the core sent)
  local w = bone.ui.popup{
    lines = { { ev.question.text or "", "Normal" },
              "y = yes   n = no" },
    keys = {
      y = function() bone.request("ask/respond", { ask_id = ev.ask_id, answer = "yes" }) end,
      n = function() bone.request("ask/respond", { ask_id = ev.ask_id, answer = "no" }) end,
      esc = function() bone.request("ask/respond", { ask_id = ev.ask_id, answer = "no" }) end,
    },
  }
end)
```

`bone.request(name, args, cb)` calls any protocol method, and `bone.rpc.call`
calls a function the core registered with `bone.rpc.register`.

## Worked example: a confirm-before-act

```lua
local function confirm(text, on_yes)
  local w = bone.ui.popup{
    lines = { { text, "Normal" }, "  y = yes   n = no" },
    keys = {
      y = function() bone.ui.close(w); on_yes() end,
      n = function() bone.ui.close(w) end,
      esc = function() bone.ui.close(w) end,
    },
  }
end
confirm("Delete the branch?", function()
  bone.system("git push origin --delete branch", {}, function(r, err)
    bone.notify(r and ("exit " .. r.code) or tostring(err), "info")
  end)
end)
```