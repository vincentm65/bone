-- Switching models. Each provider keeps a list of models to switch between
-- (`providers.<name>.models`, beside `model`, the one in use); /model, the
-- next-model key (alt+m) and the desktop composer's model button all come
-- here, so the TUI and the desktop do the same thing.
--
-- A switch moves the on-screen chat (the core saves its choice) and makes
-- that model the provider's default for new chats, as /config does. An older
-- core lists no models: then each provider has just its own, and switching
-- still works.
local M = {}

-- The switch being made: { provider, model }, so a key pressed again before
-- it lands goes on from there.
local pending

-- The first entry of `list` equal to `value` (or whose `key` field is).
local function find(list, value, key)
  for i, v in ipairs(list or {}) do
    local x = v
    if key then x = v[key] end
    if x == value then return v, i end
  end
end

local function label(provider, model)
  return provider == model and model or (provider .. " · " .. model)
end

local function copy(list)
  local out = {}
  for i, v in ipairs(list) do out[i] = v end
  return out
end

-- The last list model/list gave (for completion, which cannot wait).
M.last = nil

--- The providers, as the on-screen chat sees them: cb(list, err), each
--- { name, model, models, current }; `list.saves` is false for a core that
--- keeps no lists.
function M.list(cb)
  bone.model.list(function(list, err)
    list = list or {}
    list.saves = false
    for _, p in ipairs(list) do
      if p.models ~= nil then list.saves = true end
      if type(p.models) ~= "table" or #p.models == 0 then p.models = { p.model } end
    end
    table.sort(list, function(a, b)
      if a.current ~= b.current then return a.current end
      return a.name < b.name
    end)
    if not err then M.last = list end
    cb(list, err)
  end)
end

-- Save `p`'s list with `model` added (it is not in it).
local function remember(list, p, model, done)
  if not list.saves or find(p.models, model) then return done() end
  local models = copy(p.models)
  models[#models + 1] = model
  bone.settings.set("providers." .. p.name .. ".models", models, function(_, err)
    if err then bone.notify("cannot save " .. p.name .. "'s models: " .. tostring(err), "error") end
    done()
  end)
end

-- Switch to `model` of provider `p` (from `list`), then call done(err).
local function switch(list, p, model, done)
  pending = { provider = p.name, model = model }
  local function finish(_, err)
    pending = nil
    if err then
      bone.notify("cannot switch the model: " .. tostring(err), "error")
    else
      bone.notify("model: " .. label(p.name, model))
    end
    if done then done(err) end
  end
  if p.current then
    -- The chat follows a change to its own provider's model.
    return bone.settings.set("providers." .. p.name .. ".model", model, finish)
  end
  -- Another provider: its model first (no chat follows that), then the
  -- provider itself, which the chat follows with that model.
  local function choose() bone.settings.set("provider", p.name, finish) end
  if model == p.model then return choose() end
  bone.settings.set("providers." .. p.name .. ".model", model, function(_, err)
    if err then return finish(nil, err) end
    choose()
  end)
end

--- Switch the on-screen chat to `model` of `provider` (default: the chat's
--- own), adding the model to that provider's list when it is new.
function M.use(provider, model, done)
  M.list(function(list, err)
    if err then return bone.notify("cannot list models: " .. tostring(err), "error") end
    local p = provider and find(list, provider, "name") or (not provider and find(list, true, "current"))
    if not p then
      return bone.notify(provider and ("no provider " .. provider) or "no model provider is configured", "error")
    end
    switch(list, p, model, function(e)
      if e then return done and done(e) end
      remember(list, p, model, function() if done then done() end end)
    end)
  end)
end

--- Move the on-screen chat to its provider's next model (step -1: the one
--- before), round the list.
function M.cycle(step)
  M.list(function(list, err)
    local p = find(list, true, "current")
    if not p then
      return bone.notify(err and ("cannot list models: " .. tostring(err)) or "no model provider is configured", "error")
    end
    if #p.models < 2 then
      return bone.notify(list.saves and (p.name .. " has one model (" .. p.model .. "); /model NAME adds another")
        or "this bone's core keeps no model lists: update bone to switch with alt+m")
    end
    local from = pending and pending.provider == p.name and pending.model or p.model
    local _, at = find(p.models, from)
    switch(list, p, p.models[((at or 1) - 1 + (step or 1)) % #p.models + 1])
  end)
end

--- Take `model` off the on-screen chat's provider's list (not the one in
--- use: switch away from it first).
function M.remove(model, done)
  M.list(function(list, err)
    local p = find(list, true, "current")
    if not p then return bone.notify(err and tostring(err) or "no model provider is configured", "error") end
    if not list.saves then return bone.notify("this bone keeps no model lists yet (rebuild it)", "error") end
    if model == p.model then
      return bone.notify(model .. " is in use; switch to another model first", "error")
    end
    local models = {}
    for _, m in ipairs(p.models) do
      if m ~= model then models[#models + 1] = m end
    end
    if #models == #p.models then return bone.notify(p.name .. " has no model " .. model, "error") end
    bone.settings.set("providers." .. p.name .. ".models", models, function(_, e)
      bone.notify(e and ("cannot save: " .. tostring(e)) or ("removed " .. model .. " from " .. p.name), e and "error" or nil)
      if done then done(e) end
    end)
  end)
end

--- Add `model` to provider `provider`'s list without switching to it.
function M.add(provider, model, done)
  M.list(function(list)
    local p = find(list, provider, "name")
    if not p then return done and done("no provider " .. tostring(provider)) end
    remember(list, p, model, function() if done then done() end end)
  end)
end

--- The picker: every provider's models (the chat's provider first, its
--- model marked), and what is typed, to add to the chat's provider.
function M.pick()
  local here
  local picker = bone.ui.select({}, {
    prompt = "Model",
    loading = true,
    empty = "no model provider is configured",
    format = function(it)
      if it.action then return "  " .. it.text end
      return (it.current and "● " or "  ") .. label(it.provider, it.model)
    end,
    create = function(query)
      if not here or query:find("^%s*$") then return nil end
      query = query:gsub("^%s+", ""):gsub("%s+$", "")
      for _, m in ipairs(here.models) do
        if m == query then return nil end
      end
      return "+ " .. label(here.name, query) .. "  (add)"
    end,
    on_create = function(query)
      M.use(here.name, (query:gsub("^%s+", ""):gsub("%s+$", "")))
    end,
    on_choice = function(it)
      if not it then return end
      if it.action == "providers" then return require("bone.config").open("providers") end
      if not it.current then M.use(it.provider, it.model) end
    end,
    footer = "type to filter or name a model to add · enter switch · esc close",
  })
  M.list(function(list, err)
    if err then
      picker:close()
      return bone.notify("cannot list models: " .. tostring(err), "error")
    end
    here = find(list, true, "current")
    local items = {}
    for _, p in ipairs(list) do
      for _, m in ipairs(p.models) do
        items[#items + 1] = { provider = p.name, model = m, current = p.current and m == p.model }
      end
    end
    items[#items + 1] = { action = "providers", text = "Edit providers…" }
    picker:set_items(items)
  end)
end

--- Completion for /model: the chat's provider's models, from the last list
--- (asked for again, for the next time).
function M.complete(token)
  M.list(function() end)
  local p = M.last and find(M.last, true, "current")
  local out = {}
  for _, m in ipairs(p and p.models or {}) do
    if m:sub(1, #(token or "")) == (token or "") then
      out[#out + 1] = { value = m, desc = m == p.model and "in use" or nil }
    end
  end
  return out
end

return M
