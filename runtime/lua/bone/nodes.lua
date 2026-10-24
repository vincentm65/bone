-- The node view's chat: items as nodes (see docs/plans/gui-native-view.md).
-- A view (bone.ui.views.<kind>) returns `node`, or only `lines` (plain
-- monospace text). `M.chat` is the list node that shows the open chat,
-- making only the items in view.
local M = {}

function M.append(out, nodes)
  for _, node in ipairs(nodes or {}) do out[#out + 1] = node end
end

-- Plain lines as a node (a view with only `lines`); a line's spans,
-- `{ "text", "Group" }`, are node spans as they are.
local function lines_node(lines)
  local spans = {}
  for i, line in ipairs(lines) do
    if i > 1 then spans[#spans + 1] = "\n" end
    if type(line) == "string" then
      spans[#spans + 1] = line
    else
      for _, s in ipairs(line) do spans[#spans + 1] = s end
    end
  end
  return { spans = spans, font = "mono", size = 13, selectable = true }
end
M.lines_node = lines_node

-- Text that is clicked does not select: selectable text takes the clicks.
local function unselectable(node)
  node.selectable = nil
  for _, child in ipairs(node) do
    if type(child) == "table" then unselectable(child) end
  end
end
M.unselectable = unselectable

--- Item `item` as a node, through `bone.ui.views[item.kind]`.
function M.item(item, ctx)
  local view = bone.ui.views[item.kind]
  local r = view and view(item, ctx) or (not view and { lines = { item.text or "" } }) or nil
  if r == nil then return { height = 0 } end
  local node
  if r.node then
    node = r.node
  elseif r.lines then
    node = lines_node(r.lines)
  else
    return { height = 0 }
  end
  if r.click then
    unselectable(node)
    node.on_click = r.click
    node.cursor = "pointer"
  end
  return node
end

--- The chat on screen as a list node. opts: node fields for the list
--- (padding, grow, ...); `id` defaults to "chat".
function M.chat(opts)
  opts = opts or {}
  local node = { id = "chat", grow = 1, follow = true }
  for k, v in pairs(opts) do node[k] = v end
  local s = bone.chat.session()
  local n = s and s.items or 0
  -- Views are told their width in character cells, as in the TUI.
  -- The window passes the cell it was built with; anyone else asks.
  local cell = opts.cell
  if not cell then
    local info = bone.gui.window_info()
    cell = info and info.cell
  end
  local width = math.floor((opts.width_px or 800) / (cell and cell.w or 8))
  node.list = {
    count = n,
    estimate = 60,
    key = s and s.session_id or ("new" .. tostring(s and s.chat or "")),
    -- Each item's node is kept until the chat says the item changed.
    chat = true,
    item = function(i)
      local item = bone.chat.item(i, { full = true })
      if not item then return { height = 0 } end
      return M.item(item, { width = width, region = "chat" })
    end,
  }
  return node
end

return M
