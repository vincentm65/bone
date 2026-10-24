-- The desktop library's chat, every kind of item drawn for the GUI: your
-- messages in rounded bubbles on the right, answers as full-width
-- proportional text (Markdown laid out by the window), reasoning, notices
-- and queued messages as smaller text, tool calls as cards
-- (desktop/tools.lua), and each turn's footer. Each view returns a node;
-- `lines` are a plain fallback for a frontend without nodes. Replace any
-- of them as bone.ui.views.<kind>.
local M = {}

local md = require("bone.desktop.markdown")
local nodes = require("bone.nodes")
local timing = require("bone.timing")

local function trim(s)
  return ((s or ""):gsub("^%s+", ""):gsub("%s+$", ""))
end

-- Plain lines of `text`, for the fallback.
local function plain(text, group)
  local out = {}
  for line in ((text or "") .. "\n"):gmatch("(.-)\n") do
    out[#out + 1] = { { line, group or "Normal" } }
  end
  return out
end

-- An item's box. Plain text spans the width; a bubble (`bg` or `border`)
-- is as wide as its text, on the left or (`right`) the right. o: size,
-- pad ({ v, h }), margin ({ top, bottom }), bg, border (a color), radius,
-- max_width, right.
local function box(children, o)
  local margin = o.margin and { o.margin[1], 0, o.margin[2], 0 } or nil
  local node = {
    direction = "column", size = o.size, padding = o.pad, bg = o.bg, radius = o.radius,
    border = o.border and 1 or nil, border_color = o.border, max_width = o.max_width,
  }
  for _, c in ipairs(children) do node[#node + 1] = c end
  if not (o.bg or o.border) then
    node.margin = margin
    return node
  end
  return { direction = "row", justify = o.right and "end" or "start", align = "start", margin = margin, node }
end

-- The turn's footer under its last item.
local function footer(item, ctx, out)
  local text = timing.footer(item, ctx)
  if text then
    out[#out + 1] = md.text({ { text, "Dim" } }, { size = md.small_size, gap = 10 })
  end
  return out
end

-- A row for each image; a click opens it large.
local function images(item, out)
  for _, image in ipairs(item.images or {}) do
    local row = md.text({ { ("[Image: %s · %d×%d]"):format(image.name, image.width, image.height), "Dim" } },
      { size = md.small_size, gap = #out > 0 and 4 or 0 })
    row.selectable, row.cursor = nil, "pointer"
    row.on_click = function() bone.gui.preview(image) end
    out[#out + 1] = row
  end
end

-- The message's text, unless it has none (only images).
local function message(item, group)
  if not (item.text or ""):match("%S") then return {} end
  return { md.text({ { item.text, group } }) }
end

function M.user(item, ctx)
  local out = message(item, "UserBubble")
  images(item, out)
  footer(item, ctx, out)
  return {
    node = box(out, {
      size = md.text_size, right = true, max_width = "78%",
      bg = "UserBubble", radius = 16, pad = { 10, 14 }, margin = { 16, 6 },
    }),
    lines = plain(item.text, "UserMessage"),
  }
end

function M.assistant(item, ctx)
  local text = trim(item.text)
  local out
  if text == "" then
    if not item.streaming then return nil end
    out = { md.text({ { "…", "Dim" } }) }
  else
    out = md.nodes(text)
  end
  if item.streaming and #out > 0 then
    local last = md.spans_of(out[#out])
    last[#last + 1] = { text = " …", hl = "Dim" }
  end
  footer(item, ctx, out)
  return {
    node = box(out, { size = md.text_size, pad = { 0, 4 }, margin = { 10, 4 } }),
    lines = plain(text),
  }
end

function M.reasoning(item, ctx)
  local text = trim(item.text)
  if not bone.o.show_reasoning or text == "" then return nil end
  local out = { md.text({ { item.streaming and "Thinking…" or "Reasoning", "Dim" } }, { size = md.small_size }) }
  nodes.append(out, md.nodes(text, "Reasoning", { size = md.small_size }))
  footer(item, ctx, out)
  return {
    node = box(out, { pad = { 0, 4 }, margin = { 8, 2 } }),
    lines = plain(text, "Reasoning"),
  }
end

function M.notice(item, ctx)
  local hl = item.error and "ErrorMsg" or "Notice"
  local out = { md.text({ { item.text or "", hl } }) }
  footer(item, ctx, out)
  return {
    node = box(out, { size = md.small_size, pad = { 0, 4 }, margin = { 6, 4 } }),
    lines = plain(item.text, hl),
  }
end

-- A compaction (/compact): the chat above it stays.
function M.compacted(item)
  return {
    node = box({ md.text({ { "— ", "Dim" }, { item.text or "", "Dim" } }) },
      { size = md.small_size, pad = { 0, 4 }, margin = { 10, 10 } }),
    lines = plain(item.text, "Dim"),
  }
end

-- A message waiting its turn: a faint bubble, and when it goes.
function M.queued(item)
  local s = bone.chat.session()
  local note = s and s.queue_paused and "paused · /queue resume"
    or item.mode == "steer" and "joins this turn" or "next turn"
  local out = message(item, "Dim")
  images(item, out)
  out[#out + 1] = md.text({ { note, "Dim" } }, { size = md.small_size - 1, gap = 4 })
  return {
    node = box(out, {
      size = md.text_size, right = true, max_width = "78%",
      border = "PromptBorder", radius = 16, pad = { 8, 14 }, margin = { 8, 4 },
    }),
    lines = plain(item.text, "Dim"),
  }
end

function M.setup()
  local views = bone.ui.views
  for _, kind in ipairs({ "user", "assistant", "reasoning", "notice", "compacted", "queued" }) do
    views[kind] = M[kind]
  end
  views.tool = require("bone.desktop.tools").view
end

return M
