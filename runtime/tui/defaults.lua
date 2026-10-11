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
  return tray.cancel_edit() or (bone.prompt.empty() and tray.back())
end
local menu_down = bone.ui.actions.down
bone.ui.actions.down = function()
  if menu_down and menu_down() then
    return true
  end
  return bone.prompt.empty() and not bone.prompt.info().history and tray.enter()
end

-- Up on an empty prompt edits the last queued message (enter saves it in
-- place; the tray's Queue page edits any of them).
local menu_up = bone.ui.actions.up
bone.ui.actions.up = function()
  if menu_up and menu_up() then
    return true
  end
  local s = bone.chat.session()
  if not bone.prompt.empty() or not s or not s.session_id then
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
  ["ctrl+v"] = "paste_clipboard",
  ["super+v"] = "paste_clipboard",
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

-- /rename, /fork, /compact and /plugins are in lua/bone/commands.lua.

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
