-- The sessions sidebar (ctrl+o, /sessions): docked at the window's left
-- edge on a step-lighter black with a hairline beside the chat (a card over
-- the chat in a narrow window), drawn as nodes (bone.nodes.window places
-- `M.node()`). bone's name and a new-chat button, a search field, Chats and
-- Processes, a project filter, then your conversations: pinned ones first,
-- the recent ones next (a chat that runs, or one touched within the last
-- hour, as in the TUI), then the rest in date groups (Today, Yesterday, This
-- week, Earlier), one line each (title and when; model and tokens in its
-- tooltip) with a dot for what it needs from you (running, waiting for an
-- approval, finished while you were elsewhere).
-- Over a row, pin and rename take the place of its time. The Processes page lists every chat's background work. The
-- data is bone.chats. The rows are made once when the data changes, and the
-- list makes nodes only for the ones in view, so a long history costs
-- nothing. It takes the keyboard as it opens (its search field, or the
-- card itself on the Processes page): up/down choose, enter opens, typing
-- searches, tab changes page, x stops a process, esc goes back to the
-- prompt, ctrl+o closes; a click opens.
local M = {}

local chats = require("bone.chats")
local nodes = require("bone.nodes")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")

local ID = "sessions"
local SEARCH = "sidebar-search"
local CARD = "sessions-card"
M.width = tonumber(bone.state.load("sidebar-width")) or 260
M.min_width, M.max_width = 200, 520
-- Under this window width it floats over the chat instead of pushing it.
M.overlay_below = 760
-- How far the card stands off the window's edges.
M.inset = style.space.lg

local ROW = { group = 34, card = 32, none = 40 }

