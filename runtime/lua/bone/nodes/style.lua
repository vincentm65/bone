-- The desktop library's look in one place: the sizes, corners, spaces and
-- shadows every widget and window draws with. A `gui.lua` changes one and
-- the whole window follows (these tables are read when the view is built,
-- not copied):
--
--   local style = require("bone.nodes.style")
--   style.radius.lg = 14
--   style.size.body = 13
--
-- Colors are highlight groups (the colorscheme's); the few that cannot be
-- (a tint over what is under it) follow whether the scheme is dark or light.
local M = {}

--- Font sizes, in points.
M.size = {
  micro = 11,   -- section labels, keycaps
  small = 12,   -- hints, meta lines, quiet text
  caption = 13, -- secondary rows, tool cards
  body = 14,    -- controls, list entries
  text = 15,    -- the chat's text
  lead = 17,    -- a window's sub-heading
  title = 20,   -- a window's heading
  display = 24, -- a welcome
}

--- Corner radii, in pixels.
M.radius = {
  sm = 6,    -- a segment, a small button, a keycap
  md = 8,    -- a row, a code block
  lg = 10,   -- an entry, a field, a button
  xl = 12,   -- a window, a menu
  round = 16, -- the composer, a bubble, the chat surface
}

--- Spaces, in pixels.
M.space = { xs = 2, sm = 4, md = 8, lg = 12, xl = 16 }

--- Shadows: a floating card, a menu, and the chat over the sidebar.
M.shadow = {
  card = { blur = 22, dy = 8, color = "#000000a0" },
  menu = { blur = 18, dy = 6, color = "#00000070" },
  big = { blur = 30, dy = 10, color = "#000000c0" },
  surface = { blur = 24, dx = -2, dy = 0, color = "#00000040" },
}

--- The dimmed backdrops behind a dialog and behind a large image.
M.scrim = { dialog = "#00000099", image = "#000000d8" }

--- How long things animate, in milliseconds.
M.motion = { fast = 50, base = 60, slow = 80 }

-- The channels of a "#rrggbb" color, or nil.
local function rgb(color)
  local r, g, b = tostring(color or ""):match("^#(%x%x)(%x%x)(%x%x)$")
  if not r then return nil end
  return tonumber(r, 16), tonumber(g, 16), tonumber(b, 16)
end

--- Whether the colorscheme's background is dark (it is, when it sets none).
function M.dark()
  local r, g, b = rgb((bone.hl.get("Normal") or {}).bg)
  if not r then return true end
  return (0.299 * r + 0.587 * g + 0.114 * b) < 128
end

--- A faint tint that lifts what is under it: for a hover.
function M.hover()
  return M.dark() and "#ffffff0a" or "#0000000a"
end

--- The color of highlight group `name` (its foreground, or `field` = "bg") as
--- "#rrggbb" with the alpha `aa` ("bf"); `fallback` (a color or a group)
--- when the scheme does not set it as hex.
function M.alpha(name, aa, field, fallback)
  local g = bone.hl.get(name) or {}
  local c = g[field or "fg"]
  if rgb(c) then return c .. aa end
  return fallback
end

--- `{ top, right, bottom, left }` from a vertical and a horizontal space.
function M.pad(vertical, horizontal)
  return { vertical, horizontal, vertical, horizontal }
end

--- `{ above, 0, below, 0 }`: space above and below a node.
function M.margin(above, below)
  return { above or 0, 0, below or 0, 0 }
end

--- Bare (`/gui use bare`). bone.nodes.window.setup sets this from `bare`.
--- `M.finish` then drops decoration as each node is built.
M.plain = false

local keep_bg = { Normal = true, Selection = true, TextSelection = true }

--- This node only (not its children). When `M.plain` is false, `node` is
--- returned unchanged. Otherwise corners, shadow, text shadow, transition
--- and margin go; the outline goes unless `id` is `prompt` or `prompt:*`;
--- a fill stays on a floating node (position absolute and z above 0) and
--- when it is Normal, Selection or TextSelection. A non-table is returned
--- as it is.
function M.finish(node)
  if type(node) ~= "table" or not M.plain then return node end
  node.radius, node.shadow, node.text_shadow, node.transition, node.margin = nil, nil, nil, nil, nil
  if node.id ~= "prompt" and not (node.id or ""):match("^prompt:") then
    node.border, node.border_color = nil, nil
  end
  local floating = node.position == "absolute" and (node.z or 0) > 0
  if not floating and not keep_bg[node.bg] then node.bg = nil end
  return node
end

return M
