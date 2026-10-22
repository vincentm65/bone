-- Default statusline and divider (the line between the chat and the prompt).
-- Replace either from tui.lua:
--   bone.ui.statusline = function(ctx) return { " ", ctx.title } end
--
-- Statusline ctx: title, popup ("popup", "picker" or nil), spinner, width,
-- and session (or nil): { title, cwd, running, elapsed }.
-- Divider ctx: spinner, width, session.

local function elapsed(secs)
  return bone.util.duration(secs)
end

local tokens = bone.util.tokens
local agents = require("bone.agents")

local function items_width(items)
  local w = 0
  for _, it in ipairs(items) do
    w = w + bone.strwidth(type(it) == "table" and (it[1] or it.text or "") or it)
  end
  return w
end

local usage = require("bone.usage")

function bone.ui.statusline(ctx)
  -- Match Bone's compact information strip: context first, then metrics and
  -- activity. Keep it quiet when there is no active session.
  local left = {}
  local s = ctx.session
  local session_id = s and s.session_id
  local total = usage.of(session_id)
  if total.model then
    left[#left + 1] = { total.model, "StatusLine" }
  end
  left[#left + 1] = { "curr " .. tokens(total.curr), "StatusLineDim" }
  left[#left + 1] = { "total " .. tokens(total.input + total.output), "StatusLineDim" }
  left[#left + 1] = { "cache " .. (total.input > 0 and string.format("%.0f%%", total.cached / total.input * 100) or "0%"), "StatusLineDim" }
  -- Sub-agents' spend this run (their own sessions, so not in total).
  local spent = session_id and agents.spend(agents.top(session_id)) or 0
  if spent > 0 then
    left[#left + 1] = { "agents " .. tokens(spent), "StatusLineDim" }
  end
  if s and s.running then
    left[#left + 1] = { (ctx.spinner or "") .. " thinking" .. (s.elapsed and (" " .. elapsed(s.elapsed)) or ""), "StatusLine" }
  end

  local function joined(list)
    local out = {}
    for i, it in ipairs(list) do
      if i > 1 then
        out[#out + 1] = { " | ", "StatusLineDim" }
      end
      out[#out + 1] = it
    end
    out[#out + 1] = " "
    return out
  end
  local room = math.max((ctx.width or 1) - 3 * (#left - 1), 1)
  while #left > 1 and items_width(left) > room do
    table.remove(left, s and s.running and #left - 1 or #left)
  end
  return joined(left)
end

function bone.ui.divider(ctx)
  return { { fill = "─", hl = "WinSeparator" } }
end
