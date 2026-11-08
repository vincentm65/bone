-- Docked panels resize by dragging the border between them and the chat,
-- like the sessions sidebar's edge: press on it, drag, release. The size
-- (columns, or rows above and below the chat) is kept between runs as
-- "dock-cells.<id>" and wins over the size the panel's owner asks for.
local M = { cells = {} }

-- The least a dragged panel keeps.
M.min = 10

local function key(id)
  return "dock-cells." .. tostring(id):gsub("[^%w_%-%.]", "_")
end

--- The size the user dragged panel `id` to, or nil.
function M.size(id)
  if M.cells[id] == nil then
    M.cells[id] = tonumber(bone.state.load(key(id))) or false
  end
  return M.cells[id] or nil
end

bone.ui.panel.user_size = M.size

-- The border column (or row) between a docked panel and the chat.
local function border(p)
  if p.dock == "right" then return "x", p.col - 1 end
  if p.dock == "left" then return "x", p.col + p.width end
  if p.dock == "bottom" then return "y", p.row - 1 end
  if p.dock == "top" then return "y", p.row + p.height end
end

-- The panel's size with its border at `at`.
local function size_at(p, at)
  if p.dock == "right" then return p.col + p.width - at - 1 end
  if p.dock == "left" then return at - p.col end
  if p.dock == "bottom" then return p.row + p.height - at - 1 end
  return at - p.row
end

local drag -- { id } while a border is held

bone.on("mouse", function(ev)
  if drag then
    local p = bone.ui.panel.get(drag.id)
    local info = p and p:info()
    if ev.action == "drag" and info and info.visible then
      local axis = (info.dock == "left" or info.dock == "right") and "x" or "y"
      local n = math.max(M.min, size_at(info, ev[axis]))
      if n ~= M.cells[drag.id] then
        M.cells[drag.id] = n
        p:update({ size = n })
      end
    else
      if M.cells[drag.id] then bone.state.save(key(drag.id), M.cells[drag.id]) end
      drag = nil
    end
    return true
  end
  if ev.popup or ev.action ~= "down" or ev.button ~= "left" then return end
  for _, info in ipairs(bone.ui.panel.list()) do
    -- The sessions sidebar drags its own edge (bone.ui.sessions).
    if info.visible and info.size ~= "auto" and info.id ~= "sessions" then
      local axis, at = border(info)
      local along = axis == "x" and ev.y >= info.row and ev.y < info.row + info.height
        or axis == "y" and ev.x >= info.col and ev.x < info.col + info.width
      if at and along and ev[axis] == at then
        drag = { id = info.id }
        return true
      end
    end
  end
end)

return M
