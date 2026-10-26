-- Markdown as nodes: headings, paragraphs (their lines joined; the window
-- wraps them), list items, quotes, code blocks, tables and rules, from
-- bone.markdown.parse. `M.text` and `M.band` are the text nodes the desktop
-- library's chat is made of.
local M = {}

local style = require("bone.nodes.style")

M.text_size = style.size.text
M.small_size = style.size.caption

--- Selectable text of `spans` (`{ text, hl, ... }` or `{ "text", "Group" }`).
--- opts: size, font, hl (the group of the text around the spans), gap (px
--- above) and indent (px on the left).
function M.text(spans, opts)
  opts = opts or {}
  return style.finish({
    spans = spans, size = opts.size, font = opts.font, hl = opts.hl, selectable = true,
    margin = (opts.gap or 0) > 0 and { opts.gap, 0, 0, 0 } or nil,
    padding = (opts.indent or 0) > 0 and { 0, 0, 0, opts.indent } or nil,
  })
end

--- Text on a band across the width (a code block, a row of a diff): opts as
--- M.text's, and bg (default CodeBlock), pad (px around the text, default
--- 8), radius (default 6). The font is monospace unless opts.font says.
function M.band(spans, opts)
  opts = opts or {}
  local pad = opts.pad or 8
  return style.finish({
    bg = opts.bg or "CodeBlock", radius = opts.radius or style.radius.sm,
    padding = { pad, pad, pad, pad + (opts.indent or 0) },
    margin = (opts.gap or 0) > 0 and { opts.gap, 0, 0, 0 } or nil,
    M.text(spans, { size = opts.size, font = opts.font or "mono" }),
  })
end

-- Markdown spans as node spans: their group, and their flags kept.
function M.spans(list, group)
  local out = {}
  for _, s in ipairs(list or {}) do
    local hl = group
    if s.link then
      hl = "MdLink"
    elseif s.code then
      hl = "MdCode"
    elseif s.bold then
      hl = "MdBold"
    elseif s.italic then
      hl = "MdItalic"
    end
    out[#out + 1] = { text = s.text, hl = hl, bold = s.bold, italic = s.italic, code = s.code, link = s.link }
  end
  return out
end

-- `text` as paragraphs: { spans, size, font, gap, indent, band }.
local function paragraphs(text, group)
  group = group or "Normal"
  local out, gap = {}, 0
  local function add(p)
    p.gap = (p.gap or 0) + gap
    gap = 0
    out[#out + 1] = p
  end
  local last
  for _, b in ipairs(bone.markdown.parse(text)) do
    local k = b.kind
    if k == "paragraph" and last == "paragraph" and #out > 0 then
      local prev = out[#out].spans
      prev[#prev + 1] = { text = " ", hl = group }
      for _, s in ipairs(M.spans(b.spans, group)) do prev[#prev + 1] = s end
    elseif k == "paragraph" then
      add({ spans = M.spans(b.spans, group), gap = #out > 0 and 4 or 0 })
    elseif k == "heading" then
      local size = ({ 22, 19, 17 })[b.level] or M.text_size + 1
      add({ spans = M.spans(b.spans, "MdHeading"), size = size, gap = #out > 0 and 10 or 0 })
    elseif k == "item" then
      local marker = b.ordered and b.marker or "•"
      local item = { { text = marker .. "  ", hl = "MdBullet" } }
      for _, s in ipairs(M.spans(b.spans, group)) do item[#item + 1] = s end
      add({ spans = item, indent = 10 + 16 * math.floor((b.indent or 0) / 2), gap = 2 })
    elseif k == "quote" then
      add({ spans = M.spans(b.spans, "MdQuote"), indent = 14, gap = 4 })
    elseif k == "code" then
      local body = table.concat(b.lines or {}, "\n")
      add({ spans = { { text = body, hl = "MdCodeBlock" } }, size = M.small_size, band = true, gap = 6 })
    elseif k == "table" then
      -- One padded line wraps in the pane and piles the next cell onto it.
      -- Each cell is its own text, so a long note wraps inside its column.
      local col = { direction = "column", gap = 8 }
      for i, row in ipairs(b.rows or {}) do
        local head = b.header and i == 1
        local line = { direction = "row", gap = 16, align = "start" }
        for j, cell in ipairs(row) do
          local spans = M.spans(cell, head and "MdTableHeader" or "MdTable")
          for _, s in ipairs(spans) do s.bold = s.bold or head end
          local node = {
            spans = #spans > 0 and spans or { { text = "", hl = "MdTable" } },
            size = M.small_size, selectable = true,
          }
          if j == #row and #row > 1 then node.grow = 1
          elseif j == 1 then node.width = 168
          else node.width = 260 end
          line[#line + 1] = node
        end
        col[#col + 1] = line
      end
      add({ node = col, gap = 6 })
    elseif k == "rule" then
      add({ spans = { { text = "———", hl = "Dim" } }, gap = 6 })
    elseif k == "blank" then
      gap = gap + 6
    end
    last = k
  end
  return out
end

--- `text` as a list of nodes, a paragraph each; `group` is the body text's
--- (default Normal). opts: size (every paragraph's, headings and code too),
--- indent (px more on the left).
function M.nodes(text, group, opts)
  opts = opts or {}
  local out = {}
  for _, p in ipairs(paragraphs(text, group)) do
    if p.node then
      if (p.gap or 0) > 0 then p.node.margin = { p.gap, 0, 0, 0 } end
      out[#out + 1] = style.finish(p.node)
    else
      local o = { size = opts.size or p.size, font = p.font, gap = p.gap,
        indent = (p.indent or 0) + (opts.indent or 0) }
      out[#out + 1] = p.band and M.band(p.spans, o) or M.text(p.spans, o)
    end
  end
  return out
end

--- The spans of node `n` (a text node, or a band's text), to add to.
function M.spans_of(n)
  return n.spans or (n[1] and n[1].spans)
end

return M
