-- Conversation sidebar with a Processes tab for every chat's background work.
-- The data (what runs, archived, stats, shells) is bone.chats, shared with
-- other frontends' sidebars; this draws it for the TUI.
local M = {}
local panel, state
local chats = require("bone.chats")
local live, touched = chats.live, chats.touched
local empty_row = {}
local recent_window, recent_label = chats.recent_window, chats.recent_label
local count, timestamp = chats.count, chats.timestamp

local function setup_colors()
  -- Links follow live theme changes, including custom style modifiers.
  local selection = bone.hl.get("Selection") or {}
  local background = bone.ui.sessions_background or "#0b0b0b"
  bone.hl.set("SessionBackground", { link = "Normal", bg = background })
  bone.hl.set("SessionDim", { link = "Dim", bg = background })
  bone.hl.set("SessionAccent", { link = "Accent", bg = background, bold = true })
  -- The selected card: an accent title on the selection background.
  bone.hl.set("SessionCardTitle", { link = "Accent", bg = selection.bg, bold = true })
  bone.hl.set("SessionCardMeta", { link = "Dim", bg = selection.bg })
  bone.hl.set("SessionTab", { link = "Normal", bg = background, bold = true })
  bone.hl.set("SessionTabLine", { link = "Accent", bg = background })
  bone.hl.set("SessionCount", { link = "WarningMsg", bg = background })
end

function M.archive_current(id)
  chats.archive(id)
end
local jobs = chats.jobs

local function filter_running()
  local key = state.items[state.selected] and state.items[state.selected].key
  local titles = {}
  for _, s in ipairs(state.all) do titles[s.session_id] = s.title end
  for _, s in ipairs(bone.chat.sessions()) do
    if s.session_id then titles[s.session_id] = titles[s.session_id] or s.title end
  end
  state.filter_minute, state.filter_window, state.filter_dirty = math.floor(os.time() / 60), recent_window(), false
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
  state.filter_minute, state.filter_window, state.filter_dirty = math.floor(os.time() / 60), recent_window(), false
  local g = chats.grouped(state.all, state.query, state.stats)
  state.items = g.items
  local groups, activities, sizes, running = g.groups, g.activities, g.sizes, g.running
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
  state.selected = index
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
  if j then chats.stop(j) end
end

local function move(delta)
  state.initial = nil
  state.selected = math.max(1, math.min(#state.items, state.selected + delta))
  reveal()
end

local function render(ctx)
  -- After a drag, save the width drawn: Rust keeps the chat 20 columns, and
  -- the release can arrive before the last drag has been drawn.
  if state.save_width then
    state.save_width = nil
    bone.state.save("sessions-width", { width = ctx.width })
  end
  -- Include starting turns and background chats already known by this TUI.
  if chats.note_running() then
    state.filter_dirty = true
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
  chats.fetch_stats(function(stats)
    if state ~= st or not p:is_open() then return end
    st.stats_loading = false
    if stats then
      for id, v in pairs(stats) do st.stats[id] = v end
      filter()
    end
  end)
end

local function refresh_list(st, p)
  chats.fetch_list(function(all, err)
    if state ~= st or not p:is_open() then return end
    st.loading = false
    if err then
      bone.notify("cannot list sessions: " .. tostring(err), "error")
      filter()
      return
    end
    -- Subagents stay in their owner's tray, not in the conversation sidebar.
    st.all = all
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
  chats.fetch_shells(function()
    if state == st and p:is_open() then filter() end
  end)
  chats.fetch_active(function()
    if state == st and p:is_open() then filter() end
  end)
end

chats.watch(function(kind, ev)
  local open = panel and panel:is_open()
  if kind == "archived" then
    if state then state.filter_dirty = true end
  elseif kind == "turn" then
    if open then
      filter()
      if ev.running then refresh_list(state, panel) end
      -- The new user message is already persisted before turn/started.
      -- Refresh on both edges so running conversations aren't one turn behind.
      refresh_stats(state, panel)
    end
  elseif kind == "shells" then
    if open and state.page == "running" then state.filter_dirty = true end
  elseif open then
    refresh_list(state, panel)
  end
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
      state.save_width = true
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
    -- "Chats" spans columns 2-6 and "Processes" 10-18: split in the gap.
    if (ev.x - info.col >= 9 and "running" or "chats") ~= state.page then switch_page() end
  end
  return true
end)

return M
