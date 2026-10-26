-- The status bar under the composer: the model, what the running turn is
-- doing (the spinner and how long), and the chat's token use. Slots:
-- `statusbar.left` and `statusbar.right`. If `bone.ui.statusline` is already
-- a function, its spans are that text instead. The TUI default is not loaded.
local M = {}

local nodes = require("bone.nodes")
local usage = require("bone.usage")
local agents = require("bone.agents")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local tokens = bone.util.tokens

local function slot(name, ctx) return bone.gui.slots.nodes(name, ctx) end

-- When each running turn started, by session.
local started = {}

-- A statusline failure already shown, so a later rebuild does not notify again.
local reported_error

-- "12k context · 40k used · 80% cached · 3k agents".
local function totals(u, id)
  local out = {}
  if (u.curr or 0) > 0 then out[#out + 1] = tokens(u.curr) .. " context" end
  local total = (u.input or 0) + (u.output or 0)
  if total > 0 then
    out[#out + 1] = tokens(total) .. " used"
    if (u.input or 0) > 0 then
      out[#out + 1] = string.format("%.0f%% cached", (u.cached or 0) / u.input * 100)
    end
  end
  local spent = id and agents.spend(agents.top(id)) or 0
  if spent > 0 then out[#out + 1] = tokens(spent) .. " agents" end
  return table.concat(out, "  ·  ")
end

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
  return { text = text, hl = hl, size = style.size.small, wrap = false }
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
  local id = session and session.session_id
  local u = usage.of(id)
  local bar = { direction = "row", padding = { 6, 16, 0, 16 }, gap = 12, align = "center" }
  local elapsed
  if session and session.running then
    local since = id and (started[id] or os.time())
    if id then started[id] = since end
    -- The "Working · Ns" text is whole seconds; the spinner animates on its
    -- own canvas, so the window is not built again on every animation frame.
    bone.gui.invalidate(1000)
    elapsed = since and (os.time() - since) or nil
  elseif id then
    started[id] = nil
  end
  local spans = statusline_spans(session, elapsed)
  if spans then
    if session and session.running then
      bar[#bar + 1] = widgets.spinner(math.max(20, cell and cell.h or 20))
    end
    for _, span in ipairs(spans) do
      bar[#bar + 1] = span_node(span)
    end
  else
    if session and session.running then
      bar[#bar + 1] = {
        direction = "row", gap = 6, align = "center",
        widgets.spinner(math.max(20, cell and cell.h or 20)),
        { text = "Working" .. (elapsed and (" · " .. bone.util.duration(elapsed)) or ""),
          hl = "StatusLine", size = style.size.small, wrap = false },
      }
    end
    bar[#bar + 1] = { text = u.model or "", hl = "StatusLine", size = style.size.small, wrap = false }
  end
  nodes.append(bar, slot("statusbar.left"))
  bar[#bar + 1] = { grow = 1 }
  nodes.append(bar, slot("statusbar.right"))
  if not spans then
    bar[#bar + 1] = { text = totals(u, id), hl = "StatusLineDim", size = style.size.small, wrap = false }
  end
  return bar
end

function M.setup()
  -- A turn that ends in a chat that is not on screen leaves no clock behind.
  bone.on("turn/finished", function(ev) started[ev.session_id] = nil end)
end

return M
