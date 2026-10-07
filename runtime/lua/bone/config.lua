-- /config: the settings page. Tabs for the TUI's options, the providers,
-- the plugins and each plugin's own settings; every change is saved in
-- settings.json (through the core) and applies at once.
--
-- A plugin gets a tab by declaring its settings, either in its
-- manifest.json ("settings": [ ... ]) or from its tui.lua with
-- bone.settings.page{ name, title, fields = { ... } }. A field is
-- { key, label, type = "boolean" | "number" | "integer" | "string",
-- choices, default, min, max, desc }; its value is saved as
-- "<name>.<key>" and read with bone.settings.get.
--
-- require("bone.config").open(tab) opens the page; tab is a tab's name or
-- title ("providers", "web_search").

local M = {}

local CHAR = "^[%z\1-\127\194-\244][\128-\191]*$"

local function text_width(s)
  return bone.text.width(s)
end

local function pad(s, w)
  s = tostring(s)
  local n = text_width(s)
  if n >= w then
    return bone.text.truncate(s, w)
  end
  return s .. string.rep(" ", w - n)
end

-- ---- what the tabs hold ----------------------------------------------------

-- The TUI's options: built-in ones and those the runtime and plugins define.
local function option_rows()
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
        path = "tui." .. name,
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

-- Settings pages declared in the plugins' manifest.json files.
local function manifest_pages()
  local pages = {}
  local dir = bone.config_dir and (bone.config_dir .. "/plugins")
  for _, e in ipairs(dir and bone.fs.list(dir) or {}) do
    if e.type == "dir" then
      local f = io.open(dir .. "/" .. e.name .. "/manifest.json")
      if f then
        local text = f:read("*a")
        f:close()
        local ok, m = pcall(bone.json.decode, text)
        if ok and type(m) == "table" and type(m.settings) == "table" and #m.settings > 0 then
          pages[#pages + 1] = { name = e.name, title = m.title or e.name, fields = m.settings }
        end
      end
    end
  end
  return pages
end

local function tabs()
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
  for _, p in ipairs(manifest_pages()) do
    declared[#declared + 1] = p
  end
  table.sort(declared, function(a, b)
    return tostring(a.title):lower() < tostring(b.title):lower()
  end)
  for _, p in ipairs(declared) do
    if not seen[p.name] then
      seen[p.name] = true
      out[#out + 1] = { name = p.name, title = p.title, page = p }
    end
  end
  return out
end

-- ---- the page ----------------------------------------------------------------

function M.open(want)
  local st = {
    tabs = tabs(),
    tab = 1,
    sel = 1,
    edit = nil, -- the text being typed into a field
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
  local id
  local paste_event

  local function redraw()
    if id then
      bone.ui.update(id, {})
    end
  end

  local save_provider_field
  local function load_providers()
    bone.model.list(function(list, err)
      st.providers = list or {}
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

  local EFFORTS = { "default", "low", "medium", "high" }

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
        value = p.reasoning_effort or "default", changed = changed("reasoning_effort") },
      { kind = "pfield", key = "supports_images", label = "Accept images", type = "boolean", value = p.supports_images ~= false,
        changed = changed("supports_images"), desc = "allow screenshots for a vision-enabled model; disable for text-only models" },
      { kind = "pfield", key = "stream_usage", label = "Stream usage", type = "boolean", value = p.stream_usage ~= false,
        changed = changed("stream_usage"), desc = "ask for token usage while streaming (some servers refuse it)" },
      { kind = "pfield", key = "key", label = "API key", type = "secret", value = key_text,
        desc = "enter types a new key (saved in ~/.bone/secrets.json, readable only by you); empty removes the saved one" },
      { kind = "pfield", key = "api_key_env", label = "Key variable", type = "string", value = env,
        desc = "an environment variable that holds the key" },
      { kind = "pfield", key = "replay_reasoning", label = "Replay reasoning", type = "boolean", value = p.replay_reasoning == true,
        changed = changed("replay_reasoning"), desc = "send stored reasoning_content; enable only for compatible endpoints (e.g. Qwen)" },
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
        by[name] = { kind = "plugin", name = name, label = name, tui = false, core = false }
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
    for _, r in ipairs(rows) do
      r.on = not off[r.name]
    end
    table.sort(rows, function(a, b)
      return a.name < b.name
    end)
    return rows
  end

  local function rows()
    local t = st.tabs[st.tab]
    if t.name == "general" then
      return option_rows()
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
  -- An empty text, or "default", goes back to core.lua's value.
  function save_provider_field(row, value)
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
    if value == "" or value == "default" then
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
        saved_note(err, "using " .. row.name)
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
    elseif row.kind == "plugin" then
      toggle_plugin(row)
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

  -- Match the catalog's restrained palette: Accent is the theme's blue, while
  -- Dim carries secondary values and help text in a quiet grey.
  local MUTED, KEY, NOTE = "Dim", "Accent", "Dim"

  -- A line of { text, group } items, cut to the width as it grows.
  local function line(w, selected)
    local items, used = {}, 0
    local function add(text, hl)
      text = tostring(text)
      if used >= w or text == "" then
        return
      end
      if used + text_width(text) > w then
        text = bone.text.truncate(text, w - used)
      end
      hl = hl or "Normal"
      items[#items + 1] = { text, selected and ("ConfigSelected" .. hl) or hl }
      used = used + text_width(text)
    end
    return items, add
  end

  -- Keep the current choice identifiable even without color.
  local function choices_into(add, list, value)
    for i, c in ipairs(list) do
      if i > 1 then
        add(" · ", MUTED)
      end
      add(c == value and ("[" .. tostring(c) .. "]") or tostring(c), c == value and KEY or MUTED)
    end
  end

  local function switch_into(add, on)
    if on then
      add("● on", KEY)
    else
      add("○ off", MUTED)
    end
  end

  -- A row's value, added to a line.
  local function value_into(add, row)
    if row.kind == "provider" then
      add(pad(row.model or "", 22))
      add(pad(row.url or "", 30), MUTED)
      if row.current then
        add("  ● in use  ", KEY)
      end
      if row.added then
        add("added", MUTED)
      end
    elseif row.kind == "paction" then
      return
    elseif row.kind == "pfield" then
      if row.type == "boolean" then
        switch_into(add, row.value)
      elseif row.type == "choice" then
        choices_into(add, row.choices, row.value)
      elseif row.type == "secret" and st.prov and st.prov.new then
        if row.value ~= "" then
          add(string.rep("•", math.min(#row.value, 24)))
        else
          add("none", MUTED)
        end
      elseif row.key == "type" and (row.value == nil or row.value == "") then
        add("OpenAI-compatible")
      elseif row.value == nil or row.value == "" then
        add("none", MUTED)
      else
        add(tostring(row.value))
      end
      if row.changed then
        add("   changed", NOTE)
      end
    elseif row.kind == "plugin" then
      switch_into(add, row.on)
      add(row.on and "    " or "   ")
      add((row.tui and "tui" or "") .. (row.tui and row.core and " + " or "") .. (row.core and "core" or ""), MUTED)
      if row.error then
        add("  " .. row.error, "ErrorMsg")
      end
    elseif row.type == "boolean" then
      switch_into(add, row.value)
    elseif row.choices and #row.choices > 0 then
      choices_into(add, row.choices, row.value)
    elseif row.value == nil then
      add("unset", MUTED)
    else
      add(tostring(row.value))
    end
  end

  -- Key hints: the key bold, what it does dim.
  local function hints_into(add, pairs_)
    add("   ")
    for i, h in ipairs(pairs_) do
      if i > 1 then
        add("   ")
      end
      add(h[1], KEY)
      add(" " .. h[2], MUTED)
    end
  end

  -- A full-width pane resting on the prompt.
  local function render(ctx)
    local w = math.max(ctx.width, 1)
    -- Preserve each span's foreground/emphasis on a full-row selection background.
    -- Refresh on render so changing the colorscheme also updates an open page.
    local selection = bone.hl.get("Selection") or {}
    for _, group in ipairs({ "Normal", "Dim", "Accent", "ErrorMsg" }) do
      local style = bone.hl.get(group) or {}
      style.bg = selection.bg
      local name = "ConfigSelected" .. group
      local current = bone.hl.get(name) or {}
      for _, key in ipairs({ "fg", "bg", "bold", "italic", "underline", "reverse", "dim" }) do
        if current[key] ~= style[key] then
          bone.hl.set(name, style)
          break
        end
      end
    end
    local t = st.tabs[st.tab]
    local list = rows()
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))

    local out = {}
    -- The title in the top rule.
    local title = st.prov and (st.prov.new and "new provider" or st.prov.name)
    local top = { { "── ", "WinSeparator" }, { "Settings", "Accent" } }
    if title then
      top[#top + 1] = { " › ", MUTED }
      top[#top + 1] = { title, "Accent" }
    end
    top[#top + 1] = { " ", MUTED }
    top[#top + 1] = { fill = "─", hl = "WinSeparator" }
    out[1] = top

    -- The tabs, the current one bold and underlined, wrapped to the width.
    if #st.tabs > 1 then
      local items, used = { { "  " } }, 2
      for i, tab in ipairs(st.tabs) do
        local n = text_width(tab.title) + 3
        if used + n > w then
          out[#out + 1] = items
          items, used = { { "  " } }, 2
        end
        items[#items + 1] = { " " }
        items[#items + 1] = { tab.title, i == st.tab and "Accent" or "Dim" }
        items[#items + 1] = { "  " }
        used = used + n
      end
      out[#out + 1] = items
    end
    out[#out + 1] = ""

    local label_w = 0
    for _, r in ipairs(list) do
      label_w = math.max(label_w, text_width(r.label))
    end
    label_w = math.min(label_w + 4, math.floor(w / 2))

    -- Reserve ten slots on every page (fewer on short terminals), plus a
    -- permanent footer/counter slot so empty and overflowing pages stay level.
    local room = math.max(1, math.min(10, (ctx.height or 24) - #out - 5))
    local body_start = #out
    if t.name == "providers" and not st.providers then
      out[#out + 1] = { { "    loading…", MUTED } }
    elseif #list == 0 then
      out[#out + 1] = { { "    " .. (t.name == "plugins" and "no plugins in ~/.bone/plugins" or "nothing to set here"), MUTED } }
    end
    local first = math.max(1, st.sel - room + 1)
    for i = first, math.min(#list, first + room - 1) do
      local r = list[i]
      local items, add = line(w, i == st.sel)
      if i == st.sel then
        add("  › ", KEY)
        add(pad(r.label, label_w))
      else
        add("    " .. pad(r.label, label_w))
      end
      if i == st.sel and st.edit then
        add(r.type == "secret" and string.rep("•", #st.edit) or st.edit)
        add("▏")
      else
        value_into(add, r)
        -- tui.lua runs last; say so when it overrides the saved value.
        if r.kind == "option" then
          local saved = bone.settings.get(r.path)
          if saved ~= nil and saved ~= r.value then
            add("   tui.lua sets " .. tostring(r.value), NOTE)
          end
        end
      end
      if i == st.sel then
        items[#items + 1] = { fill = " ", hl = "ConfigSelectedNormal" }
      end
      out[#out + 1] = items
    end
    while #out < body_start + room do
      out[#out + 1] = ""
    end
    if #list > room then
      out[#out + 1] = { { ("    %d–%d of %d"):format(first, math.min(#list, first + room - 1), #list), MUTED } }
    else
      out[#out + 1] = ""
    end

    out[#out + 1] = ""
    local current = list[st.sel]
    local items, add = line(w)
    if st.ask then
      add("    " .. st.ask.text, KEY)
    elseif st.note and st.note[2] == "ErrorMsg" then
      add("    error: " .. st.note[1], "ErrorMsg")
    elseif st.note then
      add("    " .. st.note[1])
    else
      add("    " .. (current and current.desc or ""), NOTE)
    end
    out[#out + 1] = items
    out[#out + 1] = ""
    local hint
    if st.ask then
      hint = { { "y", "yes" }, { "n", "no" } }
    elseif st.edit then
      hint = { { "enter", "save" }, { "esc", "cancel" } }
    elseif t.name == "providers" and st.prov then
      hint = { { "↑↓", "move" }, { "enter", "change" }, { "r", "reset to core.lua" }, { "esc", "back" } }
    elseif t.name == "providers" then
      hint = { { "↑↓", "move" }, { "enter", "use" }, { "e", "edit" }, { "a", "add" }, { "d", "delete" }, { "tab", "section" }, { "esc", "close" } }
    elseif t.name == "plugins" then
      hint = { { "↑↓", "move" }, { "space", "on/off" }, { "tab", "section" }, { "esc", "close" } }
    else
      hint = { { "↑↓", "move" }, { "enter", "change" }, { "r", "reset" }, { "tab", "section" }, { "esc", "close" } }
    end
    items, add = line(w)
    hints_into(add, hint)
    out[#out + 1] = items
    return out
  end

  local function switch_tab(by)
    st.tab = (st.tab - 1 + by) % #st.tabs + 1
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
      elseif k == "backspace" then
        st.edit = st.edit:gsub("[%z\1-\127\194-\244][\128-\191]*$", "")
      elseif k == "ctrl+u" then
        st.edit = ""
      elseif k == "space" then
        st.edit = st.edit .. " "
      elseif k:match(CHAR) then
        st.edit = st.edit .. k
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
    if st.prov and (k == "esc" or k == "q") then
      st.prov, st.sel = nil, 1
    elseif k == "esc" or k == "ctrl+c" or k == "q" then
      if paste_event then
        bone.off(paste_event)
        paste_event = nil
      end
      bone.ui.close(id)
    elseif k == "up" or k == "wheelup" or k == "k" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" or k == "wheeldown" or k == "j" then
      st.sel = math.min(math.max(#list, 1), st.sel + 1)
    elseif k == "pageup" then
      st.sel = math.max(1, st.sel - 10)
    elseif k == "pagedown" then
      st.sel = math.min(math.max(#list, 1), st.sel + 10)
    elseif (k == "tab" or k == "right") and not st.prov then
      switch_tab(1)
    elseif (k == "shift+tab" or k == "backtab" or k == "left") and not st.prov then
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
  id = bone.ui.popup({ lines = render, on_key = on_key, anchor = "prompt", width = false })
  paste_event = bone.on("paste", function(ev)
    if ev.context == "popup" and st.edit then
      st.edit = st.edit .. (ev.text or ""):gsub("[\r\n]", "")
      redraw()
    end
  end)
  return id
end

return M
