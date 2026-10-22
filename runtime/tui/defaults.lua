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

-- ctrl+t steps through bone.o.tool_detail (defined with the views, in
-- runtime/lua/bone/ui/views.lua).
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

-- The standard key bindings (runtime/lua/bone/keys.lua), shared with the GUI.
require("bone.keys")

-- Colors (see runtime/colors/): bone.ui.setup() above loads "black".

-- /rename, /compact and /plugins are in lua/bone/commands.lua.

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
