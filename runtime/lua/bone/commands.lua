-- The built-in slash commands, as Lua. Each is an ordinary bone.cmd command:
-- replace one with bone.cmd.create, remove it with bone.cmd.del, or replace
-- this module (~/.bone/runtime/lua/bone/commands.lua).

local cmd = bone.cmd.create

local function error_text(e)
  return (tostring(e):gsub("^runtime error: ", ""):gsub("\nstack traceback:.*$", ""))
end

local KEYS = [[
Keys: enter send · alt+enter newline · ctrl+c cancel / clear / quit · ctrl+o sessions
      pageup/pagedown scroll · ctrl+home/ctrl+end top/bottom · up/down history · tab complete
      ctrl+r show/hide reasoning]]

cmd("help", function(c)
  if c.args ~= "" then
    return bone.ui.help(c.args)
  end
  local rows = {}
  for _, it in ipairs(bone.cmd.list()) do
    local aliases = #it.aliases > 0 and (" (aliases: " .. table.concat(it.aliases, ", ") .. ")") or ""
    rows[#rows + 1] = ("/%-12s %s%s"):format(it.name, it.desc, aliases)
  end
  rows[#rows + 1] = ""
  rows[#rows + 1] = KEYS
  rows[#rows + 1] = "Docs: /help topic (e.g. /help hooks, /help windows, /help keys)"
  bone.notify(table.concat(rows, "\n"))
end, { desc = "commands and keys; /help topic searches the docs", aliases = { "?" } })

cmd("health", function()
  bone.ui.health()
end, { desc = "check the setup: provider, terminal, clipboard, Lua", aliases = { "checkhealth" } })

cmd("new", function()
  bone.action("new_session")
end, { desc = "start a new session" })

cmd("sessions", function()
  bone.action("sessions")
end, { desc = "pick a session to open (ctrl+o)", aliases = { "resume" } })

cmd("open", function(c)
  local prefix = c.args
  if prefix == "" then
    return bone.notify("usage: /open {session id or prefix}", "error")
  end
  bone.request("session/list", {}, function(list, err)
    if err then
      return bone.notify("cannot list sessions: " .. tostring(err), "error")
    end
    local hits = {}
    for _, s in ipairs(list) do
      if s.session_id:sub(1, #prefix) == prefix then
        hits[#hits + 1] = s
      end
    end
    if #hits == 1 then
      bone.api.open_session(hits[1].session_id)
    elseif #hits == 0 then
      bone.notify("No session matches " .. prefix, "error")
    else
      bone.notify(#hits .. " sessions match " .. prefix, "error")
    end
  end)
end, { desc = "open a session by id or id prefix" })

cmd("cancel", function()
  bone.action("interrupt")
end, { desc = "cancel the running turn" })

cmd("quit", function()
  bone.action("quit")
end, { desc = "quit bone", aliases = { "exit", "q" } })

cmd("set", function(c)
  if c.args == "" then
    return bone.notify(table.concat(bone.o.list(), "  "))
  end
  local shown = {}
  for arg in c.args:gmatch("%S+") do
    local ok, r = pcall(bone.o.apply, arg)
    if not ok then
      return bone.notify(error_text(r), "error")
    end
    if r then
      shown[#shown + 1] = r
    end
  end
  if #shown > 0 then
    bone.notify(table.concat(shown, "  "))
  end
end, { desc = "options: name, noname, name=value, name?" })

cmd("colorscheme", function(c)
  if c.args == "" then
    return bone.notify(bone.api.colors_name() or "(built-in)")
  end
  local ok, e = pcall(bone.colorscheme, c.args)
  if not ok then
    bone.notify(error_text(e), "error")
  end
end, { desc = "black (default), ansi, or your own", aliases = { "theme" } })

local FLAGS = { bold = true, italic = true, underline = true, reverse = true, dim = true }

cmd("highlight", function(c)
  local group, rest = c.args:match("^(%S+)%s*(.*)$")
  if not group then
    return bone.notify(table.concat(bone.hl.names(), " "))
  end
  if rest == "" then
    return bone.notify(group .. " " .. bone.inspect(bone.hl.get(group)))
  end
  local spec = {}
  for a in rest:gmatch("%S+") do
    local k, v = a:match("^(%w+)=(.*)$")
    if k == "fg" or k == "bg" or k == "link" then
      spec[k] = v
    elseif not k and FLAGS[a] then
      spec[a] = true
    else
      return bone.notify(('bad highlight attribute "%s"'):format(a), "error")
    end
  end
  local ok, e = pcall(bone.hl.set, group, spec)
  if not ok then
    bone.notify(error_text(e), "error")
  end
end, { desc = "set a color: /hi Group fg=#rrggbb bg=... bold", aliases = { "hi" } })

cmd("lua", function(c)
  bone.api.exec_lua(c.args)
end, { desc = "run Lua; /lua =expr shows a value" })

cmd("source", function(c)
  if c.args == "" then
    return bone.notify("usage: /source {file}", "error")
  end
  bone.api.source(c.args)
end, { desc = "run a Lua file" })

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

cmd("unqueue", function(c)
  local n = tonumber(c.args)
  local item = n and bone.chat.items({ kind = "queued" })[n]
  if not item then
    return bone.notify("usage: /unqueue N (see /queue)", "error")
  end
  bone.request("queue/remove", { session_id = session_id(), id = item.id }, function(_, err)
    if err then
      bone.notify(tostring(err), "error")
    end
  end)
end, { desc = "take message N out of the queue" })

cmd("messages", function()
  bone.api.show(table.concat(bone.api.log(20), "\n"))
end, { desc = "recent messages and full Lua errors" })

-- /runtime: your copies of built-in runtime files, and going back to the
-- built-in ones. Bone never writes these copies; this is how to undo them.
cmd("runtime", function(c)
  local sub, what = c.args:match("^(%S*)%s*(.-)%s*$")
  local list = bone.runtime.overrides()
  if sub == "" then
    if #list == 0 then
      return bone.notify("No runtime files are overridden; bone uses its built-in ones.")
    end
    local rows = { "Your copies in runtime/ (used instead of the built-in files):" }
    for _, o in ipairs(list) do
      rows[#rows + 1] = "  " .. o.path .. (o.same and "  (same as built-in)" or "  (changed)")
    end
    rows[#rows + 1] = "/runtime reset FILE or /runtime reset all goes back to the built-in ones."
    return bone.notify(table.concat(rows, "\n"))
  end
  if sub ~= "reset" or what == "" then
    return bone.notify("usage: /runtime, /runtime reset FILE, /runtime reset all", "error")
  end
  local paths = {}
  if what == "all" then
    for _, o in ipairs(list) do
      paths[#paths + 1] = o.path
    end
    if #paths == 0 then
      return bone.notify("Nothing to reset: no runtime files are overridden.")
    end
  else
    paths[1] = what
  end
  local stamp = tostring(os.time())
  local moved, where, core = 0, nil, false
  for _, path in ipairs(paths) do
    local ok, to = pcall(bone.runtime.reset, path, stamp)
    if not ok then
      return bone.notify(error_text(to), "error")
    end
    moved = moved + 1
    where = to:sub(1, #to - #path - 1)
    core = core or path:match("^core/") ~= nil
  end
  bone.notify(
    ("Reset %d file%s to the built-in version. Your copies are in %s."):format(moved, moved == 1 and "" or "s", where)
      .. (core and " Core files apply after /plugin reload." or "")
  )
end, {
  desc = "your copies of built-in runtime files; /runtime reset FILE|all goes back to the built-in",
  complete = function(ctx)
    local argv = ctx.argv or {}
    if #argv <= 1 and not (ctx.args or ""):match("%s$") then
      return { { value = "reset", desc = "go back to the built-in file" } }
    end
    local out = { { value = "all", desc = "every overridden file" } }
    for _, o in ipairs(bone.runtime.overrides()) do
      out[#out + 1] = { value = o.path, desc = o.same and "same as built-in" or "changed" }
    end
    return out
  end,
})

return true

