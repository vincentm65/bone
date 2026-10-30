-- Elements: what a plugin shows, as plain data, for any frontend. A window
-- or panel given `view = function(ctx) return element end` in place of
-- `lines`, `render` and `node` is drawn by the library of each frontend:
-- bone.elements.lines in the terminal, bone.elements.nodes in the desktop
-- app (replace either in a library to draw them differently). Elements say
-- what something is, never how it looks: no colors, sizes or spaces, only a
-- `tone` (a role of the colorscheme) and states.
--
-- An element is a table with `kind`; containers hold their children in the
-- array part, like nodes (a nil among them is skipped):
--
--   text      { text, tone, strong, truncate }  truncate: one line, cut with …
--   row       { ... }  side by side (text, badge, button, spinner, meter, hints)
--   column    { ... }  one under another
--   section   { title, ... }  a titled group
--   split     { side, main }  two columns: a narrow one beside the rest
--   list      { items = { { text, detail, tone, on, depth, badges = { { text, tone } }, on_press } },
--               on_press(i, item), selected, on_select(i, item), on_key(key, i, item), empty }
--   checklist { items = { { text, state = "todo" | "active" | "done", on_press } }, on_press(i, item) }
--   button    { label, on_press, primary, on_key(key) }
--   tabs      { names, current, on_select(i) }
--   field     { label, value, hint, empty, editing, on_press, on_key(key) }
--   diff      { rows = { { sign = "+" | "-" | " ", text, old, new } | { separator = true }
--                        | { text, tone } }, numbers, cap, expanded }
--   code      { text }
--   markdown  { text }
--   badge     { text, tone }
--   meter     { value (0 to 1), label, tone }
--   spinner   {}
--   divider   {}
--   hints     { text = "↑↓ move · enter open" }
--
-- A view is called each time the frontend draws (every frame in the
-- terminal); one that returns the same table as last time reuses what was
-- drawn from it, so a large view can build its tree only when it changes.
--
-- Tones are normal, dim, accent, success, warning and error. Whatever has an
-- `on_press` (a button, a field, a list or checklist entry) and each tab is
-- a target: a click presses it, and in a focused window or panel of either
-- frontend up/down move a cursor through the targets, left/right change
-- tabs, and enter or space press the one it is on. Up from the first target
-- and down from the last go on to the panel or window, which scrolls; so do
-- the page keys. j and k move like down and up, unless a field is being
-- edited: then every key is the plugin's.
--
-- A list with `selected` and `on_select` keeps the selection its plugin
-- owns: the cursor starts on it and follows it, moving through the list
-- calls `on_select`, and the selected entry stays marked while the cursor
-- is elsewhere (on a button under it). A click selects an entry, and a
-- click on the selected one presses it. `on_key` gets the other keys while
-- the cursor is on an entry (return true when handled); a field and a
-- button take one too.
local M = {}

--- The kinds, and those that sit in a row's line.
M.kinds = {
  text = true, row = true, column = true, section = true, split = true, list = true,
  checklist = true, button = true, tabs = true, field = true, diff = true, code = true,
  markdown = true, badge = true, meter = true, spinner = true, divider = true, hints = true,
}
M.inline = { text = true, badge = true, button = true, spinner = true, meter = true, hints = true }

--- A tone's highlight group: the roles every colorscheme defines.
M.tones = {
  normal = "Normal", dim = "Dim", accent = "Accent", success = "Success",
  warning = "WarningMsg", error = "ErrorMsg",
}
function M.group(tone, fallback)
  return M.tones[tone] or fallback or "Normal"
end

