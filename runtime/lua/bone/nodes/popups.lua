-- Lua's windows (bone.ui.popup, bone.ui.win) drawn as nodes. Their place
-- and size come from the shared placement (the same anchors as the TUI's,
-- sized in character cells, placed in pixels); what they look like is this
-- file: the card, its border, corners, shadow and padding in `M.chrome`,
-- the text of a window of `lines` in the monospace font, and a window with
-- a `node` function (settings, help, health, setup, catalog, the picker and
-- the pager) as what it returns. Keys and
-- focus are the shared ones. Replace this file, or change M.chrome, to look
-- different.
local M = {}

--- The look of every window.
M.chrome = {
  bg = "Panel",
  border = 1,
  border_color = "PromptBorder",
  radius = 12,
  shadow = { blur = 22, dy = 8, color = "#000000a0" },
  -- Room around the content of a window of nodes.
  pad = 14,
  z = 20,
}

local function card(p, extra)
  local c = M.chrome
  local node = {
    position = "absolute", left = p.x, top = p.y, width = p.w, height = p.h, z = c.z + p.z,
    bg = c.bg, border = c.border, border_color = c.border_color, radius = c.radius,
    shadow = c.shadow, overflow = "hidden", block_input = true,
  }
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

-- A window of nodes: its `node` function's, in the card's padding.
local function node_window(p)
  local c = M.chrome
  local ok, content = pcall(p.node, { width = p.w - 2 * c.pad, height = p.h - 2 * c.pad })
  if not ok then
    content = { text = tostring(content), hl = "ErrorMsg", selectable = true }
  end
  local inner = content and { grow = 1, direction = "column", content } or nil
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

--- A window's heading.
function M.title(text)
  return { text = text, hl = "Normal", bold = true, size = 20, padding = { 4, 10 }, margin = { 0, 0, 6, 0 } }
end

--- A line of quiet text; `margin` is { above, below }.
function M.note(text, margin, hl)
  local m = margin or { 10, 0 }
  return { text = text, hl = hl or "Dim", size = 12, padding = { 4, 10 }, margin = { m[1], 0, m[2], 0 } }
end

--- An entry: a card, raised when it is the selected one; `click` runs when
--- it is pressed.
function M.entry(on, children, click)
  local node = {
    direction = "column", bg = on and "Selection" or nil, radius = 10, padding = { 7, 12 },
    margin = { 1, 0, 1, 0 }, cursor = click and "pointer" or nil, on_click = click,
    role = "option", selected = on, transition = 100,
    hover = click and not on and { bg = "#ffffff0a" } or nil,
  }
  for _, c in ipairs(children) do node[#node + 1] = c end
  return node
end

--- A button: raised, or `primary` (the accent's tint); `on` rings it, for
--- the one the keyboard is on.
function M.button(label, click, primary, on)
  return {
    text = label, size = 14, bold = primary, hl = primary and "Accent" or "Normal",
    bg = primary and "TextSelection" or "Selection", radius = 10, padding = { 8, 18 },
    border = 1, border_color = on and "Accent" or "transparent",
    cursor = "pointer", on_click = click, role = "button", transition = 120,
    hover = { opacity = 0.8 },
  }
end

--- A form field: `label` over a box holding `value` (`editing` adds the
--- cursor, `on` rings it), and `hint` under it while it is the one on.
function M.field(label, value, o)
  o = o or {}
  local shown = o.editing and (value .. "▏") or (value ~= "" and value or (o.empty or ""))
  local box = {
    text = shown, size = 14, hl = (value == "" and not o.editing) and "Dim" or "Normal",
    bg = "InputBackground", radius = 10, padding = { 6, 12 },
    border = 1, border_color = o.on and "Accent" or "PromptBorder", transition = 100,
  }
  return {
    direction = "column", margin = { 0, 0, 9, 0 }, cursor = "pointer", on_click = o.click,
    { text = label, size = 12, hl = o.on and "Accent" or "Dim", margin = { 0, 0, 3, 2 } },
    box,
    o.on and o.hint and { text = o.hint, size = 12, hl = "Dim", margin = { 4, 0, 0, 2 } } or nil,
  }
end

-- What reads as a key in a hint ("enter open"), and what is plain text.
local KEYS = {
  enter = 1, esc = 1, space = 1, tab = 1, ["↑↓"] = 1, y = 1, n = 1, a = 1, u = 1, x = 1, r = 1, q = 1,
}

--- A row of key hints from a line such as "↑↓ move · enter open · esc
--- close": each key a small keycap, then what it does.
function M.hints(text, margin)
  local m = margin or { 14, 0 }
  local row = {
    direction = "row", flex_wrap = true, gap = 16, row_gap = 6, align = "center",
    padding = { 4, 10 }, margin = { m[1], 0, m[2], 0 },
  }
  for part in (text .. " · "):gmatch("(.-) · ") do
    local key, what = part:match("^(%S+)%s+(.*)$")
    if key and KEYS[key] then
      row[#row + 1] = {
        direction = "row", gap = 6, align = "center",
        { text = key, size = 11, font = "mono", bg = "Selection", radius = 5, padding = { 2, 7 } },
        { text = what, size = 12, hl = "Dim" },
      }
    elseif part ~= "" then
      row[#row + 1] = { text = part, size = 12, hl = "Dim" }
    end
  end
  return row
end

local shown = {} -- list id -> the entry last scrolled into view

--- A scrolling list of `items` (nodes) for a window or panel, entry
--- `selected` (an index) kept in view; `id` names it. `opts.fit`: the
--- window is as tall as its entries, not the room it was given.
--- `opts.footer`: a node that stays under the list (the keys), however far
--- it scrolls.
function M.list(id, items, selected, opts)
  opts = opts or {}
  if selected and shown[id] ~= selected then
    -- Until the list has been drawn there is nothing to scroll.
    if bone.gui.scroll_item(id, selected, "nearest") then shown[id] = selected end
  end
  local list = {
    id = id, grow = 1, fit = opts.fit,
    list = { count = #items, estimate = 56, key = id, item = function(i) return items[i] end, fit = opts.fit },
  }
  if not opts.footer then return list end
  opts.footer.shrink = 0
  return { grow = 1, direction = "column", fit = opts.fit, list, opts.footer }
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
