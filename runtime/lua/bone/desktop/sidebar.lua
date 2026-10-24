-- The sessions sidebar (ctrl+o, /sessions): a drawer on the left, drawn as
-- nodes (bone.nodes.window docks `M.node()`). A Chats / Processes switch
-- with a new-chat button beside it, a project filter and a search field,
-- then your conversations: pinned ones first, the rest in date groups
-- (Today, Yesterday, This week, Earlier), a compact row each (title and
-- when, then folder and tokens) with a dot for what it needs from you
-- (running, waiting for an approval, finished while you were elsewhere).
-- Over a row, pin and rename take the place of its time. The Processes page lists every chat's background work. The
-- data is bone.chats. The rows are made once when the data changes, and the
-- list makes nodes only for the ones in view, so a long history costs
-- nothing. It takes the keyboard as it opens (its search field, or the
-- drawer itself on the Processes page): up/down choose, enter opens, typing
-- searches, tab changes page, x stops a process, esc goes back to the
-- prompt, ctrl+o closes; a click opens.
local M = {}

local chats = require("bone.chats")
local nodes = require("bone.nodes")
local ID = "sessions"
local SEARCH = "sidebar-search"
M.width = 280

local ROW = { group = 30, card = 48, none = 40 }

local open = false
local st = {
  page = "chats", query = "", sel = 1, all = {}, stats = {}, loading = false,
  rows = {}, items = {}, version = 0,
  project = nil, -- the folder the list is narrowed to, or nil for all
  menu = false, -- the project menu is open
  hover = nil, -- the row under the pointer (a session id)
  renaming = nil, -- the session being renamed
}

-- What each chat needs from you, by session id.
local waiting = {} -- ask id -> session id
local unread = {}
local pinned = bone.state.load("sidebar-pinned") or {}

local function redraw()
  bone.gui.invalidate()
end

-- A highlight group, or `fallback` when the colorscheme does not set it.
local function group(name, fallback)
  local g = bone.hl.get(name)
  return (g and g.fg) and name or fallback
end

-- "Today", "Yesterday", "This week" or "Earlier" for a time.
local function date_group(at)
  local today = os.date("*t")
  local midnight = os.time({ year = today.year, month = today.month, day = today.day, hour = 0 })
  if at >= midnight then return "Today" end
  if at >= midnight - 86400 then return "Yesterday" end
  if at >= midnight - 6 * 86400 then return "This week" end
  return "Earlier"
end

local function folder(cwd)
  return (cwd or ""):gsub("/+$", ""):match("([^/]+)$") or "~"
end

local function is_waiting(id)
  for _, sid in pairs(waiting) do
    if sid == id then return true end
  end
  return false
end

