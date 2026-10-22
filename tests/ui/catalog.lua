-- Where a catalog package runs, from its entry or its files.
local catalog = require("bone.catalog")
local function files(...)
  local out = {}
  for _, p in ipairs({ ... }) do out[#out + 1] = { path = p } end
  return out
end
eq(catalog.supports({ files = files("ui.lua") }), "both", "ui.lua runs in both")
eq(catalog.supports({ files = files("tui.lua", "core.lua") }), "tui", "tui.lua: the TUI")
eq(catalog.supports({ files = files("gui.lua") }), "gui", "gui.lua: the GUI")
eq(catalog.supports({ files = files("tui.lua", "gui.lua") }), "both", "one file each")
eq(catalog.supports({ files = files("core.lua") }), "core", "core only")
eq(catalog.supports({ files = files("tui.lua"), frontends = { "tui", "gui" } }), "both",
  "the entry's frontends list wins")
eq(catalog.supports({ kind = "tui" }), "tui", "a TUI library")
