-- What floats over the window: the message toast, the questions the core
-- asks (a modal card with its options, or a line to type) and an image
-- shown large. `M.nodes(ctx)` is their nodes, `M.setup()` listens for what
-- opens them.
local M = {}

local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")

-- ---- the core's questions -----------------------------------------------------

local asks = {}   -- ask_id -> { question, sel, text }
local order = {}  -- ask ids, oldest first: the one on screen is the first
local reported   -- the bone.ui.ask error already notified

local function add_ask(id, question)
  if asks[id] then return end
  asks[id] = { question = question, sel = 1, text = "" }
  order[#order + 1] = id
end

local function drop_ask(id)
  if not asks[id] then return end
  asks[id] = nil
  for i, v in ipairs(order) do
    if v == id then table.remove(order, i) break end
  end
end

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
  drop_ask(ask_id)
  bone.request("ask/respond", { ask_id = ask_id, answer = answer })
  bone.gui.invalidate()
end

-- Its keys, while the card has the focus (it takes it as it comes, and a
-- modal keeps it): arrows, tab and 1-9 pick, enter answers, esc denies.
local function ask_key(k)
  local cur = order[1]
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

-- Plain: the built-in card and the large image lose their radius and shadow.
-- Fills stay (the scrim, a floating panel). style.finish owns that once it exists.
local function finish(node)
  if type(node) ~= "table" then return node end
  if type(style.finish) == "function" then
    style.finish(node)
  elseif style.plain then
    node.radius, node.shadow = nil, nil
  end
  if style.plain then
    for i, child in ipairs(node) do node[i] = finish(child) end
  end
  return node
end

local function ask_dialog()
  local id = order[1]
  local a = id and asks[id]
  if not a then return nil end
  local options = question_options(a.question)
  -- A library may draw the question itself. Nil, or anything but a node,
  -- keeps the card below. One notice per distinct failure, then the card.
  if type(bone.ui.ask) == "function" then
    local ok, node = pcall(bone.ui.ask, a.question, {
      id = id,
      options = options,
      text = a.text,
      selected = a.sel,
      respond = function(answer) respond(id, answer) end,
    })
    if ok and type(node) == "table" then return node end
    if not ok then
      local err = tostring(node)
      if reported ~= err then bone.notify(err, "error") end
      reported = err
    end
  end
  local card = widgets.surface({
    id = "ask:" .. tostring(id), autofocus = true, key_guard = 300, on_key = ask_key,
    focus = {}, -- the selected option shows where the keys go, not a ring
    role = "dialog", label = question_text(a.question),
    width = 460, max_width = "90%", radius = 14,
    padding = { 18, 20, 18, 20 }, gap = 14,
    { text = question_text(a.question), size = style.size.text, selectable = true },
  })
  if #options > 0 then
    local row = { direction = "row", gap = 8, justify = "end" }
    for i, o in ipairs(options) do
      local on = i == a.sel
      row[#row + 1] = widgets.button(o, function() respond(id, o) end, false, false, {
        bold = on, padding = { 7, 16 }, bg = on and "Selection" or "InputBackground",
        focusable = false, role = "option", selected = on,
      })
    end
    card[#card + 1] = row
  else
    card[#card + 1] = { bg = "InputBackground", radius = style.radius.lg, padding = { 8, 12 },
      { text = a.text .. "▏", size = style.size.text } }
    card[#card + 1] = { direction = "row", justify = "end",
      widgets.button("Answer", function() respond(id, a.text) end, false, false,
        { padding = { 7, 16 }, focusable = false }) }
  end
  return {
    position = "absolute", left = 0, right = 0, top = 0, bottom = 0, z = 10, block_input = true,
    bg = style.scrim.dialog, direction = "column", justify = "center", align = "center", finish(card),
  }
end

-- ---- an image, large (bone.gui.preview) -----------------------------------------

local preview -- the image open: an item's images[i]

local function close_preview()
  preview = nil
  bone.gui.invalidate()
end

local function preview_overlay(ctx)
  if not preview then return nil end
  local w, h = preview.width or 640, preview.height or 480
  local k = math.min(1, ctx.width * 0.9 / w, (ctx.height - 100) * 0.9 / h)
  local name = preview.name or "image"
  return {
    position = "absolute", left = 0, right = 0, top = 0, bottom = 0, z = 15, block_input = true,
    bg = style.scrim.image, direction = "column", justify = "center", align = "center", gap = 10,
    cursor = "pointer", on_click = close_preview, autofocus = true, focus = {},
    role = "dialog", label = name .. ", click or esc to close",
    on_key = function(key)
      if key == "esc" or key == "q" or key == "enter" or key == "space" then close_preview() end
      return true
    end,
    finish({ width = math.floor(w * k), height = math.floor(h * k), radius = style.radius.lg,
      shadow = style.shadow.big, role = "image", label = name,
      image = { id = preview.id, path = preview.path, width = w, height = h, fit = "contain" } }),
    { text = name .. " · " .. w .. "×" .. h, hl = "Dim", size = style.size.caption },
    { text = "click or esc to close", hl = "Dim", size = style.size.small },
  }
end

-- ---- the message toast ------------------------------------------------------------

local function toast()
  local s = bone.gui.status()
  if not s or s.text == "" then return nil end
  -- A message stays a few seconds.
  local life = s.error and 8000 or 4000
  if s.age_ms >= life then return nil end
  bone.gui.invalidate(life - s.age_ms + 50)
  return {
    position = "absolute", left = 0, right = 0, bottom = 96, z = 4, direction = "row", justify = "center",
    widgets.press(widgets.surface({
      text = s.text, size = style.size.caption, hl = s.error and "ErrorMsg" or "StatusLine",
      padding = { 8, 14, 8, 14 }, max_width = "80%", selectable = false,
    }), bone.gui.status_clear, "button", false),
  }
end

--- The nodes over the window, bottom first.
function M.nodes(ctx)
  local out = {}
  for _, make in ipairs({ toast, ask_dialog, preview_overlay }) do
    out[#out + 1] = make(ctx)
  end
  return out
end

function M.setup()
  --- Open an image attachment (an item's images[i]) large over the window.
  function bone.gui.preview(image)
    preview = image
    bone.gui.invalidate()
  end
  bone.on("ask/requested", function(ev)
    add_ask(ev.ask_id, ev.question)
    bone.gui.invalidate()
  end)
  bone.on("ask/resolved", function(ev)
    drop_ask(ev.ask_id)
    bone.gui.invalidate()
  end)
  bone.request("ask/list", {}, function(list)
    for _, ev in ipairs(list or {}) do
      if type(ev) == "table" and ev.ask_id then add_ask(ev.ask_id, ev.question) end
    end
    bone.gui.invalidate()
  end)
end

return M
