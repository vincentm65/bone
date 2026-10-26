-- The GUI's own commands: /gui shows and chooses the GUI's library, which
-- is separate from the TUI's (/tui): standard (graphite, bubbles and cards,
-- drawn like the Android app), bare (flat text), or a copy of yours under
-- libraries/ that has a gui/ folder.
local function error_text(e)
  return (tostring(e):gsub("^runtime error: ", ""):gsub("\nstack traceback:.*$", ""))
end

local function usage()
  bone.notify("usage: /gui [use standard|bare|NAME], /gui create NAME [bare], /gui delete NAME", "error")
end

local DETAILS = {
  standard = "Bubbles, cards and the sessions sidebar",
  bare = "Flat, square, every item as plain text",
}

local function menu()
  local info = bone.gui.library.info()
  local names = { "standard", "bare" }
  for _, n in ipairs(info.custom) do names[#names + 1] = n end
  table.sort(names)
  local selected = 1
  for i, n in ipairs(names) do
    if n == info.active then selected = i break end
  end
  require("bone.nodes.popups").menu({
    title = "GUI library",
    note = "Choose how the desktop app looks. Your custom copies stay in ~/.bone/libraries.",
    selected = selected, empty = "No GUI libraries found.",
    items = function()
      local out = {}
      for _, name in ipairs(names) do
        out[#out + 1] = {
          label = name == "standard" and "Standard" or name == "bare" and "Bare" or name,
          detail = DETAILS[name] or "Your copy in ~/.bone/libraries",
          badge = name == info.active and "IN USE" or nil,
        }
      end
      return out
    end,
    choose = function(i) if names[i] then bone.gui.library.use(names[i]) end end,
    hints = "↑↓ move · enter use · esc close",
  })
end

bone.cmd.create("gui", function(c)
  local word, name, from = c.args:match("^%s*(%S*)%s*(%S*)%s*(%S*)")
  if word == "use" and name ~= "" then
    return bone.gui.library.use(name)
  end
  if word == "create" or word == "delete" then
    if name == "" then return usage() end
    local ok, out = pcall(bone.library[word], name, from ~= "" and from or nil)
    if not ok then
      return bone.notify(error_text(out), "error")
    end
    if word == "delete" then
      return bone.notify(("Deleted %s (moved to %s)."):format(name, out))
    end
    return bone.notify(("Created %s.\nEdit: %s\nActivate with: /gui use %s"):format(name, out, name))
  end
  if word ~= "" then return usage() end
  menu()
end, {
  desc = "the GUI's library: standard, bare or your copies; /gui use|create|delete NAME",
  complete = function(ctx)
    local argv = ctx.argv or {}
    if #argv <= 1 and not (ctx.args or ""):match("%s$") then
      return {
        { value = "use", desc = "switch library" },
        { value = "create", desc = "copy standard (or NAME bare) to edit" },
        { value = "delete", desc = "remove a copy not in use" },
      }
    end
    if argv[1] ~= "use" and argv[1] ~= "delete" then return {} end
    local info = bone.gui.library.info()
    local out = {}
    if argv[1] == "use" then
      for _, n in ipairs({ "standard", "bare" }) do out[#out + 1] = { value = n } end
    end
    for _, n in ipairs(info.custom) do out[#out + 1] = { value = n } end
    return out
  end,
})

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
for _, key in ipairs({ "ctrl+]", "ctrl+5" }) do
  bone.keymap.set(key, function() return terminal.toggle() end, {
    desc = "open or focus a terminal; press again to return to the prompt",
  })
end
