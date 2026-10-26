-- The GUI's standard library (the default): the window as a tree of nodes, in
-- Lua (runtime/lua/bone/nodes/): graphite colors, bubbles for your messages,
-- proportional Markdown answers, tool calls as cards, a card composer, the
-- sessions sidebar, the tray and a status bar. Its looks are the GUI's
-- alone; what it shares with the TUI is behaviour (commands, keys, the `/`
-- menu, options) and data. ~/.bone/ui.lua and gui.lua run after.
require("bone.nodes.library").setup()
