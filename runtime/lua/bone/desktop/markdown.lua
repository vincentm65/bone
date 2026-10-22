-- Markdown as GUI block paragraphs (see the GUI's `block`): headings,
-- paragraphs (their lines joined; the window wraps them), list items,
-- quotes, code blocks, tables and rules, from bone.markdown.parse.
local M = {}

M.text_size = 15
M.small_size = 13

-- Markdown spans as block spans: their group, and their flags kept.
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

--- `text` as paragraphs; `group` is the body text's (default Normal).
function M.paragraphs(text, group)
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
      add({ spans = { { text = body, hl = "MdCodeBlock" } }, font = "mono", size = M.small_size,
        bg = "CodeBlock", pad = 8, gap = 6 })
    elseif k == "table" then
      for i, row in ipairs(b.rows or {}) do
        local cells = {}
        for j, cell in ipairs(row) do
          if j > 1 then cells[#cells + 1] = { text = "   │   ", hl = "Dim" } end
          local hl = (b.header and i == 1) and "MdTableHeader" or "MdTable"
          for _, s in ipairs(M.spans(cell, hl)) do
            s.bold = s.bold or (b.header and i == 1)
            cells[#cells + 1] = s
          end
        end
        add({ spans = cells, size = M.small_size, gap = i == 1 and 4 or 2 })
      end
    elseif k == "rule" then
      add({ spans = { { text = "⎯⎯⎯⎯⎯⎯⎯⎯", hl = "Dim" } }, gap = 6 })
    elseif k == "blank" then
      gap = gap + 6
    end
    last = k
  end
  return out
end

return M
