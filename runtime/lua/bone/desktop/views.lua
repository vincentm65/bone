-- The desktop library's chat, every kind of item drawn for the GUI: your
-- messages in rounded bubbles on the right, answers as full-width
-- proportional text (Markdown laid out by the window), reasoning, notices
-- and queued messages as smaller text, tool calls as cards
-- (desktop/tools.lua), and each turn's footer. Turns before the last fold
-- into one row each (below). Each view returns a node;
-- `lines` are a plain fallback for a frontend without nodes. Replace any
-- of them as bone.ui.views.<kind>.
local M = {}

local md = require("bone.desktop.markdown")
local nodes = require("bone.nodes")
local timing = require("bone.timing")
local style = require("bone.nodes.style")
local strip = require("bone.nodes.toolstrip")

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
    return style.finish(node)
  end
  return style.finish({
    direction = "row", justify = o.right and "end" or "start", align = "start", margin = margin,
    style.finish(node),
  })
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

-- ---- folded turns ----------------------------------------------------------
-- Every finished turn before the last is one row: your message, the turn's
-- tool strip, how long it took; its answer and calls are hidden. A click on
-- the row opens the turn, a click on a segment opens it at that call. An
-- open turn has its strip under your message, with "Fold" while it is not
-- the last. `bone.o.fold_turns = false` keeps every turn open.

local open = {} -- "session:turn" -> true, for folded turns opened by hand

if not pcall(bone.o.info, "fold_turns") or not bone.o.info("fold_turns") then
  bone.o.define("fold_turns", true, {
    desc = "fold each finished turn but the last into one row",
    on_change = function() bone.chat.redraw() end,
  })
end

local function turn_key(n)
  local s = bone.chat.session()
  return (s and s.session_id or "") .. ":" .. tostring(n)
end

-- Turn `n` of the chat being drawn, and whether it is the last. Which turn
-- is last does not make a view depend on the rest of the chat: a turn
-- starting or finishing draws them all again (below).
local function turn_of(n)
  local s = bone.chat.session()
  return strip.turn(s, n), s and n == s.turns
end

-- Whether `item` belongs to a folded turn: any but the last, except the one
-- just before a running turn when a steer cut it short (it ends in a call,
-- not an answer, and is still going).
local function folded(item)
  if not bone.o.fold_turns or not item.turn or item.turn == 0 then return false end
  local s = bone.chat.session()
  if not s or item.turn >= (s.turns or 0) then return false end
  if s.running and item.turn == s.turns - 1 then
    local next = bone.chat.items({ kind = "user", turn = s.turns })[1]
    local tail = next and bone.chat.item(next.index - 1)
    if tail and tail.kind ~= "assistant" and tail.kind ~= "notice" then return false end
  end
  return not open[turn_key(item.turn)]
end

--- Open (`on`) or fold turn `n` of the chat on screen.
function M.open_turn(n, on)
  open[turn_key(n)] = on or nil
  bone.chat.redraw()
end

-- Open turn `n` at the item a segment stands for.
local function open_at(n)
  return function(seg)
    M.open_turn(n, true)
    -- After the opened turn has been laid out.
    bone.defer(60, function() bone.chat.scroll_to(seg.index, "center") end)
  end
end

local function duration(total)
  return {
    width = 52, text = total and strip.secs(total) or "", hl = "Dim", font = "mono",
    size = style.size.micro, wrap = false, text_align = "right",
  }
end

