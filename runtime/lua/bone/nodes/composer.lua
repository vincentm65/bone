-- The composer under the chat: a raised card with the prompt's text field
-- and a toolbar under it (attach, the model, what a running turn is doing,
-- how full the context is, and send or stop); the images attached to the
-- message above it, and the / menu over it. Slots: `composer.above`,
-- `composer.below`, and `composer.tools` (in the toolbar, before the
-- context).
local M = {}

local nodes = require("bone.nodes")
local menu = require("bone.menu")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local icons = require("bone.nodes.icons")
local statusbar = require("bone.nodes.statusbar")

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
  -- `argument`: these complete a command's argument (a file, a name), not
  -- the command's own name.
  local items, argument = menu.all(text)
  if #items == 0 then return nil end
  if menu.last_text ~= text then
    menu.last_text, menu.offset, menu.selected, menu.last_selected = text, 0, 1, 1
  end
  local n = #items
  menu.selected = math.max(1, math.min(menu.selected, n))
  local rows = SUGGESTIONS
  local first = math.max(1, math.min(menu.selected - rows + 1, n - rows + 1))
  if menu.selected < first then first = menu.selected end
  local list = widgets.surface({ id = "slash-menu", transition = style.appear(), padding = { 6, 6, 0, 6 },
    shadow = style.shadow.menu })
  for i = first, math.min(n, first + rows - 1) do
    local it, on = items[i], i == menu.selected
    list[#list + 1] = style.finish(widgets.press({
      direction = "row", gap = 10, align = "center", height = 34, padding = { 0, 10 }, radius = style.radius.md,
      bg = on and "Selection" or nil, selected = on,
      -- A command's name is "/name" in a column; an argument is itself, as
      -- wide as it is (a path is longer than a name).
      { text = argument and it.name or "/" .. it.name, size = style.size.body, width = not argument and 130 or nil,
        wrap = false, text_overflow = "ellipsis", shrink = argument and 1 or 0 },
      { text = it.desc or "", hl = on and "Muted" or "Dim", size = style.size.body, grow = 1, width = 0,
        wrap = false, text_overflow = "ellipsis" },
    }, function() menu.selected = i; menu.complete() end, "option", not on))
  end
  -- The keys, under a hairline.
  local keys = widgets.hints("↑↓ move · enter run · tab complete · esc close")
  keys.height, keys.padding, keys.margin = 36, { 0, 14 }, { 6, -6, 0, -6 }
  keys.border, keys.border_color = { 1, 0, 0, 0 }, "WinSeparator"
  list[#list + 1] = style.finish(keys)
  -- Over the composer with a gap, not touching it (an absolute node's
  -- margin is not laid out, so the gap is a transparent pad under it).
  return { position = "absolute", left = 0, right = 0, bottom = "100%", z = 5, padding = { 0, 0, 6, 0 }, list }
end

local function stop(session)
  if session.session_id then bone.request("turn/cancel", { session_id = session.session_id }) end
end

local usage = require("bone.usage")

-- How full the context is, as a ring: a faint track and the used part.
local function ring(fraction)
  local draw = bone.gui.draw
  local f = math.max(0, math.min(1, fraction))
  return {
    width = 14, height = 14, shrink = 0,
    canvas = { draw = function()
      local out = { draw.circle({ x = 7, y = 7, r = 5.5, stroke = "PromptBorder", width = 2 }) }
      if f > 0 then
        local pts, steps = {}, math.max(2, math.ceil(f * 40))
        for i = 0, steps do
          local a = -math.pi / 2 + (i / steps) * f * 2 * math.pi
          pts[#pts + 1] = { 7 + 5.5 * math.cos(a), 7 + 5.5 * math.sin(a) }
        end
        out[#out + 1] = draw.polyline({ points = pts, color = f > 0.85 and "WarningMsg" or "Muted", width = 2 })
      end
      return out
    end },
  }
end

-- The toolbar under the field.
local function toolbar(opts, running, empty)
  local session = opts.session
  local id = session and session.session_id
  local u = usage.of(id)
  local muted = (bone.hl.get("Muted") or {}).fg and "Muted" or "Dim"
  -- Narrow, the model's name is cut short and the context is its ring alone;
  -- they go only where there is no room at all.
  local narrow, compact = opts.width and opts.width < 420, opts.width and opts.width < 240
  local bar = { direction = "row", align = "center", gap = 6, height = 30 }
  bar[#bar + 1] = widgets.icon_button("plus", "Attach files", function()
    if opts.on_focus then opts.on_focus() end
    require("bone.desktop.attach").pick()
  end, { box = 28, size = 16, hl = muted, radius = style.radius.md, id = opts.chat and nil or "composer-attach" })
  -- The model: an outlined pill that opens the provider page.
  if u.model and u.model ~= "" and not compact then
    local name = (u.provider and u.provider ~= u.model) and (u.provider .. " · " .. u.model) or u.model
    bar[#bar + 1] = widgets.press(style.finish({
      direction = "row", align = "center", gap = 6, height = 28, padding = { 0, 8, 0, 9 }, radius = 14, shrink = 1,
      min_width = 56, border = 1, border_color = "PromptBorder", label = "Model: " .. name, tooltip = "Change the model (/provider)",
      icons.node("model", { size = 13, hl = "Dim" }),
      { text = name, size = style.size.small, hl = muted, wrap = false, text_overflow = "ellipsis", shrink = 1 },
      icons.node("chevron_down", { size = 12, hl = "Dim" }),
    }), function() require("bone.config").open("providers") end)
  end
  local elapsed = statusbar.elapsed(session)
  if running then
    bar[#bar + 1] = {
      direction = "row", gap = 6, align = "center", margin = { 0, 0, 0, 4 }, shrink = 1,
      widgets.spinner(18),
      { text = "Working" .. (elapsed and (" · " .. bone.util.duration(elapsed)) or ""),
        hl = muted, size = style.size.small, wrap = false, text_overflow = "ellipsis", shrink = 1 },
    }
  end
  bar[#bar + 1] = { grow = 1 }
  nodes.append(bar, slot("composer.tools", opts))
  -- The context: how full, by the compaction limit when there is one.
  if (u.curr or 0) > 0 and not compact then
    -- "100k", not "100.0k".
    local function k(n) return (bone.util.tokens(n):gsub("%.0([km])$", "%1")) end
    local label = k(u.curr) .. (u.limit and u.limit > 0 and (" / " .. k(u.limit)) or "")
    local used = (u.input or 0) + (u.output or 0)
    local tip = bone.util.tokens(u.curr) .. " in context"
      .. (used > 0 and (" · " .. bone.util.tokens(used) .. " used") or "")
      .. ((u.input or 0) > 0 and string.format(" · %.0f%% cached", (u.cached or 0) / u.input * 100) or "")
    bar[#bar + 1] = {
      direction = "row", align = "center", gap = 6, margin = { 0, 6, 0, 0 }, tooltip = tip,
      (u.limit and u.limit > 0) and ring(u.curr / u.limit) or nil,
      not (narrow and u.limit and u.limit > 0) and
        { text = label, size = style.size.micro, font = "mono", hl = "Dim", wrap = false } or nil,
    }
  end
  -- Send, or stop while a turn runs: a round button in the text's color.
  local fg = (bone.hl.get("Normal") or {}).fg and "Normal" or "InputText"
  local fill = style.alpha(fg, "ff", "fg", "#ededed")
  local ink = style.alpha("Normal", "ff", "bg", "#000000")
  if running then
    bar[#bar + 1] = widgets.press(style.finish({
      width = 30, height = 30, radius = 15, bg = fill, justify = "center", align = "center",
      label = "Stop (ctrl+c)", tooltip = "Stop (ctrl+c)", hover = { opacity = 0.85 },
      icons.node("stop", { size = 14, hl = ink, width = 2 }),
    }), function() stop(session) end, "button", false)
  else
    bar[#bar + 1] = widgets.press(style.finish({
      width = 30, height = 30, radius = 15, bg = fill, justify = "center", align = "center",
      opacity = empty and 0.3 or 1, label = "Send (enter)", tooltip = "Send (enter)",
      hover = not empty and { opacity = 0.85 } or nil,
      icons.node("arrow_up", { size = 15, hl = ink, width = 2 }),
    }), function()
      if opts.chat then if opts.on_focus then opts.on_focus() end return end
      if not empty then bone.action("submit") end
    end, "button", false)
  end
  return bar
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
  -- Nothing to show is no line of its own: the composer's default stays
  -- (a wrapper such as Prompt Suggestions returns {} when it has nothing).
  return #spans > 0 and spans or nil
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
    or (running and "Message bone (queues)…" or "Ask bone, or type / for commands")
  local prefix = not opts.chat and prompt_line("prefix", ctx) or nil
  local p = type(bone.ui.prompt) == "table" and bone.ui.prompt or {}
  local rows = max_rows()
  -- Stack the attach control above the field when there is not enough room
  -- for both it and a useful input. The field then gets the full width.
  local compact = opts.width and opts.width < 240
  local field_placeholder = placeholder
  if compact then
    field_placeholder = opts.width < 96 and "…" or (running and "Queue…" or "Ask bone…")
  end
  -- The field, with the prompt's prefix before it.
  local field = { direction = "row", gap = 8, align = "start", padding = { 0, 4, 0, 4 },
    { input = not opts.chat, placeholder = field_placeholder,
      text_input = opts.chat and { value = bone.gui.draft(opts.chat), placeholder = field_placeholder, multiline = true, max_rows = rows, read_only = true } or nil,
      size = style.size.text, min_rows = tonumber(p.min), max_rows = rows, hl = "InputText", grow = 1, width = 0,
      id = "composer-input" .. (opts.chat or "") },
  }
  if prefix and #prefix > 0 then
    table.insert(field, 1, { spans = prefix, size = style.size.text, hl = "UserPrompt", wrap = false })
  end
  local card = {
    id = opts.chat and ("prompt:" .. opts.chat) or "prompt", bg = "InputBackground", border = 1, border_color = "PromptBorder",
    radius = style.radius.round, padding = compact and { 8, 8, 8, 8 } or { 12, 10, 10, 12 },
    direction = "column", gap = compact and 6 or 8, shadow = style.shadow.card,
    on_press = opts.chat and opts.on_focus or nil,
    field,
    toolbar(opts, running, text == ""),
  }
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
