-- The plugin catalog: packages to install into ~/.bone/plugins, from
-- settings "catalog.url" (default: the main branch of the bone-catalog
-- repository on GitHub) or a local folder ("~/projects/bone-catalog").
-- The source holds catalog.json and plugins/<name>/<files>; every file is
-- checked against catalog.json's SHA-256 before anything is installed.
--
--   local catalog = require("bone.catalog")
--   catalog.index(function(entries, err) end)   -- with .state for each
--   catalog.install(entry, function(ok, err) end)
--   catalog.remove(name, function(ok, err) end)
--   catalog.open()                              -- the /catalog page
-- The page supports space to select, a to select all available/updated
-- packages, and one confirmation to install the selection.

local M = {}

local OMIT = { history = true, cron = true, usage = true, themes = true }

M.DEFAULT_URL = "https://raw.githubusercontent.com/vincentm65/bone-catalog/refs/heads/main"

local function home(path)
  if path:sub(1, 2) == "~/" then
    return (os.getenv("HOME") or "") .. path:sub(2)
  end
  return path
end

--- Where packages come from: a URL, or a local folder.
function M.source()
  local s = bone.settings.get("catalog.url")
  return type(s) == "string" and s ~= "" and s or M.DEFAULT_URL
end

local function local_dir(source)
  local dir = source:match("^file://(.+)$") or source
  if dir:sub(1, 1) == "/" or dir:sub(1, 2) == "~/" then
    return home(dir)
  end
end

local function read_file(path)
  local f = io.open(path, "rb")
  if not f then
    return nil
  end
  local text = f:read("*a")
  f:close()
  return text
end

-- One file of the source: cb(text) or cb(nil, err).
local function fetch(rel, cb)
  local source = M.source()
  local dir = local_dir(source)
  if dir then
    local text = read_file(dir .. "/" .. rel)
    if text then
      return cb(text)
    end
    return cb(nil, "cannot read " .. dir .. "/" .. rel)
  end
  bone.http({ url = source:gsub("/$", "") .. "/" .. rel, timeout = 20000 }, function(res, err)
    if not res then
      return cb(nil, tostring(err))
    end
    if res.status < 200 or res.status >= 300 then
      return cb(nil, rel .. ": HTTP " .. tostring(res.status))
    end
    cb(res.body or "")
  end)
end

local function plugins_dir()
  return (bone.config_dir or (os.getenv("HOME") .. "/.bone")) .. "/plugins"
end

local function installed_names()
  local out = {}
  for _, e in ipairs(bone.fs.list(plugins_dir()) or {}) do
    if e.type == "dir" and e.name:sub(1, 1) ~= "." then
      out[e.name] = true
    end
  end
  return out
end

-- "update" when an installed file differs from the catalog's.
local function state_of(entry, installed)
  if not installed[entry.name] then
    return "available"
  end
  for _, f in ipairs(entry.files or {}) do
    local text = read_file(plugins_dir() .. "/" .. entry.name .. "/" .. f.path)
    if not text or bone.sha256(text) ~= f.sha256 then
      return "update"
    end
  end
  return "installed"
end

