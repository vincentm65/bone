-- Sub-agents: sessions started by another session's tool call (an owner).
-- One record per agent, kept from the core's events for the tray, the
-- sidebar, the statusline and the chat:
--   { key, id, name, title, parent, cwd, status, detail, tokens,
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
    title = info.title or "sub-agent",
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
    n = n + a.tokens
  end
  return n
end

bone.on("session/created", function(info)
  local a = M.add(info)
  if a then
    a.status, a.started_at = "running", bone.now()
  end
end)

bone.on("turn/started", function(ev)
  local a = agents[ev.session_id]
  if a and a.status ~= "running" then
    a.status, a.finished_at, a.started_at, a.detail = "running", nil, bone.now(), nil
  end
end)

bone.on("message/delta", function(ev)
  local a = agents[ev.session_id]
  -- A step label, not the streamed text: the row changes per step, not per token.
  if a then
    a.detail = ev.kind == "reasoning" and "thinking" or "writing"
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
end)

bone.on("session/deleted", function(ev)
  agents[ev.session_id] = nil
end)

return M
