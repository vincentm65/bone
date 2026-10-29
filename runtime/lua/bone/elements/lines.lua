-- Elements as terminal lines: the terminal's renderer for bone.elements.
-- A library replaces this file to draw them its own way. Lines are the
-- usual lists of { "text", "Group" }; the groups are the colorscheme's
-- roles and the two bone.elements defines (ElementStrong, ElementDone).
local E = require("bone.elements")
local M = {}

local group = E.group

local function wrap(spans, width, opts)
  return bone.text.wrap(spans, math.max(width, 1), opts)
end

local function add(out, lines)
  for _, l in ipairs(lines) do out[#out + 1] = l end
end

-- ---- inline: an element as spans in a line, and its target's number

local inline = {}

function inline.text(el)
  local hl = el.strong and (el.tone == nil or el.tone == "normal") and "ElementStrong" or group(el.tone)
  return { { tostring(el.text or ""), hl } }
end

function inline.badge(el)
  return { { "[" .. tostring(el.text or "") .. "]", group(el.tone, "Accent") } }
end

function inline.button(el, F)
  local hl = F.at(el) and "Selection" or el.primary and "Accent" or "Normal"
  return { { "[ " .. tostring(el.label or "") .. " ]", hl } }, F.n(el)
end

function inline.spinner(_, F)
  return { { F.spinner or "…", "Accent" } }
end

function inline.meter(el)
  local v = math.max(0, math.min(1, tonumber(el.value) or 0))
  local cells = 10
  local full = math.floor(v * cells + 0.5)
  local spans = {
    { string.rep("█", full), group(el.tone, "Accent") },
    { string.rep("░", cells - full), "Dim" },
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
  add(out, wrap(inline.text(el), width))
end

function block.column(el, width, F, out)
  for _, child in ipairs(E.children(el)) do draw(child, width, F, out) end
end

-- The width of some spans, in cells.
local function cells(spans)
  local w = 0
  for _, s in ipairs(spans) do w = w + bone.text.width(s[1] or "") end
  return w
end

function block.row(el, width, F, out)
  local spans, hits, col = {}, {}, 0
  local function flush()
    if #spans == 0 then return end
    local line = #out + 1
    add(out, wrap(spans, width))
    for _, h in ipairs(hits) do F.hit(line, h[3], h[1], h[2]) end
    spans, hits, col = {}, {}, 0
  end
  for _, child in ipairs(E.children(el)) do
    if type(child) == "table" and inline[child.kind] then
      if #spans > 0 then
        spans[#spans + 1] = { "  ", "Normal" }
        col = col + 2
      end
      local more, n = inline[child.kind](child, F)
      local w = cells(more)
      if n then hits[#hits + 1] = { col + 1, col + w, n } end
      for _, s in ipairs(more) do spans[#spans + 1] = s end
      col = col + w
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

-- One entry of a list or checklist: `spans` wrapped under `first`, a
-- selection band when the cursor is on it.
local function entry(el, i, first, spans, width, F, out)
  local on = F.at(el, i)
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
  for i, it in ipairs(items) do
    local first = {}
    if marks then first[1] = { it.on and "● " or "  ", "Accent" } end
    local spans = { { tostring(it.text or ""), group(it.tone) } }
    if it.detail then spans[#spans + 1] = { "  " .. tostring(it.detail), "Dim" } end
    entry(el, i, first, spans, width, F, out)
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
  local spans = { { tostring(el.label or "") .. ": ", on and "Selection" or "Dim" }, { shown, on and "Selection" or hl } }
  local start = #out
  add(out, wrap(spans, width, { pad = on and "Selection" or nil }))
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
  for _, r in ipairs(shown) do
    if r.separator then
      out[#out + 1] = { { "···", "Dim" } }
    elseif not r.sign then
      add(out, wrap({ { tostring(r.text or ""):gsub("\t", "    "), group(r.tone, "Dim") } }, width))
    else
      local hl, mark = "Dim", " "
      if r.sign == "+" then hl, mark = "DiffAdd", "+" elseif r.sign == "-" then hl, mark = "DiffDelete", "-" end
      local gutter = mark .. " "
      if el.numbers then
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

function block.divider(_, _, _, out)
  out[#out + 1] = { { fill = "─", hl = "WinSeparator" } }
end

--- `tree` as lines `width` cells wide. `F` is bone.elements.focus's: each
--- target's place goes to `F.hit`.
function M.render(tree, width, F)
  local out = {}
  draw(tree, width, F, out)
  return out
end

return M