-- A folded turn's row, for its user message `item`.
local function fold_row(item)
  local bar, total = strip.bar(bone.chat.session(), turn_of(item.turn), open_at(item.turn),
    "turn:" .. item.turn, function() bone.chat.redraw(item.index) end)
  if not bone.o.tool_strip then bar = nil end
  local text = ((item.text or ""):match("^%s*([^\n]*)") or ""):gsub("%s+", " ")
  if text == "" and item.images and #item.images > 0 then text = "[Image]" end
  return {
    node = style.finish({
      direction = "row", align = "center", gap = 14, padding = { 6, 10 }, margin = { 1, 0, 1, 0 },
      radius = style.radius.md, hover = { bg = style.hover() }, transition = style.motion.base,
      cursor = "pointer", role = "button", label = "Open: " .. text,
      on_click = function() M.open_turn(item.turn, true) end,
      { width = "34%", text = text, hl = "Dim", size = style.size.caption, wrap = false, text_overflow = "ellipsis" },
      bar or { grow = 1 },
      duration(total),
    }),
    lines = { { { text, "Dim" } } },
  }
end

-- An open turn's strip, under its user message `item`; nil when there is
-- nothing to draw.
local function turn_header(item)
  local turn, last = turn_of(item.turn)
  if not turn or not bone.o.tool_strip then return nil end
  if turn.running then bone.chat.refresh_in(1000) end -- the time grows
  local bar, total = strip.bar(bone.chat.session(), turn, nil,
    "turn:" .. item.turn, function() bone.chat.redraw(item.index) end)
  -- A plain answer with no time to show needs no strip of its own.
  if not bar or ((turn.tools or 0) == 0 and not total) then return nil end
  local fold = bone.o.fold_turns and not last and style.finish({
    text = "Fold", hl = "Dim", size = style.size.small, wrap = false, cursor = "pointer", role = "button",
    padding = { 2, 6 }, radius = style.radius.sm, hover = { bg = style.hover() },
    on_click = function() M.open_turn(item.turn, false) end,
  }) or nil
  return style.finish({
    direction = "row", align = "center", gap = 14, padding = { 0, 10 }, margin = { 2, 0, 6, 0 },
    bar, duration(total), fold,
  })
end

-- The open turn's strip grows with its calls: its message is drawn again.
local function redraw_last(ev)
  local s = bone.chat.session()
  if not s or s.session_id ~= ev.session_id then return end
  local t = strip.turn(s, s.turns)
  if t then bone.chat.redraw(t.first) end
end

-- ---- views -----------------------------------------------------------------

function M.user(item, ctx)
  if folded(item) then return fold_row(item) end
  local out = message(item, "UserBubble")
  images(item, out)
  footer(item, ctx, out)
  local node = box(out, {
    size = md.text_size, right = true, max_width = "78%",
    bg = "UserBubble", radius = style.radius.round, pad = { 10, 14 }, margin = { 16, 6 },
  })
  local header = turn_header(item)
  if header then node = { direction = "column", node, header } end
  return { node = node, lines = plain(item.text, "UserMessage") }
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
    -- A table has no text of its own to end on: the dots go under it.
    local last = md.spans_of(out[#out])
    if last then
      last[#last + 1] = { text = " …", hl = "Dim" }
    else
      out[#out + 1] = md.text({ { "…", "Dim" } })
    end
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
      border = "PromptBorder", radius = style.radius.round, pad = { 8, 14 }, margin = { 8, 4 },
    }),
    lines = plain(item.text, "Dim"),
  }
end

function M.setup()
  strip.setup()
  local views = bone.ui.views
  for _, kind in ipairs({ "user", "assistant", "reasoning", "notice", "compacted", "queued" }) do
    views[kind] = M[kind]
  end
  views.tool = require("bone.desktop.tools").view
  -- A folded turn shows only its row (drawn by its user message).
  for _, kind in ipairs({ "assistant", "reasoning", "notice", "tool" }) do
    local view = views[kind]
    views[kind] = function(item, ctx)
      if folded(item) then return nil end
      return view(item, ctx)
    end
  end
  -- The turn that was last folds once another starts (or, cut short by a
  -- steer, once the one after it finishes).
  for _, ev in ipairs({ "turn/started", "turn/steered", "turn/finished" }) do
    bone.on(ev, function() bone.chat.redraw() end)
  end
  for _, ev in ipairs({ "tool/started", "tool/finished", "ask/requested" }) do
    bone.on(ev, redraw_last)
  end
end

return M
