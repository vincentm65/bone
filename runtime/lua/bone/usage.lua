-- Each session's token usage and model, for status lines to show:
-- M.of(session_id) loads them the first time, and they stay current with
-- turn, message, compaction and settings events. Shared by every
-- frontend's status line (runtime/lua/bone/ui/statusline.lua for the TUI).
local M = {}

local state = bone._standard_status or {}
bone._standard_status = state
-- Each session's usage and model, by id ("" for a new chat), loaded when its
-- statusline first shows.
state.info = state.info or {}

local function load_total(key, legacy)
  local context = legacy and "input_tokens" or "coalesce(context_tokens, input_tokens)"
  bone.request("store/query", {
    sql = "SELECT coalesce(sum(input_tokens), 0), coalesce(sum(output_tokens), 0), coalesce(sum(cached_tokens), 0), (SELECT " .. context .. " FROM usage WHERE session_id = ?1 AND source = 'turn' ORDER BY rowid DESC LIMIT 1) FROM usage WHERE session_id = ?1",
    params = { key },
  }, function(res)
    local row = res and res.rows and res.rows[1]
    -- Retry older servers without the optional context column.
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

--- Session `session_id`'s usage (`nil`: a new chat): { model, input,
--- output, cached, curr } (curr: the last call's context).
function M.of(session_id)
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
state.events = {}
-- Usage is indexed before these events, including the summary call's.
for _, event in ipairs({ "turn/finished", "message/completed", "session/compacted" }) do
  state.events[#state.events + 1] = bone.on(event, function(ev)
    if state.info[ev.session_id] then load_total(ev.session_id) end
  end)
end
state.events[#state.events + 1] = bone.on("settings/changed", function(ev)
  if ev.path == "provider" or tostring(ev.path):match("^providers%.") then
    for key in pairs(state.info) do load_model(key) end
  end
end)


return M
