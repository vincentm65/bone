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
-- The options in groups, each under its name; one in none of them (a
-- plugin's) is under the last.
local GROUPS = {
  { "Appearance", "font", "font_sans", "font_sans_bold", "font_sans_italic", "font_size", "opacity", "titlebar",
    "widget_radius", "window_radius" },
  { "Chat", "show_reasoning", "tool_detail", "tool_preview_lines", "diff_preview_lines", "fold_turns" },
  { "Input", "prompt_max_height", "queue_mode", "scroll_line", "wheel_speed", "mouse", "timeoutlen" },
  { "More" },
}
local GROUP = {}
for i, g in ipairs(GROUPS) do
  for j = 2, #g do GROUP[g[j]] = i end
end

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
    group = function(name) return GROUP[name] or #GROUPS end,
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

  -- A value in an outlined box, as a select reads.
  local function boxed(value, hl)
    return style.finish({
      direction = "row", align = "center", height = 26, padding = { 0, 10 }, shrink = 1, max_width = VALUE,
      border = 1, border_color = "PromptBorder", radius = style.radius.md,
      { text = value, size = SMALL, hl = hl or "Normal", wrap = false, text_overflow = "ellipsis", shrink = 1 },
    })
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
        r.current and widgets.pill("In use") or nil,
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
      if r.kind == "option" then return boxed("Default", "Dim") end
      return text("none")
    elseif r.kind == "option" and type(r.value) == "number" then
      -- A number steps: − and + either side of it, in one outlined box.
      local v = r.value
      local step = (v % 1 ~= 0) and 0.1 or 1
      local function set(by)
        return act(i, function()
          local n = math.floor((v + by) / step + 0.5) * step
          page.set(r, step < 1 and tonumber(string.format("%.2f", n)) or math.floor(n + 0.5))
        end)
      end
      local function key(glyph, by)
        return widgets.press(style.finish({ width = 24, height = 24, justify = "center", align = "center",
          { text = glyph, size = SMALL, hl = "Dim" } }), set(by), "button")
      end
      return style.finish({
        direction = "row", align = "center", height = 26, shrink = 0,
        border = 1, border_color = "PromptBorder", radius = style.radius.md, overflow = "hidden",
        key("−", -step),
        { text = (step < 1 and string.format("%.1f", v) or tostring(math.floor(v))), font = "mono", size = SMALL,
          hl = "Normal", min_width = 30, text_align = "center", padding = { 0, 6 }, wrap = false,
          border = { 0, 1, 0, 1 }, border_color = "PromptBorder" },
        key("+", step),
      })
    elseif r.kind == "option" and type(r.value) == "string" then
      return boxed(tostring(r.value), "Normal")
    end
    return text(tostring(r.value) .. (r.changed and "  · changed" or ""), "Normal")
  end

  -- The heading: the title and the sections, or, on a page of its own, a
  -- way back and where you are.
  local function heading(t, section)
    if t.page or st.prov then
      local row = { direction = "row", align = "center", gap = style.space.md, min_height = 30, margin = { 0, 0, 6, 0 } }
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
    return widgets.dialog_head("Settings", {
      tabs = names, tab = section,
      pick = function(i) page.switch_tab(i - st.tab) refresh() end,
      close = function() page.on_key("esc") refresh() end,
    })
  end

  -- The search: a box under the heading; a press (or /) types into it.
  local function search()
    return widgets.search_box(st.query, {
      typing = st.search, key = "/",
      clear = function() st.query, st.search, st.sel = "", false, 1 refresh() end,
      click = function()
        if not st.search then page.on_key("/") end
        refresh()
      end,
    })
  end

  local function node()
    local t = st.tabs[st.tab]
    -- The heading and the search stay put over the list, as help's do.
    local top = { direction = "column", shrink = 0, heading(t, t.section or st.tab), search() }
    local items = {}

    local list = page.rows()
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
      if r.group and r.group ~= (list[i - 1] or {}).group then
        items[#items + 1] = widgets.heading(GROUPS[r.group][1])
      end
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
            direction = "column", grow = 1, width = 0,
            {
              text = r.kind == "option" and (r.label:sub(1, 1):upper() .. r.label:sub(2)) or r.label,
              -- A provider's name is what it is called in core.lua and /provider.
              size = SIZE, medium = true, wrap = false, font = r.kind == "provider" and "mono" or nil,
              text_overflow = "ellipsis", hl = danger and "ErrorMsg" or "Normal",
            },
            -- What it does, under its name.
            (r.desc and r.desc ~= "" and r.kind ~= "provider") and {
              text = r.desc:sub(1, 1):upper() .. r.desc:sub(2), size = SMALL, hl = "Dim", wrap = false,
              text_overflow = "ellipsis", margin = { 2, 0, 0, 0 },
            } or nil,
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

    -- Under the rows, a space for a question or what happened (what a
    -- setting does is under its name).
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
    end
    -- The one dialog size, whichever section or page it shows.
    return style.as_dialog({ direction = "column", grow = 1, top, widgets.list("settings:" .. id, items, selected, {
      estimate = 44,
      footer = { direction = "column", detail, widgets.hints(hints(st, t), { 0, 0 }) },
    }) })
  end

  id = bone.ui.popup({ node = node, on_key = page.on_key })
  paste_event = bone.on("paste", function(ev)
    if ev.context == "popup" then page.paste(ev.text) end
  end)
  return id
end

return M
