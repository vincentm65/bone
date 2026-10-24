-- The GUI's own Lua API, loaded after runtime/ui/api.lua (the API every
-- frontend shares) and before runtime/gui/defaults.lua and ~/.bone/gui.lua.
-- Check bone.has_capability("gui.window") before using it from shared code.

local api = bone._api

bone.gui = {
  --- Change the window: { title, width, height (logical pixels),
  --- fullscreen, maximized, minimized }; only what is given changes.
  window = function(spec)
    api("gui_window", spec or {})
  end,
  --- { width, height (pixels), columns, rows (character cells), cell =
  --- { w, h } (a character cell of the monospace font, in pixels),
  --- fullscreen, maximized, focused }.
  window_info = function()
    return api("gui_window_info")
  end,
  --- The window as a tree of nodes: view(function(ctx) return node end),
  --- ctx = { width, height } in pixels, called when something changed
  --- (see invalidate); view(nil) leaves only the fallback. A node is a
  --- table, its array part the children; `text` or `spans` makes a text
  --- node. See docs/customizing/customizing-gui.md for the fields. An
  --- error is shown once and the fallback is drawn meanwhile.
  view = function(fn)
    api("gui_view", fn)
  end,
  --- Scroll the node view's scroll container `id` (a node with
  --- overflow = "scroll" and that id) to pixels, "top" or "end".
  scroll = function(id, to)
    api("gui_scroll", id, to)
  end,
  --- { offset, max, viewport } of scroll container `id`, or nil.
  scroll_info = function(id)
    return api("gui_scroll_info", id)
  end,
  --- Lua builds the node view's tree when something changed (an event, a
  --- key, a click, an option, a resize, an op that changes the UI); call
  --- this when your view reads something else: now, or in `ms`.
  invalidate = function(ms)
    api("gui_invalidate", ms)
  end,
  --- The message shown last ({ text, error }) or nil; status_clear()
  --- dismisses it.
  status = function()
    return api("gui_status")
  end,
  status_clear = function()
    api("gui_status_clear")
  end,
  --- The view drawn when the view fails or none is set: fallback(function(ctx)
  --- return node end), ctx = { width, height, error }. A default is set
  --- below; replace it to change how a broken window looks.
  fallback = function(fn)
    api("gui_fallback", fn)
  end,
  --- Scroll list `id` to its item `index` (from 1): at = "top" (default),
  --- "center", "bottom" or "nearest". True once the list has been drawn.
  scroll_item = function(id, index, at)
    return api("gui_scroll_item", id, index, at)
  end,
  --- The windows of bone.ui.popup / win, placed as in the TUI: a list of
  --- { id, x, y, w, h (pixels), z, focus, lines = { { { text, hl } } } },
  --- and `cell = { w, h }` (the monospace cell they are laid out in).
  --- bone.nodes.popups draws them (a window with a `node` as that).
  popups = function()
    return api("gui_popups")
  end,
  --- Register a font family from a file: font("serif", "/path/Serif.ttf"),
  --- then `font = "serif"` on any node or span. The built-in ones are
  --- "sans" and "mono".
  font = function(name, path)
    api("gui_font", name, path)
  end,
  --- The text of the text input node `id` (text_input = {…}), or nil; set
  --- it, or give it the keyboard.
  input_text = function(id)
    return api("gui_input_text", id)
  end,
  set_input_text = function(id, text)
    api("gui_input_set", id, text)
  end,
  --- Give node or text input `id` the keyboard (nil: back to the prompt).
  --- A node the keyboard reaches has `on_key`, `on_click` or `focusable =
  --- true`; tab and ctrl+p move between them.
  focus = function(id)
    api("gui_focus", id)
  end,
  --- The id of the node with the keyboard, or nil.
  focused = function()
    return api("gui_focused")
  end,
  --- A desktop notification (gui.notifications).
  notify = function(title, body)
    api("gui_notify", title, body)
  end,
  --- The system's file dialogs (gui.dialogs): opts = { title, dir,
  --- filters = { { "Images", { "png", "jpg" } } }, multiple (open), name
  --- (save) }; then(path) gets the path, a list with `multiple`, or nil
  --- when cancelled.
  open_file = function(opts, then_)
    api("gui_open_file", opts or {}, then_)
  end,
  save_file = function(opts, then_)
    api("gui_save_file", opts or {}, then_)
  end,
  --- The GUI's library, separate from the TUI's (/gui): info() →
  --- { active, builtin = { names }, custom = { names with a gui/ folder } };
  --- use(name) switches to `standard`, `bare`, a built-in one (`desktop`)
  --- or a custom one and loads the GUI's Lua again, going back if it does
  --- not load.
  library = {
    info = function()
      return api("gui_library_info")
    end,
    use = function(name)
      api("gui_library_use", name)
    end,
  },
}

