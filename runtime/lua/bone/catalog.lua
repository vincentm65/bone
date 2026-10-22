-- The plugin catalog: packages to install into ~/.bone/plugins, from
-- settings "catalog.url" (default: the main branch of the bone-catalog
-- repository on GitHub) or a local folder ("~/projects/bone-catalog").
-- The source holds catalog.json, plugins/<name>/<files> and tuis/<name>/<files>
-- (kind "tui": TUI libraries, installed into ~/.bone/libraries); every file is
-- checked against catalog.json's SHA-256 before anything is installed.
--
--   local catalog = require("bone.catalog")
--   catalog.index(function(entries, err) end)   -- with .state for each
--   catalog.install(entry, function(ok, err) end)
--   catalog.remove(name, function(ok, err) end, kind)
--   catalog.open()                              -- the /catalog page
-- The page supports space to select, a to select all available/updated
-- packages, and one confirmation to install the selection.

local M = {}

local OMIT = { history = true, cron = true, themes = true }

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

local function plugins_dir(kind)
  return (bone.config_dir or (os.getenv("HOME") .. "/.bone")) .. (kind == "tui" and "/libraries" or "/plugins")
end

local function installed_names(kind)
  local out = {}
  for _, e in ipairs(bone.fs.list(plugins_dir(kind)) or {}) do
    if e.type == "dir" and e.name:sub(1, 1) ~= "." then
      out[e.name] = true
    end
  end
  return out
end

-- "update" when an installed file differs from the catalog's; for a TUI,
-- which is yours to edit, only when the catalog's version is newer.
local function state_of(entry, installed)
  if not installed[entry.name] then
    return "available"
  end
  local dir = plugins_dir(entry.kind) .. "/" .. entry.name
  if entry.kind == "tui" then
    local ok, m = pcall(bone.json.decode, read_file(dir .. "/manifest.json") or "")
    local have = ok and type(m) == "table" and m.version
    return have and not M.at_least(have, entry.version) and "update" or "installed"
  end
  for _, f in ipairs(entry.files or {}) do
    local text = read_file(dir .. "/" .. f.path)
    if not text or bone.sha256(text) ~= f.sha256 then
      return "update"
    end
  end
  return "installed"
end