local open = false
local overlaid = false -- drawn over the chat (a narrow window), not beside it
local st = {
  page = "chats", query = "", sel = 1, all = {}, stats = {}, loading = false,
  rows = {}, items = {}, version = 0,
  minute = 0, -- the minute the rows were made in: the recent group ages
  window = 0, -- the recent window they were made with
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
      -- A chat with no title has no message in it: nothing to go back to.
      -- It stays listed while on screen, pinned, running or searched for.
      local empty = not s.title and st.query == "" and s.session_id ~= current
        and not pinned[s.session_id] and not chats.live[s.session_id]
      if not empty and (not st.project or s.cwd == st.project) then items[#items + 1] = s end
    end
    if #items == 0 then
      rows[1] = { kind = "none", text = st.loading and "Loading…"
        or ((st.query == "" and not st.project) and "No chats yet" or "No matching chats") }
    end
    -- Pinned first, then the recent ones, then the rest; newest first in
    -- each group. A chat is recent while it runs or was touched within
    -- the window (bone.ui.sessions_recent_seconds), as in the TUI.
    table.sort(items, function(a, b)
      local ap, bp = pinned[a.session_id] and 1 or 0, pinned[b.session_id] and 1 or 0
      if ap ~= bp then return ap > bp end
      local ag, bg = g.groups[a.session_id] or 2, g.groups[b.session_id] or 2
      if ag ~= bg then return ag < bg end
      local aa, ba = g.activities[a.session_id], g.activities[b.session_id]
      if aa ~= ba then return aa > ba end
      return a.session_id < b.session_id
    end)
    local heading
    for i, s in ipairs(items) do
      local id, at = s.session_id, g.activities[s.session_id]
      local name
      if pinned[id] then
        name = "Pinned"
      elseif (g.groups[id] or 2) == 1 then
        name = "Recent · last " .. chats.recent_label()
      else
        name = date_group(at)
      end
      if name ~= heading then
        rows[#rows + 1] = { kind = "group", text = name }
        heading = name
      end
      local state = (is_waiting(id) and "waiting") or (chats.live[id] and "running")
        or (unread[id] and "unread") or nil
      local meta = {}
      local stats = st.stats[id]
      if not st.project then meta[#meta + 1] = (stats and stats.model) or "No model used" end
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
  st.minute = math.floor(os.time() / 60)
  st.window = chats.recent_window()
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

local function choose(i, keep_keys)
  local r = st.items[i]
  if not r or not r.session_id then return end
  st.sel = i
  unread[r.session_id] = nil
  bone.api.open_session(r.session_id)
  if keep_keys and not overlaid then take_keys() else bone.gui.focus(nil) end
  -- Over the chat it has done its job: let the chat show.
  if overlaid then open = false end
  compute()
  redraw()
end

local function start_rename(id)
  st.renaming, st.rename_focused = id, false
  bone.gui.focus("sidebar-rename")
  redraw()
end

local function toggle_pin(id)
  pinned[id] = not pinned[id] or nil
  bone.state.save("sidebar-pinned", pinned)
  compute()
  redraw()
end

-- `away`: the field lost the keyboard to a click elsewhere, which keeps it.
local function rename(id, title, away)
  st.renaming = nil
  title = (title or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if title ~= "" then
    bone.request("session/rename", { session_id = id, title = title }, function(_, err)
      if err then bone.notify("rename: " .. tostring(err), "error") end
      refresh()
    end)
  end
  if not away then take_keys() end
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
  if st.page == "chats" then
    local r = st.items[st.sel]
    if r and r.session_id then
      unread[r.session_id] = nil
      bone.api.open_session(r.session_id)
    end
  end
  -- open_session can replace/rebuild the GUI view; refresh the card so
  -- its keyboard focus and selection remain visible.
  take_keys()
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

-- Its keys, from the card or its search field (which edits with the
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
  return style.finish({
    text = "●", hl = hl, size = 7,
    opacity = state == "running" and (breathing() and 1 or 0.35) or 1,
    transition = state == "running" and { duration = 850, easing = "ease-in-out" } or nil,
    tooltip = ({ running = "Running", waiting = "Waiting for your approval", unread = "New since you looked" })[state],
  })
end

-- A small glyph button shown over a row.
local function action(glyph, label, fn, on)
  return widgets.icon_button(glyph, label, fn, { hl = on and "Normal" or "Dim", size = 13, box = 22,
    id = "sidebar-" .. label:lower() })
end

local function row(i)
  local r = st.rows[i]
  if not r then return { height = 0 } end
  if r.kind == "group" then
    return { height = ROW.group, direction = "row", align = "end", padding = { 0, 8, 7, 8 },
      { text = r.text, hl = "Dim", size = style.size.small, medium = true } }
  elseif r.kind == "none" then
    return { height = ROW.none, direction = "row", align = "center", justify = "center",
      { text = r.text, hl = "Dim", size = style.size.caption } }
  end
  local on = r.item == st.sel
  local current = r.id and r.id == (bone.chat.session() or {}).session_id
  local over = r.id and st.hover == r.id
  local title = { direction = "row", gap = 8, align = "center", height = 30 }
  title[#title + 1] = dot(r.state)
  -- The rename field lost the keyboard (a click elsewhere): it ends, and
  -- keeps what was typed, as a file manager's does.
  if r.id and st.renaming == r.id then
    if bone.gui.focused() == "sidebar-rename" then
      st.rename_focused = true
    elseif st.rename_focused then
      local id, text = r.id, bone.gui.input_text("sidebar-rename")
      if text == r.title then text = nil end -- unchanged: nothing to send
      st.renaming, st.rename_focused = nil, false
      bone.defer(0, function() rename(id, text, true) end)
    end
  end
  if r.id and st.renaming == r.id then
    title[#title + 1] = style.finish({
      id = "sidebar-rename", grow = 1, size = style.size.body, hl = "Normal", bg = "Normal",
      radius = style.radius.sm - 1, padding = { 1, 6 }, border = 1, border_color = "PromptBorder",
      on_key = function(k)
        if k == "esc" then st.renaming, st.rename_focused = nil, false take_keys() redraw() return true end
        return false
      end,
      text_input = {
        value = r.title, select = true,
        on_submit = function(text) rename(r.id, text) end,
      },
    })
  else
    title[#title + 1] = {
      text = r.title, size = style.size.body, medium = r.state == "unread", -- the current one has its fill
      hl = (on or current or r.state == "unread") and "Normal" or group("Muted", nil),
      text_overflow = "ellipsis", wrap = false, grow = 1, shrink = 1,
    }
    if over then
      local id = r.id
      title[#title + 1] = action("pin", r.pinned and "Unpin" or "Pin",
        function() toggle_pin(id) end, r.pinned)
      title[#title + 1] = action("edit", "Rename", function() start_rename(id) end)
    else
      title[#title + 1] = { text = r.time or "", hl = "Dim", size = style.size.micro, font = "mono", wrap = false, shrink = 0 }
    end
  end
  -- What a row leaves out (the model, the tokens; what it waits for) is its
  -- tooltip.
  local tip = r.state == "waiting" and "Waiting for your approval" or (r.meta ~= "" and r.meta or nil)
  -- The row takes the pointer edge to edge, the card inset in it: across the
  -- gap between two cards the pointer stays a hand and the row stays lit.
  return {
    height = ROW.card, padding = { 1, 0 }, direction = "column",
    cursor = "pointer", role = "option", selected = on, tooltip = tip,
    on_hover = r.id and function(inside)
      if inside then st.hover = r.id elseif st.hover == r.id then st.hover = nil end
      redraw()
    end or nil,
    on_click = function()
      if st.page == "running" then
        select(r.item)
      elseif st.renaming ~= r.id then
        choose(r.item, true)
      end
    end,
    style.finish({
      grow = 1, direction = "column", justify = "center", padding = { 0, 8 },
      radius = style.radius.md, bg = on and "Selection" or (over and style.hover()) or nil,
      hover = not on and { bg = style.hover() } or nil, transition = style.motion.base,
      title,
    }),
  }
end

-- The project filter: what it shows, and the menu under it while open.
local function project_filter()
  local name = st.project and folder(st.project) or "All projects"
  local icons = require("bone.nodes.icons")
  return widgets.press({
    direction = "row", align = "center", gap = 10, height = 30, padding = { 0, 8 }, radius = style.radius.md,
    label = "Project: " .. name,
    icons.node("folder", { hl = st.project and "Normal" or "Dim" }),
    { text = name, size = style.size.body, hl = st.project and "Normal" or group("Muted", "Dim"), text_overflow = "ellipsis", wrap = false, shrink = 1 },
    icons.node(st.menu and "arrow_up" or "chevron_down", { size = 12, hl = "Dim" }),
    { grow = 1 },
    st.project and widgets.press({
      text = "×", size = style.size.caption, hl = "Dim", padding = { 0, 4 }, radius = style.radius.sm - 1,
      label = "All projects", hover = { color = "Normal" },
    }, function() set_project(nil) end) or nil,
  }, function() st.menu = not st.menu redraw() end)
end

local function project_menu()
  if not st.menu then return nil end
  local menu = {
    -- Under the filter: the padding, the header and the filter above it.
    position = "absolute", left = 8, right = 8, top = 216, z = 30,
    bg = "Panel", radius = style.radius.lg, border = 1, border_color = "PromptBorder", padding = 4,
    shadow = style.shadow.menu, direction = "column", max_height = 360,
    overflow = "scroll", id = "sidebar-projects",
  }
  local function item(label, count, cwd)
    local on = st.project == cwd
    return widgets.press({
      direction = "row", align = "center", gap = 8, padding = { 6, 10 }, radius = style.radius.sm,
      bg = on and "Selection" or nil, selected = on,
      { text = label, size = style.size.caption, hl = on and "Normal" or nil, text_overflow = "ellipsis", grow = 1, shrink = 1 },
      count and { text = tostring(count), size = style.size.micro, hl = "Dim" } or nil,
    }, function() set_project(cwd) end, "option", not on)
  end
  menu[#menu + 1] = item("All projects", #st.all, nil)
  for _, p in ipairs(projects()) do menu[#menu + 1] = item(p.name, p.count, p.cwd) end
  style.finish(menu)
  -- A click anywhere else closes it.
  return {
    position = "absolute", left = 0, right = 0, top = 0, bottom = 0, z = 29, block_input = true,
    on_click = function() st.menu = false redraw() end,
    menu,
  }
end

-- It opens out from the window's edge: how far, 0 to 1, and the slide under
-- way: where it set off from, where to, when, and the frame before.
local slide = { at = 0, from = 0, to = 0, started = 0, last = 0 }
M.slide_ms = 0 -- no slide: it opens and closes in one frame (a gui.lua can set a time)
-- The most a frame moves it, in ms of the slide.
local FRAME_MAX = 17
-- How far right of the pointer the width handle's edge was when pressed.
local grab

--- How far the sidebar is out, 0 (closed) to 1 (open), by the clock since
--- it set off; asks for frames until there. A slow frame (the first lays
--- out the whole list) moves it no more than a 60 Hz one would, so it is
--- not over before it is seen. Turned back halfway, it takes half as
--- long. The first frame is a frame in, so it moves at once.
local function progress()
  local want = open and 1 or 0
  local now = bone.now()
  if slide.to ~= want then
    slide.from, slide.to, slide.started, slide.last = slide.at, want, now - 16, now
  end
  if slide.at == want then return want end
  slide.started = slide.started + math.max(0, now - slide.last - FRAME_MAX)
  slide.last = now
  local span = M.slide_ms * math.abs(slide.to - slide.from)
  local k = span > 0 and math.min(1, (now - slide.started) / span) or 1
  slide.at = slide.from + (slide.to - slide.from) * k
  -- Built again next frame, in step with the display; a 16 ms timer
  -- beats against it and drops frames.
  bone.gui.invalidate()
  return slide.at
end

--- The sidebar as a node, the room it takes from the chat's row (0 while
--- it is closed) and, in a window narrower than `M.overlay_below`
--- (`ctx.width`), the backdrop it floats over the chat on, which closes it;
--- the room is 0 then. It is a card like the composer: its width follows
--- how far out it is while the content keeps its own, so it is uncovered
--- rather than squeezed.
function M.node(ctx)
  local p = progress()
  if p <= 0 then
    overlaid = false
    return nil, 0
  end
  local ease = 1 - (1 - p) ^ 3
  overlaid = ctx ~= nil and (ctx.width or math.huge) < M.overlay_below
  local inner = M.content()
  inner.width = M.width - (overlaid and 2 or 1) -- inside the card's border, or beside the hairline
  -- Its right edge is a handle: drag it to set the width (kept between runs).
  -- The edge stays as far from the pointer as where it was grabbed.
  local handle = {
    position = "absolute", top = 0, bottom = 0, right = 0, width = 6, z = 1, cursor = "col-resize",
    on_press = function(pos) grab = 6 - pos.x end,
    on_drag = function(pos)
      local left = (bone.gui.rect(CARD) or { x = 0 }).x
      local w = math.floor(pos.x - left + (grab or 3))
      w = math.max(M.min_width, math.min(M.max_width, w))
      if w ~= M.width then M.width = w redraw() end
    end,
    on_release = function() bone.state.save("sidebar-width", M.width) end,
  }
  local card = {
    id = CARD, width = math.floor(M.width * ease), overflow = "hidden", direction = "row",
    opacity = ease, inner, handle,
  }
  if overlaid then
    -- Under the title bar the window draws, if it draws one.
    local below = require("bone.nodes.titlebar").offset()
    card.position, card.left, card.top, card.bottom, card.z = "absolute", M.inset, M.inset + below, M.inset, 8
    card.shadow = style.shadow.card
    local backdrop = style.finish({
      position = "absolute", left = 0, right = 0, top = 0, bottom = 0, z = 7,
      bg = style.scrim.dialog, opacity = ease, on_click = close,
    })
    return widgets.card(card), 0, backdrop
  end
  -- Docked: flush with the window's edges, a step lighter than the chat,
  -- a hairline between them. The lane is the whole width from the first
  -- frame to the last, so the chat beside it is laid out once as it opens.
  local g = bone.hl.get("Sidebar")
  card.bg = (g and g.bg) and "Sidebar" or "Panel"
  card.margin = { 0, M.width - card.width, 0, 0 }
  table.insert(card, #card, { position = "absolute", top = 0, bottom = 0, right = 0, width = 1,
    bg = style.alpha("WinSeparator", "ff", "fg", "PromptBorder") })
  return style.finish(card), M.width
end

--- The sidebar's own content.
function M.content()
  local jobs = chats.jobs()
  if st.page == "running" and #jobs > 0 then bone.gui.invalidate(1000) end
  -- The recent group ages: a chat leaves it when the window passes, and
  -- the window itself can change (bone.ui.sessions_recent_seconds), so
  -- the rows are made again each minute and when it changes (as in the TUI).
  local minute = math.floor(os.time() / 60)
  local window = chats.recent_window()
  if minute ~= st.minute or window ~= st.window then compute() end
  bone.gui.invalidate((60 - os.time() % 60) * 1000)
  local icons = require("bone.nodes.icons")
  local muted = group("Muted", "Dim")
  -- A page of the sidebar, or a place it opens: an icon and a name.
  local function nav(icon, name, on, click, count)
    return widgets.press({
      direction = "row", align = "center", gap = 10, height = 30, padding = { 0, 8 }, radius = style.radius.md,
      label = name, selected = on,
      icons.node(icon, { hl = on and "Normal" or "Dim" }),
      { text = name, size = style.size.body, hl = on and "Normal" or muted, grow = 1 },
      count and { text = tostring(count), size = style.size.micro, font = "mono", hl = "Dim" } or nil,
    }, click)
  end
  local out = {
    id = ID, on_key = on_key, focus = {},
    direction = "column", padding = { 0, 8, 0, 8 }, gap = 1,
    -- bone's name, and a new chat beside it.
    { direction = "row", align = "center", height = 50, padding = { 0, 2, 0, 8 },
      { text = "bone", size = style.size.body + 1, bold = true, hl = "Normal", grow = 1 },
      widgets.icon_button("compose", "New chat", function()
        bone.gui.focus(nil)
        bone.action("new_session")
      end, { box = 28, size = 16, hl = muted, radius = style.radius.md, id = "sidebar-new" }),
    },
  }
  -- The search field: an icon, the field, and the key that comes back to it.
  out[#out + 1] = style.finish({
    direction = "row", align = "center", gap = 8, height = 32, padding = { 0, 6, 0, 10 }, margin = { 0, 0, 8, 0 },
    bg = "Normal", radius = style.radius.md, border = 1, border_color = "PromptBorder",
    cursor = "text", on_click = function() bone.gui.focus(SEARCH) end,
    icons.node("search", { hl = "Dim", size = 15 }),
    {
      id = SEARCH, grow = 1, width = 0, size = style.size.body, hl = "Normal", on_key = on_key,
      text_input = {
        placeholder = "Search", value = st.query,
        on_change = function(text)
          if st.page ~= "chats" then st.page = "chats" end
          st.query, st.sel = text, 1
          compute()
          bone.gui.scroll("sessions-list", "top")
          redraw()
        end,
      },
    },
    widgets.keycap("Ctrl O"),
  })
  out[#out + 1] = nav("chat", "Chats", st.page == "chats", function() if st.page ~= "chats" then switch_page() end end)
  out[#out + 1] = nav("terminal", "Processes", st.page == "running",
    function() if st.page ~= "running" then switch_page() end end, #jobs > 0 and #jobs or nil)
  out[#out + 1] = nav("layers", "Plugins", false, function() require("bone.catalog").open() end)
  if st.page == "chats" then
    out[#out + 1] = project_filter()
  end
  nodes.append(out, bone.gui.slots.nodes("sidebar.top"))
  out[#out + 1] = {
    id = "sessions-list", grow = 1, padding = { 0, 0, 8, 0 },
    scrollbar = { width = 4, inset = 1, show = "hover" },
    list = { count = #st.rows, estimate = ROW.card, key = st.page .. ":" .. st.version, item = row },
  }
  nodes.append(out, bone.gui.slots.nodes("sidebar.bottom"))
  -- The list fades into the card's foot rather than cutting a row in half
  -- there (as the chat fades into the composer).
  local under = overlaid and "InputBackground" or ((bone.hl.get("Sidebar") or {}).bg and "Sidebar" or "Panel")
  local clear, solid = style.alpha(under, "00", "bg"), style.alpha(under, "ff", "bg")
  if clear and solid then
    out[#out + 1] = { position = "absolute", left = 0, right = 0, bottom = 0, height = 28, z = 1,
      gradient = { clear, solid }, radius = overlaid and { 0, 0, style.radius.round - 1, style.radius.round - 1 } or nil }
  end
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
  -- Start navigation from the conversation already on screen. Up/down then
  -- previews the adjacent conversation instead of jumping from the top.
  local current = (bone.chat.session() or {}).session_id
  if current then
    for i, chat in ipairs(st.items) do
      if chat.session_id == current then st.sel = i break end
    end
  end
  take_keys()
  refresh()
  chats.fetch_shells(function() compute() redraw() end)
  chats.fetch_active(function() compute() redraw() end)
  redraw()
end

--- Close the sidebar.
M.close = close

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
