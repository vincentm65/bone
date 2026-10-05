-- usage: a full-screen dashboard of what your sessions used and did, from
-- the session index (`store/query`): token cards compared with the period
-- before, a usage chart, models, tools, a weekday × hour punchcard, a
-- daily activity calendar with streaks, and the busiest sessions.
--
--   /usage (or /stats)          the last 7 days
--   /usage today|week|month|year|all
--   /usage 2026-09-01..2026-09-30
--
-- Keys: 1-5 or ←→ period · t dates · j/k scroll · r refresh · q close.

local PERIODS = { "today", "week", "month", "year", "all" }
local TITLES = { today = "Today", week = "Week", month = "Month", year = "Year", all = "All" }
local DAY = 86400

-- Calendar helpers. Noon avoids DST edges when stepping by days.

local function midnight(t, days_back)
  local d = os.date("*t", t)
  return os.time({ year = d.year, month = d.month, day = d.day - (days_back or 0), hour = 0 })
end

local function noon_plus(t, days)
  local d = os.date("*t", t)
  return os.time({ year = d.year, month = d.month, day = d.day + days, hour = 12 })
end

local function ymd(t)
  return os.date("%Y-%m-%d", t)
end

local function parse_date(s)
  local y, m, d = tostring(s or ""):match("^(%d%d%d%d)%-(%d%d?)%-(%d%d?)$")
  if not y then
    return nil
  end
  return os.time({ year = tonumber(y), month = tonumber(m), day = tonumber(d), hour = 0 })
end

-- Monday of the week holding t (local time), at noon.
local function monday(t)
  local wday = os.date("*t", t).wday -- 1 = Sunday
  return noon_plus(t, -((wday + 5) % 7))
end

-- What a period covers: [from, to), the same length before it, and how the
-- chart groups it.
local function span(period, custom)
  local now = os.time()
  local r
  if custom then
    local days = math.floor((custom.to - custom.from) / DAY + 0.5)
    r = { from = custom.from, to = custom.to, bucket = days <= 62 and "day" or (days <= 400 and "week" or "month") }
    r.label = ymd(custom.from) .. " → " .. ymd(custom.to - 1)
  elseif period == "today" then
    r = { from = midnight(now), to = now + 1, bucket = "hour", label = os.date("%a %b %d") }
    r.prev_from, r.prev_to = r.from - DAY, now - DAY
  elseif period == "week" then
    r = { from = midnight(now, 6), to = now + 1, bucket = "day" }
  elseif period == "month" then
    r = { from = midnight(now, 29), to = now + 1, bucket = "day" }
  elseif period == "year" then
    r = { from = midnight(now, 364), to = now + 1, bucket = "week" }
  else
    r = { from = 0, to = now + 1, bucket = "month", label = "everything recorded" }
  end
  if not r.label then
    local fmt = os.date("%Y", r.from) == os.date("%Y", now) and "%b %d" or "%b %d %Y"
    r.label = os.date(fmt, r.from) .. " → " .. os.date(fmt, now)
  end
  if not r.prev_from and r.from > 0 then
    r.prev_from, r.prev_to = r.from - (r.to - r.from), r.from
  end
  return r
end

local BUCKET_SQL = {
  hour = "strftime('%H', at, 'unixepoch', 'localtime')",
  day = "date(at, 'unixepoch', 'localtime')",
  week = "date(at, 'unixepoch', 'localtime', 'weekday 0', '-6 days')",
  month = "strftime('%Y-%m', at, 'unixepoch', 'localtime')",
}

