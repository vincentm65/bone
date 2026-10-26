-- The desktop library's picker (bone.ui.select) and pager (bone.ui.pager),
-- as cards in the middle of the screen: a picker is a filter field over
-- entries to press, a pager is text to read, a string laid out as Markdown
-- as the chat does and a list of lines as terminal text. What keys do is
-- the runtime's; these draw it.
local M = {}

local md = require("bone.desktop.markdown")
local nodes = require("bone.nodes")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local title, note, entry, hints = widgets.title, widgets.note, widgets.entry, widgets.hints

--- The picker's spec for `page` (bone.ui.windows.select): a heading, a
--- search field, the matches, and the keys.
function M.select(page)
  local st, opts = page.st, page.opts

  local function node()
    local list = page.matches()
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))
    local items = {}
    if opts.prompt then
      items[#items + 1] = { text = opts.prompt, hl = "Normal", bold = true, size = style.size.lead, padding = { 2, 8 }, margin = { 0, 0, 10, 0 } }
    end
    items[#items + 1] = {
      text = st.query == "" and "Search…" or (st.query .. "▏"), size = style.size.body, hl = st.query == "" and "Dim" or "Normal",
      bg = "InputBackground", radius = style.radius.lg, padding = { 8, 12 }, margin = { 0, 0, 10, 0 },
      border = 1, border_color = "Accent",
    }
    if st.loading then
      items[#items + 1] = note("Loading…", { 4, 4 })
    elseif #list == 0 then
      items[#items + 1] = note(#st.items == 0 and (opts.empty or "Nothing to pick") or "No matches", { 4, 4 })
    end
    local selected
    for i, m in ipairs(list) do
      items[#items + 1] = entry(i == st.sel, { { text = m.text, hl = "Normal", bold = i == st.sel, size = style.size.body } },
        function() page.finish(m) end)
      if i == st.sel then selected = #items end
    end
    local footer = opts.footer == nil and "↑↓ move · enter open · esc close" or opts.footer
    return widgets.list("select:" .. page.id, items, selected,
      { fit = true, footer = footer and hints(footer, { 10, 0 }) or nil })
  end

  return { node = node }
end

--- The pager's spec for `page` (bone.ui.windows.pager). Its keys scroll the
--- window's list, which the wheel does too.
function M.pager(page)
  local opts = page.opts
  local list

  local function node()
    list = "pager:" .. page.id
    local content = page.content()
    local body
    if type(content) == "string" then
      body = { direction = "column", size = md.text_size, padding = { 0, 10 } }
      nodes.append(body, md.nodes(content))
    else
      body = require("bone.nodes").lines_node(content)
      body.padding = { 0, 10 }
    end
    local items = { body }
    if opts.title then table.insert(items, 1, title(opts.title)) end
    return widgets.list(list, items, nil, { fit = true, footer = hints("↑↓ scroll · esc close", { 8, 0 }) })
  end

  local function on_key(k)
    local at = bone.gui.scroll_info(list or "") or { offset = 0, viewport = 0 }
    local by = ({ up = -40, k = -40, wheelup = -40, down = 40, j = 40, wheeldown = 40 })[k]
    if k == "esc" or k == "q" or k == "ctrl+c" then
      page.close()
    elseif by then
      bone.gui.scroll(list, math.max(0, at.offset + by))
    elseif k == "pageup" or k == "pagedown" or k == "space" then
      bone.gui.scroll(list, math.max(0, at.offset + (k == "pageup" and -1 or 1) * at.viewport * 0.9))
    elseif k == "home" or k == "g" then
      bone.gui.scroll(list, "top")
    elseif k == "end" or k == "G" then
      bone.gui.scroll(list, "end")
    else
      return false
    end
    return true
  end

  return { node = node, on_key = on_key }
end

return M
