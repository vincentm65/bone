-- Elements as desktop nodes: the desktop app's renderer for bone.elements,
-- made of bone.nodes.widgets, so elements look like the rest of the window
-- and follow bone.nodes.style (and its bare look). A library replaces this
-- file to draw them its own way.
local E = require("bone.elements")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local M = {}

local group = E.group

-- The keyboard's cursor on what it is on: the hover's lift, a little
-- stronger, over a node that has no fill of its own (a button and a field
-- show it their own way).
local function cursor(node, on)
  if on and node.bg == nil then node.bg = style.alpha("Normal", "14", "fg", "Selection") end
  return node
end

-- A target's node, named so the cursor can be scrolled into view.
local function target(node, F, el, i)
  node.id = node.id or F.id(F.n(el, i))
  return node
end

local draw

local kinds = {}

function kinds.text(el)
  local strong = el.strong and (el.tone == nil or el.tone == "normal")
  local node = {
    text = tostring(el.text or ""), hl = group(el.tone), bold = strong or nil, size = style.size.body,
  }
  if el.truncate then node.wrap, node.text_overflow = false, "ellipsis" end
  return style.finish(node)
end

local function children(node, el, ctx, F)
  for _, child in ipairs(E.children(el)) do node[#node + 1] = draw(child, ctx, F) end
  return node
end

function kinds.column(el, ctx, F)
  return children({ direction = "column", gap = style.space.sm }, el, ctx, F)
end

-- A text with `truncate` takes the row's room left over, and gives up the rest.
function kinds.row(el, ctx, F)
  local one_line = false
  local node = { direction = "row", gap = style.space.md, align = "center" }
  for _, child in ipairs(E.children(el)) do
    local n = draw(child, ctx, F)
    if n and child.kind == "text" and child.truncate then
      n.grow, n.width, one_line = 1, 0, true
    end
    node[#node + 1] = n
  end
  node.flex_wrap = not one_line or nil
  return node
end

function kinds.section(el, ctx, F)
  local node = { direction = "column", gap = style.space.sm, margin = style.margin(style.space.sm, 0) }
  if el.title then
    node[1] = style.finish({ text = tostring(el.title), hl = "Dim", bold = true, size = style.size.small })
  end
  return children(node, el, ctx, F)
end

-- A narrow column beside the rest.
function kinds.split(el, ctx, F)
  local kids = E.children(el)
  return {
    direction = "row", gap = style.space.lg, align = "start",
    { width = 240, shrink = 0, direction = "column", draw(kids[1], ctx, F) },
    { grow = 1, width = 0, direction = "column", draw(kids[2], ctx, F) },
  }
end

function kinds.list(el, _, F)
  local items = el.items or {}
  if #items == 0 then
    return el.empty and widgets.note(tostring(el.empty), { 0, 0 }) or nil
  end
  local owned = E.controlled(el)
  local list = { direction = "column" }
  for i, it in ipairs(items) do
    local line = {
      direction = "row", gap = style.space.md, align = "center",
      { text = tostring(it.text or ""), hl = group(it.tone), size = style.size.body, grow = 1, width = 0,
        wrap = false, text_overflow = "ellipsis" },
    }
    for _, b in ipairs(it.badges or {}) do
      line[#line + 1] = widgets.pill(tostring(b.text or ""), { hl = group(b.tone, "Dim"), bg = "Selection" })
    end
    if it.detail then
      line[#line + 1] = { text = tostring(it.detail), hl = "Dim", size = style.size.small, wrap = false }
    end
    local on = it.on == true or (owned and i == el.selected)
    local node = widgets.entry(on, { line }, F.press(el, i))
    if (it.depth or 0) > 0 then
      local pad = node.padding or { 5, 10 }
      node.padding = { pad[1], pad[2], pad[1], pad[2] + it.depth * 16 }
    end
    list[#list + 1] = target(cursor(node, F.at(el, i)), F, el, i)
  end
  return list
end

local STATES = { todo = "todo", active = "active", done = "done" }

function kinds.checklist(el, _, F)
  local items = {}
  for i, it in ipairs(el.items or {}) do
    items[i] = { text = tostring(it.text or ""), state = STATES[it.state] or "todo" }
  end
  local node = widgets.checklist(items)
  for i, row in ipairs(node) do
    local click = F.press(el, i)
    if click then
      widgets.press(row, click, "checkbox")
      target(cursor(row, F.at(el, i)), F, el, i)
      row.radius = style.radius.sm
    end
  end
  return node
end

function kinds.button(el, _, F)
  return target(widgets.button(tostring(el.label or ""), F.press(el), el.primary, F.at(el)), F, el)
end

-- The window's switch between views (the sidebar's Chats/Processes), as
-- wide as its choices.
function kinds.tabs(el, _, F)
  local node = widgets.segmented(el.names or {}, el.current, function(i)
    local click = F.press(el, i)
    if click then click() end
  end, { align_self = "start" })
  for i, tab in ipairs(node) do target(cursor(tab, F.at(el, i)), F, el, i) end
  return node
end

function kinds.field(el, _, F)
  return target(widgets.field(tostring(el.label or ""), tostring(el.value or ""), {
    editing = el.editing, on = F.at(el) or el.editing, hint = el.hint, empty = el.empty, click = F.press(el),
  }), F, el)
end

function kinds.diff(el)
  local rows = {}
  for i, r in ipairs(el.rows or {}) do
    if r.sign or r.separator then
      rows[i] = r
    else
      rows[i] = { text = r.text, hl = group(r.tone, "Dim") }
    end
  end
  return widgets.diff(rows, { numbers = el.numbers, cap = el.cap, expanded = el.expanded })
end

function kinds.code(el)
  return style.finish({
    text = tostring(el.text or ""), font = "mono", size = style.size.small, hl = "MdCodeBlock",
    bg = "CodeBlock", radius = style.radius.md, padding = style.pad(6, 10), selectable = true,
  })
end

function kinds.markdown(el)
  local md = require("bone.desktop.markdown")
  local node = { direction = "column", size = md.text_size }
  for _, n in ipairs(md.nodes(tostring(el.text or "")) or {}) do node[#node + 1] = n end
  return node
end

function kinds.badge(el)
  return widgets.pill(tostring(el.text or ""), { hl = group(el.tone, "Accent") })
end

function kinds.meter(el)
  local v = math.max(0, math.min(1, tonumber(el.value) or 0))
  local fill = style.alpha(group(el.tone, "Accent"), "ff", "fg", "Accent")
  return {
    direction = "row", gap = style.space.md, align = "center",
    style.finish({
      width = 80, height = 4, radius = 2, bg = "Selection", shrink = 0, direction = "row",
      style.finish({ width = string.format("%d%%", math.floor(v * 100 + 0.5)), radius = 2, bg = fill }),
    }),
    el.label and { text = tostring(el.label), hl = "Dim", size = style.size.small, wrap = false } or nil,
  }
end

function kinds.spinner()
  return widgets.spinner(14)
end

function kinds.divider()
  return widgets.divider()
end

function kinds.hints(el)
  return widgets.hints(tostring(el.text or ""), { 0, 0 })
end

function draw(el, ctx, F)
  if type(el) ~= "table" then return nil end
  local k = el.kind
  if k == nil then return kinds.column(el, ctx, F) end
  local fn = kinds[k]
  if not fn then
    return { text = "unknown element " .. tostring(k), hl = "ErrorMsg", size = style.size.small }
  end
  return fn(el, ctx, F)
end

--- `tree` as a node; `ctx` is the window's or panel's, `F` bone.elements.focus's.
--- In a window or panel it scrolls (the page keys and the wheel move it).
function M.render(tree, ctx, F)
  local node = draw(tree, ctx, F)
  if not node or not F.scroll_id then return node end
  return { id = F.scroll_id, grow = 1, direction = "column", overflow = "scroll", node }
end

return M
