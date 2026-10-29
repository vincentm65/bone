-- A turn's tool strip, drawn by the chat (bone.desktop.views) under your
-- message or in a folded turn's row: the turn as one bar, a segment per
-- tool call as wide as it ran, the model's thinking between them as fine
-- dots, and its answer as the bright segment at the end. Reads are faint,
-- commands brighter, edits green, failures red, a sub-agent an outline with
-- a dot; a running call breathes amber, one waiting on an answer is
-- outlined in blue. A segment's tooltip says what it did; a click scrolls
-- the chat to it. `bone.o.tool_strip = false` hides it.
local M = {}

local style = require("bone.nodes.style")
local tools = require("bone.desktop.tools")

M.height = 8 -- the bar, in pixels
M.min_gap = 300 -- model time shorter than this (ms) draws no segment

-- The latest turn's clock, by session: { started, finished }.
local clocks = {}
-- Questions the core is waiting on: ask id -> session id.
local asks = {}

if not pcall(bone.o.info, "tool_strip") or not bone.o.info("tool_strip") then
  bone.o.define("tool_strip", true, {
    desc = "show each turn's tool calls as a timeline",
    on_change = function() bone.chat.redraw() end,
  })
end

local KINDS = {
  read_file = "read", grep = "read", rg = "read", glob = "read", list_files = "read",
  web_search = "read", web_fetch = "read", shell = "shell",
  edit_file = "edit", write_file = "edit", subagent = "agent",
}

local function waiting(id)
  for _, s in pairs(asks) do
    if s == id then return true end
  end
  return false
end

local function exit_code(call)
  return tonumber(tostring(call.output or ""):match("%[exit code: (%-?%d+)%]%s*$"))
end

local function kind(call, id)
  if not call.done then return waiting(id) and "wait" or "live" end
  local code = call.name == "shell" and exit_code(call)
  if call.is_error or (code and code ~= 0) then return "fail" end
  return KINDS[call.name] or "read"
end

local function secs(ms)
  ms = math.max(0, ms)
  if ms < 1000 then return string.format("%dms", math.floor(ms)) end
  if ms < 10000 then return string.format("%.1fs", ms / 1000) end
  return bone.util.duration(ms / 1000)
end

