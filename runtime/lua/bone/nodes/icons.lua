-- Line icons drawn on a canvas: one stroke weight, on a 16px grid, in a
-- highlight group's color, so they follow the colorscheme and stay sharp at
-- any size.
--
--   local icons = require("bone.nodes.icons")
--   icons.node("search", { hl = "Dim" })        -- 16px
--   icons.node("folder", { size = 13 })
--
-- A shape is { "circle", x, y, r } | { "line", x1, y1, x2, y2 } |
-- { "poly", { x, y, x, y, ... }, closed } | { "rect", x, y, w, h, radius }
-- | { "dot", x, y, r } (filled). Add one with M.define(name, shapes).
local M = {}

local shapes = {
  search = { { "circle", 7, 7, 4.5 }, { "line", 10.5, 10.5, 13.5, 13.5 } },
  plus = { { "line", 8, 3, 8, 13 }, { "line", 3, 8, 13, 8 } },
  close = { { "line", 4.5, 4.5, 11.5, 11.5 }, { "line", 11.5, 4.5, 4.5, 11.5 } },
  compose = { { "poly", { 11.5, 2.5, 13.5, 4.5, 7, 11, 4.5, 11.5, 5, 9 }, true }, { "line", 2.5, 13.5, 13.5, 13.5 } },
  chat = { { "poly", { 3, 4.5, 13, 4.5, 13, 10.5, 8, 10.5, 5, 13, 5, 10.5, 3, 10.5 }, true } },
  terminal = { { "rect", 2.5, 3.5, 11, 9, 1.5 }, { "poly", { 5, 7, 7, 8.5, 5, 10 } }, { "line", 8.5, 10, 11, 10 } },
  layers = { { "poly", { 3, 5.5, 8, 3, 13, 5.5, 8, 8 }, true }, { "poly", { 3, 8, 8, 10.5, 13, 8 } },
    { "poly", { 3, 10.5, 8, 13, 13, 10.5 } } },
  archive = { { "rect", 2.5, 3, 11, 3, 1 }, { "poly", { 3.5, 6, 3.5, 12.5, 12.5, 12.5, 12.5, 6 } }, { "line", 6.5, 9, 9.5, 9 } },
  compress = { { "poly", { 4, 6, 8, 3, 12, 6 } }, { "poly", { 4, 10, 8, 13, 12, 10 } } },
  pulse = { { "poly", { 2.5, 8, 5, 8, 6.5, 4.5, 9.5, 11.5, 11, 8, 13.5, 8 } } },
  model = { { "circle", 8, 8, 2.5 }, { "line", 8, 2, 8, 4 }, { "line", 8, 12, 8, 14 }, { "line", 2, 8, 4, 8 },
    { "line", 12, 8, 14, 8 } },
  question = { { "circle", 8, 8, 5.5 }, { "poly", { 6.3, 6.5, 6.6, 5.6, 7.4, 5.1, 8.6, 5.1, 9.5, 5.7, 9.7, 6.7, 9.1, 7.5, 8, 8.2, 8, 9.3 } },
    { "dot", 8, 11, 0.8 } },
  slash = { { "line", 10, 3, 6, 13 } },
  upload = { { "line", 8, 13, 8, 7 }, { "poly", { 5, 9.5, 8, 6.5, 11, 9.5 } }, { "line", 4, 3.5, 12, 3.5 } },
  sidebar = { { "rect", 2.5, 3, 11, 10, 2 }, { "line", 6.5, 3, 6.5, 13 } },
  folder = { { "poly", { 2.5, 4.5, 6.5, 4.5, 8, 6, 13.5, 6, 13.5, 12.5, 2.5, 12.5 }, true } },
  chevron_right = { { "poly", { 6, 4, 10, 8, 6, 12 } } },
  chevron_down = { { "poly", { 4.5, 6.5, 8, 10, 11.5, 6.5 } } },
  check = { { "poly", { 3.5, 8.5, 6.5, 11.5, 12.5, 4.5 } } },
  arrow_up = { { "line", 8, 13, 8, 3 }, { "poly", { 4, 7, 8, 3, 12, 7 } } },
  arrow_down = { { "line", 8, 3, 8, 13 }, { "poly", { 4, 9, 8, 13, 12, 9 } } },
  stop = { { "rect", 4.5, 4.5, 7, 7, 1.5 } },
  gear = { { "circle", 8, 8, 2 }, { "line", 8, 2, 8, 3.5 }, { "line", 8, 12.5, 8, 14 }, { "line", 2, 8, 3.5, 8 },
    { "line", 12.5, 8, 14, 8 }, { "line", 3.8, 3.8, 4.8, 4.8 }, { "line", 11.2, 11.2, 12.2, 12.2 },
    { "line", 3.8, 12.2, 4.8, 11.2 }, { "line", 11.2, 4.8, 12.2, 3.8 } },
  copy = { { "rect", 5, 5, 8, 8, 1.5 }, { "poly", { 11, 5, 11, 3.5, 10, 2.5, 3.5, 2.5, 2.5, 3.5, 2.5, 10, 3.5, 11, 5, 11 } } },
  more = { { "dot", 4, 8, 1 }, { "dot", 8, 8, 1 }, { "dot", 12, 8, 1 } },
  pin = { { "poly", { 6, 2.5, 10, 2.5, 9.5, 6.5, 11.5, 9, 4.5, 9, 6.5, 6.5 }, true }, { "line", 8, 9, 8, 13.5 } },
  edit = { { "poly", { 11.5, 2.5, 13.5, 4.5, 6, 12, 3, 13, 4, 10 }, true } },
  clock = { { "circle", 8, 8, 5.5 }, { "poly", { 8, 5, 8, 8, 10, 9.5 } } },
  attach = { { "poly", { 10.5, 5.5, 6, 10, 6, 11.2, 7, 12, 8, 12, 13, 7, 13.5, 5.5, 13, 4, 12, 3, 10.5, 2.5,
    9, 3, 4, 8, 3, 9.5, 3, 11, 3.6, 12.4, 5, 13.3, 6.5, 13.5, 8, 13 } } },
}