--- Windows and panels drawn as nodes (gui.window_nodes). A spec for
--- bone.ui.popup, bone.ui.win, bone.ui.update, bone.ui.panel.open or
--- bone.ui.panel.update may give
---   node = function(ctx) return node end
--- with ctx = { width, height } (pixels inside the card), in place of
--- `lines`. Keys, focus and closing stay the window's or panel's; a node
--- with `on_click` is clicked as anywhere else. bone.nodes.popups.list
--- makes a scrolling list that keeps a selected entry in view.
--- panel_node(id): the function panel `id` draws with, or nil.
bone.gui.panel_node = function(id)
  return api("gui_panel_node", id)
end

-- What a window with no working view shows: the error, in a plain node
-- tree, so a broken ~/.bone/gui.lua can be read and fixed.
bone.gui.fallback(function(ctx)
  return {
    bg = "#161616", padding = 28, direction = "column", gap = 10,
    { text = "bone has no working view", size = 20, bold = true, color = "#ffffff" },
    { text = ctx.error or "", color = "#ffb0b7", size = 14, selectable = true, font = "mono" },
    { text = "Fix the file named above and save it: the window reloads when you come back to it. /gui use desktop goes back to the built-in one.",
      color = "#9a9a9a", size = 13 },
  }
end)

-- Slots: named places in the window where plugins add nodes. The window
-- (bone.nodes.window) asks for `statusbar.left`, `statusbar.right`,
-- `composer.above`, `composer.below`, `chat.top`, `sidebar.top`,
-- `sidebar.bottom`, `window.left`, `window.right` and `window.overlay`;
-- anything else is yours to ask for with bone.gui.slots.nodes(name).
--   bone.gui.slots.add("statusbar.right", { id = "clock", order = 10,
--     node = function(ctx) return { text = os.date("%H:%M"), hl = "Dim", size = 12 } end })
-- `node` is a node or a function returning one (or nil for none); it runs
-- each time the window is built. Lower `order` comes first.
local slots, seq = {}, 0
bone.gui.slots = {
  add = function(name, spec)
    assert(type(spec) == "table" and spec.node ~= nil, "a slot needs a node")
    local list = slots[name] or {}
    slots[name] = list
    seq = seq + 1
    spec.id = spec.id or ("slot" .. seq)
    for i, s in ipairs(list) do
      if s.id == spec.id then table.remove(list, i) break end
    end
    spec.seq = seq
    list[#list + 1] = spec
    table.sort(list, function(a, b)
      if (a.order or 0) ~= (b.order or 0) then return (a.order or 0) < (b.order or 0) end
      return a.seq < b.seq
    end)
    bone.gui.invalidate()
    return spec.id
  end,
  remove = function(name, id)
    for i, s in ipairs(slots[name] or {}) do
      if s.id == id then table.remove(slots[name], i) bone.gui.invalidate() return true end
    end
    return false
  end,
  --- The nodes in slot `name`, in order: what the window puts there.
  nodes = function(name, ctx)
    local out = {}
    for _, s in ipairs(slots[name] or {}) do
      local node = s.node
      if type(node) == "function" then
        local ok, r = pcall(node, ctx or {})
        if not ok then
          if not s.failed then bone.notify("slot " .. name .. "/" .. s.id .. ": " .. tostring(r), "error") end
          s.failed = true
          r = nil
        else
          s.failed = nil
        end
        node = r
      end
      if type(node) == "table" then out[#out + 1] = node end
    end
    return out
  end,
  list = function(name)
    local out = {}
    for _, s in ipairs(slots[name] or {}) do out[#out + 1] = s.id end
    return out
  end,
}

-- Shapes for a canvas node: `canvas = { draw = function(ctx) return { ... } end, fps = 0 }`.
-- Coordinates are pixels from the box's top left; colors are hex or a
-- highlight group.
--   bone.gui.draw.rect{ x, y, w, h, fill, stroke, width, radius }
--   bone.gui.draw.circle{ x, y, r, fill, stroke, width }
--   bone.gui.draw.line{ x1, y1, x2, y2, color, width }
--   bone.gui.draw.polyline{ points = { { x, y }, ... }, color, width, closed, fill }
--   bone.gui.draw.text{ x, y, text, size, color, align = "left"|"center"|"right",
--     baseline = "top"|"middle", font = "sans"|"mono" }
--   bone.gui.draw.image{ x, y, w, h, id | path, radius }
bone.gui.draw = setmetatable({}, {
  __index = function(_, op)
    return function(shape)
      shape.op = op
      return shape
    end
  end,
})
