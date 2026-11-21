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

-- The saved provider, for a new chat (no session yet) so the model and
-- effort controls can draw before model/list comes back.
local function configured()
  local provider = bone.settings.get("provider")
  if type(provider) ~= "string" or provider == "" then return nil end
  local model = bone.settings.get("providers." .. provider .. ".model")
  if type(model) ~= "string" or model == "" then return nil end
  return provider, model
end

-- Sessions keep their own model: ask the core for each one's.
local function load_model(key)
  bone.request("model/list", { session_id = key ~= "" and key or nil }, function(list)
    local info = state.info[key]
    if not info then return end
    local changed = false
    for _, model in ipairs(list or {}) do
      if model.current then
        local name = model.model or model.name
        -- What the core actually compacts at: the provider's own limit, or
        -- compact.limit when it sets none (0 still means "never").
        local limit = tonumber(model.compact_limit) or tonumber(bone.settings.get("compact.limit"))
        if info.model ~= name or info.provider ~= model.name or info.limit ~= limit then
          info.model, info.provider, info.limit = name, model.name, limit
          changed = true
        end
        break
      end
    end
    -- A new chat paints before this returns; redraw once the model is known.
    if changed and bone.gui and bone.gui.invalidate then bone.gui.invalidate() end
  end)
end

--- Session `session_id`'s usage (`nil`: a new chat): { model, provider,
--- input, output, cached, curr, limit } (curr: the last call's context;
--- limit: where it compacts, when the provider sets one).
function M.of(session_id)
  local key = session_id or ""
  if not state.info[key] then
    state.info[key] = { input = 0, output = 0, cached = 0, curr = 0 }
    if key == "" then
      local provider, model = configured()
      if provider then
        state.info[key].provider, state.info[key].model = provider, model
      end
    else
      load_total(key)
    end
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
