-- How long each turn worked, for a footer under its last item ("Worked for
-- 3s · Finished at 9:17 PM"). The core times each turn: a turn's user
-- message has `started_at` and `duration_ms` once it ended. M.footer gives
-- the text; each frontend draws it (runtime/lua/bone/ui/turn_timing.lua for
-- the TUI).
local M = {}
local kinds = { "user", "reasoning", "assistant", "tool", "notice" }

-- The clock follows the machine's locale: 12-hour where the locale's own time
-- format carries its AM/PM marker, 24-hour where it does not.
os.setlocale("", "time")
local probe = 1700003200
local marker = os.date("%p", probe)
local clock = marker ~= "" and os.date("%X", probe):find(marker, 1, true) and "%I:%M %p" or "%H:%M"

--- When `item` is the last of a finished turn: { secs (how long it worked),
--- at (the clock time it ended, "9:17 PM") }; else nil.
function M.finished(item, ctx)
  if not ctx or ctx.region ~= "chat" then return end
  local data = bone.chat.item(item.index)
  local user = data and bone.chat.items({ turn = data.turn, kind = "user", first = 1 })[1]
  if not user or not user.duration_ms then return end
  local last = bone.chat.items({ turn = data.turn, kind = kinds, last = 1 })[1]
  if not last or last.index ~= item.index then return end
  local at = os.date(clock, math.floor((user.started_at + user.duration_ms) / 1000))
  if clock == "%I:%M %p" then at = at:gsub("^0", "") end
  return { secs = user.duration_ms / 1000, at = at }
end

--- The footer under `item` when it is the last of a finished turn, else nil.
function M.footer(item, ctx)
  local t = M.finished(item, ctx)
  return t and ("Worked for " .. bone.util.duration(t.secs) .. " · Finished at " .. t.at)
end

return M
