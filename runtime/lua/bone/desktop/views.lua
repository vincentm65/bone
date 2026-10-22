-- The desktop library's chat: your messages in rounded bubbles on the
-- right, answers as full-width proportional text (Markdown laid out by
-- the GUI), reasoning and notices as smaller text, and the standard tool
-- rows (they stay monospace). Each view returns a GUI block with the
-- standard view's lines beside it, which a frontend without blocks draws.
-- Replace any of them as bone.ui.views.<kind>.
local M = {}

-- Sizes in points.
M.text_size = 15
M.small_size = 13
M.edge = 16

local standard = {}

-- Markdown spans as block spans: their group, and their flags kept.
local function spans(list, group)
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

-- Markdown text as block paragraphs. Lines of one paragraph join (the GUI
-- wraps them); blank lines are space.
function M.markdown(text)
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
      prev[#prev + 1] = { text = " ", hl = "Normal" }
      for _, s in ipairs(spans(b.spans, "Normal")) do prev[#prev + 1] = s end
    elseif k == "paragraph" then
      add({ spans = spans(b.spans, "Normal"), gap = #out > 0 and 4 or 0 })
    elseif k == "heading" then
      local size = ({ 22, 19, 17 })[b.level] or M.text_size + 1
      add({ spans = spans(b.spans, "MdHeading"), size = size, gap = #out > 0 and 10 or 0 })
    elseif k == "item" then
      local marker = b.ordered and b.marker or "•"
      local item = { { text = marker .. "  ", hl = "MdBullet" } }
      for _, s in ipairs(spans(b.spans, "Normal")) do item[#item + 1] = s end
      add({ spans = item, indent = 10 + 16 * math.floor((b.indent or 0) / 2), gap = 2 })
    elseif k == "quote" then
      add({ spans = spans(b.spans, "MdQuote"), indent = 14, gap = 4 })
    elseif k == "code" then
      local body = table.concat(b.lines or {}, "\n")
      add({ spans = { { text = body, hl = "MdCodeBlock" } }, font = "mono", size = M.small_size,
        bg = "CodeBlock", pad = 8, gap = 6 })
    elseif k == "table" then
      for _, row in ipairs(b.rows or {}) do
        local cells = {}
        for i, cell in ipairs(row) do
          if i > 1 then cells[#cells + 1] = { text = "   │   ", hl = "Dim" } end
          for _, s in ipairs(spans(cell, "Normal")) do cells[#cells + 1] = s end
        end
        add({ spans = cells, size = M.small_size, gap = 2 })
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

function M.user(item, ctx)
  local paras = {}
  for line in ((item.text or "") .. "\n"):gmatch("(.-)\n") do
    paras[#paras + 1] = { spans = { { line, "UserBubble" } } }
  end
  for _, image in ipairs(item.images or {}) do
    paras[#paras + 1] = {
      spans = { { ("[Image: %s · %d×%d]"):format(image.name, image.width, image.height), "Dim" } },
      size = M.small_size, gap = 4,
    }
  end
  return {
    block = {
      paragraphs = paras, size = M.text_size, align = "right", max_width = 0.78,
      bg = "UserBubble", radius = 16, pad = { 10, 14 }, margin = { 14, 6 },
    },
    lines = standard.user(item, ctx),
  }
end

function M.assistant(item, ctx)
  local text = (item.text or ""):gsub("^%s+", ""):gsub("%s+$", "")
  local paras
  if text == "" then
    if not item.streaming then return standard.assistant(item, ctx) end
    paras = { { spans = { { "…", "Dim" } } } }
  else
    paras = M.markdown(text)
  end
  if item.streaming and #paras > 0 then
    local last = paras[#paras].spans
    last[#last + 1] = { text = " ▍", hl = "Dim" }
  end
  return {
    block = { paragraphs = paras, size = M.text_size, pad = { 0, 4 }, margin = { 8, 4 } },
    lines = standard.assistant(item, ctx),
  }
end

function M.reasoning(item, ctx)
  local lines = standard.reasoning(item, ctx)
  if not lines then return nil end
  local text = (item.text or ""):gsub("^%s+", ""):gsub("%s+$", "")
  local paras = { { spans = { { item.streaming and "Thinking" or "Reasoning", "Dim" } }, size = M.small_size } }
  for _, p in ipairs(M.markdown(text)) do
    p.size, p.fg = M.small_size, "Reasoning"
    for _, s in ipairs(p.spans) do
      if s.hl == "Normal" then s.hl = "Reasoning" end
    end
    paras[#paras + 1] = p
  end
  return {
    block = { paragraphs = paras, pad = { 0, 4 }, margin = { 6, 2 } },
    lines = lines,
  }
end

function M.notice(item, ctx)
  local hl = item.error and "ErrorMsg" or ((item.text or ""):match("^Worked for") and "Dim" or "Notice")
  return {
    block = {
      paragraphs = { { spans = { { item.text or "", hl } } } },
      size = M.small_size, pad = { 0, 4 }, margin = { 4, 4 },
    },
    lines = standard.notice(item, ctx),
  }
end

function M.setup()
  local views = bone.ui.views
  for _, kind in ipairs({ "user", "assistant", "reasoning", "notice" }) do
    standard[kind] = views[kind]
    views[kind] = M[kind]
  end
end

return M
