-- Lua-owned agents/conversation browser. Transcripts load only when chosen.
local M = {}
local panel, state
local live, touched, versions = {}, {}, {}
local empty_row = {}
local archived = bone.state.load("sessions-archived")
-- Set bone.ui.sessions_recent_seconds in tui.lua; read each frame for live changes.
local function recent_window()
  return math.max(0, tonumber(bone.ui.sessions_recent_seconds) or 3600)
end

local function recent_label()
  local seconds = recent_window()
  if seconds >= 86400 then return string.format("%gd", seconds / 86400) end
  if seconds >= 3600 then return string.format("%gh", seconds / 3600) end
  if seconds >= 60 then return string.format("%gm", seconds / 60) end
  return string.format("%gs", seconds)
end

local function count(n)
  n = tonumber(n) or 0
  if n >= 1000000 then return string.format("%.1fM", n / 1000000) end
  if n >= 1000 then return string.format("%.1fk", n / 1000) end
  return tostring(n)
end

local function timestamp(at)
  if at <= 0 then return "" end
  local age = math.max(0, os.time() - at)
  if age < 60 then return "now" end
  if age < 3600 then return math.floor(age / 60) .. "m ago" end
  if age < 86400 then return math.floor(age / 3600) .. "h ago" end
  if age < 7 * 86400 then return math.floor(age / 86400) .. "d ago" end
  return os.date(os.date("%Y", at) == os.date("%Y") and "%b %d" or "%b %d %Y", at)
end

local function setup_colors()
  -- Derive from the theme, so this also works with custom colorschemes.
  local normal, dim, selection = bone.hl.get("Normal") or {}, bone.hl.get("Dim") or {}, bone.hl.get("Selection") or {}
  bone.hl.set("SessionCardTitle", { fg = normal.fg, bg = selection.bg, bold = true })
  bone.hl.set("SessionCardMeta", { fg = dim.fg, bg = selection.bg })
end

local function activity(s)
  local meta = state.stats[s.session_id]
  return math.max(touched[s.session_id] or 0, meta and meta.at or s.created_at or 0)
end

function M.archive_current(id)
  id = id or (bone.chat.session() or {}).session_id
  if not id then return end
  archived[id] = true
  bone.state.save("sessions-archived", archived)
  if state then state.filter_dirty = true end
