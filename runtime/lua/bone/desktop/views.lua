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
local widgets = require("bone.nodes.widgets")
local tools = require("bone.desktop.tools")

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

-- Draws `item` again (a copy button's check comes and goes this way).
local function redraw_of(item)
  return function() bone.chat.redraw(item.index) end
end

-- How a turn that did not finish says so in its footer.
local ENDED = { failed = "Failed after ", stopped = "Stopped after " }

-- The turn's footer under its last item: on a reply, a quiet copy button,
-- then how long the turn worked ("Failed after 2s · 1:46 PM" for one that
-- failed, "Stopped after" for one you stopped). o: copy (the text a copy
-- button copies), copy_label, ended ("failed", "stopped"), actions (nodes
-- before the copy button; the row shows with them even when the turn's
-- time is not known).
local function footer(item, ctx, out, o)
  o = o or {}
  local t = timing.finished(item, ctx)
  local actions = o.actions or {}
  if not t and #actions == 0 then return out end
  local line
  if t then
    local text = o.ended and (ENDED[o.ended] .. bone.util.duration(t.secs) .. " · " .. t.at)
      or ("Worked for " .. bone.util.duration(t.secs) .. " · Finished at " .. t.at)
    line = md.text({ { text, "Dim" } }, { size = style.size.small })
    line.margin = nil
  end
  if not o.copy and #actions == 0 then
    line.margin = { 10, 0, 0, 0 }
    out[#out + 1] = line
    return out
  end
  -- A bare icon leads a little left, so its glyph (not its box) is on the
  -- text's edge; a filled button starts on that edge itself.
  local row = { direction = "row", align = "center", gap = 8, margin = { 8, 0, 0, #actions > 0 and 0 or -4 } }
  for _, a in ipairs(actions) do row[#row + 1] = a end
  if o.copy then
    row[#row + 1] = widgets.copy_button(o.copy,
      "copy:" .. tostring(ctx and ctx.list or "") .. ":" .. tostring(item.key or item.id or item.index),
      redraw_of(item), { label = o.copy_label or "Copy the reply", box = 26, size = 14, hl = "Dim", radius = style.radius.md })
  end
  row[#row + 1] = line
  out[#out + 1] = row
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
  return { md.text({ { item.text, group } }, { line_height = md.line_height }) }
end

-- A message from someone other than the user (item.source). One that
-- reports on work the chat started (source "result" from agents, "job"
-- from a background shell job) is not drawn: the call's own line shows how
-- it went. Others (an agent's "From …:"): folded ("summary"), one line, the
-- first line then the start of the rest; else the first line, then the
-- first lines of the rest. A click or ctrl+t shows all of it, as on a tool
-- card.
local PREVIEW = 3
local HIDDEN = { result = true, job = true }
local function report(item, ctx)
  if HIDDEN[item.source] then return nil end
  local head, rest = trim(item.text):match("^([^\n]*)\n?(.*)$")
  local first = (trim(head):gsub(":$", ""))
  local spans = md.spans((bone.markdown.parse(first)[1] or {}).spans or { { text = first } }, "Dim") -- `code` as code
  local all, kept, n = tools.expanded(item), {}, 0
  if bone.o.tool_detail ~= "rows" and bone.o.tool_detail ~= "full" and not all then
    local line = { { "◆ ", "Accent" }, unpack(spans) }
    local gist = trim(rest):match("^[^\n]*") or ""
    if gist ~= "" then
      line[#line + 1] = { " · ", "Dim" }
      for _, s in ipairs(md.spans((bone.markdown.parse(gist)[1] or {}).spans or { { text = gist } })) do
        line[#line + 1] = s
      end
    end
    local text = md.text(line, { size = md.small_size })
    text.wrap, text.text_overflow = false, "ellipsis"
    local out = { text }
    images(item, out)
    footer(item, ctx, out)
    return {
      node = box(out, { pad = { 0, 4 }, margin = { 10, 4 } }),
      lines = plain(item.text, "Dim"),
      click = function() tools.toggle(item) end,
    }
  end
  local out = { md.text({ { "◆ ", "Accent" }, unpack(spans) }, { size = md.small_size }) }
  for line in (trim(rest) .. "\n"):gmatch("(.-)\n") do
    if not line:match("^%s*([`~])%1%1") then n = n + 1 end -- a code fence is no line of its own
    if all or n <= PREVIEW then kept[#kept + 1] = line end
  end
  nodes.append(out, md.nodes(table.concat(kept, "\n"), "ToolOutput",
    { size = md.small_size, indent = 12, redraw = redraw_of(item) }))
  if not all and n > PREVIEW then
    out[#out + 1] = md.text({ { "⋮ +" .. (n - PREVIEW) .. " more", "Dim" } }, { size = md.small_size, indent = 12 })
  end
  images(item, out)
  footer(item, ctx, out)
  return {
    node = box(out, { pad = { 0, 4 }, margin = { 10, 4 } }),
    lines = plain(item.text, "Dim"),
    click = function() tools.toggle(item) end,
  }
end

-- ---- views -----------------------------------------------------------------

function M.user(item, ctx)
  if item.source then return report(item, ctx) end
  local out = message(item, "UserBubble")
  images(item, out)
  footer(item, ctx, out)
  local node = box(out, {
    size = md.text_size, right = true, max_width = "78%",
    bg = "UserBubble", radius = style.radius.round, pad = { 10, 14 }, margin = { 16, 6 },
  })
  return { node = node, lines = plain(item.text, "UserMessage") }
end

function M.assistant(item, ctx)
  local text = trim(item.text)
  local out
  if text == "" then
    if not item.streaming then return nil end
    out = { md.text({ { "…", "Dim" } }) }
  else
    out = md.nodes(text, nil, {
      id = "md:" .. tostring(ctx and ctx.list or "") .. ":" .. tostring(item.key or item.id or item.index),
      width = ctx and ctx.width_px and ctx.width_px - 8,
      redraw = redraw_of(item),
    })
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
  footer(item, ctx, out, { copy = not item.streaming and text ~= "" and text or nil })
  return {
    node = box(out, { size = md.text_size, pad = { 0, 4 }, margin = { 10, 4 } }),
    lines = plain(text),
  }
end

function M.reasoning(item, ctx)
  local text = trim(item.text)
  if not bone.o.show_reasoning or text == "" then return nil end
  local out = { md.text({ { item.streaming and "Thinking…" or "Reasoning", "Dim" } }, { size = md.small_size }) }
  nodes.append(out, md.nodes(text, "Reasoning", { size = md.small_size, redraw = redraw_of(item) }))
  footer(item, ctx, out)
  return {
    node = box(out, { pad = { 0, 4 }, margin = { 8, 2 } }),
    lines = plain(text, "Reasoning"),
  }
end

-- ---- a failed turn -----------------------------------------------------------

-- What a failed turn's error means, in a line: the provider bone could not
-- reach, or the HTTP status it answered with; nil when the message says it
-- plainly enough itself.
local function explain(message, provider)
  local named = type(provider) == "string" and provider ~= ""
  local url = message:match("error sending request for url %((.-)%)")
  if url then
    local host = url:match("^%a[%w+.-]*://([^/]+)") or url
    return "Couldn't reach " .. (named and provider or "the provider") .. " at " .. host
  end
  local status = tonumber(message:match("HTTP (%d%d%d)"))
  if status then
    local why = (status == 401 or status == 403) and "refused the API key"
      or status == 404 and "has no such model or address"
      or status == 429 and "hit its rate limit"
      or status == 400 and "rejected the request"
      or status >= 500 and "had an error of its own"
      or "answered with an error"
    return (named and provider or "The provider") .. " " .. why .. " (HTTP " .. status .. ")"
  end
  if message:lower():find("timed out", 1, true) then
    return (named and provider or "The provider") .. " took too long to answer"
  end
  return nil
end

-- The longest error shown in full; a copy takes all of it.
local DETAIL_MAX = 400

-- The error of the chat on screen that offers Retry: { session, index }.
local offered = nil

-- Retry: the failed turn's message, sent again (the core keeps the failed
-- one in the history, as there is no going back on a turn).
local function retry(item)
  local s = bone.chat.session()
  local data = bone.chat.item(item.index)
  local user = data and bone.chat.items({ turn = data.turn, kind = "user", first = 1 })[1]
  if not (s and user) then return end
  local params = { session_id = s.session_id, text = user.text or "" }
  if user.images and #user.images > 0 then params.images = user.images end
  bone.request("queue/add", params, function(_, err)
    if err then bone.notify(tostring(err), "error") end
    bone.chat.redraw(item.index)
  end)
end

-- Whether `item` (an error) can send its turn again: it is the last thing
-- in the chat, nothing runs, and you sent the message it failed on.
local function can_retry(item)
  local s = bone.chat.session()
  if not s or s.running then return false end
  local last = bone.chat.items({ last = 1 })[1]
  if not last or last.index ~= item.index then return false end
  local data = bone.chat.item(item.index)
  local user = data and bone.chat.items({ turn = data.turn, kind = "user", first = 1 })[1]
  return user ~= nil and not user.source
end

-- A turn that failed: what went wrong in a line, the error itself under it
-- in grey (cut short; the copy button takes all of it), then Retry and
-- when it failed.
local function failed(item, ctx)
  local message = trim((item.text or ""):gsub("^error:%s*", ""))
  local s = bone.chat.session()
  local provider = s and require("bone.usage").of(s.session_id).provider
  local head = explain(message, provider)
  local first, rest = message:match("^([^\n]*)\n?(.*)$")
  local out = { md.text({ { head or first, "ErrorMsg" } }) }
  -- Under a line of ours, the provider's own words when its answer carries
  -- them ({"error":{"message":"…"}}), else the error as it came.
  local said = message:match('"message"%s*:%s*"([^"]+)"')
  local detail = head and (said or message) or trim(rest)
  if detail ~= "" then
    if #detail > DETAIL_MAX then
      detail = detail:sub(1, DETAIL_MAX):gsub("[\192-\255][\128-\191]*$", "") .. "…"
    end
    out[#out + 1] = md.text({ { detail, "Dim" } }, { size = style.size.small, gap = 2 })
  end
  local actions = {}
  if can_retry(item) then
    offered = { session = s.session_id, index = item.index }
    actions[1] = widgets.button("Retry", function() retry(item) end, false, false,
      { size = style.size.small, padding = { 3, 10 }, radius = style.radius.sm,
        label = "Send the message again", tooltip = "Send the message again" })
  end
  footer(item, ctx, out, { actions = actions, copy = message, copy_label = "Copy the error", ended = "failed" })
  return {
    node = box(out, { size = md.small_size, pad = { 0, 4 }, margin = { 6, 4 } }),
    lines = plain(item.text, "ErrorMsg"),
  }
end

-- A turn starting puts away the Retry of the error before it.
bone.on("turn/started", function(ev)
  local s = bone.chat.session()
  if offered and offered.session == ev.session_id and s and s.session_id == ev.session_id then
    bone.chat.redraw(offered.index)
  end
  if offered and offered.session == ev.session_id then offered = nil end
end)

function M.notice(item, ctx)
  if item.error then return failed(item, ctx) end
  local hl = "Notice"
  local out = {}
  if item.text == "cancelled" then
    -- You stopped it: its footer says so ("Stopped after 4s · 2:52 PM"),
    -- or a quiet "Stopped" where the turn's time is not known.
    footer(item, ctx, out, { ended = "stopped" })
    if #out == 0 then out[1] = md.text({ { "Stopped", "Dim" } }) end
    out[1].margin = nil
  else
    out[1] = md.text({ { item.text or "", hl } })
    footer(item, ctx, out)
  end
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

-- Tool strips and folded turns are the toolstrip plugin's: it wraps these.
function M.setup()
  local views = bone.ui.views
  for _, kind in ipairs({ "user", "assistant", "reasoning", "notice", "compacted", "queued" }) do
    views[kind] = M[kind]
  end
  views.tool = tools.view
end

return M
