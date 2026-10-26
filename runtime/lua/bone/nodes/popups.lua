-- Lua's windows (bone.ui.popup, bone.ui.win) drawn as nodes. A window of
-- `lines` is placed like the TUI's (the same anchors, sized in character
-- cells, in pixels). A window with a `node` function gets the anchor
-- (`bounds`) and is centered in it, sized to the content box, capped.
-- What they look like is this file: the card, its border, corners, shadow
-- and padding in `M.chrome`, the text of a window of `lines` in the
-- monospace font, and a window with a `node` function (settings, help,
-- health, setup, catalog, the picker and the pager) as what it returns. Keys and
-- focus are the shared ones. Replace this file, or change M.chrome, to look
-- different. The pieces a window of nodes is built from (`title`, `note`,
-- `entry`, `button`, `field`, `hints`, `list`) are bone.nodes.widgets', here
-- under the names this module has always had.
local M = {}

local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")

--- The look of every window.
M.chrome = {
  bg = "Panel",
  border = 1,
  border_color = "PromptBorder",
  radius = style.radius.xl,
  shadow = style.shadow.card,
  -- Room around the content of a window of nodes.
  pad = 14,
  z = 20,
}

for _, name in ipairs({ "title", "note", "entry", "button", "field", "hints", "list" }) do
  M[name] = widgets[name]
end

local function card(p, extra)
  local c = M.chrome
  local node = {
    position = "absolute", left = p.x, top = p.y, width = p.w, height = p.h, z = c.z + p.z,
    bg = c.bg, border = c.border, border_color = c.border_color, radius = c.radius,
    shadow = c.shadow, overflow = "hidden", block_input = true,
  }
  -- A plain look (bone.nodes.style.plain) is a flat square card.
  if style.plain then
    node.radius = 0
    node.shadow = nil
    node.border = 0
  end
  for k, v in pairs(extra) do node[k] = v end
  return node
end

-- A window of `lines` (a plugin's, written for the TUI): one row of
-- monospace text to a cell row. Its keys are the shared ones; there is no
-- pointer in it, a window that wants one is made of nodes.
local function lines_window(p, cell)
  local size = bone.o.font_size or 14
  local node = card(p, { direction = "column" })
  for _, spans in ipairs(p.lines) do
    local row = {}
    for i, s in ipairs(spans) do
      -- A group's background is a chip behind its text (a selected row),
      -- except Normal's, which is the window's.
      row[i] = { text = s.text, hl = s.hl, bg = s.hl ~= "Normal" and s.hl or nil }
    end
    node[#node + 1] = {
      height = cell.h, line_height = cell.h, font = "mono", size = size, wrap = false,
      spans = #row > 0 and row or { " " },
    }
  end
  return node
end

-- The content box of a node window is at most this, and at most the
-- anchor minus the card's padding.
local CONTENT_W, CONTENT_H = 880, 680

-- A window of nodes: its `node` function's, in the card's padding.
-- `bounds`: x/y/w/h are the anchor, not the card.
local function node_window(p)
  local c = M.chrome
  local inner_w, inner_h
  if p.bounds then
    inner_w = math.max(0, math.min(p.w - 2 * c.pad, CONTENT_W))
    inner_h = math.max(0, math.min(p.h - 2 * c.pad, CONTENT_H))
  else
    inner_w = p.w - 2 * c.pad
    inner_h = p.h - 2 * c.pad
  end
  local ok, content = pcall(p.node, { width = inner_w, height = inner_h })
  if not ok then
    content = { text = tostring(content), hl = "ErrorMsg", selectable = true }
  end
  local inner = content and { grow = 1, direction = "column", content } or nil
  if p.bounds then
    local card_w = math.min(p.w, inner_w + 2 * c.pad)
    local card_h = math.min(p.h, inner_h + 2 * c.pad)
    local box = card({ x = 0, y = 0, w = card_w, h = card_h, z = 0 },
      { direction = "column", padding = c.pad, inner })
    -- Centered in the anchor. The size is already inside it; a `fit`
    -- window is only as tall as it holds, still no bigger than this box.
    box.position, box.left, box.top, box.z = nil, nil, nil, nil
    if content and content.fit then
      box.height = nil
      box.width = card_w
      box.max_width = card_w
      box.max_height = card_h
    end
    return {
      position = "absolute", left = p.x, top = p.y, width = p.w, height = p.h, z = c.z + p.z,
      direction = "column", justify = "center", align = "center", box,
    }
  end
  local box = card(p, { direction = "column", padding = c.pad, inner })
  if not (content and content.fit) then return box end
  -- Sized to what it holds: the card, no bigger than the room the window
  -- was given, centered in a box of that size.
  box.position, box.left, box.top, box.width, box.height, box.z = nil, nil, nil, nil, nil, nil
  box.max_height = p.h
  return {
    position = "absolute", left = p.x, top = p.y, width = p.w, height = p.h, z = c.z + p.z,
    direction = "column", justify = "center", box,
  }
end

--- A window of choices, one entry each: a heading, a line under it, the
--- entries (a label, what it is, a badge) and the keys. spec: `title`,
--- `note`, `items` (a table, or a function that returns one, of `{ label,
--- detail, badge, active }`), `selected` (index), `choose(i)`, `hints`.
--- Returns the window's id. The card is centered in the anchor and sized
--- to the content box (capped).
function M.menu(spec)
  local selected = spec.selected or 1
  local id
  local function items()
    return type(spec.items) == "function" and spec.items() or spec.items
  end
  local function draw()
    local list = items()
    local rows = { M.title(spec.title) }
    if spec.note then rows[#rows + 1] = M.note(spec.note, { 0, 12 }) end
    local at
    for i, it in ipairs(list) do
      local heading = {
        direction = "row", justify = "between", align = "center",
        { text = it.label, size = style.size.lead, bold = i == selected, hl = "Normal" },
        it.badge and widgets.pill(it.badge) or nil,
      }
      local row = M.entry(i == selected, {
        heading,
        it.detail and { text = it.detail, size = style.size.body, hl = "Dim", margin = { 5, 0, 0, 0 } } or nil,
      }, function() spec.choose(i) end)
      row.padding = { 13, 18 }
      rows[#rows + 1] = row
      if i == selected then at = #rows end
    end
    if #list == 0 then rows[#rows + 1] = M.note(spec.empty or "Nothing here.", { 8, 0 }) end
    return M.list("menu:" .. id, rows, at,
      { footer = spec.hints and M.hints(spec.hints, { 10, 0 }) or nil })
  end
  id = bone.ui.popup({
    node = draw,
    on_key = function(k)
      local n = #items()
      if k == "esc" or k == "q" then bone.ui.close(id)
      elseif n == 0 then return false
      elseif k == "up" or k == "k" then selected = (selected - 2) % n + 1 bone.ui.update(id, {})
      elseif k == "down" or k == "j" then selected = selected % n + 1 bone.ui.update(id, {})
      elseif k == "enter" then spec.choose(selected)
      else return false end
      return true
    end,
  })
  return id
end

--- The windows now open, as nodes (bottom first).
function M.nodes()
  local out = {}
  local placed = bone.gui.popups()
  for _, p in ipairs(placed) do
    if p.node then
      out[#out + 1] = node_window(p)
    elseif #p.lines > 0 then
      out[#out + 1] = lines_window(p, placed.cell)
    end
  end
  return out
end

return M
