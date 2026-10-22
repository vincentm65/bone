-- Size of a card window (help, settings, health): as big as the screen
-- allows, up to a comfortable size. The GUI draws the blocks 12 points
-- inside the card. Two rows cover that inset at the usual cell size, so
-- the last line is not what the clip cuts off.
local M = {}

function M.size()
  local cols, rows = 100, 34
  local screen_h = 0
  local ok, geo = pcall(bone.ui.geometry)
  if ok and geo then
    local w, h = 0, 0
    for _, l in ipairs(geo.leaves or {}) do
      w, h = math.max(w, l.x + l.width), math.max(h, l.y + l.height)
    end
    if w > 0 then cols = math.min(cols, w - 8) end
    if h > 0 then rows = math.min(rows, h - 4) end
    screen_h = h
  end
  if screen_h > 0 then
    rows = math.min(rows + 2, screen_h - 2)
  else
    rows = rows + 2
  end
  return math.max(40, cols), math.max(12, rows)
end

return M
