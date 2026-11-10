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

-- The chat and composer column, in pixels: 900 of text (about 120
-- characters a line of prose) and the scrollbar's inset beside it.
M.max_width = 914
-- The list reserves this much room on its right for its scrollbar. Keep the
-- composer and bottom docks on the same visual edge as the chat items.
M.chat_scrollbar_inset = 14
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
    text = title, hl = "Normal", bold = true, size = style.size.body, wrap = false,
    text_overflow = "ellipsis", padding = { 0, 4 }, margin = { 2, 0, 10, 0 }, shrink = 0,
  })
end

-- A panel docked at the window's side or top is flush with its edges, as
-- the sidebar is: the sidebar's black, a hairline on the side that faces
-- the chat, its content `M.dock_pad` ({ v, h }) inside. One over the
-- composer (`flush`) is a card lined up with it.
M.dock_inset = 0
M.dock_pad = { 12, 12 }

-- The pixels around a dock's content: the padding and the edge.
local function chrome(sideways, flush)
  return 2 * (sideways and M.dock_pad[2] or M.dock_pad[1]) + (flush and 2 or 1)
end

-- The hairline of a dock on `side`: on the edge toward the chat.
local EDGE = { left = { 0, 1, 0, 0 }, right = { 0, 0, 0, 1 }, top = { 0, 0, 1, 0 }, bottom = { 1, 0, 0, 0 } }

-- Sizes set by dragging a dock's edge, in pixels, by panel id (kept
-- between runs as "dock-size.<id>").
M.dock_px = {}

-- A state name for panel `id` (state names are letters, digits, _, - and .).
local function size_key(id)
  return "dock-size." .. tostring(id):gsub("[^%w_%-%.]", "_")
end
-- The least a dragged dock keeps, and the least it leaves the chat.
M.dock_min, M.chat_min = 160, 320

local function dragged_size(id)
  if M.dock_px[id] == nil then
    M.dock_px[id] = tonumber(bone.state.load(size_key(id))) or false
  end
  return M.dock_px[id] or nil
end

local function dock_size(info, ctx, sideways, flush)
  local cell = ctx.cell or bone.gui.window_info().cell or { w = 8, h = 18 }
  local room = sideways and ctx.width or ctx.height
  local px = info.size ~= "auto" and dragged_size(info.id)
  -- A dragged size gives way as the window narrows: beside the chat it
  -- leaves the chat its least, as the drag itself does.
  if px then return math.max(0, math.min(room, sideways and math.max(M.dock_min, room - M.chat_min) or room, px)) end
  local size = tonumber(info.size)
  if size and size < 1 then return math.max(0, room * size) end
  local cells = size or tonumber(info.max) or (sideways and 30 or 10)
  return math.max(0, math.min(room, cells * (sideways and cell.w or cell.h) + chrome(sideways, flush)))
end

-- The edge of a dock on `side` that faces the chat, as a handle: drag it
-- to set the dock's size (kept between runs). The edge stays as far from
-- the pointer as where it was grabbed. `room` is the window's width (or
-- height) the dock and the chat share.
local function edge(side, info, room)
  local id, sideways = info.id, side == "left" or side == "right"
  local grab = 0
  local handle = { position = "absolute", z = 1, cursor = sideways and "col-resize" or "row-resize" }
  local thick = 6
  if side == "right" then handle.left, handle.top, handle.bottom, handle.width = 0, 0, 0, thick
  elseif side == "left" then handle.right, handle.top, handle.bottom, handle.width = 0, 0, 0, thick
  elseif side == "bottom" then handle.top, handle.left, handle.right, handle.height = 0, 0, 0, thick
  else handle.bottom, handle.left, handle.right, handle.height = 0, 0, 0, thick end
  handle.on_press = function(pos) grab = sideways and pos.x or pos.y end
  handle.on_drag = function(pos)
    local r = bone.gui.rect("dock:" .. id)
    if not r then return end
    local size
    if side == "right" then size = r.x + r.w - pos.x + grab
    elseif side == "left" then size = pos.x - r.x + (thick - grab)
    elseif side == "bottom" then size = r.y + r.h - pos.y + grab
    else size = pos.y - r.y + (thick - grab) end
    size = math.floor(math.max(M.dock_min, math.min(room - M.chat_min, size)))
    if size ~= M.dock_px[id] then
      M.dock_px[id] = size
      bone.gui.invalidate()
    end
  end
  handle.on_release = function()
    if M.dock_px[id] then bone.state.save(size_key(id), M.dock_px[id]) end
  end
  return handle
