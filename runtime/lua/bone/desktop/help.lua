-- The desktop library's help (/help, f1), document reader and /health, as
-- windows in the middle of the screen. Help: Commands, Keys and Docs
-- (click or tab), a search field (type to filter), each entry as a card
-- with what it does; enter puts a command in the prompt or opens its
-- documentation in the reader, which lays Markdown out as the chat does.
-- The content is bone.help's.
local M = {}

local help = require("bone.help")
local md = require("bone.desktop.markdown")
local nodes = require("bone.nodes")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")

local note, entry, hints = widgets.note, widgets.entry, widgets.hints

-- A dialog that is read: its title and × over what scrolls (`body()`'s
-- nodes), and esc. `sized`: the size of every dialog, not all the room.
local function reader(name, key, body, keys, sized)
  local id
  id = bone.ui.popup({
    node = function()
      local view = { direction = "column", grow = 1,
        widgets.dialog_head(name, { close = function() bone.ui.close(id) end }),
        widgets.list(key .. id, body(), nil, { estimate = 40, footer = hints(keys) }) }
      return sized and style.as_dialog(view) or view
    end,
    on_key = function(k)
      if k == "esc" or k == "q" or k == "ctrl+c" then bone.ui.close(id) end
      return true
    end,
  })
  return id
end

--- Read Markdown `text` in a window titled `name`; esc closes it.
function M.read(text, name)
  return reader(name or "Help", "read:", function()
    local body = { direction = "column", size = md.text_size, padding = { 0, 10 } }
    nodes.append(body, md.nodes(text or "", nil, { id = "read" }))
    return { body }
  end, "wheel scroll · esc close")
end

--- A guide or section by name ("lua", "keys"), in the reader.
function M.topic(topic)
  local text, name = help.find(topic)
  if not text then
    return bone.notify("no help for " .. topic .. " (try /help lua or /help usage)", "error")
  end
  return M.read(text, name)
end

--- The help browser.
function M.open()
  local st = { tab = 1, query = "", sel = 1 }
  local id
  local list = {}
  local on_key
  local function choose(i)
    if help.choose(list[i], M.read) then bone.ui.close(id) end
  end
  local function node()
    list = help.list(st.tab, st.query)
    st.sel = math.max(1, math.min(st.sel, math.max(1, #list)))
    -- As every dialog: the title with the sections and × at its right,
    -- the search under it (typing goes there), one line per entry, the keys.
    local top = { direction = "column", shrink = 0,
      widgets.dialog_head("Help", {
        tabs = help.TABS, tab = st.tab,
        pick = function(i)
          st.tab, st.query, st.sel = i, "", 1
          bone.ui.update(id, {})
        end,
        close = function() bone.ui.close(id) end,
      }),
      widgets.search_box(st.query, { typing = st.query ~= "", placeholder = "Search commands, keys and docs" }),
    }
    local items = {}
    if #list == 0 then items[#items + 1] = note("No matches.", { 4, 4 }) end
    local selected, group
    for i, it in ipairs(list) do
      local on = i == st.sel
      -- Keys come in groups: a quiet heading over each.
      local g = st.tab == 2 and (it.meta or ""):match("^(.-) · ") or nil
      if g and g ~= group then
        items[#items + 1] = widgets.heading(g)
      end
      group = g
      -- One line: the name (a key as a cap), and what it does.
      local lines
      if st.tab == 3 then
        -- A doc: its path takes the line, what kind it is quiet at the right.
        lines = { {
          direction = "row", gap = 10, align = "center", height = 24,
          { text = it.label, hl = "Normal", size = style.size.body, grow = 1, width = 0,
            wrap = false, text_overflow = "ellipsis" },
          { text = it.meta or "", hl = "Dim", size = style.size.small, wrap = false, shrink = 0 },
        } }
      else
        local name = st.tab == 2
          and { direction = "row", width = 150, shrink = 0, widgets.keycap(it.label) }
          or { text = it.label, hl = "Normal", size = style.size.body, width = 130, shrink = 0,
            wrap = false, text_overflow = "ellipsis" }
        lines = { {
          direction = "row", gap = 10, align = "center", height = 24,
          name,
          { text = it.desc, hl = on and "Muted" or "Dim", size = style.size.body,
            grow = 1, width = 0, wrap = false, text_overflow = "ellipsis" },
        } }
      end
      items[#items + 1] = entry(on, lines, function()
        if st.sel == i then choose(i) else st.sel = i end
        bone.ui.update(id, {})
      end)
      if on then selected = #items end
    end
    local body = widgets.list("help:" .. id, items, selected, {
      estimate = 34,
      footer = hints("↑↓ move · enter open · tab section · esc close", { 0, 0 }),
    })
    return style.as_dialog({ direction = "column", grow = 1, top, body })
  end
  function on_key(k)
    if k == "esc" or k == "ctrl+c" then
      bone.ui.close(id)
    elseif k == "tab" or k == "shift+tab" or k == "right" or k == "left" then
      local by = (k == "tab" or k == "right") and 1 or -1
      st.tab, st.query, st.sel = (st.tab - 1 + by) % #help.TABS + 1, "", 1
    elseif k == "up" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" then
      st.sel = math.min(math.max(1, #list), st.sel + 1)
    elseif k == "pageup" or k == "pagedown" then
      st.sel = math.max(1, math.min(math.max(1, #list), st.sel + (k == "pageup" and -8 or 8)))
    elseif k == "enter" then
      choose(st.sel)
    elseif bone.text.edit(st.query, k) then
      st.query, st.sel = bone.text.edit(st.query, k), 1
    else
      return false
    end
    bone.ui.update(id, {})
    return true
  end
  id = bone.ui.popup({ node = node, on_key = on_key })
  return id
end

-- /health: this window's checks, then the core's. Each with its mark: a
-- green dot, a cross for an error, an amber dot for anything else.
local function checks(name, items, out)
  out[#out + 1] = widgets.heading(name)
  for _, it in ipairs(items) do
    out[#out + 1] = {
      direction = "column", padding = { 4, style.inset },
      { direction = "row", gap = 8, align = "center",
        it.status == "error" and widgets.mark("failed") or widgets.mark(nil, it.status == "ok" and "ToolDone" or "WarningMsg"),
        { text = it.name, hl = "Normal", bold = true, size = style.size.body } },
      { text = tostring(it.message), hl = "Dim", size = style.size.small, margin = { 2, 0, 0, 0 }, padding = { 0, 0, 0, 22 } },
    }
  end
end

function M.health()
  local mine = bone.api.health()
  for _, it in ipairs(bone._run_health()) do mine[#mine + 1] = it end
  local core
  local id = reader("Health", "health:", function()
    local items = {}
    checks("This window", mine, items)
    if core then
      checks("Core", core, items)
    else
      items[#items + 1] = note("Core: checking…", { 6, 0 })
    end
    return items
  end, "esc close", true)
  bone.request("health/check", {}, function(res, err)
    core = err and { { name = "core", status = "error", message = err } } or res or {}
    bone.ui.update(id, {})
  end)
  return id
end

return M
