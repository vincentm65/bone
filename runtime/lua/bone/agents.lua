-- Sub-agents: sessions started by another session's tool call (an owner).
-- One record per agent, kept from the core's events for the tray, the
-- sidebar, the statusline and the chat:
--   { key, id, name, title, parent, cwd, status, detail, step, tokens,
--     started_at, finished_at (ms; nil for agents only seen in a list) }
local M = {}

-- session id -> agent; tool call id -> session id.
local agents, by_call = {}, {}

-- Learn an agent from its session info; a no-op without an owner.
function M.add(info)
  local o = info.owner
  if not o or agents[info.session_id] then
    return agents[info.session_id]
  end
  local a = {
    key = "agent:" .. info.session_id,
    id = info.session_id,
    name = o.name or "agent",
    -- A sub-agent's title is its whole task, often many lines; every view
    -- draws it as one line.
    title = (info.title or "sub-agent"):gsub("%s+", " "):match("^ ?(.-) ?$"),
    parent = o.session_id,
    cwd = info.cwd,
    tokens = 0,
  }
  agents[info.session_id] = a
  if o.call_id then
    by_call[o.call_id] = info.session_id
  end
  return a
end

function M.get(id)
  return agents[id or ""]
end

--- The agent a tool call started, or nil.
function M.for_call(call_id)
  return agents[by_call[call_id or ""] or ""]
end

--- The top-level session `id` works for (itself when it is not an agent).
function M.top(id)
  while agents[id] do
    id = agents[id].parent
  end
  return id
end

--- Agents started this run working for top-level session `top`; without
--- `top`, every agent known, including ones only seen in a session list.
function M.list(top)
  local out = {}
  for id, a in pairs(agents) do
    if top == nil or (a.started_at and M.top(id) == top) then
      out[#out + 1] = a
    end
  end
  return out
end

--- Tokens (input + output) those agents spent this run.
function M.spend(top)
  local n = 0
  for _, a in ipairs(M.list(top)) do
    n = n + a.tokens + (a.extra_tokens or 0)
  end
  return n
end

-- Known at creation, but listed as running only once a turn starts: a
-- child whose start failed would otherwise show as running forever.
bone.on("session/created", function(info)
  local a = M.add(info)
  if a then a.first_turn = 0 end
end)

-- An agent works on after the call that started it returns: its row in the
-- chat reads this record, so a turn starting or ending draws the chat again.
local function redraw()
  if bone.chat and bone.chat.redraw then bone.chat.redraw() end
end

local listed -- the core's sessions were asked for once

--- The agent a tool call started, like for_call; one from before a restart
--- or a reload is not known yet, so the first miss asks the core for its
--- sessions and draws the chat again when they arrive.
function M.lookup(call_id)
  local a = M.for_call(call_id)
  if a or listed then return a end
  listed = true
  bone.request("session/list", {}, function(list)
    for _, s in ipairs(list or {}) do
      if s.owner then M.add(s) end
    end
    redraw()
  end)
end

--- How sub-agent call `item` stands: its agent's state once the agent is
--- known (it works on after the call returns), else the call's own.
--- Returns the agent (or nil) and "running", "failed" or "done".
function M.call_state(item)
  local a = M.lookup(item.id)
  if not item.done or (a and a.status == "running") then return a, "running" end
  if item.is_error or (a and (a.status == "failed" or a.status == "cancelled")) then return a, "failed" end
  return a, "done"
end

bone.on("turn/started", function(ev)
  local a = agents[ev.session_id]
  if a then a.first_turn = a.first_turn or ev.turn_id end
  if a and a.status ~= "running" then
    a.status, a.finished_at, a.started_at, a.detail, a.step = "running", nil, bone.now(), nil, nil
    redraw()
  end
end)

-- step: a word for what it does now (a tool's name, thinking, writing), for
-- one-line rows; the tray's detail says more.
bone.on("tool/started", function(ev)
  local a = agents[ev.session_id]
  if a and ev.call and ev.call.name then a.step = ev.call.name end
end)

bone.on("message/delta", function(ev)
  local a = agents[ev.session_id]
  -- A step label, not the streamed text: the row changes per step, not per token.
  if a then
    a.detail = ev.kind == "reasoning" and "thinking" or "writing"
    a.step = a.detail
  end
end)

-- What each model call cost, summed over the agent's turns.
bone.on("message/completed", function(ev)
  local a, u = agents[ev.session_id], ev.usage
  if a and u then
    a.tokens = a.tokens + (u.input_tokens or 0) + (u.output_tokens or 0)
  end
end)

bone.on("turn/finished", function(ev)
  local a = agents[ev.session_id]
  if not a then
    return
  end
  local o = ev.outcome or {}
  a.status = ({ completed = "done", cancelled = "cancelled" })[o.status] or "failed"
  a.finished_at = bone.now()
  a.detail = o.status == "completed" and "done" or o.message or a.status
  redraw()
end)

-- Each agent's last answer: { key, text } once fetched, keyed by when its
-- turn finished so a later turn fetches again.
local reports = {}

--- Agent `a`'s report: the last answer it wrote, or nil while it works or
--- the answer loads (the chat is drawn again when it arrives). The same
--- fetch sets a.took_ms, how long its last turn ran, for agents from before
--- a restart (no clock of this run).
function M.report(a)
  if not a or a.status == "running" then return nil end
  local key = tostring(a.finished_at or "")
  local r = reports[a.id]
  if r and (r.key == key or r.loading == key) then return r.text end
  reports[a.id] = { loading = key, text = r and r.text }
  bone.request("session/messages", { session_id = a.id, tail = 4 }, function(res)
    local text = ""
    local messages = res and res.messages or {}
    for i = #messages, 1, -1 do
      local m = messages[i]
      if m.role == "user" then break end
      if m.role == "assistant" and type(m.content) == "string" and m.content:match("%S") then
        text = m.content
        break
      end
    end
    local turn
    for n, t in pairs(res and res.turn_times or {}) do
      n = tonumber(n)
      if n and (not turn or n > turn) and type(t) == "table" and t.duration_ms then
        turn, a.took_ms = n, t.duration_ms
      end
    end
    reports[a.id] = { key = key, text = text }
    redraw()
  end)
  return r and r.text
end

-- Non-turn calls inherit the active turn_id, even pre-response compaction.
-- Its first usage row bounds resumed history and later idle (turn_id=0) calls.
for _, event in ipairs({ "session/compacted", "session/compact_failed", "turn/finished" }) do
  bone.on(event, function(ev)
    local a = agents[ev.session_id]
    if not a or not a.first_turn then return end
    bone.request("store/query", {
      sql = "SELECT coalesce(sum(input_tokens + output_tokens), 0) FROM usage WHERE session_id = ?1 AND source <> 'turn' AND rowid >= (SELECT min(rowid) FROM usage WHERE session_id = ?1 AND turn_id >= ?2)",
      params = { a.id, a.first_turn },
    }, function(res)
      local row = res and res.rows and res.rows[1]
      if row and agents[a.id] == a then
        a.extra_tokens = math.max(a.extra_tokens or 0, tonumber(row[1]) or 0)
      end
    end)
  end)
end

bone.on("session/deleted", function(ev)
  for call, id in pairs(by_call) do
    if id == ev.session_id then by_call[call] = nil end
  end
  agents[ev.session_id] = nil
  reports[ev.session_id] = nil
end)

return M
