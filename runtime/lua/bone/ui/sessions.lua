-- Conversation sidebar with a Processes tab for every chat's background work.
local M = {}
local panel, state
local live, touched, versions = {}, {}, {}
local shells = {}
local agents = require("bone.agents")
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
  local accent, warn = bone.hl.get("Accent") or {}, bone.hl.get("WarningMsg") or {}
  local background = bone.ui.sessions_background or "#202020"
  bone.hl.set("SessionBackground", { fg = normal.fg, bg = background })
  bone.hl.set("SessionDim", { fg = dim.fg, bg = background })
  bone.hl.set("SessionAccent", { fg = accent.fg, bg = background, bold = true })
  -- The selected card: an accent title on the selection background.
  bone.hl.set("SessionCardTitle", { fg = accent.fg, bg = selection.bg, bold = true })
  bone.hl.set("SessionCardMeta", { fg = dim.fg, bg = selection.bg })
  bone.hl.set("SessionTab", { fg = normal.fg, bg = background, bold = true })
  bone.hl.set("SessionTabLine", { fg = accent.fg, bg = background })
  bone.hl.set("SessionCount", { fg = warn.fg or accent.fg, bg = background })
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
local function jobs()
  local out = {}
  for _, p in pairs(shells) do
    out[#out + 1] = { key = "shell:" .. p.id, shell = p, session_id = p.session_id,
      title = "$ " .. (p.command or p.id), detail = p.tail or "", started = math.floor((p.started_at_ms or 0) / 1000) }
  end
  for _, a in ipairs(agents.list()) do
    local id = a.id
    if live[id] then
      touched[id] = touched[id] or os.time()
      out[#out + 1] = { key = "agent:" .. id, session_id = id, title = a.name .. " · " .. (a.title or "sub-agent"),
        detail = "running", started = touched[id] }
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

