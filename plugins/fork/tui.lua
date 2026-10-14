local alive, serial, window, pending = true, 0, nil, nil
local function notify(err) bone.notify("fork: " .. tostring(err), "error") end
local function current() return (bone.api.session() or {}).session_id end
local function close()
  if window then bone.ui.close(window); window = nil end
end
bone.plugin.on_shutdown(function() alive = false; pending = nil; close() end)

-- open_session loads asynchronously. Prefill only once the intended chat is
-- shown, never the old source chat or an unrelated chat the user switched to.
local function prefill()
  if not alive or not pending or current() ~= pending.id then return end
  local p = pending
  pending = nil
  if not bone.prompt.empty() then
    return notify("draft changed while loading; kept your edits instead of restoring the selected message")
  end
  local ok, err = pcall(function()
    if bone.prompt and bone.prompt.set_images then bone.prompt.set_images(p.draft.images or {}) end
    bone.api.prompt_set(p.draft.text or "")
  end)
  if not ok then notify("draft restoration failed; original text/images remain in the source session: " .. tostring(err)) end
end
bone.on("chat/shown", prefill)
local function create(id, n, tree, valid, done)
  local draft = n and tree.points[n]
  if draft and #(draft.images or {}) > 0 and not (bone.prompt and bone.prompt.set_images) then
    if done then done(false) end
    return notify("this TUI cannot restore image attachments; fork was not created")
  end
  bone.rpc.call("fork.create", { session_id = id, before_turn = n, expected = tree.expected }, function(r, err)
    if not valid() then return end
    if err or not r then if done then done(false) end; return notify(err or "no result") end
    if r.warning then notify(r.warning) end
    close()
    pending = r.draft and { id = r.info.session_id, draft = r.draft } or nil
    bone.api.open_session(r.info.session_id)
    prefill()
  end)
end

