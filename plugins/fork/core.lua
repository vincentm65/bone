-- Genealogy is plugin metadata; seeded sessions remain ordinary sessions.
local state = bone.state.load("fork")
state.edges = state.edges or {}
local function save() bone.state.save("fork", state) end

-- Stable encoding, independent of JSON object iteration order. Two small hashes
-- are an optimistic stale-view check, not a cryptographic identity.
local function encode(v)
  if type(v) ~= "table" then return type(v) .. ":" .. string.format("%q", tostring(v)) end
  local keys, out = {}, { "{" }
  for k in pairs(v) do keys[#keys + 1] = k end
  table.sort(keys, function(a, b) return type(a) .. tostring(a) < type(b) .. tostring(b) end)
  for _, k in ipairs(keys) do out[#out + 1] = encode(k) .. "=" .. encode(v[k]) .. ";" end
  return table.concat(out) .. "}"
end
local function fingerprint(v)
  local a, b, s = 0, 0, encode(v)
  for i = 1, #s do
    a = (a * 131 + s:byte(i)) % 2147483647
    b = (b * 137 + s:byte(i)) % 2147483629
  end
  return string.format("%08x%08x", a, b)
end
local function expected(s)
  return { generation = s.generation, content = fingerprint({ s.raw_messages or s.messages, s.summary, s.model, s.info.cwd }) }
end
local function snapshot(id)
  assert(type(id) == "string" and id ~= "", "fork: no source session")
  assert(bone.session.snapshot, "fork: this bone version lacks bone.session.snapshot")
  local s = bone.session.snapshot(id)
  assert(s and s.info and s.messages, "fork: source session unavailable")
  if s.info.parent and not state.edges[id] then
    state.edges[id] = { parent = s.info.parent, legacy = true }
    save()
  end
  return s
end
local function pending_step(pending, m)
  if m.role == "assistant" then
    for _, call in ipairs(m.tool_calls or {}) do
      assert(call.id, "fork: tool call without an id")
      assert(not pending[call.id], "fork: duplicate unresolved tool call")
      pending[call.id] = true
    end
  elseif m.role == "tool" then
    local id = m.call_id or m.tool_call_id
    assert(id and pending[id], "fork: orphan tool result in prefix")
    pending[id] = nil
  end
end
local function points(s)
  local out, pending = {}, {}
  for i, m in ipairs(s.messages) do
    if m.role == "user" then
      out[#out + 1] = { before_turn = #out + 1, copied_count = i - 1,
        text = m.content or "", images = m.images, safe = next(pending) == nil }
    end
    pending_step(pending, m)
  end
  return out
end

bone.rpc.register("fork.tree", function(args, ctx)
  args = args or {}
  local id = args.session_id or ctx.session_id
  local s = snapshot(id)
  local known, changed = { [id] = s.info }, false
  -- A TUI may supply the one session/list result. Import old header parents;
  -- the core itself never calls protocol methods.
  for _, info in ipairs(args.sessions or {}) do
    known[info.session_id] = info
    if info.parent and not state.edges[info.session_id] then
      state.edges[info.session_id] = { parent = info.parent, legacy = true }
      changed = true
    end
  end
  if changed then save() end
  local family = { [id] = true }
  repeat
    changed = false
    for child, edge in pairs(state.edges) do
      if family[child] or family[edge.parent] then
        if not family[child] or not family[edge.parent] then changed = true end
        family[child], family[edge.parent] = true, true
      end
    end
  until not changed
  local sessions, edges = {}, {}
  for sid in pairs(family) do
    sessions[#sessions + 1] = known[sid] or { session_id = sid }
    edges[sid] = state.edges[sid]
  end
  table.sort(sessions, function(a, b) return a.session_id < b.session_id end)
  return { session_id = id, sessions = sessions, edges = edges,
    snapshot = { info = s.info }, points = points(s), expected = expected(s) }
end)

bone.rpc.register("fork.create", function(args, ctx)
  args = args or {}
  local id = args.session_id or ctx.session_id
  local s = snapshot(id)
  if args.expected then
    assert(encode(args.expected) == encode(expected(s)), "fork: source changed; reopen /fork")
  end
  local count, draft = #s.messages, nil
  if args.before_turn ~= nil then
    local n = args.before_turn
    assert(type(n) == "number" and n >= 1 and n % 1 == 0, "fork: ordinal must be a positive integer")
    draft = points(s)[n]
    assert(draft, "fork: user ordinal out of range")
    count = draft.copied_count
  else
    assert(not s.active_turn or s.active_turn == false, "fork: wait for the source turn to finish before copying")
  end
  local messages, pending = {}, {}
  for i = 1, count do
    -- Exact JSON strings bypass the lossy JSON↔Lua conversion of nested data.
    messages[i] = s.raw_messages and s.raw_messages[i] or s.messages[i]
    pending_step(pending, s.messages[i])
  end
  assert(not next(pending), "fork: unsafe prefix has unresolved tool calls (possibly a steering message)")
  local summary = s.summary and s.summary.through <= count and s.summary or nil
  local info = bone.session.create({ cwd = s.info.cwd, messages = messages,
    summary = summary, model = s.model })
  local child = assert(info and info.session_id, "fork: session creation failed")
  state.edges[child] = { parent = id, before_turn = args.before_turn,
    prefix = fingerprint(messages) }
  -- Never defer this to shutdown. If saving fails the child still exists.
  local ok, err = pcall(save)
  return { info = info, draft = draft, warning = not ok and ("genealogy save failed: " .. tostring(err)) or nil }
end)
