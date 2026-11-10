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
    text = text, hl = "Normal", bold = true, size = style.size.lead, letter_spacing = -0.1,
    padding = { 2, style.inset }, margin = style.margin(0, 6),
  })
end

--- A line of quiet text; `margin` is { above, below }.
function M.note(text, margin, hl)
  local m = margin or { 10, 0 }
  return style.finish({
    text = text, hl = hl or "Dim", size = style.size.small,
    padding = { 4, style.inset }, margin = style.margin(m[1], m[2]),
  })
end

--- A quiet label over a group of entries.
function M.heading(text)
  return style.finish({
    text = text, hl = "Dim", medium = true, size = style.size.small,
    padding = { 2, style.inset }, margin = style.margin(6, 2),
  })
end

--- The mark in front of a step or a check: "done" (a check), "failed" (a
--- cross), "active" (an arrow), else a dot; in `hl`, or the state's own
--- color. Drawn, so it asks no font for a glyph.
local MARK = { done = { "check", "DiffAdd" }, failed = { "close", "ErrorMsg" }, active = { "chevron_right", "Accent" } }
function M.mark(state, hl)
  local m = MARK[state]
  return {
    width = 14, height = 16, shrink = 0, direction = "row", justify = "center", align = "center",
    m and require("bone.nodes.icons").node(m[1], { size = 12, hl = hl or m[2], width = 2 })
      or { width = 4, height = 4, radius = 2, bg = style.alpha(hl or "Dim", "ff", "fg", "#7a7a7a") },
  }
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
    text = text, size = style.size.micro, medium = true, hl = "Muted", bg = "Selection",
    radius = style.radius.sm, padding = { 2, 7 }, wrap = false,
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

--- A card floating on the window: the composer's look (its fill, border
--- and corners) with a soft shadow. The sidebar and docked panels are these.
function M.card(o)
  return style.finish(extend({
    direction = "column", bg = "InputBackground", border = 1, border_color = "PromptBorder",
    radius = style.radius.round, shadow = style.shadow.bubble,
  }, o))
end