--- The rows of the current page, from the data: { kind = "group" | "card" |
--- "none", ... }, and the things the cards stand for (`items`).
local function compute()
  chats.note_running()
  local rows, items = {}, {}
  if st.page == "running" then
    local jobs = chats.jobs()
    if #jobs == 0 then
      rows[1] = { kind = "none", text = "Nothing running in the background" }
    end
    for i, j in ipairs(jobs) do
      items[i] = j
      rows[#rows + 1] = {
        kind = "card", item = i, state = "running", title = j.title,
        meta = j.detail ~= "" and j.detail or "running", time = bone.util.duration(os.time() - j.started),
      }
    end
  else
    local g = chats.grouped(st.all, st.query, st.stats)
    local current = (bone.chat.session() or {}).session_id
    if current then unread[current] = nil end
    for _, s in ipairs(g.items) do
      if not st.project or s.cwd == st.project then items[#items + 1] = s end
    end
    if #items == 0 then
      rows[1] = { kind = "none", text = st.loading and "Loading…"
        or ((st.query == "" and not st.project) and "No chats yet" or "No matching chats") }
    end
    -- Pinned first, then newest first.
    table.sort(items, function(a, b)
      local ap, bp = pinned[a.session_id] and 1 or 0, pinned[b.session_id] and 1 or 0
      if ap ~= bp then return ap > bp end
      local aa, ba = g.activities[a.session_id], g.activities[b.session_id]
      if aa ~= ba then return aa > ba end
      return a.session_id < b.session_id
    end)
    local heading
    for i, s in ipairs(items) do
      local id, at = s.session_id, g.activities[s.session_id]
      local name = pinned[id] and "Pinned" or date_group(at)
      if name ~= heading then
        rows[#rows + 1] = { kind = "group", text = name }
        heading = name
      end
      local state = (is_waiting(id) and "waiting") or (chats.live[id] and "running")
        or (unread[id] and "unread") or nil
      local meta = {}
      if not st.project then meta[#meta + 1] = folder(s.cwd) end
      local stats = st.stats[id]
      if stats and (stats.tokens or 0) > 0 then meta[#meta + 1] = chats.count(stats.tokens) .. " tokens" end
      if stats and (stats.turns or 0) > 0 and st.project then
        meta[#meta + 1] = stats.turns .. (stats.turns == 1 and " turn" or " turns")
      end
      rows[#rows + 1] = {
        kind = "card", item = i, id = id, state = state, pinned = pinned[id],
        title = s.title or "Untitled", meta = table.concat(meta, " · "),
        time = (chats.timestamp(at):gsub(" ago$", "")),
      }
    end
  end
  st.rows, st.items = rows, items
  st.sel = #items == 0 and 0 or math.max(1, math.min(st.sel, #items))
  st.version = st.version + 1
end

-- The folders there are chats in, the most recently used first:
-- { { cwd, name, count } }.
local function projects()
  local seen, out = {}, {}
  local g = chats.grouped(st.all, "", st.stats)
  for _, s in ipairs(g.items) do
    local cwd = s.cwd or ""
    if not seen[cwd] then
      seen[cwd] = { cwd = cwd, name = folder(cwd), count = 0, at = g.activities[s.session_id] or 0 }
      out[#out + 1] = seen[cwd]
    end
    seen[cwd].count = seen[cwd].count + 1
  end
  table.sort(out, function(a, b) return a.at > b.at end)
  return out
end

-- Whether the sidebar has the keyboard.
local function has_keys()
  local f = bone.gui.focused()
  return f == ID or f == SEARCH
end

-- The keyboard to the sidebar: its search field, or itself where there is
-- none.
local function take_keys()
  bone.gui.focus(st.page == "chats" and SEARCH or ID)
end

local function refresh()
  if not open then return end
  chats.fetch_list(function(all)
    if all then st.all = all end
    st.loading = false
    compute()
    redraw()
  end)
  chats.fetch_stats(function(stats)
    if stats then st.stats = stats end
    compute()
    redraw()
  end)
end

local function choose(i)
  local r = st.items[i]
  if not r or not r.session_id then return end
  st.sel = i
  unread[r.session_id] = nil
  bone.gui.focus(nil)
  bone.api.open_session(r.session_id)
  compute()
  redraw()
end

local function start_rename(id)
  st.renaming = id
  bone.gui.focus("sidebar-rename")
  redraw()
end

local function toggle_pin(id)
  pinned[id] = not pinned[id] or nil
  bone.state.save("sidebar-pinned", pinned)
  compute()
  redraw()
end

local function rename(id, title)
  st.renaming = nil
  title = (title or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if title ~= "" then
    bone.request("session/rename", { session_id = id, title = title }, function(_, err)
      if err then bone.notify("rename: " .. tostring(err), "error") end
      refresh()
    end)
  end
  take_keys()
  redraw()
end

-- Bring the selected row into view.
local function reveal()
  local top, h = 0, 0
  for _, r in ipairs(st.rows) do
    if r.kind == "card" and r.item == st.sel then
      h = ROW.card
      break
    end
    top = top + ROW[r.kind]
  end
  local info = bone.gui.scroll_info("sessions-list")
  if not info or h == 0 then return end
  if top < info.offset then
    bone.gui.scroll("sessions-list", top - ROW.group)
  elseif top + h > info.offset + info.viewport then
    bone.gui.scroll("sessions-list", top + h - info.viewport + 8)
  end
end

local function select(i)
  st.sel = math.max(1, math.min(#st.items, i))
  reveal()
  redraw()
end

local function switch_page()
  st.page = st.page == "chats" and "running" or "chats"
  st.sel, st.menu = 1, false
  compute()
  bone.gui.scroll("sessions-list", "top")
  take_keys()
  redraw()
end

local function set_project(cwd)
  st.project, st.menu, st.sel = cwd, false, 1
  compute()
  bone.gui.scroll("sessions-list", "top")
  take_keys()
  redraw()
end

local function close()
  open, st.menu = false, false
  bone.gui.focus(nil)
  redraw()
end

-- Its keys, from the drawer or its search field (which edits with the
-- rest).
local KEYS = {
  esc = function()
    if st.menu then st.menu = false redraw() else bone.gui.focus(nil) end
  end,
  ["ctrl+o"] = close,
  tab = switch_page, ["shift+tab"] = switch_page,
  enter = function() if st.page == "chats" then choose(st.sel) end end,
  up = function() select(st.sel - 1) end, down = function() select(st.sel + 1) end,
  pageup = function() select(st.sel - 5) end, pagedown = function() select(st.sel + 5) end,
}

local function on_key(key)
  if KEYS[key] then
    KEYS[key]()
    return true
  end
  -- x stops a process on the Processes page.
  if st.page == "running" and key == "x" and st.items[st.sel] then
    chats.stop(st.items[st.sel])
    return true
  end
  return false
end

-- ---- nodes ------------------------------------------------------------------

-- A faint tint under the pointer, below the selected row's.
local HOVER = "#ffffff0a"

-- Running dots breathe: the phase flips on a timer while one is shown.
local pulse = false
local pulse_at = 0

local function breathing()
  local now = bone.now()
  if now - pulse_at >= 900 then pulse, pulse_at = not pulse, now end
  bone.gui.invalidate(900)
  return pulse
end

-- The dot for what a chat needs, or nil.
local function dot(state)
  if not state then return nil end
  local hl = ({
    running = group("SessionRunning", "ToolRunning"),
    waiting = group("SessionWaiting", "ErrorMsg"),
    unread = group("SessionUnread", "Normal"),
  })[state]
  return {
    text = "●", hl = hl, size = state == "unread" and 7 or 8,
    opacity = state == "running" and (breathing() and 1 or 0.35) or 1,
    transition = state == "running" and { duration = 850, easing = "ease-in-out" } or nil,
    tooltip = ({ running = "Running", waiting = "Waiting for your approval", unread = "New since you looked" })[state],
  }
end

-- One half of the page switch.
local function segment(text, page)
  local on = st.page == page
  return {
    text = text, size = 12.5, bold = on, hl = on and "Normal" or "Dim",
    padding = { 4, 12 }, radius = 6, bg = on and "Selection" or nil,
    hover = not on and { color = "Normal" } or nil, transition = 120, cursor = "pointer",
    role = "option", selected = on,
    on_click = function() if not on then switch_page() end end,
  }
end

-- A small glyph button shown over a row.
local function action(glyph, label, fn, on)
  return {
    text = glyph, size = 13, hl = on and "Normal" or "Dim", label = label, tooltip = label,
    width = 22, height = 22, radius = 6, text_align = "center", padding = { 2, 0 },
    cursor = "pointer", hover = { bg = "Selection", color = "Normal" }, transition = 80,
    on_click = fn,
  }
end

local function row(i)
  local r = st.rows[i]
  if not r then return { height = 0 } end
  if r.kind == "group" then
    return { height = ROW.group, direction = "row", align = "end", padding = { 0, 10, 6, 10 },
      { text = r.text:upper(), hl = "Dim", size = 10.5, bold = true, letter_spacing = 0.8 } }
  elseif r.kind == "none" then
    return { height = ROW.none, direction = "row", align = "center", justify = "center",
      { text = r.text, hl = "Dim", size = 12.5 } }
  end
  local on = r.item == st.sel
  local current = r.id and r.id == (bone.chat.session() or {}).session_id
  local over = r.id and st.hover == r.id
  local title = { direction = "row", gap = 6, align = "center", height = 22 }
  title[#title + 1] = dot(r.state)
  if r.id and st.renaming == r.id then
    title[#title + 1] = {
      id = "sidebar-rename", grow = 1, size = 13.5, hl = "Normal", bg = "Normal",
      radius = 5, padding = { 1, 6 }, border = 1, border_color = "PromptBorder",
      on_key = function(k)
        if k == "esc" then st.renaming = nil take_keys() redraw() return true end
        return false
      end,
      text_input = {
        value = r.title, select = true,
        on_submit = function(text) rename(r.id, text) end,
      },
    }
  else
    title[#title + 1] = {
      text = r.title, size = 13.5, bold = current or r.state == "unread",
      hl = (on or current or r.state == "unread") and "Normal" or nil,
      text_overflow = "ellipsis", grow = 1, shrink = 1,
    }
    if over then
      local id = r.id
      title[#title + 1] = action(r.pinned and "★" or "☆", r.pinned and "Unpin" or "Pin",
        function() toggle_pin(id) end, r.pinned)
      title[#title + 1] = action("✎", "Rename", function() start_rename(id) end)
    else
      title[#title + 1] = { text = r.time or "", hl = "Dim", size = 11, wrap = false }
    end
  end
  local meta = r.state == "waiting"
    and { text = "Waiting for approval", hl = group("SessionWaiting", "ErrorMsg"), size = 11.5 }
    or { text = r.meta, hl = "Dim", size = 11.5, text_overflow = "ellipsis", shrink = 1 }
  return {
    height = ROW.card - 2, margin = { 1, 0, 1, 0 }, direction = "column", justify = "center", gap = 2,
    padding = { 0, 10 }, radius = 8, cursor = "pointer",
    bg = on and "Selection" or nil, hover = not on and { bg = HOVER } or nil, transition = 100,
    on_hover = r.id and function(inside)
      if inside then st.hover = r.id elseif st.hover == r.id then st.hover = nil end
      redraw()
    end or nil,
    on_click = function()
      if st.page == "running" then
        st.sel = r.item
        redraw()
      elseif st.renaming ~= r.id then
        choose(r.item)
      end
    end,
    title,
    meta,
  }
end

-- The project filter: what it shows, and the menu under it while open.
local function project_filter()
  local name = st.project and folder(st.project) or "All projects"
  return {
    direction = "row", align = "center", gap = 6, padding = { 5, 10 }, radius = 8,
    cursor = "pointer", hover = { bg = HOVER }, transition = 100,
    role = "button", label = "Project: " .. name,
    on_click = function() st.menu = not st.menu redraw() end,
    { text = name, size = 12.5, hl = st.project and "Normal" or "Dim", text_overflow = "ellipsis", shrink = 1 },
    { text = st.menu and "▴" or "▾", size = 10, hl = "Dim" },
    { grow = 1 },
    st.project and {
      text = "×", size = 13, hl = "Dim", padding = { 0, 4 }, radius = 5, label = "All projects",
      hover = { color = "Normal" }, cursor = "pointer",
      on_click = function() set_project(nil) end,
    } or nil,
  }
end

local function project_menu()
  if not st.menu then return nil end
  local menu = {
    -- Under the filter: the padding, the header and the filter above it.
    position = "absolute", left = 10, right = 10, top = 86, z = 30,
    bg = "Panel", radius = 10, border = 1, border_color = "PromptBorder", padding = 4,
    shadow = { blur = 18, dy = 6, color = "#00000070" }, direction = "column", max_height = 360,
    overflow = "scroll", id = "sidebar-projects",
  }
  local function item(label, count, cwd)
    local on = st.project == cwd
    return {
      direction = "row", align = "center", gap = 8, padding = { 6, 10 }, radius = 6,
      bg = on and "Selection" or nil, hover = not on and { bg = HOVER } or nil, cursor = "pointer",
      role = "option", selected = on,
      on_click = function() set_project(cwd) end,
      { text = label, size = 12.5, hl = on and "Normal" or nil, text_overflow = "ellipsis", grow = 1, shrink = 1 },
      count and { text = tostring(count), size = 11, hl = "Dim" } or nil,
    }
  end
  menu[#menu + 1] = item("All projects", #st.all, nil)
  for _, p in ipairs(projects()) do menu[#menu + 1] = item(p.name, p.count, p.cwd) end
  -- A click anywhere else closes it.
  return {
    position = "absolute", left = 0, right = 0, top = 0, bottom = 0, z = 29, block_input = true,
    on_click = function() st.menu = false redraw() end,
    menu,
  }
end

-- It slides out from under the chat: how far, 0 to 1, and when that was
-- last moved.
local slide = { at = 0, last = nil }
M.slide_ms = 200

--- How far the sidebar is out, 0 (closed) to 1 (open), moved toward where
--- it is going by the time since last asked; asks for frames until there.
local function progress()
  local want = open and 1 or 0
  local now = bone.now()
  if slide.at == want then
    slide.last = nil
    return want
  end
  local dt = slide.last and math.min(now - slide.last, 50) or 16
  slide.last = now
  local step = dt / M.slide_ms
  slide.at = want > slide.at and math.min(want, slide.at + step) or math.max(want, slide.at - step)
  bone.gui.invalidate(16)
  return slide.at
end

--- The sidebar as a node and how far out it is (nil, 0 while it is
--- closed). Its width follows that; the content keeps its own, so it is
--- uncovered rather than squeezed.
function M.node()
  local p = progress()
  if p <= 0 then return nil, 0 end
  local ease = 1 - (1 - p) ^ 3
  local inner = M.content()
  inner.width = M.width
  return {
    width = math.floor(M.width * ease), overflow = "hidden", direction = "row", inner,
  }, ease
end

--- The sidebar's own content.
function M.content()
  local jobs = chats.jobs()
  if st.page == "running" and #jobs > 0 then bone.gui.invalidate(1000) end
  local out = {
    id = ID, on_key = on_key, focus = {},
    direction = "column", padding = { 12, 10, 0, 10 }, gap = 8,
    -- The page switch, and a new chat beside it.
    { direction = "row", align = "center", gap = 8, padding = { 4, 2, 2, 2 },
      { direction = "row", gap = 2, padding = 2, radius = 8, bg = "Normal",
        segment("Chats", "chats"),
        segment("Processes" .. (#jobs > 0 and ("  " .. #jobs) or ""), "running") },
      { grow = 1 },
      { width = 28, height = 28, radius = 7, direction = "row", justify = "center", align = "center",
        cursor = "pointer", hover = { bg = "Selection" }, transition = 100,
        tooltip = "New chat (ctrl+n)", label = "New chat",
        on_click = function()
          bone.gui.focus(nil)
          bone.action("new_session")
        end,
        { text = "+", size = 17, hl = "Normal" } },
    },
  }
  if st.page == "chats" then
    out[#out + 1] = project_filter()
    out[#out + 1] = {
      id = SEARCH, bg = "Normal", radius = 8, padding = { 6, 10 }, margin = { -4, 0, 0, 0 },
      border = 1, border_color = "PromptBorder",
      size = 13, hl = "Normal", on_key = on_key,
      text_input = {
        placeholder = "Search", value = st.query,
        on_change = function(text)
          st.query, st.sel = text, 1
          compute()
          bone.gui.scroll("sessions-list", "top")
          redraw()
        end,
      },
    }
  end
  nodes.append(out, bone.gui.slots.nodes("sidebar.top"))
  out[#out + 1] = {
    id = "sessions-list", grow = 1, padding = { 0, 0, 8, 0 },
    scrollbar = { width = 4, inset = 1, show = "hover" },
    list = { count = #st.rows, estimate = ROW.card, key = st.page .. ":" .. st.version, item = row },
  }
  nodes.append(out, bone.gui.slots.nodes("sidebar.bottom"))
  out[#out + 1] = st.page == "chats" and project_menu() or nil
  return out
end

--- Open the sidebar, or close it when it has the keyboard.
function M.open()
  if open then
    if has_keys() then close() else take_keys() end
    redraw()
    return
  end
  open, st.loading, st.sel = true, true, 1
  compute()
  take_keys()
  refresh()
  chats.fetch_shells(function() compute() redraw() end)
  chats.fetch_active(function() compute() redraw() end)
  redraw()
end

--- Whether it is open, and whether it has the keyboard.
function M.is_open() return open end
M.has_keys = has_keys

chats.watch(function(kind, ev)
  -- A turn that finishes in a chat you are not looking at is new to you.
  if kind == "turn" and ev and not ev.running and ev.session_id
      and ev.session_id ~= (bone.chat.session() or {}).session_id then
    unread[ev.session_id] = true
  end
  if not open then return end
  if kind == "turn" or kind == "created" or kind == "updated" or kind == "deleted" then
    refresh()
  else
    compute()
    redraw()
  end
end)

-- A question from the core (an approval) waits in its chat until answered.
bone.on("ask/requested", function(ev)
  if ev.session_id then waiting[ev.ask_id] = ev.session_id end
  if open then compute() redraw() end
end)
bone.on("ask/resolved", function(ev)
  waiting[ev.ask_id] = nil
  if open then compute() redraw() end
end)

return M
