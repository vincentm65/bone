-- Default keys. ~/.bone/tui.lua runs after this and can change or delete
-- any of them; ~/.bone/runtime/tui/defaults.lua replaces this file.
-- Keys without a mapping type text into the prompt.

-- The standard Lua UI is the starting point. User config and plugins run
-- afterward and can replace any individual view, region, or widget.
require("bone.ui").setup()

-- The slash commands and the / menu are Lua modules (runtime/lua/bone/).
require("bone.commands")
require("bone.menu")

-- Enter during a turn queues the message in the core: "steer" joins the
-- running turn at its next step, "next" waits for a turn of its own. The
-- actions queue_steer and queue_next do one or the other whatever this says.
bone.o.define("queue_mode", "steer", {
  desc = "what enter does during a turn: steer or next",
  choices = { "steer", "next" },
})

-- How much of each tool call the chat shows: "summary" (one line per run
-- of calls, edits and failures in full), "rows" (a row per call, output
-- cut short) or "full" (everything). ctrl+t steps through them.
bone.o.define("tool_detail", "summary", {
  desc = "tool calls: summary, rows or full (ctrl+t)",
  choices = { "summary", "rows", "full" },
  on_change = function()
    bone.ui.refresh()
  end,
})
local next_detail = { summary = "rows", rows = "full", full = "summary" }
bone.keymap.set("ctrl+t", function()
  bone.o.tool_detail = next_detail[bone.o.tool_detail] or "rows"
  -- Remembered for next time (settings.json).
  bone.settings.set("tui.tool_detail", bone.o.tool_detail)
end)

-- Sub-agents and shell jobs: the tray below the prompt
-- (runtime/lua/bone/ui/tray.lua). Down on an empty prompt moves into it;
-- esc on an empty prompt in a sub-agent's session goes back to the session
-- that started it.
local tray = require("bone.ui.tray")
local menu_dismiss = bone.ui.actions.dismiss
bone.ui.actions.dismiss = function()
  local done = menu_dismiss and menu_dismiss()
  if done then
    return done
  end
  return tray.cancel_edit() or (bone.prompt.get() == "" and tray.back())
end
local menu_down = bone.ui.actions.down
bone.ui.actions.down = function()
  if menu_down and menu_down() then
    return true
  end
  return bone.prompt.get() == "" and not bone.prompt.info().history and tray.enter()
end

-- Up on an empty prompt edits the last queued message (enter saves it in
-- place; the tray's Queue page edits any of them).
local menu_up = bone.ui.actions.up
bone.ui.actions.up = function()
  if menu_up and menu_up() then
    return true
  end
  local s = bone.chat.session()
  if bone.prompt.get() ~= "" or not s or not s.session_id then
    return false
  end
  local queued = bone.chat.items({ kind = "queued" })
  local last = queued[#queued]
  if not last then
    return false
  end
  tray.edit(last)
  return true
end

local function map(keys, context)
  for key, action in pairs(keys) do
    bone.keymap.set(key, action, { context = context })
  end
end

-- The prompt (the "main" context).
map({
  ["f1"] = "/help",
  ["enter"] = "submit",
  ["alt+enter"] = "newline",
  ["shift+enter"] = "newline",
  ["ctrl+j"] = "newline",
  ["tab"] = "complete",
  ["esc"] = "dismiss",
  ["ctrl+c"] = "interrupt",
  ["ctrl+d"] = "quit_if_empty",
  ["ctrl+o"] = "sessions",
  ["ctrl+n"] = "new_session",
  ["ctrl+p"] = "focus_next",
  ["ctrl+r"] = function()
    -- Toggle the model reasoning (live, in the chat, and in the transcript).
    -- The same switch as bone.o.apply("show_reasoning!"); it redraws on its own.
    bone.o.apply("show_reasoning!")
    bone.settings.set("tui.show_reasoning", bone.o.show_reasoning)
  end,

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

-- The conversation sidebar (ctrl+o, /sessions), implemented entirely in Lua.
function bone.ui.sessions()
  return require("bone.ui.sessions").open()
end

-- Matching slash commands while typing "/…", attached to the prompt.
-- ctx = { items = { { name, desc } }, selected, width, height };
function bone.ui.suggestions(ctx)
  local name_w, desc_w = 0, 0
  for _, it in ipairs(ctx.items) do
    name_w = math.max(name_w, bone.text.width(it.name) + 1)
    desc_w = math.max(desc_w, bone.text.width(it.desc))
  end
  local lines = {}
  for i, it in ipairs(ctx.items) do
    local hl = i == ctx.selected and "Selection" or "InputBackground"
    local prefix = "  "
    local name = "/" .. it.name
    local name_text = prefix .. name
    local used = bone.text.width(prefix) + name_w + 1 + desc_w
    lines[i] = {
      { name_text .. string.rep(" ", name_w + 1 - bone.text.width(name)), hl },
      { it.desc .. string.rep(" ", desc_w - bone.text.width(it.desc)), hl },
      { string.rep(" ", math.max(0, ctx.width - used)), hl },
    }
  end
  return lines
end

-- /help and F1: the Lua help browser; /help topic opens a docs section.
function bone.ui.help(topic)
  local help = require("bone.help")
  if topic and topic:match("%S") then
    return help.topic(topic)
  end
  return help.open()
end

-- /health: the TUI's checks, then the core's (over the protocol).
local MARK = { ok = { "✓ ", "DiffAdd" }, warn = { "! ", "WarningMsg" }, error = { "✗ ", "ErrorMsg" } }

local function health_lines(title, items, out)
  out[#out + 1] = { { title, "Accent" } }
  for _, it in ipairs(items) do
    local m = MARK[it.status] or MARK.warn
    local rows = {}
    for line in (tostring(it.message) .. "\n"):gmatch("(.-)\n") do
      rows[#rows + 1] = line
    end
    out[#out + 1] = { m, { it.name .. ": ", "Normal" }, { rows[1] or "", "Dim" } }
    for i = 2, #rows do
      out[#out + 1] = { { "    " .. rows[i], "Dim" } }
    end
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

local WHY = { limit = " (over compact.limit)", overflow = " (the context was full)" }

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

bone.cmd.create("compact", function(c)
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
bone.cmd.create("plugins", function(c)
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

-- First run: with no model provider at all, /setup opens by itself (unless
-- it was dismissed before; /setup is always there). An empty provider list
-- can still mean one from BONE_BASE_URL, so the core's health check decides.
bone.on("ready", function()
  if bone.settings.get("setup.skipped") then
    return
  end
  bone.model.list(function(list)
    if not list or #list > 0 then
      return
    end
    bone.request("health/check", {}, function(items)
      for _, it in ipairs(items or {}) do
        if it.name == "provider" and it.status == "error" then
          return require("bone.setup").open({ first_run = true })
        end
      end
    end)
  end)
end)
