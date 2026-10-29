-- The desktop library's settings (/config, /settings, /model): a window in
-- the middle of the screen, as a card. Its heading is the title and the
-- sections (General, Providers, Plugins; click or tab to change), or, on a
-- page of its own, a back button and where you are. Each setting is a line:
-- its name on the left, and on the right its value as what changes it (a
-- switch, the choices, the text being typed), then a › where it opens a
-- page; the selected one raised with what it does under it.
-- At the bottom, what happened and the keys. The page's state and actions
-- are bone.settings_page; options are saved as gui.<name>. Opened in place
-- of bone.config, so every way in comes here.
local M = {}

local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local SIZE, CAPTION, SMALL = style.size.body, style.size.caption, style.size.small
-- The widest a value's text gets, in pixels; the name has the rest.
local VALUE = 300
-- Choices up to this many are a segmented switch; more, the value as text.
local SEGMENTS = 5

local function hints(st, t)
  if st.ask then return "y yes · n no" end
  if st.edit then return "enter save · esc cancel" end
  if st.search then return "↑↓ move · enter choose · esc clear" end
  if t.page or st.prov then return "↑↓ move · / search · enter change · r reset · esc back" end
  if t.name == "providers" then return "↑↓ move · / search · enter use · e edit · a add · d delete · esc close" end
  if t.name == "plugins" then return "↑↓ move · / search · enter settings · space on/off · esc close" end
  return "↑↓ move · / search · enter change · r reset · tab section · esc close"
end

-- A value as text, cut to fit.
local function text(value, hl, size)
  return { text = value, hl = hl or "Dim", size = size or CAPTION, max_width = VALUE,
    wrap = false, text_overflow = "ellipsis" }
end

