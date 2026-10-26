-- The node library's window: the chat with its welcome and "jump to
-- latest", the composer, tray and status bar under it, the sessions
-- sidebar, docked panels, and what floats over them. Everything is a node;
-- the pieces are files of their own (bone.nodes.composer, .statusbar,
-- .overlays, .popups, bone.desktop.sidebar, .tray), so replace one, or
-- this file, to draw the window differently.
local M = {}

local nodes = require("bone.nodes")
local composer = require("bone.nodes.composer")
local statusbar = require("bone.nodes.statusbar")
local overlays = require("bone.nodes.overlays")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local tray = require("bone.desktop.tray")

local function slot(name, ctx) return bone.gui.slots.nodes(name, ctx) end

M.max_width = 1040 -- the chat and composer column, in pixels
M.spinner = widgets.spin -- the status bar's: { dots, interval } (ms to go round)

-- ---- docked panels: those drawn as nodes (bone.ui.panel.open{ node = fn }) --

--- Panels placed by a window manager rather than a dock. Keep them visible
--- in the shared panel API so they can still receive keyboard focus.
M.embedded = {}

--- Optional manager hook: attach_panel(id) embeds an existing panel; nil by default.
-- Set by a catalog/window manager, not by the shared panel API.
M.attach_panel = nil

--- Reserve a panel for a manager. Only the latest owner can release it;
--- releasing twice is harmless, and never closes the shared panel.
function M.embed(id)
  local owner = {}
  M.embedded[id] = owner
  bone.gui.invalidate()
  return function()
    if M.embedded[id] ~= owner then return end
    M.embedded[id] = nil
    bone.gui.invalidate()
  end
end

--- A panel's content in its allocated pixels, without docking chrome.
--- ctx.width/height are the fallback until the first placement.
--- The shared panel API still owns keyboard focus and key handling.
function M.panel(id, ctx)
  local panel = bone.ui.panel.get(id)
  local info = panel and panel:info()
  local draw = info and not info.hidden and bone.gui.panel_node(id)
  if not draw then return nil end
  ctx = ctx or {}
  local stable = 'panel:' .. id
  local rect = bone.gui.rect(stable)
  local width, height = rect and rect.w or ctx.width or 0, rect and rect.h or ctx.height or 0
  local ok, content = pcall(draw, {
    id = id, dock = info.dock, width = width, height = height,
    cell = ctx.cell, focused = bone.ui.panel.focused() == id,
  })
  if not ok then content = { text = tostring(content), hl = "ErrorMsg", selectable = true } end
  return {
    id = stable, grow = 1, height = 0, direction = "column", overflow = "hidden",
    on_press = info.focusable and function() bone.ui.panel.focus(id) end or nil,
    content and { grow = 1, direction = "column", content } or nil,
  }
end

local function dock_size(info, ctx, sideways)
  local cell = ctx.cell or bone.gui.window_info().cell or { w = 8, h = 18 }
  local room = sideways and ctx.width or ctx.height
  local size = tonumber(info.size)
  if size and size < 1 then return math.max(0, room * size) end
  local cells = size or tonumber(info.max) or (sideways and 30 or 10)
  return math.max(0, math.min(room, cells * (sideways and cell.w or cell.h) + (sideways and 24 or 16)))
end

--- The nodes docked on `side` ("left", "right", "top" or "bottom").
function M.docked(side, ctx, available)
  local out = {}
  ctx = ctx or {}
  local sideways = side == "left" or side == "right"
  local remaining = available or (sideways and ctx.width or ctx.height)
  for _, info in ipairs(bone.ui.panel.list()) do
    if info.dock == side and not info.hidden and not M.embedded[info.id] and bone.gui.panel_node(info.id) then
      local size = math.min(remaining, dock_size(info, ctx, sideways))
      remaining = remaining - size
      local width, height = sideways and size or ctx.width, sideways and ctx.height or size
      out[#out + 1] = style.finish({
        width = sideways and size or nil, height = not sideways and size or nil,
        bg = "Panel", direction = "column", padding = { 8, 6, 8, 6 }, overflow = "hidden",
        M.panel(info.id, { width = math.max(0, width - 12), height = math.max(0, height - 16), cell = ctx.cell }),
      })
    end
  end
  return out
