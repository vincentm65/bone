-- The node library's window: the chat, the composer card with its
-- attachments, suggestions and status bar, the tray, docked panels (the
-- sessions sidebar), a welcome, the message toast and the questions the
-- core asks. Everything is a node; replace this file, or part of it, to
-- draw the window differently.
local M = {}

local nodes = require("bone.nodes")
local usage = require("bone.usage")
local agents = require("bone.agents")
local menu = require("bone.menu")
local tray = require("bone.desktop.tray")
local tokens = bone.util.tokens

local function slot(name, ctx) return bone.gui.slots.nodes(name, ctx) end

M.max_width = 1040 -- the chat and composer column, in pixels

-- ---- docked panels: those drawn as nodes (bone.ui.panel.open{ node = fn }) --

local function panel_node(info, draw, ctx)
  -- A panel's size is in character cells, as in the TUI.
  local cell = ctx.cell
  local width = math.max(8, tonumber(info.size) or 40) * (cell and cell.w or 8) + 24
  local ok, content = pcall(draw, { width = width - 12, height = ctx.height or 0 })
  if not ok then content = { text = tostring(content), hl = "ErrorMsg", selectable = true } end
  return {
    width = width, bg = "Panel", direction = "column", padding = { 8, 6, 8, 6 },
    -- A press gives it the keyboard, as in the TUI.
    on_press = info.focusable and function() bone.ui.panel.focus(info.id) end or nil,
    content and { grow = 1, direction = "column", content } or nil,
  }
end

