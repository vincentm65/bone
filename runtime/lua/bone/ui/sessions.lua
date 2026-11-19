-- Conversation sidebar with a Processes tab for every chat's background work.
-- The data (what runs, archived, stats, shells) is bone.chats, shared with
-- other frontends' sidebars; this draws it for the TUI, the way the desktop
-- draws it (bone.chats.list): a header per section with its count (enter
-- or a click folds it; the archive starts folded), one line per chat with
-- its status word or time at the right, and under the selected chat its
-- time, tokens and turns, and at its right what can be done to it, each a
-- click or a key: ☆ pin (ctrl+t), ✎ rename (f2, in its row: enter keeps
-- it, esc drops it), ⤓ archive (ctrl+a).
local M = {}
local panel, state
-- The selected chat's actions (defined below, used by render).
local start_rename, finish_rename, archive
local chats = require("bone.chats")
local empty_row = {}
local recent_window = chats.recent_window
local count, timestamp = chats.count, chats.timestamp

local function setup_colors()
  -- Links follow live theme changes, including custom style modifiers.
  local selection = bone.hl.get("Selection") or {}
  -- The panel follows the input's surface unless it is told otherwise.
  local background = bone.ui.sessions_background or (bone.hl.get("InputBackground") or {}).bg or "#141414"
  bone.hl.set("SessionBackground", { link = "Normal", bg = background })
  bone.hl.set("SessionDim", { link = "Dim", bg = background })
  bone.hl.set("SessionAccent", { link = "Accent", bg = background, bold = true })
  -- The selected card: an accent title on the selection background.
  bone.hl.set("SessionCardTitle", { link = "Accent", bg = selection.bg, bold = true })
  bone.hl.set("SessionCardMeta", { link = "Dim", bg = selection.bg })
  bone.hl.set("SessionTab", { link = "Normal", bg = background, bold = true })
  bone.hl.set("SessionTabLine", { link = "Accent", bg = background })
  bone.hl.set("SessionCount", { link = "WarningMsg", bg = background })
  -- A chat's dot (bone.chats.state), on the panel and on a selected card.
  local function dot(name, link)
    bone.hl.set(name, { link = link, bg = background, bold = true })
    bone.hl.set(name .. "Selected", { link = link, bg = selection.bg, bold = true })
  end
  dot("SessionDotWaiting", "SessionWaiting")
  dot("SessionDotUnread", "SessionUnread")
  dot("SessionDotOff", "Dim")
  -- Section headers, and the titles of chats that need nothing from you.
  bone.hl.set("SessionHeader", { link = "Dim", bg = background, bold = true })
  -- (Its color, not a link: a scheme without Muted has no group to follow,
  -- and gives Normal's color for it; Dim, the step below, stands in.)
  local muted = (bone.hl.get("Muted") or {}).fg
  if muted == (bone.hl.get("Normal") or {}).fg then muted = nil end
  bone.hl.set("SessionQuiet", muted and { fg = muted, bg = background } or { link = "Dim", bg = background })
  -- The status words (bone.chats.status), on the panel and on a selected row.
  for name, link in pairs({ Working = "SessionWorking", Waiting = "SessionWaiting",
      Done = "SessionDone", Failed = "SessionFailed", Time = "Dim" }) do
    bone.hl.set("SessionStatus" .. name, { link = link, bg = background })
    bone.hl.set("SessionStatus" .. name .. "Selected", { link = link, bg = selection.bg })
  end
end

-- bone.chats.status's colors, as this panel's groups.
local STATUS = {
  SessionWorking = "SessionStatusWorking", SessionWaiting = "SessionStatusWaiting",
  SessionDone = "SessionStatusDone", SessionFailed = "SessionStatusFailed", Dim = "SessionStatusTime",
}

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

-- What a row of the Chats page is, to find it again after the list changes:
-- its chat's id, or its section's header.
local function key_of(r)
  if not r then return nil end
  if r.kind == "header" then return "header:" .. r.section end
  return r.session and r.session.session_id
end

-- The Chats page's lines from its rows (state.items): positions are built
-- once, shared by rendering, reveal and hit testing. The selected chat has
-- a second line, so this runs again when the selection moves.
local function layout()
  state.lines, state.hits, state.descriptors, state.rows = {}, {}, {}, {}
  state.visible_rows = {}
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
  -- A rule above each header (after a blank, below a chat), and a blank
  -- line between two chats.
  for i, r in ipairs(state.items) do
    local prev = state.items[i - 1]
    if prev and prev.kind == "chat" then append("blank") end
    if r.kind == "header" then append("separator") end
    local line = append(r.kind == "header" and "header" or "title", i)
    state.lines[i], state.hits[line] = line, i
    if r.kind == "chat" and i == state.selected then
      state.hits[append("meta", i)] = i
    end
  end
end

local function filter(reset)
  if reset then state.initial = nil end
  if state.page == "running" then return filter_running() end
  local key = not reset and key_of(state.items[state.selected]) or (not reset and state.initial)
  -- A header stays selected only where you moved to it.
  if reset then state.picked = false end
  if key and key:match("^header:") and not state.picked then key = state.initial end
  state.filter_minute, state.filter_window, state.filter_dirty = math.floor(os.time() / 60), recent_window(), false
  local rows, g = chats.list(state.all, state.query, state.stats)
  state.items, state.running = rows, g.running
  state.selected = math.min(math.max(1, state.selected), #state.items)
  -- The same row again; else (nothing chosen yet, or it went) the chat on
  -- screen, else the first chat: not a header.
  local found = false
  for i, r in ipairs(rows) do
    if key and key_of(r) == key then state.selected, found = i, true break end
  end
  if not found then
    -- (A new search starts at its first match.)
    local current, first = not reset and (bone.chat.session() or {}).session_id, nil
    for i, r in ipairs(rows) do
      if r.kind == "chat" then
        first = first or i
        if r.session.session_id == current then first = i break end
      end
    end
    state.selected = first or state.selected
  end
  layout()
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
  local r = state.items[index]
  if not r then return end
  state.renaming = nil
  state.selected, state.picked = index, true
  -- A section's header folds it, or unfolds it.
  if r.kind == "header" then
    chats.toggle_folded(r.section)
    return
  end
  if state.page == "chats" then layout() end
  -- Keep the agents panel available while interacting with the chosen chat.
  bone.ui.panel.focus(nil)
  -- A shell opens the chat that started it; a sub-agent, its own chat.
  bone.api.open_session(r.session and r.session.session_id or r.session_id)
end

local function switch_page()
  state.initial, state.renaming = nil, nil
  state.pages[state.page] = { items = state.items, selected = state.selected, top = panel:info().top }
  state.page = state.page == "running" and "chats" or "running"
  local saved = state.pages[state.page] or { items = {}, selected = 1, top = 0 }
  state.items, state.selected = saved.items, saved.selected
  filter()
  panel:scroll(saved.top - panel:info().top)
end

-- The selected row of the Chats page, if it is a chat (not a header).
local function selected_chat()
  local r = state.page == "chats" and state.items[state.selected]
  return r and r.kind == "chat" and r or nil
end

-- Whether chat row `r` can be archived: not pinned (pinned chats never
-- are) and not in Archived already.
local function archivable(r)
  return r.section ~= 1 and r.section ~= 4
end

-- ctrl+t on the Chats page: pin the selected chat, or unpin it.
local function pin()
  local r = selected_chat()
  if r then chats.toggle_pin(r.session.session_id) end
end

-- x on the Processes page: stop the selected shell or sub-agent.
local function stop()
  local j = state.items[state.selected]
  if j then chats.stop(j) end
end

local function move(delta)
  state.initial, state.picked, state.renaming = nil, true, nil
  local was = state.selected
  state.selected = math.max(1, math.min(#state.items, state.selected + delta))
  -- The selected chat's second line moves with it.
  if state.page == "chats" and state.selected ~= was then layout() end
  reveal()
end

local function render(ctx)
  -- With the keys, the blank column at the edge (where a drag resizes it)
  -- shows the lit bar every focused panel has; without them it stays blank.
  if panel and (ctx.focused or false) ~= (state.edge or false) then
    state.edge = ctx.focused or false
    panel:update({ separator = state.edge })
  end
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
  -- A rename ends when the panel loses the keyboard.
  if state.renaming and not ctx.focused then state.renaming = nil end
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
  -- A line of spans: `left` from the margin (its last span cut to fit),
  -- `right` against the far margin, `fill` (a group) between them.
  local function spread(left, right, fill)
    local inner = math.max(0, ctx.width - 4)
    local rw = 0
    for _, span in ipairs(right) do rw = rw + bone.text.width(span[1]) end
    if rw > inner then right, rw = {}, 0 end
    local room = math.max(0, inner - rw - (rw > 0 and 2 or 0))
    local out, used = { { "  ", "Normal" } }, 0
    for _, span in ipairs(left) do
      local text = span[1]:gsub("%s+", " ")
      if bone.text.width(text) > room - used then text = bone.text.truncate(text, math.max(0, room - used)) end
      out[#out + 1] = { text, span[2] }
      used = used + bone.text.width(text)
    end
    out[#out + 1] = { string.rep(" ", math.max(0, inner - used - rw)), fill }
    for _, span in ipairs(right) do out[#out + 1] = span end
    out[#out + 1] = { "  ", "Normal" }
    return out
  end
  -- `mark`: a span before the text, in its own color.
  local function card(text, style, mark)
    local width = math.max(0, ctx.width - 4 - (mark and bone.text.width(mark[1]) or 0))
    text = bone.text.truncate(text:gsub("%s+", " "), width)
    local out = { { "  ", "Normal" } }
    out[#out + 1] = mark
    out[#out + 1] = { text .. string.rep(" ", math.max(0, width - bone.text.width(text))), style }
    out[#out + 1] = { "  ", "Normal" }
    return out
  end
  -- A waiting chat's dot blinks: on for 700 ms, off for 500.
  local blink_on = bone.now() % 1200 < 700
  local blinking = false
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
      formatted = row(state.page == "running" and "  ↵ open · x stop · tab chats · esc prompt" or "  ↵ open · tab processes", "Dim")
    elseif kind == "status" then
      formatted = row(state.page == "running" and " nothing running in the background"
        or state.loading and " loading…" or (state.query == "" and " no sessions yet" or " no matching sessions"), "Dim")
    elseif kind == "chat" then
      formatted = row("  " .. (state.titles[state.items[index].chat] or "[untitled]"), "Dim")
    elseif kind == "job" or kind == "job_meta" then
      local j, active = state.items[index], index == state.selected and ctx.focused
      if kind == "job" then
        formatted = card(j.title, active and "SessionCardTitle" or "Normal",
          { " " .. ctx.spinner .. " ", "SessionStatusWorking" .. (active and "Selected" or "") })
      else
        local detail = j.detail ~= "" and ("  ·  " .. j.detail) or ""
        formatted = card("   " .. bone.util.duration(os.time() - j.started) .. detail, active and "SessionCardMeta" or "Dim")
      end
    elseif kind == "separator" then
      formatted = { { fill = "─", hl = "Dim" } }
    elseif kind == "header" then
      -- "Active 3", and whether it is folded.
      local r = state.items[index]
      local active = index == state.selected and ctx.focused
      local quiet = active and "SessionCardMeta" or "Dim"
      formatted = spread({ { chats.sections[r.section], active and "SessionCardTitle" or "SessionHeader" },
        { " " .. r.count, quiet } }, { { r.folded and "▸" or "▾", quiet } }, quiet)
    elseif kind == "blank" then
      formatted = row("", "Normal")
    else
      local r = state.items[index]
      local s = r.session
      local active = index == state.selected and ctx.focused
      local sel = active and "Selected" or ""
      if kind == "title" and state.renaming and state.renaming.id == s.session_id then
        -- Being renamed: the text typed so far and the caret, its end
        -- kept in view.
        local text, room = state.renaming.text, math.max(1, ctx.width - 5)
        while bone.text.width(text) > room do
          text = text:gsub("^[%z\1-\127\194-\244][\128-\191]*", "", 1)
        end
        local hl = active and "SessionCardTitle" or "Normal"
        formatted = spread({ { text, hl }, { "▏", hl } }, {}, hl)
      elseif kind == "title" then
        -- The title: white, or a step quieter in Archived unless the chat
        -- needs you or is on screen; at the right what it needs
        -- (bone.chats.status), or its time.
        local status = chats.status(s.session_id, r.at)
        local quiet = r.section == 4 and not (status.state or s.session_id == current)
        local hl = active and "SessionCardTitle" or (quiet and "SessionQuiet" or "Normal")
        local right
        if status.state == "running" then
          right = { { ctx.spinner .. " " .. status.text, "SessionStatusWorking" .. sel } }
        elseif status.state == "waiting" then
          -- Its dot blinks.
          blinking = true
          right = { { "● ", (blink_on and "SessionDotWaiting" or "SessionDotOff") .. sel },
            { status.text, "SessionStatusWaiting" .. sel } }
        else
          right = { { status.text, (STATUS[status.hl] or "SessionStatusTime") .. sel } }
        end
        formatted = spread({ { s.title or "[untitled]", hl } }, right, hl)
      else
        local meta = state.stats[s.session_id]
        local totals = meta and (count(meta.tokens) .. " tokens · " .. count(meta.turns) .. " turns")
          or (state.stats_loading and "usage loading…" or "usage unavailable")
        local hl = active and "SessionCardMeta" or "Dim"
        -- At its right, what can be done to it: pin, rename, archive (a
        -- click: state.action_cols, for the mouse). Not while it is being
        -- renamed.
        local icons, right = {}, {}
        if not state.renaming then
          icons = { { r.section == 1 and "★" or "☆", pin }, { "✎", start_rename } }
          if archivable(r) then icons[#icons + 1] = { "⤓", archive } end
        end
        for k, a in ipairs(icons) do
          if k > 1 then right[#right + 1] = { "  ", hl } end
          right[#right + 1] = { a[1], hl }
        end
        formatted = spread({ { "  " .. timestamp(r.at) .. " · " .. totals, hl } }, right, hl)
        -- Each icon's columns (one either side too), from the right margin.
        state.action_cols = {}
        local col = ctx.width - 2
        for k = #icons, 1, -1 do
          local w = bone.text.width(icons[k][1])
          col = col - w
          state.action_cols[k] = { col - 1, col + w, icons[k][2] }
          col = col - 2
        end
        if col < 2 then state.action_cols = {} end
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
    if blinking then
      local t = bone.now() % 1200
      bone.ui.refresh_in(t < 700 and 700 - t or 1200 - t)
    end
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

-- f2: rename the selected chat, in its row.
start_rename = function()
  local r = selected_chat()
  if not r then return end
  state.renaming = { id = r.session.session_id, text = r.session.title or "" }
  -- (A click on it from the prompt: the keys come here for the typing.)
  if bone.ui.panel.focused() ~= "sessions" then panel:focus() end
end

-- enter keeps the new title (`keep`), esc drops it.
finish_rename = function(keep)
  local ren = state.renaming
  state.renaming = nil
  if not (keep and ren) then return end
  local title = ren.text:gsub("^%s+", ""):gsub("%s+$", "")
  if title == "" then return end
  local st, p = state, panel
  bone.request("session/rename", { session_id = ren.id, title = title }, function(_, err)
    if err then bone.notify("rename: " .. tostring(err), "error") end
    if state == st and p:is_open() then refresh_list(st, p) end
  end)
end

-- ctrl+a: archive the selected chat (it leaves Active until it runs again).
archive = function()
  local r = selected_chat()
  -- (Not while its title is being typed.)
  if state.renaming or not (r and archivable(r)) then return end
  -- The selection stays in place: on the next chat, else the one before.
  for _, d in ipairs({ 1, -1 }) do
    local i = state.selected + d
    while state.items[i] and state.items[i].kind ~= "chat" do i = i + d end
    if state.items[i] then
      state.selected = i
      break
    end
  end
  chats.archive(r.session.session_id)
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
      esc = function()
        if state.renaming then return finish_rename(false) end
        bone.ui.panel.focus(nil)
      end,
      ["ctrl+o"] = function() panel:close() end,
      tab = switch_page, ["shift+tab"] = switch_page,
      enter = function()
        if state.renaming then return finish_rename(true) end
        choose(state.selected)
      end,
      ["ctrl+t"] = pin,
      f2 = function() start_rename() end,
      ["ctrl+a"] = function() archive() end,
      up = function() move(-1) end, down = function() move(1) end,
      pageup = function() move(-math.max(1, math.floor(panel:info().height / 3))) end,
      pagedown = function() move(math.max(1, math.floor(panel:info().height / 3))) end,
      home = function() move(-#state.items) end, ["end"] = function() move(#state.items) end,
    },
    on_key = function(key)
      -- Typing goes to the title being renamed.
      if state.renaming then
        local text = bone.text.edit(state.renaming.text, key)
        if text then state.renaming.text = text end
        return text ~= nil
      end
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
  if kind == "archived" or kind == "pinned" or kind == "attention" or kind == "folded" then
    -- Only the sections change (a waiting question moves its chat to
    -- Active; a header folds): nothing to fetch.
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
  -- A click on one of the selected chat's icons runs it; elsewhere on the
  -- line, it opens the chat.
  if (state.descriptors[ev.panel_line] or {})[1] == "meta" then
    local col = ev.x - info.col
    for _, a in ipairs(state.action_cols or {}) do
      if col >= a[1] and col <= a[2] then
        a[3]()
        return true
      end
    end
  end
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