--- The children of `el`, in order, skipping any nil among them.
function M.children(el)
  local out = {}
  for i = 1, table.maxn(el) do
    if el[i] ~= nil then out[#out + 1] = el[i] end
  end
  return out
end

--- Whether list `el` keeps a selection of its plugin's.
function M.controlled(el)
  return el.kind == "list" and el.selected ~= nil and el.on_select ~= nil
end

-- The groups the renderers draw states with, under the colorscheme so a
-- scheme can give them its own look.
bone.hl.set("ElementStrong", { link = "Normal", bold = true, default = true })
bone.hl.set("ElementDone", { link = "Dim", strikethrough = true, default = true })

local states = {} -- "panel:<id>" or "win:<id>" -> the view's state

-- ---- targets and the cursor -------------------------------------------------

--- The targets of `tree` in reading order, the same in every renderer, and
--- what the renderers ask of them: `at(el, i)` whether the cursor is on
--- entry `i` of `el` (nil for the element itself) while the view has the
--- keyboard, `n(el, i)` its number, `press(el, i)` the function a click
--- runs (or nil), `id(n)` a node id for target `n` (desktop), and
--- `hit(line, n, from, to)` where a terminal renderer drew target `n`
--- (columns `from` to `to`, all of the line by default).
function M.focus(tree, st, focused)
  local list, index, owned = {}, {}, {}
  local function add(el, i, fn, tab)
    list[#list + 1] = { el = el, i = i, fn = fn, tab = tab }
    index[el] = index[el] or {}
    index[el][i or 0] = #list
  end
  st.editing = false
  local function walk(el)
    if type(el) ~= "table" then return end
    local k = el.kind
    if k == "field" and el.editing then st.editing = true end
    if k == "button" or k == "field" then
      if el.on_press then add(el, nil, el.on_press) end
    elseif k == "list" or k == "checklist" then
      local controlled, first = M.controlled(el), #list + 1
      for i, it in ipairs(el.items or {}) do
        local fn = it.on_press or (el.on_press and function() el.on_press(i, it) end)
        if fn or controlled then add(el, i, fn) end
      end
      if controlled and #list >= first then owned[#owned + 1] = { el = el, first = first, last = #list } end
    elseif k == "tabs" then
      for i in ipairs(el.names or {}) do
        if el.on_select then add(el, i, function() el.on_select(i) end, true) end
      end
    else
      for _, child in ipairs(M.children(el)) do walk(child) end
    end
  end
  walk(tree)
  st.targets = list
  -- The cursor starts on, and follows, a selection its plugin keeps.
  for _, o in ipairs(owned) do
    local at = o.first + math.max(0, math.min(o.el.selected - 1, o.last - o.first))
    if st.cursor == nil or (st.cursor >= o.first and st.cursor <= o.last) then st.cursor = at end
  end
  if #list == 0 then
    st.cursor = nil
  else
    st.cursor = math.max(1, math.min(st.cursor or 1, #list))
  end
  local F = { focused = focused, line = nil, hits = {}, scroll_id = st.key and ("elements:" .. st.key) }
  st.hits = F.hits
  function F.hit(line, n, from, to)
    if not n then return end
    local h = F.hits[line] or {}
    h[#h + 1] = { from or 1, to or math.huge, n }
    F.hits[line] = h
    if n == st.cursor and not F.line then F.line = line end
  end
  function F.n(el, i)
    return index[el] and index[el][i or 0]
  end
  function F.at(el, i)
    return focused and st.cursor ~= nil and F.n(el, i) == st.cursor
  end
  function F.id(n)
    return n and F.scroll_id and (F.scroll_id .. ":" .. n) or nil
  end
  function F.press(el, i)
    local n = F.n(el, i)
    if not n then return nil end
    local t = list[n]
    -- A click selects an entry its plugin keeps a selection for, and
    -- presses the one already selected.
    if t.i and M.controlled(t.el) and t.i ~= t.el.selected then
      return function()
        st.cursor = n
        t.el.on_select(t.i, (t.el.items or {})[t.i])
      end
    end
    if not t.fn then return nil end
    return function()
      st.cursor = n
      t.fn()
    end
  end
  return F
end

--- A key for the view whose state is `st`; true when it was the cursor's.
--- A view with no targets leaves every key (the panel scrolls), and so does
--- one with a field being edited (the keys type into it). j and k move too.
function M.key(st, key)
  local list, n = st.targets or {}, st.cursor
  if #list == 0 or not n or st.editing then return false end
  key = key == "j" and "down" or key == "k" and "up" or key
  local t = list[n]
  if key == "up" or key == "down" then
    local next = n + (key == "up" and -1 or 1)
    if next < 1 or next > #list then return false end
    st.cursor, st.moved = next, true
    local to = list[next]
    if to.i and M.controlled(to.el) and to.i ~= to.el.selected then
      to.el.on_select(to.i, (to.el.items or {})[to.i])
    end
    return true
  end
  if t.el.on_key and t.el.on_key(key, t.i, t.i and (t.el.items or {})[t.i] or nil) then return true end
  if (key == "left" or key == "right") and t.tab then
    local names = t.el.names or {}
    local i = math.max(1, math.min(#names, (t.el.current or t.i) + (key == "left" and -1 or 1)))
    if i ~= t.el.current then
      t.el.on_select(i)
      st.cursor = n - t.i + i
    end
    return true
  end
  if (key == "enter" or key == "space") and t.fn then
    t.fn()
    return true
  end
  return false
end

-- ---- drawing ----------------------------------------------------------------

local function view_ctx(ctx, gui)
  local cell = gui and (ctx.cell or (bone.gui.window_info() or {}).cell) or nil
  local w, h = ctx.width or 0, ctx.height or 0
  return {
    focused = ctx.focused,
    columns = cell and math.floor(w / math.max(cell.w or 8, 1)) or w,
    rows = cell and math.floor(h / math.max(cell.h or 18, 1)) or h,
  }
end

local function build(st, ctx, gui)
  local ok, tree = pcall(st.view, view_ctx(ctx, gui))
  if not ok then
    return { kind = "text", text = tostring(tree), tone = "error" }
  end
  return tree
end

-- Press the target `st` drew at content line `line`, column `col`.
local function hit(st, line, col)
  for _, h in ipairs(st and st.hits and st.hits[line] or {}) do
    if col >= h[1] and col <= h[2] and st.targets[h[3]] then
      local t = st.targets[h[3]]
      local press
      if t.i and M.controlled(t.el) and t.i ~= t.el.selected then
        press = function() t.el.on_select(t.i, (t.el.items or {})[t.i]) end
      else
        press = t.fn
      end
      st.cursor = h[3]
      if press then press() end
      return true
    end
  end
  return false
end

--- A press of the left button over a terminal panel or window drawn from a
--- view presses the target under it; true when it did.
function M.mouse(ev)
  if ev.button ~= "left" or ev.action ~= "down" then return false end
  if ev.popup then
    -- In the window's box: a border row above, a border and a space left.
    local st = states["win:" .. tostring(ev.popup)]
    if not st or not ev.popup_row then return false end
    return hit(st, ev.popup_row - 1 + (st.top or 0), (ev.popup_col or 1) - 2)
  end
  if not ev.panel then return false end
  local st = states["panel:" .. ev.panel]
  local hits = st and st.hits and st.hits[ev.panel_line]
  local info = hits and bone.ui.panel.get(ev.panel)
  info = info and info:info()
  if not info then return false end
  -- The column in the panel's content, from 1.
  local col = info.col and ev.x and ev.x - info.col + 1 or 1
  local panel = ev.panel
  if hit(st, ev.panel_line, col) then
    if info.focusable ~= false then bone.ui.panel.focus(panel) end
    return true
  end
  return false
end

--- `tree` as terminal lines, `width` cells wide.
function M.lines(tree, width, st, focused, spinner)
  local F = M.focus(tree, st, focused)
  F.spinner = spinner
  return require("bone.elements.lines").render(tree, width, F), F
end

--- `tree` as a desktop node.
function M.node(tree, ctx, st, focused)
  local F = M.focus(tree, st, focused)
  return require("bone.elements.nodes").render(tree, ctx, F)
end

-- ---- windows and panels -----------------------------------------------------

local function gui()
  return bone.has_capability("gui.window_nodes")
end

-- Desktop: the page keys scroll a view, and the cursor is kept in view.
local function scroller(st)
  local id = "elements:" .. tostring(st.key)
  return id, st.key and bone.gui.scroll_info(id)
end

local STEP = { up = -40, down = 40, wheelup = -60, wheeldown = 60 }

local function page(st, key)
  local id, info = scroller(st)
  if not info then return false end
  if key == "home" or key == "end" then
    bone.gui.scroll(id, key == "home" and "top" or "end")
    return true
  end
  local dy = STEP[key] or (info.viewport or 240) * 0.9 * (key == "pageup" and -1 or 1)
  bone.gui.scroll(id, math.max(0, (info.offset or 0) + dy))
  return true
end

local function reveal(st)
  local id, info = scroller(st)
  local r, c = bone.gui.rect(id .. ":" .. tostring(st.cursor)), bone.gui.rect(id)
  if not (info and r and c) then return end
  local offset, pad = info.offset or 0, 8
  if r.y < c.y then
    bone.gui.scroll(id, math.max(0, offset - (c.y - r.y) - pad))
  elseif r.y + r.h > c.y + c.h then
    bone.gui.scroll(id, offset + (r.y + r.h - c.y - c.h) + pad)
  end
end

-- Terminal windows: the rows past the window's height scroll.
local function scroll_window(st, key)
  local room, total = st.rows or 0, st.total or 0
  if total <= room then return false end
  local by = ({ up = -1, down = 1, wheelup = -3, wheeldown = 3, pageup = -room, pagedown = room })[key]
  if key == "home" then by = -total elseif key == "end" then by = total end
  if not by then return false end
  st.top = math.max(0, math.min(total - room, (st.top or 0) + by))
  return true
end

local PAGE = { pageup = true, pagedown = true, home = true, ["end"] = true, up = true, down = true }

local function hook_keys(spec, st, window)
  st.on_key = spec.on_key or st.on_key
  spec.on_key = function(key, ...)
    if M.key(st, key) then
      if st.moved and gui() then reveal(st) end
      return true
    end
    if st.on_key and st.on_key(key, ...) then return true end
    if gui() then
      if PAGE[key] then return page(st, key) end
    elseif window then
      return scroll_window(st, key)
    end
    return false
  end
end

--- A panel spec with `view`: `render` and, in the desktop app, `node` that
--- draw it, and keys for its cursor. `st` is the panel's state, kept across
--- updates; returns it.
function M.panel_spec(spec, st)
  st = st or {}
  st.view = spec.view
  spec.view = nil
  M.drawer_title(spec, st)
  spec.render = function(ctx)
    local tree, moved = build(st, ctx), st.moved
    st.moved = nil
    local c = st.cache
    if c and c.tree == tree and c.width == ctx.width and c.cursor == st.cursor and c.focused == ctx.focused
      and c.spinner == ctx.spinner and not moved then
      return c.lines
    end
    local lines, F = M.lines(tree, ctx.width or 0, st, ctx.focused, ctx.spinner)
    if st.drawer and type(st.title) == "string" then
      -- Over the content: a row off the chat, then the title.
      local hl = ctx.focused and "PanelTitleFocus" or "PanelTitle"
      local head = { {}, { { st.title, hl }, { fill = " ", hl = hl } } }
      for i, l in ipairs(lines) do head[i + 2] = l end
      local hits = {}
      for line, h in pairs(st.hits or {}) do hits[line + 2] = h end
      lines, st.hits, F.line = head, hits, F.line and F.line + 2
    end
    st.cache = { tree = tree, width = ctx.width, cursor = st.cursor, focused = ctx.focused, spinner = ctx.spinner, lines = lines }
    -- Keep the cursor in view once a key has moved it.
    if moved and F.line and ctx.height and ctx.top then
      local id, top, line = ctx.id, ctx.top, F.line
      local delta = line <= top and line - top - 1 or line > top + ctx.height and line - top - ctx.height or 0
      if delta ~= 0 then
        bone.defer(0, function()
          local p = bone.ui.panel.get(id)
          if p then p:scroll(delta) end
        end)
      end
    end
    return lines
  end
  spec.lines = nil
  if gui() then
    spec.node = function(ctx)
      return M.node(build(st, ctx, true), ctx, st, ctx.focused)
    end
  end
  hook_keys(spec, st)
  return st
end

--- A panel docked under the chat in the terminal (a drawer) draws its
--- title itself, a row off the chat; `spec.title` goes to `st.title`.
function M.drawer_title(spec, st)
  if gui() then return end
  st.drawer = (spec.dock or st.dock_side) == "bottom"
  st.dock_side = spec.dock or st.dock_side
  if st.drawer and spec.title ~= nil then
    st.title, spec.title = spec.title, false
    st.cache = nil
  end
end

--- The same for a window: `lines` (in a box, with `title`, scrolling when
--- taller than the room) and `node`.
function M.win_spec(spec, st)
  st = st or {}
  st.view = spec.view
  st.title = spec.title or st.title
  spec.view, spec.title = nil, nil
  local focused = spec.focus ~= false
  local width, height = spec.width, spec.height
  spec.lines = function(ctx)
    local w = math.min(width or 64, ctx.width or 64)
    local all, F = M.lines(build(st, ctx), math.max(w - 4, 1), st, focused)
    local most = math.max(1, (ctx.height or (#all + 2)) - 2)
    local rows = math.max(1, math.min(height and height - 2 or #all, most))
    st.rows, st.total = rows, #all
    if st.moved and F.line then
      local top = st.top or 0
      if F.line <= top then st.top = F.line - 1 elseif F.line > top + rows then st.top = F.line - rows end
    end
    st.moved = nil
    st.top = math.max(0, math.min(st.top or 0, #all - rows))
    local shown = {}
    for i = st.top + 1, math.min(#all, st.top + rows) do shown[#shown + 1] = all[i] end
    while height and #shown < rows do shown[#shown + 1] = {} end
    return bone.ui.box(shown, { title = st.title, width = w })
  end
  if gui() then
    spec.node = function(ctx)
      local node = M.node(build(st, ctx, true), ctx, st, focused)
      if not st.title then return node end
      local widgets = require("bone.nodes.widgets")
      return { direction = "column", grow = 1, widgets.title(st.title), node }
    end
  end
  hook_keys(spec, st, true)
  return st
end

--- Remember the state of window or panel `key` (nil forgets it).
function M.keep(key, st)
  if st then st.key = key end
  states[key] = st
end
function M.state(key)
  return states[key]
end

return M