end

-- ---- the chat -------------------------------------------------------------

local function welcome()
  local s = bone.chat.session()
  if bone.api.session() or (s and (s.items > 0 or s.loading)) then return nil end
  local title, hint
  if bone.settings.get("setup.skipped") then
    title = "No model provider configured."
    hint = "Run /setup to connect one · /health to diagnose setup"
  else
    title = "New session. Type a message and press enter."
    hint = "/help lists commands · ctrl+o opens an earlier session"
  end
  return {
    position = "absolute", left = 0, right = 0, top = 0, bottom = 0,
    direction = "column", justify = "center", align = "center",
    style.finish(widgets.surface({
      gap = 6, align = "center", radius = style.radius.lg, border = 0, padding = { 14, 28, 14, 28 },
      { text = title, hl = "Normal", size = style.size.lead, text_align = "center" },
      { text = hint, hl = "Dim", size = style.size.caption, text_align = "center" },
    })),
  }
end

local function jump()
  local i = bone.gui.scroll_info("chat")
  if not i or i.offset >= i.max - 4 then return nil end
  return {
    position = "absolute", bottom = 10, left = 0, right = 0, direction = "row", justify = "center",
    style.finish(widgets.press(widgets.surface({
      text = "↓  Jump to latest", hl = "Accent", size = style.size.caption,
      padding = { 6, 14, 6, 14 }, radius = style.radius.round,
    }), function() bone.gui.scroll("chat", "end") end, "button")),
  }
end

-- ---- the window -----------------------------------------------------------

-- The chat remains an opaque rounded surface with its shadow. The sidebar
-- keeps its own near-opaque background, even when the window is see-through.
local function see_through()
  local a = bone.gui.window_info().opacity or 1
  return a < 1 and a or nil
end