end

-- The insets of a dock on `side`: away from the window's edges, none
-- toward the chat (its own margins keep it off).
local function insets(side)
  local i = M.dock_inset
  if i == 0 then return { 0, 0, 0, 0 } end
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
      -- A card has a border all round; a flush dock, its hairline on one edge.
      local bw = flush and 2 or (sideways and 1 or 0)
      local bh = flush and 2 or (sideways and 0 or 1)
      local inner_w = width - pad[2] - pad[4] - 2 * M.dock_pad[2] - bw
      local inner_h = height - pad[1] - pad[3] - 2 * M.dock_pad[1] - bh - (title and 32 or 0)
      out[#out + 1] = {
        id = id, width = sideways and size or nil,
        height = not sideways and not auto and size or nil, max_height = auto and size or nil,
        direction = "column", padding = pad,
      }
      local card
      if flush then
        card = widgets.card({ grow = not auto and 1 or nil, padding = M.dock_pad, overflow = "hidden" })
      else
        card = style.finish({
          grow = not auto and 1 or nil, direction = "column", padding = M.dock_pad, overflow = "hidden",
          bg = (bone.hl.get("Sidebar") or {}).bg and "Sidebar" or "Panel",
          border = EDGE[side], border_color = "WinSeparator",
        })
      end
      card[#card + 1] = title
      card[#card + 1] = M.panel(info.id, {
        width = math.max(0, inner_w), height = math.max(0, inner_h), cell = ctx.cell, auto = auto,
      })
      out[#out][1] = card
      -- Its inner edge resizes it; an auto-sized one fits its content instead.
      if not auto and not flush then
        out[#out][2] = edge(side, info, sideways and ctx.width or ctx.height)
      end
    end
  end
  return out, used
end

-- ---- the chat -------------------------------------------------------------

-- Whether chat `s` (a column's) is new: nothing in it yet, nothing running.
local function is_new(s)
  if bone.api.session() or not s then return false end
  return (s.items or 0) == 0 and not s.loading and not s.running
end

--- What a new chat offers to start with: { label, text = a message to
--- start, or cmd = a command to run }. A gui.lua can replace them.
M.suggestions = {
  { label = "Review my unstaged changes", icon = "check", text = "Review my unstaged changes" },
  { label = "Find where a function is used", icon = "search", text = "Find where this function is used: " },
  { label = "Explain a failing test", icon = "terminal", text = "Explain why this test fails: " },
}

-- A new chat leads with the composer: a short greeting over it, places to
-- start under it, and the keys that go elsewhere. `input` is the composer.
local function welcome(width, input)
  local unset = bone.settings.get("setup.skipped")
  local compact = width < 240
  local muted = (bone.hl.get("Muted") or {}).fg and "Muted" or "Dim"
  local icons = require("bone.nodes.icons")
  local chips = { direction = "row", flex_wrap = true, justify = "center", gap = 8, row_gap = 8 }
  local list = unset and {
    { label = "Connect a model", icon = "plus", cmd = "setup" },
    { label = "Check the setup", icon = "gear", cmd = "health" },
  } or M.suggestions
  for _, it in ipairs(list) do
    chips[#chips + 1] = widgets.press(style.finish({
      direction = "row", align = "center", gap = 8, height = 32, padding = { 0, 12 }, radius = 16,
      border = 1, border_color = "PromptBorder", label = it.label,
      it.icon and icons.node(it.icon, { size = 14, hl = "Dim" }) or nil,
      { text = it.label, size = style.size.body, hl = muted, wrap = false },
    }), function()
      if it.cmd then bone.cmd(it.cmd) return end
      bone.prompt.set(it.text or it.label)
      bone.gui.focus("composer-input")
    end)
  end
  local hints = widgets.hints("ctrl+o earlier chats · / commands", { 0, 0 })
  hints.justify = "center"
  return {
    grow = 1, direction = "column", justify = "center", align = "center", gap = compact and 12 or 22,
    padding = { 0, 0, compact and 8 or 40, 0 },
    { text = unset and "Connect a model to start" or "What are we building?", hl = "Normal",
      size = compact and style.size.lead or style.size.display, medium = true, text_align = "center",
      letter_spacing = compact and 0 or -0.4 },
    { width = width, direction = "column", input },
    not compact and chips or nil,
    not compact and hints or nil,
  }
end

local function jump(gutter)
  local i = bone.gui.scroll_info("chat")
  if not i or i.offset >= i.max - 4 then return nil end
  -- A small round button at the foot of the scrollbar's lane, beside the
  -- text: it covers none of it.
  return {
    position = "absolute", bottom = 4, right = math.max(0, gutter - M.chat_scrollbar_inset), z = 2,
    widgets.icon_button("arrow_down", "Jump to latest", function() bone.gui.scroll("chat", "end") end,
      { box = 28, size = 15, hl = "Normal", radius = 14, bg = "Panel", border = 1, border_color = "PromptBorder" }),
  }
end

-- ---- the window -----------------------------------------------------------

-- The chat remains an opaque surface, even when the window is see-through.
local function see_through()
  local a = bone.gui.window_info().opacity or 1
  return a < 1 and a or nil
end

-- The pixels under the chat's last item, above the composer. The list keeps
-- them as its bottom padding: at the end of the chat the last line rests
-- above them, and what scrolls down through them fades into the composer.
M.chat_fade = 32

-- The same at the top, shorter: what scrolls up out of the chat fades at
-- the window's edge instead of being cut there. It covers no more than the
-- list's top padding, so the first line at the top of a chat is clear.
M.chat_fade_top = 14

-- The fade over that padding, from clear to the chat's surface at the
-- composer's edge (`top`: at the window's). nil when the scheme's
-- background is not a hex color.
local function chat_fade(gutter, top)
  local clear, solid = style.alpha("Normal", "00", "bg"), style.alpha("Normal", "ff", "bg")
  if not clear or not solid then return nil end
  return {
    position = "absolute", left = gutter, right = gutter, z = 1, [top and "top" or "bottom"] = 0,
    height = top and M.chat_fade_top or M.chat_fade, gradient = top and { solid, clear } or { clear, solid },
  }
