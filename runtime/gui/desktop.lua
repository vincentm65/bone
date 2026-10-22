-- The desktop library (/gui use desktop): the standard UI, drawn like the
-- bone Android app. Your messages sit in rounded bubbles, answers are
-- proportional text with Markdown laid out by the window, tool calls stay
-- monospace rows, the composer is a card, and the colors are the app's
-- graphite. It is all Lua over the shared UI: the modules are in
-- runtime/lua/bone/desktop/, and ~/.bone/ui.lua and gui.lua run after this.

-- The standard UI first, then its looks replaced.
require("bone.ui").setup()
require("bone.commands")
require("bone.menu")
require("bone.keys")
require("bone.gui")
require("bone.desktop.theme").setup()
require("bone.desktop.views").setup()
require("bone.desktop.layout").setup()

-- The font every grid line is drawn in (blocks use the window's
-- proportional font): a monospace font file (empty for the built-in one),
-- and its size in points.
bone.o.define("font", "", { desc = "monospace font file for the GUI (empty: built in)" })
bone.o.define("font_size", 14, { desc = "GUI font size in points", type = "number" })
