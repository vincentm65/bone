-- Default statusline and divider (the line between the chat and the prompt).
-- Replace either from tui.lua:
--   bone.ui.statusline = function(ctx) return { " ", ctx.title } end
--
-- Statusline ctx: title, popup ("popup", "picker" or nil), spinner, width,
-- and session (or nil): { title, cwd, running,
-- elapsed, usage = { input, output } }.
-- Divider ctx: spinner, width, session.

local function elapsed(secs)
  if not secs then
    return ""
  end
  if secs < 60 then
    return secs .. "s"
  end
  return string.format("%dm%02ds", math.floor(secs / 60), secs % 60)
end

local function tokens(n)
  if not n then
    return "0"
  end
  if n >= 1000 then
    return string.format("%.1fk", n / 1000)
  end
  return tostring(n)
end

local function items_width(items)
  local w = 0
  for _, it in ipairs(items) do
    w = w + bone.strwidth(type(it) == "table" and (it[1] or it.text or "") or it)
  end
  return w
end

local state = bone._standard_status or { generation = 0, started = nil, finished = nil }
bone._standard_status = state
state.generation = state.generation + 1
local generation = state.generation

local function now()
  return os.time()
end

local function refresh_later()
  if state.generation ~= generation then return end
  bone.ui.refresh()
  bone.defer(1000, refresh_later)
end

if state.events then
  for _, id in ipairs(state.events) do bone.off(id) end
end
state.events = {
  bone.on("turn/started", function()
    state.started = now()
    state.finished = nil
    bone.ui.refresh()
    bone.defer(1000, refresh_later)
  end),
  bone.on("turn/finished", function()
    state.finished = now()
    if state.started then state.finished_elapsed = state.finished - state.started end
    state.started = nil
    bone.ui.refresh()
  end),
}

function bone.ui.statusline(ctx)
  -- Match Bone's compact information strip: context first, then metrics and
  -- activity. Keep it quiet when there is no active session.
  local title = ctx.title or (ctx.session and ctx.session.title) or "bone"
  local left = { { title, "StatusLine" } }
  local right = {}
  local s = ctx.session
  if s and s.usage then
    right[#right + 1] = { "in " .. tokens(s.usage.input), "StatusLineDim" }
    right[#right + 1] = { "out " .. tokens(s.usage.output), "StatusLineDim" }
  end
  if s and s.elapsed then
    right[#right + 1] = { elapsed(s.elapsed), "StatusLineDim" }
  end
  if s and s.running then
    local running_for = state.started and (now() - state.started) or nil
    right[#right + 1] = { (ctx.spinner or "") .. " thinking" .. (running_for and (" " .. elapsed(running_for)) or ""), "Accent" }
  elseif state.finished_elapsed then
    right[#right + 1] = { "done " .. elapsed(state.finished_elapsed), "StatusLineDim" }
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
  local room = math.max((ctx.width or 1) - items_width(left) - 1, 1)
  local r = joined(right)
  while #right > 0 and items_width(r) > room do
    right[#right] = nil
    r = joined(right)
  end

  local items = left
  items[#items + 1] = "%="
  for _, it in ipairs(r) do
    items[#items + 1] = it
  end
  return items
end

function bone.ui.divider(ctx)
  local s = ctx.session
  if s and s.running then
    local status = s.elapsed and ("working " .. elapsed(s.elapsed)) or "starting"
    return {
      { "── ", "WinSeparator" },
      { (ctx.spinner or "") .. " " .. status, "Accent" },
      { "  ctrl+c to cancel ", "Dim" },
      { fill = "─", hl = "WinSeparator" },
    }
  end
  return { { fill = "─", hl = "WinSeparator" } }
end