local function filter_running()
  local key = state.items[state.selected] and state.items[state.selected].key
  local titles = {}
  for _, s in ipairs(state.all) do titles[s.session_id] = s.title end
  for _, s in ipairs(bone.chat.sessions()) do
    if s.session_id then titles[s.session_id] = titles[s.session_id] or s.title end
  end
  state.filter_dirty = false
  state.items = jobs()
  state.lines, state.hits, state.descriptors, state.rows, state.visible_rows = {}, {}, {}, {}, {}
  state.running, state.titles = #state.items > 0, titles
  local function append(kind, index)
    state.rows[#state.rows + 1] = empty_row
    state.descriptors[#state.rows] = { kind, index }
    return #state.rows
  end
  append("blank")
  append("tabs")
  append("underline")
  append("blank")
  append("help")
  if #state.items == 0 then append("status") end
  local previous
  for i, j in ipairs(state.items) do
    if j.key == key then state.selected = i end
    if j.chat ~= previous then
      if previous then append("separator") end
      append("chat", i)
      previous = j.chat
    end
    local line = append("job", i)
    append("job_meta", i)
    append("blank")
    state.lines[i], state.hits[line], state.hits[line + 1] = line, i, i
  end
  state.selected = math.min(math.max(1, state.selected), #state.items)
end

local function filter(reset)
  if reset then state.initial = nil end
  if state.page == "running" then return filter_running() end
  local selected = not reset and state.items[state.selected]
  local id = selected and selected.session_id or (not reset and state.initial)
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
  append("blank")
  append("tabs")
  append("underline")
  append("blank")
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

local function reveal(info)
  info = info or panel:info()
  local first = state.lines[state.selected]
  if not first or not info.height or info.height == 0 then return end
  local top = info.top
  if first < top + 1 then
    top = first - 1
  elseif first + 1 > top + info.height then
    top = first + 1 - info.height
  end
  panel:scroll(top - info.top)
  info.top = top
end

local function choose(index)
  local s = state.items[index]
  if not s then return end
  -- Keep the agents panel available while interacting with the chosen chat.
  bone.ui.panel.focus(nil)
  -- A shell opens the chat that started it; a sub-agent, its own chat.
  bone.api.open_session(s.session_id)
end

local function switch_page()
  state.initial = nil
  state.pages[state.page] = { items = state.items, selected = state.selected, top = panel:info().top }
  state.page = state.page == "running" and "chats" or "running"
  local saved = state.pages[state.page] or { items = {}, selected = 1, top = 0 }
  state.items, state.selected = saved.items, saved.selected
  filter()
  panel:scroll(saved.top - panel:info().top)
end

-- x on the Processes page: stop the selected shell or sub-agent.
local function stop()
  local j = state.items[state.selected]
  if not j then return end
  if j.shell then
    bone.request("process/cancel", { session_id = j.session_id, id = j.shell.id })
  else
    bone.request("turn/cancel", { session_id = j.session_id })
  end
end

local function move(delta)
  state.initial = nil
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
  -- Wait for both list ordering and viewport geometry, then reveal only once.
  if state.initial and state.page == "chats" and not state.loading
      and not state.stats_loading and ctx.height > 0 then
    state.initial = nil
    reveal(ctx)
  end
  local function row(text, hl)
    -- Collapse whitespace inside the text, but keep the indent.
    text = bone.text.truncate(text:gsub("(%S)%s+", "%1 "), ctx.width)
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
  local styles = { Normal = "SessionBackground", Dim = "SessionDim", Accent = "SessionAccent" }
  for line = math.max(1, top + 1), math.min(#out, top + ctx.height) do
    local descriptor = state.descriptors[line]
    local kind, index = descriptor[1], descriptor[2]
    local formatted
    if kind == "tabs" then
      local n = state.page == "running" and #state.items or #jobs()
      formatted = {
        { "  ", "Normal" },
        { "Chats", state.page == "chats" and "SessionTab" or "Dim" },
        { "   ", "Normal" },
        { "Processes", state.page == "running" and "SessionTab" or "Dim" },
        { n > 0 and (" " .. n) or "", "SessionCount" },
        { fill = " ", hl = "Normal" },
      }
    elseif kind == "underline" then
      -- Under the active tab's label: "Chats" at column 2, "Processes" at 10.
      formatted = row(state.page == "running" and ("          " .. string.rep("─", 9)) or ("  " .. string.rep("─", 5)), "SessionTabLine")
    elseif kind == "search" then
      formatted = row("  ⌕ " .. (state.query == "" and "search" or state.query), state.query == "" and "Dim" or "Accent")
    elseif kind == "help" then
      formatted = row(state.page == "running" and "  ↵ open · x stop · tab chats · esc prompt" or "  ↵ open · tab processes · esc prompt", "Dim")
    elseif kind == "status" then
      formatted = row(state.page == "running" and " nothing running in the background"
        or state.loading and " loading…" or (state.query == "" and " no sessions yet" or " no matching sessions"), "Dim")
    elseif kind == "chat" then
      formatted = row("  " .. (state.titles[state.items[index].chat] or "[untitled]"), "Dim")
    elseif kind == "job" or kind == "job_meta" then
      local j, active = state.items[index], index == state.selected and ctx.focused
      if kind == "job" then
        formatted = card(" " .. ctx.spinner .. " " .. j.title, active and "SessionCardTitle" or "Normal")
      else
        local detail = j.detail ~= "" and ("  ·  " .. j.detail) or ""
        formatted = card("   " .. bone.util.duration(os.time() - j.started) .. detail, active and "SessionCardMeta" or "Dim")
      end
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
    for _, span in ipairs(formatted) do
      if span.hl then span.hl = styles[span.hl] or span.hl
      else span[2] = styles[span[2]] or span[2] end
    end
    out[line] = formatted
    state.visible_rows[#state.visible_rows + 1] = line
  end
  -- Animate runs, or refresh idle timestamps and age-based grouping each minute.
  if bone.ui.refresh_in then
    bone.ui.refresh_in(state.running and ((bone.ui.spinner and bone.ui.spinner.interval) or 120) or (60 - os.time() % 60) * 1000)
  end
  -- Fill unused viewport rows without adding padding to the scrollable content.
  if #out < ctx.height then
    local padded = {}
    for i = 1, ctx.height do padded[i] = out[i] or row("", "SessionBackground") end
    return padded
  end
  return out
end

local function refresh_stats(st, p)
  -- Aggregate separately: joining messages directly to usage multiplies totals.
  -- A chat's tokens include its agents' (sessions.root).
  bone.request("store/query", { sql = [[
    SELECT s.id, s.updated_at, coalesce(u.tokens, 0), coalesce(m.turns, 0)
    FROM sessions s
    LEFT JOIN (SELECT coalesce(a.root, a.id) AS chat, sum(input_tokens + output_tokens) AS tokens
               FROM usage JOIN sessions a ON a.id = usage.session_id GROUP BY chat) u ON u.chat = s.id
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
      if s.owner then
        agents.add(s)
      else
        st.all[#st.all + 1] = s
      end
    end
    filter()
  end)
end

function M.open()
  if panel and panel:is_open() then
    if bone.ui.panel.focused() == "sessions" then panel:close() else panel:focus() end
    return
  end
  state = { all = {}, items = {}, stats = {}, lines = {}, hits = {}, selected = 1, query = "", loading = true, stats_loading = true, page = "chats", pages = {}, initial = (bone.chat.session() or {}).session_id }
  local st = state
  setup_colors()
  panel = bone.ui.panel.open({
    id = "sessions", dock = "left", full_height = true, separator = false, focus = true,
    size = tonumber(bone.state.load("sessions-width").width) or tonumber(bone.ui.sessions_width) or 48,
    render = render,
    keys = {
      esc = function() bone.ui.panel.focus(nil) end,
      ["ctrl+o"] = function() panel:close() end,
      tab = switch_page, ["shift+tab"] = switch_page,
      enter = function() choose(state.selected) end,
      up = function() move(-1) end, down = function() move(1) end,
      pageup = function() move(-math.max(1, math.floor(panel:info().height / 3))) end,
      pagedown = function() move(math.max(1, math.floor(panel:info().height / 3))) end,
      home = function() move(-#state.items) end, ["end"] = function() move(#state.items) end,
    },
    on_key = function(key)
      if state.page == "running" then
        if key == "x" then stop() return true end
        return false
      elseif bone.text.edit(state.query, key) then
        state.query = bone.text.edit(state.query, key)
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
  bone.request("processes/list", {}, function(res, err)
    if err or not res then return end
    shells = {}
    for _, proc in ipairs(res.processes or {}) do
      if proc.running and proc.terminal then shells[proc.id] = proc end
    end
    if state == st and p:is_open() then filter() end
  end)
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
local function running_changed()
  if panel and panel:is_open() and state.page == "running" then state.filter_dirty = true end
end
bone.on("session/created", function()
  if panel and panel:is_open() then refresh_list(state, panel) end
end)
-- Background shells run in pseudo-terminals; foreground ones are tool calls.
bone.on("process/changed", function(ev)
  local proc = ev.process or {}
  if proc.running and proc.terminal then
    shells[proc.id] = proc
  elseif not shells[proc.id] then
    return
  else
    shells[proc.id] = nil
  end
  running_changed()
end)
bone.on("session/updated", function()
  if panel and panel:is_open() then refresh_list(state, panel) end
end)
bone.on("session/deleted", function(ev)
  live[ev.session_id], touched[ev.session_id] = nil, nil
  for id, proc in pairs(shells) do
    if proc.session_id == ev.session_id then shells[id] = nil end
  end
  if panel and panel:is_open() then refresh_list(state, panel) end
end)

bone.on("mouse", function(ev)
  if not panel or not panel:is_open() then return end
  local info = panel:info()
  if not info.visible then return end
  if state.resizing then
    if ev.action == "drag" then
      panel:update({ size = math.max(20, ev.x - info.col) })
    else
      state.resizing = nil
      bone.state.save("sessions-width", { width = info.size })
    end
    return true
  end
  if ev.popup or ev.action ~= "down" or ev.button ~= "left" then return end
  state.resizing = ev.x == info.col + info.width
  if state.resizing then return true end
  if ev.panel ~= "sessions" then return end
  state.initial = nil
  local index = state.hits[ev.panel_line]
  if index then
    choose(index)
  elseif ({ tabs = true, underline = true })[(state.descriptors[ev.panel_line] or {})[1]] then
    switch_page()
  end
  return true
end)

return M
