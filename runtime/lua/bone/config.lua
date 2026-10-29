-- /config for the TUI: General, Providers and Plugins, with settings detail
-- pages, as a pane resting on the prompt. The page's state and actions are
-- bone.settings_page (options saved as tui.<name>); this draws it.
--
-- require("bone.config").open(tab) opens a section or detail page by name or
-- title ("providers", "web_search").

local M = {}

local function text_width(s)
  return bone.text.width(s)
end

local function pad(s, w)
  s = tostring(s)
  local n = text_width(s)
  if n >= w then
    return bone.text.truncate(s, w)
  end
  return s .. string.rep(" ", w - n)
end

function M.open(want)
  local id, paste_event
  local page = require("bone.settings_page").new(want, {
    scope = "tui",
    changed = function()
      if id then
        bone.ui.update(id, {})
      end
    end,
    close = function()
      if paste_event then
        bone.off(paste_event)
        paste_event = nil
      end
      bone.ui.close(id)
    end,
  })
  local st, rows = page.st, page.rows

  -- Match the catalog's restrained palette: Accent is the theme's blue, while
  -- Dim carries secondary values and help text in a quiet grey.
  local MUTED, KEY, NOTE = "Dim", "Accent", "Dim"

  -- A line of { text, group } items, cut to the width as it grows.
  local function line(w, selected)
    local items, used = {}, 0
    local function add(text, hl)
      text = tostring(text)
      if used >= w or text == "" then
        return
      end
      if used + text_width(text) > w then
        text = bone.text.truncate(text, w - used)
      end
      hl = hl or "Normal"
      items[#items + 1] = { text, selected and ("ConfigSelected" .. hl) or hl }
      used = used + text_width(text)
    end
    return items, add
  end

  -- Keep the current choice identifiable even without color.
  local function choices_into(add, list, value)
    for i, c in ipairs(list) do
      if i > 1 then
        add(" · ", MUTED)
      end
      add(c == value and ("[" .. tostring(c) .. "]") or tostring(c), c == value and KEY or MUTED)
    end
  end

  local function switch_into(add, on)
    if on then
      add("● on", KEY)
    else
      add("○ off", MUTED)
    end
  end

  -- A row's value, added to a line.
  local function value_into(add, row)
    if row.kind == "provider" then
      add(pad(row.model or "", 22))
      add(pad(row.url or "", 30), MUTED)
      if row.current then
        add("  ● in use  ", KEY)
      end
      if row.added then
        add("added", MUTED)
      end
    elseif row.kind == "paction" then
      return
    elseif row.kind == "pfield" then
      if row.type == "boolean" then
        switch_into(add, row.value)
      elseif row.type == "choice" then
        choices_into(add, row.choices, row.value)
      elseif row.type == "secret" and st.prov and st.prov.new then
        if row.value ~= "" then
          add(string.rep("•", math.min(#row.value, 24)))
        else
          add("none", MUTED)
        end
      elseif row.key == "type" and (row.value == nil or row.value == "") then
        add("OpenAI-compatible")
      elseif row.value == nil or row.value == "" then
        add("none", MUTED)
      else
        add(tostring(row.value))
      end
      if row.changed then
        add("   changed", NOTE)
      end
    elseif row.kind == "page" then
      add("›", KEY)
    elseif row.kind == "plugin" then
      switch_into(add, row.on)
      add(row.on and "    " or "   ")
      add((row.tui and "tui" or "") .. (row.tui and row.core and " + " or "") .. (row.core and "core" or ""), MUTED)
      if row.error then
        add("  " .. row.error, "ErrorMsg")
      end
      -- Enter opens its settings page.
      if row.page then
        add("  ›", KEY)
      end
    elseif row.type == "boolean" then
      switch_into(add, row.value)
    elseif row.choices and #row.choices > 0 then
      choices_into(add, row.choices, row.value)
    elseif row.value == nil then
      add("unset", MUTED)
    else
      add(tostring(row.value))
    end
  end

  -- Key hints: the key bold, what it does dim.
  local function hints_into(add, pairs_)
    add("   ")
    for i, h in ipairs(pairs_) do
      if i > 1 then
        add("   ")
      end
      add(h[1], KEY)
      add(" " .. h[2], MUTED)
    end
  end

  -- A full-width pane resting on the prompt.
  local function render(ctx)
    local w = math.max(ctx.width, 1)
    -- Preserve each span's foreground/emphasis on a full-row selection background.
    -- Refresh on render so changing the colorscheme also updates an open page.
    local selection = bone.hl.get("Selection") or {}
    for _, group in ipairs({ "Normal", "Dim", "Accent", "ErrorMsg" }) do
      local style = bone.hl.get(group) or {}
      style.bg = selection.bg
      local name = "ConfigSelected" .. group
      local current = bone.hl.get(name) or {}
      for _, key in ipairs({ "fg", "bg", "bold", "italic", "underline", "reverse", "dim" }) do
        if current[key] ~= style[key] then
          bone.hl.set(name, style)
          break
        end
      end
    end
    local t = st.tabs[st.tab]
    local list = rows()
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))

    local out = {}
    -- The title in the top rule.
    local section = t.section or st.tab
    local title = t.page and (st.tabs[section].title .. " › " .. t.title) or (st.prov and (st.prov.new and "new provider" or st.prov.name))
    local top = { { "── ", "WinSeparator" }, { "Settings", "Accent" } }
    if title then
      top[#top + 1] = { " › ", MUTED }
      top[#top + 1] = { title, "Accent" }
    end
    top[#top + 1] = { " ", MUTED }
    top[#top + 1] = { fill = "─", hl = "WinSeparator" }
    out[1] = top

    -- The tabs, the current one bold and underlined, wrapped to the width.
    if #st.tabs > 1 then
      local items, used = { { "  " } }, 2
      for i, tab in ipairs({ st.tabs[1], st.tabs[2], st.tabs[3] }) do
        local n = text_width(tab.title) + 3
        if used + n > w then
          out[#out + 1] = items
          items, used = { { "  " } }, 2
        end
        items[#items + 1] = { " " }
        items[#items + 1] = { tab.title, i == section and "Accent" or "Dim" }
        items[#items + 1] = { "  " }
        used = used + n
      end
      out[#out + 1] = items
    end
    -- The search, in the line under the tabs (blank without one).
    if st.search or st.query ~= "" then
      out[#out + 1] = { { "    / ", KEY }, { st.query, "Normal" }, { st.search and "▏" or "", KEY } }
    else
      out[#out + 1] = ""
    end

    local label_w = 0
    for _, r in ipairs(list) do
      label_w = math.max(label_w, text_width(r.label))
    end
    label_w = math.min(label_w + 4, math.floor(w / 2))

    -- Reserve ten slots on every page (fewer on short terminals), plus a
    -- permanent footer/counter slot so empty and overflowing pages stay level.
    local room = math.max(1, math.min(10, (ctx.height or 24) - #out - 5))
    local body_start = #out
    if t.name == "providers" and not st.providers then
      out[#out + 1] = { { "    loading…", MUTED } }
    elseif #list == 0 and st.query ~= "" then
      out[#out + 1] = { { "    no matches", MUTED } }
    elseif #list == 0 then
      out[#out + 1] = { { "    " .. (t.name == "plugins" and "no plugins in ~/.bone/plugins" or "nothing to set here"), MUTED } }
    end
    local first = math.max(1, st.sel - room + 1)
    for i = first, math.min(#list, first + room - 1) do
      local r = list[i]
      local items, add = line(w, i == st.sel)
      if i == st.sel then
        add("  › ", KEY)
        add(pad(r.label, label_w))
      else
        add("    " .. pad(r.label, label_w))
      end
      if i == st.sel and st.edit then
        add(r.type == "secret" and string.rep("•", #st.edit) or st.edit)
        add("▏")
      else
        value_into(add, r)
        -- tui.lua runs last; say so when it overrides the saved value.
        if r.kind == "option" then
          local saved = bone.settings.get(r.path)
          if saved ~= nil and saved ~= r.value then
            add("   tui.lua sets " .. tostring(r.value), NOTE)
          end
        end
      end
      if i == st.sel then
        items[#items + 1] = { fill = " ", hl = "ConfigSelectedNormal" }
      end
      out[#out + 1] = items
    end
    while #out < body_start + room do
      out[#out + 1] = ""
    end
    if #list > room then
      out[#out + 1] = { { ("    %d–%d of %d"):format(first, math.min(#list, first + room - 1), #list), MUTED } }
    else
      out[#out + 1] = ""
    end

    out[#out + 1] = ""
    local current = list[st.sel]
    local items, add = line(w)
    if st.ask then
      add("    " .. st.ask.text, KEY)
    elseif st.note and st.note[2] == "ErrorMsg" then
      add("    error: " .. st.note[1], "ErrorMsg")
    elseif st.note then
      add("    " .. st.note[1])
    else
      add("    " .. (current and current.desc or ""), NOTE)
    end
    out[#out + 1] = items
    out[#out + 1] = ""
    local hint
    if st.ask then
      hint = { { "y", "yes" }, { "n", "no" } }
    elseif st.search then
      hint = { { "↑↓", "move" }, { "enter", "choose" }, { "esc", "clear" } }
    elseif st.edit then
      hint = { { "enter", "save" }, { "esc", "cancel" } }
    elseif t.page then
      hint = { { "↑↓", "move" }, { "/", "search" }, { "enter", "change" }, { "r", "reset" }, { "esc", "back" } }
    elseif t.name == "providers" and st.prov then
      hint = { { "↑↓", "move" }, { "/", "search" }, { "enter", "change" }, { "r", "reset to core.lua" }, { "esc", "back" } }
    elseif t.name == "providers" then
      hint = { { "↑↓", "move" }, { "/", "search" }, { "enter", "use" }, { "e", "edit" }, { "a", "add" }, { "d", "delete" }, { "tab", "section" }, { "esc", "close" } }
    elseif t.name == "plugins" then
      hint = { { "↑↓", "move" }, { "/", "search" }, { "enter", "settings" }, { "space", "on/off" }, { "tab", "section" }, { "esc", "close" } }
    else
      hint = { { "↑↓", "move" }, { "/", "search" }, { "enter", "change" }, { "r", "reset" }, { "tab", "section" }, { "esc", "close" } }
    end
    items, add = line(w)
    hints_into(add, hint)
    out[#out + 1] = items
    return out
  end

  id = bone.ui.popup({ lines = render, on_key = page.on_key, anchor = "prompt", width = false })
  paste_event = bone.on("paste", function(ev)
    if ev.context == "popup" then
      page.paste(ev.text)
    end
  end)
  return id
end

return M
