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

local state = bone._standard_status or {}
bone._standard_status = state
-- Each session's last turn: { started, finished }, kept across chat switches.
state.turns = state.turns or {}
-- Each session's usage and model, by id ("" for a new chat), loaded when its
-- statusline first shows.
state.info = state.info or {}

local function now()
  return os.time()
end

local function load_total(key, legacy)
  -- A chat's totals include its agents' (sessions.root).
  local context = legacy and "input_tokens" or "coalesce(context_tokens, input_tokens)"
  local owned = legacy and "" or " OR session_id IN (SELECT id FROM sessions WHERE root = ?1)"
  bone.request("store/query", {
    sql = "SELECT coalesce(sum(input_tokens), 0), coalesce(sum(output_tokens), 0), coalesce(sum(cached_tokens), 0), (SELECT " .. context .. " FROM usage WHERE session_id = ?1 AND source = 'turn' ORDER BY rowid DESC LIMIT 1) FROM usage WHERE session_id = ?1" .. owned,
    params = { key },
  }, function(res)
    local row = res and res.rows and res.rows[1]
    -- Retry older servers without the optional context and root columns.
    if not row and not legacy then return load_total(key, true) end
    if row then
      local info = state.info[key]
      info.input, info.output = tonumber(row[1]) or 0, tonumber(row[2]) or 0
      info.cached, info.curr = tonumber(row[3]) or 0, tonumber(row[4]) or 0
    end
  end)
end

-- Sessions keep their own model: ask the core for each one's.
local function load_model(key)
  bone.request("model/list", { session_id = key ~= "" and key or nil }, function(list)
    for _, model in ipairs(list or {}) do
      if model.current then
        state.info[key].model = model.model or model.name
        break
      end
    end
  end)
end

local function info_of(session_id)
  local key = session_id or ""
  if not state.info[key] then
    state.info[key] = { input = 0, output = 0, cached = 0, curr = 0 }
    if key ~= "" then load_total(key) end
    load_model(key)
  end
  return state.info[key]
end

if state.events then
  for _, id in ipairs(state.events) do bone.off(id) end
end
state.events = {
  bone.on("turn/started", function(ev)
    state.turns[ev.session_id] = { started = now() }
  end),
  bone.on("turn/finished", function(ev)
    local t = state.turns[ev.session_id]
    if t then t.finished = now() end
  end),
}
-- Usage is indexed before these events, including the summary call's; an
-- agent's counts toward its chat.
for _, event in ipairs({ "turn/finished", "message/completed", "session/compacted" }) do
  state.events[#state.events + 1] = bone.on(event, function(ev)
    local key = agents.top(ev.session_id)
    if state.info[key] then load_total(key) end
  end)
end
state.events[#state.events + 1] = bone.on("settings/changed", function(ev)
  if ev.path == "provider" or tostring(ev.path):match("^providers%.") then
    for key in pairs(state.info) do load_model(key) end
  end
end)

function bone.ui.statusline(ctx)
  -- Match Bone's compact information strip: context first, then metrics and
  -- activity. Keep it quiet when there is no active session.
  local left = {}
  local s = ctx.session
  local session_id = s and s.session_id
  local total = info_of(session_id)
  if total.model then
    left[#left + 1] = { total.model, "StatusLine" }
  end
  left[#left + 1] = { "curr " .. tokens(total.curr), "StatusLineDim" }
  left[#left + 1] = { "total " .. tokens(total.input + total.output), "StatusLineDim" }
  left[#left + 1] = { "cache " .. (total.input > 0 and string.format("%.0f%%", total.cached / total.input * 100) or "0%"), "StatusLineDim" }
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