--- An entry: a card, raised when it is the selected one; `click` runs when
--- it is pressed.
function M.entry(on, children, click)
  local node = {
    direction = "column", bg = on and "Selection" or nil, radius = style.radius.lg,
    padding = { 5, 10 }, role = "option", selected = on,
    transition = style.motion.base,
    hover = click and not on and { bg = style.hover() } or nil,
  }
  for _, c in ipairs(children) do node[#node + 1] = c end
  if click then M.press(node, click, "option", false) end
  return style.finish(node)
end

--- A button: raised, or `primary` (filled in the text's color, the text in
--- the window's); `on` rings it, for the one the keyboard is on.
function M.button(label, click, primary, on, o)
  local fill = primary and style.alpha("Normal", "ff", "fg", "TextSelection") or "Selection"
  local ink = primary and style.alpha("Normal", "ff", "bg", nil) or nil
  return style.finish(M.press(extend({
    text = label, size = style.size.body, medium = true, hl = (primary and not ink) and "Accent" or "Normal",
    color = ink, bg = fill, radius = style.radius.lg, padding = { 7, 14 },
    border = 1, border_color = on and (primary and "PromptBorder" or "Muted") or "transparent",
    transition = style.motion.slow, hover = { opacity = 0.85 },
  }, o), click, "button", false))
end

--- A button that is a glyph, or an icon of bone.nodes.icons by name:
--- `label` names it for readers and in a tooltip.
--- o: box (side, default 22), size (the glyph's or icon's), hl, radius.
function M.icon_button(glyph, label, click, o)
  o = o or {}
  local box = o.box or 22
  local icons = require("bone.nodes.icons")
  local mark = icons.has(glyph) and icons.node(glyph, { size = o.size or 16, hl = o.hl or "Dim" })
    or { text = glyph, size = o.size or style.size.caption, hl = o.hl or "Dim" }
  local node = {
    width = box, height = box, radius = o.radius or style.radius.sm,
    direction = "row", justify = "center", align = "center",
    label = label, tooltip = label, transition = style.motion.fast,
    hover = { bg = "Selection", color = "Normal" },
    mark,
  }
  o = extend({}, o)
  o.box, o.size, o.hl = nil, nil, nil
  return style.finish(M.press(extend(node, o), click, "button", false))
end

--- An on/off switch: a track with a knob; on, the track is the text's color
--- and the knob the window's. `click` runs when it is pressed.
function M.switch(on, click)
  local fill = on and style.alpha("Normal", "ff", "fg", "TextSelection") or style.alpha("PromptBorder", "ff", "fg", "Selection")
  local knob = on and style.alpha("Normal", "ff", "bg", "Normal") or style.alpha("Dim", "ff", "fg", "Normal")
  local node = style.finish({
    width = 30, height = 18, radius = 9, padding = 2, shrink = 0, bg = fill,
    direction = "row", justify = on and "end" or "start", align = "center",
    role = "checkbox", selected = on, transition = style.motion.slow,
    { width = 14, height = 14, radius = 7, bg = knob },
  })
  if click then M.press(node, click, "checkbox", false) end
  return node
end

--- A check box in front of an option: filled, with a check, when `on`.
function M.checkbox(on, o)
  local fill = on and style.alpha("Normal", "ff", "fg", "TextSelection") or nil
  return style.finish(extend({
    width = 14, height = 14, radius = 4, shrink = 0, bg = fill, justify = "center", align = "center",
    border = 1, border_color = fill or "Muted", role = "checkbox", selected = on,
    on and require("bone.nodes.icons").node("check", { size = 10, width = 2.4,
      hl = style.alpha("Normal", "ff", "bg", "#000000") }) or nil,
  }, o))
end

--- A checklist: `items` are { text, state = "todo", "active" or "done" }.
--- A dot for one to do, an accent arrow and bold text for the active one, a
--- green check and struck-through grey text for one done.
function M.checklist(items, o)
  local list = extend({ direction = "column", gap = 3 }, o)
  for _, it in ipairs(items) do
    local done, active = it.state == "done", it.state == "active"
    list[#list + 1] = {
      direction = "row", gap = 6, align = "start", role = "checkbox", selected = done,
      M.mark(it.state),
      { text = tostring(it.text or ""), hl = done and "Dim" or "Normal", size = style.size.body,
        bold = active or nil, strike = done or nil, grow = 1, width = 0 },
    }
  end
  return style.finish(list)
end

-- One choice of a set, as a node.
local function segment(text, on, pick, o)
  return style.finish(M.press(extend({
    text = text, size = style.size.small, medium = on, hl = on and "Normal" or "Muted",
    padding = { 4, 10 }, radius = style.radius.sm, bg = on and "Selection" or nil,
    hover = not on and { color = "Normal" } or nil, role = "option", selected = on,
    transition = style.motion.slow,
  }, o), function() if not on then pick() end end, "option", false))
end

--- A switch between a few views, joined in one rounded box. `names` are
--- the labels; `pick(i)` runs for one that is not the current. `o.item`
--- replaces fields of each choice (a smaller `size` and `padding`).
function M.segmented(names, current, pick, o)
  local item = o and o.item
  local row = extend({
    direction = "row", gap = 2, padding = 2, radius = style.radius.md, bg = "Normal",
    border = 1, border_color = "WinSeparator", align_self = "start",
  }, o)
  row.item = nil
  for i, name in ipairs(names) do
    row[#row + 1] = segment(name, i == current, function() pick(i) end, item)
  end
  return style.finish(row)
end

--- Tabs along the top of a window: a segmented switch, the current one
--- raised.
function M.tabs(names, current, pick, o)
  -- In a row of its own, so a list that stretches its entries does not
  -- stretch the switch.
  local seg = M.segmented(names, current, function(i) pick(i) end, o)
  local margin = seg.margin
  seg.margin = nil
  return { direction = "row", margin = margin or style.margin(0, 6), padding = { 0, style.inset - 2 }, seg }
end

--- A dialog's head: its `title`, then at the right `o.tabs` (names; `o.tab`
--- is the current one, `o.pick(i)` runs for another) and × (`o.close`).
function M.dialog_head(title, o)
  o = o or {}
  return style.finish({
    direction = "row", align = "center", gap = style.space.md, shrink = 0, min_height = 30, margin = { 0, 0, 6, 0 },
    { text = title, hl = "Normal", bold = true, size = style.size.lead, grow = 1, width = 0, wrap = false,
      text_overflow = "ellipsis", padding = { 0, style.inset } },
    o.tabs and M.segmented(o.tabs, o.tab, o.pick) or nil,
    o.close and M.icon_button("close", "Close (esc)", o.close, { box = 28, size = 15, radius = style.radius.md }) or nil,
  })
end

--- A dialog's search field, under its head: `query`, with the cursor while
--- `o.typing`; `o.placeholder`; `o.click` (a press on it), `o.clear` (its ×,
--- while there is a query), else `o.key` as the cap that comes to it.
function M.search_box(query, o)
  local shown = o.typing and (query .. "▏") or (query ~= "" and query or (o.placeholder or "Search"))
  return M.press({
    direction = "row", align = "center", gap = 8, shrink = 0, height = 32, margin = { 0, 0, 6, 0 },
    padding = { 0, style.inset }, bg = "Normal", radius = style.radius.md, border = 1,
    border_color = o.typing and "Muted" or "PromptBorder", cursor = "text",
    require("bone.nodes.icons").node("search", { size = 15, hl = "Dim" }),
    { text = shown, hl = (o.typing or query ~= "") and "Normal" or "Dim", size = style.size.body, grow = 1, width = 0,
      wrap = false, text_overflow = "ellipsis" },
    (query ~= "" and o.clear) and M.icon_button("close", "Clear the search", o.clear, { box = 20, size = 13 })
      or (o.key and not o.typing and M.keycap(o.key)) or nil,
  }, o.click, "button", false)
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
  -- The label, the box and the hint on the text edge, with the heading
  -- above and the buttons under the form.
  return style.finish({
    direction = "column", margin = { 0, 0, 9, 0 }, padding = { 0, style.inset },
    cursor = o.click and "pointer" or nil, on_click = o.click,
    style.finish({ text = label, size = style.size.small, hl = o.on and "Accent" or "Dim", margin = { 0, 0, 3, 0 } }),
    box,
    o.on and o.hint and style.finish({ text = o.hint, size = style.size.small, hl = "Dim", margin = { 4, 0, 0, 0 } }) or nil,
  })
end

--- A fixed space under a list, two lines tall: what the selected entry is,
--- what just happened, or a question. Text (`hl`, Dim by default) or a node.
--- The same height whatever it holds, so moving through the list never moves
--- the list.
function M.detail(content, hl)
  local node = { height = 2 * math.ceil(style.size.small * 1.35) + 8, padding = { 4, style.inset }, overflow = "hidden",
    direction = "column", justify = "center", shrink = 0 }
  if type(content) == "table" then
    node[1] = content
  elseif content and content ~= "" then
    -- Two lines at most: a longer text is cut at a word with an ellipsis
    -- rather than spilling a third, half-clipped line over what is under it.
    -- (Text cannot be measured here; a dialog's width in average chars.)
    local room = style.dialog.inner or style.dialog.width
    local fit = math.floor(2 * (room - 2 * style.inset - 40) / (style.size.small * 0.55))
    if #content > fit then
      content = content:sub(1, fit):gsub("%s+%S*$", "") .. "…"
    end
    node[1] = { text = content, hl = hl or "Dim", size = style.size.small }
  end
  return style.finish(node)
end

-- ---- key hints --------------------------------------------------------------

-- What reads as a key in a hint ("enter open"), besides a single letter,
-- a modified key ("ctrl+o") and a function key.
local KEYS = {
  enter = 1, esc = 1, space = 1, tab = 1, ["↑↓"] = 1, ["←→"] = 1, ["↑"] = 1, ["↓"] = 1, ["←"] = 1,
  ["→"] = 1, up = 1, down = 1, left = 1, right = 1, pageup = 1, pagedown = 1, home = 1, ["end"] = 1,
  backspace = 1, delete = 1, wheel = 1,
}

-- A single character is a key too ("/", "?", "x").
local function is_key(k)
  return KEYS[k] ~= nil or #k == 1 or k:match("^%a+%+.+$") ~= nil or k:match("^[fF]%d%d?$") ~= nil
end

--- A key as a small outlined cap, named one way everywhere: "esc" and
--- "Esc" are "Esc", "ctrl+o" is "Ctrl O"; a single character is itself.
function M.keycap(key)
  if #key > 1 and key:match("^%a") then
    key = key:gsub("%+", " "):gsub("%a+", function(w) return #w > 1 and (w:sub(1, 1):upper() .. w:sub(2)) or w:upper() end)
  end
  local cap = {
    text = key, size = style.size.micro, font = "mono", hl = "Dim", wrap = false,
    border = 1, border_color = "PromptBorder", radius = style.radius.sm - 2, padding = { 1, 5 },
  }
  if key == "↑↓" then
    -- Drawn: the font's two arrows run together.
    local icons = require("bone.nodes.icons")
    cap.text, cap.direction, cap.align, cap.height = nil, "row", "center", 18
    cap[1], cap[2] = icons.node("arrow_up", { size = 10 }), icons.node("arrow_down", { size = 10 })
  end
  return style.finish(cap)
end

--- A row of key hints from a line such as "↑↓ move · enter open · esc
--- close": each key a small keycap, then what it does.
function M.hints(text, margin)
  local m = margin or { 8, 0 }
  local row = {
    direction = "row", flex_wrap = true, gap = 14, row_gap = 4, align = "center",
    padding = { 2, style.inset }, margin = style.margin(m[1], m[2]),
  }
  for part in (text .. " · "):gmatch("(.-) · ") do
    local key, what = part:match("^(%S+)%s+(.*)$")
    if key and is_key(key) then
      -- Esc stands apart, at the right of its line (the last, if they wrap).
      local esc = key:lower() == "esc"
      row[#row + 1] = {
        direction = "row", gap = 6, align = "center", grow = esc and 1 or nil, justify = esc and "end" or nil,
        M.keycap(key),
        { text = what, size = style.size.small, hl = "Dim" },
      }
    elseif part ~= "" then
      row[#row + 1] = { text = part, size = style.size.small, hl = "Dim" }
    end
  end
  return style.finish(row)
end

-- ---- code -------------------------------------------------------------------

--- A diff as one block of monospace rows: `rows` are { sign = "+", "-" or
--- " ", text, old, new } (old and new are line numbers, shown with
--- `o.numbers`), { separator = true } ("···" between hunks), or { text, hl }
--- with no sign (a hunk header, a note; Dim by default). Changed rows are
--- highlighted flat across the block, unchanged ones grey. `o.cap` cuts it
--- to that many rows, with how many more under them, unless `o.expanded`.
function M.diff(rows, o)
  o = o or {}
  if #rows == 0 then return nil end
  local shown, cut = rows, 0
  if o.cap and not o.expanded and #rows > o.cap then
    shown, cut = {}, #rows - o.cap
    for i = 1, o.cap do shown[i] = rows[i] end
  end
  local size = o.size or style.size.small
  local digits = 1
  if o.numbers then
    for _, r in ipairs(shown) do
      digits = math.max(digits, #tostring(r.old or ""), #tostring(r.new or ""))
    end
  end
  -- Vertical padding covers the corner curve, so a highlighted row's square
  -- background never paints over a rounded card around the block.
  local block = { direction = "column", padding = { 6, 0 }, font = "mono", size = size }
  local function add(text, hl, bg, py)
    block[#block + 1] = style.finish({
      bg = bg, padding = { py or 1, 12 },
      { text = text, hl = hl, font = "mono", size = size, selectable = true },
    })
  end
  for _, r in ipairs(shown) do
    if r.separator then
      add("···", "Dim", nil, 6)
    elseif not r.sign then
      add(tostring(r.text or ""):gsub("\t", "    "), r.hl or "Dim", nil, 4)
    else
      local hl, mark = "Dim", " "
      if r.sign == "+" then hl, mark = "DiffAdd", "+" elseif r.sign == "-" then hl, mark = "DiffDelete", "-" end
      local bg = mark ~= " " and hl or nil
      local text = tostring(r.text or ""):gsub("\t", "    ")
      if o.numbers then
        -- The numbers stay put; the code beside them wraps.
        block[#block + 1] = style.finish({
          direction = "row", align = "start", bg = bg, padding = { 1, 12 },
          { text = string.format("%" .. digits .. "s %" .. digits .. "s %s ", r.old or "", r.new or "", mark),
            hl = bg and hl or "Dim", font = "mono", size = size, wrap = false, shrink = 0 },
          { text = text, hl = hl, font = "mono", size = size, grow = 1, width = 0, selectable = true },
        })
      else
        add("  " .. mark .. " " .. text, hl, bg)
      end
    end
  end
  if cut > 0 then add("⋯ " .. cut .. " more line" .. (cut == 1 and "" or "s"), "Dim", nil, 4) end
  return style.finish(block)
end

-- ---- lists ------------------------------------------------------------------

local shown = {} -- list id -> the entry last scrolled into view

--- A dialog's foot: `node` (the keys) under a hairline the width of the window.
function M.footer(node)
  node.shrink = 0
  return { direction = "column", shrink = 0, margin = { 4, -style.inset - 4, 0, -style.inset - 4 },
    padding = { 6, style.inset + 4, 0, style.inset + 4 }, border = { 1, 0, 0, 0 }, border_color = "WinSeparator",
    node }
end

--- A scrolling list of `items` (nodes) for a window or panel, entry
--- `selected` (an index) kept in view; `id` names it. `opts.fit`: the
--- window is as tall as its entries, not the room it was given.
--- `opts.footer`: a node that stays under the list (the keys), however far
--- it scrolls. `opts.estimate`: an entry's height in pixels before it is
--- measured (56); near the real one, a `fit` window opens at its size.
function M.list(id, items, selected, opts)
  opts = opts or {}
  if selected and shown[id] ~= selected then
    -- Until the list has been drawn there is nothing to scroll.
    if bone.gui.scroll_item(id, selected, "nearest") then shown[id] = selected end
  end
  local list = {
    id = id, grow = 1, fit = opts.fit,
    -- In a dialog the scrollbar has a lane of its own, off the rows' fills.
    padding = opts.footer and { 0, 10, 0, 0 } or nil,
    list = { count = #items, estimate = opts.estimate or 56, key = id, item = function(i) return items[i] end, fit = opts.fit },
  }
  if not opts.footer then return list end
  opts.footer = M.footer(opts.footer)
  -- A dialog's list fades into its footer rather than cutting a row there.
  local clear, solid = style.alpha("Panel", "00", "bg"), style.alpha("Panel", "ff", "bg")
  if clear and solid and not opts.fit then
    list = { grow = 1, direction = "column", list,
      { position = "absolute", left = 0, right = 0, bottom = 0, height = 20, z = 1, gradient = { clear, solid } } }
  end
  return { grow = 1, direction = "column", fit = opts.fit, list, opts.footer }
end

-- ---- the spinner ------------------------------------------------------------

--- The spinner's dots and the time one takes to go around ten of them.
M.spin = { dots = 10, interval = 100 }

--- A ring of dots with a bright head that goes around, drawn on a canvas
--- of its own, so the window is not built again to animate it. It is drawn
--- twice a step: the head moves on time, and nothing is painted between.
--- `side` is its size in pixels.
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
      fps = 2000 / s.interval,
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
