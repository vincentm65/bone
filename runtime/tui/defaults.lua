-- Default keys. ~/.bone/tui.lua runs after this and can change or delete
-- any of them; ~/.bone/runtime/tui/defaults.lua replaces this file.
-- Keys without a mapping type text into the prompt.

local function map(keys, context)
  for key, action in pairs(keys) do
    bone.keymap.set(key, action, { context = context })
  end
end

-- The prompt (the "main" context).
map({
  ["enter"] = "submit",
  ["alt+enter"] = "newline",
  ["shift+enter"] = "newline",
  ["ctrl+j"] = "newline",
  ["tab"] = "complete",
  ["esc"] = "dismiss",
  ["ctrl+c"] = "interrupt",
  ["ctrl+d"] = "quit_if_empty",
  ["ctrl+r"] = "sessions",
  ["ctrl+n"] = "new_session",

  ["left"] = "left",
  ["right"] = "right",
  ["up"] = "up",
  ["down"] = "down",
  ["ctrl+left"] = "word_left",
  ["ctrl+right"] = "word_right",
  ["alt+left"] = "word_left",
  ["alt+right"] = "word_right",
  ["home"] = "line_start",
  ["end"] = "line_end",
  ["ctrl+a"] = "line_start",
  ["ctrl+e"] = "line_end",
  ["backspace"] = "backspace",
  ["ctrl+h"] = "backspace",
  ["delete"] = "delete",
  ["ctrl+w"] = "delete_word",
  ["alt+backspace"] = "delete_word",
  ["ctrl+u"] = "delete_to_start",
  ["ctrl+k"] = "delete_to_end",

  ["pageup"] = "page_up",
  ["pagedown"] = "page_down",
  wheelup = "scroll_up",
  wheeldown = "scroll_down",
  ["shift+up"] = "scroll_up",
  ["shift+down"] = "scroll_down",
  ["ctrl+home"] = "scroll_top",
  ["ctrl+end"] = "scroll_bottom",
})