-- Every bucket of a span in order, as { key, label }.
local function buckets(r, first_key)
  local out = {}
  if r.bucket == "hour" then
    for h = 0, 23 do
      out[#out + 1] = { key = string.format("%02d", h), label = string.format("%02d", h) }
    end
  elseif r.bucket == "day" or r.bucket == "week" then
    local step = r.bucket == "day" and 1 or 7
    local t = r.bucket == "day" and noon_plus(r.from, 0) or monday(r.from)
    while t < r.to do
      local d = os.date("*t", t)
      out[#out + 1] = { key = ymd(t), label = d.month .. "/" .. d.day }
      t = noon_plus(t, step)
    end
  else
    local from = r.from
    if from == 0 then
      local y, m = tostring(first_key or ""):match("^(%d+)%-(%d+)")
      from = y and os.time({ year = tonumber(y), month = tonumber(m), day = 1, hour = 12 }) or os.time()
    end
    local d = os.date("*t", from)
    local y, m = d.year, d.month
    while os.time({ year = y, month = m, day = 1, hour = 0 }) < r.to do
      local t = os.time({ year = y, month = m, day = 1, hour = 12 })
      out[#out + 1] = { key = string.format("%04d-%02d", y, m), label = os.date(m == 1 and "%b'%y" or "%b", t) }
      m = m + 1
      if m > 12 then
        y, m = y + 1, 1
      end
    end
  end
  return out
end

local function queries(r)
  local b = BUCKET_SQL[r.bucket]
  return {
    totals = [[
      SELECT count(*), sum(input_tokens), sum(output_tokens), sum(cached_tokens),
             count(DISTINCT nullif(session_id, '')),
             count(DISTINCT date(at, 'unixepoch', 'localtime'))
      FROM usage WHERE at >= ?1 AND at < ?2]],
    prev = r.prev_from and [[
      SELECT count(*), sum(input_tokens), sum(output_tokens), sum(cached_tokens),
             count(DISTINCT nullif(session_id, ''))
      FROM usage WHERE at >= ?1 AND at < ?2]] or nil,
    series = "SELECT " .. b .. [[ AS k, sum(input_tokens + output_tokens), count(*)
      FROM usage WHERE at >= ?1 AND at < ?2 GROUP BY k ORDER BY k]],
    models = [[
      SELECT coalesce(nullif(provider, ''), '?'), model, count(*), sum(input_tokens),
             sum(output_tokens), sum(cached_tokens)
      FROM usage WHERE at >= ?1 AND at < ?2
      GROUP BY provider, model ORDER BY sum(input_tokens + output_tokens) DESC LIMIT 8]],
    punch = [[
      SELECT strftime('%w', at, 'unixepoch', 'localtime'), strftime('%H', at, 'unixepoch', 'localtime'),
             sum(input_tokens + output_tokens)
      FROM usage WHERE at >= ?1 AND at < ?2 GROUP BY 1, 2]],
    daily = [[
      SELECT date(at, 'unixepoch', 'localtime') AS d, sum(input_tokens + output_tokens)
      FROM usage WHERE at >= ?1 AND at < ?2 GROUP BY d]],
    sessions = [[
      SELECT coalesce(s.renamed, s.title, '(untitled)'), s.cwd,
             sum(u.input_tokens + u.output_tokens), count(*)
      FROM usage u JOIN sessions s ON s.id = u.session_id
      WHERE u.at >= ?1 AND u.at < ?2
      GROUP BY u.session_id ORDER BY sum(u.input_tokens + u.output_tokens) DESC LIMIT 6]],
    tools = [[
      SELECT name, count(*), sum(coalesce(is_error, 0)), avg(output_chars)
      FROM tool_calls WHERE at >= ?1 AND at < ?2 GROUP BY name ORDER BY count(*) DESC]],
  }
end

-- Text helpers ---------------------------------------------------------------

local function num(v)
  v = tonumber(v) or 0
  for _, u in ipairs({ { 1e9, "B" }, { 1e6, "M" }, { 1e3, "k" } }) do
    if v >= u[1] then
      local x = v / u[1]
      local s = x >= 100 and string.format("%.0f", x) or string.format("%.1f", x)
      return s:gsub("%.0$", "") .. u[2]
    end
  end
  return tostring(math.floor(v + 0.5))
end

local function width(s)
  return bone.text.width(s)
end

local function pad(s, w)
  s = tostring(s)
  return s .. string.rep(" ", math.max(w - width(s), 0))
end

local function lpad(s, w)
  s = tostring(s)
  return string.rep(" ", math.max(w - width(s), 0)) .. s
end

local function cut(s, w)
  s = tostring(s or ""):gsub("%s+", " ")
  if w <= 0 then
    return ""
  end
  return width(s) > w and bone.text.truncate(s, w) or s
end

local function line_width(l)
  local w = 0
  for _, sp in ipairs(l) do
    w = w + width(type(sp) == "table" and sp[1] or sp)
  end
  return w
end

-- A line builder: add(text, hl) appends, merging runs of the same group.
local function builder()
  local l = {}
  return l, function(text, hl)
    if text == "" then
      return
    end
    hl = hl or "Normal"
    local last = l[#l]
    if last and last[2] == hl then
      last[1] = last[1] .. text
    else
      l[#l + 1] = { text, hl }
    end
  end
end

local function pct(a, b)
  a, b = tonumber(a) or 0, tonumber(b) or 0
  return b > 0 and a * 100 / b or nil
end

-- Colors ---------------------------------------------------------------------

local function hex(c)
  if type(c) ~= "string" then
    return nil
  end
  local r, g, b = c:match("^#(%x%x)(%x%x)(%x%x)$")
  return r and { tonumber(r, 16), tonumber(g, 16), tonumber(b, 16) }
end

local function fg_of(group)
  local seen = 0
  local h = bone.hl.get(group)
  while h and h.link and seen < 5 do
    h, seen = bone.hl.get(h.link), seen + 1
  end
  return h and h.fg
end

-- Heat levels 0-4: from the border color up to the accent when both are
-- #rrggbb, otherwise existing groups.
local function setup_colors()
  local lo, hi = hex(fg_of("WinSeparator")), hex(fg_of("Accent"))
  if lo and hi then
    for i, f in ipairs({ 0, 0.35, 0.6, 0.8, 1 }) do
      local c = {}
      for j = 1, 3 do
        c[j] = math.floor(lo[j] + (hi[j] - lo[j]) * f + 0.5)
      end
      bone.hl.set("UsageHeat" .. (i - 1), { fg = string.format("#%02x%02x%02x", c[1], c[2], c[3]) })
    end
  else
    for i, g in ipairs({ "WinSeparator", "Dim", "ToolArgs", "Normal", "Accent" }) do
      bone.hl.set("UsageHeat" .. (i - 1), { link = g })
    end
  end
  bone.hl.set("UsageBar", { link = "Accent" })
  bone.hl.set("UsageValue", { link = "MdBold" })
end

local function heat(v, max)
  v = tonumber(v) or 0
  if v <= 0 or max <= 0 then
    return "UsageHeat0"
  end
  return "UsageHeat" .. math.max(1, math.min(4, math.ceil(math.sqrt(v / max) * 4)))
end

-- Sections: each returns a list of lines no wider than w. -------------------

local function title(text, w, right)
  local l, add = builder()
  add(" " .. text .. " ", "MdHeading")
  local tail = right and (" " .. right .. " ") or ""
  if width(text) + 2 + width(tail) > w then
    tail = ""
  end
  add(string.rep("─", math.max(w - width(text) - 2 - width(tail), 0)), "WinSeparator")
  add(tail, "Dim")
  return l
end

-- The change from the period before; nil when there is none to compare.
local function delta(now, before, has_prev)
  if not has_prev then
    return nil
  end
  now, before = tonumber(now) or 0, tonumber(before) or 0
  if before == 0 then
    return nil
  end
  local d = (now - before) * 100 / before
  if math.abs(d) < 0.5 then
    return "±0%"
  end
  return (d > 0 and "▲" or "▼") .. string.format("%.0f%%", math.abs(d))
end

local function cards(d, r, w)
  local t, p = d.totals, d.prev
  local tokens = (tonumber(t[2]) or 0) + (tonumber(t[3]) or 0)
  local calls = tonumber(t[1]) or 0
  local tool_calls, tool_fail = 0, 0
  for _, row in ipairs(d.tools) do
    tool_calls = tool_calls + (tonumber(row[2]) or 0)
    tool_fail = tool_fail + (tonumber(row[3]) or 0)
  end
  local ptok = p and ((tonumber(p[2]) or 0) + (tonumber(p[3]) or 0))
  local cache = pct(t[4], t[2])
  local list = {
    { "Tokens", num(tokens), delta(tokens, ptok, p),
      r.bucket == "hour" and "in + out" or (num(tokens / math.max(tonumber(t[6]) or 1, 1)) .. "/day") },
    { "Input", num(t[2]), p and delta(t[2], p[2], true), string.format("%.0f%% of total", pct(t[2], tokens) or 0) },
    { "Output", num(t[3]), p and delta(t[3], p[3], true), num((tonumber(t[3]) or 0) / math.max(calls, 1)) .. "/request" },
    { "Cached", num(t[4]), p and delta(t[4], p[4], true), cache and string.format("%.0f%% of input", cache) or "no input" },
    { "Requests", num(calls), p and delta(calls, p[1], true), num(tokens / math.max(calls, 1)) .. " avg" },
    { "Sessions", num(t[5]), p and delta(t[5], p[5], true), num(calls / math.max(tonumber(t[5]) or 1, 1)) .. " req avg" },
    { "Tool calls", num(tool_calls), nil,
      tool_calls > 0 and string.format("%.1f%% failed", tool_fail * 100 / tool_calls) or "none" },
  }
  -- As many per row as fit at 16+ columns, spread over the width.
  local per = math.max(1, math.min(#list, math.floor(w / 16)))
  local out = {}
  for start = 1, #list, per do
    local lines = { {}, {}, {}, {} }
    local function put(li, text, hl)
      local l = lines[li]
      local last = l[#l]
      if last and last[2] == hl then
        last[1] = last[1] .. text
      elseif text ~= "" then
        l[#l + 1] = { text, hl }
      end
    end
    local base = math.floor(w / per)
    for i = start, math.min(start + per - 1, #list) do
      local c = list[i]
      local cw = (i - start + 1 == per) and (w - base * (per - 1)) or base
      local inner = cw - 2
      local label = cut(c[1], inner - 3)
      put(1, "╭ ", "WinSeparator")
      put(1, label, "Dim")
      put(1, " " .. string.rep("─", math.max(inner - width(label) - 2, 0)) .. "╮", "WinSeparator")
      local value = cut(c[2], inner - 2)
      local dl = c[3] and cut(" " .. c[3], math.max(inner - 2 - width(value), 0)) or ""
      put(2, "│ ", "WinSeparator")
      put(2, value, "UsageValue")
      put(2, dl, c[3] and c[3]:find("▲") and "Notice" or "Dim")
      put(2, string.rep(" ", math.max(inner - 1 - width(value) - width(dl), 0)) .. "│", "WinSeparator")
      local sub = cut(c[4] or "", inner - 2)
      put(3, "│ ", "WinSeparator")
      put(3, sub, "Dim")
      put(3, string.rep(" ", math.max(inner - 1 - width(sub), 0)) .. "│", "WinSeparator")
      put(4, "╰" .. string.rep("─", math.max(inner, 0)) .. "╯", "WinSeparator")
    end
    for _, l in ipairs(lines) do
      out[#out + 1] = l
    end
  end
  return out
end

local BLOCKS = { "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█" }

-- Columns per bucket, scaled in eighths of a row, with a value axis on the
-- left and labels under it. The current bucket is drawn in the accent.
local function chart(d, r, w, h)
  local list = buckets(r, d.series_first)
  local axis_w = 7
  local room = w - axis_w
  -- Keep the latest buckets when there are more than columns.
  if #list > room then
    local keep = {}
    for i = #list - room + 1, #list do
      keep[#keep + 1] = list[i]
    end
    list = keep
  end
  local n = math.max(#list, 1)
  local cw = math.max(1, math.min(10, math.floor(room / n)))
  local bw = cw >= 3 and cw - 1 or cw
  local max, total, peak, active = 0, 0, nil, 0
  for _, b in ipairs(list) do
    local s = d.series[b.key]
    b.v = s and (tonumber(s[1]) or 0) or 0
    total = total + b.v
    if b.v > 0 then
      active = active + 1
    end
    if b.v > max then
      max, peak = b.v, b
    end
  end
  local right = peak and string.format("peak %s on %s · avg %s", num(max), peak.label, num(total / math.max(active, 1)))
  local out = { title("Tokens per " .. r.bucket, w, right) }
  if max == 0 then
    out[#out + 1] = { { "  no model calls in this period", "Dim" } }
    return out
  end
  local now_key = r.bucket == "hour" and os.date("%H") or (r.bucket == "day" and ymd(os.time())) or nil
  for row = h, 1, -1 do
    local l, add = builder()
    local tick = (row == h and num(max)) or (row == math.ceil(h / 2) and num(max * (row - 0.5) / h)) or ""
    add(lpad(tick, axis_w - 2) .. " ", "Dim")
    add(tick ~= "" and "┤" or "│", "WinSeparator")
    for _, b in ipairs(list) do
      local eighths = b.v > 0 and math.max(1, math.floor(b.v / max * h * 8 + 0.5)) or 0
      local fill = math.max(0, math.min(8, eighths - (row - 1) * 8))
      add(string.rep(fill == 0 and " " or BLOCKS[fill], bw), b.key == now_key and "Accent" or heat(b.v, max))
      add(string.rep(" ", cw - bw), "Normal")
    end
    out[#out + 1] = l
  end
  -- Labels under the columns, skipping ones that would touch the previous.
  local axis = {}
  for i = 1, axis_w + n * cw do
    axis[i] = " "
  end
  local free = 0
  for i, b in ipairs(list) do
    local at = axis_w + (i - 1) * cw + 1
    if at > free and at + #b.label - 1 <= #axis then
      for j = 1, #b.label do
        axis[at + j - 1] = b.label:sub(j, j)
      end
      free = at + #b.label
    end
  end
  out[#out + 1] = { { string.rep(" ", axis_w - 1) .. "└" .. string.rep("─", n * cw), "WinSeparator" } }
  out[#out + 1] = { { table.concat(axis), "Dim" } }
  return out
end

local PARTS = { "▏", "▎", "▍", "▌", "▋", "▊", "▉", "█" }

local function bar(frac, w)
  local cells = math.max(0, math.min(1, frac)) * w
  local full = math.floor(cells)
  local rest = math.floor((cells - full) * 8 + 0.5)
  local s = string.rep("█", full)
  if rest > 0 and full < w then
    s = s .. PARTS[rest]
  elseif full == 0 and frac > 0 then
    s = PARTS[1]
  end
  return s
end

local function models(d, w)
  local out = { title("Models", w) }
  if #d.models == 0 then
    out[#out + 1] = { { "  none", "Dim" } }
    return out
  end
  local total = 0
  for _, m in ipairs(d.models) do
    total = total + (tonumber(m[4]) or 0) + (tonumber(m[5]) or 0)
  end
  local share_w = w >= 70 and 10 or 0
  local name_w = math.max(w - 2 - 29 - share_w, 8)
  local head, add = builder()
  add("  " .. pad(cut("provider / model", name_w - 1), name_w) .. lpad("req", 6) .. lpad("in", 8) .. lpad("out", 8) .. lpad("cache", 7), "Dim")
  if share_w > 0 then
    add(pad("  share", share_w), "Dim")
  end
  out[#out + 1] = head
  for _, m in ipairs(d.models) do
    local tok = (tonumber(m[4]) or 0) + (tonumber(m[5]) or 0)
    local cache = pct(m[6], m[4])
    local l, a = builder()
    a("  ")
    a(pad(cut(m[1] .. " / " .. m[2], name_w - 1), name_w), "Normal")
    a(lpad(num(m[3]), 6), "Dim")
    a(lpad(num(m[4]), 8), "Normal")
    a(lpad(num(m[5]), 8), "Normal")
    a(lpad(cache and string.format("%.0f%%", cache) or "-", 7), "Dim")
    if share_w > 0 then
      a("  ", "Normal")
      a(bar(total > 0 and tok / total or 0, share_w - 2), "UsageBar")
    end
    out[#out + 1] = l
  end
  return out
end

local function tools(d, w, limit)
  local calls, fails = 0, 0
  for _, t in ipairs(d.tools) do
    calls, fails = calls + (tonumber(t[2]) or 0), fails + (tonumber(t[3]) or 0)
  end
  local out = { title("Tools", w, calls > 0 and string.format("%s calls · %s failed", num(calls), num(fails)) or nil) }
  if #d.tools == 0 then
    out[#out + 1] = { { "  no tool calls in this period", "Dim" } }
    return out
  end
  local top = tonumber(d.tools[1][2]) or 1
  local nums = 7 + 8 + 9
  local bar_w = w >= 50 and math.min(16, w - 2 - nums - 16) or 0
  local name_w = math.max(w - 2 - nums - bar_w, 8)
  local head, add = builder()
  add("  " .. pad("tool", name_w + bar_w) .. lpad("calls", 7) .. lpad("failed", 8) .. lpad("avg out", 9), "Dim")
  out[#out + 1] = head
  for i, t in ipairs(d.tools) do
    if i > limit then
      out[#out + 1] = { { string.format("  … %d more", #d.tools - limit), "Dim" } }
      break
    end
    local c, f = tonumber(t[2]) or 0, tonumber(t[3]) or 0
    local rate = c > 0 and f * 100 / c or 0
    local l, a = builder()
    a("  ")
    a(pad(cut(t[1], name_w - 1), name_w), "Normal")
    if bar_w > 0 then
      local b = bar(c / top, bar_w - 1)
      a(b, "UsageBar")
      a(string.rep(" ", bar_w - width(b)), "Normal")
    end
    a(lpad(num(c), 7), "Normal")
    a(lpad(f > 0 and string.format("%.1f%%", rate) or "-", 8), rate >= 10 and "ErrorMsg" or (f > 0 and "Notice" or "Dim"))
    a(lpad(t[4] and (num(t[4]) .. "ch") or "-", 9), "Dim")
    out[#out + 1] = l
  end
  return out
end

local WEEKDAYS = { "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun" }

-- Weekday × hour: when in the week the tokens were spent.
local function punchcard(d, w)
  local cell = w >= 4 + 48 and 2 or 1
  local grid, max = {}, 0
  for _, row in ipairs(d.punch) do
    local mon0 = ((tonumber(row[1]) or 0) + 6) % 7 -- 0 = Monday
    local key = mon0 * 24 + (tonumber(row[2]) or 0)
    grid[key] = tonumber(row[3]) or 0
    max = math.max(max, grid[key])
  end
  -- The busiest hour of the day, summed over the week.
  local by_hour, best = {}, nil
  for hr = 0, 23 do
    local s = 0
    for wd = 0, 6 do
      s = s + (grid[wd * 24 + hr] or 0)
    end
    by_hour[hr] = s
    if s > 0 and (not best or s > by_hour[best]) then
      best = hr
    end
  end
  local out = { title("Hours", w, best and string.format("busiest %02d:00-%02d:00", best, (best + 1) % 24)) }
  if max == 0 then
    out[#out + 1] = { { "  no activity", "Dim" } }
    return out
  end
  local axis = { "    " }
  for hr = 0, 23, 6 do
    axis[#axis + 1] = pad(tostring(hr), 6 * cell)
  end
  out[#out + 1] = { { table.concat(axis), "Dim" } }
  for wd = 0, 6 do
    local l, add = builder()
    add(WEEKDAYS[wd + 1] .. " ", "Dim")
    for hr = 0, 23 do
      add(cell == 2 and "■ " or "■", heat(grid[wd * 24 + hr], max))
    end
    out[#out + 1] = l
  end
  return out
end

-- Active-day streaks over everything recorded: (current, longest, days).
local function streaks(daily)
  local cur, t = 0, noon_plus(os.time(), 0)
  if not daily[ymd(t)] then
    t = noon_plus(t, -1)
  end
  while daily[ymd(t)] do
    cur, t = cur + 1, noon_plus(t, -1)
  end
  local days = {}
  for k in pairs(daily) do
    days[#days + 1] = k
  end
  table.sort(days)
  local best, run, prev = 0, 0, nil
  for _, k in ipairs(days) do
    run = (prev and ymd(noon_plus(parse_date(prev), 1)) == k) and run + 1 or 1
    best, prev = math.max(best, run), k
  end
  return cur, best, #days
end

-- GitHub-style calendar of the last weeks that fit, ending this week.
local function calendar(d, w)
  local weeks = math.max(4, math.min(53, math.floor((w - 4) / 2)))
  local start = noon_plus(monday(os.time()), -7 * (weeks - 1))
  local today = ymd(os.time())
  local keys, max = {}, 0
  for i = 0, weeks * 7 - 1 do
    keys[i] = ymd(noon_plus(start, i))
    max = math.max(max, d.daily[keys[i]] or 0)
  end
  local cur, best, active = streaks(d.daily)
  local out = { title("Activity", w, string.format("streak %dd · best %dd · %d day%s", cur, best, active, active == 1 and "" or "s")) }
  -- Month names over the week a month starts in.
  local axis = {}
  for i = 1, 4 + weeks * 2 do
    axis[i] = " "
  end
  local free = 0
  for wk = 0, weeks - 1 do
    local t = noon_plus(start, wk * 7)
    local at = 5 + wk * 2
    local name = os.date("%b", t)
    if (os.date("*t", t).day <= 7 or wk == 0) and at > free and at + #name - 1 <= #axis then
      for j = 1, #name do
        axis[at + j - 1] = name:sub(j, j)
      end
      free = at + #name
    end
  end
  out[#out + 1] = { { table.concat(axis), "Dim" } }
  for wd = 0, 6 do
    local l, add = builder()
    add((wd % 2 == 0) and (WEEKDAYS[wd + 1] .. " ") or "    ", "Dim")
    for wk = 0, weeks - 1 do
      local k = keys[wk * 7 + wd]
      if k > today then
        add("  ", "Normal")
      else
        add("■ ", heat(d.daily[k], max))
      end
    end
    out[#out + 1] = l
  end
  local legend, add = builder()
  add(string.rep(" ", math.max(math.min(w, 4 + weeks * 2) - 16, 0)) .. "less ", "Dim")
  for i = 0, 4 do
    add("■", "UsageHeat" .. i)
  end
  add(" more", "Dim")
  out[#out + 1] = legend
  return out
end

local function sessions(d, w)
  local out = { title("Busiest sessions", w) }
  if #d.sessions == 0 then
    out[#out + 1] = { { "  none", "Dim" } }
    return out
  end
  local dir_w = w >= 48 and math.min(16, w - 40) or 0
  for _, s in ipairs(d.sessions) do
    local l, add = builder()
    add(lpad(num(s[3]), 8), "UsageValue")
    add(lpad(num(s[4]) .. "r", 6) .. "  ", "Dim")
    local name_w = w - 16 - dir_w
    add(pad(cut(s[1], name_w - 1), name_w), "Normal")
    if dir_w > 0 then
      add(lpad(cut(tostring(s[2] or ""):match("[^/]+$") or "", dir_w), dir_w), "ToolPath")
    end
    out[#out + 1] = l
  end
  return out
end

-- Two columns of lines side by side.
local function beside(left, lw, right, gap)
  local out = {}
  for i = 1, math.max(#left, #right) do
    local l = {}
    for _, sp in ipairs(left[i] or {}) do
      l[#l + 1] = sp
    end
    l[#l + 1] = { string.rep(" ", lw - line_width(left[i] or {}) + gap), "Normal" }
    for _, sp in ipairs(right[i] or {}) do
      l[#l + 1] = sp
    end
    out[#out + 1] = l
  end
  return out
end

local function append(out, lines, blank)
  if blank and #out > 0 then
    out[#out + 1] = {}
  end
  for _, l in ipairs(lines) do
    out[#out + 1] = l
  end
end

-- The page ---------------------------------------------------------------------

local function body(d, r, w, h)
  local out = {}
  local inner = w - 2
  append(out, cards(d, r, inner))
  local two = inner >= 110
  append(out, chart(d, r, inner, math.max(6, math.min(two and 16 or 10, h - (two and 46 or 30)))), true)
  if two then
    local lw = math.floor((inner - 3) / 2)
    local rw = inner - 3 - lw
    local left, right = {}, {}
    append(left, models(d, lw))
    append(left, tools(d, lw, 14), true)
    append(right, punchcard(d, rw))
    append(right, calendar(d, rw), true)
    append(right, sessions(d, rw), true)
    append(out, beside(left, lw, right, 3), true)
  else
    append(out, models(d, inner), true)
    append(out, tools(d, inner, 10), true)
    append(out, punchcard(d, inner), true)
    append(out, calendar(d, inner), true)
    append(out, sessions(d, inner), true)
  end
  for _, l in ipairs(out) do
    table.insert(l, 1, { " ", "Normal" })
  end
  return out
end

local function parse_range(s)
  local a, b = tostring(s or ""):match("^%s*(%d+%-%d+%-%d+)%s*%.*%s*(%d*%-?%d*%-?%d*)%s*$")
  local from = parse_date(a)
  local to = (b and b ~= "") and parse_date(b) or from
  if from and to and to >= from then
    return { from = from, to = midnight(to, -1) }
  end
end

local function open(period, custom)
  setup_colors()
  local st = { period = period, custom = custom, scroll = 0, gen = 0 }
  local id

  local function redraw()
    if id then
      bone.ui.update(id, {})
    end
  end

  local function load()
    st.gen = st.gen + 1
    local gen = st.gen
    local r = span(st.period, st.custom)
    st.loading, st.err = true, nil
    local results, pending, failed = {}, 0, nil
    local qs = queries(r)
    for _ in pairs(qs) do
      pending = pending + 1
    end
    for name, sql in pairs(qs) do
      -- Every query takes exactly ?1 and ?2: the span, the span before it,
      -- or (for the calendar) everything.
      local params = { r.from, r.to }
      if name == "prev" then
        params = { r.prev_from, r.prev_to }
      elseif name == "daily" then
        params = { 0, os.time() + DAY }
      end
      bone.request("store/query", { sql = sql, params = params }, function(res, err)
        if gen ~= st.gen then
          return
        end
        pending = pending - 1
        if err then
          failed = failed or (type(err) == "table" and err.message or tostring(err))
        else
          results[name] = res.rows or {}
        end
        if pending > 0 then
          return
        end
        st.loading = false
        if failed then
          st.err = failed
          return redraw()
        end
        local data = {
          totals = results.totals[1] or {},
          prev = results.prev and results.prev[1],
          series = {},
          models = results.models,
          punch = results.punch,
          daily = {},
          sessions = results.sessions,
          tools = results.tools,
        }
        for _, row in ipairs(results.series) do
          data.series[tostring(row[1])] = { row[2], row[3] }
          data.series_first = data.series_first or tostring(row[1])
        end
        for _, row in ipairs(results.daily) do
          if (tonumber(row[2]) or 0) > 0 then
            data.daily[tostring(row[1])] = tonumber(row[2])
          end
        end
        st.data, st.span, st.updated = data, r, os.time()
        redraw()
      end)
    end
  end

  local function header(w)
    local r = st.span or span(st.period, st.custom)
    local l1, add = builder()
    add(" Usage ", "Accent")
    add(" " .. r.label, "Normal")
    local note = st.loading and "loading…" or (st.updated and ("updated " .. os.date("%H:%M", st.updated)) or "")
    add(string.rep(" ", math.max(w - line_width(l1) - width(note) - 1, 1)) .. note, "Dim")
    local l2, tab = builder()
    tab(" ")
    for i, p in ipairs(PERIODS) do
      local on = not st.custom and st.period == p
      tab(" " .. i .. " ", on and "Accent" or "Dim")
      tab(TITLES[p] .. " ", on and "Selection" or "Normal")
      tab(" ")
    end
    local dates = st.custom and (r.label .. " ") or "Dates "
    if line_width(l2) + 3 + width(dates) <= w then
      tab(" t ", st.custom and "Accent" or "Dim")
      tab(dates, st.custom and "Selection" or "Normal")
    end
    return { l1, l2, { { string.rep("─", w), "WinSeparator" } } }
  end

  local function footer(w)
    local l, add = builder()
    if st.input then
      add(" dates ", "Accent")
      add(st.input, "Normal")
      add("▏", "Accent")
      add("   YYYY-MM-DD..YYYY-MM-DD · enter apply · esc cancel", "Dim")
    elseif st.note then
      add(" " .. st.note, "ErrorMsg")
    else
      local hints = { { "1-5 ←→", "period" }, { "t", "dates" }, { "j/k", "scroll" }, { "r", "refresh" }, { "q", "close" } }
      for i, h in ipairs(hints) do
        if line_width(l) + 3 + width(h[1] .. " " .. h[2]) > w then
          break
        end
        add(i == 1 and " " or "   ")
        add(h[1], "Accent")
        add(" " .. h[2], "Dim")
      end
      if (st.max_scroll or 0) > 0 then
        local more = string.format("%d/%d ", st.scroll, st.max_scroll)
        add(string.rep(" ", math.max(w - line_width(l) - width(more), 1)) .. more, "Dim")
      end
    end
    return { { { string.rep("─", w), "WinSeparator" } }, l }
  end

  local function render(ctx)
    local w, h = math.max(ctx.width, 40), math.max(ctx.height, 12)
    local out = header(w)
    local view_h = h - #out - 2
    local lines
    if st.err then
      lines = { { { "  error: " .. st.err, "ErrorMsg" } } }
    elseif not st.data then
      lines = { { { "  loading…", "Dim" } } }
    else
      lines = body(st.data, st.span, w, h)
    end
    st.max_scroll = math.max(#lines - view_h, 0)
    st.scroll = math.max(0, math.min(st.scroll, st.max_scroll))
    for i = st.scroll + 1, st.scroll + view_h do
      out[#out + 1] = lines[i] or {}
    end
    for _, l in ipairs(footer(w)) do
      out[#out + 1] = l
    end
    return out
  end

  local function set_period(p)
    if p and (p ~= st.period or st.custom) then
      st.period, st.custom, st.scroll = p, nil, 0
      load()
    end
  end

  local SHORT = { d = "today", w = "week", m = "month", y = "year", a = "all" }
  local function on_key(k)
    st.note = nil
    if st.input then
      if k == "enter" then
        local c = parse_range(st.input)
        if c then
          st.custom, st.input, st.scroll = c, nil, 0
          load()
        else
          st.note = "dates: YYYY-MM-DD..YYYY-MM-DD, start on or before end"
          st.input = nil
        end
      elseif k == "esc" then
        st.input = nil
      elseif k == "backspace" then
        st.input = st.input:sub(1, -2)
      elseif k == "ctrl+u" then
        st.input = ""
      elseif k:match("^[%d%.%-]$") and #st.input < 24 then
        st.input = st.input .. k
      end
      return true
    end
    local idx = 1
    for i, p in ipairs(PERIODS) do
      if p == st.period then
        idx = i
      end
    end
    if k == "esc" or k == "q" or k == "ctrl+c" then
      bone.ui.close(id)
    elseif k:match("^[1-5]$") then
      set_period(PERIODS[tonumber(k)])
    elseif SHORT[k] then
      set_period(SHORT[k])
    elseif k == "left" or k == "h" then
      set_period(PERIODS[st.custom and idx or math.max(1, idx - 1)])
    elseif k == "right" or k == "l" then
      set_period(PERIODS[st.custom and idx or math.min(#PERIODS, idx + 1)])
    elseif k == "t" then
      local r = st.span or span(st.period, st.custom)
      st.input = (r.from > 0 and ymd(r.from) or "") .. ".." .. ymd(r.to - 1)
    elseif k == "r" then
      load()
    elseif k == "down" or k == "j" or k == "wheeldown" then
      st.scroll = st.scroll + (k == "wheeldown" and 3 or 1)
    elseif k == "up" or k == "k" or k == "wheelup" then
      st.scroll = math.max(0, st.scroll - (k == "wheelup" and 3 or 1))
    elseif k == "pagedown" or k == "space" then
      st.scroll = st.scroll + 10
    elseif k == "pageup" then
      st.scroll = math.max(0, st.scroll - 10)
    elseif k == "home" or k == "g" then
      st.scroll = 0
    elseif k == "end" or k == "G" then
      st.scroll = st.max_scroll or 0
    end
    return true
  end

  load()
  id = bone.ui.popup({
    lines = render,
    on_key = on_key,
    anchor = "screen",
    row = 0,
    col = 0,
    width = 10000,
    height = 10000,
  })
  return id
end

local function run(c)
  local arg = (c.args or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if arg == "" then
    return open("week")
  end
  for _, p in ipairs(PERIODS) do
    if arg == p then
      return open(p)
    end
  end
  local custom = parse_range(arg)
  if custom then
    return open("week", custom)
  end
  bone.notify("usage: use today, week, month, year, all or YYYY-MM-DD..YYYY-MM-DD", "error")
end

local opts = {
  desc = "usage dashboard: tokens, models, tools, activity; today|week|month|year|all|DATE..DATE",
  complete = function()
    return {
      { value = "today", desc = "since midnight, by hour" },
      { value = "week", desc = "the last 7 days" },
      { value = "month", desc = "the last 30 days" },
      { value = "year", desc = "the last 365 days, by week" },
      { value = "all", desc = "everything recorded, by month" },
    }
  end,
}
bone.cmd.create("usage", run, opts)
bone.cmd.create("stats", run, opts)
