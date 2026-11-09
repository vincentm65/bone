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

local title, note, entry, hints = widgets.title, widgets.note, widgets.entry, widgets.hints

--- Read Markdown `text` in a window titled `name`; esc closes it.
function M.read(text, name)
  local id
  id = bone.ui.popup({
    node = function()
      local body = { direction = "column", size = md.text_size, padding = { 0, 10 } }
      nodes.append(body, md.nodes(text or ""))
      return widgets.list("read:" .. id, { title(name or "Help"), body },
        nil, { footer = hints("wheel scroll · esc close") })
    end,
    on_key = function(k)
      if k == "esc" or k == "q" or k == "ctrl+c" then bone.ui.close(id) end
      return true
    end,
  })
  return id
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
    -- A palette: the search is the top line (the tabs at its right), set
    -- off by a hairline; then one line per entry, and the keys.
    local icons = require("bone.nodes.icons")
    local top = style.finish({
      direction = "row", align = "center", gap = 10, height = 50, shrink = 0,
      margin = { -6, -style.inset - 4, 4, -style.inset - 4 }, padding = { 0, style.inset + 4, 0, style.inset + 6 },
      border = { 0, 0, 1, 0 }, border_color = "WinSeparator",
      icons.node("search", { size = 16, hl = "Dim" }),
      { text = st.query == "" and "Search commands, keys and docs" or (st.query .. "▏"),
        hl = st.query == "" and "Dim" or "Normal", size = style.size.text, grow = 1, width = 0, wrap = false,
        text_overflow = "ellipsis" },
      widgets.segmented(help.TABS, st.tab, function(i)
        st.tab, st.query, st.sel = i, "", 1
        bone.ui.update(id, {})
      end, { align_self = "center" }),
      widgets.icon_button("close", "Close (esc)", function() bone.ui.close(id) end,
        { box = 28, size = 15, radius = style.radius.md }),
    })
    local items = {}
    if #list == 0 then items[#items + 1] = note("No matches.", { 4, 4 }) end
    local selected, group
    for i, it in ipairs(list) do
      local on = i == st.sel
      -- Keys come in groups: a quiet heading over each.
      local g = st.tab == 2 and (it.meta or ""):match("^(.-) · ") or nil
      if g and g ~= group then
        items[#items + 1] = { text = g, hl = "Dim", size = style.size.small, medium = true,
          padding = { 8, style.inset, 4, style.inset } }
      end
      group = g
      -- One line: the name (a key as a cap), and what it does.
      local name = st.tab == 2
        and { direction = "row", width = 150, shrink = 0, widgets.keycap(it.label) }
        or { text = it.label, hl = "Normal", size = style.size.body, width = 130, shrink = 0,
          wrap = false, text_overflow = "ellipsis" }
      local lines = { {
        direction = "row", gap = 10, align = "center", height = 24,
        name,
        { text = st.tab == 3 and it.meta or it.desc, hl = on and "Muted" or "Dim", size = style.size.body,
          grow = 1, width = 0, wrap = false, text_overflow = "ellipsis" },
      } }
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

-- /health: this window's checks, then the core's.
local MARK = { ok = { "•", "ToolDone" }, warn = { "!", "WarningMsg" }, error = { "×", "ErrorMsg" } }
local function checks(name, items, out)
  out[#out + 1] = { text = name, hl = "Dim", medium = true, size = style.size.small, padding = { 2, style.inset }, margin = { 6, 0, 2, 0 } }
  for _, it in ipairs(items) do
    local m = MARK[it.status] or MARK.warn
    out[#out + 1] = {
      direction = "column", padding = { 4, style.inset },
      { spans = { { text = m[1] .. "   ", hl = m[2], bold = true }, { text = it.name, hl = "Normal", bold = true } }, size = style.size.body },
      { text = tostring(it.message), hl = "Dim", size = style.size.small, margin = { 2, 0, 0, 0 }, padding = { 0, 0, 0, 22 } },
    }
  end
end

function M.health()
  local mine = bone.api.health()
  for _, it in ipairs(bone._run_health()) do mine[#mine + 1] = it end
  local core
  local id
  id = bone.ui.popup({
    node = function()
      local items = { title("Health") }
      checks("This window", mine, items)
      if core then
        checks("Core", core, items)
      else
        items[#items + 1] = note("Core: checking…", { 6, 0 })
      end
      return style.as_dialog(widgets.list("health:" .. id, items, nil, { estimate = 40, footer = hints("esc close") }))
    end,
    on_key = function(k)
      if k == "esc" or k == "q" or k == "ctrl+c" then bone.ui.close(id) end
      return true
    end,
  })
  bone.request("health/check", {}, function(res, err)
    core = err and { { name = "core", status = "error", message = err } } or res or {}
    bone.ui.update(id, {})
  end)
  return id
end

return M
