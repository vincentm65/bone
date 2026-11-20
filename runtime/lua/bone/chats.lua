-- The conversations and their background work, as data for a sessions
-- sidebar to draw: every frontend's sidebar uses this, and draws it its own
-- way (runtime/lua/bone/ui/sessions.lua for the TUI). It tracks which
-- sessions run (turn events), when each was last touched, which are
-- pinned or archived (shared by every frontend), which section each is in
-- (M.section), the background shells, and fetches the session list and usage.
-- It also knows what each chat needs from you (M.state): a question open in
-- it, or a turn that ended (or failed) while you were elsewhere. M.list and
-- M.status are the list itself as both sidebars draw it: section headers
-- (with counts, foldable, the fold shared by every frontend) and one line
-- per chat, its title and a status word (Working, Waiting, Done, Failed) or
-- its time.
-- Views subscribe with M.watch(fn); fn(kind, ev) runs after each change:
-- "turn" (ev.running), "created", "updated" (a pin or an archive too),
-- "deleted", "shells", "attention", "folded".
local M = {}

local agents = require("bone.agents")
M.live, M.touched, M.versions, M.shells = {}, {}, {}, {}
-- Questions waiting for an answer: ask id -> the chat it is in (false: none).
-- Any core-side `bone.ask` counts (a tool's approval, ask_user, a plugin's
-- own prompt): what asked is never looked at.
M.asks = {}
-- Chats whose turn ended while another chat was on screen.
M.unread = {}
-- Of those, the ones whose turn failed.
M.failed = {}
-- When each running turn started (os.time()), for the sidebar's live clock;
-- unknown for one already running when this frontend started.
M.started = {}

-- Their colors, for a colorscheme without its own (graphite has them).
bone.hl.set("SessionWaiting", { link = "WarningMsg", default = true })
bone.hl.set("SessionUnread", { link = "Accent", default = true })
bone.hl.set("SessionRunning", { link = "ToolRunning", default = true })
-- The list's status words (M.status).
bone.hl.set("SessionWorking", { link = "Accent", default = true })
bone.hl.set("SessionDone", { link = "Success", default = true })
bone.hl.set("SessionFailed", { link = "ErrorMsg", default = true })

-- Pinned and archived chats, by id: the core keeps them with each session
-- (`session/set`), so every frontend shows the same.
M.pinned, M.archived = {}, {}
local function note(s)
  M.pinned[s.session_id], M.archived[s.session_id] = s.pinned or nil, s.archived or nil
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

