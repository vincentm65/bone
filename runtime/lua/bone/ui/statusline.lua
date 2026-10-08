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

local function tokens(n)
  if not n then
    return "0"
  end
  n = math.floor(n + 0.5)
  if n >= 1000000 then
    return string.format("%.1fm", n / 1000000)
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

local state = bone._standard_status or {}
bone._standard_status = state
-- Each session's last turn: { started, finished }, kept across chat switches.
state.turns = state.turns or {}
state.total = state.total or { input = 0, output = 0, cached = 0, curr = 0 }
state.total_session = state.total_session or ""

local function now()
  return os.time()
end

local function load_total(session_id, legacy)
  session_id = session_id or (bone.chat.session() or {}).session_id
  local key = session_id or ""
  state.total_session = key
  state.total = { input = 0, output = 0, cached = 0, curr = 0 }
  if key == "" then
    return
  end
  local context = legacy and "input_tokens" or "coalesce(context_tokens, input_tokens)"
  bone.request("store/query", {
    sql = "SELECT coalesce(sum(input_tokens), 0), coalesce(sum(output_tokens), 0), coalesce(sum(cached_tokens), 0), (SELECT " .. context .. " FROM usage WHERE session_id = ?1 AND source = 'turn' ORDER BY rowid DESC LIMIT 1) FROM usage WHERE session_id = ?1",
    params = { session_id },
  }, function(res)
    if state.total_session ~= key then return end
    local row = res and res.rows and res.rows[1]
    -- Retry older servers without the optional context column.
    if not row and not legacy then return load_total(session_id, true) end
    if row then
      state.total = {
        input = tonumber(row[1]) or 0,
        output = tonumber(row[2]) or 0,
        cached = tonumber(row[3]) or 0,
        curr = tonumber(row[4]) or 0,
      }
    end
  end)
end

if state.events then
  for _, id in ipairs(state.events) do bone.off(id) end
end
local function load_model()
  bone.model.list(function(list)
    for _, model in ipairs(list or {}) do
      if model.current then
        state.model = model.model or model.name
        break
      end
    end
  end)
end
load_model()
load_total()
state.events = {
  bone.on("turn/started", function(ev)
    state.turns[ev.session_id] = { started = now() }
  end),
  bone.on("turn/finished", function(ev)
    local t = state.turns[ev.session_id]
    if t then t.finished = now() end
  end),
}
-- Usage is indexed before these events, including the summary call's.
for _, event in ipairs({ "turn/finished", "message/completed", "session/compacted" }) do
  state.events[#state.events + 1] = bone.on(event, function(ev)
    if ev.session_id ~= (bone.chat.session() or {}).session_id then return end
    load_total()
  end)
end
-- Sessions keep their own model: ask the core for the one on screen's.
state.events[#state.events + 1] = bone.on("settings/changed", function(ev)
  if ev.path == "provider" or tostring(ev.path):match("^providers%.") then
    load_model()
  end
end)

function bone.ui.statusline(ctx)
  -- Match Bone's compact information strip: context first, then metrics and
  -- activity. Keep it quiet when there is no active session.
  local left = {}
  local s = ctx.session
  local session_id = s and s.session_id
  if (session_id or "") ~= state.total_session then
    load_total(session_id)
    load_model()
  end
  if state.model then
    left[#left + 1] = { state.model, "StatusLine" }
  end
  left[#left + 1] = { "curr " .. tokens(state.total.curr), "StatusLineDim" }
  left[#left + 1] = { "total " .. tokens(state.total.input + state.total.output), "StatusLineDim" }
  left[#left + 1] = { "cache " .. (state.total.input > 0 and string.format("%.0f%%", state.total.cached / state.total.input * 100) or "0%"), "StatusLineDim" }
  local t = session_id and state.turns[session_id]
  if s and s.running then
    left[#left + 1] = { (ctx.spinner or "") .. " thinking" .. (s.elapsed and (" " .. elapsed(s.elapsed)) or ""), "StatusLine" }
  elseif t and t.finished then
    left[#left + 1] = { "worked " .. elapsed(t.finished - t.started) .. ", finished at " .. bone.util.format_time(t.finished), "StatusLineDim" }
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
