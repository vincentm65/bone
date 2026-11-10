-- The status bar under the composer. The model, what a running turn is
-- doing and the context are in the composer's toolbar now, so the bar is
-- only drawn for what is put in it: `bone.ui.statusline` when that is a
-- function (its spans), and the slots `statusbar.left` and
-- `statusbar.right`. Nothing to show is no bar. The TUI default is not
-- loaded.
local M = {}

local nodes = require("bone.nodes")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")

local function slot(name, ctx) return bone.gui.slots.nodes(name, ctx) end

-- When each running turn started, by session.
local started = {}

--- The seconds the turn running in `session` has taken, nil when none is
--- (or the chat has no id yet). Whole seconds: it asks for the next one's
--- frame, and the spinner animates on its own canvas between them.
function M.elapsed(session)
  local id = session and session.session_id
  if not (session and session.running) then
    if id then started[id] = nil end
    return nil
  end
  bone.gui.invalidate(1000)
  if not id then return nil end
  started[id] = started[id] or os.time()
  return os.time() - started[id]
end

-- A statusline failure already shown, so a later rebuild does not notify again.
local reported_error

-- One statusline span: a string or `{ text, hl }` (`{ "text", "hl" }` too).
-- `"%="` is a flexible spacer, not text.
local function span_node(span)
  if span == "%=" then return { grow = 1 } end
  local text, hl
  if type(span) == "table" then
    text = span.text
    if text == nil then text = span[1] end
    hl = span.hl
    if hl == nil then hl = span[2] end
  else
    text = span
  end
  if type(text) ~= "string" then text = text == nil and "" or tostring(text) end
  if type(hl) ~= "string" then hl = "StatusLine" end
  return { text = text, hl = hl, size = style.size.small, wrap = false, text_overflow = "ellipsis" }
end

-- Spans from `bone.ui.statusline` when that is already a function. Nil means
-- it is not set, or it failed: this build keeps the built-in text.
local function statusline_spans(session, elapsed)
  local fn = bone.ui and bone.ui.statusline
  if type(fn) ~= "function" then return nil end
  local ok, result = pcall(fn, {
    title = session and session.title or nil,
    -- Character cells. This bar is not given a pixel width.
    width = 80,
    spinner = "",
    session = session and {
      title = session.title,
      cwd = session.cwd,
      running = session.running,
      elapsed = elapsed,
      session_id = session.session_id,
    } or nil,
  })
  if not ok then
    local err = tostring(result)
    if reported_error ~= err then
      reported_error = err
      bone.notify(tostring(result), "error")
    end
    return nil
  end
  reported_error = nil
  if result == nil then return {} end
  if type(result) ~= "table" then return { result } end
  return result
end

--- The bar for `session`; `cell` is the monospace cell, which the spinner
--- is as tall as.
function M.node(session, cell)
  -- A narrow window has no room for the whole bar: the pieces that do not
  -- fit go on a line of their own, and a piece too long for its line is
  -- cut short with an ellipsis. Nothing is painted over anything else.
  local bar = {
    -- Inset like the composer's attach "+" above it, so the two read as one edge.
    direction = "row", flex_wrap = true, padding = { 6, 22, 0, 22 }, gap = 12, align = "center",
  }
  local spans = statusline_spans(session, M.elapsed(session))
  if spans then
    if session and session.running then
      bar[#bar + 1] = widgets.spinner(math.max(20, cell and cell.h or 20))
    end
    for _, span in ipairs(spans) do
      bar[#bar + 1] = span_node(span)
    end
  end
  nodes.append(bar, slot("statusbar.left"))
  local right = slot("statusbar.right")
  if right and #right > 0 then
    bar[#bar + 1] = { grow = 1 }
    nodes.append(bar, right)
  end
  if #bar == 0 then return nil end
  return bar
end

function M.setup()
  -- A turn that ends in a chat that is not on screen leaves no clock behind.
  bone.on("turn/finished", function(ev) started[ev.session_id] = nil end)
end

return M