local function describe(call, ms)
  local what = tools.target(call, 48)
  local out = { (tools.labels[call.name] or call.name) .. (what ~= "" and (" " .. what) or "") }
  if ms then
    local gen = call.generation_ms or 0
    out[#out + 1] = secs(ms) .. (gen >= 100 and (" (" .. secs(gen) .. " writing it)") or "")
  end
  local code = call.done and call.name == "shell" and exit_code(call)
  if code and code ~= 0 then out[#out + 1] = "exit " .. code end
  if call.done and call.is_error then out[#out + 1] = "failed" end
  if not call.done then out[#out + 1] = waiting(call.session) and "waiting for you" or "running" end
  return table.concat(out, "  ·  ")
end

-- The segments of `turn` (an entry of bone.chat.turns()) in chat `s`, and
-- how long it took (nil when nothing carries a time). A turn without tool
-- calls is one stretch of model time.
local function segments(s, turn)
  if not turn or not turn.first or not turn.last then return nil end
  local calls = (turn.tools or 0) > 0
    and bone.chat.items({ kind = "tool", from = turn.first, to = turn.last, session = s.session_id, full = true }) or {}
  local id, now = s.session_id, bone.now()
  local clock = (clocks[id] or {})[turn.index] or {}
  local running = s.running and turn.running
  if #calls == 0 then
    local from = running and s.turn and s.turn.elapsed_ms and now - s.turn.elapsed_ms or clock.started
    local stop = running and now or clock.finished
    local k, label = running and "model" or "answer", running and "Thinking" or "Answer"
    if from and stop then
      return { { kind = k, ms = stop - from, index = turn.first, tip = label .. "  ·  " .. secs(stop - from) } },
        stop - from
    end
    return { { kind = k, ms = 1, index = turn.first, tip = label } }, nil
  end
  local timed = true
  for _, c in ipairs(calls) do
    c.session = id
    -- A call still waiting its turn has no start yet; it begins where we are.
    timed = timed and (c.started_at ~= nil or not c.done)
  end
  local segs = {}
  if not timed then
    -- Times only come with live events: an old chat's calls get equal shares.
    for _, c in ipairs(calls) do
      segs[#segs + 1] = { kind = kind(c, id), ms = 1, index = c.index, tip = describe(c, c.duration_ms) }
    end
    return segs, clock.started and clock.finished and (clock.finished - clock.started) or nil
  end
  -- A call's segment begins when the model began writing it (its reply's
  -- first call carries that, `generation_ms`); its reasoning stays dots.
  local function began(c) return c.started_at and (c.started_at - (c.generation_ms or 0)) end
  local first = began(calls[1]) or now
  local start = clock.started
  if running and s.turn and s.turn.elapsed_ms then start = now - s.turn.elapsed_ms end
  -- A clock from another turn (or none): the turn starts with its first call.
  if not start or start > first then start = first end
  local at = start
  local function gap(to, index, answer)
    if to - at >= M.min_gap then
      segs[#segs + 1] = answer
        and { kind = "answer", ms = to - at, index = index, tip = "Answer  ·  " .. secs(to - at) }
        or { kind = "model", ms = to - at, index = index, tip = "Thinking  ·  " .. secs(to - at) }
    end
  end
  for _, c in ipairs(calls) do
    local from = began(c) or at
    gap(from, math.max(turn.first, c.index - 1))
    local ran = c.duration_ms or (c.started_at and not c.done and math.max(0, now - c.started_at)) or 0
    local ms = (c.generation_ms or 0) + ran
    segs[#segs + 1] = { kind = kind(c, id), ms = ms, index = c.index, tip = describe(c, c.started_at and ms or nil) }
    at = math.max(at, from + ms)
  end
  local stop = at
  if running then
    stop = now
  elseif clock.finished and clock.finished >= at then
    stop = clock.finished
  end
  -- After the last call, a finished turn was writing its answer.
  gap(stop, turn.last, not running)
  return segs, stop - start
end

-- Each kind's color is a highlight group of its own (`ToolStripRead`, …),
-- the TUI's toolstrip plugin's names too. Defaults are set only where the
-- colorscheme or config has not: `bone.hl.get` resolves any name, so ask
-- `bone.hl.names` what is defined.
-- Quiet by brightness, as the design it comes from: reads faint, commands
-- and sub-agents brighter, the answer nearly white; only edits, failures,
-- a running call and one waiting on you have color. Solid, so a terminal
-- draws them the same.
M.colors = {
  ToolStripRead = "#3b3b3a", ToolStripCommand = "#6b6a68", ToolStripEdit = "#60ae81",
  ToolStripFail = "#db7369", ToolStripAgent = "#6b6a68", ToolStripLive = "#ffb46a",
  ToolStripWait = "#7cc4ff", ToolStripAnswer = "#cfcdc9",
}
do
  local defined = {}
  for _, name in ipairs(bone.hl.names()) do defined[name] = true end
  for name, fg in pairs(M.colors) do
    if not defined[name] then bone.hl.set(name, { fg = fg }) end
  end
end

local function ink(aa, fallback) return style.alpha("Normal", aa, "fg", fallback or "Dim") end
local function hue(group, aa) return style.alpha(group, aa or "ff", "fg", group) end

local function look(k)
  if k == "read" then return { bg = hue("ToolStripRead") } end
  if k == "shell" then return { bg = hue("ToolStripCommand") } end
  if k == "answer" then return { bg = hue("ToolStripAnswer") } end
  if k == "edit" then return { bg = hue("ToolStripEdit") } end
  if k == "fail" then return { bg = hue("ToolStripFail") } end
  if k == "agent" then
    return {
      border = 1, border_color = hue("ToolStripAgent"), direction = "row", align = "center", justify = "center",
      { width = 4, height = 4, radius = 2, bg = ink("ff", "Normal") },
    }
  end
  return { bg = hue("ToolStripRead") }
end

-- The model thinking between calls: a row of fine dots on a faint band.
local function thinking()
  local band, dot = ink("0d"), ink("66")
  return {
    bg = band,
    canvas = {
      draw = function(ctx)
        local draw, out = bone.gui.draw, {}
        for x = 2, ctx.width - 1, 4 do
          out[#out + 1] = draw.circle({ x = x, y = ctx.height / 2, r = 1, fill = dot })
        end
        return out
      end,
    },
  }
end

-- A running segment (warm, filled) or one waiting on you (cool, outlined):
-- breathing on a canvas of its own, so the window is not built again to
-- animate it, with a soft glow of its color.
local function glowing(group, outlined)
  local fg = (bone.hl.get(group) or {}).fg
  local hex = type(fg) == "string" and fg:match("^#%x%x%x%x%x%x$") and fg or nil
  return {
    shadow = hex and { blur = 10, spread = 1, color = hex .. "8c" } or nil,
    canvas = {
      fps = 30,
      draw = function(ctx)
        local a = 0.55 + 0.45 * (0.5 + 0.5 * math.sin(ctx.time * 4.5))
        local c = hex and (hex .. string.format("%02x", math.floor(255 * a))) or group
        if outlined then
          return { bone.gui.draw.rect({
            x = 0.5, y = 0.5, w = ctx.width - 1, h = ctx.height - 1, radius = 2,
            fill = hex and (hex .. "1f") or nil, stroke = c, width = 1,
          }) }
        end
        return { bone.gui.draw.rect({ x = 0, y = 0, w = ctx.width, h = ctx.height, fill = c, radius = 2 }) }
      end,
    },
  }
end

-- The segment under the pointer: { bar = id, k = n }. While it is set, the
-- rest of that bar dims and a playhead marks it.
M.hot = nil

local function segment(seg, weight, pick, k, bar)
  local node
  if seg.kind == "live" then
    node = glowing("ToolStripLive")
  elseif seg.kind == "wait" then
    node = glowing("ToolStripWait", true)
  elseif seg.kind == "model" then
    node = thinking()
  else
    node = look(seg.kind)
  end
  node.grow = weight
  node.width = 0
  node.min_width = 3
  node.height = node.height or M.height
  node.radius = node.radius or 2
  node.align_self = "center"
  node.tooltip = seg.tip
  node.role = "button"
  node.label = seg.tip
  node.cursor = "pointer"
  node.id = bar.id .. ":" .. k
  local hot = M.hot and M.hot.bar == bar.id
  node.opacity = hot and M.hot.k ~= k and 0.45 or 1
  node.transition = 120
  node.on_hover = function(on)
    -- Leaving the bar, not a segment, ends it: the gaps do not flicker.
    if on and not (M.hot and M.hot.bar == bar.id and M.hot.k == k) then
      M.hot = { bar = bar.id, k = k }
      bar.redraw()
    end
  end
  node.on_click = function()
    if pick then return pick(seg) end
    bone.chat.scroll_to(seg.index, "center")
  end
  return style.finish(node)
end

M.secs = secs

--- Turn `n` of chat `s` (bone.chat.session()) as bone.chat.turns() lists
--- it, or nil. It reads only that turn's messages and calls: in a chat
--- view, bone.chat.turns() depends on every item, so each streamed word
--- would draw every view again.
function M.turn(s, n)
  if not s or not n or n < 1 then return nil end
  local id = s.session_id
  local user = bone.chat.items({ kind = "user", turn = n, session = id })[1]
  if not user then return nil end
  local last = s.items
  if n < (s.turns or 0) then
    local after = bone.chat.items({ kind = "user", turn = n + 1, session = id })[1]
    last = after and after.index - 1 or last
  end
  return {
    index = n, first = user.index, last = last, running = n == s.turns and s.running == true,
    tools = #bone.chat.items({ kind = "tool", from = user.index, to = last, session = id }),
  }
end

--- The bar of `turn` (an entry of bone.chat.turns()) in chat `s`, and how
--- long the turn took (nil when unknown); nil when there is nothing to draw.
--- `pick(seg)` answers a click on a segment (default: scroll to its item).
--- `id` names this bar among the window's; `redraw()` shows it again when
--- the pointer moves over it (default: the whole window).
function M.bar(s, turn, pick, id, redraw)
  if not s then return nil end
  local segs, total = segments(s, turn)
  if not segs then return nil end
  local sum = 0
  for _, seg in ipairs(segs) do sum = sum + seg.ms end
  -- Every segment stays wide enough to see and to point at.
  local floor = sum * 0.012
  local info = { id = "strip:" .. (id or tostring(turn.index)), redraw = redraw or function() bone.gui.invalidate() end }
  local bar = {
    id = info.id, grow = 1, width = 0, height = 14, direction = "row", align = "center", gap = 2,
    on_hover = function(on)
      if not on and M.hot and M.hot.bar == info.id then
        M.hot = nil
        info.redraw()
      end
    end,
  }
  for k, seg in ipairs(segs) do
    bar[#bar + 1] = segment(seg, math.max(seg.ms, floor), pick, k, info)
  end
  -- The playhead: a thin line through the segment under the pointer, where
  -- the last frame put it. It takes no pointer of its own.
  local at = M.hot and M.hot.bar == info.id and bone.gui.rect(info.id .. ":" .. M.hot.k)
  local box = at and bone.gui.rect(info.id)
  if at and box then
    bar[#bar + 1] = {
      position = "absolute", top = 0, bottom = 0, left = at.x - box.x + at.w / 2 - 0.75, width = 1.5,
      radius = 1, bg = ink("ff", "Normal"), shadow = { blur = 6, color = ink("80", "Dim") },
    }
  end
  return bar, total
end

-- The last turn of chat `id`, if it is open in this window.
local function last_turn(id)
  local s = bone.chat.session({ session = id })
  return s and M.turn(s, s.turns)
end

function M.setup()
  -- The turn's message is not in the chat yet when it starts: its clock is
  -- filed under it when it finishes (a running turn's comes from the session).
  bone.on("turn/started", function(ev)
    clocks[ev.session_id] = clocks[ev.session_id] or {}
    clocks[ev.session_id].starting = bone.now()
  end)
  bone.on("turn/finished", function(ev)
    local c, t = clocks[ev.session_id], last_turn(ev.session_id)
    if c and c.starting and t then
      c[t.index] = { started = c.starting, finished = bone.now() }
      c.starting = nil
    end
    for ask, id in pairs(asks) do
      if id == ev.session_id then asks[ask] = nil end
    end
  end)
  bone.on("ask/requested", function(ev) asks[ev.ask_id] = ev.session_id end)
  bone.on("ask/resolved", function(ev) asks[ev.ask_id] = nil end)
end

return M
