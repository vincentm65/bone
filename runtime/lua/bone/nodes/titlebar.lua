-- The title bar the window draws itself on macOS and Windows, where the
-- system's is hidden (bone.gui.window_info().titlebar is "mac" or
-- "windows"; nil elsewhere, in full screen, or with `titlebar = false`).
-- It is the chat's title, quiet and centered, on the window's own color.
-- On macOS the traffic lights stay, at its left; on Windows it draws the
-- app's mark, and minimize, maximize and close. Its colors are the
-- TitleBar group's (fg: the title, bg: the bar), else Dim on the window.
--
-- `window_control` marks what the system acts on: "drag" moves the window
-- (Windows' snap and double click too), "min", "max" and "close" are the
-- caption buttons. Anything clickable put in the bar must stand outside
-- the "drag" part, which the system takes the pointer from on Windows.
local M = {}

local style = require("bone.nodes.style")

--- Its height in pixels. The traffic lights are placed for 34.
M.height = 34
--- What the traffic lights take at the left on macOS.
M.mac_inset = 78
--- A caption button's width on Windows (the system's own).
M.button_width = 46

-- Which caption button the pointer is over, so its glyph can follow.
local hovered = nil

local function group(field)
  return (bone.hl.get("TitleBar") or {})[field]
end

local function text_hl()
  return group("fg") and "TitleBar" or "Dim"
end

-- The glyph color of a caption button.
local function glyph_color(name)
  if hovered == name and name == "close" then return "#ffffff" end
  if hovered == name then return group("fg") and "TitleBar" or "Normal" end
  return text_hl()
end

-- Minimize, maximize (restore when maximized) and close, as the system
-- draws them: 10px glyphs, 1px lines.
local function glyph(name, maximized)
  local draw = bone.gui.draw
  return function(ctx)
    local c = glyph_color(name)
    local x, y = math.floor(ctx.width / 2) - 5, math.floor(ctx.height / 2) - 5
    if name == "min" then
      return { draw.line({ x1 = x, y1 = y + 5.5, x2 = x + 10, y2 = y + 5.5, color = c, width = 1 }) }
    elseif name == "max" and maximized then
      return {
        draw.rect({ x = x + 0.5, y = y + 2.5, w = 7, h = 7, stroke = c, width = 1, radius = 1 }),
        draw.polyline({ points = { { x + 2.5, y + 2.5 }, { x + 2.5, y + 0.5 }, { x + 9.5, y + 0.5 },
          { x + 9.5, y + 7.5 }, { x + 7.5, y + 7.5 } }, color = c, width = 1 }),
      }
    elseif name == "max" then
      return { draw.rect({ x = x + 0.5, y = y + 0.5, w = 9, h = 9, stroke = c, width = 1, radius = 1.5 }) }
    end
    return {
      draw.line({ x1 = x, y1 = y, x2 = x + 10, y2 = y + 10, color = c, width = 1 }),
      draw.line({ x1 = x + 10, y1 = y, x2 = x, y2 = y + 10, color = c, width = 1 }),
    }
  end
end

local function caption_button(name, maximized)
  return {
    width = M.button_width, height = M.height, shrink = 0,
    window_control = name,
    hover = { bg = name == "close" and "#c42b1c" or style.hover() },
    active = { bg = name == "close" and "#c42b1cd0" or style.hover() },
    on_hover = function(on)
      local was = hovered
      if on then hovered = name elseif hovered == name then hovered = nil end
      if hovered ~= was then bone.gui.invalidate() end
    end,
    canvas = { draw = glyph(name, maximized) },
  }
end

-- The app's mark at 16px: a prompt's chevron and cursor (Accent) on a
-- faint tile.
local function mark()
  local draw = bone.gui.draw
  return {
    width = 16, height = 16, shrink = 0, margin = { 0, 0, 0, 12 },
    canvas = { draw = function()
      return {
        draw.rect({ x = 0, y = 0, w = 16, h = 16, radius = 4, fill = style.dark() and "#ffffff14" or "#0000000f" }),
        draw.polyline({ points = { { 4.5, 4.5 }, { 8, 8 }, { 4.5, 11.5 } }, color = "Accent", width = 1.6 }),
        draw.line({ x1 = 9, y1 = 11.5, x2 = 12, y2 = 11.5, color = "Accent", width = 1.6 }),
      }
    end },
  }
end

local function title_text()
  local s = bone.chat.session()
  local t = s and s.title
  -- None yet: a placeholder ("[new session]", "[untitled]") stays out.
  if type(t) ~= "string" or not t:match("%S") or t:match("^%[.*%]$") then return "" end
  return t
end

--- The pixels the bar takes at the window's top: M.height, or 0 when the
--- system draws its own.
function M.offset()
  local kind = bone.gui.window_info().titlebar
  return (kind == "mac" or kind == "windows") and M.height or 0
end

--- The bar, or nil when the system draws its own. `bg`: the window's
--- color under it, when TitleBar sets none.
function M.node(bg)
  local info = bone.gui.window_info()
  local kind = info.titlebar
  if kind ~= "mac" and kind ~= "windows" then return nil end
  local windows = kind == "windows"
  local side = windows and 3 * M.button_width or M.mac_inset
  local bar = {
    id = "titlebar", height = M.height, shrink = 0, direction = "row", align = "center",
    bg = group("bg") and "TitleBar" or bg, window_control = "drag",
    -- macOS: a double click does what the system's settings say (zoom
    -- or minimize). Windows does it itself, on the "drag" part.
    on_double_click = not windows and function()
      bone.gui.window({ titlebar_double_click = true })
    end or nil,
  }
  -- The title, centered on the window, clear of what stands at the sides.
  bar[#bar + 1] = {
    position = "absolute", left = side, right = side, top = 0, bottom = 0,
    direction = "row", align = "center", justify = "center",
    { text = title_text(), hl = text_hl(), size = style.size.small, wrap = false,
      text_overflow = "ellipsis", text_align = "center" },
  }
  if windows then
    bar[#bar + 1] = mark()
    bar[#bar + 1] = { grow = 1 }
    bar[#bar + 1] = caption_button("min")
    bar[#bar + 1] = caption_button("max", info.maximized)
    bar[#bar + 1] = caption_button("close")
  end
  return style.finish(bar)
end

return M