local function popup(origin, token)
  local st = { selected = 1, top = 1, page = 12, rows = {}, expanded = {}, cache = {},
    infos = {}, edges = {}, loading = {}, status = "Loading family…", busy = false }
  local function valid() return alive and serial == token and window and bone.ui.is_open(window) and current() == origin end
  local rebuild, fetch
  rebuild = function()
    local selected = st.rows[st.selected]
    local key = selected and selected.key
    local root, seen = origin, {}
    while st.edges[root] and not seen[root] do seen[root] = true; root = st.edges[root].parent end
    local children = {}
    for id, e in pairs(st.edges) do
      children[e.parent] = children[e.parent] or {}
      children[e.parent][#children[e.parent] + 1] = id
    end
    for _, ids in pairs(children) do table.sort(ids) end
    st.rows, seen = {}, {}
    local function visit(id, depth)
      if seen[id] then return end
      seen[id] = true
      st.rows[#st.rows + 1] = { key = id, id = id, depth = depth, parent = st.edges[id] and st.edges[id].parent }
      if not st.expanded[id] then return end
      local tree = st.cache[id]
      for _, p in ipairs(tree and tree.points or {}) do
        -- A child copied the prefix immediately before this user point.
        for _, child in ipairs(children[id] or {}) do
          if st.edges[child].before_turn == p.before_turn then visit(child, depth + 1) end
        end
        st.rows[#st.rows + 1] = { key = id .. ":" .. p.before_turn, id = id, depth = depth + 1, point = p, parent = id }
      end
      -- Whole copies, unknown/legacy cutoffs and unloaded/missing parents.
      for _, child in ipairs(children[id] or {}) do visit(child, depth + 1) end
    end
    visit(root, 0)
    for i, row in ipairs(st.rows) do if row.key == key then st.selected = i end end
    st.selected = math.max(1, math.min(st.selected, #st.rows))
  end
  fetch = function(id, sessions, initial)
    if st.loading[id] then return end
    st.loading[id] = true
    bone.rpc.call("fork.tree", { session_id = id, sessions = sessions }, function(r, err)
      if not valid() then return end
      st.loading[id] = nil
      if err or not r then st.status = "Cannot load branch"; return notify(err or "no result") end
      st.status, st.cache[id] = "", r
      for sid, e in pairs(r.edges or {}) do st.edges[sid] = e end
      for _, info in ipairs(r.sessions or {}) do
        if not st.infos[info.session_id] then st.infos[info.session_id] = info end
      end
      st.infos[id] = r.snapshot.info
      if initial then
        local ancestor, seen = origin, {}
        while ancestor and not seen[ancestor] do
          seen[ancestor], st.expanded[ancestor] = true, true
          ancestor = st.edges[ancestor] and st.edges[ancestor].parent
        end
      end
      rebuild()
      if initial then for i, row in ipairs(st.rows) do if row.key == origin then st.selected = i end end end
    end)
  end
  local function move(n) st.selected = math.max(1, math.min(#st.rows, st.selected + n)) end
  local function right()
    local row = st.rows[st.selected]
    if not row or row.point then return end
    st.expanded[row.id] = true
    rebuild()
    if not st.cache[row.id] and (st.infos[row.id] or {}).cwd then fetch(row.id) end
  end
  local function left()
    local row = st.rows[st.selected]
    if not row then return end
    if not row.point and st.expanded[row.id] then st.expanded[row.id] = nil; rebuild(); return end
    for i, r in ipairs(st.rows) do if r.key == row.parent then st.selected = i; return end end
  end
  local function enter()
    if st.busy then return end
    local row = st.rows[st.selected]
    if not row then return end
    if not row.point then
      if not (st.infos[row.id] or {}).cwd then return notify("ancestor session is missing") end
      close(); bone.api.open_session(row.id); return
    end
    if not row.point.safe then return notify("unsafe steering cutoff: unresolved tool calls") end
    st.busy = true
    create(row.id, row.point.before_turn, st.cache[row.id], valid, function() st.busy = false end)
  end
  close()
  window = bone.ui.popup{
    width = 84, height = 22, anchor = "screen", guard = 100,
    lines = function(ctx)
      local width = math.max(8, math.min(84, ctx.width))
      st.page = math.max(1, math.min(18, ctx.height - 4))
      st.top = math.max(1, math.min(st.top, st.selected))
      if st.selected >= st.top + st.page then st.top = st.selected - st.page + 1 end
      local lines = {}
      for i = st.top, math.min(#st.rows, st.top + st.page - 1) do
        local row, label = st.rows[i]
        if row.point then
          local p = row.point
          label = "↳ " .. p.before_turn .. "  " .. (p.text ~= "" and p.text or "[image message]"):gsub("%s+", " ")
          if #(p.images or {}) > 0 then label = label .. "  [" .. #p.images .. " image(s)]" end
          if not p.safe then label = label .. " [unsafe cutoff]" end
        else
          local info, e = st.infos[row.id] or {}, st.edges[row.id]
          label = (st.expanded[row.id] and "▾ " or "▸ ") .. (info.title or row.id)
          if row.id == origin then label = label .. "  ●" end
          if e then label = label .. (e.before_turn and ("  ← before #" .. e.before_turn) or e.legacy and "  ← legacy (unknown cutoff)" or "  ← copy") end
          if st.loading[row.id] then label = label .. " …" end
          if not info.cwd then label = label .. " [missing]" end
        end
        local text = string.rep("  ", math.min(row.depth, 12)) .. label
        text = bone.text.truncate(text, width - 4)
        lines[#lines + 1] = { { text, i == st.selected and "Selection" or row.point and "Dim" or "Normal" } }
      end
      if #lines == 0 then lines[1] = { { st.status, "Dim" } } end
      lines[#lines + 1] = { { st.busy and "Creating…" or "↑↓/wheel move · ←→ fold · Enter open/fork · Esc close", "Dim" } }
      return bone.ui.box(lines, { title = "Fork · session family", width = width })
    end,
    keys = { esc = close, q = close, up = function() move(-1) end, down = function() move(1) end,
      wheelup = function() move(-3) end, wheeldown = function() move(3) end,
      pageup = function() move(-st.page) end, pagedown = function() move(st.page) end,
      home = function() move(-#st.rows) end, ["end"] = function() move(#st.rows) end,
      left = left, right = right, enter = enter },
  }
  bone.request("session/list", {}, function(list, err)
    if not valid() then return end
    if err then st.status = "Cannot list sessions"; return notify(err) end
    for _, info in ipairs(list or {}) do st.infos[info.session_id] = info end
    fetch(origin, list, true)
  end)
end

bone.cmd.create("fork", function(c)
  local origin = current()
  if not origin then return notify("no current session") end
  local arg = (c.args or ""):match("^%s*(.-)%s*$")
  if arg ~= "" and arg ~= "copy" and not arg:match("^[1-9]%d*$") then return notify("usage: /fork [N|copy]") end
  serial = serial + 1
  local token = serial
  close(); pending = nil
  if arg == "" then return popup(origin, token) end
  local function valid() return alive and serial == token and current() == origin end
  bone.rpc.call("fork.tree", { session_id = origin }, function(tree, err)
    if not valid() then return end
    if err or not tree then return notify(err or "no result") end
    create(origin, arg ~= "copy" and tonumber(arg) or nil, tree, valid)
  end)
end, { desc = "Browse forks, fork before user message N, or copy the whole session" })
