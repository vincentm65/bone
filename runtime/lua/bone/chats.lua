-- The conversations and their background work, as data for a sessions
-- sidebar to draw: every frontend's sidebar uses this, and draws it its own
-- way (runtime/lua/bone/ui/sessions.lua for the TUI). It tracks which
-- sessions run (turn events), when each was last touched, which are
-- pinned or archived (shared by every frontend), which section each is in
-- (M.section), the background shells, and fetches the session list and usage.
-- It also knows what each chat needs from you (M.state): a question open in
-- it, or a turn that ended while you were elsewhere.
-- Views subscribe with M.watch(fn); fn(kind, ev) runs after each change:
-- "turn" (ev.running), "created", "updated", "deleted", "shells", "attention",
-- "archived", "pinned".
local M = {}

local agents = require("bone.agents")
M.live, M.touched, M.versions, M.shells = {}, {}, {}, {}
-- Questions waiting for an answer: ask id -> the chat it is in (false: none).
-- Any core-side `bone.ask` counts (a tool's approval, ask_user, a plugin's
-- own prompt): what asked is never looked at.
M.asks = {}
-- Chats whose turn ended while another chat was on screen.
M.unread = {}

-- Their colors, for a colorscheme without its own (graphite has them).
bone.hl.set("SessionWaiting", { link = "WarningMsg", default = true })
bone.hl.set("SessionUnread", { link = "Accent", default = true })
bone.hl.set("SessionRunning", { link = "ToolRunning", default = true })

-- Pinned and archived chats are kept where every frontend on this machine
-- reads them (state/shared/), so the TUI and the desktop agree. Each side
-- once kept its own (`old`): it is copied over once (sessions-moved says
-- so) and left as it was, for an older bone that still reads it.
local moved = bone.state.load("sessions-moved")
local function shared(name, old)
  local ok, set = pcall(bone.state.load, name, { shared = true })
  set = ok and type(set) == "table" and set or {}
  if old and not moved[name] then
    local had = bone.state.load(old)
    for id, on in pairs(type(had) == "table" and had or {}) do set[id] = set[id] or on end
    bone.state.save(name, set, { shared = true })
    moved[name] = true
    bone.state.save("sessions-moved", moved)
  end
  return set
end
M.archived = shared("sessions-archived", "sessions-archived")
M.pinned = shared("sessions-pinned", "sidebar-pinned")

-- Read them again: the other frontend may have changed them.
local function reload()
  M.archived = shared("sessions-archived")
  M.pinned = shared("sessions-pinned")
end

-- Change one chat in a shared set, reading it first so the other
-- frontend's changes are kept.
local function mark(name, id, on)
  reload()
  local set = name == "sessions-pinned" and M.pinned or M.archived
  set[id] = on or nil
  bone.state.save(name, set, { shared = true })
end
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

--- How long a finished chat waits for review before it is archived, in
--- seconds: bone.ui.sessions_archive_after (the older name,
--- bone.ui.sessions_recent_seconds, still works), an hour by default. Read
--- each time, so a change applies at once.
function M.recent_window()
  return math.max(0, tonumber(bone.ui.sessions_archive_after)
    or tonumber(bone.ui.sessions_recent_seconds) or 3600)
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

--- Archive session `id` (default: the one on screen): it leaves
--- Pending review until it runs again.
function M.archive(id)
  id = id or (bone.chat.session() or {}).session_id
  if not id then return end
  mark("sessions-archived", id, true)
  changed("archived", { session_id = id })
end

--- Pin chat `id`, or unpin it (`on` false): pinned chats are listed first,
--- in every frontend, and are never archived.
function M.pin(id, on)
  if not id then return end
  mark("sessions-pinned", id, on)
  changed("pinned", { session_id = id })
end

--- Pin chat `id` if it is not, else unpin it.
function M.toggle_pin(id)
  M.pin(id, not M.pinned[id])
end

-- The sections of the list, in order.
M.sections = { "Pinned", "Pending review", "Running", "Archived" }

