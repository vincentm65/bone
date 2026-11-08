-- The desktop library's /setup (and the window that opens by itself on a
-- first run): a card the size of every dialog, a welcome with buttons, then the
-- provider as a card to choose, its details as a form, and the plugins to
-- add. The steps, their rows and what keys do are bone.setup's page; this
-- draws it.
local M = {}

local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local entry, button, field, hints = widgets.entry, widgets.button, widgets.field, widgets.hints

local STEPS = { provider = 1, details = 2, packages = 3 }
local NAMES = { "Provider", "Details", "Packages" }

-- Where you are: Provider ─ Details ─ Packages.
local function steps(st)
  local at = STEPS[st.step]
  if not at then return nil end
  local spans = {}
  for i, name in ipairs(NAMES) do
    if i > 1 then spans[#spans + 1] = { text = "   ─   ", hl = "Dim" } end
    local done = i < at
    spans[#spans + 1] = {
      text = (done and "✓ " or (i .. "  ")) .. name,
      hl = i == at and "Accent" or (done and "ToolDone" or "Dim"), bold = i == at,
    }
  end
  return { spans = spans, size = style.size.small, padding = { 2, 8 }, margin = { 0, 0, 6, 0 } }
end

-- A heading and a line under it; `size` defaults to a window's title.
local function heading(text, sub, hl, size)
  return {
    direction = "column", padding = { 2, 8 }, margin = { 0, 0, 6, 0 },
    { text = text, hl = hl or "Normal", bold = true, size = size or style.size.title },
    sub and { text = sub, hl = "Dim", size = style.size.body, margin = { 2, 0, 0, 0 } } or nil,
  }
end

-- A label over a group of entries.
local function section(text)
  return { text = text:upper(), hl = "Dim", size = style.size.micro, bold = true, padding = { 8, 10, 2, 10 } }
end

local function paragraph(text)
  return { text = text, hl = "Normal", size = style.size.body, padding = { 2, 8 }, margin = { 0, 0, 6, 0 } }
end

-- Buttons side by side.
local function buttons(...)
  return { direction = "row", gap = 8, margin = { 10, 0, 0, 0 }, padding = { 0, 4 }, ... }
end

--- The window's spec for `page` (bone.ui.windows.setup).
function M.view(page)
  local st = page.st
  local setup = require("bone.setup")
  local presets = setup.PRESETS

  -- A press selects, and acts on what is already selected.
  local function click(i)
    return function()
      if st.sel == i then page.on_key("enter") else st.sel = i end
    end
  end
  local function press(key) return function() page.on_key(key) end end

  local function node()
    local items = {}
    local selected
    local function add(n, on)
      items[#items + 1] = n
      if on then selected = #items end
    end
    local bar = steps(st)
    if bar then add(bar) end

    if st.step == "welcome" then
      add(heading("Welcome to bone", "Connect a model and start talking.", nil, style.size.display))
      add(paragraph("Bone needs a model provider: a server on this machine, or a hosted service. "
        .. "This sets one up and can add plugins from the catalog. It is saved in ~/.bone/settings.json; "
        .. "your core.lua is not touched."))
      add({ text = "Make this window yours with ~/.bone/gui.lua, or /gui create personal for a full copy.",
        hl = "Dim", size = style.size.small, padding = { 2, 8 } })
      for _, p in ipairs(st.broken or {}) do
        add({ text = "Plugin " .. p.name .. " was not loaded: " .. setup.plain_error(p.error), hl = "ErrorMsg",
          size = style.size.caption, padding = { 2, 8 }, margin = { 6, 0, 0, 0 } })
      end
      add(buttons(button("Get started", press("enter"), true),
        button(page.opts.first_run and "Not now" or "Close", press("esc"))))
    elseif st.step == "applying" or st.step == "done" then
      local done = st.step == "done"
      if done then
        add(heading("You're all set", nil, "ToolDone"))
      else
        add({ direction = "row", align = "center", gap = 10, padding = { 2, 8 }, margin = { 0, 0, 6, 0 },
          widgets.spinner(18),
          { text = "Setting things up…", hl = "Normal", bold = true, size = style.size.title } })
      end
      -- Each thing done: an accent ▸ while it runs, then a green ✓ (✗ when it failed).
      for _, e in ipairs(st.log) do
        local active, failed = e.state == "active", e.state == "error"
        add({ direction = "row", gap = 6, align = "start", padding = { 1, 10 },
          { text = active and "▸" or failed and "✗" or "✓", hl = active and "Accent" or failed and "ErrorMsg" or "DiffAdd",
            bold = true, font = "mono", size = style.size.body, width = 14, wrap = false, shrink = 0 },
          { text = e.text, hl = active and "Normal" or failed and "ErrorMsg" or "Dim", size = style.size.body,
            grow = 1, width = 0 } })
      end
      if done then
        add(widgets.divider({ margin = { 10, 8, 4, 8 } }))
        add(section("Try asking"))
        for i, prompt in ipairs(st.first_prompts) do
          local on = i == st.sel
          add(entry(on, { { text = prompt, hl = "Normal", bold = on, size = style.size.body } }, click(i)), on)
        end
        add(buttons(button("Close", press("esc"))))
      end
    elseif st.step == "provider" then
      add(heading("Choose a provider", "Where the model runs."))
      local list = page.rows()
      st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))
      for i, r in ipairs(list) do
        local on = i == st.sel
        if r.group ~= (list[i - 1] or {}).group then
          add(section(r.group))
        end
        local name, aside = setup.preset_parts(presets[r.preset])
        add(entry(on, { {
          direction = "row", align = "center", gap = 8,
          { grow = 1, direction = "column",
            { text = name, hl = "Normal", bold = true, size = style.size.body },
            { text = aside, hl = "Dim", size = style.size.small, margin = { 2, 0, 0, 0 } } },
          { text = "›", hl = on and "Accent" or "Dim", size = style.size.lead },
        } }, click(i)), on)
      end
      if st.note then
        add({ text = st.note[1], hl = st.note[2] and "ErrorMsg" or "Dim", size = style.size.caption, padding = { 2, 10 } })
      end
    else
      local list = page.rows()
      st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))
      if st.step == "details" then
        add(heading("Connect " .. ((st.form.name or "") ~= "" and st.form.name or "a provider"),
          "Check the details, then continue."))
      else
        add(heading("Add plugins", "Optional. Pick any now; /catalog has them all later."))
        if st.packages_err or not st.packages then
          add({ text = st.packages_err and ("The catalog is not reachable (" .. st.packages_err .. "); /catalog later")
            or "Loading the catalog…", hl = "Dim", size = style.size.caption, padding = { 2, 10 }, margin = { 0, 0, 8, 0 } })
        end
      end
      for i, r in ipairs(list) do
        local on = i == st.sel
        if r.action then
          add(buttons(button(r.label, click(i), true, on)), on)
        elseif r.package then
          local picked = st.picked[r.package.name]
          add(entry(on, { {
            direction = "row", align = "center", gap = 10,
            widgets.checkbox(picked),
            { grow = 1, direction = "column",
              { text = r.label, hl = "Normal", bold = on, size = style.size.body },
              { text = r.package.description or "", hl = "Dim", size = style.size.small, margin = { 2, 0, 0, 0 } } },
          } }, click(i)), on)
        else
          local typed = r.key == "key" and string.rep("•", #(st.edit or "")) or st.edit
          add(field(r.label, on and st.edit and typed or r.value, {
            on = on, editing = on and st.edit ~= nil, click = click(i), hint = r.hint,
            empty = r.key == "key" and "none" or "empty",
          }), on)
        end
      end
      if st.note then
        add({ text = st.note[1], hl = st.note[2] and "ErrorMsg" or "Dim", size = style.size.caption, padding = { 2, 10 } })
      end
    end
    -- The one dialog size, whichever step it shows.
    return style.as_dialog(widgets.list("setup", items, selected, { estimate = 40, footer = hints(page.hint()) }))
  end

  return { node = node }
end

return M