-- While a Lua popup is open (after the popup's own keys).
map({
  ["ctrl+c"] = "interrupt",
}, "popup")

-- While a panel has the keyboard (after the panel's own keys). Scrolling and
-- dismiss act on the panel; text that is not mapped is ignored.
map({
  ["esc"] = "dismiss",
  ["ctrl+c"] = "interrupt",
  ["tab"] = "focus_next",
  ["shift+tab"] = "focus_prev",
  ["up"] = "up",
  ["down"] = "down",
  ["pageup"] = "page_up",
  ["pagedown"] = "page_down",
  ["home"] = "scroll_top",
  ["end"] = "scroll_bottom",
  wheelup = "scroll_up",
  wheeldown = "scroll_down",
}, "panel")

-- One blank row between the chat and the prompt, so text never touches
-- what you type. Replace it (a spinner line, a rule) or set it to nil.
function bone.ui.divider()
  return {}
end

-- The session picker (ctrl+r, /sessions), built on bone.ui.select.
function bone.ui.sessions()
  local home = os.getenv("HOME")
  local picker = bone.ui.select({}, {
    prompt = "Sessions",
    loading = true,
    empty = "no sessions yet",
    format = function(s)
      local dir = s.cwd
      if home and dir:sub(1, #home) == home then
        dir = "~" .. dir:sub(#home + 1)
      end
      return (s.title or "[untitled]") .. "  " .. dir
    end,
    on_choice = function(s)
      if s then
        bone.api.open_session(s.session_id)
      end
    end,
  })
  bone.request("session/list", {}, function(list, err)
    if err then
      picker:close()
      bone.notify("cannot list sessions: " .. err, "error")
      return
    end
    picker:set_items(list)
  end)
end

-- Matching slash commands while typing "/…", right above the prompt.
-- ctx = { items = { { name, desc } }, selected, width, height }.
function bone.ui.suggestions(ctx)
  local name_w, desc_w = 0, 0
  for _, it in ipairs(ctx.items) do
    name_w = math.max(name_w, bone.text.width(it.name) + 1)
    desc_w = math.max(desc_w, bone.text.width(it.desc))
  end
  local lines = {}
  for i, it in ipairs(ctx.items) do
    local hl = i == ctx.selected and "Selection" or "Normal"
    local name = "/" .. it.name
    lines[i] = {
      { name .. string.rep(" ", name_w + 1 - bone.text.width(name)), hl },
      { it.desc .. string.rep(" ", desc_w - bone.text.width(it.desc)), hl },
    }
  end
  return bone.ui.box(lines, { border_hl = "WinSeparator", width = math.min(name_w + desc_w + 5, ctx.width) })
end

-- /help topic: the best matching section of the docs, in a pager.
local function doc_sections()
  local out = {}
  for _, d in ipairs(bone._docs) do
    local code = false
    for line in (d.text .. "\n"):gmatch("(.-)\n") do
      if line:match("^```") then
        code = not code
      end
      local hashes, title = line:match("^(#+)%s+(.*)$")
      if hashes and not code then
        out[#out + 1] = { doc = d.name, level = #hashes, title = title, lines = {} }
      end
      if #out > 0 and out[#out].doc == d.name then
        table.insert(out[#out].lines, line)
      end
    end
  end
  return out
end

function bone.ui.help(topic)
  local t = topic:lower()
  for _, d in ipairs(bone._docs) do
    if d.name == t then
      return bone.ui.pager(d.text, { title = d.name .. ".md" })
    end
  end
  local secs = doc_sections()
  local best, score = nil, 0
  for i, s in ipairs(secs) do
    local title = s.title:lower():gsub("`", "")
    local sc = 0
    if title == t then
      sc = 4
    elseif (" " .. title .. " "):find("[^%w_]" .. t:gsub("%p", "%%%0") .. "[^%w_]") then
      sc = 3
    elseif title:find(t, 1, true) then
      sc = 2
    elseif table.concat(s.lines, "\n"):lower():find(t, 1, true) then
      sc = 1
    end
    if sc > score then
      best, score = i, sc
    end
  end
  if not best then
    return bone.notify("no help for " .. topic .. " (try /help lua or /help usage)", "error")
  end
  -- The section with its subsections.
  local s = secs[best]
  local lines = {}
  for i = best, #secs do
    if i > best and (secs[i].doc ~= s.doc or secs[i].level <= s.level) then
      break
    end
    for _, l in ipairs(secs[i].lines) do
      lines[#lines + 1] = l
    end
  end
  bone.ui.pager(table.concat(lines, "\n"), { title = s.doc .. ".md: " .. s.title })
end

-- /health: the TUI's checks, then the core's (over the protocol).
local MARK = { ok = { "✓ ", "DiffAdd" }, warn = { "! ", "WarningMsg" }, error = { "✗ ", "ErrorMsg" } }

local function health_lines(title, items, out)
  out[#out + 1] = { { title, "Accent" } }
  for _, it in ipairs(items) do
    local m = MARK[it.status] or MARK.warn
    out[#out + 1] = { m, { it.name .. ": ", "Normal" }, { it.message, "Dim" } }
  end
  out[#out + 1] = {}
end

function bone.ui.health()
  local tui = bone.api.health()
  for _, it in ipairs(bone._run_health()) do
    tui[#tui + 1] = it
  end
  local lines = {}
  health_lines("TUI", tui, lines)
  local pager = bone.ui.pager({ unpack(lines), { { "Core: checking…", "Dim" } } }, { title = "Health" })
  bone.request("health/check", {}, function(core, err)
    local all = { unpack(lines) }
    if err then
      health_lines("Core", { { name = "core", status = "error", message = err } }, all)
    else
      health_lines("Core", core or {}, all)
    end
    pager:set(all)
  end)
end

-- Colors (see runtime/colors/).
bone.colorscheme("black")

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

bone.cmd.create("rename", function(c)
  local id = current_session()
  if not id then
    return
  end
  if c.args == "" then
    return bone.notify("usage: /rename title", "error")
  end
  bone.request("session/rename", { session_id = id, title = c.args }, function(info, err)
    if err then
      return bone.notify("rename failed: " .. tostring(err), "error")
    end
    bone.notify("renamed to " .. (info.title or ""))
  end)
end, { desc = "give this session a title" })

bone.cmd.create("fork", function(c)
  local id = current_session()
  if not id then
    return
  end
  local n
  if c.args ~= "" then
    n = tonumber(c.args)
    if not n or n < 1 or n % 1 ~= 0 then
      return bone.notify("usage: /fork [turn number]", "error")
    end
  end
  bone.request("session/fork", { session_id = id, before_turn = n }, function(info, err)
    if err then
      return bone.notify("fork failed: " .. tostring(err), "error")
    end
    bone.api.open_session(info.session_id)
    bone.notify(n and ("forked from before turn " .. n) or "forked")
  end)
end, { desc = "copy this session to try something else; /fork N starts before turn N" })

bone.cmd.create("delete", function(c)
  local id = current_session()
  if not id then
    return
  end
  if c.args ~= "yes" then
    return bone.notify("Delete this session and its file? /delete yes")
  end
  bone.request("session/delete", { session_id = id }, function(_, err)
    if err then
      bone.notify("delete failed: " .. tostring(err), "error")
    end
  end)
end, { desc = "delete this session (/delete yes)" })

local function message(e)
  return (tostring(e):gsub("^runtime error: ", ""):gsub("\nstack traceback:.*$", ""))
end

-- /plugin lists both halves of every plugin; /plugin load|unload|reload name
-- acts on its TUI half here and its core half in the core; /plugin reload
-- reloads the core's whole configuration.
bone.cmd.create("plugin", function(c)
  local op, name = c.args:match("^(%S+)%s*(%S*)$")
  if not op then
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
  if (op ~= "load" and op ~= "unload" and op ~= "reload") or name == "" then
    return bone.notify("usage: /plugin [reload] [load|unload|reload name]", "error")
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
  desc = "plugins: list, load/unload/reload name; /plugin reload reloads the core's config",
  aliases = { "plugins" },
})

bone.cmd.create("project", function(c)
  local info = bone.project.info()
  if not info then
    return bone.notify("no .bone/tui.lua here or in a parent directory", "error")
  end
  if c.args == "" then
    bone.notify(info.file .. ": " .. (info.trusted and "trusted" or "not trusted") .. (info.loaded and ", loaded" or ""))
  elseif c.args == "trust" then
    bone.project.trust(true)
  elseif c.args == "untrust" then
    bone.project.trust(false)
    bone.notify("project config unloaded and no longer trusted")
  else
    bone.notify('unknown /project argument "' .. c.args .. '" (trust, untrust)', "error")
  end
end, { desc = "this project's .bone/tui.lua: show, trust, untrust" })