--- One chat's column: the chat, with the composer, tray and status bar
--- under it. opts: `width` (pixels), `chat` (a chat number; nil or the chat
--- on screen gets the editor, any other its read-only draft), `on_focus` (a
--- press on that draft), `gutter` (pixels each side; default centers M.max_width).
function M.column(ctx, opts)
  opts = opts or {}
  local n = opts.chat
  local session = bone.chat.session(n ~= nil and { session = tostring(n) } or nil)
  local here = n == nil or (session and session.current)
  local width = opts.width or ctx.width
  local gutter = opts.gutter or math.max(24, (width - M.max_width) / 2)
  local chat = { grow = 1, direction = "column", padding = { 0, gutter, 0, gutter } }
  if here then nodes.append(chat, slot("chat.top", ctx)) end
  chat[#chat + 1] = nodes.chat({
    chat = not here and n or nil,
    width_px = width - 2 * gutter, cell = ctx.cell, padding = { 14, 14, 14, 0 },
  })
  if here then
    chat[#chat + 1] = welcome()
    chat[#chat + 1] = jump()
  end
  local below = { direction = "column", padding = { 0, gutter, 12, gutter }, gap = 4 }
  below[#below + 1] = composer.node({ session = session, chat = not here and n or nil, on_focus = opts.on_focus })
  below[#below + 1] = here and tray.node() or nil
  below[#below + 1] = statusbar.node(session, ctx.cell)
  return { grow = 1, direction = "column", chat, below }
end

--- The chats beside the sidebar: the chat on screen's column. A plugin
--- replaces it to show several (ctx = { width, height, cell } of the area).
function M.area(ctx)
  return M.column(ctx, { width = ctx.width })
end

function M.view(ctx)
  local sidebar = require("bone.desktop.sidebar")
  local side, out, backdrop = sidebar.node(ctx)
  local alpha = see_through()
  local chat_bg = alpha and (bone.hl.get("Normal") or {}).bg or "Normal"
  -- The chat is a rounded surface over the window, which shows grey where
  -- the sidebar slides out from under it; closed, the chat fills the window.
  -- A narrow window gives the sidebar no room of its own: it floats over.
  -- Beside it, the chat lies over its edge (sidebar.under) by the corners.
  local under = math.floor(sidebar.under * out)
  local room = (side and not backdrop) and side.width - under or 0
  local main = style.finish({
    grow = 1, width = 0, direction = "column", bg = chat_bg, overflow = "hidden",
    radius = style.radius.round * out, margin = { 0, 8 * out, 0, -under },
    shadow = out > 0 and style.shadow.surface or nil,
  })
  local width, height = ctx.width, ctx.height
  local left = M.docked("left", ctx, width)
  for _, panel in ipairs(left) do width = width - panel.width end
  local right = M.docked("right", ctx, width)
  for _, panel in ipairs(right) do width = width - panel.width end
  local dock_ctx = { width = width, height = height, cell = ctx.cell }
  local top = M.docked("top", dock_ctx, height)
  for _, panel in ipairs(top) do height = height - panel.height end
  local bottom = M.docked("bottom", dock_ctx, height)
  for _, panel in ipairs(bottom) do height = height - panel.height end
  local area = { width = math.max(0, width - room - 8 * out), height = height, cell = ctx.cell }
  local ok, node = pcall(M.area, area)
  -- A broken replacement leaves the one chat, and says so once.
  if not ok then
    if M.area_error ~= node then bone.notify("window area: " .. tostring(node), "error") end
    M.area_error = node
    node = M.column(area, { width = area.width })
  end
  main[#main + 1] = node
  local root = { direction = "row", bg = not alpha and "Normal" or nil, hl = "Normal" }
  nodes.append(root, left)
  nodes.append(root, slot("window.left", ctx))
  local center = { grow = 1, width = 0, direction = "column" }
  nodes.append(center, top)
  local middle = { grow = 1, height = 0, direction = "row" }
  middle[#middle + 1] = not backdrop and side or nil
  middle[#middle + 1] = main
  center[#center + 1] = middle
  nodes.append(center, bottom)
  root[#root + 1] = center
  nodes.append(root, slot("window.right", ctx))
  nodes.append(root, right)
  root[#root + 1] = backdrop
  root[#root + 1] = backdrop and side or nil
  nodes.append(root, overlays.nodes(ctx))
  nodes.append(root, slot("window.overlay", ctx))
  -- Windows of bone.ui.popup / win, over everything.
  nodes.append(root, require("bone.nodes.popups").nodes())
  return root
end

--- opts: `bare`, the window without its decoration.
function M.setup(opts)
  M.bare = opts and opts.bare or false
  require("bone.nodes.style").plain = M.bare
  tray.setup()
  statusbar.setup()
  overlays.setup()
  package.loaded["bone.config"] = require("bone.desktop.settings")
  -- The windows the runtime opens itself, drawn as nodes.
  local windows, pick = bone.ui.windows, require("bone.desktop.pick")
  windows.setup = require("bone.desktop.setup").view
  windows.catalog = require("bone.desktop.catalog").view
  windows.select, windows.pager = pick.select, pick.pager
  bone.ui.sessions = require("bone.desktop.sidebar").open
  bone.ui.health = require("bone.desktop.help").health
  function bone.ui.help(topic)
    local help = require("bone.desktop.help")
    if topic and topic:match("%S") then return help.topic(topic) end
    return help.open()
  end
  bone.cmd.create("attach", function(c)
    local path = c.args:gsub("^%s+", ""):gsub("%s+$", "")
    if path == "" then return require("bone.desktop.attach").pick() end
    bone.prompt.attach(path)
  end, { desc = "add files (a dialog), or attach an image by path" })
  bone.on("submit", function() bone.gui.scroll("chat", "end") end)
  -- Files dropped on the window: images attach, other paths go in the message.
  bone.on("drop", function(ev) require("bone.desktop.attach").add(ev.paths or {}) end)
  -- An answer that finishes while the window is away: a notification.
  bone.on("turn/finished", function(ev)
    local s = bone.chat.session()
    if bone.gui.window_info().focused or not s or ev.session_id ~= s.session_id then return end
    local status = type(ev.outcome) == "table" and ev.outcome.status
    if status == "cancelled" then return end
    local title = status == "failed" and "bone: the turn failed" or "bone finished"
    bone.gui.notify(title, status == "failed" and ev.outcome.message or (s.title or ""))
  end)
  bone.gui.view(M.view)
end

return M
