-- The GUI's library: the window as a tree of nodes (bone.gui.view), drawn
-- by bone.nodes.window with the item views of bone.desktop.views (each a
-- node). The commands, keys and options are the shared ones. `plain` leaves
-- the item views out: every item is its text, and the window is drawn
-- without its decoration (square, flat).
local M = {}

function M.setup(opts)
  opts = opts or {}
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
  if not opts.plain then require("bone.desktop.views").setup() end

  -- The monospace font (`font = "mono"`: code, tool output, Lua's windows
  -- of lines; the rest is the proportional font): a font file (empty for
  -- the built-in one), and its size in points.
  bone.o.define("font", "", { desc = "monospace font file for the GUI (empty: built in)" })
  bone.o.define("font_size", 14, { desc = "GUI font size in points", type = "number" })
  -- The proportional text's faces (font files; empty: the system's sans).
  bone.o.define("font_sans", "", { desc = "proportional font file for the GUI (empty: the system's)" })
  bone.o.define("font_sans_bold", "", { desc = "its bold face (empty: the system's)" })
  bone.o.define("font_sans_italic", "", { desc = "its italic face (empty: the system's)" })

  -- The corners of what the window draws itself (the prompt's field).
  bone.o.define("scroll_line", 20, { desc = "pixels of chat one scroll-key line moves", type = "number" })
  bone.o.define("wheel_speed", 1.6, { desc = "how far the wheel scrolls, times the system's", type = "number" })
  local corner = not opts.plain
  bone.o.define("widget_radius", corner and 6 or 0, { desc = "corner radius of native widgets, px", type = "number" })
  bone.o.define("window_radius", corner and 8 or 0, { desc = "corner radius of native windows, px", type = "number" })
  -- Below 1 the window's background lets the desktop show through (a
  -- compositor is needed); takes effect when the window opens. Bare is
  -- opaque.
  bone.o.define("opacity", opts.plain and 1 or 0.9, { desc = "window background opacity, 0-1 (on restart)", type = "number" })

  require("bone.nodes.window").setup({ bare = opts.plain })
end

return M
