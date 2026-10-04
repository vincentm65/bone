-- Bone's built-in Lua interface. Every visual decision lives in these Lua
-- modules and can be replaced from ~/.bone/lua or a plugin.
local M = {}

function M.setup(opts)
  opts = opts or {}
  -- Reasoning is hidden by default (the option's default); only an
  -- explicit choice here changes it, so ctrl+r survives a reload.
  if opts.show_reasoning ~= nil then
    bone.o.show_reasoning = opts.show_reasoning
  end
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