--- The catalog's packages, each with .state: "available", "installed" or
--- "update". cb(entries) or cb(nil, err).
function M.index(cb)
  fetch("catalog.json", function(text, err)
    if not text then
      return cb(nil, "cannot get the catalog from " .. M.source() .. ": " .. tostring(err))
    end
    local ok, entries = pcall(bone.json.decode, text)
    if not ok or type(entries) ~= "table" then
      return cb(nil, "the catalog's catalog.json is not valid")
    end
    local installed = installed_names()
    local kept = {}
    for _, e in ipairs(entries) do
      if not OMIT[e.name] then
        e.state = state_of(e, installed)
        kept[#kept + 1] = e
      end
    end
    entries = kept
    table.sort(entries, function(a, b)
      return a.name < b.name
    end)
    cb(entries)
  end)
end

local function backup_dir()
  return (bone.config_dir or (os.getenv("HOME") .. "/.bone")) .. "/plugins-backup/" .. os.time()
end

-- Load (or unload) a package's halves now, and keep it out of (or in)
-- settings' plugins.disabled.
local function switch(name, files, on, cb)
  local has_core, has_tui = false, false
  for _, f in ipairs(files or {}) do
    has_core = has_core or f.path == "core.lua"
    has_tui = has_tui or f.path == "tui.lua"
  end
  if has_tui then
    pcall(on and bone.plugin.load or bone.plugin.unload, name)
  end
  if not has_core then
    return cb(true)
  end
  bone.request(on and "plugin/load" or "plugin/unload", { name = name }, function(_, err)
    cb(err == nil, err)
  end)
end

--- Fetch every file, check each against the catalog, stage them, then put
--- the package in place (a version already there goes to plugins-backup/)
--- and load it. cb(true) or cb(false, err).
function M.install(entry, cb)
  local files = entry.files or {}
  local name = entry.name
  if type(name) ~= "string" or not name:match("^[%w_%-]+$") then
    return cb(false, "bad package name")
  end
  local stage = plugins_dir() .. "/." .. name .. ".staging"
  local i = 0
  local function finish()
    local dest = plugins_dir() .. "/" .. name
    if installed_names()[name] then
      local b = backup_dir()
      bone.fs.mkdir(b)
      local ok, err = os.rename(dest, b .. "/" .. name)
      if not ok then
        return cb(false, "cannot move the old version aside: " .. tostring(err))
      end
    end
    local ok, err = os.rename(stage, dest)
    if not ok then
      return cb(false, "cannot install " .. name .. ": " .. tostring(err))
    end
    switch(name, files, true, function(_, e)
      cb(true, e and ("installed, but loading failed: " .. tostring(e)) or nil)
    end)
  end
  local function next_file()
    i = i + 1
    local f = files[i]
    if not f then
      return finish()
    end
    if type(f.path) ~= "string" or f.path:find("%.%.") or f.path:sub(1, 1) == "/" then
      return cb(false, "bad file path in the catalog: " .. tostring(f.path))
    end
    fetch("plugins/" .. name .. "/" .. f.path, function(text, err)
      if not text then
        return cb(false, tostring(err))
      end
      if bone.sha256(text) ~= f.sha256 then
        return cb(false, name .. "/" .. f.path .. " does not match the catalog (hash)")
      end
      local ok, werr = bone.fs.write(stage .. "/" .. f.path, text)
      if not ok then
        return cb(false, tostring(werr))
      end
      next_file()
    end)
  end
  next_file()
end

--- Unload a package and move it to plugins-backup/. cb(true) or cb(false, err).
function M.remove(name, cb)
  local dest = plugins_dir() .. "/" .. name
  if not installed_names()[name] then
    return cb(false, name .. " is not installed")
  end
  local files = {}
  for _, e in ipairs(bone.fs.list(dest) or {}) do
    files[#files + 1] = { path = e.name }
  end
  switch(name, files, false, function()
    local b = backup_dir()
    bone.fs.mkdir(b)
    local ok, err = os.rename(dest, b .. "/" .. name)
    cb(ok and true or false, ok and nil or tostring(err))
  end)
end

-- ---- the /catalog page -------------------------------------------------------

local function pad(s, w)
  s = tostring(s or "")
  local n = bone.text.width(s)
  if n >= w then
    return bone.text.truncate(s, w)
  end
  return s .. string.rep(" ", w - n)
end

function M.open()
  local st = {
    entries = nil,
    err = nil,
    sel = 1,
    selected = {},
    note = nil,
    ask = nil,
    busy = false,
    progress = nil,
  }
  local id

  local function redraw()
    if id then
      bone.ui.update(id, {})
    end
  end

  local function load(note)
    st.entries, st.err = nil, nil
    if note then
      st.note = { note }
    end
    M.index(function(entries, err)
      st.entries, st.err = entries, err
      if entries then
        local present = {}
        for _, e in ipairs(entries) do
          present[e.name] = true
        end
        for name in pairs(st.selected) do
          if not present[name] then
            st.selected[name] = nil
          end
        end
      end
      redraw()
    end)
  end

  local ORDER = { update = 1, installed = 2, available = 3 }
  local HEAD = { update = "Updates", installed = "Installed", available = "Available" }
  local function rows()
    local list = {}
    for _, e in ipairs(st.entries or {}) do
      list[#list + 1] = e
    end
    table.sort(list, function(a, b)
      if ORDER[a.state] ~= ORDER[b.state] then
        return ORDER[a.state] < ORDER[b.state]
      end
      return a.name < b.name
    end)
    return list
  end

  local function selected_rows(list)
    local out = {}
    for _, e in ipairs(list) do
      if st.selected[e.name] then
        out[#out + 1] = e
      end
    end
    return out
  end

  local function actionable(e)
    return e.state == "available" or e.state == "update"
  end

  local function render(ctx)
    local w = math.max(ctx.width, 30)
    local out = { { { fill = "─", hl = "WinSeparator" } } }
    local list = rows()
    local selected = selected_rows(list)
    local updates = 0
    for _, item in ipairs(list) do
      if item.state == "update" then
        updates = updates + 1
      end
    end
    out[#out + 1] = {
      { " Catalog  ", "Accent" },
      { bone.text.truncate(M.source(), w - 11), "Dim" },
    }
    out[#out + 1] = {
      { (" Updates: %d"):format(updates), "Accent" },
      { (" · Selected: %d"):format(#selected), "Dim" },
    }
    out[#out + 1] = ""
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))
    if st.err then
      out[#out + 1] = { { "   error: " .. st.err, "ErrorMsg" } }
    elseif not st.entries then
      out[#out + 1] = { { "   loading…", "Dim" } }
    elseif #list == 0 then
      out[#out + 1] = { { "   the catalog is empty", "Dim" } }
    end
    local name_w = 0
    for _, e in ipairs(list) do
      name_w = math.max(name_w, bone.text.width(e.name))
    end
    name_w = name_w + 3
    local room = math.max(math.min(16, (ctx.height or 24) - 9), 3)
    local first = math.max(1, st.sel - room + 1)
    local last_state
    for i = first, math.min(#list, first + room - 1) do
      local e = list[i]
      if e.state ~= last_state then
        out[#out + 1] = { { "   " .. HEAD[e.state], "MdHeading" } }
        last_state = e.state
      end
      local cursor = i == st.sel and "›" or " "
      local check = st.selected[e.name] and "x" or " "
      local row_hl = i == st.sel and "Selection" or "Normal"
      local muted_hl = i == st.sel and "Selection" or "Dim"
      local prefix = { { cursor, i == st.sel and "Accent" or "Dim" }, { "[", "Dim" } }
      prefix[#prefix + 1] = { check, st.selected[e.name] and "Accent" or "Dim" }
      prefix[#prefix + 1] = { "] ", "Dim" }
      prefix[#prefix + 1] = { pad(e.name, name_w), row_hl }
      prefix[#prefix + 1] = { pad(e.version or "", 8), muted_hl }
      prefix[#prefix + 1] = { bone.text.truncate(e.description or "", math.max(w - name_w - 13, 1)), muted_hl }
      out[#out + 1] = prefix
    end
    out[#out + 1] = ""
    local e = list[st.sel]
    local note = st.ask and st.ask.text
      or (st.note and ((st.note[2] and "error: " or "") .. st.note[1]))
      or (st.progress and ("installing %d/%d…"):format(st.progress.done, st.progress.total))
      or (st.busy and "working…")
      or ""
    out[#out + 1] = { { bone.text.truncate("   " .. note, w), st.ask and "Accent" or "Dim" } }
    local hint
    if st.ask then
      hint = "y yes · n no"
    elseif st.busy then
      hint = "working…"
    elseif e and #selected > 0 then
      hint = "space toggle · enter install selected · a all · n clear · u scan · x remove · esc close"
    elseif e and e.state == "available" then
      hint = "↑↓ move · space select · enter install · a all · u scan · esc close"
    elseif e then
      hint = "↑↓ move · space select · enter " .. (e.state == "update" and "update" or "reinstall") .. " · a all · u scan · x remove · esc close"
    else
      hint = "r refresh · esc close"
    end
    out[#out + 1] = { { "   " .. hint, "Dim" } }
    return out
  end

  local function done(what)
    return function(ok, err)
      st.busy = false
      st.progress = nil
      if ok then
        st.note = { what .. (err and (" (" .. err .. ")") or "") }
      else
        st.note = { tostring(err), true }
      end
      load()
    end
  end

  local function install_selected(entries)
    st.busy = true
    st.progress = { done = 0, total = #entries, failures = {} }
    local remaining = #entries
    local function finished()
      local p = st.progress
      st.busy, st.progress = false, nil
      st.selected = {}
      if #p.failures == 0 then
        st.note = { ("installed %d package%s"):format(p.total, p.total == 1 and "" or "s") }
      else
        st.note = { ("installed %d/%d; failed: %s"):format(p.total - #p.failures, p.total, table.concat(p.failures, ", ")), true }
      end
      load(st.note[1])
    end
    for _, e in ipairs(entries) do
      redraw()
      M.install(e, function(ok)
        if not ok then
          st.progress.failures[#st.progress.failures + 1] = e.name
        end
        st.progress.done = st.progress.done + 1
        remaining = remaining - 1
        if remaining == 0 then
          finished()
        end
      end)
    end
  end

  local function on_key(k)
    local list = rows()
    local e = list[st.sel]
    if st.ask then
      if k == "y" then
        local action = st.ask.action
        st.ask, st.busy = nil, true
        action()
      elseif k == "n" or k == "esc" then
        st.ask = nil
      end
      return true
    end
    st.note = nil
    if k == "esc" or k == "q" or k == "ctrl+c" then
      bone.ui.close(id)
    elseif k == "up" or k == "k" or k == "wheelup" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" or k == "j" or k == "wheeldown" then
      st.sel = math.min(math.max(#list, 1), st.sel + 1)
    elseif (k == "r" or k == "u") and not st.busy then
      load(k == "u" and "scanning for updates…" or nil)
    elseif k == "space" and e and not st.busy then
      st.selected[e.name] = not st.selected[e.name] or nil
    elseif k == "a" and not st.busy then
      for _, item in ipairs(list) do
        if actionable(item) then
          st.selected[item.name] = true
        end
      end
      st.note = { "selected available and updated packages" }
    elseif k == "n" and not st.busy then
      st.selected = {}
      st.note = { "selection cleared" }
    elseif k == "enter" and e and not st.busy then
      local chosen = selected_rows(list)
      if #chosen > 0 then
        st.ask = {
          text = ("install %d selected package%s? Its Lua runs with your permissions."):format(#chosen, #chosen == 1 and "" or "s"),
          action = function()
            install_selected(chosen)
          end,
        }
        return true
      end
      local verb = e.state == "available" and "install" or (e.state == "update" and "update" or "reinstall")
      st.ask = {
        text = verb .. " " .. e.name .. "? Its Lua runs with your permissions.",
        action = function()
          M.install(e, done(e.name .. " " .. verb .. "ed"))
        end,
      }
    elseif k == "x" and e and e.state ~= "available" and not st.busy then
      st.ask = {
        text = "remove " .. e.name .. "? It is moved to ~/.bone/plugins-backup/.",
        action = function()
          M.remove(e.name, done(e.name .. " removed"))
        end,
      }
    end
    return true
  end

  load()
  id = bone.ui.popup({ lines = render, on_key = on_key, anchor = "prompt" })
  return id
end

return M
