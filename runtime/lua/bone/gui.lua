-- The GUI's own commands: /gui shows and chooses the GUI's library, which
-- is separate from the TUI's (/tui): standard (the shared UI, as the TUI
-- draws it), bare, desktop (drawn like the Android app), or a copy of
-- yours under libraries/ that has a gui/ folder.
bone.cmd.create("gui", function(c)
  local word, name = c.args:match("^%s*(%S*)%s*(%S*)")
  if word == "use" and name ~= "" then
    return bone.gui.library.use(name)
  end
  if word ~= "" then
    return bone.notify("usage: /gui [use standard|bare|desktop|NAME]", "error")
  end
  local info = bone.gui.library.info()
  local names = { "standard", "bare" }
  for _, n in ipairs(info.builtin) do names[#names + 1] = n end
  for _, n in ipairs(info.custom) do names[#names + 1] = n end
  local shown = {}
  for _, n in ipairs(names) do
    shown[#shown + 1] = (n == info.active and "● " or "  ") .. n
  end
  bone.notify("GUI library: " .. table.concat(shown, "  ") .. "  (/gui use NAME)")
end, { desc = "show or choose the GUI's library: standard, bare, desktop" })

-- Terminals in panels (/term, ctrl+]): runtime/lua/bone/terminal.lua. A
-- plugin that defines /term too (the term plugin) replaces these.
local terminal = require("bone.terminal")
terminal.setup()
bone.cmd.create("term", function(c)
  local command = (c.args or ""):match("^%s*(.-)%s*$")
  if command == "close" then
    return terminal.close() or bone.notify("no terminal is open", "error")
  end
  return terminal.open(nil, { cmd = command ~= "" and command or nil })
end, {
  desc = "open a terminal panel; /term [command], /term close (ctrl+])",
  aliases = { "terminal" },
})
bone.keymap.set("ctrl+]", function() return terminal.toggle() end, {
  desc = "open or focus a terminal; press again to return to the prompt",
})
