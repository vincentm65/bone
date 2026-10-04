-- stats: what your sessions used and did, from the session index
-- (`store/query`): tokens by model and by day, the busiest sessions, and
-- how often each tool was called and failed.
--
--   /stats            the last 30 days
--   /stats today|week|month|all

local PERIODS = {
  today = { label = "today" },
  week = { days = 7, label = "the last 7 days" },
  month = { days = 30, label = "the last 30 days" },
  all = { label = "all time" },
}

local function since(name)
  local p = PERIODS[name]
  if name == "today" then
    local t = os.date("*t")
    return os.time({ year = t.year, month = t.month, day = t.day, hour = 0 })
  end
  return p.days and (os.time() - p.days * 86400) or 0
end

-- 1234 → "1.2k", 1234567 → "1.2M".
local function n(v)
  v = tonumber(v) or 0
  if v >= 1e6 then
    return string.format("%.1fM", v / 1e6)
  elseif v >= 1e4 then
    return string.format("%.0fk", v / 1e3)
  elseif v >= 1e3 then
    return string.format("%.1fk", v / 1e3)
  end
  return tostring(math.floor(v))
end

local function pad(s, w)
  s = tostring(s)
  local len = bone.text and bone.text.width and bone.text.width(s) or #s
  return s .. string.rep(" ", math.max(w - len, 0))
end

local function lpad(s, w)
  s = tostring(s)
  return string.rep(" ", math.max(w - #s, 0)) .. s
end

local function cut(s, w)
  s = tostring(s or "")
  if #s > w then
    return s:sub(1, w - 1) .. "…"
  end
  return s
end

local QUERIES = {
  totals = [[
    SELECT count(*), sum(input_tokens), sum(output_tokens), count(DISTINCT nullif(session_id, ''))
    FROM usage WHERE at >= ?1]],
  models = [[
    SELECT model, count(*), sum(input_tokens), sum(output_tokens)
    FROM usage WHERE at >= ?1 GROUP BY model ORDER BY sum(input_tokens) DESC LIMIT 10]],
  days = [[
    SELECT date(at, 'unixepoch', 'localtime') AS day, count(*), sum(input_tokens), sum(output_tokens)
    FROM usage WHERE at >= ?1 GROUP BY day ORDER BY day DESC LIMIT 14]],
  sessions = [[
    SELECT coalesce(s.renamed, s.title, '(untitled)'), sum(u.input_tokens), sum(u.output_tokens), count(*)
    FROM usage u JOIN sessions s ON s.id = u.session_id
    WHERE u.at >= ?1 GROUP BY u.session_id ORDER BY sum(u.input_tokens) DESC LIMIT 8]],
  tools = [[
    SELECT name, count(*), sum(is_error), avg(output_chars)
    FROM tool_calls WHERE at >= ?1 GROUP BY name ORDER BY count(*) DESC LIMIT 15]],
}

local function render(period, r)
  local out = {}
  local function add(s)
    out[#out + 1] = s
  end
  local t = r.totals[1] or {}
  add("# Usage, " .. PERIODS[period].label)
  if (tonumber(t[1]) or 0) == 0 then
    add("No model calls recorded in this period.")
    add("(Usage is recorded from now on; sessions from before have none.)")
  else
    add(string.format("%s model calls in %s sessions: %s tokens in, %s out.",
      n(t[1]), n(t[4]), n(t[2]), n(t[3])))
    add("")
    add("# By model")
    add(pad("model", 28) .. lpad("calls", 7) .. lpad("in", 9) .. lpad("out", 9))
    for _, row in ipairs(r.models) do
      add(pad(cut(row[1], 27), 28) .. lpad(n(row[2]), 7) .. lpad(n(row[3]), 9) .. lpad(n(row[4]), 9))
    end
    add("")
    add("# By day")
    add(pad("day", 12) .. lpad("calls", 7) .. lpad("in", 9) .. lpad("out", 9))
    for _, row in ipairs(r.days) do
      add(pad(row[1], 12) .. lpad(n(row[2]), 7) .. lpad(n(row[3]), 9) .. lpad(n(row[4]), 9))
    end
    add("")
    add("# Busiest sessions")
    for _, row in ipairs(r.sessions) do
      add(lpad(n(row[2]), 7) .. " in  " .. lpad(n(row[3]), 6) .. " out  " .. cut(row[1], 50))
    end
  end
  add("")
  add("# Tools")
  if #r.tools == 0 then
    add("No tool calls in this period.")
  else
    add(pad("tool", 24) .. lpad("calls", 7) .. lpad("failed", 8) .. lpad("rate", 7) .. lpad("avg out", 9))
    for _, row in ipairs(r.tools) do
      local calls, failed = tonumber(row[2]) or 0, tonumber(row[3]) or 0
      local rate = calls > 0 and string.format("%.1f%%", failed * 100 / calls) or "-"
      add(pad(cut(row[1], 23), 24) .. lpad(n(calls), 7) .. lpad(n(failed), 8) .. lpad(rate, 7)
        .. lpad(n(row[4]), 9))
    end
  end
  return table.concat(out, "\n")
end

local function show(period)
  local pager = bone.ui.pager("loading…", { title = "stats" })
  local from = since(period)
  local results, pending, failed = {}, 0, nil
  for name, sql in pairs(QUERIES) do
    pending = pending + 1
    bone.request("store/query", { sql = sql, params = { from } }, function(res, err)
      pending = pending - 1
      if err then
        failed = failed or (type(err) == "table" and err.message or tostring(err))
      else
        results[name] = res.rows or {}
      end
      if pending == 0 then
        pager:set(failed and ("stats: " .. failed) or render(period, results))
      end
    end)
  end
end

local function show_usage(c)
  local period = c.args ~= "" and c.args or "month"
  if not PERIODS[period] then
    return bone.notify("stats: use today, week, month or all", "error")
  end
  show(period)
end

bone.cmd.create("usage", show_usage, {
  desc = "tokens, sessions and tool calls; /usage today|week|month|all",
  complete = function()
    return {
      { value = "today", desc = "since midnight" },
      { value = "week", desc = "the last 7 days" },
      { value = "month", desc = "the last 30 days" },
      { value = "all", desc = "everything recorded" },
    }
  end,
})
bone.cmd.create("stats", show_usage, { desc = "alias for /usage" })