--- The icon of a slash command, by its name (nil for one without).
M.commands = {
  ["?"] = "question", help = "question", new = "plus", clear = "plus", archive = "archive", compact = "compress",
  context = "clock", sessions = "chat", attach = "attach", detach = "close", catalog = "layers", plugins = "layers",
  checkhealth = "pulse", health = "pulse", config = "gear", settings = "gear", provider = "model", model = "model",
  setup = "plus", rename = "edit", paste = "copy", quit = "close", ["codex-usage"] = "upload", diff = "compress",
  editor = "edit", files = "folder", where = "folder", queue = "clock", runtime = "terminal", tui = "terminal",
  gui = "sidebar", term = "terminal",
}

--- The icon node for slash command `name` (a "/" when it has none). o as M.node.
function M.command(name, o)
  name = tostring(name or ""):gsub("^/", ""):match("^%S*")
  return M.node(M.commands[name] or "slash", o)
end

--- Add or replace icon `name` (shapes on a 16px grid).
function M.define(name, list) shapes[name] = list end

--- Whether there is an icon called `name`.
function M.has(name) return shapes[name] ~= nil end

-- The draw list for `list` scaled by `k`, stroked in `color`.
local function draw_list(list, k, color, width)
  local d = bone.gui.draw
  local out = {}
  for _, s in ipairs(list) do
    local kind = s[1]
    if kind == "circle" then
      out[#out + 1] = d.circle({ x = s[2] * k, y = s[3] * k, r = s[4] * k, stroke = color, width = width })
    elseif kind == "dot" then
      -- A fill names a group's background; a dot is in its text color.
      local fill = color
      if not tostring(color):match("^#") then
        local g = bone.hl.get(color) or {}
        fill = g.fg or color
      end
      out[#out + 1] = d.circle({ x = s[2] * k, y = s[3] * k, r = s[4] * k, fill = fill })
    elseif kind == "line" then
      out[#out + 1] = d.line({ x1 = s[2] * k, y1 = s[3] * k, x2 = s[4] * k, y2 = s[5] * k, color = color, width = width })
    elseif kind == "rect" then
      out[#out + 1] = d.rect({ x = s[2] * k, y = s[3] * k, w = s[4] * k, h = s[5] * k, radius = (s[6] or 0) * k,
        stroke = color, width = width })
    elseif kind == "poly" then
      local pts = {}
      for i = 1, #s[2], 2 do pts[#pts + 1] = { s[2][i] * k, s[2][i + 1] * k } end
      out[#out + 1] = d.polyline({ points = pts, closed = s[3] or false, color = color, width = width })
    end
  end
  return out
end

--- Icon `name` as a node. o: size (16), hl (a group or "#rrggbb"; "Dim"),
--- width (stroke, 1.5 at 16px), and any node fields to add (margin, tooltip…).
function M.node(name, o)
  o = o or {}
  local list = shapes[name]
  local size = o.size or 16
  local k = size / 16
  local color = o.hl or "Dim"
  local width = (o.width or 1.5) * math.max(0.85, k)
  local node = {
    width = size, height = size, shrink = 0,
    canvas = { draw = function() return list and draw_list(list, k, color, width) or {} end },
  }
  for key, v in pairs(o) do
    if key ~= "size" and key ~= "hl" and key ~= "width" then node[key] = v end
  end
  return node
end

return M
