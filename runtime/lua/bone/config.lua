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

  local function redraw()
    if id then
      bone.ui.update(id, {})
    end
  end

  local function load_providers()
    bone.model.list(function(list, err)
      st.providers = list or {}
      if err then
        st.note = { "cannot list providers: " .. tostring(err), "ErrorMsg" }
      end
      redraw()
    end)
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
      local out = {}
      for _, p in ipairs(st.providers or {}) do
        out[#out + 1] = { kind = "provider", name = p.name, label = p.name, model = p.model, current = p.current, ptype = p.type }
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
    if row.kind == "provider" then
      if text == "" then
        return bone.settings.reset("models." .. row.name, function(_, err)
          load_providers()
          saved_note(err, row.name .. ": the model from core.lua")
        end)
      end
      return bone.settings.set("models." .. row.name, text, function(_, err)
        load_providers()
        saved_note(err, row.name .. " uses " .. text)
      end)
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

  -- A row's value as plain text.
  local function value_text(row)
    if row.kind == "provider" then
      return pad(row.model or "", 30) .. (row.current and "in use" or "")
    elseif row.kind == "plugin" then
      local halves = (row.tui and "tui" or "") .. (row.tui and row.core and " + " or "") .. (row.core and "core" or "")
      return pad(row.on and "on" or "off", 6) .. halves .. (row.error and ("  " .. row.error) or "")
    elseif row.type == "boolean" then
      return row.value and "on" or "off"
    elseif row.choices and #row.choices > 0 then
      -- Every choice, the current one in brackets.
      local out = {}
      for _, c in ipairs(row.choices) do
        out[#out + 1] = c == row.value and ("[" .. c .. "]") or c
      end
      return table.concat(out, " ")
    end
    return row.value == nil and "(unset)" or tostring(row.value)
  end

  -- Plain text only: a full-width pane resting on the prompt.
  local function render(ctx)
    local w = math.max(ctx.width, 30)
    local t = st.tabs[st.tab]
    local list = rows()
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))

    local out = { { { fill = "─", hl = "Normal" } } }
    -- The tabs, the current one in brackets, wrapped to the width.
    local line = " Settings  "
    for i, tab in ipairs(st.tabs) do
      local label = i == st.tab and ("[" .. tab.title .. "]") or (" " .. tab.title .. " ")
      if text_width(line) + text_width(label) + 1 > w then
        out[#out + 1] = line
        line = "           "
      end
      line = line .. label .. " "
    end
    out[#out + 1] = line
    out[#out + 1] = ""

    local label_w = 0
    for _, r in ipairs(list) do
      label_w = math.max(label_w, text_width(r.label))
    end
    label_w = math.min(label_w + 3, math.floor(w / 2))

    -- At most this many rows; the list scrolls with the selection.
    local room = math.max(math.min(14, (ctx.height or 24) - 9), 3)
    if t.name == "providers" and not st.providers then
      out[#out + 1] = "   loading…"
    elseif #list == 0 then
      out[#out + 1] = "   " .. (t.name == "plugins" and "no plugins in ~/.bone/plugins" or "nothing to set here")
    end
    local first = math.max(1, st.sel - room + 1)
    for i = first, math.min(#list, first + room - 1) do
      local r = list[i]
      local text = (i == st.sel and " › " or "   ") .. pad(r.label, label_w)
      if i == st.sel and st.edit then
        text = text .. st.edit .. "▏"
      else
        text = text .. value_text(r)
        -- tui.lua runs last; say so when it overrides the saved value.
        if r.kind == "option" then
          local saved = bone.settings.get(r.path)
          if saved ~= nil and saved ~= r.value then
            text = text .. "   (tui.lua sets " .. tostring(r.value) .. ")"
          end
        end
      end
      out[#out + 1] = bone.text.truncate(text, w)
    end
    if #list > room then
      out[#out + 1] = ("   %d–%d of %d"):format(first, math.min(#list, first + room - 1), #list)
    end

    out[#out + 1] = ""
    local current = list[st.sel]
    local note = st.note and ((st.note[2] == "ErrorMsg" and "error: " or "") .. st.note[1])
      or (current and current.desc or "")
    out[#out + 1] = bone.text.truncate("   " .. note, w)
    local hint
    if st.edit then
      hint = "enter save · esc cancel"
    elseif t.name == "providers" then
      hint = "↑↓ move · enter use · e model · tab section · esc close"
    elseif t.name == "plugins" then
      hint = "↑↓ move · space on/off · tab section · esc close"
    else
      hint = "↑↓ move · enter change · r reset · tab section · esc close"
    end
    out[#out + 1] = bone.text.truncate("   " .. hint, w)
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
    st.note = nil
    if k == "esc" or k == "ctrl+c" or k == "q" then
      bone.ui.close(id)
    elseif k == "up" or k == "wheelup" or k == "k" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" or k == "wheeldown" or k == "j" then
      st.sel = math.min(math.max(#list, 1), st.sel + 1)
    elseif k == "pageup" then
      st.sel = math.max(1, st.sel - 10)
    elseif k == "pagedown" then
      st.sel = math.min(math.max(#list, 1), st.sel + 10)
    elseif k == "tab" or k == "right" then
      switch_tab(1)
    elseif k == "shift+tab" or k == "backtab" or k == "left" then
      switch_tab(-1)
    elseif k == "enter" then
      activate(row, false)
    elseif k == "space" then
      activate(row, true)
    elseif k == "r" and row and row.path then
      reset(row)
    elseif k == "e" and row and row.kind == "provider" then
      st.edit = row.model or ""
    else
      return true
    end
    return true
  end

  load_providers()
  load_plugins()
  id = bone.ui.popup({ lines = render, on_key = on_key, anchor = "prompt", width = false })
  return id
end

return M
