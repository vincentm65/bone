-- The built-in slash commands, as Lua. Each is an ordinary bone.cmd command:
-- replace one with bone.cmd.create, remove it with bone.cmd.del, or give a
-- library of yours lua/bone/commands.lua, starting from bone.builtin.

local cmd = bone.cmd.create

local function error_text(e)
  return (tostring(e):gsub("^runtime error: ", ""):gsub("\nstack traceback:.*$", ""))
end

cmd("help", function(c)
  return bone.ui.help(c.args)
end, { desc = "commands and keys; /help topic searches the docs", aliases = { "?" } })

cmd("health", function()
  bone.ui.health()
end, { desc = "check the setup: provider, terminal, clipboard, Lua", aliases = { "checkhealth" } })

cmd("new", function()
  bone.action("new_session")
end, { desc = "start a new session", aliases = { "clear" } })

cmd("archive", function()
  require("bone.ui.sessions").archive_current()
  bone.action("new_session")
end, { desc = "archive this conversation and start a new session" })

cmd("sessions", function()
  bone.action("sessions")
end, { desc = "pick a session to open (ctrl+o)", aliases = { "resume" } })

cmd("where", function()
  local s = bone.api.session()
  if s then
    bone.notify((s.title or "untitled") .. " — " .. s.cwd)
  else
    bone.notify("new session (not started yet) — " .. bone.api.cwd())
  end
end, { desc = "show the current session and directory" })

-- ---- /cd: the directory this session works in ------------------------------
-- /cd moves the session on screen: its tools run in the new directory from
-- its next turn on (the core tells the model where the change was), and new
-- sessions start there too. With none started yet it only names where the
-- next one starts.

local function home_dir()
  local h = os.getenv("HOME")
  if not h or h == "" then h = os.getenv("USERPROFILE") end
  return h and h ~= "" and (h:gsub("\\", "/")) or "/"
end