end

--- The header over the chat: the sidebar's button while it is closed, the
--- chat's title and its folder, a hairline under them. Off where the window
--- draws a title bar of its own (macOS, Windows), which shows the title.
M.header_height = 48

local function title_of(session)
  local t = session and session.title
  -- None yet: a placeholder ("[new session]", "[untitled]") stays out.
  if type(t) ~= "string" or not t:match("%S") or t:match("^%[.*%]$") then return nil end
  return t
end

local function short_path(cwd)
  if not cwd or cwd == "" then return nil end
  local home = os.getenv("HOME")
  if home and home ~= "" and cwd:sub(1, #home) == home then cwd = "~" .. cwd:sub(#home + 1) end
  return cwd == "~" and "~/" or cwd
end

function M.header(session, width)
  if require("bone.nodes.titlebar").offset() > 0 then return nil end
  M.menu_nodes = nil
  local icons = require("bone.nodes.icons")
  local sidebar = require("bone.desktop.sidebar")
  local title = title_of(session)
  local muted = (bone.hl.get("Muted") or {}).fg and "Muted" or "Dim"
  -- A new chat has no folder of its own yet: it starts where bone runs.
  local path = short_path((session and session.cwd) or os.getenv("PWD"))
  local bar = {
    height = M.header_height, shrink = 0, direction = "row", align = "center", gap = 10,
    padding = { 0, 16, 0, sidebar.is_open() and 20 or 12 },
  }
  if not sidebar.is_open() then
    bar[#bar + 1] = widgets.icon_button("sidebar", "Chats (ctrl+o)", function() sidebar.open() end,
      { box = 28, size = 16, hl = muted, radius = style.radius.md, id = "header-sidebar" })
  end
  bar[#bar + 1] = { text = title or "New chat", size = style.size.body, medium = title ~= nil,
    hl = title and "Normal" or muted, wrap = false, text_overflow = "ellipsis", shrink = 1 }
  -- The folder keeps its shape (the title gives way first), and goes where
  -- there is no room for it.
  if path and (width or math.huge) >= 420 then
    bar[#bar + 1] = style.finish({
      direction = "row", align = "center", gap = 6, height = 24, padding = { 0, 9, 0, 8 }, shrink = 0, max_width = 220,
      radius = 12, border = 1, border_color = "PromptBorder", tooltip = (session and session.cwd) or os.getenv("PWD"),
      icons.node("folder", { size = 13, hl = "Dim" }),
      { text = path, size = style.size.small, hl = muted, wrap = false, text_overflow = "ellipsis", shrink = 1 },
    })
  end
  -- The chat's actions, quiet at the right: a menu of what you can do to it.
  bar[#bar + 1] = { grow = 1 }
  if title then
    bar[#bar + 1] = widgets.icon_button("more", "Chat actions", function()
      M.chat_menu = not M.chat_menu
      bone.gui.invalidate()
    end, { box = 28, size = 16, hl = muted, radius = style.radius.md, id = "header-more" })
  end
  bar[#bar + 1] = { position = "absolute", left = 0, right = 0, bottom = 0, height = 1,
    bg = style.alpha("WinSeparator", "ff", "fg", "PromptBorder") }
  if M.chat_menu and title then
    local function item(icon, label, run, danger)
      return widgets.press(style.finish({
        direction = "row", align = "center", gap = 10, height = 32, padding = { 0, 10 }, radius = style.radius.md,
        icons.node(icon, { size = 15, hl = danger and "ErrorMsg" or "Dim" }),
        { text = label, size = style.size.body, hl = danger and "ErrorMsg" or "Normal", wrap = false },
      }), function() M.chat_menu = false run() bone.gui.invalidate() end)
    end
    -- Over everything (M.body puts it at the window's top), with a press
    -- anywhere else closing it.
    local menu = widgets.surface({
      id = "chat-menu", transition = style.appear(),
      position = "absolute", right = 12, top = require("bone.nodes.titlebar").offset() + M.header_height - 4,
      z = 30, width = 200, padding = 4,
      shadow = style.shadow.menu,
      item("edit", "Rename", function()
        bone.prompt.set("/rename " .. title)
        bone.gui.focus("composer-input")
      end),
      item("compress", "Compact", function() bone.cmd("compact") end),
      item("archive", "Archive", function() bone.cmd("archive") end, true),
    })
    M.menu_nodes = {
      { position = "absolute", left = 0, top = 0, right = 0, bottom = 0, z = 29,
        on_click = function() M.chat_menu = false bone.gui.invalidate() end },
      menu,
    }
  end
  return style.finish(bar)
end

--- One chat's column: the chat, with the composer, tray and status bar
--- under it, and its header (`header = false`: none). opts: `width` (pixels), `chat` (a chat number; nil or the chat
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
  -- Past M.max_width the column is centered, never with less than that
  -- gutter (a width just over it would otherwise get almost none).
  local gutter = opts.gutter
    or math.max(math.min(24, width / 10), (width - M.max_width) / 2)
  local content_width = math.max(0, width - 2 * gutter)
  -- `nodes.chat` keeps a right inset for its scrollbar. Reserve that same
  -- inset below it so the composer is centered on the usable chat width,
  -- rather than extending underneath the scrollbar gutter.
  local scrollbar_inset = content_width >= 240 and M.chat_scrollbar_inset or 0
  local usable_width = content_width - scrollbar_inset
  local chat = { grow = 1, direction = "column", padding = { 0, gutter, 0, gutter } }
  local new = is_new(session)
  local top = here and slot("chat.top", ctx) or nil
  if not new then nodes.append(chat, top) end
  -- A chat being read in: say so, rather than show a blank page.
  if not new and session and session.loading and (session.items or 0) == 0 then
    chat[#chat + 1] = widgets.note("Loading this chat…", { 4, 2 })
  end
  chat[#chat + 1] = nodes.chat({
    chat = not here and n or nil,
    width_px = usable_width, cell = ctx.cell, padding = { 14, 14, M.chat_fade, 0 },
  })
  if here then
    chat[#chat + 1] = chat_fade(gutter)
    chat[#chat + 1] = chat_fade(gutter, true)
    chat[#chat + 1] = jump(gutter)
  end
  local below = {
    direction = "column", padding = { 0, gutter + scrollbar_inset, 18, gutter }, gap = 4,
  }
  -- Panels docked at the bottom stand over the composer, as in the terminal
  -- they stand over the prompt.
  if here then
    local docks = M.docked("bottom", { width = usable_width, height = ctx.height, cell = ctx.cell },
      ctx.height and ctx.height / 2, true)
    for _, d in ipairs(docks) do d.margin = { 0, 0, 4, 0 } end
    nodes.append(below, docks)
  end
  local input = composer.node({ session = session, width = usable_width,
    chat = not here and n or nil, on_focus = opts.on_focus })
  if new then
    -- The chat's list stays (empty, out of the way); the composer moves up
    -- to the middle with the welcome.
    chat.grow, chat.height, chat.overflow = 0, 0, "hidden"
  else
    below[#below + 1] = input
  end
  below[#below + 1] = here and tray.node() or nil
  below[#below + 1] = statusbar.node(session, ctx.cell)
  local middle = nil
  if new then
    middle = { grow = 1, direction = "column", padding = { 0, gutter + scrollbar_inset, 0, gutter } }
    nodes.append(middle, top)
    middle[#middle + 1] = welcome(usable_width, input)
  end
  -- A column a window manager places (it passes `on_focus`) has the
  -- manager's title bar instead, unless it asks for `header = true`.
  local header = opts.header
  if header == nil then header = opts.on_focus == nil end
  return { grow = 1, direction = "column", header and M.header(session, width) or nil, chat, middle, below }
end

--- The chats beside the sidebar: the chat on screen's column. A plugin
--- replaces it to show several (ctx = { width, height, cell } of the area).
function M.area(ctx)
  return M.column(ctx, { width = ctx.width })
end

--- The window: the title bar it draws itself (bone.nodes.titlebar; none
--- where the system draws one) over the rest (M.body), which starts
--- under it. What floats (dialogs, popups) is still placed on the whole
--- window, and a dialog's backdrop dims the bar too.
function M.view(ctx)
  local alpha = see_through()
  local bar = require("bone.nodes.titlebar").node(alpha and (bone.hl.get("Normal") or {}).bg or "Normal")
  if not bar then return M.body(ctx) end
  local area = {}
  for k, v in pairs(ctx) do area[k] = v end
  area.height = math.max(0, (ctx.height or 0) - bar.height)
  local root = M.body(area)
  root.padding = { bar.height, 0, 0, 0 }
  bar.position, bar.top, bar.left, bar.right, bar.z = "absolute", 0, 0, 0, M.titlebar_z
  root[#root + 1] = bar
  return root
end

-- Over the sidebar floating on a narrow window (and its backdrop), under
-- dialogs and popups.
M.titlebar_z = 9

--- Everything under the title bar: the docks, the sidebar, the chat, and
--- what floats over them. ctx = { width, height, cell } of that area.
function M.body(ctx)
  local sidebar = require("bone.desktop.sidebar")
  local alpha = see_through()
  local chat_bg = alpha and (bone.hl.get("Normal") or {}).bg or "Normal"
  -- The chat fills the window. The sidebar is a card on it, in a lane at
  -- its left that the chat makes room for; a narrow window has no room for
  -- one, and it floats over.
  local main = style.finish({
    grow = 1, width = 0, direction = "row", bg = chat_bg, overflow = "hidden",
  })
  -- The sidebar floats over the chat where what the docks leave has no
  -- room for both.
  local width, height = ctx.width, ctx.height
  local left, used = M.docked("left", ctx, width)
  width = width - used
  local right
  right, used = M.docked("right", ctx, width)
  width = width - used
  local side, room, backdrop = sidebar.node({ width = width, height = height, cell = ctx.cell })
  room = (side and not backdrop) and room or 0
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
  nodes.append(root, M.menu_nodes)
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
  -- Esc closes the chat's action menu before it dismisses anything else.
  local dismissed = bone.ui.actions.dismiss
  bone.ui.actions.dismiss = function()
    if M.chat_menu then
      M.chat_menu = false
      bone.gui.invalidate()
      return true
    end
    return dismissed and dismissed() or false
  end
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
