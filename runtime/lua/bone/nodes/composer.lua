-- The composer under the chat: the card with its attach button, the
-- prompt's text field and the stop button while a turn runs; the images
-- attached to the message above it, and the / menu over it. Slots:
-- `composer.above` and `composer.below`.
local M = {}

local nodes = require("bone.nodes")
local menu = require("bone.menu")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")

local SUGGESTIONS = 8 -- rows of the / menu

local function slot(name, ctx) return bone.gui.slots.nodes(name, ctx) end

local function attachments()
  local rows = { direction = "column", gap = 2, padding = { 0, 14, 4, 14 } }
  for i, image in ipairs(bone.prompt.images()) do
    rows[#rows + 1] = {
      direction = "row", gap = 10, align = "center",
      { text = ("Image %d: %s · %d×%d"):format(i, image.name, image.width, image.height),
        hl = "Dim", size = style.size.small },
      widgets.link("remove", function() bone.prompt.remove_image(i) end, { size = style.size.small }),
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
  local rows = SUGGESTIONS
  local first = math.max(1, math.min(menu.selected - rows + 1, n - rows + 1))
  if menu.selected < first then first = menu.selected end
  local list = widgets.surface({
    position = "absolute", left = 0, right = 0, bottom = "100%", z = 5,
    margin = style.margin(0, 8), padding = style.pad(6, 6),
  })
  for i = first, math.min(n, first + rows - 1) do
    local it, on = items[i], i == menu.selected
    list[#list + 1] = style.finish(widgets.press({
      direction = "row", gap = 12, padding = { 5, 10, 5, 10 }, radius = style.radius.md,
      bg = on and "Selection" or nil, selected = on,
      { text = "/" .. it.name, size = style.size.body, bold = on, width = 140, wrap = false, overflow = "hidden" },
      { text = it.desc or "", hl = "Dim", size = style.size.caption, grow = 1, wrap = false, overflow = "hidden" },
    }, function() menu.selected = i; menu.complete() end, "option", not on))
  end
  return list
end

local function stop(session)
  if session.session_id then bone.request("turn/cancel", { session_id = session.session_id }) end
end

-- `bone.ui.prompt`'s field `name`, shared with the TUI: a line, or a
-- function of `ctx` returning one; as node spans.
local function prompt_line(name, ctx)
  local v = type(bone.ui.prompt) == "table" and bone.ui.prompt[name] or nil
  if type(v) == "function" then
    local ok, r = pcall(v, ctx)
    v = ok and r or nil
  end
  if type(v) == "string" then return { v } end
  if type(v) ~= "table" then return nil end
  local spans = {}
  for _, s in ipairs(v) do
    if type(s) == "string" or (type(s) == "table" and s.fill == nil) then spans[#spans + 1] = s end
  end
  return spans
end

local function spans_text(spans)
  local out = {}
  for _, s in ipairs(spans) do out[#out + 1] = type(s) == "string" and s or (s.text or s[1] or "") end
  return table.concat(out)
end

-- How many rows the field grows to: `bone.ui.prompt.max`, else
-- `bone.o.prompt_max_height` when it is set, else the composer's own 8.
local function max_rows()
  local p = type(bone.ui.prompt) == "table" and bone.ui.prompt or {}
  if tonumber(p.max) then return tonumber(p.max) end
  local ok, info = pcall(bone.o.info, "prompt_max_height")
  if ok and info and info.value ~= info.default then return info.value end
  return 8
end

--- The composer for `opts.session`, with what goes above and under it.
--- `bone.ui.prompt`'s `placeholder`, `prefix`, `min` and `max` apply, as in
--- the TUI; its box (border, background, padding, edges) is this card.
function M.node(opts)
  local running = opts.session and opts.session.running
  local text = bone.prompt.get()
  local ctx = { text = text, empty = text == "", focused = not opts.chat, running = running or false,
    session = opts.session }
  local placeholder = prompt_line("placeholder", ctx)
  placeholder = placeholder and spans_text(placeholder)
    or (running and "Message bone (queues)…" or "Message bone…")
  local prefix = not opts.chat and prompt_line("prefix", ctx) or nil
  local p = type(bone.ui.prompt) == "table" and bone.ui.prompt or {}
  local rows = max_rows()
  local card = {
    id = opts.chat and ("prompt:" .. opts.chat) or "prompt", bg = "InputBackground", border = 1, border_color = "PromptBorder",
    radius = style.radius.round, padding = { 10, 14, 10, 14 }, direction = "row", gap = 10, align = "end",
    on_press = opts.chat and opts.on_focus or nil,
    widgets.icon_button("+", "Attach files", function() if opts.on_focus then opts.on_focus() end; require("bone.desktop.attach").pick() end,
      { box = 26, size = 20, hl = "Accent", radius = style.radius.md }),
    { input = not opts.chat, placeholder = placeholder,
      text_input = opts.chat and { value = bone.gui.draft(opts.chat), placeholder = placeholder, multiline = true, max_rows = rows, read_only = true } or nil,
      size = style.size.text, min_rows = tonumber(p.min), max_rows = rows, hl = "InputText", grow = 1,
      id = "composer-input" .. (opts.chat or "") },
  }
  if prefix and #prefix > 0 then
    table.insert(card, 2, { spans = prefix, size = style.size.text, hl = "UserPrompt", wrap = false })
  end
  if running then
    card[#card + 1] = widgets.icon_button("■", "Stop (ctrl+c)", function() stop(opts.session) end,
      { box = 26, size = style.size.small, hl = "Accent", radius = style.radius.md,
        bg = "Selection", hover = { opacity = 0.8 } })
  end
  -- id is prompt or prompt:N, so the outline stays when plain.
  style.finish(card)
  local col = { direction = "column" }
  nodes.append(col, slot("composer.above", opts))
  col[#col + 1] = not opts.chat and attachments() or nil
  col[#col + 1] = card
  col[#col + 1] = not opts.chat and suggestions() or nil
  nodes.append(col, slot("composer.below", opts))
  return col
end

return M
