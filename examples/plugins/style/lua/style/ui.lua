-- Default statusline and divider (the line between the chat and the prompt).
-- Replace either from tui.lua:
--   bone.ui.statusline = function(ctx) return { " ", ctx.title } end
--
-- Statusline ctx: title, popup ("popup", "picker" or nil), spinner, width,
-- and session (or nil): { title, cwd, running, elapsed }.
-- Divider ctx: spinner, width, session.

local function elapsed(secs)
  return bone.util.duration(secs)
end

local function tokens(n)
  if n >= 1000 then
    return string.format("%.1fk", n / 1000)
  end
  return tostring(n)
end

local function items_width(items)
  local w = 0
  for _, it in ipairs(items) do
    w = w + bone.strwidth(type(it) == "table" and (it[1] or it.text or "") or it)
  end
  return w
end

-- The latest turn call's tokens, from the session index (so a resumed
-- session shows them too); refreshed as each reply completes.
local last = {}
local function load_last(id)
  last = { session = id }
  if not id then return end
  bone.request("store/query", {
    sql = "SELECT input_tokens, output_tokens FROM usage WHERE session_id = ?1 AND source = 'turn' ORDER BY rowid DESC LIMIT 1",
    params = { id },
  }, function(res)
    if last.session == id then last.row = res and res.rows and res.rows[1] end
  end)
end
bone.on("message/completed", function(ev)
  if ev.session_id == last.session then load_last(ev.session_id) end
end)

function bone.ui.statusline(ctx)
  local left = { { " " .. ctx.title, "StatusLine" } }
  -- Right side, most important first; trailing items are dropped to fit.
  local right = {}
  local s = ctx.session
  if (s and s.session_id) ~= last.session then load_last(s and s.session_id) end
  if last.row then
    right[#right + 1] = { tokens(last.row[1]) .. " in · " .. tokens(last.row[2]) .. " out", "StatusLineDim" }
  end
  if s and s.cwd then
    right[#right + 1] = { s.cwd:gsub("^" .. (os.getenv("HOME") or "\0"), "~"), "StatusLineDim" }
  end

  local function joined(list)
    local out = {}
    for i, it in ipairs(list) do
      if i > 1 then
        out[#out + 1] = { "  │  ", "StatusLineDim" }
      end
      out[#out + 1] = it
    end
    out[#out + 1] = " "
    return out
  end
  local room = ctx.width - items_width(left) - 1
  local r = joined(right)
  while #right > 0 and items_width(r) > room do
    right[#right] = nil
    r = joined(right)
  end

  local items = left
  items[#items + 1] = "%="
  for _, it in ipairs(r) do
    items[#items + 1] = it
  end
  return items
end

function bone.ui.divider(ctx)
  local s = ctx.session
  if s and s.running then
    local status = s.elapsed and ("working " .. elapsed(s.elapsed)) or "starting"
    return {
      { "── ", "WinSeparator" },
      { ctx.spinner .. " " .. status, "Accent" },
      { "  ctrl+c to cancel ", "Dim" },
      { fill = "─", hl = "WinSeparator" },
    }
  end
  return { { fill = "─", hl = "WinSeparator" } }
end
