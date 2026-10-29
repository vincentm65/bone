-- The settings page (/config) as state and actions, for every frontend's
-- settings screen to draw: General (the frontend's options and declared
-- pages), Providers (with an editor) and Plugins, saved in settings.json
-- through the core and applied at once. runtime/lua/bone/config.lua draws
-- it for the TUI.
--
-- A plugin gets a detail page by declaring its settings, either in its
-- manifest.json ("settings": [ ... ]) or with bone.settings.page{ name,
-- title, fields = { ... } }. A field is { key, label, type = "boolean" |
-- "number" | "integer" | "string", choices, default, min, max, desc }; its
-- value is saved as "<name>.<key>" and read with bone.settings.get.
--
-- local page = require("bone.settings_page").new(tab, { scope = "gui",
--   changed = redraw, close = close }) gives page.st (the state: tabs, tab,
-- sel, edit, note, ask, prov, providers), page.rows(), page.on_key(key),
-- page.activate(row, space), page.set(row, value), page.switch_tab(by) and
-- page.paste(text). `/` searches the rows in view (st.query; st.search while
-- it is being typed): each word must be in a row's name, value or text.
local M = {}

local function text_width(s)
  return bone.text.width(s)
end

-- ---- what the tabs hold ----------------------------------------------------

-- The frontend's options: built-in ones and those the runtime and plugins
-- define, saved as <scope>.<name> ("tui" or "gui").
local function option_rows(scope)
  local rows = {}
  local names = bone.o.names()
  table.sort(names)
  for _, name in ipairs(names) do
    local ok, info = pcall(bone.o.info, name)
    if ok and info then
      rows[#rows + 1] = {
        kind = "option",
        name = name,
        label = name:gsub("_", " "),
        type = info.type,
        choices = info.choices,
        value = info.value,
        default = info.default,
        desc = info.desc or "",
        path = scope .. "." .. name,
      }
    end
  end
  return rows
end

-- Fields of a declared settings page, with their saved values.
local function field_rows(page)
  local rows = {}
  for _, f in ipairs(page.fields or {}) do
    if type(f.key) == "string" then
      local path = page.name .. "." .. f.key
      local saved = bone.settings.get(path)
      local ftype = f.type or (f.choices and "string") or type(f.default)
      rows[#rows + 1] = {
        kind = "field",
        name = f.key,
        label = f.label or f.key:gsub("_", " "),
        type = ftype == "boolean" and "boolean" or ftype,
        choices = f.choices,
        value = saved == nil and f.default or saved,
        default = f.default,
        min = f.min,
        max = f.max,
        desc = f.desc or "",
        path = path,
        saved = saved ~= nil,
      }
    end
  end
  return rows
end

