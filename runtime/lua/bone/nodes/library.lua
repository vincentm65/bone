-- The GUI's library: the window as a tree of nodes (bone.gui.view), drawn
-- by bone.nodes.window with the item views of bone.desktop.views (each a
-- node). The commands, keys and options are the shared ones. `plain` leaves
-- the item views out: every item is its text, and the window is drawn
-- without its decoration (square, flat).
local M = {}

-- The zoom's steps, as a browser's.
local ZOOM = { 0.5, 0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3 }

--- Zoom the window a step in (`dir` 1) or out (-1), or back to 1 (0), and
--- keep it (settings.json gui.zoom).
function M.zoom(dir)
  local now, to = tonumber(bone.o.zoom) or 1, 1
  if dir > 0 then
    to = ZOOM[#ZOOM]
    for _, z in ipairs(ZOOM) do if z > now + 0.01 then to = z break end end
  elseif dir < 0 then
    to = ZOOM[1]
    for i = #ZOOM, 1, -1 do if ZOOM[i] < now - 0.01 then to = ZOOM[i] break end end
  end
  bone.o.zoom = to
  bone.settings.set("gui.zoom", to)
  bone.notify(("Zoom %d%%"):format(math.floor(to * 100 + 0.5)))
  bone.gui.invalidate()
end

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
  -- The whole window's zoom, over the system's scale: ctrl+= and ctrl+-
  -- step it, ctrl+0 puts it back (where a panel with the keys has no use
  -- for them).
  bone.o.define("zoom", 1, { desc = "GUI zoom, 0.5-3 (1: the system's size)", type = "number" })
  for key, dir in pairs({ ["ctrl+="] = 1, ["ctrl++"] = 1, ["ctrl+-"] = -1, ["ctrl+0"] = 0 }) do
    bone.keymap.set(key, function() M.zoom(dir) end)
  end
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

  -- macOS and Windows: the window draws its own title bar
  -- (bone.nodes.titlebar) instead of the system's; takes effect when the
  -- window opens.
  bone.o.define("titlebar", true, { desc = "draw the title bar in the window's colors on macOS and Windows (on restart)" })

  require("bone.nodes.window").setup({ bare = opts.plain })
end

return M
