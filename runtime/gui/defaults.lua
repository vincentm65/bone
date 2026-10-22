-- The GUI's defaults: the standard UI (runtime/lua/bone/ui/), the same
-- modules the TUI draws with, the slash commands and the / menu, and the
-- standard keys. Your ~/.bone/ui.lua and gui.lua run after this.
require("bone.ui").setup()
require("bone.commands")
require("bone.menu")
require("bone.keys")

-- The font every line is drawn in: a monospace font file (empty for the
-- built-in one), and its size in points.
bone.o.define("font", "", { desc = "monospace font file for the GUI (empty: built in)" })
bone.o.define("font_size", 14, { desc = "GUI font size in points", type = "number" })
