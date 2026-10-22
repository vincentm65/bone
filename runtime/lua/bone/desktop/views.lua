-- The desktop library's chat, every kind of item drawn for the GUI: your
-- messages in rounded bubbles on the right, answers as full-width
-- proportional text (Markdown laid out by the window), reasoning, notices
-- and queued messages as smaller text, tool calls as cards
-- (desktop/tools.lua), and each turn's footer. Each view returns a GUI
-- block; `lines` are a plain fallback for a frontend without blocks.
-- Replace any of them as bone.ui.views.<kind>.
local M = {}

local md = require("bone.desktop.markdown")
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

-- The turn's footer under its last item, as a paragraph.
local function footer(item, ctx, paras)
  local text = timing.footer(item, ctx)
  if text then
    paras[#paras + 1] = { spans = { { text, "Dim" } }, size = md.small_size, gap = 10 }
  end
  return paras
end

local function images(item, paras)
  for _, image in ipairs(item.images or {}) do
    paras[#paras + 1] = {
      spans = { { ("[Image: %s · %d×%d]"):format(image.name, image.width, image.height), "Dim" } },
      size = md.small_size, gap = 4,
    }
  end
end

function M.user(item, ctx)
  local paras = {}
  for line in ((item.text or "") .. "\n"):gmatch("(.-)\n") do
    paras[#paras + 1] = { spans = { { line, "UserBubble" } } }
  end
  images(item, paras)
  footer(item, ctx, paras)
  return {
    block = {
      paragraphs = paras, size = md.text_size, align = "right", max_width = 0.78,
      bg = "UserBubble", radius = 16, pad = { 10, 14 }, margin = { 16, 6 },
    },
    lines = plain(item.text, "UserMessage"),
  }
end

function M.assistant(item, ctx)
  local text = trim(item.text)
  local paras
  if text == "" then
    if not item.streaming then return nil end
    paras = { { spans = { { "…", "Dim" } } } }
  else
    paras = md.paragraphs(text)
  end
  if item.streaming and #paras > 0 then
    local last = paras[#paras].spans
    last[#last + 1] = { text = " …", hl = "Dim" }
  end
  footer(item, ctx, paras)
  return {
    block = { paragraphs = paras, size = md.text_size, pad = { 0, 4 }, margin = { 10, 4 } },
    lines = plain(text),
  }
end

function M.reasoning(item, ctx)
  local text = trim(item.text)
  if not bone.o.show_reasoning or text == "" then return nil end
  local paras = { { spans = { { item.streaming and "Thinking…" or "Reasoning", "Dim" } }, size = md.small_size } }
  for _, p in ipairs(md.paragraphs(text, "Reasoning")) do
    p.size = md.small_size
    paras[#paras + 1] = p
  end
  footer(item, ctx, paras)
  return {
    block = { paragraphs = paras, pad = { 0, 4 }, margin = { 8, 2 } },
    lines = plain(text, "Reasoning"),
  }
end

function M.notice(item, ctx)
  local hl = item.error and "ErrorMsg" or "Notice"
  local paras = { { spans = { { item.text or "", hl } } } }
  footer(item, ctx, paras)
  return {
    block = { paragraphs = paras, size = md.small_size, pad = { 0, 4 }, margin = { 6, 4 } },
    lines = plain(item.text, hl),
  }
end

-- A compaction (/compact): the chat above it stays.
function M.compacted(item)
  return {
    block = {
      paragraphs = { { spans = { { "— ", "Dim" }, { item.text or "", "Dim" } } } },
      size = md.small_size, pad = { 0, 4 }, margin = { 10, 10 },
    },
    lines = plain(item.text, "Dim"),
  }
end

-- A message waiting its turn: a faint bubble, and when it goes.
function M.queued(item)
  local s = bone.chat.session()
  local note = s and s.queue_paused and "paused · /queue resume"
    or item.mode == "steer" and "joins this turn" or "next turn"
  local paras = {}
  for line in ((item.text or "") .. "\n"):gmatch("(.-)\n") do
    paras[#paras + 1] = { spans = { { line, "Dim" } } }
  end
  images(item, paras)
  paras[#paras + 1] = { spans = { { note, "Dim" } }, size = md.small_size - 1, gap = 4 }
  return {
    block = {
      paragraphs = paras, size = md.text_size, align = "right", max_width = 0.78,
      border = "PromptBorder", radius = 16, pad = { 8, 14 }, margin = { 8, 4 },
    },
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
