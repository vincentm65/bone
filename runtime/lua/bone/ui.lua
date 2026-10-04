-- Bone's built-in Lua interface. Every visual decision lives in these Lua
-- modules and can be replaced from ~/.bone/lua or a plugin.
local M = {}

function M.setup(opts)
  opts = opts or {}
  if not opts.colorscheme then
    bone.colorscheme("black")
  else
    bone.colorscheme(opts.colorscheme)
  end
  require("bone.ui.views")
  require("bone.ui.statusline")
  require("bone.ui.layout").setup(opts)
  return M
end

return M
