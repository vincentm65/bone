-- The desktop library's status bar: the model, how full the context is and
-- what the session has used, and what is running, in quiet text under the
-- composer. The numbers are bone.usage's.
local M = {}

local usage = require("bone.usage")
local agents = require("bone.agents")
local tokens = bone.util.tokens

function M.render(ctx)
  local s = ctx.session
  local id = s and s.session_id
  local u = usage.of(id)
  local parts = {}
  local function add(text, hl) parts[#parts + 1] = { text, hl or "StatusLineDim" } end
  if s and s.running then
    add((ctx.spinner or "●") .. " working" .. (s.elapsed and (" " .. bone.util.duration(s.elapsed)) or ""), "StatusLine")
  end
  if u.model then add(u.model, "StatusLine") end
  if (u.curr or 0) > 0 then add(tokens(u.curr) .. " in context") end
  local total = (u.input or 0) + (u.output or 0)
  if total > 0 then
    add(tokens(total) .. " used")
    if (u.input or 0) > 0 then add(string.format("%.0f%% cached", (u.cached or 0) / u.input * 100)) end
  end
  local spent = id and agents.spend(agents.top(id)) or 0
  if spent > 0 then add(tokens(spent) .. " by agents") end
  local out = { { "  ", "StatusLineDim" } }
  for i, p in ipairs(parts) do
    if i > 1 then out[#out + 1] = { "   ·   ", "StatusLineDim" } end
    out[#out + 1] = p
  end
  return out
end

function M.setup()
  bone.ui.statusline = M.render
  bone.ui.divider = nil
end

return M