--- The nodes of the docked panels on `side` ("left" or "right").
function M.docked(side, ctx)
  local out = {}
  ctx = ctx or {}
  for _, info in ipairs(bone.ui.panel.list()) do
    local draw = info.dock == side and not info.hidden and bone.gui.panel_node(info.id)
    if draw then out[#out + 1] = panel_node(info, draw, ctx) end
  end
  return out
end

-- ---- the composer ---------------------------------------------------------

local function attachments()
  local rows = { direction = "column", gap = 2, padding = { 0, 14, 4, 14 } }
  for i, image in ipairs(bone.prompt.images()) do
    rows[#rows + 1] = {
      direction = "row", gap = 10, align = "center",
      { text = ("Image %d: %s · %d×%d"):format(i, image.name, image.width, image.height), hl = "Dim", size = 12 },
      { text = "remove", hl = "Accent", size = 12, cursor = "pointer",
        on_click = function() bone.prompt.remove_image(i) end },
    }
  end
  return #rows > 0 and rows or nil
end

-- The / menu, above the composer: the closest match nearest the text.
local function suggestions()
  local text = bone.prompt.get()
  local items = menu.all(text)
  if #items == 0 then return nil end
  if menu.last_text ~= text then
    menu.last_text, menu.offset, menu.selected, menu.last_selected = text, 0, 1, 1
  end
  local n = #items
  menu.selected = math.max(1, math.min(menu.selected, n))
  local rows = 8
  local first = math.max(1, math.min(menu.selected - rows + 1, n - rows + 1))
  if menu.selected < first then first = menu.selected end
  local list = {
    position = "absolute", left = 0, right = 0, bottom = "100%", z = 5,
    margin = { 0, 0, 8, 0 }, bg = "Panel", radius = 12, padding = { 6, 6, 6, 6 },
    direction = "column", border = 1, border_color = "PromptBorder",
  }
  for i = first, math.min(n, first + rows - 1) do
    local it, on = items[i], i == menu.selected
    list[#list + 1] = {
      direction = "row", gap = 12, padding = { 5, 10, 5, 10 }, radius = 8, cursor = "pointer",
      bg = on and "Selection" or nil, transition = 100,
      on_click = function() menu.selected = i; menu.complete() end,
      { text = "/" .. it.name, size = 14, bold = on, width = 140, wrap = false, overflow = "hidden" },
      { text = it.desc or "", hl = "Dim", size = 13, grow = 1, wrap = false, overflow = "hidden" },
    }
  end
  return list
end

-- What the turn on screen is doing: when it started (by session), and the
-- TUI's spinner while it runs.
local started = {}
M.spinner = { frames = { "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏" }, interval = 100 }

local function spinner(cell)
  local s = M.spinner
  -- The "Working · Ns" text is whole seconds; the glyph animates on its
  -- own canvas, so the window is not rebuilt ten times a second.
  bone.gui.invalidate(1000)
  local w = math.max(14, cell and cell.w or 14)
  local h = math.max(16, cell and cell.h or 16)
  return {
    width = w, height = h,
    canvas = {
      fps = 1000 / s.interval,
      draw = function()
        local frame = s.frames[math.floor(bone.now() / s.interval) % #s.frames + 1]
        return { bone.gui.draw.text({
          x = 0, y = 0, text = frame, size = 13, color = "ToolRunning",
          font = "mono", baseline = "top",
        }) }
      end,
    },
  }
end

local function stop(session)
  if session.session_id then bone.request("turn/cancel", { session_id = session.session_id }) end
end

local function composer(opts)
  local running = opts.session and opts.session.running
  local card = {
    id = "prompt", bg = "InputBackground", border = 1, border_color = "PromptBorder", radius = 16,
    padding = { 10, 14, 10, 14 }, direction = "row", gap = 10, align = "end",
    { text = "+", hl = "Accent", size = 20, cursor = "pointer", padding = { 0, 2, 0, 2 },
      on_click = function() require("bone.desktop.attach").pick() end },
    { input = true, placeholder = running and "Message bone (queues)…" or "Message bone…",
      size = 15, max_rows = 8, hl = "InputText", grow = 1, id = "composer-input" },
  }
  if running then
    card[#card + 1] = {
      text = "■", hl = "Accent", size = 12, cursor = "pointer", padding = { 4, 7, 4, 7 }, radius = 8,
      bg = "Selection", tooltip = "Stop (ctrl+c)", role = "button", label = "Stop",
      on_click = function() stop(opts.session) end,
    }
  end
  local col = { direction = "column" }
  nodes.append(col, slot("composer.above", opts))
  col[#col + 1] = attachments()
  col[#col + 1] = card
  col[#col + 1] = suggestions()
  nodes.append(col, slot("composer.below", opts))
  return col
end

-- ---- the status bar -------------------------------------------------------

local function statusbar(session, cell)
  local id = session and session.session_id
  local u = usage.of(id)
  local right = {}
  if (u.curr or 0) > 0 then right[#right + 1] = tokens(u.curr) .. " context" end
  local total = (u.input or 0) + (u.output or 0)
  if total > 0 then
    right[#right + 1] = tokens(total) .. " used"
    if (u.input or 0) > 0 then
      right[#right + 1] = string.format("%.0f%% cached", (u.cached or 0) / u.input * 100)
    end
  end
  local spent = id and agents.spend(agents.top(id)) or 0
  if spent > 0 then right[#right + 1] = tokens(spent) .. " agents" end
  local bar = { direction = "row", padding = { 6, 16, 0, 16 }, gap = 12, align = "center" }
  if session and session.running then
    local since = id and (started[id] or os.time())
    if id then started[id] = since end
    bar[#bar + 1] = {
      direction = "row", gap = 6, align = "center",
      spinner(cell),
      { text = "Working" .. (since and (" · " .. bone.util.duration(os.time() - since)) or ""),
        hl = "StatusLine", size = 12, wrap = false },
    }
  elseif id then
    started[id] = nil
  end
  bar[#bar + 1] = { text = u.model or "", hl = "StatusLine", size = 12, wrap = false }
  nodes.append(bar, slot("statusbar.left"))
  bar[#bar + 1] = { grow = 1 }
  nodes.append(bar, slot("statusbar.right"))
  bar[#bar + 1] = { text = table.concat(right, "  ·  "), hl = "StatusLineDim", size = 12, wrap = false }
  return bar
end

-- ---- the chat -------------------------------------------------------------

-- Ten field greys, darkest first: the new chat's canvas, the GUI's version
-- of the dot matrix in the TUI (a native `field` node; Lua sets its look).
M.field = {
  colors = {
    "#070707", "#090909", "#0b0b0b", "#0d0d0d", "#0f0f0f",
    "#111111", "#131313", "#151515", "#171717", "#1a1a1a",
  },
  pitch = 5, size = 2.2, scale = 1, speed = 1, fps = 20, round = "top",
}

local function welcome(out)
  local s = bone.chat.session()
  if bone.api.session() or (s and s.items > 0) then return nil end
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
    -- The field, behind a clear patch that keeps it off the text.
    { position = "absolute", left = 0, right = 0, top = 0, bottom = 0, z = -1, radius = 16 * (out or 0), field = M.field },
    { direction = "column", gap = 6, align = "center", bg = M.field.colors[1], radius = 10,
      padding = { 14, 28, 14, 28 },
      { text = title, color = "#eeeeee", size = 16, text_align = "center" },
      { text = hint, color = "#bbbbbb", size = 13, text_align = "center" } },
  }
end

local function jump()
  local i = bone.gui.scroll_info("chat")
  if not i or i.offset >= i.max - 4 then return nil end
  return {
    position = "absolute", bottom = 10, left = 0, right = 0, direction = "row", justify = "center",
    { text = "↓  Jump to latest", hl = "Accent", size = 13, cursor = "pointer", bg = "Panel",
      padding = { 6, 14, 6, 14 }, radius = 16, border = 1, border_color = "PromptBorder",
      on_click = function() bone.gui.scroll("chat", "end") end },
  }
end

-- ---- overlays: the message toast and the core's questions -----------------

local asks = {} -- ask_id -> { question, sel, text }

local function question_text(q)
  if type(q) ~= "table" then return tostring(q) end
  for _, k in ipairs({ "title", "text", "message", "prompt" }) do
    if type(q[k]) == "string" then return q[k] end
  end
  return bone.inspect and bone.inspect(q) or tostring(q)
end

local function question_options(q)
  local out = {}
  for _, o in ipairs(type(q) == "table" and (q.options or q.choices) or {}) do
    if type(o) == "string" then out[#out + 1] = o end
  end
  return out
end

local function respond(ask_id, answer)
  asks[ask_id] = nil
  bone.request("ask/respond", { ask_id = ask_id, answer = answer })
  bone.gui.invalidate()
end

-- Its keys, while the card has the focus (it takes it as it comes, and a
-- modal keeps it): arrows, tab and 1-9 pick, enter answers, esc denies.
local function ask_key(k)
  local cur = next(asks)
  local a = cur and asks[cur]
  if not a then return false end
  local options = question_options(a.question)
  if #options > 0 then
    if k == "left" or k == "shift+tab" then a.sel = (a.sel - 2) % #options + 1
    elseif k == "right" or k == "tab" then a.sel = a.sel % #options + 1
    elseif k == "enter" then respond(cur, options[a.sel])
    elseif k == "esc" then
      for _, o in ipairs(options) do
        if o == "deny" or o == "no" then respond(cur, o) return true end
      end
    elseif k:match("^[1-9]$") and options[tonumber(k)] then respond(cur, options[tonumber(k)])
    else return false end
  elseif k == "enter" then respond(cur, a.text)
  else
    local edited = bone.text.edit(a.text, k)
    if not edited then return false end
    a.text = edited
  end
  return true
end

local function ask_dialog()
  local id = next(asks)
  if not id then return nil end
  local a = asks[id]
  local options = question_options(a.question)
  local card = {
    id = "ask:" .. tostring(id), autofocus = true, key_guard = 300, on_key = ask_key,
    focus = {}, -- the selected option shows where the keys go, not a ring
    role = "dialog", label = question_text(a.question),
    width = 460, max_width = "90%", bg = "Panel", radius = 14, border = 1, border_color = "PromptBorder",
    padding = { 18, 20, 18, 20 }, direction = "column", gap = 14,
    { text = question_text(a.question), size = 15, selectable = true },
  }
  if #options > 0 then
    local row = { direction = "row", gap = 8, justify = "end" }
    for i, o in ipairs(options) do
      row[#row + 1] = {
        text = o, size = 14, bold = i == a.sel, cursor = "pointer", padding = { 7, 16, 7, 16 }, radius = 10,
        bg = i == a.sel and "Selection" or "InputBackground", focusable = false,
        role = "option", selected = i == a.sel,
        on_click = function() respond(id, o) end,
      }
    end
    card[#card + 1] = row
  else
    card[#card + 1] = { bg = "InputBackground", radius = 10, padding = { 8, 12, 8, 12 },
      { text = a.text .. "▏", size = 15 } }
    card[#card + 1] = { direction = "row", justify = "end",
      { text = "Answer", size = 14, cursor = "pointer", padding = { 7, 16, 7, 16 }, radius = 10,
        bg = "Selection", focusable = false, on_click = function() respond(id, a.text) end } }
  end
  return {
    position = "absolute", left = 0, right = 0, top = 0, bottom = 0, z = 10, block_input = true,
    bg = "#00000099", direction = "column", justify = "center", align = "center", card,
  }
end

-- ---- an image, large (bone.gui.preview) ------------------------------------

local preview -- the image open: an item's images[i]

local function close_preview()
  preview = nil
  bone.gui.invalidate()
end

local function open_preview(image)
  preview = image
  bone.gui.invalidate()
end

local function preview_overlay(ctx)
  if not preview then return nil end
  local w, h = preview.width or 640, preview.height or 480
  local k = math.min(1, ctx.width * 0.9 / w, (ctx.height - 100) * 0.9 / h)
  return {
    position = "absolute", left = 0, right = 0, top = 0, bottom = 0, z = 15, block_input = true,
    bg = "#000000d8", direction = "column", justify = "center", align = "center", gap = 10,
    cursor = "pointer", on_click = close_preview, autofocus = true, focus = {},
    role = "dialog", label = (preview.name or "image") .. ", click or esc to close",
    on_key = function(k)
      if k == "esc" or k == "q" or k == "enter" or k == "space" then close_preview() end
      return true
    end,
    { width = math.floor(w * k), height = math.floor(h * k), radius = 10,
      shadow = { blur = 30, dy = 10, color = "#000000c0" },
      image = { id = preview.id, path = preview.path, width = w, height = h, fit = "contain" } },
    { text = (preview.name or "image") .. " · " .. w .. "×" .. h, hl = "Dim", size = 13 },
    { text = "click or esc to close", hl = "Dim", size = 12 },
  }
end

local function toast()
  local s = bone.gui.status()
  if not s or s.text == "" then return nil end
  -- A message stays a few seconds.
  local life = s.error and 8000 or 4000
  if s.age_ms >= life then return nil end
  bone.gui.invalidate(life - s.age_ms + 50)
  return {
    position = "absolute", left = 0, right = 0, bottom = 96, z = 4, direction = "row", justify = "center",
    { text = s.text, size = 13, hl = s.error and "ErrorMsg" or "StatusLine", bg = "Panel", radius = 12,
      padding = { 8, 14, 8, 14 }, max_width = "80%", cursor = "pointer", selectable = false,
      border = 1, border_color = "PromptBorder", on_click = bone.gui.status_clear },
  }
end

-- ---- the window -----------------------------------------------------------

-- In a see-through window (`opacity` below 1) the canvas under the view is
-- drawn with that alpha, and backgrounds laid over it would stack it
-- opaque again: the window is one sheet of it, the chat flush against the
-- sidebar, whose Panel takes the same alpha.
local function see_through()
  local a = bone.gui.window_info().opacity or 1
  return a < 1 and a or nil
end

local function sheet(group, a)
  local bg = (bone.hl.get(group) or {}).bg
  if not (bg and bg:match("^#%x%x%x%x%x%x$")) then return group end
  return bg .. string.format("%02x", math.floor(a * 255 + 0.5))
end

function M.view(ctx)
  local session = bone.chat.session()
  local side, out = require("bone.desktop.sidebar").node()
  local alpha = see_through()
  if alpha then
    out = 0
    if side then side.bg = sheet("Panel", alpha) end
  end
  -- The chat is a rounded surface over the window, which shows grey where
  -- the sidebar slides out from under it; closed, the chat fills the window.
  local gutter = math.max(24, (ctx.width - (side and side.width or 0) - M.max_width) / 2)
  local chat = { grow = 1, direction = "column", padding = { 0, gutter, 0, gutter } }
  nodes.append(chat, slot("chat.top", ctx))
  chat[#chat + 1] = nodes.chat({
    width_px = ctx.width - 2 * gutter, cell = ctx.cell, padding = { 14, 0, 14, 0 },
  })
  chat[#chat + 1] = welcome(out)
  chat[#chat + 1] = jump()
  local main = {
    grow = 1, direction = "column", bg = not alpha and "Normal" or nil, overflow = "hidden",
    radius = 16 * out, margin = { 8 * out, 8 * out, 8 * out, 0 },
    shadow = out > 0 and { blur = 18, dx = -3, dy = 0, color = "#00000066" } or nil,
    chat,
    { direction = "column", padding = { 0, gutter, 12, gutter }, gap = 4,
      composer({ session = session }), tray.node(), statusbar(session, ctx.cell) },
  }
  local root = { direction = "row", bg = not alpha and "Panel" or nil, hl = "Normal" }
  nodes.append(root, M.docked("left", ctx))
  nodes.append(root, slot("window.left", ctx))
  root[#root + 1] = side
  root[#root + 1] = main
  nodes.append(root, slot("window.right", ctx))
  nodes.append(root, M.docked("right", ctx))
  root[#root + 1] = toast()
  root[#root + 1] = ask_dialog()
  root[#root + 1] = preview_overlay(ctx)
  nodes.append(root, slot("window.overlay", ctx))
  -- Windows of bone.ui.popup / win, over everything.
  nodes.append(root, require("bone.nodes.popups").nodes())
  return root
end

function M.setup()
  tray.setup()
  --- Open an image attachment (an item's images[i]) large over the window.
  function bone.gui.preview(image) open_preview(image) end
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
  bone.on("ask/requested", function(ev)
    local q = ev.question
    asks[ev.ask_id] = { question = q, sel = 1, text = "" }
    bone.gui.invalidate()
  end)
  bone.on("ask/resolved", function(ev)
    asks[ev.ask_id] = nil
    bone.gui.invalidate()
  end)
  bone.request("ask/list", {}, function(list)
    for _, ev in ipairs(list or {}) do
      if type(ev) == "table" and ev.ask_id then
        asks[ev.ask_id] = asks[ev.ask_id] or { question = ev.question, sel = 1, text = "" }
      end
    end
    bone.gui.invalidate()
  end)
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
