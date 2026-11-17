-- Default statusline and divider (the line between the chat and the prompt).
-- Replace either from tui.lua:
--   bone.ui.statusline = function(ctx) return { " ", ctx.title } end
--
-- Statusline ctx: title, popup ("popup", "picker" or nil), spinner, width,
-- and session (or nil): { title, cwd, running, elapsed }.
-- Divider ctx: spinner, width, session.
--
-- What waits for you (bone.chats) is on it too: a question open in this
-- chat (any bone.ask, whatever tool asked) blinks "waiting for you" where
-- "thinking" would be, and other chats with a question or a reply you have
-- not seen are counted at its end.

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
local chats = require("bone.chats")

-- Colors on the statusline's own background: a span without one would show
-- the terminal's. Set again when the theme changes what they come from.
local made = {}
local function on_line(name, from)
  local bg = (bone.hl.get("StatusLine") or {}).bg
  if made[name] ~= tostring(bg) then
    made[name] = tostring(bg)
    bone.hl.set(name, { link = from, bg = bg, bold = true })
  end
  return name
end

-- A blink: on for 700 ms, off for 500, while something waits. Asks for
-- the frame where it changes.
local function blink()
  local t = bone.now() % 1200
  if bone.ui.refresh_in then bone.ui.refresh_in(t < 700 and 700 - t or 1200 - t) end
  return t < 700
end

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
  -- What is going on, and what waits for you: kept when room runs out.
  local keep = {}
  if session_id and chats.waiting(session_id) then
    keep[#keep + 1] = { "● waiting for you", blink() and on_line("StatusLineWaiting", "SessionWaiting") or "StatusLineDim" }
  elseif s and s.running then
    keep[#keep + 1] = { (ctx.spinner or "") .. " thinking" .. (s.elapsed and (" " .. elapsed(s.elapsed)) or ""), "StatusLine" }
  end
  local other = chats.elsewhere()
  if other.waiting > 0 then
    local on = blink()
    keep[#keep + 1] = { "● " .. other.waiting .. (other.waiting == 1 and " chat waits" or " chats wait") .. " (ctrl+o)",
      on and on_line("StatusLineWaiting", "SessionWaiting") or "StatusLineDim" }
  end
  if other.unread > 0 then
    keep[#keep + 1] = { "● " .. other.unread .. " new", on_line("StatusLineUnread", "SessionUnread") }
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
  -- Room runs out: the numbers go first, last first, then the model; what
  -- is going on stays.
  local function fits(extra)
    local n = #left + #extra
    return items_width(left) + items_width(extra) <= math.max((ctx.width or 1) - 3 * (n - 1), 1)
  end
  while #left > 1 and not fits(keep) do
    table.remove(left)
  end
  for _, it in ipairs(keep) do left[#left + 1] = it end
  while #left > 1 and not fits({}) do
    table.remove(left, 1)
  end
  return joined(left)
end

function bone.ui.divider(ctx)
  return { { fill = "─", hl = "WinSeparator" } }
end