--- Which frontends a package's UI runs in: its catalog entry's
--- `frontends` list when it has one, else what its files say (ui.lua is
--- both, tui.lua and gui.lua one each). Returns the list and whether it has
--- a core half.
function M.frontends(entry)
  local core, ui, tui, gui = false, false, false, false
  for _, f in ipairs(entry.files or {}) do
    core = core or f.path == "core.lua"
    ui = ui or f.path == "ui.lua"
    tui = tui or f.path == "tui.lua"
    gui = gui or f.path == "gui.lua"
  end
  if type(entry.frontends) == "table" then
    return entry.frontends, core
  end
  local list = {}
  if ui or tui then list[#list + 1] = "tui" end
  if ui or gui then list[#list + 1] = "gui" end
  return list, core
end

--- A short label of where a package runs: "both", "tui", "gui", or "core".
function M.supports(entry)
  if entry.kind == "tui" then
    return "tui"
  end
  local list, core = M.frontends(entry)
  if #list == 2 then
    return "both"
  end
  return list[1] or (core and "core" or "")
end

--- A package's display name: its catalog title ("Git Diff"), else its name.
function M.label(entry)
  return type(entry.title) == "string" and entry.title ~= "" and entry.title or entry.name
end

--- Whether version `have` is at least `need` ("3.0.2"; a missing `need`
--- always is).
function M.at_least(have, need)
  local a, b = {}, {}
  for n in tostring(have or ""):gmatch("%d+") do
    a[#a + 1] = tonumber(n)
  end
  for n in tostring(need or ""):gmatch("%d+") do
    b[#b + 1] = tonumber(n)
  end
  for i = 1, math.max(#a, #b) do
    if (a[i] or 0) ~= (b[i] or 0) then
      return (a[i] or 0) > (b[i] or 0)
    end
  end
  return true
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
    local installed = { plugin = installed_names(), tui = installed_names("tui") }
    local kept = {}
    for _, e in ipairs(entries) do
      if not OMIT[e.name] then
        e.state = state_of(e, e.kind == "tui" and installed.tui or installed.plugin)
        kept[#kept + 1] = e
      end
    end
    entries = kept
    table.sort(entries, function(a, b)
      return M.label(a):lower() < M.label(b):lower()
    end)
    cb(entries)
  end)
end

-- POSIX tmpname creates a file; Windows only supplies a name.
local function temp_name()
  local ok, tmp, err = pcall(os.tmpname)
  if not ok or not tmp then
    return nil, tostring(err or tmp or "cannot create temporary name")
  end
  local removed, why, code = os.remove(tmp)
  if not removed and code ~= 2 then
    return nil, tostring(why)
  end
  return tmp:match("[^/\\]+$")
end

local function backup_dir()
  local tmp, err = temp_name()
  return tmp and ((bone.config_dir or (os.getenv("HOME") .. "/.bone")) .. "/plugins-backup/" .. tmp), err
end

-- Load, reload or unload a package's halves now: the core's, and this
-- frontend's (its ui.lua, or tui.lua / gui.lua).
local function switch(name, files, on, cb)
  local side = bone.api_info().side
  local has_core, has_tui, tui_error = false, false
  for _, f in ipairs(files or {}) do
    has_core = has_core or f.path == "core.lua"
    has_tui = has_tui or f.path == "ui.lua" or f.path == side .. ".lua"
  end
  if has_tui then
    local loaded = false
    for _, p in ipairs(bone.plugin.list()) do
      loaded = loaded or (p.name == name and p.loaded)
    end
    if on or loaded then
      local action = on and (loaded and bone.plugin.reload or bone.plugin.load) or bone.plugin.unload
      local ok, err = pcall(action, name)
      if not ok then
        return cb(false, tostring(err))
      end
      for _, p in ipairs(bone.plugin.list()) do
        if on and p.name == name and p.error then
          tui_error = p.error
        end
      end
    end
  end
  if not has_core then
    return cb(not tui_error, tui_error)
  end
  bone.request(on and "plugin/load" or "plugin/unload", { name = name }, function(_, err)
    cb(err == nil and not tui_error, err or tui_error)
  end)
end

--- Fetch every file, check each against the catalog, stage them, then put
--- the package in place (a version already there goes to plugins-backup/)
--- and load it. cb(true) or cb(false, err).
function M.install(entry, cb)
  local files = entry.files or {}
  local name = entry.name
  if not M.at_least(bone.version, entry.min_bone_version) then
    return cb(false, ("%s needs bone %s or newer (this is %s); update bone first")
      :format(tostring(name), entry.min_bone_version, bone.version))
  end
  if type(name) ~= "string" or not name:match("^[%w_%-]+$") then
    return cb(false, "bad package name")
  end
  local tmp, err = temp_name()
  if not tmp then
    return cb(false, err)
  end
  local tui = entry.kind == "tui"
  local stage = plugins_dir(entry.kind) .. "/." .. name .. "." .. tmp .. ".staging"
  local ok, err = bone.fs.mkdir(stage)
  if not ok then
    return cb(false, tostring(err))
  end
  local function fail(why)
    local cleaned, e = bone.fs.remove_tree(stage)
    cb(false, tostring(why) .. (cleaned and "" or ("; cannot clean staging: " .. tostring(e))))
  end
  local i = 0
  local function finish()
    local dest, backup = plugins_dir(entry.kind) .. "/" .. name
    if installed_names(entry.kind)[name] then
      local b, err = backup_dir()
      if not b then
        return fail(err)
      end
      backup = b .. "/" .. name
      bone.fs.mkdir(b)
      local moved, e = os.rename(dest, backup)
      if not moved then
        return fail("cannot move the old version aside: " .. tostring(e))
      end
    end
    local moved, e = os.rename(stage, dest)
    if not moved then
      if backup then
        local restored, why = os.rename(backup, dest)
        if not restored then
          e = tostring(e) .. "; cannot restore the old version: " .. tostring(why)
        end
      end
      return fail("cannot install " .. name .. ": " .. tostring(e))
    end
    if tui then
      return cb(true)
    end
    switch(name, files, true, function(loaded, why)
      cb(loaded, why and ("installed, but loading failed: " .. tostring(why)) or nil)
    end)
  end
  local function next_file()
    i = i + 1
    local f = files[i]
    if not f then
      return finish()
    end
    if type(f.path) ~= "string" or f.path:find("%.%.") or f.path:sub(1, 1) == "/" then
      return fail("bad file path in the catalog: " .. tostring(f.path))
    end
    fetch((tui and "tuis/" or "plugins/") .. name .. "/" .. f.path, function(text, err)
      if not text then
        return fail(tostring(err))
      end
      if bone.sha256(text) ~= f.sha256 then
        return fail(name .. "/" .. f.path .. " does not match the catalog (hash)")
      end
      local ok, werr = bone.fs.write(stage .. "/" .. f.path, text)
      if not ok then
        return fail(tostring(werr))
      end
      next_file()
    end)
  end
  next_file()
end

--- Unload a package and move it to plugins-backup/ (a TUI: delete it with
--- bone.library.delete, which keeps a copy). cb(true) or cb(false, err).
function M.remove(name, cb, kind)
  if kind == "tui" then
    local ok, err = pcall(bone.library.delete, name)
    return cb(ok, not ok and tostring(err) or nil)
  end
  local dest = plugins_dir() .. "/" .. name
  if not installed_names()[name] then
    return cb(false, name .. " is not installed")
  end
  local files = {}
  for _, e in ipairs(bone.fs.list(dest) or {}) do
    files[#files + 1] = { path = e.name }
  end
  switch(name, files, false, function(unloaded, err)
    if not unloaded then
      return cb(false, err)
    end
    local b, err = backup_dir()
    if not b then
      return cb(false, err)
    end
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
    page = "plugin", -- "plugin" or "tui"; tab switches
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
      if (e.kind == "tui") == (st.page == "tui") then
        list[#list + 1] = e
      end
    end
    table.sort(list, function(a, b)
      if ORDER[a.state] ~= ORDER[b.state] then
        return ORDER[a.state] < ORDER[b.state]
      end
      return M.label(a):lower() < M.label(b):lower()
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
    out[#out + 1] = {
      { "   " },
      { "Plugins", st.page == "plugin" and "Accent" or "Dim" },
      { "   " },
      { "TUIs", st.page == "tui" and "Accent" or "Dim" },
    }
    out[#out + 1] = ""
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))
    if st.err then
      out[#out + 1] = { { "   error: " .. st.err, "ErrorMsg" } }
    elseif not st.entries then
      out[#out + 1] = { { "   loading…", "Dim" } }
    elseif #list == 0 then
      out[#out + 1] = { { st.page == "tui" and "   no TUIs in the catalog" or "   the catalog is empty", "Dim" } }
    end
    local name_w = 0
    for _, e in ipairs(list) do
      name_w = math.max(name_w, bone.text.width(M.label(e)))
    end
    name_w = name_w + 3
    local room = math.max(math.min(16, (ctx.height or 24) - 10), 3)
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
      prefix[#prefix + 1] = { pad(M.label(e), name_w), row_hl }
      prefix[#prefix + 1] = { pad(e.version or "", 8), muted_hl }
      prefix[#prefix + 1] = { pad(M.supports(e), 6), muted_hl }
      prefix[#prefix + 1] = { bone.text.truncate(e.description or "", math.max(w - name_w - 19, 1)), muted_hl }
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
      hint = "space toggle · enter install selected · a all · n clear · u scan · x remove · tab page · esc close"
    elseif e and e.state == "available" then
      hint = "↑↓ move · space select · enter install · a all · u scan · tab page · esc close"
    elseif e then
      hint = "↑↓ move · space select · enter " .. (e.state == "update" and "update" or "reinstall") .. " · a all · u scan · x remove · tab page · esc close"
    else
      hint = "r refresh · tab page · esc close"
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
          st.progress.failures[#st.progress.failures + 1] = M.label(e)
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
    elseif k == "tab" and not st.busy then
      st.page, st.sel, st.selected = st.page == "tui" and "plugin" or "tui", 1, {}
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
        text = verb .. " " .. M.label(e) .. "? Its Lua runs with your permissions.",
        action = function()
          M.install(e, done(M.label(e) .. " " .. verb .. "ed" .. (e.kind == "tui" and "; /tui use " .. e.name .. " to switch" or "")))
        end,
      }
    elseif k == "x" and e and e.state ~= "available" and not st.busy then
      st.ask = {
        text = "remove " .. M.label(e) .. "? It is moved to ~/.bone/"
          .. (e.kind == "tui" and "libraries/.deleted-…" or "plugins-backup/") .. ".",
        action = function()
          M.remove(e.name, done(M.label(e) .. " removed"), e.kind)
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
