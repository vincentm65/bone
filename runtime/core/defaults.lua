-- Default core settings. ~/.bone/core.lua runs after this.

local BASE = [[
You are bone, a coding assistant working in the user's terminal. Use the tools
to inspect files, make changes and run commands. Read files before editing
them. Keep changes minimal and focused, and keep answers concise.]]

-- The apps by client name: what they are called, their folder in a library
-- and the file in the config dir that says which library they use.
local APPS = {
  ["bone-tui"] = { name = "terminal app (TUI)", folder = "tui", choice = "library", key = "tui" },
  ["bone-gui"] = { name = "desktop app (GUI)", folder = "gui", choice = "gui-library", key = "gui" },
}

--- The system prompt's part on changing bone itself, for the user of the
--- app `client` ("bone-tui", "bone-gui"; anything else is unknown): which
--- UI library it uses and exactly what to do, so the agent never guesses.
local function customizing(client)
  local home = bone.config_dir
  if not home then
    return ""
  end
  local libs = bone.ui_libraries()
  local out = {
    "",
    "# Changing bone itself",
    "",
    "Follow this only when the user asks to change bone itself: how its app looks or behaves (layout,",
    "colors, keys, slash commands, panels, the chat). For any other task, ignore this section.",
    "",
    "Rules:",
    "- Bone's look comes from a UI library. The built-in ones, standard and bare, can never be changed:",
    "  never edit bone's source code or its built-in files, and never copy a built-in file whole.",
    "- Every change to the app goes in a UI library of the user's own, a folder in " .. libs.dir .. "/.",
    "- Before your first edit, read " .. home .. "/docs/customizing.md. It says which file to change",
    "  for each kind of change and how; then read only the guide it points to.",
    "- Core changes (tools, hooks, providers, this prompt) are not UI: they go in " .. home .. "/core.lua.",
    "",
  }
  local function add(line)
    out[#out + 1] = line
  end
  local mine = {}
  for _, l in ipairs(libs.mine) do
    mine[#mine + 1] = l.name
  end
  local app = APPS[type(client) == "string" and client or ""]
  if not app then
    add("Which of bone's apps the user is in is not known. If it matters, ask: the terminal app (TUI) keeps")
    add("its files in a library's tui/ folder and its choice in " .. home .. "/library; the desktop app (GUI)")
    add("in gui/ and " .. home .. "/gui-library. The user's own libraries: "
      .. (#mine > 0 and table.concat(mine, ", ") or "none") .. ".")
    return table.concat(out, "\n")
  end
  local active = libs[app.key]
  local builtin = active == "standard" or active == "bare"
  local entry = app.folder .. "/defaults.lua"
  local line = ('bone.builtin("%s/%s.lua")'):format(app.folder, active == "bare" and "bare" or "defaults")
  add(("The user is in bone's %s. It uses %s."):format(
    app.name, builtin and ("the built-in " .. active .. " UI library") or ('their own UI library "' .. active .. '"')))
  add("The user's own UI libraries: " .. (#mine > 0 and table.concat(mine, ", ") or "none") .. ".")
  add("")
  add("What to do:")
  local make = "create " .. libs.dir .. "/NAME/" .. entry .. " containing exactly this line,\n"
    .. "   which runs the built-in UI (your changes go after it):\n"
    .. "     " .. line
  local switch = "Switch the " .. app.name .. " to it: write just NAME to " .. home .. "/" .. app.choice
    .. ". The app reloads by itself."
  if not builtin then
    add(('Do not ask which library to change: make the change in "%s" (%s/%s/) directly.'):format(
      active, libs.dir, active))
    add(("If %s/%s/%s does not exist yet, first create it containing exactly this line:"):format(libs.dir, active, entry))
    add("  " .. line)
  elseif #mine == 0 then
    add("1. Ask the user only this (skip it if they already answered in this conversation):")
    add(('   "Bone\'s %s look can\'t be changed directly, so I\'ll make you your own UI library based on it.'):format(active))
    add('   What should I call it?"')
    add("   A name is letters, digits, - or _.")
    add("2. Make the library: " .. make)
    add("3. " .. switch)
    add("4. Make the change in " .. libs.dir .. "/NAME/.")
  else
    add("1. Ask the user only this (skip it if they already answered in this conversation):")
    add(('   "Bone\'s %s look can\'t be changed directly. Which of your UI libraries should I change: %s,'):format(
      active, table.concat(mine, ", ")))
    add('   or a new one?" For a new one, also ask its name (letters, digits, - or _).')
    add("2. For a new one, make the library: " .. make)
    add("   For an existing one: if its " .. entry .. " does not exist yet, create it with that same line.")
    add("3. " .. switch)
    add("4. Make the change in " .. libs.dir .. "/NAME/.")
  end
  return table.concat(out, "\n")
end

bone.config.system_prompt = function(ctx)
  return BASE .. "\n" .. customizing(ctx.client)
end

-- A background shell job the agent started has stopped: tell that session,
-- so the agent need not poll. Jobs cancelled on purpose stay quiet.
-- Set bone.config.notify_job_exit = false in core.lua to turn this off.
bone.config.notify_job_exit = true
bone.hook("process_exit", function(ev)
  if not bone.config.notify_job_exit or ev.state == "cancelled" then
    return
  end
  local how = ev.state == "exited" and ("exit code " .. tostring(ev.code))
    or ev.state == "timed_out" and "timed out"
    or ("failed: " .. tostring(ev.error or ev.signal))
  local text = ("Background job %s (`%s`) finished after %ds: %s."):format(
    ev.id, ev.command, math.floor((ev.elapsed_ms or 0) / 1000), how)
  if ev.output and ev.output ~= "" then
    text = text .. "\n\nLast output:\n```\n" .. ev.output .. "\n```"
  end
  pcall(bone.queue.add, ev.session_id, text, "steer", nil, "job")
end)
