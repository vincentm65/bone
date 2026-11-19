-- The sessions sidebar (ctrl+o, /sessions): docked at the window's left
-- edge on a step-lighter black with a hairline beside the chat (a card over
-- the chat where there is no room for both), drawn as nodes
-- (bone.nodes.window places `M.node()`). A search field with the project
-- and new-chat buttons beside it, Chats and Processes, then your
-- conversations as the TUI lists them (bone.chats.list): a header per
-- section (Pinned, Active, Running, Archived) with its count, which folds
-- it (a click, or enter on it; the archive starts folded, and the TUI and
-- this share the fold), then one line per chat: its title, white when it
-- needs you or is open, and at the right what it needs (bone.chats.status:
-- Working with a spinner, Waiting, Done, Failed) or its time. Its model
-- and tokens are its tooltip.
-- Over a row, pin, rename and archive take the place of its status (the
-- window does that, `group_hover`: nothing is built again as the pointer
-- moves). The
-- Processes page lists every chat's background work. The data is
-- bone.chats. The rows are made once when the data changes (and each
-- minute, for the times), and the list makes nodes only for the ones in
-- view, so a long history costs nothing. It takes the keyboard as it opens
-- (its search field, or the card itself on the Processes page): up/down
-- choose (a header too), enter opens or folds, typing searches, ctrl+t
-- pins, f2 renames, ctrl+a archives, tab changes page, x stops a process,
-- esc goes back to the prompt, ctrl+o closes; a click opens.
local M = {}

local chats = require("bone.chats")
local nodes = require("bone.nodes")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")

local ID = "sessions"
local SEARCH = "sidebar-search"
local CARD = "sessions-card"
M.width = tonumber(bone.state.load("sidebar-width")) or 248
M.min_width, M.max_width = 200, 520
-- It floats over the chat, instead of pushing it, where the chat beside it
-- would be narrower than this.
M.chat_min = 480
-- How far the card stands off the window's edges.
M.inset = style.space.lg

local ROW = { header = 42, card = 38, none = 40 }

local open = false
local overlaid = false -- drawn over the chat (a narrow window), not beside it
local st = {
  page = "chats", query = "", sel = 1, all = {}, stats = {}, loading = false,
  rows = {}, items = {}, version = 0,
  minute = 0, -- the minute the rows were made in: the recent group ages
  window = 0, -- the recent window they were made with
  project = nil, -- the folder the list is narrowed to, or nil for all
  menu = false, -- the project menu is open
  renaming = nil, -- the session being renamed
}

local function redraw()
  bone.gui.invalidate()
end

-- A highlight group, or `fallback` when the colorscheme does not set it.
local function group(name, fallback)
  local g = bone.hl.get(name)
  return (g and g.fg) and name or fallback
end

local function folder(cwd)
  return (cwd or ""):gsub("/+$", ""):match("([^/]+)$") or "~"
end

-- The chat an item stands for: a chat row's, or a process's.
local function sid(item)
  return item and (item.session and item.session.session_id or item.session_id)
end

-- What an item is, to find it again when the list changes.
local function key_of(item)
  if not item then return nil end
  if item.kind == "header" then return "header:" .. item.section end
  return item.key or sid(item)
end