--- `path` with ~ for the home directory, for showing.
local function short_dir(path)
  local home = home_dir()
  if home ~= "/" and path:sub(1, #home) == home and (#path == #home or path:sub(#home + 1, #home + 1) == "/") then
    return "~" .. path:sub(#home + 1)
  end
  return path
end

--- Where /cd starts from: the session's directory, or where the next one
--- starts.
local function here()
  local s = bone.api.session()
  return (((s and s.cwd) or bone.api.cwd()):gsub("\\", "/"):gsub("^//%?/", ""))
end

--- What was typed as an absolute path with no `.` or `..` left in it:
--- ~ is home, anything else relative starts from here().
local function full_dir(path)
  -- (Windows' \\?\C:\... long form, which sessions' directories come in, is C:/...)
  path = path:gsub("\\", "/"):gsub("^//%?/", "")
  if path == "~" or path:sub(1, 2) == "~/" then
    path = home_dir() .. path:sub(2)
  elseif path:sub(1, 1) ~= "/" and not path:match("^%a:/") then
    path = here():gsub("/+$", "") .. "/" .. path
  end
  local drive = path:match("^%a:") or ""
  local parts = {}
  for part in path:sub(#drive + 1):gmatch("[^/]+") do
    if part == ".." then
      parts[#parts] = nil
    elseif part ~= "." then
      parts[#parts + 1] = part
    end
  end
  return drive .. "/" .. table.concat(parts, "/")
end

-- The directory each session was in before its last /cd ("" for none
-- started), for /cd -.
local before = {}

local function cd(path)
  local s = bone.api.session()
  local key, from = s and s.session_id or "", full_dir(".")
  local dir = path == "-" and before[key] or path ~= "-" and full_dir(path)
  if path == "-" and not dir then
    return bone.notify("/cd -: no directory to go back to yet", "error")
  end
  if bone.fs.list(dir) == nil then
    return bone.notify("/cd: " .. short_dir(dir) .. " is not a directory that can be read", "error")
  end
  if dir == from then return bone.notify("already in " .. short_dir(dir)) end
  if not s then
    before[key] = from
    bone.api.cd(dir)
    return bone.notify("new sessions start in " .. short_dir(dir))
  end
  if s.running then
    return bone.notify("/cd: a turn is running here; stop it (esc) or wait for it to end", "error")
  end
  bone.request("session/cd", { session_id = s.session_id, cwd = dir }, function(info, err)
    if not info then return bone.notify("/cd: " .. tostring(err), "error") end
    -- The same place by another name (a link to it).
    if info.cwd == s.cwd then return bone.notify("already in " .. short_dir(info.cwd)) end
    before[key] = s.cwd
    bone.api.cd(info.cwd)
    bone.notify("moved to " .. short_dir(info.cwd))
  end)
end

--- Walk the directories: each one opens, the first row takes it.
local function browse(dir)
  local items = { { use = dir, text = (bone.api.session() and "Move to " or "Start sessions in ") .. short_dir(dir) } }
  local up = dir:match("^(.*)/[^/]+$")
  if up then items[#items + 1] = { open = (up == "" or up:match("^%a:$")) and up .. "/" or up, text = ".." } end
  for _, e in ipairs(bone.fs.list(dir) or {}) do
    if e.type == "dir" and e.name:sub(1, 1) ~= "." then
      items[#items + 1] = { open = dir:gsub("/+$", "") .. "/" .. e.name, text = e.name .. "/" }
    end
  end
  bone.ui.select(items, {
    prompt = short_dir(dir) .. " ",
    format = function(it) return it.text end,
    on_choice = function(it)
      if not it then return end
      if it.use then cd(it.use) else browse(it.open) end
    end,
  })
end

cmd("cd", function(c)
  local arg = (c.args or ""):match("^%s*(.-)%s*$")
  if arg ~= "" then return cd(arg) end
  -- The directories sessions are in, the latest first, then a way to any
  -- other. Not the one this is in already.
  local at, home = full_dir("."), home_dir()
  local function choose(it)
    if not it then return end
    if it.use then cd(it.use) else browse(it.open) end
  end
  local picker = bone.ui.select({}, {
    prompt = bone.api.session() and "move this session to: " or "start sessions in: ", loading = true,
    format = function(it) return it.text end, on_choice = choose,
  })
  bone.request("session/list", {}, function(list)
    local items, seen = {}, { [at] = true }
    for _, s in ipairs(list or {}) do
      if s.cwd and s.cwd ~= "" and not s.owner and not seen[s.cwd] then
        seen[s.cwd] = true
        items[#items + 1] = { use = s.cwd, text = short_dir(s.cwd) }
      end
    end
    items[#items + 1] = { open = at, text = "Browse " .. short_dir(at) .. " …" }
    if home ~= at then items[#items + 1] = { open = home, text = "Browse ~ …" } end
    picker:set_items(items)
  end)
end, {
  desc = "move this session to another directory (new ones start there too): /cd ~/projects/x, /cd .., /cd - for the one before, or alone to pick",
  complete = function(ctx)
    -- The directories under what is typed so far.
    local token = ctx.token or ""
    local typed, leaf = token:match("^(.*/)([^/]*)$")
    if not typed then typed, leaf = "", token end
    local out = {}
    for _, e in ipairs(bone.fs.list(full_dir(typed == "" and "." or typed)) or {}) do
      if e.type == "dir" and e.name:sub(1, #leaf) == leaf and (leaf ~= "" or e.name:sub(1, 1) ~= ".") then
        out[#out + 1] = typed .. e.name .. "/"
      end
    end
    return out
  end,
})

cmd("quit", function()
  bone.action("quit")
end, { desc = "quit bone", aliases = { "exit", "q" } })
local function session_id()
  local s = bone.chat.session()
  return s and s.session_id
end

cmd("queue", function(c)
  local id = session_id()
  if not id then
    return bone.notify("queue is empty")
  end
  if c.args == "clear" or c.args == "resume" then
    return bone.request("queue/" .. c.args, { session_id = id }, function(_, err)
      if err then
        bone.notify(tostring(err), "error")
      end
    end)
  end
  if c.args ~= "" then
    return bone.notify("usage: /queue [clear|resume]", "error")
  end
  local items = bone.chat.items({ kind = "queued" })
  if #items == 0 then
    return bone.notify("queue is empty")
  end
  local rows = {}
  for i, q in ipairs(items) do
    rows[i] = ("%d. [%s] %s"):format(i, q.mode, q.text)
  end
  bone.notify(table.concat(rows, "\n"))
end, {
  desc = "queued messages; /queue clear, /queue resume",
  complete = function()
    return { { value = "clear", desc = "empty the queue" }, { value = "resume", desc = "let a paused queue go on" } }
  end,
})

-- /tui: which UI library the TUI loads. Standard and bare are built in and
-- cannot be edited; a library of yours (libraries/NAME/ in the config dir)
-- starts from one of them and holds only your changes.
local function use_library(name)
  local ok, e = pcall(bone.library.use, name)
  if not ok then
    return bone.notify(error_text(e), "error")
  end
  local info = bone.library.info()
  local where = info.active == "standard" and "built into Bone"
    or info.active == "bare" and "built into Bone (plain UI)"
    or (info.dir .. "/" .. info.active)
  bone.notify(("Using TUI library: %s\nLocation: %s\nAutoreload: %s"):format(
    info.active, where, bone.o.autoreload and "enabled" or "disabled"))
end

cmd("tui", function(c)
  local sub, name = c.args:match("^(%S*)%s*(.-)%s*$")
  local info = bone.library.info()
  if sub == "" or sub == "list" then
    return bone.ui.select({ "standard", "bare", unpack(info.custom) }, {
      prompt = "TUI library",
      format = function(n)
        return (n == info.active and "● " or "  ") .. n
      end,
      footer = "enter switch · esc close · /tui create NAME [bare] makes your own",
      on_choice = function(n)
        if n and n ~= info.active then
          use_library(n)
        end
      end,
    })
  end
  if not (sub == "use" or sub == "create" or sub == "delete") or name == "" then
    return bone.notify("usage: /tui, /tui use NAME, /tui create NAME [bare], /tui delete NAME", "error")
  end
  if sub == "use" then
    return use_library(name)
  end
  local new, from = name:match("^(%S+)%s*(%S*)$")
  local ok, out = pcall(bone.library[sub], new, from ~= "" and from or nil)
  if not ok then
    return bone.notify(error_text(out), "error")
  end
  if sub == "delete" then
    return bone.notify(("Deleted %s (moved to %s)."):format(new, out))
  end
  bone.notify(("Created %s in %s (it starts as the %s UI; ask bone to change it).\nUse it: /tui use %s")
    :format(new, out, from == "bare" and "bare" or "standard", new))
end, {
  desc = "the TUI library: standard, bare or your own; /tui use|create|delete NAME",
  complete = function(ctx)
    local argv = ctx.argv or {}
    if #argv <= 1 and not (ctx.args or ""):match("%s$") then
      return {
        { value = "use", desc = "switch library" },
        { value = "create", desc = "your own, starting from standard (or bare: NAME bare)" },
        { value = "delete", desc = "remove one of yours not in use" },
      }
    end
    if argv[1] ~= "use" and argv[1] ~= "delete" then
      return {}
    end
    local info = bone.library.info()
    local out = argv[1] == "use" and { { value = "standard" }, { value = "bare" } } or {}
    for _, n in ipairs(info.custom) do
      out[#out + 1] = { value = n }
    end
    return out
  end,
})

cmd("config", function(c)
  require("bone.config").open(c.args)
end, {
  desc = "settings: options, provider and model, plugins",
  aliases = { "settings" },
})

-- /model: switch models (bone.models). Alone it opens the picker; with a
-- name it moves this chat to that model of its provider and adds it to the
-- provider's list (alt+m goes round the list); `rm NAME` takes one off.
cmd("model", function(c)
  local models = require("bone.models")
  local args = c.args:gsub("^%s+", ""):gsub("%s+$", "")
  if args == "" then return models.pick() end
  local rm = args:match("^rm%s+(.+)$")
  if rm then return models.remove(rm) end
  models.use(nil, args)
end, {
  desc = "switch the model: alone a picker; /model NAME switches and adds it to the provider's list; /model rm NAME",
  complete = function(ctx)
    local argv = ctx.argv or {}
    if argv[1] == "rm" and (#argv > 1 or (ctx.args or ""):match("%s$")) then
      return require("bone.models").complete(argv[2] or "")
    end
    if #argv > 1 or (ctx.args or ""):match("%s$") then return {} end
    local out = require("bone.models").complete(ctx.token or "")
    if ("rm"):sub(1, #(ctx.token or "")) == (ctx.token or "") then
      out[#out + 1] = { value = "rm", desc = "take a model off the list" }
    end
    return out
  end,
})

cmd("provider", function()
  require("bone.config").open("providers")
end, { desc = "the model provider (the providers page in /config)" })


cmd("catalog", function()
  require("bone.catalog").open()
end, { desc = "browse, install, update and remove plugins from the catalog" })

cmd("setup", function()
  require("bone.setup").open()
end, { desc = "add a model provider (and catalog plugins)" })



cmd("paste", function() bone.prompt.paste() end, { desc = "paste clipboard image or text (ctrl+v)" })
cmd("attach", function(c)
  local path = c.args:gsub("^%s+", ""):gsub("%s+$", "")
  if path == "" then return bone.notify("usage: /attach PATH (PNG, JPEG or WebP)", "error") end
  if (path:sub(1, 1) == '"' and path:sub(-1) == '"') or (path:sub(1, 1) == "'" and path:sub(-1) == "'") then
    path = path:sub(2, -2)
  end
  bone.prompt.attach(path)
end, { desc = "attach an image file; paths with spaces are supported" })
cmd("detach", function(c)
  if c.args == "" then
    local images = bone.prompt.images()
    if #images == 0 then return bone.notify("no images attached") end
    return bone.prompt.remove_image(#images)
  end
  if c.args == "all" then return bone.prompt.set_images({}) end
  local index = tonumber(c.args)
  if not index or index % 1 ~= 0 or index < 1 then return bone.notify("usage: /detach [NUMBER|all]", "error") end
  bone.prompt.remove_image(index)
end, { desc = "remove the last image, a numbered image, or all images" })

-- Commands for sessions, plugins and the project config. They are plain Lua:
-- replace them, or remove them with bone.cmd.del.

local function current_session()
  local s = bone.chat.session()
  if not s or not s.session_id then
    bone.notify("this session has no messages yet", "error")
    return nil
  end
  return s.session_id
end

cmd("rename", function(c)
  local id = current_session()
  if not id then
    return
  end
  if c.args == "" then
    return bone.notify("usage: /rename title", "error")
  end
  bone.request("session/set", { session_id = id, title = c.args }, function(info, err)
    if err then
      return bone.notify("rename failed: " .. tostring(err), "error")
    end
    bone.notify("renamed to " .. (info.title or ""))
  end)
end, { desc = "give this session a title" })

-- Compaction (the core's; see crates/bone-core/src/compact.rs): the model
-- gets a summary in place of the older part of a long session, while the
-- chat and the session file keep everything. Each compaction, by /compact
-- or automatic, leaves one line in the chat.
local function tokens(n)
  n = tonumber(n) or 0
  if n >= 1000 then
    return (string.format("%.1fk", n / 1000):gsub("%.0k$", "k"))
  end
  return tostring(n)
end

local WHY = { limit = " (over compaction limit)", overflow = " (the context was full)" }

bone.on("session/compacted", function(ev)
  local text
  if ev.reason == "clear" then
    text = ("compaction cleared · the full history is sent again (~%s tokens)"):format(tokens(ev.tokens_after))
  else
    text = ("compacted %d message%s · ~%s tokens saved (%s → %s)%s"):format(ev.messages,
      ev.messages == 1 and "" or "s", tokens(math.max(ev.tokens_before - ev.tokens_after, 0)),
      tokens(ev.tokens_before), tokens(ev.tokens_after), WHY[ev.reason] or "")
  end
  -- Only into a chat this TUI has open.
  pcall(bone.chat.add, "compacted", { text = text, reason = ev.reason }, { session = ev.session_id })
end)

bone.on("session/compact_failed", function(ev)
  local text = "compaction failed" .. (WHY[ev.reason] or "") .. ": " .. ev.error
    .. " · context unchanged"
  pcall(bone.chat.add, "compacted", { text = text, reason = ev.reason }, { session = ev.session_id })
end)

cmd("compact", function(c)
  local id = current_session()
  if not id then
    return
  end
  if c.args ~= "" and c.args ~= "clear" then
    return bone.notify("usage: /compact [clear]", "error")
  end
  if c.args == "" then
    bone.notify("compacting…")
  end
  bone.request("session/compact", { session_id = id, clear = c.args == "clear" or nil }, function(_, err)
    if err then
      return bone.notify("compact: " .. tostring(err):gsub("^rpc error %-?%d+: ", ""), "error")
    end
    bone.notify(c.args == "clear" and "compaction cleared" or "compacted")
  end)
end, {
  desc = "summarize the older part of this session for the model; the chat stays",
  complete = function()
    return { { value = "clear", desc = "send the full history again" } }
  end,
})

bone.settings.page({
  name = "compact",
  title = "Compaction",
  fields = {
    { key = "keep", label = "Turns kept in full", type = "integer", default = 0, min = 0, max = 50,
      desc = "the latest user turns always sent word for word" },
    { key = "auto", label = "Compact when full", type = "boolean", default = true,
      desc = "compact and retry when the model says the context is too long" },
    { key = "limit", label = "Token limit", type = "integer", default = 0, min = 0,
      desc = "compact before a call estimated above this many tokens (0: never)" },
    { key = "provider", label = "Summary provider", type = "string", default = "",
      desc = "the provider that writes summaries (empty: the current one)" },
  },
})

local function message(e)
  return (tostring(e):gsub("^runtime error: ", ""):gsub("\nstack traceback:.*$", ""))
end

-- /plugins opens the plugin settings; /plugins list lists both halves of
-- every plugin; /plugins load|unload|reload name acts on its TUI half here
-- and its core half in the core; /plugins reload reloads the core's whole
-- configuration.
cmd("plugins", function(c)
  local op, name = c.args:match("^(%S+)%s*(%S*)$")
  if not op then
    return require("bone.config").open("plugins")
  end
  if op == "list" and name == "" then
    local tui = bone.plugin.list()
    bone.request("plugin/list", {}, function(core)
      core = core or {}
      local seen, names = {}, {}
      local function add(n)
        if not seen[n] then
          seen[n] = true
          names[#names + 1] = n
        end
      end
      for _, p in ipairs(tui) do
        add(p.name)
      end
      for _, p in ipairs(core) do
        if p.core then
          add(p.name)
        end
      end
      table.sort(names)
      local rows = {}
      for _, n in ipairs(names) do
        local parts = {}
        for _, p in ipairs(tui) do
          if p.name == n then
            local state = p.loaded and "loaded" or "unloaded"
            parts[#parts + 1] = p.error and ("tui " .. state .. ": " .. p.error) or ("tui " .. state)
          end
        end
        for _, p in ipairs(core) do
          if p.name == n and p.core then
            parts[#parts + 1] = p.loaded and "core loaded" or "core unloaded"
          end
        end
        rows[#rows + 1] = n .. " (" .. table.concat(parts, ", ") .. ")"
      end
      bone.notify(#rows > 0 and table.concat(rows, "\n") or "no plugins")
    end)
    return
  end
  if op == "reload" and name == "" then
    bone.request("core/reload", {}, function(r, err)
      if err then
        return bone.notify("core reload failed: " .. tostring(err), "error")
      end
      local warnings = r.warnings or {}
      bone.notify("core configuration reloaded" .. (#warnings > 0 and (": " .. table.concat(warnings, "; ")) or ""))
    end)
    return
  end
  if (op == "trust" or op == "untrust") and name == "" then
    -- This directory's .bone/tui.lua runs only once trusted.
    local info = bone.project.info()
    if not info then
      return bone.notify("no .bone/tui.lua here or in a parent directory", "error")
    end
    local ok, e = pcall(bone.project.trust, op == "trust")
    if not ok then
      return bone.notify(message(e), "error")
    end
    return bone.notify(info.file .. (op == "trust" and ": trusted and loaded" or ": no longer trusted, unloaded"))
  end
  if (op ~= "load" and op ~= "unload" and op ~= "reload") or name == "" then
    return bone.notify("usage: /plugins [list] [reload] [load|unload|reload name] [trust|untrust]", "error")
  end
  local has_tui = false
  for _, p in ipairs(bone.plugin.list()) do
    has_tui = has_tui or p.name == name
  end
  if not has_tui and bone.config_dir then
    local f = io.open(bone.config_dir .. "/plugins/" .. name .. "/tui.lua")
    if f then
      f:close()
      has_tui = true
    end
  end
  local tui_ok, tui_err = true, nil
  if has_tui then
    tui_ok, tui_err = pcall(bone.plugin[op], name)
  end
  local done = op .. "ed"
  -- The core decides whether it has a core half.
  bone.request("plugin/" .. op, { name = name }, function(_, err)
    if not tui_ok then
      return bone.notify(message(tui_err), "error")
    end
    if err then
      err = tostring(err)
      if not has_tui then
        return bone.notify(err, "error")
      elseif err:find("no core.lua", 1, true) or err:find("no plugin", 1, true) then
        return bone.notify(name .. ": tui " .. done)
      end
      return bone.notify(name .. ": tui " .. done .. ", but the core: " .. err, "error")
    end
    bone.notify(name .. (has_tui and ": tui and core " or ": core ") .. done)
  end)
end, {
  desc = "plugin settings; list, load/unload/reload name; /plugins reload reloads the core's config; /plugins trust runs this project's .bone/tui.lua",
})

return true
