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

-- A table's columns, sized from their cells as a browser sizes an auto
-- table: each column starts as wide as its longest word (so a path is not
-- broken mid-name), then takes the room left in proportion to how much
-- more its longest cell wants, up to that cell's width. Lua cannot measure
-- text, so widths are estimated from the font size: monospace for code.
-- Per column: { word, full } in pixels.
local function table_widths(rows, size)
  local function px(text, code, bold)
    -- A code span is drawn at 90% of the text's size (crates/bone-gui node.rs).
    return #text * size * (code and 0.6 * 0.9 or bold and 0.56 or 0.52)
  end
  local widths = {}
  for i, row in ipairs(rows) do
    for j, cell in ipairs(row) do
      local w = widths[j] or { word = 24, full = 24 }
      widths[j] = w
      local full = 0
      for _, s in ipairs(cell) do
        local text = s.text or ""
        local bold = s.bold or i == 1
        full = full + px(text, s.code, bold)
        -- A code span's chip keeps its padding; its words still wrap at spaces.
        for word in text:gmatch("%S+") do
          w.word = math.max(w.word, px(word, s.code, bold) + (s.code and 8 or 0))
        end
      end
      w.full = math.max(w.full, full + 8)
    end
  end
  for _, w in pairs(widths) do
    w.word = math.ceil(w.word * 1.08)
    w.full = math.max(w.word, math.ceil(math.min(w.full * 1.08, 480)))
  end
  return widths
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
      -- Nearer what it heads than what is above it.
      local size = ({ 22, 19, 17 })[b.level] or M.text_size + 1
      add({ spans = M.spans(b.spans, "MdHeading"), size = size, gap = #out > 0 and 18 or 0 })
    elseif k == "item" then
      -- The marker in a column of its own, so a wrapped line hangs under
      -- the item's text rather than under the marker.
      local marker = b.ordered and b.marker or "•"
      local indent = 10 + 16 * math.floor((b.indent or 0) / 2)
      -- A zero-width space at the text's size keeps the first line as tall
      -- as the marker's when the item is only (smaller) code.
      local body = { { text = "\u{200B}", hl = group } }
      for _, s in ipairs(M.spans(b.spans, group)) do body[#body + 1] = s end
      add({ gap = 2, node = {
        direction = "row", align = "start", padding = { 0, 0, 0, indent },
        { text = marker, hl = "MdBullet", size = M.text_size, width = b.ordered and (#marker * 9 + 6) or 14,
          shrink = 0, wrap = false },
        style.finish({ spans = body, size = M.text_size, selectable = true, grow = 1, width = 0 }),
      } })
    elseif k == "quote" then
      add({ spans = M.spans(b.spans, "MdQuote"), indent = 14, gap = 4 })
    elseif k == "code" then
      local body = table.concat(b.lines or {}, "\n")
      add({ spans = { { text = body, hl = "MdCodeBlock" } }, size = M.small_size, band = true, gap = 6 })
    elseif k == "table" then
      -- One padded line wraps in the pane and piles the next cell onto it.
      -- Each cell is its own text, so a long note wraps inside its column.
      -- Framed: a hairline round it and between its rows, the header row
      -- on the raised surface in quiet grey.
      local col = style.finish({ direction = "column", border = 1, border_color = "WinSeparator",
        radius = style.radius.md, overflow = "hidden" })
      local widths = table_widths(b.rows or {}, M.small_size)
      -- A column of numbers is set flush right, so its digits line up.
      local numeric = {}
      for j in pairs(widths) do
        local any, all = false, true
        for i, row in ipairs(b.rows or {}) do
          if not (b.header and i == 1) and row[j] then
            local t = ""
            for _, sp in ipairs(row[j]) do t = t .. (sp.text or "") end
            t = t:gsub("^%s+", ""):gsub("%s+$", "")
            if t ~= "" and t ~= "—" and t ~= "-" then
              any = true
              all = all and t:match("^[~≈<>%+%-]?[%d][%d.,]*%s*[%%kKmMbB]?$") ~= nil
            end
          end
        end
        numeric[j] = any and all
      end
      local count = #(b.rows or {})
      for i, row in ipairs(b.rows or {}) do
        local head = b.header and i == 1
        local line = style.finish({ direction = "row", gap = 16, align = "start", padding = { 7, 12 },
          bg = head and "InputBackground" or nil,
          border = i < count and { 0, 0, 1, 0 } or nil, border_color = i < count and "WinSeparator" or nil })
        for j, cell in ipairs(row) do
          local spans = M.spans(cell, head and "Dim" or "MdTable")
          if head then
            for _, s in ipairs(spans) do s.medium, s.hl = true, s.code and s.hl or "Dim" end
          end
          -- At the text's size, so a cell of (smaller) code lines up with its row.
          table.insert(spans, 1, { text = "\u{200B}", hl = "MdTable" })
          local node = {
            spans = spans,
            size = M.small_size, selectable = true,
          }
          -- Every row's cell j gets the same box, so the columns line up.
          local w = widths[j]
          node.width, node.grow, node.max_width, node.shrink = w.word, w.full - w.word, w.full, 1
          if numeric[j] then node.text_align = "right" end
          line[#line + 1] = node
        end
        col[#col + 1] = line
      end
      -- Wider than the chat (a narrow window), the table scrolls sideways
      -- rather than breaking its words; the columns never go under their
      -- longest word.
      local least = 0
      for j, w in pairs(widths) do least = least + w.word + (j > 1 and 16 or 0) end
      least = least + 2 * 12 + 2
      col.grow, col.width, col.min_width = 1, 0, least
      add({ node = { direction = "row", overflow = "scroll", col }, gap = 6 })
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