--- The rows of the current page, from the data: { kind = "header" | "card"
--- | "none", ... }, and the things they stand for (`items`: a chat page's
--- bone.chats.list rows, or the processes), which the keyboard moves over.
--- `fresh` (a new search, project or page) starts at the first chat.
local function compute(fresh)
  chats.note_running()
  local key = not fresh and key_of(st.items[st.sel])
  -- A header stays selected only where the keyboard moved to it.
  if fresh then st.picked = false end
  if key and key:match("^header:") and not st.picked then key = nil end
  local rows, items = {}, {}
  if st.page == "running" then
    local jobs = chats.jobs()
    if #jobs == 0 then
      rows[1] = { kind = "none", text = "Nothing running in the background" }
    end
    for i, j in ipairs(jobs) do
      items[i] = j
      rows[#rows + 1] = {
        kind = "card", item = i, title = j.title, meta = j.detail ~= "" and j.detail or "running",
        status = { state = "running", hl = "SessionWorking", text = bone.util.duration(os.time() - j.started) },
      }
    end
  else
    local current = (bone.chat.session() or {}).session_id
    local list = chats.list(st.all, st.query, st.stats, function(s)
      -- A chat with no title has no message in it: nothing to go back to.
      -- It stays listed while on screen, pinned, running or searched for.
      local empty = not s.title and st.query == "" and s.session_id ~= current
        and not chats.pinned[s.session_id] and not chats.live[s.session_id]
      return not empty and (not st.project or s.cwd == st.project)
    end)
    if #list == 0 then
      rows[1] = { kind = "none", text = st.loading and "Loading…"
        or ((st.query == "" and not st.project) and "No chats yet" or "No matching chats") }
    end
    for i, r in ipairs(list) do
      items[i] = r
      if r.kind == "header" then
        rows[#rows + 1] = { kind = "header", item = i, section = r.section, count = r.count, folded = r.folded }
      else
        local id = r.session.session_id
        local meta, stats = {}, st.stats[id]
        if not st.project then meta[#meta + 1] = (stats and stats.model) or "No model used" end
        if stats and (stats.tokens or 0) > 0 then meta[#meta + 1] = chats.count(stats.tokens) .. " tokens" end
        if stats and (stats.turns or 0) > 0 and st.project then
          meta[#meta + 1] = stats.turns .. (stats.turns == 1 and " turn" or " turns")
        end
        rows[#rows + 1] = {
          kind = "card", item = i, id = id, pinned = chats.pinned[id], section = r.section,
          title = r.session.title or "Untitled", meta = table.concat(meta, " · "),
          status = chats.status(id, r.at),
        }
      end
    end
  end
  st.rows, st.items = rows, items
  st.sel = #items == 0 and 0 or math.max(1, math.min(st.sel, #items))
  -- The same item again; else (nothing chosen yet, or it went) the chat on
  -- screen, else the first chat: not a header.
  local found = false
  for i, item in ipairs(items) do
    if key and key_of(item) == key then st.sel, found = i, true break end
  end
  if not found and st.page == "chats" then
    local current, first = not fresh and (bone.chat.session() or {}).session_id, nil
    for i, item in ipairs(items) do
      if item.kind == "chat" then
        first = first or i
        if sid(item) == current then first = i break end
      end
    end
    st.sel = first or st.sel
  end
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

-- Open item `i`: a chat, or fold a section's header.
local function choose(i, keep_keys)
  local r = st.items[i]
  if not r then return end
  st.sel, st.picked = i, true
  if r.kind == "header" then
    -- (A search shows them all unfolded: nothing to fold.)
    if st.query == "" then chats.toggle_folded(r.section) end -- the watcher below draws it again
    return
  end
  local id = sid(r)
  if not id then return end
  chats.seen(id)
  bone.api.open_session(id)
  if keep_keys and not overlaid then take_keys() else bone.gui.focus(nil) end
  -- Over the chat it has done its job: let the chat show.
  if overlaid then open = false end
  compute()
  redraw()
end

local function start_rename(id, title)
  st.renaming, st.rename_focused = id, false
  -- From its title, not what a rename given up left in the field.
  bone.gui.set_input_text("sidebar-rename", title or "")
  bone.gui.focus("sidebar-rename")
  redraw()
end

local function toggle_pin(id)
  chats.toggle_pin(id) -- the watcher below draws it again
end

-- `away`: the field lost the keyboard to a click elsewhere, which keeps it.
local function rename(id, title, away)
  st.renaming = nil
  chats.rename(id, title, refresh)
  if not away then take_keys() end
  redraw()
end

-- Bring the selected row into view.
local function reveal()
  local top, h = 0, 0
  for _, r in ipairs(st.rows) do
    if r.item == st.sel then
      h = ROW[r.kind]
      break
    end
    top = top + ROW[r.kind]
  end
  local info = bone.gui.scroll_info("sessions-list")
  if not info or h == 0 then return end
  if top < info.offset then
    bone.gui.scroll("sessions-list", top - ROW.header)
  elseif top + h > info.offset + info.viewport then
    bone.gui.scroll("sessions-list", top + h - info.viewport + 8)
  end
end

-- Move the keyboard's row to item `i`; on the Chats page a chat there opens
-- as you pass it.
local function select(i)
  st.sel, st.picked = math.max(1, math.min(#st.items, i)), true
  reveal()
  local r = st.items[st.sel]
  if st.page == "chats" and r and r.kind == "chat" then
    chats.seen(sid(r))
    bone.api.open_session(sid(r))
  end
  -- open_session can replace/rebuild the GUI view; refresh the card so
  -- its keyboard focus and selection remain visible.
  take_keys()
  redraw()
end

local function switch_page()
  st.page = st.page == "chats" and "running" or "chats"
  st.sel, st.menu = 1, false
  compute(true)
  bone.gui.scroll("sessions-list", "top")
  take_keys()
  redraw()
end

local function set_project(cwd)
  st.project, st.menu, st.sel = cwd, false, 1
  compute(true)
  bone.gui.scroll("sessions-list", "top")
  take_keys()
  redraw()
end

local function close()
  open, st.menu, st.renaming = false, false, nil
  bone.gui.focus(nil)
  redraw()
end

-- Its keys, from the card or its search field (which edits with the
-- rest).
local KEYS = {
  esc = function()
    -- Menu, then the filter, then the panel: each esc gives back one thing.
    if st.menu then st.menu = false redraw() return end
    if st.query ~= "" then
      st.query, st.sel = "", 1
      compute(true)
      bone.gui.scroll("sessions-list", "top")
      redraw()
      return
    end
    close()
  end,
  ["ctrl+o"] = close,
  tab = switch_page, ["shift+tab"] = switch_page,
  enter = function() if st.page == "chats" then choose(st.sel) end end,
  ["ctrl+t"] = function()
    local r = st.page == "chats" and st.items[st.sel]
    if r and r.kind == "chat" then toggle_pin(sid(r)) end
  end,
  f2 = function()
    local r = st.page == "chats" and st.items[st.sel]
    if r and r.kind == "chat" then start_rename(sid(r), r.session.title) end
  end,
  -- (While searching, ctrl+a selects the search's text instead.)
  ["ctrl+a"] = function()
    local r = st.query == "" and st.page == "chats" and st.items[st.sel]
    if not (r and r.kind == "chat") then return false end
    if chats.archivable(r.section) then chats.archive(sid(r)) end
  end,
  up = function() select(st.sel - 1) end, down = function() select(st.sel + 1) end,
  pageup = function() select(st.sel - 5) end, pagedown = function() select(st.sel + 5) end,
}

local function on_key(key)
  if KEYS[key] then return KEYS[key]() ~= false end
  -- x stops a process on the Processes page.
  if st.page == "running" and key == "x" and st.items[st.sel] then
    chats.stop(st.items[st.sel])
    return true
  end
  return false
end

-- ---- nodes ------------------------------------------------------------------

-- A small glyph button shown over the row of chat `id`.
local function action(glyph, label, fn, id, on)
  return widgets.icon_button(glyph, label, fn, { hl = on and "Normal" or "Dim", size = 13, box = 22,
    focusable = false, id = "sidebar-" .. glyph .. ":" .. id })
end

-- A row's status (bone.chats.status): Working with a spinner, Waiting with
-- its pulsing dot, Done, Failed, or the time. `id`: a chat, whose pin,
-- rename and archive take its place while the pointer is on the row.
local function status_node(s, id)
  local box = { direction = "row", align = "center", gap = 5, group_hover = id and { opacity = 0 } or nil }
  if s.state == "running" then
    box[#box + 1] = widgets.spinner(12, "SessionWorking")
  elseif s.state == "waiting" then
    box[#box + 1] = widgets.status_dot("waiting", { tooltip = false })
  end
  box[#box + 1] = { text = s.text or "", hl = s.hl or "Dim", size = style.size.micro, wrap = false }
  return box
end

-- What a row's tooltip says first, for a state that needs you.
local TIPS = { waiting = "Waiting for you", failed = "Its last turn failed", unread = "Finished while you were elsewhere" }

local function row(i)
  local r = st.rows[i]
  if not r then return { height = 0 } end
  if r.kind == "header" then
    -- A section's header is a button: it folds the section, or unfolds it.
    -- Its arrow shows while it is folded, or the pointer is on it.
    local icons = require("bone.nodes.icons")
    local name = chats.sections[r.section]
    return {
      height = ROW.header, padding = { 12, 0, 2, 0 }, direction = "column", cursor = "pointer",
      role = "button", label = name .. ", " .. r.count .. (r.folded and ", folded" or ""),
      on_click = function() if st.query == "" then chats.toggle_folded(r.section) end end,
      style.finish({
        grow = 1, direction = "row", align = "center", gap = 6, padding = { 0, 9 }, radius = style.radius.md,
        border = 1, border_color = (r.item == st.sel and has_keys()) and "PromptBorder" or "transparent",
        hover = { bg = style.hover() }, transition = style.motion.base,
        { text = name, hl = "Dim", size = style.size.small, medium = true, wrap = false },
        { text = tostring(r.count), hl = "Dim", size = style.size.micro, wrap = false },
        { grow = 1 },
        { opacity = r.folded and 1 or 0, group_hover = { opacity = 1 },
          icons.node(r.folded and "chevron_right" or "chevron_down", { size = 11, hl = "Dim" }) },
      }),
    }
  end
  if r.kind == "none" then
    return { height = ROW.none, direction = "row", align = "center", justify = "center",
      { text = r.text, hl = "Dim", size = style.size.caption } }
  end
  local on = r.item == st.sel
  local current = r.id and r.id == (bone.chat.session() or {}).session_id
  local status = r.status or {}
  local title = { direction = "row", gap = 8, align = "center", height = 30 }
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
    -- White; an archived chat a step quieter, unless it needs you, is open,
    -- or has the keyboard. The only color in the row is its status's.
    local quiet = r.section == 4 and not ((on and has_keys()) or current or status.state)
    title[#title + 1] = {
      text = r.title, size = style.size.body,
      color = quiet and group("Muted", nil) or "Normal",
      text_overflow = "ellipsis", wrap = false, grow = 1, shrink = 1,
    }
    -- The status, and over it pin, rename and archive while the pointer is
    -- on the row: one slot as wide as they are, so the title keeps its
    -- width and nothing is built again.
    local id = r.id
    local actions = id and { position = "absolute", right = 0, top = 0, direction = "row", gap = 2,
      opacity = 0, group_hover = { opacity = 1 },
      action("pin", r.pinned and "Unpin" or "Pin", function() toggle_pin(id) end, id, r.pinned),
      action("edit", "Rename", function() start_rename(id, r.title) end, id) }
    if actions and chats.archivable(r.section) then
      actions[#actions + 1] = action("archive", "Archive", function() chats.archive(id) end, id)
    end
    local n = actions and (#actions) or 0
    local slot = { min_width = id and (n * 22 + (n - 1) * 2) or nil, height = 22, shrink = 0, direction = "row",
      justify = "end", align = "center", status_node(status, id) }
    if actions then slot[#slot + 1] = actions end
    title[#title + 1] = slot
  end
  -- What a row leaves out (the model, the tokens; what it needs) is its
  -- tooltip.
  local tip = TIPS[status.state]
  if r.meta and r.meta ~= "" then tip = tip and (tip .. " · " .. r.meta) or r.meta end
  -- The row takes the pointer edge to edge, the card inset in it: across the
  -- gap between two cards the pointer stays a hand and the row stays lit.
  return {
    height = ROW.card, padding = { 2, 0 }, direction = "column",
    cursor = "pointer", role = "option", selected = on, tooltip = tip,
    on_click = function()
      if st.page == "running" then
        select(r.item)
      elseif st.renaming ~= r.id then
        choose(r.item, true)
      end
    end,
    style.finish({
      grow = 1, direction = "column", justify = "center", padding = { 0, 9 },
      -- The open chat is filled. The keyboard's row, where it is another
      -- (a search), is outlined, while the list has the keyboard.
      radius = style.radius.md, bg = current and "Selection" or nil,
      border = 1, border_color = (on and not current and has_keys()) and "PromptBorder" or "transparent",
      hover = not current and { bg = style.hover() } or nil, transition = style.motion.base,
      title,
    }),
  }
end

-- The project filter while one is chosen: its name, and × for all of them.
-- (The folder button at the top opens the menu.)
local function project_filter()
  if not st.project then return nil end
  local name = folder(st.project)
  local icons = require("bone.nodes.icons")
  return widgets.press({
    direction = "row", align = "center", gap = 10, height = 30, padding = { 0, 8 }, radius = style.radius.md,
    label = "Project: " .. name,
    icons.node("folder", { hl = "Normal" }),
    { text = name, size = style.size.body, hl = "Normal", text_overflow = "ellipsis", wrap = false, shrink = 1 },
    icons.node(st.menu and "arrow_up" or "chevron_down", { size = 12, hl = "Dim" }),
    { grow = 1 },
    widgets.press({
      text = "×", size = style.size.caption, hl = "Dim", padding = { 0, 4 }, radius = style.radius.sm - 1,
      label = "All projects", hover = { color = "Normal" },
    }, function() set_project(nil) end),
  }, function() st.menu = not st.menu redraw() end)
end

local function project_menu()
  if not st.menu then return nil end
  local menu = {
    -- Under the search row and its folder button.
    position = "absolute", left = 8, right = 8, top = 46, z = 30,
    bg = "Panel", radius = style.radius.lg, border = 1, border_color = "PromptBorder", padding = 4,
    shadow = style.shadow.menu, direction = "column", max_height = 360,
    overflow = "scroll", id = "sidebar-projects", transition = style.appear(-6),
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

-- How far right of the pointer the width handle's edge was when pressed.
local grab

--- The sidebar as a node, the room it takes from the chat's row (0 while
--- it is closed) and, where `ctx.width` (the room it shares with the chat)
--- would leave the chat under `M.chat_min`, the backdrop it floats over the
--- chat on, which closes it;
--- the room is 0 then. It opens and closes in one frame.
function M.node(ctx)
  if not open then
    overlaid = false
    return nil, 0
  end
  overlaid = ctx ~= nil and (ctx.width or math.huge) - M.width < M.chat_min
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
  local card = { id = CARD, width = M.width, overflow = "hidden", direction = "row", inner, handle }
  if overlaid then
    -- Under the title bar the window draws, if it draws one.
    local below = require("bone.nodes.titlebar").offset()
    card.position, card.left, card.top, card.bottom, card.z = "absolute", M.inset, M.inset + below, M.inset, 8
    card.shadow = style.shadow.card
    -- It slides in from the window's edge over a dimming chat.
    card.transition = style.appear(nil, -16)
    local backdrop = style.finish({
      position = "absolute", left = 0, right = 0, top = 0, bottom = 0, z = 7, id = "sidebar-scrim",
      bg = style.scrim.dialog, on_click = close, on_scroll = function() end, transition = style.appear(),
    })
    return widgets.card(card), 0, backdrop
  end
  -- Docked: flush with the window's edges, a step lighter than the chat,
  -- a hairline between them.
  local g = bone.hl.get("Sidebar")
  card.bg = (g and g.bg) and "Sidebar" or "Panel"
  table.insert(card, #card, { position = "absolute", top = 0, bottom = 0, right = 0, width = 1,
    bg = style.alpha("WinSeparator", "ff", "fg", "PromptBorder") })
  return style.finish(card), M.width
end

--- The sidebar's own content.
function M.content()
  local jobs = chats.jobs()
  if st.page == "running" and #jobs > 0 then bone.gui.invalidate(1000) end
  -- Active ages: a chat is archived when the window passes, and
  -- the window itself can change (bone.ui.sessions_archive_after), so the
  -- rows are made again each minute and when it changes (as in the TUI).
  -- That also picks up pins and archives made in the other frontend.
  local minute = math.floor(os.time() / 60)
  local window = chats.recent_window()
  if minute ~= st.minute or window ~= st.window then compute() end
  -- While a chat runs, its row shows a live clock: its rows are made again
  -- each second (the spinner's own canvas aside).
  local running = false
  for _, r in ipairs(st.rows) do
    if r.kind == "card" and r.status and r.status.state == "running" then running = true break end
  end
  if running then compute() end
  bone.gui.invalidate(running and 1000 or (60 - os.time() % 60) * 1000)
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
    direction = "column", padding = { 10, 8, 0, 8 }, gap = 1,
  }
  -- The search field (an icon, the field, and the key that comes to it),
  -- with projects and a new chat beside it.
  out[#out + 1] = {
    direction = "row", align = "center", gap = 2, margin = { 0, 0, 8, 0 },
    style.finish({
      direction = "row", align = "center", gap = 8, height = 32, padding = { 0, 6, 0, 10 }, margin = { 0, 4, 0, 0 },
      grow = 1, width = 0,
      -- An inset a step above the sidebar's black.
      bg = style.alpha("WinSeparator", "ff", "fg", "Normal"), radius = style.radius.md, border = 1, border_color = "PromptBorder",
      cursor = "text", on_click = function() bone.gui.focus(SEARCH) end,
      icons.node("search", { hl = "Dim", size = 15 }),
      {
        id = SEARCH, grow = 1, width = 0, size = style.size.body, hl = "Normal", on_key = on_key,
        text_input = {
          placeholder = "Search", value = st.query,
          on_change = function(text)
            if st.page ~= "chats" then st.page = "chats" end
            st.query, st.sel = text, 1
            compute(true)
            bone.gui.scroll("sessions-list", "top")
            redraw()
          end,
        },
      },
      widgets.keycap("ctrl+o"),
    }),
    st.page == "chats" and widgets.icon_button("folder", "Projects", function() st.menu = not st.menu redraw() end,
      { box = 28, size = 15, hl = st.project and "Normal" or muted, radius = style.radius.md, id = "sidebar-projects-button" }) or nil,
    widgets.icon_button("compose", "New chat", function()
      bone.gui.focus(nil)
      bone.action("new_session")
    end, { box = 28, size = 16, hl = muted, radius = style.radius.md, id = "sidebar-new" }),
  }
  out[#out + 1] = nav("chat", "Chats", st.page == "chats", function() if st.page ~= "chats" then switch_page() end end)
  out[#out + 1] = nav("terminal", "Processes", st.page == "running",
    function() if st.page ~= "running" then switch_page() end end, #jobs > 0 and #jobs or nil)
  out[#out + 1] = nav("layers", "Catalog", false, function() require("bone.catalog").open(take_keys) end)
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
  -- You, and the settings.
  local user = os.getenv("USER") or os.getenv("USERNAME") or ""
  out[#out + 1] = style.finish({
    direction = "row", align = "center", gap = 10, height = 48, shrink = 0, padding = { 0, 2, 0, 4 },
    margin = { 0, -8, 0, -8 }, border = { 1, 0, 0, 0 }, border_color = "WinSeparator",
    { width = 0, padding = { 0, 4 } },
    user ~= "" and style.finish({
      width = 24, height = 24, radius = 12, border = 1, border_color = "PromptBorder", bg = "Selection",
      justify = "center", align = "center",
      { text = user:sub(1, 1):upper(), size = style.size.micro, medium = true, hl = "Normal" },
    }) or nil,
    { text = user, size = style.size.body, hl = muted, grow = 1, wrap = false, text_overflow = "ellipsis" },
    widgets.icon_button("gear", "Settings", function() require("bone.config").open(nil, take_keys) end,
      { box = 28, size = 16, hl = muted, radius = style.radius.md, id = "sidebar-settings" }),
    { width = 4 },
  })
  -- The list fades into the card's foot rather than cutting a row in half
  -- there (as the chat fades into the composer).
  local under = overlaid and "InputBackground" or ((bone.hl.get("Sidebar") or {}).bg and "Sidebar" or "Panel")
  local clear, solid = style.alpha(under, "00", "bg"), style.alpha(under, "ff", "bg")
  if clear and solid then
    out[#out + 1] = { position = "absolute", left = 0, right = 0, bottom = 48, height = 28, z = 1,
      gradient = { clear, solid }, radius = overlaid and { 0, 0, style.radius.round - 1, style.radius.round - 1 } or nil }
  end
  out[#out + 1] = st.page == "chats" and project_menu() or nil
  return out
end

--- Open the sidebar, or close it.
function M.open()
  if open then
    close()
    return
  end
  open, st.loading, st.sel, st.picked = true, true, 1, false
  compute()
  -- Start navigation from the conversation already on screen. Up/down then
  -- previews the adjacent conversation instead of jumping from the top.
  local current = (bone.chat.session() or {}).session_id
  if current then
    for i, item in ipairs(st.items) do
      if item.kind == "chat" and sid(item) == current then st.sel = i break end
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

chats.watch(function(kind)
  if not open then return end
  if kind == "turn" or kind == "created" or kind == "updated" or kind == "deleted" then
    refresh()
  else
    compute()
    redraw()
  end
end)

return M
