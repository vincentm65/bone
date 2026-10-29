-- The conversations and their background work, as data for a sessions
-- sidebar to draw: every frontend's sidebar uses this, and draws it its own
-- way (runtime/lua/bone/ui/sessions.lua for the TUI). It tracks which
-- sessions run (turn events), when each was last touched, which are
-- archived, the background shells, and fetches the session list and usage.
-- Views subscribe with M.watch(fn); fn(kind, ev) runs after each change:
-- "turn" (ev.running), "created", "updated", "deleted", "shells".
local M = {}

local agents = require("bone.agents")
M.live, M.touched, M.versions, M.shells = {}, {}, {}, {}
M.archived = bone.state.load("sessions-archived")
local watchers = {}

--- Call fn(kind, ev) after each change; returns a function that stops it.
function M.watch(fn)
  watchers[#watchers + 1] = fn
  return function()
    for i, w in ipairs(watchers) do
      if w == fn then table.remove(watchers, i) return end
    end
  end
end

local function changed(kind, ev)
  for _, fn in ipairs(watchers) do fn(kind, ev) end
end

--- How recent a conversation is to count as recent, in seconds
--- (bone.ui.sessions_recent_seconds; read each time for live changes).
function M.recent_window()
  return math.max(0, tonumber(bone.ui.sessions_recent_seconds) or 3600)
end

--- That window as "1h", "30m", "2d".
function M.recent_label()
  local seconds = M.recent_window()
  if seconds >= 86400 then return string.format("%gd", seconds / 86400) end
  if seconds >= 3600 then return string.format("%gh", seconds / 3600) end
  if seconds >= 60 then return string.format("%gm", seconds / 60) end
  return string.format("%gs", seconds)
end

--- 1234 as "1.2k".
function M.count(n)
  n = tonumber(n) or 0
  if n >= 1000000 then return string.format("%.1fM", n / 1000000) end
  if n >= 1000 then return string.format("%.1fk", n / 1000) end
  return tostring(n)
end

--- A time as "now", "5m ago", "Oct 03".
function M.timestamp(at)
  if at <= 0 then return "" end
  local age = math.max(0, os.time() - at)
  if age < 60 then return "now" end
  if age < 3600 then return math.floor(age / 60) .. "m ago" end
  if age < 86400 then return math.floor(age / 3600) .. "h ago" end
  if age < 7 * 86400 then return math.floor(age / 86400) .. "d ago" end
  return os.date(os.date("%Y", at) == os.date("%Y") and "%b %d" or "%b %d %Y", at)
end

--- When session `s` last did something (stats from M.fetch_stats).
function M.activity(s, stats)
  local meta = stats and stats[s.session_id]
  return math.max(M.touched[s.session_id] or 0, meta and meta.at or s.created_at or 0)
end

--- Archive session `id` (default: the one on screen): it leaves the
--- recent group until it runs again.
function M.archive(id)
  id = id or (bone.chat.session() or {}).session_id
  if not id then return end
  M.archived[id] = true
  bone.state.save("sessions-archived", M.archived)
  changed("archived", { session_id = id })
end

--- Sessions with a turn running that this frontend already knows of;
--- returns whether that found new ones.
function M.note_running()
  local found = false
  for _, s in ipairs(bone.chat.sessions()) do
    if s.session_id and s.running and not M.live[s.session_id] then
      M.live[s.session_id] = true
      found = true
    end
  end
  return found
end

--- The background work of every chat: live shells and running sub-agents,
--- each { key, session_id, chat, title, detail, started, shell? }, by chat
--- then start.
function M.jobs()
  local out = {}
  for _, p in pairs(M.shells) do
    out[#out + 1] = { key = "shell:" .. p.id, shell = p, session_id = p.session_id,
      title = "$ " .. (p.command or p.id), detail = p.tail or "", started = math.floor((p.started_at_ms or 0) / 1000) }
  end
  for _, a in ipairs(agents.list()) do
    local id = a.id
    if M.live[id] then
      M.touched[id] = M.touched[id] or os.time()
      out[#out + 1] = { key = "agent:" .. id, session_id = id, title = a.name .. " · " .. (a.title or "sub-agent"),
        detail = "running", started = M.touched[id] }
    end
  end
  for _, j in ipairs(out) do j.chat = agents.top(j.session_id) end
  table.sort(out, function(a, b)
    if a.chat ~= b.chat then return a.chat < b.chat end
    if a.started ~= b.started then return a.started < b.started end
    return a.key < b.key
  end)
  return out
end

--- The sessions in `all` that match `query` (title or folder), as
--- { items, groups, activities, sizes, running }: items sorted recent
--- (group 1) before history (group 2), newest first.
function M.grouped(all, query, stats)
  local now, window = os.time(), M.recent_window()
  local items, groups, activities, sizes = {}, {}, {}, { 0, 0 }
  local running = false
  query = (query or ""):lower()
  for _, s in ipairs(all) do
    if ((s.title or "[untitled]") .. " " .. (s.cwd or "")):lower():find(query, 1, true) then
      local sid, at = s.session_id, M.activity(s, stats)
      local g = not M.archived[sid] and (M.live[sid] or at >= now - window) and 1 or 2
      groups[sid], activities[sid] = g, at
      sizes[g] = sizes[g] + 1
      running = running or M.live[sid]
      items[#items + 1] = s
    end
  end
  table.sort(items, function(a, b)
    local ag, bg = groups[a.session_id], groups[b.session_id]
    if ag ~= bg then return ag < bg end
    local aa, ba = activities[a.session_id], activities[b.session_id]
    if aa ~= ba then return aa > ba end
    return a.session_id < b.session_id
  end)
  return { items = items, groups = groups, activities = activities, sizes = sizes, running = running }
end

--- The top-level sessions (sub-agents go to bone.agents): done(list, err).
function M.fetch_list(done)
  bone.request("session/list", {}, function(list, err)
    if err then return done(nil, err) end
    local all = {}
    for _, s in ipairs(list or {}) do
      if s.owner then
        agents.add(s)
      else
        all[#all + 1] = s
      end
    end
    done(all)
  end)
end

--- Each session's last activity, tokens (its agents' included), turns and
--- last-used model: done({ [id] = { at, tokens, turns, model } } or nil).
function M.fetch_stats(done)
  -- Aggregate separately: joining messages directly to usage multiplies totals.
  bone.request("store/query", { sql = [[
    SELECT s.id, s.updated_at, coalesce(u.tokens, 0), coalesce(m.turns, 0),
           (SELECT model FROM usage WHERE session_id = s.id ORDER BY at DESC, rowid DESC LIMIT 1)
    FROM sessions s
    LEFT JOIN (SELECT coalesce(a.root, a.id) AS chat, sum(input_tokens + output_tokens) AS tokens
               FROM usage JOIN sessions a ON a.id = usage.session_id GROUP BY chat) u ON u.chat = s.id
    LEFT JOIN (SELECT session_id, count(*) AS turns FROM messages
               WHERE role = 'user' GROUP BY session_id) m ON m.session_id = s.id
  ]] }, function(res, err)
    if err or not res then return done(nil) end
    local stats = {}
    for _, r in ipairs(res.rows or {}) do
      stats[r[1]] = { at = r[2], tokens = r[3], turns = r[4], model = r[5] }
    end
    done(stats)
  end)
end

--- Every chat's live background shells, then done().
function M.fetch_shells(done)
  bone.request("processes/list", {}, function(res, err)
    if err or not res then return end
    M.shells = {}
    for _, proc in ipairs(res.processes or {}) do
      if proc.running and proc.terminal then M.shells[proc.id] = proc end
    end
    done()
  end)
end

--- Which sessions run now (the core's answer, unless an event came while
--- it was asked), then done().
function M.fetch_active(done)
  local before = {}
  for id, version in pairs(M.versions) do before[id] = version end
  bone.request("session/active", {}, function(ids, err)
    if err then return end
    local active = {}
    for _, id in ipairs(ids or {}) do active[id] = true end
    -- Don't overwrite events received while the lookup was in flight.
    for id in pairs(M.live) do
      if M.versions[id] == before[id] then M.live[id] = active[id] end
    end
    for id in pairs(active) do
      if M.versions[id] == before[id] then M.live[id] = true end
    end
    done()
  end)
end

--- Stop a job from M.jobs: a shell, or a sub-agent's turn.
function M.stop(j)
  if j.shell then
    bone.request("process/cancel", { session_id = j.session_id, id = j.shell.id })
  else
    bone.request("turn/cancel", { session_id = j.session_id })
  end
end

local function turn_changed(ev, running)
  local id = ev.session_id
  if not id then return end
  M.versions[id] = (M.versions[id] or 0) + 1
  M.live[id], M.touched[id] = running, os.time()
  if running and M.archived[id] then
    M.archived[id] = nil
    bone.state.save("sessions-archived", M.archived)
  end
  changed("turn", { session_id = id, running = running })
end
bone.on("turn/started", function(ev) turn_changed(ev, true) end)
bone.on("turn/finished", function(ev) turn_changed(ev, false) end)
bone.on("session/created", function(ev) changed("created", ev) end)
-- Background shells run in pseudo-terminals; foreground ones are tool calls.
bone.on("process/changed", function(ev)
  local proc = ev.process or {}
  if proc.running and proc.terminal then
    M.shells[proc.id] = proc
  elseif not M.shells[proc.id] then
    return
  else
    M.shells[proc.id] = nil
  end
  changed("shells", ev)
end)
bone.on("session/updated", function(ev) changed("updated", ev) end)
bone.on("session/deleted", function(ev)
  M.live[ev.session_id], M.touched[ev.session_id] = nil, nil
  for id, proc in pairs(M.shells) do
    if proc.session_id == ev.session_id then M.shells[id] = nil end
  end
  changed("deleted", ev)
end)

return M
