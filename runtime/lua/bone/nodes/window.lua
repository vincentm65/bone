-- The node library's window: the chat with its welcome and "jump to
-- latest", the composer, tray and status bar under it, the sessions
-- sidebar, docked panels, and what floats over them. Everything is a node;
-- the pieces are files of their own (bone.nodes.composer, .statusbar,
-- .overlays, .popups, bone.desktop.sidebar, .tray),
-- so replace one, or
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
--- ctx.auto: as tall as the content (up to ctx.height) rather than all of it.
function M.panel(id, ctx)
  local panel = bone.ui.panel.get(id)
  local info = panel and panel:info()
  local draw = info and not info.hidden and bone.gui.panel_node(id)
  if not draw then return nil end
  ctx = ctx or {}
  local stable = 'panel:' .. id
  local rect = not ctx.auto and bone.gui.rect(stable)
  local width, height = rect and rect.w or ctx.width or 0, rect and rect.h or ctx.height or 0
  local ok, content = pcall(draw, {
    id = id, dock = info.dock, width = width, height = height,
    cell = ctx.cell, focused = bone.ui.panel.focused() == id,
  })
  if not ok then content = { text = tostring(content), hl = "ErrorMsg", selectable = true } end
  return {
    id = stable, grow = not ctx.auto and 1 or nil, height = not ctx.auto and 0 or nil,
    direction = "column", overflow = "hidden",
    on_press = info.focusable and function() bone.ui.panel.focus(id) end or nil,
    content and { grow = not ctx.auto and 1 or nil, direction = "column", content } or nil,
  }
end

-- A docked panel's title, as the terminal draws one over it.
local function heading(title)
  if type(title) ~= "string" or title == "" then return nil end
  return style.finish({
    text = title, hl = "PanelTitle", bold = true, size = style.size.caption, wrap = false,
    text_overflow = "ellipsis", padding = { 0, 4 }, margin = { 0, 0, 6, 0 }, shrink = 0,
  })
end

-- A docked panel is a card (bone.nodes.widgets.card) standing `M.dock_inset`
-- off the window's edges, its content `M.dock_pad` ({ v, h }) inside it.
M.dock_inset = style.space.lg
M.dock_pad = { 10, 12 }

-- The pixels around a dock's content: the inset, the padding, the border.
local function chrome(sideways, flush)
  return (flush and 0 or M.dock_inset) + 2 * (sideways and M.dock_pad[2] or M.dock_pad[1]) + 2
end

local function dock_size(info, ctx, sideways, flush)
  local cell = ctx.cell or bone.gui.window_info().cell or { w = 8, h = 18 }
  local room = sideways and ctx.width or ctx.height
  local size = tonumber(info.size)
  if size and size < 1 then return math.max(0, room * size) end
  local cells = size or tonumber(info.max) or (sideways and 30 or 10)
  return math.max(0, math.min(room, cells * (sideways and cell.w or cell.h) + chrome(sideways, flush)))
end

-- The insets of a dock on `side`: away from the window's edges, none
-- toward the chat (its own margins keep it off).
local function insets(side)
  local i = M.dock_inset
  if side == "left" then return { i, 0, i, i } end
  if side == "right" then return { i, i, i, 0 } end
  if side == "top" then return { i, i, 0, i } end
  return { 0, i, i, i }
end

