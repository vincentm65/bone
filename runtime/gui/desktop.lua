-- The desktop library (/gui use desktop): the GUI's own UI, styled after
-- the bone Android app and apart from the TUI's: its colors (graphite), its
-- views for every kind of item, tool calls included, its layout, composer,
-- status bar, sessions sidebar and tray. What it shares with the TUI is
-- behaviour, not looks: the commands, keys and the `/` menu, and the data
-- modules (bone.chats, bone.background, bone.usage, bone.timing). It is all
-- Lua, in runtime/lua/bone/desktop/; ~/.bone/ui.lua and gui.lua run after.

bone.colorscheme("graphite")
require("bone.commands")
require("bone.menu")
require("bone.keys")
require("bone.gui")
-- First run: with no model provider, /setup opens by itself.
require("bone.setup").first_run()
-- The data the sidebar, tray and status bar draw (tracking from the start).
require("bone.chats")
require("bone.background")
require("bone.desktop.views").setup()
require("bone.desktop.layout").setup()
require("bone.desktop.statusline").setup()
require("bone.desktop.tray").setup()

-- The font every grid line is drawn in (blocks use the window's
-- proportional font): a monospace font file (empty for the built-in one),
-- and its size in points.
bone.o.define("font", "", { desc = "monospace font file for the GUI (empty: built in)" })
bone.o.define("font_size", 14, { desc = "GUI font size in points", type = "number" })
-- The proportional text's faces (font files; empty: the system's sans).
bone.o.define("font_sans", "", { desc = "proportional font file for the GUI (empty: the system's)" })
bone.o.define("font_sans_bold", "", { desc = "its bold face (empty: the system's)" })
bone.o.define("font_sans_italic", "", { desc = "its italic face (empty: the system's)" })
