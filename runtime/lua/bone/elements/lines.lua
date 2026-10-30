-- Elements as terminal lines: the terminal's renderer for bone.elements.
-- A library replaces this file to draw them its own way. Lines are the
-- usual lists of { "text", "Group" }; the groups are the colorscheme's
-- roles and the two bone.elements defines (ElementStrong, ElementDone).
local E = require("bone.elements")
local M = {}

local group = E.group
local NBSP = "\194\160" -- a space a line is never broken at

local function wrap(spans, width, opts)
  return bone.text.wrap(spans, math.max(width, 1), opts)
end

local function add(out, lines)
  for _, l in ipairs(lines) do out[#out + 1] = l end
end

-- The width of some spans, in cells.
local function cells(spans)
  local w = 0
  for _, s in ipairs(spans) do w = w + bone.text.width(s[1] or "") end
  return w
end

-- `line` as exactly `w` cells: cut, a fill stretched, padded.
local function fit(line, w)
  local spans, fill = {}, nil
  for _, s in ipairs(line) do
    if type(s) == "string" then s = { s, "Normal" } end
    if s.fill then fill = s else spans[#spans + 1] = s end
  end
  if cells(spans) > w then spans = bone.text.clip(spans, w) end
  local used = cells(spans)
  if fill and used < w then
    spans[#spans + 1] = { string.rep(fill.fill, w - used), fill.hl or "Normal" }
    used = w
  end
  if used < w then spans[#spans + 1] = { string.rep(" ", w - used), "Normal" } end
  return spans
end

-- ---- inline: an element as spans in a line, and its target's number

local inline = {}

function inline.text(el)
  local hl = el.strong and (el.tone == nil or el.tone == "normal") and "ElementStrong" or group(el.tone)
  return { { tostring(el.text or ""), hl } }
end

function inline.badge(el)
  return { { "[" .. tostring(el.text or ""):gsub(" ", NBSP) .. "]", group(el.tone, "Accent") } }
end

function inline.button(el, F)
  local hl = F.at(el) and "Selection" or el.primary and "Accent" or "Normal"
  local label = tostring(el.label or ""):gsub(" ", NBSP)
  return { { "[" .. NBSP .. label .. NBSP .. "]", hl } }, F.n(el)
end

function inline.spinner(_, F)
  return { { F.spinner or "…", "Accent" } }
end

function inline.meter(el)
  local v = math.max(0, math.min(1, tonumber(el.value) or 0))
  local width = 10
  local full = math.floor(v * width + 0.5)
  local spans = {
    { string.rep("█", full), group(el.tone, "Accent") },
    { string.rep("░", width - full), "Dim" },
  }
  if el.label then spans[#spans + 1] = { " " .. tostring(el.label), "Dim" } end
  return spans
end

function inline.hints(el)
  return { { tostring(el.text or ""), "Dim" } }
end

-- ---- blocks: an element as lines -------------------------------------------

local block = {}

local function draw(el, width, F, out)
  if type(el) ~= "table" then return end
  local k = el.kind
  local start = #out
  if block[k] then
    block[k](el, width, F, out)
  elseif inline[k] then
    local spans, n = inline[k](el, F)
    add(out, wrap(spans, width))
    F.hit(start + 1, n)
  elseif k == nil then
    -- A bare list of children: a column.
    block.column(el, width, F, out)
  else
    out[#out + 1] = { { "unknown element " .. tostring(k), "ErrorMsg" } }
  end
end

function block.text(el, width, _, out)
  if el.truncate then
    out[#out + 1] = bone.text.clip(inline.text(el), math.max(width, 1))
  else
    add(out, wrap(inline.text(el), width))
  end
end

function block.column(el, width, F, out)
  for _, child in ipairs(E.children(el)) do draw(child, width, F, out) end
end

-- Inline children side by side, two spaces apart, wrapped; a text with
-- `truncate` keeps the line to one row and gives up what does not fit.
function block.row(el, width, F, out)
  local parts, one_line = {}, false
  local function flush()
    if #parts == 0 then return end
    if one_line then
      local others = 2 * (#parts - 1)
      for _, p in ipairs(parts) do
        if not p.cut then others = others + cells(p.spans) end
      end
      for _, p in ipairs(parts) do
        if p.cut then
          local room = math.max(1, width - others)
          p.spans = { { bone.text.truncate(p.spans[1][1], room), p.spans[1][2] } }
        end
      end
    end
    local spans, hits, col = {}, {}, 0
    for i, p in ipairs(parts) do
      if i > 1 then
        spans[#spans + 1] = { "  ", "Normal" }
        col = col + 2
      end
      local w = cells(p.spans)
      if p.n then hits[#hits + 1] = { col + 1, col + w, p.n } end
      for _, s in ipairs(p.spans) do spans[#spans + 1] = s end
      col = col + w
    end
    local line = #out + 1
    if one_line then
      out[#out + 1] = bone.text.clip(spans, math.max(width, 1))
    else
      add(out, wrap(spans, width))
    end
    for _, h in ipairs(hits) do F.hit(line, h[3], h[1], h[2]) end
    parts, one_line = {}, false
  end
  for _, child in ipairs(E.children(el)) do
    if type(child) == "table" and inline[child.kind] then
      local spans, n = inline[child.kind](child, F)
      local cut = child.kind == "text" and child.truncate
      parts[#parts + 1] = { spans = spans, n = n, cut = cut }
      one_line = one_line or cut
    else
      flush()
      draw(child, width, F, out)
    end
  end
  flush()
end

function block.section(el, width, F, out)
  if #out > 0 then out[#out + 1] = {} end
  if el.title then add(out, wrap({ { tostring(el.title), "ElementStrong" } }, width)) end
  block.column(el, width, F, out)
end

-- `F` with its hits moved: `line` lines down and `col` columns right, cut
-- at column `last`.
local function shifted(F, line, col, last)
  return setmetatable({
    hit = function(l, n, from, to)
      from, to = (from or 1) + col, to and (to + col) or math.huge
      F.hit(l + line, n, from, last and math.min(to, last) or to)
    end,
  }, { __index = F })
end

-- A narrow column beside the rest, split by a rule; one under the other
-- where there is no room for both.
function block.split(el, width, F, out)
  local kids = E.children(el)
  local side, main = kids[1], kids[2]
  if width < 48 then
    draw(side, width, F, out)
    draw(main, width, F, out)
    return
  end
  local lw = math.min(32, math.floor(width * 0.35))
  local rw = width - lw - 3
  local left, right = {}, {}
  draw(side, lw, shifted(F, #out, 0, lw), left)
  draw(main, rw, shifted(F, #out, lw + 3, nil), right)
  for i = 1, math.max(#left, #right) do
    local line = fit(left[i] or {}, lw)
    line[#line + 1] = { " │ ", "WinSeparator" }
    for _, s in ipairs(fit(right[i] or {}, rw)) do line[#line + 1] = s end
    out[#out + 1] = line
  end
end

-- One entry of a list or checklist: `spans` wrapped under `first`, a
-- selection band when the cursor is on it (or it is the list's selected).
local function entry(el, i, first, spans, width, F, out, on)
  on = on or F.at(el, i)
  if on then
    for _, s in ipairs(first) do s[2] = "Selection" end
    for _, s in ipairs(spans) do s[2] = "Selection" end
  end
  local start = #out
  local rest = { { string.rep(" ", cells(first)), on and "Selection" or "Normal" } }
  add(out, wrap(spans, width, { first = #first > 0 and first or nil, rest = #first > 0 and rest or nil,
    pad = on and "Selection" or nil }))
  for line = start + 1, #out do F.hit(line, F.n(el, i)) end
end

function block.list(el, width, F, out)
  local items = el.items or {}
  if #items == 0 then
    if el.empty then add(out, wrap({ { tostring(el.empty), "Dim" } }, width)) end
    return
  end
  local marks = false
  for _, it in ipairs(items) do marks = marks or it.on ~= nil end
  local owned = E.controlled(el)
  for i, it in ipairs(items) do
    local first = {}
    if (it.depth or 0) > 0 then first[#first + 1] = { string.rep("  ", it.depth), "Normal" } end
    if marks then first[#first + 1] = { it.on and "● " or "  ", "Accent" } end
    local spans = { { tostring(it.text or ""), group(it.tone) } }
    for _, b in ipairs(it.badges or {}) do
      spans[#spans + 1] = { " " .. inline.badge(b)[1][1], group(b.tone, "Dim") }
    end
    if it.detail then spans[#spans + 1] = { "  " .. tostring(it.detail), "Dim" } end
    entry(el, i, first, spans, width, F, out, owned and i == el.selected)
  end
end

local CHECK = {
  todo = { "· ", "Dim", "Normal" },
  active = { "▸ ", "Accent", "ElementStrong" },
  done = { "✓ ", "Success", "ElementDone" },
}

function block.checklist(el, width, F, out)
  for i, it in ipairs(el.items or {}) do
    local c = CHECK[it.state] or CHECK.todo
    entry(el, i, { { c[1], c[2] } }, { { tostring(it.text or ""), c[3] } }, width, F, out)
  end
end

function block.tabs(el, width, F, out)
  local spans, col, line = {}, 0, #out + 1
  for i, name in ipairs(el.names or {}) do
    if i > 1 then
      spans[#spans + 1] = { " ", "Normal" }
      col = col + 1
    end
    local hl = F.at(el, i) and "Selection" or i == el.current and "Accent" or "Dim"
    local text = " " .. tostring(name) .. " "
    local w = bone.text.width(text)
    F.hit(line, F.n(el, i), col + 1, col + w)
    spans[#spans + 1] = { text, hl }
    col = col + w
  end
  add(out, wrap(spans, width))
end

function block.field(el, width, F, out)
  local on = F.at(el)
  local value = tostring(el.value or "")
  local shown, hl = value, "Normal"
  if el.editing then
    shown = value .. "▏"
  elseif value == "" then
    shown, hl = tostring(el.empty or ""), "Dim"
  end
  local start = #out
  local label = { { tostring(el.label or "") .. ": ", on and "Selection" or "Dim" } }
  -- Each line of the value under the label's width.
  local first = true
  for text in (shown .. "\n"):gmatch("(.-)\n") do
    local spans = { { text, on and "Selection" or hl } }
    local rest = { { string.rep(" ", cells(label)), on and "Selection" or "Normal" } }
    add(out, wrap(spans, width, { first = first and label or rest, rest = rest, pad = on and "Selection" or nil }))
    first = false
  end
  for line = start + 1, #out do F.hit(line, F.n(el)) end
  if (on or el.editing) and el.hint then add(out, wrap({ { "  " .. tostring(el.hint), "Dim" } }, width)) end
end

function block.diff(el, width, _, out)
  local rows = el.rows or {}
  local shown, cut = rows, 0
  if el.cap and not el.expanded and #rows > el.cap then
    shown, cut = {}, #rows - el.cap
    for i = 1, el.cap do shown[i] = rows[i] end
  end
  local digits = 1
  if el.numbers then
    for _, r in ipairs(shown) do
      digits = math.max(digits, #tostring(r.old or ""), #tostring(r.new or ""))
    end
  end
  -- Too narrow for numbers beside the code: only the sign.
  local numbers = el.numbers and width >= 24 + 2 * digits
  for _, r in ipairs(shown) do
    if r.separator then
      out[#out + 1] = { { "···", "Dim" } }
    elseif not r.sign then
      add(out, wrap({ { tostring(r.text or ""):gsub("\t", "    "), group(r.tone, "Dim") } }, width))
    else
      local hl, mark = "Dim", " "
      if r.sign == "+" then hl, mark = "DiffAdd", "+" elseif r.sign == "-" then hl, mark = "DiffDelete", "-" end
      local gutter = mark .. " "
      if numbers then
        gutter = string.format("%" .. digits .. "s %" .. digits .. "s %s ", r.old or "", r.new or "", mark)
      end
      local text = tostring(r.text or ""):gsub("\t", "    ")
      local band = mark ~= " " and hl or nil
      add(out, wrap({ { text, hl } }, width, {
        first = { { gutter, band or "Dim" } },
        rest = { { string.rep(" ", bone.text.width(gutter)), band or "Dim" } },
        pad = band,
      }))
    end
  end
  if cut > 0 then
    out[#out + 1] = { { "⋯ " .. cut .. " more line" .. (cut == 1 and "" or "s"), "Dim" } }
  end
end

function block.code(el, width, _, out)
  for line in (tostring(el.text or "") .. "\n"):gmatch("(.-)\n") do
    out[#out + 1] = { { bone.text.truncate(line:gsub("\t", "    "), width), "MdCodeBlock" } }
  end
end

function block.markdown(el, width, _, out)
  add(out, bone.ui.markdown(tostring(el.text or ""), math.max(width, 1), ""))
end

function block.divider(_, width, _, out)
  out[#out + 1] = { { string.rep("─", math.max(width, 1)), "WinSeparator" } }
end

--- `tree` as lines `width` cells wide. `F` is bone.elements.focus's: each
--- target's place goes to `F.hit`.
function M.render(tree, width, F)
  local out = {}
  draw(tree, width, F, out)
  return out
end

return M
