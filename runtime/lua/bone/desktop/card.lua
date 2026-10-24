-- Size of a card window (help, settings, health), in character cells: as
-- big as the window allows, up to a comfortable size.
local M = {}

function M.size()
  local cols, rows = 100, 36
  local info = bone.gui.window_info()
  if info and (info.columns or 0) > 0 then cols = math.min(cols, info.columns - 8) end
  if info and (info.rows or 0) > 0 then rows = math.min(rows, info.rows - 2) end
  return math.max(40, cols), math.max(12, rows)
end

return M