--- Which section chat `id` belongs in (an index into M.sections), with
--- `at` its last activity: pinned; a question waiting for you, or finished
--- within M.recent_window() and not archived (Pending review); a turn
--- running; or the rest (Archived).
function M.section(id, at)
  if M.pinned[id] then return 1 end
  if M.waiting(id) then return 2 end
  if M.live[id] then return 3 end
  if not M.archived[id] and at >= os.time() - M.recent_window() then return 2 end
  return 4
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
--- { items, groups, activities, sizes, running }: items sorted by section
--- (M.section: groups[id] and sizes are indexes into M.sections), newest
--- first in each.
function M.grouped(all, query, stats)
  reload()
  local items, groups, activities, sizes = {}, {}, {}, { 0, 0, 0, 0 }
  local running = false
  query = (query or ""):lower()
  for _, s in ipairs(all) do
    if ((s.title or "[untitled]") .. " " .. (s.cwd or "")):lower():find(query, 1, true) then
      local sid, at = s.session_id, M.activity(s, stats)
      local g = M.section(sid, at)
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

-- ---- what a chat needs from you ---------------------------------------------

local function on_screen()
  return (bone.chat.session() or {}).session_id
end

--- Whether chat `id` (or one of its agents) has a question waiting for you.
--- Without `id`: whether any chat does.
function M.waiting(id)
  for _, chat in pairs(M.asks) do
    if id == nil or chat == id then return true end
  end
  return false
end

--- What chat `id` needs from you, the most pressing first: "waiting" (a
--- question is open in it), "running", "unread" (it finished while you were
--- in another chat), or nil.
function M.state(id)
  if not id then return nil end
  if M.waiting(id) then return "waiting" end
  if M.live[id] then return "running" end
  if M.unread[id] and id ~= on_screen() then return "unread" end
  return nil
end

--- The chats other than the one on screen that need you: { waiting = n,
--- unread = n } (a chat counts once, as waiting if it is both).
function M.elsewhere()
  local current, seen, out = on_screen(), {}, { waiting = 0, unread = 0 }
  for _, chat in pairs(M.asks) do
    if chat and chat ~= current and not seen[chat] then
      seen[chat] = true
      out.waiting = out.waiting + 1
    end
  end
  for id in pairs(M.unread) do
    if id ~= current and not seen[id] and not M.live[id] then out.unread = out.unread + 1 end
  end
  return out
end

--- Chat `id` has been looked at: it is no longer new.
function M.seen(id)
  if id and M.unread[id] then
    M.unread[id] = nil
    changed("attention", { session_id = id })
  end
end

-- A question from a sub-agent belongs to the chat that started it.
local function chat_of(ev)
  return ev.session_id and agents.top(ev.session_id) or false
end

-- ask/list's answer can cross events: a question answered while it was on
-- its way is not brought back, and one asked meanwhile is kept.
local answered, fresh = {}, {}

-- What waits now, from the core: on load, and after a reconnect (whose
-- questions may have been answered or dropped while this one was away).
local function fetch_asks()
  answered, fresh = {}, {}
  bone.request("ask/list", {}, function(list, err)
    if err then return end
    local asks = {}
    for _, ev in ipairs(list or {}) do
      if type(ev) == "table" and ev.ask_id ~= nil and not answered[ev.ask_id] then
        asks[ev.ask_id] = chat_of(ev)
      end
    end
    for ask_id in pairs(fresh) do asks[ask_id] = M.asks[ask_id] end
    M.asks = asks
    changed("attention", {})
  end)
end

bone.on("ask/requested", function(ev)
  if type(ev) ~= "table" or ev.ask_id == nil or answered[ev.ask_id] then return end
  M.asks[ev.ask_id], fresh[ev.ask_id] = chat_of(ev), true
  changed("attention", ev)
end)
bone.on("ask/resolved", function(ev)
  if type(ev) ~= "table" or ev.ask_id == nil then return end
  answered[ev.ask_id], fresh[ev.ask_id] = true, nil
  M.asks[ev.ask_id] = nil
  changed("attention", ev)
end)
bone.on("server/reconnected", fetch_asks)
bone.on("chat/shown", function(ev) M.seen(ev and ev.session_id) end)
fetch_asks()

local function turn_changed(ev, running)
  local id = ev.session_id
  if not id then return end
  M.versions[id] = (M.versions[id] or 0) + 1
  M.live[id], M.touched[id] = running, os.time()
  if running and M.archived[id] then mark("sessions-archived", id, false) end
  -- A turn that ends in a chat you are not looking at is new to you (an
  -- agent's ends in its own chat, which you did not start).
  if not running and id ~= on_screen() and not agents.get(id) then M.unread[id] = true end
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
  M.unread[ev.session_id] = nil
  for ask_id, chat in pairs(M.asks) do
    if chat == ev.session_id then M.asks[ask_id] = nil end
  end
  for id, proc in pairs(M.shells) do
    if proc.session_id == ev.session_id then M.shells[id] = nil end
  end
  changed("deleted", ev)
end)

return M