function M.open(want)
  local id, paste_event
  local page
  page = require("bone.settings_page").new(want, {
    scope = "gui",
    changed = function() if id then bone.ui.update(id, {}) end end,
    close = function()
      if paste_event then bone.off(paste_event) paste_event = nil end
      bone.ui.close(id)
    end,
  })
  local st = page.st
  local function refresh() bone.ui.update(id, {}) end

  -- A press on a row selects it; on the selected one it acts (enter).
  local function on_click(i)
    return function()
      local list = page.rows()
      if st.sel == i then page.activate(list[i], false) else st.sel = i end
      refresh()
    end
  end
  -- A press on a row's control selects the row and acts at once.
  local function act(i, fn)
    return function()
      st.sel = i
      fn()
      refresh()
    end
  end

  -- What a row holds on its right: the control that changes it, or its value.
  local function value_node(r, i, on)
    if on and st.edit then
      local typed = r.type == "secret" and string.rep("•", #st.edit) or st.edit
      return {
        width = VALUE, bg = "InputBackground", border = 1, border_color = "Accent",
        radius = style.radius.sm, padding = { 2, 8 },
        { text = typed .. "▏", hl = "Normal", size = CAPTION, wrap = false, text_overflow = "ellipsis" },
      }
    end
    if r.kind == "provider" then
      return {
        direction = "row", align = "center", gap = style.space.md, shrink = 1,
        text((r.model or "") .. (r.url and r.url ~= "" and ("  ·  " .. r.url:gsub("^%w+://", "")) or "")),
        r.current and widgets.pill("IN USE") or nil,
      }
    elseif r.kind == "plugin" then
      local where = (r.tui and "gui" or "") .. (r.tui and r.core and " + " or "") .. (r.core and "core" or "")
      return {
        direction = "row", align = "center", gap = style.space.md, shrink = 1,
        text(r.error or where, r.error and "ErrorMsg" or "Dim", SMALL),
        widgets.switch(r.on, act(i, function() page.activate(r, true) end)),
      }
    elseif r.kind == "paction" or r.kind == "page" then
      return nil
    elseif r.type == "boolean" then
      return widgets.switch(r.value, act(i, function() page.activate(r, false) end))
    elseif r.choices and #r.choices > 0 and #r.choices <= SEGMENTS then
      local names, current = {}, 0
      for k, c in ipairs(r.choices) do
        names[k] = tostring(c)
        if c == r.value then current = k end
      end
      return widgets.segmented(names, current, function(k)
        act(i, function() page.set(r, r.choices[k]) end)()
      end, { padding = 1, shrink = 0, item = { size = SMALL, padding = { 1, 8 } } })
    elseif r.type == "secret" and st.prov and st.prov.new then
      return text(r.value ~= "" and string.rep("•", math.min(#r.value, 24)) or "none")
    elseif r.key == "type" and (r.value == nil or r.value == "") then
      return text("OpenAI-compatible", "Normal")
    elseif r.value == nil or r.value == "" then
      return text(r.kind == "option" and "unset" or "none")
    end
    return text(tostring(r.value) .. (r.changed and "  · changed" or ""), "Normal")
  end

  -- The heading: the title and the sections, or, on a page of its own, a
  -- way back and where you are.
  local function heading(t, section)
    local row = { direction = "row", align = "center", gap = style.space.md, padding = { 0, 4 }, margin = { 0, 0, 6, 0 } }
    if t.page or st.prov then
      local here = t.page and t.title or (st.prov.new and "New provider" or st.prov.name)
      row[#row + 1] = widgets.icon_button("‹", "Back", function() page.on_key("esc"); refresh() end,
        { size = style.size.lead, box = 26 })
      row[#row + 1] = {
        spans = {
          { text = "Settings  ›  " .. st.tabs[t.page and section or 2].title .. "  ›  ", hl = "Dim" },
          { text = here, hl = "Normal", bold = true },
        },
        size = style.size.lead, grow = 1, width = 0, wrap = false, text_overflow = "ellipsis",
      }
      return row
    end
    local names = {}
    for i = 1, 3 do names[i] = st.tabs[i].title end
    row[#row + 1] = { text = "Settings", hl = "Normal", bold = true, size = style.size.lead, grow = 1 }
    row[#row + 1] = widgets.tabs(names, section, function(i)
      page.switch_tab(i - st.tab)
      refresh()
    end, { margin = { 0, 0, 0, 0 }, padding = 0 })
    return row
  end

  -- The search: a box under the heading; a press (or /) types into it.
  local function search()
    local typing = st.search
    local shown = typing and (st.query .. "▏") or (st.query ~= "" and st.query or "Search")
    return widgets.press({
      direction = "row", align = "center", gap = style.space.sm, margin = { 0, 0, 6, 0 }, padding = { 3, 8 },
      bg = "InputBackground", radius = style.radius.sm, border = 1,
      border_color = typing and "Accent" or "PromptBorder",
      { text = "⌕", hl = "Dim", size = CAPTION },
      { text = shown, hl = (typing or st.query ~= "") and "Normal" or "Dim", size = CAPTION, grow = 1, width = 0,
        wrap = false, text_overflow = "ellipsis" },
      st.query ~= "" and widgets.icon_button("✕", "Clear the search", function()
        st.query, st.search, st.sel = "", false, 1
        refresh()
      end, { box = 16, size = SMALL }) or nil,
    }, function()
      if not st.search then page.on_key("/") end
      refresh()
    end, "button", false)
  end

  local function node()
    local t = st.tabs[st.tab]
    local items = { heading(t, t.section or st.tab) }

    local list = page.rows()
    items[#items + 1] = search()
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))
    if t.name == "providers" and not st.providers then
      items[#items + 1] = widgets.note("Loading…", { 2, 2 })
    elseif #list == 0 and st.query ~= "" then
      items[#items + 1] = widgets.note("No matches", { 2, 2 })
    elseif #list == 0 then
      items[#items + 1] = widgets.note(t.name == "plugins" and "No plugins in ~/.bone/plugins" or "Nothing to set here", { 2, 2 })
    end
    local selected
    for i, r in ipairs(list) do
      local on = i == st.sel
      -- Pages (Compaction) after the options, set apart.
      if r.kind == "page" and i > 1 and list[i - 1].kind ~= "page" then
        items[#items + 1] = widgets.divider({ margin = { 4, 0, 4, 0 } })
      end
      local action = r.kind == "paction"
      local danger = action and (r.action == "delete" or r.action == "undo")
      -- The name takes what the control leaves; the › has a column of its
      -- own, so the controls line up whether a row opens a page or not.
      local row = {
        {
          direction = "row", align = "center", gap = style.space.md, min_height = 20,
          {
            text = r.label, size = SIZE, bold = on or action, grow = 1, width = 0, wrap = false,
            text_overflow = "ellipsis", hl = danger and "ErrorMsg" or action and "Accent" or "Normal",
          },
          value_node(r, i, on),
          {
            text = (r.kind == "page" or r.page) and "›" or "", hl = "Dim", size = SIZE,
            width = 10, shrink = 0, text_align = "right",
          },
        },
      }
      local function add(line)
        row[#row + 1] = { text = line, hl = "Dim", size = SMALL, margin = { 2, 0, 0, 0 } }
      end
      if r.kind == "option" and not (on and st.edit) then
        local saved = bone.settings.get(r.path)
        if saved ~= nil and saved ~= r.value then add("gui.lua sets " .. tostring(r.value)) end
      end
      items[#items + 1] = widgets.entry(on, row, on_click(i))
      if on then selected = #items end
    end

    -- Under the rows, one space for a question, what happened, or what the
    -- selected setting does: rows never change height as the selection moves.
    local detail
    if st.ask then
      local small = { size = CAPTION, padding = { 3, 14 } }
      detail = widgets.detail({
        direction = "row", align = "center", gap = style.space.md,
        { text = st.ask.text, hl = "WarningMsg", size = CAPTION, grow = 1, width = 0 },
        widgets.button("Yes", function() page.on_key("y"); refresh() end, true, false, small),
        widgets.button("No", function() page.on_key("n"); refresh() end, false, false, small),
      })
    elseif st.note then
      detail = widgets.detail(st.note[1], st.note[2] == "ErrorMsg" and "ErrorMsg" or "Dim")
    else
      detail = widgets.detail(list[st.sel] and list[st.sel].desc)
    end
    -- The one dialog size, whichever section or page it shows.
    return style.as_dialog(widgets.list("settings:" .. id, items, selected, {
      estimate = 30,
      footer = { direction = "column", widgets.divider({ margin = { 4, 0, 0, 0 } }), detail,
        widgets.hints(hints(st, t), { 0, 0 }) },
    }))
  end

  id = bone.ui.popup({ node = node, on_key = page.on_key })
  paste_event = bone.on("paste", function(ev)
    if ev.context == "popup" then page.paste(ev.text) end
  end)
  return id
end

return M
