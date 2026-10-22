-- The GUI's defaults: the standard UI (runtime/lua/bone/ui/), the same
-- modules the TUI draws with, the slash commands and the / menu, and the
-- standard keys. Your ~/.bone/ui.lua and gui.lua run after this.
require("bone.ui").setup()
require("bone.commands")
require("bone.menu")
require("bone.keys")
