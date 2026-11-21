-- Switching models. Each provider keeps a list of models to switch between
-- (`providers.<name>.models`; model/list always counts `model`, the one in
-- use). /model, alt+m and the desktop's model button all come here; the
-- core's model/use moves the on-screen chat to a model, makes it the default
-- for new chats and adds it to its provider's list, in one change.
local M = {}

local last    -- the last model/list, for completion (which cannot wait)
local pending -- the model being switched to: alt+m again goes on from it

local function index(list, value)
  for i, v in ipairs(list or {}) do
    if v == value then return i end
  end
end

local function current(providers)
  for _, p in ipairs(providers or {}) do
    if p.current then return p end
  end
end

function M.label(provider, model)
  return (not provider or provider == model) and model or (provider .. " · " .. model)
end

-- model/list for the on-screen chat, its provider first: cb(providers).
local function list(cb)
  bone.model.list(function(providers, err)
    if err then return bone.notify("cannot list models: " .. tostring(err), "error") end
    table.sort(providers, function(a, b)
      if a.current ~= b.current then return a.current end
      return a.name < b.name
    end)
    last = providers
    cb(providers)
  end)
end

--- Switch the on-screen chat to `model` of `provider` (nil: the chat's).
function M.use(provider, model)
  pending = model
  bone.request("model/use", { session_id = (bone.chat.session() or {}).session_id, provider = provider, model = model },
    function(all, err)
      pending, last = nil, nil
      bone.notify(err and ("cannot switch the model: " .. tostring(err))
        or ("model: " .. M.label((all or {}).provider or provider, model)), err and "error" or nil)
    end)
end

--- The chat's provider's next model (step -1: the one before), round its list.
function M.cycle(step)
  list(function(providers)
    local p = current(providers)
    if not p then return end
    if #p.models < 2 then return bone.notify(p.name .. " has one model; /model NAME adds another") end
    local at = index(p.models, pending) or index(p.models, p.model)
    M.use(p.name, p.models[(at - 1 + step) % #p.models + 1])
  end)
end

--- Take `model` off the chat's provider's list (not the one in use).
function M.remove(model)
  list(function(providers)
    local p = current(providers)
    if not p then return end
    if model == p.model then return bone.notify(model .. " is in use; switch to another first", "error") end
    if not index(p.models, model) then return bone.notify(p.name .. " has no model " .. model, "error") end
    local rest = {}
    for _, m in ipairs(p.models) do
      if m ~= model then rest[#rest + 1] = m end
    end
    bone.settings.set("providers." .. p.name .. ".models", rest, function(_, err)
      last = nil
      bone.notify(err and ("cannot save: " .. tostring(err)) or ("removed " .. model), err and "error" or nil)
    end)
  end)
end

--- The picker: the chat's provider's models (the one in use marked), with
--- a row to add what is typed; under them, the other providers' models.
function M.pick()
  local here
  local function trim(s) return (s:gsub("^%s+", ""):gsub("%s+$", "")) end
  local picker = bone.ui.select({}, {
    prompt = "Model",
    loading = true,
    empty = "no model provider is configured",
    format = function(it) return it.text end,
    section = function(it) return it.section end,
    create = function(query)
      query = trim(query)
      if here and query ~= "" and not index(here.models, query) then
        return "+ add " .. query, here.name
      end
    end,
    on_create = function(query) M.use(here.name, trim(query)) end,
    on_choice = function(it)
      if it and it.edit then return require("bone.config").open("providers") end
      if it and not it.current then M.use(it.provider, it.model) end
    end,
    footer = "type to filter or name a model to add · enter switch · esc close",
  })
  list(function(providers)
    here = current(providers)
    local items = {}
    for _, p in ipairs(providers) do
      for _, m in ipairs(p.models) do
        local on = p.current and m == p.model
        items[#items + 1] = { provider = p.name, model = m, current = on,
          text = (on and "● " or "  ") .. (p.current and m or M.label(p.name, m)),
          section = p.current and p.name or "Other providers" }
      end
    end
    items[#items + 1] = { edit = true, text = "  Edit providers…", section = #providers > 1 and "Other providers" or nil }
    picker:set_items(items)
  end)
end

--- Completion for /model: the chat's provider's models.
function M.complete(token)
  if not last then list(function() end) end
  local p, out = current(last), {}
  for _, m in ipairs(p and p.models or {}) do
    if m:sub(1, #token) == token then
      out[#out + 1] = { value = m, desc = m == p.model and "in use" or nil }
    end
  end
  return out
end

return M
