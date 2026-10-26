-- The desktop library's widgets: the small pieces its windows are made of,
-- each a function that returns a node. Whatever answers a press (a button,
-- a tab, a row, a link) also gets the pointer cursor, a name for screen
-- readers, a hover look and a fade, so none of them can be left out. The
-- sizes, corners and spaces come from bone.nodes.style. An `o` table, where
-- one is taken, replaces any field of the node it makes.
local M = {}

local style = require("bone.nodes.style")

local function extend(node, o)
  for k, v in pairs(o or {}) do
    if type(k) ~= "number" then node[k] = v end
  end
  for _, child in ipairs(o or {}) do node[#node + 1] = child end
  return node
end
M.extend = extend

--- Make `node` something a press answers: `click` runs, the pointer is a
--- hand, readers hear a `role` (default "button"), and it lightens under the
--- pointer (`hover = false` for no look of its own).
function M.press(node, click, role, hover)
  node.on_click = click
  node.cursor = node.cursor or "pointer"
  node.role = node.role or role or "button"
  node.transition = node.transition or style.motion.base
  if hover ~= false and node.hover == nil then node.hover = { bg = style.hover() } end
  return style.finish(node)
end

-- ---- text -----------------------------------------------------------------

--- A window's heading.
function M.title(text)
  return style.finish({
    text = text, hl = "Normal", bold = true, size = style.size.title,
    padding = { 4, 10 }, margin = style.margin(0, 6),
  })
end

--- A line of quiet text; `margin` is { above, below }.
function M.note(text, margin, hl)
  local m = margin or { 10, 0 }
  return style.finish({
    text = text, hl = hl or "Dim", size = style.size.small,
    padding = { 4, 10 }, margin = style.margin(m[1], m[2]),
  })
end

--- A clickable piece of text in the accent color.
function M.link(text, click, o)
  return M.press(extend({
    text = text, hl = "Accent", size = style.size.caption, hover = { opacity = 0.75 },
  }, o), click, "link")
end

--- A small label on a tinted chip ("IN USE"). o: hl, bg.
function M.pill(text, o)
  return style.finish(extend({
    text = text, size = style.size.micro, bold = true, hl = "Accent", bg = "TextSelection",
    radius = style.radius.sm, padding = { 3, 7 }, wrap = false,
  }, o))
end

--- A thin line between sections.
function M.divider(o)
  -- PromptBorder is a foreground. A background has to be a color that has one.
  return style.finish(extend({
    height = 1, bg = style.alpha("PromptBorder", "", "fg", "#3a3a3a"), margin = { 6, 0, 6, 0 },
  }, o))
end

-- ---- surfaces and controls ------------------------------------------------

--- A raised card; the fields of `o` and its array part (the children) are
--- the card's own. The look of a menu, a toast, a dialog.
function M.surface(o)
  return style.finish(extend({
    direction = "column", bg = "Panel", border = 1, border_color = "PromptBorder",
    radius = style.radius.xl,
  }, o))
end

--- An entry: a card, raised when it is the selected one; `click` runs when
--- it is pressed.
function M.entry(on, children, click)
  local node = {
    direction = "column", bg = on and "Selection" or nil, radius = style.radius.lg,
    padding = { 7, 12 }, margin = { 1, 0, 1, 0 }, role = "option", selected = on,
    transition = style.motion.base,
    hover = click and not on and { bg = style.hover() } or nil,
  }
  for _, c in ipairs(children) do node[#node + 1] = c end
  if click then M.press(node, click, "option", false) end
  return style.finish(node)
end

--- A button: raised, or `primary` (the accent's tint); `on` rings it, for
--- the one the keyboard is on.
function M.button(label, click, primary, on, o)
  return style.finish(M.press(extend({
    text = label, size = style.size.body, bold = primary, hl = primary and "Accent" or "Normal",
    bg = primary and "TextSelection" or "Selection", radius = style.radius.lg, padding = { 8, 18 },
    border = 1, border_color = on and "Accent" or "transparent",
    transition = style.motion.slow, hover = { opacity = 0.8 },
  }, o), click, "button", false))
end

--- A button that is a glyph: `label` names it for readers and in a tooltip.
--- o: box (side, default 22), size (the glyph's), hl, radius.
function M.icon_button(glyph, label, click, o)
  o = o or {}
  local box = o.box or 22
  local node = {
    width = box, height = box, radius = o.radius or style.radius.sm,
    direction = "row", justify = "center", align = "center",
    label = label, tooltip = label, transition = style.motion.fast,
    hover = { bg = "Selection", color = "Normal" },
    { text = glyph, size = o.size or style.size.caption, hl = o.hl or "Dim" },
  }
  o = extend({}, o)
  o.box, o.size, o.hl = nil, nil, nil
  return style.finish(M.press(extend(node, o), click, "button", false))
end

--- A check mark in front of an option: a glyph node.
function M.checkbox(on, o)
  return extend({
    text = on and "☑" or "☐", hl = on and "Accent" or "Dim", size = style.size.lead,
    role = "checkbox", selected = on,
  }, o)
end

--- The same as a span, to put in a line of text.
function M.check_span(on)
  return { text = on and "☑  " or "☐  ", hl = on and "Accent" or "Dim" }
end

-- One choice of a set, as a node.
local function segment(text, on, pick, o)
  return style.finish(M.press(extend({
    text = text, size = style.size.caption, bold = on, hl = on and "Normal" or "Dim",
    padding = { 4, 12 }, radius = style.radius.sm, bg = on and "Selection" or nil,
    hover = not on and { color = "Normal" } or nil, role = "option", selected = on,
    transition = style.motion.slow,
  }, o), function() if not on then pick() end end, "option", false))
end

--- A switch between a few views, joined in one rounded box. `names` are
--- the labels; `pick(i)` runs for one that is not the current.
function M.segmented(names, current, pick, o)
  local row = extend({
    direction = "row", gap = 2, padding = 2, radius = style.radius.md, bg = "Normal",
  }, o)
  for i, name in ipairs(names) do
    row[#row + 1] = segment(name, i == current, function() pick(i) end)
  end
  return style.finish(row)
end

--- Tabs along the top of a window: the current one raised.
function M.tabs(names, current, pick, o)
  local row = extend({ direction = "row", gap = 4, padding = { 2, 6 }, margin = style.margin(0, 8) }, o)
  for i, name in ipairs(names) do
    row[#row + 1] = segment(name, i == current, function() pick(i) end,
      { size = style.size.body, padding = { 5, 12 } })
  end
  return style.finish(row)
end

--- A form field: `label` over a box holding `value` (`editing` adds the
--- cursor, `on` rings it), and `hint` under it while it is the one on.
function M.field(label, value, o)
  o = o or {}
  local shown = o.editing and (value .. "▏") or (value ~= "" and value or (o.empty or ""))
  local box = style.finish({
    text = shown, size = style.size.body, hl = (value == "" and not o.editing) and "Dim" or "Normal",
    bg = "InputBackground", radius = style.radius.lg, padding = { 6, 12 },
    border = 1, border_color = o.on and "Accent" or "PromptBorder", transition = style.motion.base,
  })
  return style.finish({
    direction = "column", margin = { 0, 0, 9, 0 }, cursor = o.click and "pointer" or nil, on_click = o.click,
    style.finish({ text = label, size = style.size.small, hl = o.on and "Accent" or "Dim", margin = { 0, 0, 3, 2 } }),
    box,
    o.on and o.hint and style.finish({ text = o.hint, size = style.size.small, hl = "Dim", margin = { 4, 0, 0, 2 } }) or nil,
  })
end

-- ---- key hints --------------------------------------------------------------

-- What reads as a key in a hint ("enter open"), besides a single letter,
-- a modified key ("ctrl+o") and a function key.
local KEYS = {
  enter = 1, esc = 1, space = 1, tab = 1, ["↑↓"] = 1, ["←→"] = 1, ["↑"] = 1, ["↓"] = 1, ["←"] = 1,
  ["→"] = 1, up = 1, down = 1, left = 1, right = 1, pageup = 1, pagedown = 1, home = 1, ["end"] = 1,
  backspace = 1, delete = 1, wheel = 1,
}

local function is_key(k)
  return KEYS[k] ~= nil or k:match("^%w$") ~= nil or k:match("^%a+%+.+$") ~= nil or k:match("^[fF]%d%d?$") ~= nil
end

--- A row of key hints from a line such as "↑↓ move · enter open · esc
--- close": each key a small keycap, then what it does.
function M.hints(text, margin)
  local m = margin or { 14, 0 }
  local row = {
    direction = "row", flex_wrap = true, gap = 16, row_gap = 6, align = "center",
    padding = { 4, 10 }, margin = style.margin(m[1], m[2]),
  }
  for part in (text .. " · "):gmatch("(.-) · ") do
    local key, what = part:match("^(%S+)%s+(.*)$")
    if key and is_key(key) then
      row[#row + 1] = {
        direction = "row", gap = 6, align = "center",
        style.finish({ text = key, size = style.size.micro, font = "mono", bg = "Selection", radius = style.radius.sm - 1,
          padding = { 2, 7 } }),
        { text = what, size = style.size.small, hl = "Dim" },
      }
    elseif part ~= "" then
      row[#row + 1] = { text = part, size = style.size.small, hl = "Dim" }
    end
  end
  return style.finish(row)
end

-- ---- lists ------------------------------------------------------------------

local shown = {} -- list id -> the entry last scrolled into view

--- A scrolling list of `items` (nodes) for a window or panel, entry
--- `selected` (an index) kept in view; `id` names it. `opts.fit`: the
--- window is as tall as its entries, not the room it was given.
--- `opts.footer`: a node that stays under the list (the keys), however far
--- it scrolls.
function M.list(id, items, selected, opts)
  opts = opts or {}
  if selected and shown[id] ~= selected then
    -- Until the list has been drawn there is nothing to scroll.
    if bone.gui.scroll_item(id, selected, "nearest") then shown[id] = selected end
  end
  local list = {
    id = id, grow = 1, fit = opts.fit,
    list = { count = #items, estimate = 56, key = id, item = function(i) return items[i] end, fit = opts.fit },
  }
  if not opts.footer then return list end
  opts.footer.shrink = 0
  return { grow = 1, direction = "column", fit = opts.fit, list, opts.footer }
end

-- ---- the spinner ------------------------------------------------------------

--- The spinner's dots and the time one takes to go around ten of them.
M.spin = { dots = 10, interval = 100 }

--- A ring of dots with a bright head that goes around, drawn on a canvas
--- of its own every frame (at most 60 a second), so the window is not built
--- again to animate it and the head moves on each step, not up to a frame
--- late. `side` is its size in pixels.
function M.spinner(side)
  local s = M.spin
  -- The head is the running color; the dots behind it fade from the text's.
  local trail = {
    style.alpha("Normal", "cc", "fg", "Dim"), style.alpha("Normal", "88", "fg", "Dim"),
    style.alpha("Normal", "55", "fg", "Dim"),
  }
  local rest = style.alpha("Normal", "30", "fg", "Dim")
  return {
    width = side, height = side, role = "none",
    canvas = {
      fps = 60,
      draw = function(ctx)
        local draw, dots, count = bone.gui.draw, {}, s.dots
        local head = math.floor(ctx.time * 1000 / s.interval) % count
        local c, radius = side / 2, side * 0.34
        for i = 0, count - 1 do
          local angle = (i / count) * math.pi * 2 - math.pi / 2
          local behind = (head - i) % count
          dots[#dots + 1] = draw.circle({
            x = c + math.cos(angle) * radius,
            y = c + math.sin(angle) * radius,
            r = behind == 0 and 2.4 or 1.5,
            fill = behind == 0 and "ToolRunning" or trail[behind] or rest,
          })
        end
        return dots
      end,
    },
  }
end

return M
