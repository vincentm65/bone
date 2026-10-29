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
--   text      { text, tone, strong }
--   row       { ... }  side by side (text, badge, button, spinner, meter, hints)
--   column    { ... }  one under another
--   section   { title, ... }  a titled group
--   list      { items = { { text, detail, tone, on, on_press } }, on_press(i, item), empty }
--   checklist { items = { { text, state = "todo" | "active" | "done", on_press } }, on_press(i, item) }
--   button    { label, on_press, primary }
--   tabs      { names, current, on_select(i) }
--   field     { label, value, hint, empty, editing, on_press }
--   diff      { rows = { { sign = "+" | "-" | " ", text, old, new } | { separator = true }
--                        | { text, tone } }, numbers, cap, expanded }
--   code      { text }
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
-- a target: a click presses it (in a terminal panel too), and in a focused
-- window or panel of either frontend up/down move a cursor through the
-- targets, left/right change tabs, and enter or space press the one it is
-- on. Up from the first target and down from the last go on to the panel,
-- which scrolls; so do the page keys.
local M = {}

--- The kinds, and those that sit in a row's line.
M.kinds = {
  text = true, row = true, column = true, section = true, list = true, checklist = true,
  button = true, tabs = true, field = true, diff = true, code = true, badge = true,
  meter = true, spinner = true, divider = true, hints = true,
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
--- runs (or nil), and `hit(line, n, from, to)` where a terminal renderer
--- drew target `n` (columns `from` to `to`, all of the line by default).
function M.focus(tree, st, focused)
  local list, index = {}, {}
  local function add(el, i, fn, tab)
    list[#list + 1] = { el = el, i = i, fn = fn, tab = tab }
    index[el] = index[el] or {}
    index[el][i or 0] = #list
  end
  local function walk(el)
    if type(el) ~= "table" then return end
    local k = el.kind
    if k == "button" or k == "field" then
      if el.on_press then add(el, nil, el.on_press) end
    elseif k == "list" or k == "checklist" then
      for i, it in ipairs(el.items or {}) do
        if it.on_press then
          add(el, i, it.on_press)
        elseif el.on_press then
          add(el, i, function() el.on_press(i, it) end)
        end
      end
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
  function F.press(el, i)
    local n = F.n(el, i)
    if not n then return nil end
    return function()
      st.cursor = n
      list[n].fn()
    end
  end
  return F
end

--- A key for the view whose state is `st`; true when it was the cursor's.
--- A view with no targets leaves every key (the panel scrolls).
function M.key(st, key)
  local list, n = st.targets or {}, st.cursor
  if #list == 0 or not n then return false end
  local t = list[n]
  if key == "up" or key == "down" then
    local next = n + (key == "up" and -1 or 1)
    if next < 1 or next > #list then return false end
    st.cursor, st.moved = next, true
    return true
  end
  if (key == "left" or key == "right") and t.tab then
    local names = t.el.names or {}
    local i = math.max(1, math.min(#names, (t.el.current or t.i) + (key == "left" and -1 or 1)))
    if i ~= t.el.current then
      t.el.on_select(i)
      st.cursor = n - t.i + i
    end
    return true
  end
  if key == "enter" or key == "space" then
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

--- A press of the left button over a terminal panel drawn from a view
--- presses the target under it; true when it did.
function M.mouse(ev)
  if ev.button ~= "left" or ev.action ~= "down" or not ev.panel then return false end
  local st = states["panel:" .. ev.panel]
  local hits = st and st.hits and st.hits[ev.panel_line]
  local info = hits and bone.ui.panel.get(ev.panel)
  info = info and info:info()
  -- The column in the panel's content, from 1.
  local col = info and info.col and ev.x and ev.x - info.col + 1 or 1
  for _, h in ipairs(hits or {}) do
    if col >= h[1] and col <= h[2] and st.targets[h[3]] then
      if info.focusable ~= false then bone.ui.panel.focus(ev.panel) end
      st.cursor = h[3]
      st.targets[h[3]].fn()
      return true
    end
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

-- The page keys scroll a desktop view (a terminal panel scrolls itself).
local function page(st, key)
  local id = "elements:" .. tostring(st.key)
  local info = st.key and bone.gui.scroll_info(id)
  if not info then return false end
  if key == "home" or key == "end" then
    bone.gui.scroll(id, key == "home" and "top" or "end")
    return true
  end
  local dy = (info.viewport or 240) * 0.9
  bone.gui.scroll(id, math.max(0, (info.offset or 0) + (key == "pageup" and -dy or dy)))
  return true
end
local PAGE = { pageup = true, pagedown = true, home = true, ["end"] = true }

local function hook_keys(spec, st)
  st.on_key = spec.on_key or st.on_key
  spec.on_key = function(key, ...)
    if M.key(st, key) then return true end
    if st.on_key and st.on_key(key, ...) then return true end
    if PAGE[key] and gui() then return page(st, key) end
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
  spec.render = function(ctx)
    local tree, moved = build(st, ctx), st.moved
    st.moved = nil
    local c = st.cache
    if c and c.tree == tree and c.width == ctx.width and c.cursor == st.cursor and c.focused == ctx.focused
      and c.spinner == ctx.spinner and not moved then
      return c.lines
    end
    local lines, F = M.lines(tree, ctx.width or 0, st, ctx.focused, ctx.spinner)
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

--- The same for a window: `lines` (in a box, with `title`) and `node`.
function M.win_spec(spec, st)
  st = st or {}
  st.view = spec.view
  st.title = spec.title or st.title
  spec.view, spec.title = nil, nil
  local focused = spec.focus ~= false
  local width = spec.width
  spec.lines = function(ctx)
    local w = math.min(width or 64, ctx.width or 64)
    local lines = M.lines(build(st, ctx), math.max(w - 4, 1), st, focused)
    return bone.ui.box(lines, { title = st.title, width = w })
  end
  if gui() then
    spec.node = function(ctx)
      local node = M.node(build(st, ctx, true), ctx, st, focused)
      if not st.title then return node end
      local widgets = require("bone.nodes.widgets")
      return { direction = "column", widgets.title(st.title), node }
    end
  end
  hook_keys(spec, st)
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