--- A time as "now", "5m ago", "Oct 03" (M.ago for the list's shorter form).
function M.timestamp(at)
  if at <= 0 then return "" end
  local age = math.max(0, os.time() - at)
  if age < 60 then return "now" end
  if age < 3600 then return math.floor(age / 60) .. "m ago" end
  if age < 86400 then return math.floor(age / 3600) .. "h ago" end
  if age < 7 * 86400 then return math.floor(age / 86400) .. "d ago" end
  return os.date(os.date("%Y", at) == os.date("%Y") and "%b %d" or "%b %d %Y", at)
end

--- A time as the list shows it: "now", "5m", "3h", "2d", "Oct 03".
function M.ago(at)
  return (M.timestamp(at or 0):gsub(" ago$", ""))
end

--- How long a running turn has gone, as the sidebars' live clock: "0:47",
--- "14:05", "1:02:30".
function M.clock(seconds)
  seconds = math.max(0, math.floor(seconds or 0))
  local h, m, s = math.floor(seconds / 3600), math.floor(seconds % 3600 / 60), seconds % 60
  if h > 0 then return ("%d:%02d:%02d"):format(h, m, s) end
  return ("%d:%02d"):format(m, s)
end

--- How long a turn has run, as the list shows it: "" under a minute, then
--- "4m", "1h 5m".
function M.elapsed(seconds)
  seconds = math.max(0, seconds or 0)
  if seconds < 60 then return "" end
  if seconds < 3600 then return math.floor(seconds / 60) .. "m" end
  local h, m = math.floor(seconds / 3600), math.floor(seconds % 3600 / 60)
  return m > 0 and (h .. "h " .. m .. "m") or (h .. "h")
end

--- When session `s` last did something (stats from M.fetch_stats).
function M.activity(s, stats)
  local meta = stats and stats[s.session_id]
  return math.max(M.touched[s.session_id] or 0, meta and meta.at or s.created_at or 0)
end

--- Archive session `id` (default: the one on screen): it leaves
--- Active until it runs again.
function M.archive(id)
  id = id or (bone.chat.session() or {}).session_id
  if id then bone.request("session/set", { session_id = id, archived = true }) end
end

--- Whether a chat in section `n` can be archived: one in Active or
--- Running (pinned chats never are; archived ones already are).
function M.archivable(n)
  return n == 2 or n == 3
end

--- Rename chat `id` to `title` (trimmed; an empty one is ignored), then
--- done() once the core has it.
function M.rename(id, title, done)
  title = (title or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if title == "" then return end
  bone.request("session/set", { session_id = id, title = title }, function(_, err)
    if err then bone.notify("rename: " .. tostring(err), "error") end
    if done then done() end
  end)
end

--- Pin chat `id`, or unpin it (`on` false): pinned chats are listed first,
--- in every frontend, and are never archived.
function M.pin(id, on)
  if id then bone.request("session/set", { session_id = id, pinned = on and true or false }) end
end

--- Pin chat `id` if it is not, else unpin it.
function M.toggle_pin(id)
  M.pin(id, not M.pinned[id])
end

-- The sections of the list, in order.
M.sections = { "Pinned", "Active", "Running", "Archived" }

-- Folded unless chosen otherwise: the archive, which is long.
local FOLDED = { [4] = true }

--- Whether section `n` (an index into M.sections) is folded: its header
--- stays, its chats are hidden. Every frontend shares it (the setting
--- `chats.folded`).
function M.is_folded(n)
  local on = (bone.settings.get("chats.folded") or {})[tostring(n)]
  if on == nil then return FOLDED[n] or false end
  return on == true
end

--- Fold section `n`, or unfold it.
function M.toggle_folded(n)
  local folded = bone.settings.get("chats.folded") or {}
  folded[tostring(n)] = not M.is_folded(n)
  bone.settings.set("chats.folded", folded)
end

--- Which section chat `id` belongs in (an index into M.sections), with
--- `at` its last activity: pinned; a question waiting for you, or finished
--- within M.recent_window() and not archived (Active); a turn
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
      -- Its turn began before this frontend saw it: count the clock from
      -- the moment we noticed, rather than show no time at all.
      M.started[s.session_id] = M.started[s.session_id] or os.time()
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
      note(s)
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
      if M.versions[id] == before[id] then
        M.live[id] = true
        M.started[id] = M.started[id] or os.time()
      end
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
--- question is open in it), "running", "failed" (its turn failed while you
--- were in another chat), "unread" (it finished while you were elsewhere),
--- or nil.
function M.state(id)
  if not id then return nil end
  if M.waiting(id) then return "waiting" end
  if M.live[id] then return "running" end
  if id ~= on_screen() then
    if M.failed[id] then return "failed" end
    if M.unread[id] then return "unread" end
  end
  return nil
end

-- The word for each state, and its color.
local WORDS = {
  waiting = { "Waiting", "SessionWaiting" },
  failed = { "Failed", "SessionFailed" },
  unread = { "Done", "SessionDone" },
}

--- What the list shows at the right of chat `id` (last active at `at`):
--- { text, hl, state }. A state (M.state) is a word in its color:
--- a live clock ("2:34", SessionWorking) while it runs, then "Waiting",
--- "Failed", "Done"; with none, the time (M.ago), in Dim.
function M.status(id, at)
  local state = M.state(id)
  if state == "running" then
    local t = M.started[id] and M.clock(os.time() - M.started[id]) or "Working"
    return { state = state, hl = "SessionWorking", text = t }
  end
  local word = WORDS[state]
  if word then return { state = state, text = word[1], hl = word[2] } end
  return { text = M.ago(at), hl = "Dim" }
end

--- The Chats list as rows, the same in every sidebar: before each
--- section's chats its header, { kind = "header", section, count, folded },
--- then { kind = "chat", session, section, at } for each chat. A folded
--- section keeps its header (and its count) and shows only the chat on
--- screen; a search (`query`) unfolds them all. `keep(s)`, if given,
--- leaves out the chats it is false for (they are not counted). Also
--- returns M.grouped's answer.
function M.list(all, query, stats, keep)
  local g = M.grouped(all, query, stats)
  local current, searching = on_screen(), (query or "") ~= ""
  local rows, head = {}, nil
  for _, s in ipairs(g.items) do
    if not keep or keep(s) then
      local id = s.session_id
      local section = g.groups[id] or 4
      if not head or head.section ~= section then
        head = { kind = "header", section = section, count = 0,
          folded = not searching and M.is_folded(section) }
        rows[#rows + 1] = head
      end
      head.count = head.count + 1
      if not head.folded or id == current then
        rows[#rows + 1] = { kind = "chat", session = s, section = section, at = g.activities[id] or 0 }
      end
    end
  end
  return rows, g
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
  if id and (M.unread[id] or M.failed[id]) then
    M.unread[id], M.failed[id] = nil, nil
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
  M.started[id] = running and os.time() or nil
  if running then M.failed[id] = nil end
  -- A turn that ends in a chat you are not looking at is new to you (an
  -- agent's ends in its own chat, which you did not start), and says so
  -- when it failed.
  if not running and id ~= on_screen() and not agents.get(id) then
    M.unread[id] = true
    local outcome = type(ev.outcome) == "table" and ev.outcome.status
    M.failed[id] = outcome == "failed" or nil
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
bone.on("session/changed", function(ev)
  note(ev)
  changed("updated", ev)
end)
bone.on("settings/changed", function(ev)
  if ev.path == "chats.folded" then changed("folded", {}) end
end)
bone.on("session/deleted", function(ev)
  M.live[ev.session_id], M.touched[ev.session_id] = nil, nil
  M.unread[ev.session_id], M.failed[ev.session_id], M.started[ev.session_id] = nil, nil, nil
  for ask_id, chat in pairs(M.asks) do
    if chat == ev.session_id then M.asks[ask_id] = nil end
  end
  for id, proc in pairs(M.shells) do
    if proc.session_id == ev.session_id then M.shells[id] = nil end
  end
  changed("deleted", ev)
end)

return M
