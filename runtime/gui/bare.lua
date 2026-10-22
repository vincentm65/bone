-- The bare library in the GUI (/tui use bare, shared with the TUI) loads
-- this in place of defaults.lua: the plain UI Rust draws by itself (chat as
-- text, the prompt as a text box) with the basic keys and the standard
-- slash commands. Plugins still load; ~/.bone/ui.lua, gui.lua and the
-- project's do not.

for key, action in pairs({
  ["f1"] = "/help",
  ["enter"] = "submit",
  ["alt+enter"] = "newline",
  ["shift+enter"] = "newline",
  ["ctrl+j"] = "newline",
  ["esc"] = "dismiss",
  ["ctrl+c"] = "interrupt",
  ["ctrl+d"] = "quit_if_empty",
  ["ctrl+o"] = "sessions",
  ["left"] = "left",
  ["right"] = "right",
  ["up"] = "up",
  ["down"] = "down",
  ["home"] = "line_start",
  ["end"] = "line_end",
  ["backspace"] = "backspace",
  ["delete"] = "delete",
  ["pageup"] = "page_up",
  ["pagedown"] = "page_down",
  wheelup = "scroll_up",
  wheeldown = "scroll_down",
}) do
  bone.keymap.set(key, action)
end

-- While a popup or a panel has the keyboard.
bone.keymap.set("ctrl+c", "interrupt", { context = "popup" })
bone.keymap.set("esc", "dismiss", { context = "panel" })
bone.keymap.set("ctrl+c", "interrupt", { context = "panel" })

-- The standard commands (/tui switches back), without the / menu:
-- enter on "/name args" runs the command instead of sending it.
require("bone.commands")
require("bone.gui")
bone.ui.actions.submit = function()
  local line = bone.prompt.get():match("^%s*/(%S.*)$")
  if not line or not bone.cmd.find(line:match("^%S+")) then
    return false
  end
  bone.prompt.history_add("/" .. line)
  bone.prompt.set("")
  bone.cmd(line)
  return true
end

function bone.ui.help(topic)
  local help = require("bone.help")
  return topic and topic:match("%S") and help.topic(topic) or help.open()
end

function bone.ui.sessions()
  return require("bone.ui.sessions").open()
end

-- /health: the GUI's checks, then the core's, as plain lines.
function bone.ui.health()
  local mark = { ok = "ok  ", warn = "!   ", error = "x   " }
  local function add(out, title, items)
    out[#out + 1] = { { title, "Normal" } }
    for _, it in ipairs(items) do
      out[#out + 1] = { { (mark[it.status] or "!   ") .. it.name .. ": " .. tostring(it.message), "Normal" } }
    end
    out[#out + 1] = {}
  end
  local lines = {}
  local gui = bone.api.health()
  for _, it in ipairs(bone._run_health()) do
    gui[#gui + 1] = it
  end
  add(lines, "GUI", gui)
  local pager = bone.ui.pager({ unpack(lines), { { "Core: checking…", "Normal" } } }, { title = "Health" })
  bone.request("health/check", {}, function(core, err)
    local all = { unpack(lines) }
    add(all, "Core", err and { { name = "core", status = "error", message = err } } or core or {})
    pager:set(all)
  end)
end

-- The font, as in the standard library.
bone.o.define("font", "", { desc = "monospace font file for the GUI (empty: built in)" })
bone.o.define("font_size", 14, { desc = "GUI font size in points", type = "number" })