--- The nodes docked on `side` ("left", "right", "top" or "bottom"), and
--- the pixels they take from it. Each is a card, its `title` over it. One
--- above or below the chat with `size = "auto"` is as tall as its content,
--- up to its `max` rows (default 10). `flush`: no inset (the bottom ones,
--- which stand in the chat's column over the composer, lined up with it).
function M.docked(side, ctx, available, flush)
  local out, used = {}, 0
  ctx = ctx or {}
  local sideways = side == "left" or side == "right"
  local remaining = available or (sideways and ctx.width or ctx.height)
  for _, info in ipairs(bone.ui.panel.list()) do
    if info.dock == side and not info.hidden and not M.embedded[info.id] and bone.gui.panel_node(info.id) then
      local size = math.min(remaining, dock_size(info, ctx, sideways, flush))
      local auto = not sideways and info.size == "auto"
      local id = "dock:" .. info.id
      -- An auto panel takes what it took last frame (the cap before then).
      local took = auto and math.min(size, (bone.gui.rect(id) or {}).h or size) or size
      remaining, used = remaining - took, used + took
      local width, height = sideways and size or ctx.width, sideways and ctx.height or size
      local title = heading(info.title)
      local pad = flush and { 0, 0, 0, 0 } or insets(side)
      local inner_w = width - pad[2] - pad[4] - 2 * M.dock_pad[2] - 2
      local inner_h = height - pad[1] - pad[3] - 2 * M.dock_pad[1] - 2 - (title and 26 or 0)
      out[#out + 1] = {
        id = id, width = sideways and size or nil,
        height = not sideways and not auto and size or nil, max_height = auto and size or nil,
        direction = "column", padding = pad,
      }
      local card = widgets.card({ grow = not auto and 1 or nil, padding = M.dock_pad, overflow = "hidden" })
      card[#card + 1] = title
      card[#card + 1] = M.panel(info.id, {
        width = math.max(0, inner_w), height = math.max(0, inner_h), cell = ctx.cell, auto = auto,
      })
      out[#out][1] = card
    end
  end
  return out, used
end

-- ---- the chat -------------------------------------------------------------

local function welcome(width)
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
  -- Keep the welcome card's chrome from consuming the whole chat column on
  -- a phone-sized window. Text still wraps, but at a readable size.
  local compact = width < 240
  local pad = compact and math.min(12, math.max(4, width / 10)) or 28
  return {
    position = "absolute", left = 0, right = 0, top = 0, bottom = 0,
    direction = "column", justify = "center", align = "center",
    style.finish(widgets.surface({
      gap = compact and 4 or 6, align = "center", radius = style.radius.lg, border = 0,
      padding = { compact and 10 or 14, pad, compact and 10 or 14, pad },
      { text = title, hl = "Normal", size = compact and style.size.body or style.size.lead, text_align = "center" },
      { text = hint, hl = "Dim", size = compact and style.size.small or style.size.caption, text_align = "center" },
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

-- The chat remains an opaque surface, even when the window is see-through.
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
  -- The usual 24px gutter is comfortable, but would leave no chat column on
  -- a very narrow window. Let it scale down before the composer does.
  local gutter = opts.gutter or (width > M.max_width
    and (width - M.max_width) / 2 or math.min(24, width / 10))
  local content_width = math.max(0, width - 2 * gutter)
  local chat = { grow = 1, direction = "column", padding = { 0, gutter, 0, gutter } }
  if here then nodes.append(chat, slot("chat.top", ctx)) end
  chat[#chat + 1] = nodes.chat({
    chat = not here and n or nil,
    width_px = content_width, cell = ctx.cell, padding = { 14, 14, 14, 0 },
  })
  if here then
    chat[#chat + 1] = welcome(content_width)
    chat[#chat + 1] = jump()
  end
  local below = { direction = "column", padding = { 0, gutter, 12, gutter }, gap = 4 }
  -- Panels docked at the bottom stand over the composer, as in the terminal
  -- they stand over the prompt.
  if here then
    local docks = M.docked("bottom", { width = width - 2 * gutter, height = ctx.height, cell = ctx.cell },
      ctx.height and ctx.height / 2, true)
    for _, d in ipairs(docks) do d.margin = { 0, 0, 4, 0 } end
    nodes.append(below, docks)
  end
  below[#below + 1] = composer.node({ session = session, width = content_width,
    chat = not here and n or nil, on_focus = opts.on_focus })
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
  local side, room, backdrop = sidebar.node(ctx)
  local alpha = see_through()
  local chat_bg = alpha and (bone.hl.get("Normal") or {}).bg or "Normal"
  -- The chat fills the window. The sidebar is a card on it, in a lane at
  -- its left that the chat makes room for; a narrow window has no room for
  -- one, and it floats over.
  local main = style.finish({
    grow = 1, width = 0, direction = "row", bg = chat_bg, overflow = "hidden",
  })
  room = (side and not backdrop) and room or 0
  local width, height = ctx.width, ctx.height
  local left, used = M.docked("left", ctx, width)
  width = width - used
  local right
  right, used = M.docked("right", ctx, width)
  width = width - used
  local dock_ctx = { width = width, height = height, cell = ctx.cell }
  local top
  top, used = M.docked("top", dock_ctx, height)
  height = height - used
  -- The lanes the docks stand in are the chat's surface too: a see-through
  -- window would show the desktop around their cards.
  for _, docks in ipairs({ left, right, top }) do
    for _, d in ipairs(docks) do d.bg = chat_bg end
  end
  local area = { width = math.max(0, width - room), height = height, cell = ctx.cell }
  local ok, node = pcall(M.area, area)
  -- A broken replacement leaves the one chat, and says so once.
  if not ok then
    if M.area_error ~= node then bone.notify("window area: " .. tostring(node), "error") end
    M.area_error = node
    node = M.column(area, { width = area.width })
  end
  main[#main + 1] = not backdrop and side or nil
  main[#main + 1] = { grow = 1, width = 0, direction = "column", node }
  local root = { direction = "row", bg = not alpha and "Normal" or nil, hl = "Normal" }
  nodes.append(root, left)
  nodes.append(root, slot("window.left", ctx))
  local center = { grow = 1, width = 0, direction = "column" }
  nodes.append(center, top)
  local middle = { grow = 1, height = 0, direction = "row", main }
  center[#center + 1] = middle
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