end
local function filter(reset)
  local selected = not reset and state.items[state.selected]
  local id = selected and selected.session_id
  local now, window = os.time(), recent_window()
  state.filter_minute, state.filter_window, state.filter_dirty = math.floor(now / 60), window, false
  state.items = {}
  local query = state.query:lower()
  local groups, activities, sizes = {}, {}, { 0, 0 }
  local running = false
  for _, s in ipairs(state.all) do
    if ((s.title or "[untitled]") .. " " .. (s.cwd or "")):lower():find(query, 1, true) then
      local sid, at = s.session_id, activity(s)
      local g = not archived[sid] and (live[sid] or at >= now - window) and 1 or 2
      groups[sid], activities[sid] = g, at
      sizes[g] = sizes[g] + 1
      running = running or live[sid]
      state.items[#state.items + 1] = s
    end
  end
  table.sort(state.items, function(a, b)
    local ag, bg = groups[a.session_id], groups[b.session_id]
    if ag ~= bg then return ag < bg end
    local aa, ba = activities[a.session_id], activities[b.session_id]
    if aa ~= ba then return aa > ba end
    return a.session_id < b.session_id
  end)
  state.selected = math.min(math.max(1, state.selected), #state.items)
  -- Build content positions once, shared by rendering, reveal and hit testing.
  state.lines, state.hits, state.descriptors, state.rows = {}, {}, {}, {}
  state.visible_rows = {}
  state.activities, state.sizes, state.running = activities, sizes, running
  local function append(kind, index)
    local line = #state.rows + 1
    state.rows[line] = empty_row
    state.descriptors[line] = { kind, index }
    return line
  end
  append("search")
  append("help")
  if state.loading or #state.items == 0 then append("status") end
  local previous
  for i, s in ipairs(state.items) do
    if s.session_id == id then state.selected = i end
    local g = groups[s.session_id]
    if g ~= previous then
      if previous or g == 1 then append("separator") end
      append("header", g)
      previous = g
    end
    local line = append("title", i)
    append("meta", i)
    append("blank")
    state.lines[i] = line
    state.hits[line], state.hits[line + 1] = i, i
  end
end

local function reveal()
  local info = panel:info()
  local first = state.lines[state.selected]
  if not first then return end
  if first < info.top + 1 then
    panel:scroll(first - info.top - 1)
  elseif first + 1 > info.top + info.height then
    panel:scroll(first + 1 - info.top - info.height)
  end
end

local function choose(index)
  local s = state.items[index]
  if not s then return end
  -- Keep the agents panel available while interacting with the chosen chat.
  bone.ui.panel.focus(nil)
  bone.api.open_session(s.session_id)
end

local function move(delta)
  state.selected = math.max(1, math.min(#state.items, state.selected + delta))
  reveal()
end

local function render(ctx)
  -- Include starting turns and background chats already known by this TUI.
  for _, s in ipairs(bone.chat.sessions()) do
    if s.session_id and s.running and not live[s.session_id] then
      live[s.session_id] = true
      state.filter_dirty = true
    end
  end
  if state.filter_dirty or state.filter_window ~= recent_window()
      or state.filter_minute ~= math.floor(os.time() / 60) then
    filter()
  end
  local function row(text, hl)
    text = bone.text.truncate(text:gsub("%s+", " "), ctx.width)
    return { { text .. string.rep(" ", math.max(0, ctx.width - bone.text.width(text))), hl } }
  end
  local function card(text, style)
    local width = math.max(0, ctx.width - 4)
    text = bone.text.truncate(text:gsub("%s+", " "), width)
    return { { "  ", "Normal" }, { text .. string.rep(" ", math.max(0, width - bone.text.width(text))), style }, { "  ", "Normal" } }
  end
  -- Rust needs the full dense list for scrolling, but only viewport rows need text.
  local out = state.rows
  for _, line in ipairs(state.visible_rows) do out[line] = empty_row end
  state.visible_rows = {}
  local current = (bone.chat.session() or {}).session_id
  -- Rust clamps scrolling after the callback; format that same clamped viewport.
  local top = math.min(ctx.top, math.max(0, #out - ctx.height))
  for line = math.max(1, top + 1), math.min(#out, top + ctx.height) do
    local descriptor = state.descriptors[line]
    local kind, index = descriptor[1], descriptor[2]
    local formatted
    if kind == "search" then
      formatted = row(state.query == "" and "  Search conversations…" or ("  Search: " .. state.query), state.query == "" and "Dim" or "Accent")
    elseif kind == "help" then
      formatted = row("  ↑↓ move · ↵ open · esc close", "Dim")
    elseif kind == "status" then
      formatted = row(state.loading and " loading…" or (state.query == "" and " no sessions yet" or " no matching sessions"), "Dim")
    elseif kind == "separator" then
      formatted = { { fill = "─", hl = "Dim" } }
    elseif kind == "header" then
      local name = index == 1 and ("Recent · last " .. recent_label()) or "History"
      formatted = row("  " .. name .. " · " .. state.sizes[index], "Dim")
    elseif kind == "blank" then
      formatted = row("", "Normal")
    else
      local s = state.items[index]
      local active = index == state.selected and ctx.focused
      if kind == "title" then
        local marker = live[s.session_id] and (" " .. ctx.spinner .. " ")
          or (s.session_id == current and " ● " or "   ")
        formatted = card(marker .. (s.title or "[untitled]"), active and "SessionCardTitle" or "Normal")
      else
        local meta = state.stats[s.session_id]
        local totals = meta and (count(meta.tokens) .. " tokens · " .. count(meta.turns) .. " turns")
          or (state.stats_loading and "usage loading…" or "usage unavailable")
        formatted = card("   " .. timestamp(state.activities[s.session_id]) .. "  ·  " .. totals, active and "SessionCardMeta" or "Dim")
      end
    end
    out[line] = formatted
    state.visible_rows[#state.visible_rows + 1] = line
  end
  -- Animate runs, or refresh idle timestamps and age-based grouping each minute.
  if bone.ui.refresh_in then
    bone.ui.refresh_in(state.running and ((bone.ui.spinner and bone.ui.spinner.interval) or 120) or (60 - os.time() % 60) * 1000)
  end
  return out
end

local function refresh_stats(st, p)
  -- Aggregate separately: joining messages directly to usage multiplies totals.
  bone.request("store/query", { sql = [[
    SELECT s.id, s.updated_at, coalesce(u.tokens, 0), coalesce(m.turns, 0)
    FROM sessions s
    LEFT JOIN (SELECT session_id, sum(input_tokens + output_tokens) AS tokens
               FROM usage GROUP BY session_id) u ON u.session_id = s.id
    LEFT JOIN (SELECT session_id, count(*) AS turns FROM messages
               WHERE role = 'user' GROUP BY session_id) m ON m.session_id = s.id
  ]] }, function(res, err)
    if state ~= st or not p:is_open() then return end
    st.stats_loading = false
    if not err and res then
      for _, r in ipairs(res.rows or {}) do
        st.stats[r[1]] = { at = r[2], tokens = r[3], turns = r[4] }
      end
      filter()
    end
  end)
end

local function refresh_list(st, p)
  bone.request("session/list", {}, function(list, err)
    if state ~= st or not p:is_open() then return end
    st.loading = false
    if err then
      bone.notify("cannot list sessions: " .. tostring(err), "error")
      filter()
      return
    end
    -- Subagents stay in their owner's tray, not in the conversation sidebar.
    st.all = {}
    for _, s in ipairs(list or {}) do
      if not s.owner then st.all[#st.all + 1] = s end
    end
    filter()
  end)
end

function M.open()
  if panel and panel:is_open() then
    if bone.ui.panel.focused() == "sessions" then panel:close() else panel:focus() end
    return
  end
  state = { all = {}, items = {}, stats = {}, lines = {}, hits = {}, selected = 1, query = "", loading = true, stats_loading = true }
  local st = state
  setup_colors()
  panel = bone.ui.panel.open({
    id = "sessions", dock = "left", size = 48, full_height = true, focus = true,
    render = render,
    keys = {
      esc = function() panel:close() end,
      ["ctrl+o"] = function() panel:close() end,
      enter = function() choose(state.selected) end,
      up = function() move(-1) end, down = function() move(1) end,
      ["ctrl+p"] = function() move(-1) end, ["ctrl+n"] = function() move(1) end,
      pageup = function() move(-math.max(1, math.floor(panel:info().height / 3))) end,
      pagedown = function() move(math.max(1, math.floor(panel:info().height / 3))) end,
      home = function() move(-#state.items) end, ["end"] = function() move(#state.items) end,
    },
    on_key = function(key)
      if key == "backspace" then
        state.query = state.query:gsub("[%z\1-\127\194-\244][\128-\191]*$", "")
      elseif key == "ctrl+u" then
        state.query = ""
      elseif key == "space" then
        state.query = state.query .. " "
      elseif key:match("^[%z\1-\127\194-\244][\128-\191]*$") then
        state.query = state.query .. key
      else
        return false
      end
      state.selected = 1
      filter(true)
      panel:scroll("top")
      return true
    end,
  })
  local p = panel
  refresh_list(st, p)
  refresh_stats(st, p)
  local before = {}
  for id, version in pairs(versions) do before[id] = version end
  bone.request("session/active", {}, function(ids, err)
    if state ~= st or not p:is_open() or err then return end
    local active = {}
    for _, id in ipairs(ids or {}) do active[id] = true end
    -- Don't overwrite events received while the lookup was in flight.
    for id in pairs(live) do
      if versions[id] == before[id] then live[id] = active[id] end
    end
    for id in pairs(active) do
      if versions[id] == before[id] then live[id] = true end
    end
    filter()
  end)
end

local function turn_changed(ev, running)
  local id = ev.session_id
  if not id then return end
  versions[id] = (versions[id] or 0) + 1
  live[id], touched[id] = running, os.time()
  if running and archived[id] then
    archived[id] = nil
    bone.state.save("sessions-archived", archived)
  end
  if panel and panel:is_open() then
    filter()
    if running then refresh_list(state, panel) end
    -- The new user message is already persisted before turn/started.
    -- Refresh on both edges so running conversations aren't one turn behind.
    refresh_stats(state, panel)
  end
end
bone.on("turn/started", function(ev) turn_changed(ev, true) end)
bone.on("turn/finished", function(ev) turn_changed(ev, false) end)
bone.on("session/created", function()
  if panel and panel:is_open() then refresh_list(state, panel) end
end)
bone.on("session/updated", function()
  if panel and panel:is_open() then refresh_list(state, panel) end
end)
bone.on("session/deleted", function(ev)
  live[ev.session_id], touched[ev.session_id] = nil, nil
  if panel and panel:is_open() then refresh_list(state, panel) end
end)

bone.on("mouse", function(ev)
  if not panel or not panel:is_open() or ev.panel ~= "sessions" then return end
  if ev.button == "left" and ev.action == "down" then
    local index = state.hits[ev.panel_line]
    if index then choose(index) end
    return true
  end
end)

return M