-- Settings pages and display titles from the plugins' manifest.json files.
local function manifest_pages(titles)
  local pages = {}
  local dir = bone.config_dir and (bone.config_dir .. "/plugins")
  for _, e in ipairs(dir and bone.fs.list(dir) or {}) do
    -- Hidden ones are not plugins: a catalog install stages in .NAME.*.staging.
    if e.type == "dir" and e.name:sub(1, 1) ~= "." then
      local f = io.open(dir .. "/" .. e.name .. "/manifest.json")
      if f then
        local text = f:read("*a")
        f:close()
        local ok, m = pcall(bone.json.decode, text)
        if ok and type(m) == "table" then
          titles[e.name] = require("bone.catalog").label({ name = e.name, title = m.title })
          if type(m.settings) == "table" and #m.settings > 0 then
            pages[#pages + 1] = { name = e.name, title = titles[e.name], fields = m.settings }
          end
        end
      end
    end
  end
  return pages
end

local function tabs(titles)
  local out = {
    { name = "general", title = "General" },
    { name = "providers", title = "Providers" },
    { name = "plugins", title = "Plugins" },
  }
  local seen = {}
  local declared = {}
  for _, p in ipairs(bone._settings_pages or {}) do
    declared[#declared + 1] = p
  end
  for _, p in ipairs(manifest_pages(titles)) do
    declared[#declared + 1] = p
  end
  table.sort(declared, function(a, b)
    return tostring(a.title):lower() < tostring(b.title):lower()
  end)
  for _, p in ipairs(declared) do
    if not seen[p.name] then
      seen[p.name] = true
      out[#out + 1] = { name = p.name, title = p.title, page = p, section = p.name == "compact" and 1 or 3 }
    end
  end
  return out
end

--- A page opened on `want` (a section or detail page, by name or title).
function M.new(want, opts)
  local titles = {}
  local st = {
    tabs = tabs(titles),
    tab = 1,
    sel = 1,
    edit = nil, -- the text being typed into a field
    query = "", -- what the rows in view are filtered by
    search = false, -- true while the query is being typed
    note = nil, -- a line under the rows: what happened, or an error
    providers = nil, -- from model/list, nil while loading
    prov = nil, -- the provider being edited: { name, new, draft }
    keys = {}, -- providers with a key in secrets.json
    ask = nil, -- a yes/no question: { text, action }
    core_plugins = nil, -- from plugin/list
  }
  if want and want ~= "" then
    for i, t in ipairs(st.tabs) do
      if t.name == want or t.title:lower() == want:lower() then
        st.tab = i
      end
    end
  end
  local function redraw()
    opts.changed()
  end

  local save_provider_field
  local function load_providers()
    bone.model.list(function(list, err)
      st.providers = list or {}; if want == "providers" and not st.prov then for i, p in ipairs(st.providers) do if p.current then st.sel = i; break end end end
      if err then
        st.note = { "cannot list providers: " .. tostring(err), "ErrorMsg" }
      end
      redraw()
    end)
    bone.request("secrets/list", {}, function(names)
      st.keys = {}
      for _, n in ipairs(names or {}) do
        st.keys[n] = true
      end
      redraw()
    end)
  end

  local function provider_info(name)
    for _, p in ipairs(st.providers or {}) do
      if p.name == name then
        return p
      end
    end
  end

  local EFFORTS = { "default", "low", "medium", "high", "xhigh", "max", "none" }
  local APIS = { "auto", "completions", "responses", "anthropic" }

  -- The editor's rows for one provider (or a new one's draft).
  local function provider_rows()
    local pv = st.prov
    if pv.new then
      local d = pv.draft
      return {
        { kind = "pfield", key = "name", label = "Name", type = "string", value = d.name, desc = "letters, digits, _ and -" },
        { kind = "pfield", key = "base_url", label = "URL", type = "string", value = d.base_url, desc = "the OpenAI-compatible endpoint, ending in /v1" },
        { kind = "pfield", key = "model", label = "Model", type = "string", value = d.model, desc = "the model name the server knows" },
        { kind = "pfield", key = "type", label = "Type", type = "string", value = d.type, desc = "a Lua provider type (from a plugin); empty: OpenAI-compatible" },
        { kind = "pfield", key = "key", label = "API key", type = "secret", value = d.key, desc = "saved in ~/.bone/secrets.json, readable only by you" },
        { kind = "pfield", key = "api_key_env", label = "Key variable", type = "string", value = d.api_key_env, desc = "or an environment variable that holds the key" },
        { kind = "paction", action = "save", label = "Save" },
      }
    end
    local p = provider_info(pv.name) or {}
    local base = "providers." .. pv.name
    local function changed(key)
      return not p.added and bone.settings.get(base .. "." .. key) ~= nil
    end
    local env = bone.settings.get(base .. ".api_key_env") or ""
    local key_text = st.keys[pv.name] and "saved" or (env ~= "" and ("from $" .. env)) or (p.has_key and "set in core.lua") or "none"
    local out = {
      { kind = "pfield", key = "model", label = "Model", type = "string", value = p.model, changed = changed("model") },
      { kind = "pfield", key = "base_url", label = "URL", type = "string", value = p.base_url, changed = changed("base_url") },
      { kind = "pfield", key = "type", label = "Type", type = "string", value = p.type or "", changed = changed("type"),
        desc = "a Lua provider type (from a plugin); empty: OpenAI-compatible" },
      { kind = "pfield", key = "reasoning_effort", label = "Reasoning effort", type = "choice", choices = EFFORTS,
        value = p.reasoning_effort or "default", changed = changed("reasoning_effort"),
        desc = "default sends nothing (the provider chooses)" },
      { kind = "pfield", key = "supports_images", label = "Accept images", type = "boolean", value = p.supports_images ~= false,
        changed = changed("supports_images"), desc = "allow screenshots for a vision-enabled model; disable for text-only models" },
      { kind = "pfield", key = "stream_usage", label = "Stream usage", type = "boolean", value = p.stream_usage ~= false,
        changed = changed("stream_usage"), desc = "ask for token usage while streaming (some servers refuse it)" },
      { kind = "pfield", key = "key", label = "API key", type = "secret", value = key_text,
        desc = "enter types a new key (saved in ~/.bone/secrets.json, readable only by you); empty removes the saved one" },
      { kind = "pfield", key = "api_key_env", label = "Key variable", type = "string", value = env,
        desc = "an environment variable that holds the key" },
      { kind = "pfield", key = "replay_reasoning", label = "Replay reasoning", type = "choice", choices = { "auto", "on", "off" },
        value = p.replay_reasoning == nil and "auto" or (p.replay_reasoning and "on" or "off"), changed = changed("replay_reasoning"),
        desc = "send reasoning back with past messages; auto does once the server streams some" },
      { kind = "pfield", key = "api", label = "API", type = "choice", choices = APIS,
        value = p.api_auto and "auto" or p.api or "completions", changed = changed("api"),
        desc = (p.api_auto and ("auto picks " .. (p.api or "completions") .. " for this URL; ") or "")
          .. "set it for a gateway that forwards /responses or /messages" },
      { kind = "pfield", key = "compact_limit", label = "Compaction limit", type = "integer", value = p.compact_limit,
        changed = changed("compact_limit"), desc = "tokens before proactive compaction; unset: global compact.limit; 0: disabled (overflow retry unchanged)" },
    }
    if not p.current then
      out[#out + 1] = { kind = "paction", action = "use", label = "Use this provider" }
    end
    if p.added then
      out[#out + 1] = { kind = "paction", action = "delete", label = "Delete this provider" }
    elseif bone.settings.get(base) ~= nil then
      out[#out + 1] = { kind = "paction", action = "undo", label = "Undo changes (back to core.lua)" }
    end
    return out
  end

  local function load_plugins()
    bone.request("plugin/list", {}, function(list)
      st.core_plugins = list or {}
      redraw()
    end)
  end

  local function disabled_plugins()
    local saved = bone.settings.get("plugins.disabled")
    return type(saved) == "table" and saved or {}
  end

  local function plugin_rows()
    local rows, by = {}, {}
    local off = {}
    for _, n in ipairs(disabled_plugins()) do
      off[n] = true
    end
    local function add(name)
      if not by[name] then
        by[name] = { kind = "plugin", name = name, label = titles[name] or name, tui = false, core = false }
        rows[#rows + 1] = by[name]
      end
      return by[name]
    end
    for _, p in ipairs(bone.plugin.list()) do
      if p.kind ~= "project" then
        local r = add(p.name)
        r.tui, r.tui_loaded, r.error = true, p.loaded, p.error
      end
    end
    for _, p in ipairs(st.core_plugins or {}) do
      if p.core then
        local r = add(p.name)
        r.core, r.core_loaded = true, p.loaded
      end
    end
    for i, t in ipairs(st.tabs) do
      if t.section == 3 then
        local r = add(t.name)
        r.label, r.page = t.title, i
        r.kind = (r.tui or r.core) and "plugin" or "page"
      end
    end
    for _, r in ipairs(rows) do
      r.on = not off[r.name]
    end
    table.sort(rows, function(a, b)
      return a.name < b.name
    end)
    return rows
  end

  local function all_rows()
    local t = st.tabs[st.tab]
    if t.name == "general" then
      local out = option_rows(opts.scope)
      for i, page in ipairs(st.tabs) do
        if page.section == 1 then
          out[#out + 1] = { kind = "page", label = page.title, page = i }
        end
      end
      return out
    elseif t.name == "providers" then
      if st.prov then
        return provider_rows()
      end
      local out = {}
      for _, p in ipairs(st.providers or {}) do
        out[#out + 1] = {
          kind = "provider", name = p.name, label = p.name, model = p.model, url = p.base_url,
          current = p.current, added = p.added,
          desc = p.added and "added in /config (or /setup)" or "from core.lua",
        }
      end
      return out
    elseif t.name == "plugins" then
      return plugin_rows()
    end
    return field_rows(t.page)
  end

  -- What a search looks in: the row's name, its value and what it says.
  local function haystack(r)
    local parts = {}
    for _, k in ipairs({ "label", "name", "desc", "model", "url", "path" }) do
      if type(r[k]) == "string" then parts[#parts + 1] = r[k] end
    end
    if r.value ~= nil and type(r.value) ~= "table" and r.type ~= "secret" then parts[#parts + 1] = tostring(r.value) end
    return table.concat(parts, " "):lower()
  end

  -- The rows in view, those the query matches. A search belongs to the
  -- section, page or provider it was typed in; elsewhere it starts empty.
  local function rows()
    local where = st.tab .. ":" .. (st.prov and (st.prov.name or "+") or "")
    if st.search_where ~= where then
      st.query, st.search, st.search_where = "", false, where
    end
    local list = all_rows()
    if st.query == "" then return list end
    local out = {}
    for _, r in ipairs(list) do
      local text, match = haystack(r), true
      for word in st.query:lower():gmatch("%S+") do
        if not text:find(word, 1, true) then match = false break end
      end
      if match then out[#out + 1] = r end
    end
    return out
  end

  local function saved_note(err, ok_text)
    if err then
      st.note = { tostring(err), "ErrorMsg" }
    else
      st.note = { ok_text, "Dim" }
    end
    redraw()
  end

  -- Save a value: an option is set now too; settings.json keeps it.
  local function save(row, value)
    if row.kind == "option" then
      local ok, e = pcall(function()
        bone.o[row.name] = value
      end)
      if not ok then
        st.note = { tostring(e):gsub("^.-: ", ""), "ErrorMsg" }
        return
      end
    end
    bone.settings.set(row.path, value, function(_, err)
      saved_note(err, "saved " .. row.path)
    end)
  end

  local function reset(row)
    if row.kind == "option" and row.default ~= nil then
      pcall(function()
        bone.o[row.name] = row.default
      end)
    end
    bone.settings.reset(row.path, function(_, err)
      saved_note(err, "reset " .. row.path)
    end)
  end

  local function toggle_plugin(row)
    local list = {}
    for _, n in ipairs(disabled_plugins()) do
      if n ~= row.name then
        list[#list + 1] = n
      end
    end
    local turning_on = not row.on
    if not turning_on then
      list[#list + 1] = row.name
    end
    table.sort(list)
    local function after(_, err)
      if err then
        return saved_note(err)
      end
      if row.tui then
        pcall(turning_on and bone.plugin.load or bone.plugin.unload, row.name)
      end
      if row.core then
        bone.request(turning_on and "plugin/load" or "plugin/unload", { name = row.name }, function(_, e)
          load_plugins()
          saved_note(e, row.name .. (turning_on and " on" or " off"))
        end)
      else
        saved_note(nil, row.name .. (turning_on and " on" or " off"))
      end
    end
    if #list > 0 then
      bone.settings.set("plugins.disabled", list, after)
    else
      bone.settings.reset("plugins.disabled", after)
    end
  end

  -- One field of the provider being edited (or of a new one's draft).
  -- An empty text, "default" or "auto" goes back to core.lua's value.
  function save_provider_field(row, value)
    if row.key == "compact_limit" and value ~= "" then
      value = tonumber(value)
      if not value or value < 0 or value % 1 ~= 0 then
        return saved_note(row.label .. " must be a nonnegative whole number")
      end
    end
    local pv = st.prov
    if pv.new then
      pv.draft[row.key] = value
      return redraw()
    end
    local name = pv.name
    if row.key == "key" then
      local key = value ~= "" and value or nil
      return bone.request("secrets/set", { provider = name, key = key }, function(_, err)
        load_providers()
        saved_note(err, value ~= "" and ("saved the key for " .. name) or ("removed the saved key for " .. name))
      end)
    end
    local path = "providers." .. name .. "." .. row.key
    local function after(_, err)
      load_providers()
      saved_note(err, "saved " .. row.label:lower())
    end
    if value == "on" or value == "off" then
      value = value == "on"
    end
    if value == "" or value == "default" or value == "auto" then
      bone.settings.reset(path, after)
    else
      bone.settings.set(path, value, after)
    end
  end

  -- Enter / space on a row.
  local function activate(row, space)
    if not row then
      return
    end
    if row.kind == "provider" then
      if space then
        return
      end
      bone.settings.set("provider", row.name, function(_, err)
        load_providers()
        saved_note(err, "using " .. row.name); if not err and want == "providers" then opts.close() end
      end)
    elseif row.kind == "paction" then
      if space then
        return
      end
      local name = st.prov.name
      if row.action == "save" then
        local d = st.prov.draft
        if not d.name:match("^[%w_%-]+$") then
          return saved_note("the name is letters, digits, _ and -")
        elseif provider_info(d.name) then
          return saved_note("there is already a provider " .. d.name)
        elseif d.model == "" or (d.base_url == "" and d.type == "") then
          return saved_note("a provider needs a model and a URL (or a type)")
        end
        local spec = { model = d.model }
        for _, k in ipairs({ "base_url", "type", "api_key_env" }) do
          if d[k] ~= "" then
            spec[k] = d[k]
          end
        end
        bone.settings.set("providers." .. d.name, spec, function(_, err)
          if err then
            return saved_note(err)
          end
          local function done()
            st.prov = { name = d.name }
            st.sel = 1
            load_providers()
            saved_note(nil, "added " .. d.name)
          end
          if d.key ~= "" then
            bone.request("secrets/set", { provider = d.name, key = d.key }, function(_, kerr)
              if kerr then
                return saved_note("added, but the key was not saved: " .. tostring(kerr))
              end
              done()
            end)
          else
            done()
          end
        end)
      elseif row.action == "use" then
        bone.settings.set("provider", name, function(_, err)
          load_providers()
          saved_note(err, "using " .. name)
        end)
      elseif row.action == "undo" then
        st.ask = { text = "undo your changes to " .. name .. "? (back to core.lua)", action = function()
          bone.settings.reset("providers." .. name, function(_, err)
            load_providers()
            saved_note(err, name .. " is as core.lua has it")
          end)
        end }
      elseif row.action == "delete" then
        st.ask = { text = "delete " .. name .. " and its saved key?", action = function()
          bone.settings.reset("providers." .. name, function(_, err)
            if err then
              return saved_note(err)
            end
            bone.request("secrets/set", { provider = name, key = nil }, function()
              st.prov, st.sel = nil, 1
              load_providers()
              saved_note(nil, "deleted " .. name)
            end)
          end)
        end }
      end
    elseif row.kind == "pfield" then
      if row.type == "boolean" then
        return save_provider_field(row, not row.value)
      elseif row.type == "choice" then
        local i = 1
        for k, c in ipairs(row.choices) do
          if c == row.value then
            i = k
          end
        end
        return save_provider_field(row, row.choices[i % #row.choices + 1])
      elseif not space then
        st.edit = (row.type == "secret" and "") or (row.value == nil and "" or tostring(row.value))
      end
    elseif row.kind == "plugin" or row.kind == "page" then
      if space and row.kind == "plugin" then
        toggle_plugin(row)
      elseif not space and row.page then
        st.tab, st.sel = row.page, 1
      end
    elseif row.type == "boolean" then
      save(row, not row.value)
    elseif row.choices and #row.choices > 0 then
      local i = 1
      for k, c in ipairs(row.choices) do
        if c == row.value then
          i = k
        end
      end
      save(row, row.choices[i % #row.choices + 1])
    elseif not space then
      st.edit = row.value == nil and "" or tostring(row.value)
    end
  end

  local function finish_edit(row)
    local text = st.edit
    st.edit = nil
    local value = text
    if row.kind == "pfield" then
      return save_provider_field(row, text)
    end
    if row.type == "integer" or row.type == "number" then
      value = tonumber(text)
      if not value or (row.type == "integer" and value % 1 ~= 0) then
        st.note = { row.label .. " must be " .. (row.type == "integer" and "a whole number" or "a number"), "ErrorMsg" }
        return
      end
      if (row.min and value < row.min) or (row.max and value > row.max) then
        st.note = { ("%s must be between %s and %s"):format(row.label, row.min or "…", row.max or "…"), "ErrorMsg" }
        return
      end
    end
    save(row, value)
  end

  local function switch_tab(by)
    st.tab = (st.tab - 1 + by) % 3 + 1
    st.sel, st.note, st.edit = 1, nil, nil
  end

  local function on_key(k)
    local list = rows()
    local row = list[st.sel]
    if st.edit then
      if k == "enter" then
        finish_edit(row)
      elseif k == "esc" then
        st.edit = nil
      else
        st.edit = bone.text.edit(st.edit, k) or st.edit
      end
      return true
    end
    if st.ask then
      if k == "y" then
        local action = st.ask.action
        st.ask = nil
        action()
      elseif k == "n" or k == "esc" then
        st.ask = nil
      end
      return true
    end
    st.note = nil
    if st.search then
      -- Typing filters; the arrows move among the matches, enter acts on
      -- one, esc drops the search.
      if k == "esc" then
        st.query, st.search, st.sel = "", false, 1
        return true
      elseif k == "enter" then
        st.search = false
        activate(row, false)
        return true
      elseif k ~= "up" and k ~= "down" and k ~= "pageup" and k ~= "pagedown" and k ~= "wheelup" and k ~= "wheeldown" then
        local q = bone.text.edit(st.query, k)
        if q then st.query, st.sel = q, 1 end
        return true
      end
    elseif k == "/" then
      st.search, st.sel = true, 1
      return true
    elseif k == "esc" and st.query ~= "" then
      st.query, st.sel = "", 1
      return true
    end
    if st.tab > 3 and (k == "esc" or k == "q") then
      local page = st.tab
      st.tab = st.tabs[page].section
      for i, r in ipairs(rows()) do
        if r.page == page then
          st.sel = i
        end
      end
    elseif st.prov and (k == "esc" or k == "q") then
      st.prov, st.sel = nil, 1
    elseif k == "esc" or k == "ctrl+c" or k == "q" then
      opts.close()
    elseif k == "up" or k == "wheelup" or k == "k" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" or k == "wheeldown" or k == "j" then
      st.sel = math.min(math.max(#list, 1), st.sel + 1)
    elseif k == "pageup" then
      st.sel = math.max(1, st.sel - 10)
    elseif k == "pagedown" then
      st.sel = math.min(math.max(#list, 1), st.sel + 10)
    elseif (k == "tab" or k == "right") and not st.prov and st.tab <= 3 then
      switch_tab(1)
    elseif (k == "shift+tab" or k == "backtab" or k == "left") and not st.prov and st.tab <= 3 then
      switch_tab(-1)
    elseif k == "enter" then
      activate(row, false)
    elseif k == "space" then
      activate(row, true)
    elseif k == "r" and row and row.kind == "pfield" and st.prov and not st.prov.new then
      save_provider_field(row, "")
    elseif k == "r" and row and row.path then
      reset(row)
    elseif k == "e" and row and row.kind == "provider" then
      st.prov, st.sel = { name = row.name }, 1
    elseif k == "a" and st.tabs[st.tab].name == "providers" and not st.prov then
      st.prov = { new = true, draft = { name = "", base_url = "", model = "", type = "", key = "", api_key_env = "" } }
      st.sel = 1
    elseif k == "d" and row and row.kind == "provider" then
      if not row.added then
        st.note = { row.name .. " is defined in core.lua; e edits it, and core.lua is where to remove it", "ErrorMsg" }
      else
        local name = row.name
        st.ask = { text = "delete " .. name .. " and its saved key?", action = function()
          bone.settings.reset("providers." .. name, function(_, err)
            if err then
              return saved_note(err)
            end
            bone.request("secrets/set", { provider = name }, function()
              load_providers()
              saved_note(nil, "deleted " .. name)
            end)
          end)
        end }
      end
    else
      return true
    end
    return true
  end

  load_providers()
  load_plugins()
  return {
    st = st,
    rows = rows,
    on_key = on_key,
    activate = activate,
    --- Give a row (an option, a field or a provider's field) `value`, as
    --- a choice picked with the pointer does.
    set = function(row, value)
      if row.kind == "pfield" then
        save_provider_field(row, value)
      else
        save(row, value)
      end
    end,
    switch_tab = switch_tab,
    --- Text pasted while a field is being typed into.
    paste = function(text)
      if st.edit then
        st.edit = st.edit .. (text or ""):gsub("[\r\n]", "")
        redraw()
        return true
      elseif st.search then
        st.query, st.sel = st.query .. (text or ""):gsub("[\r\n]", " "), 1
        redraw()
        return true
      end
      return false
    end,
  }
end

return M
