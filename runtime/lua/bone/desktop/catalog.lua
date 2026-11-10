-- The desktop library's /catalog: plugins and GUIs to browse, install,
-- update and remove, as a card in the middle of the screen. Each package is
-- a one-line entry (checked to choose several), under Updates, Installed and
-- Available. The state and keys are bone.catalog's page; this draws it.
local M = {}

local catalog = require("bone.catalog")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local note, entry, hints = widgets.note, widgets.entry, widgets.hints

--- The window's spec for `page` (bone.ui.windows.catalog).
function M.view(page)
  local st = page.st

  local function click(i)
    return function()
      if st.sel == i then page.on_key("space") else st.sel = i end
    end
  end

  local function node()
    local list = page.rows()
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))
    local updates = 0
    for _, e in ipairs(list) do
      if e.state == "update" then updates = updates + 1 end
    end
    -- As every dialog: the title, with the two pages (a press on the other
    -- one is the tab key) and × at its right; they stay over the list.
    local top = widgets.dialog_head("Catalog", {
      tabs = { "Plugins", "GUIs" }, tab = st.page == "tui" and 2 or 1,
      pick = function() page.on_key("tab") end,
      close = function() page.on_key("esc") end,
    })
    -- What there is to do; where the catalog comes from is its tooltip.
    local items = { widgets.extend(note(("%d update%s   ·   %d selected"):format(
      updates, updates == 1 and "" or "s", #page.selected_rows(list)), { 0, 4 }), { tooltip = catalog.source() }) }
    if st.err then
      items[#items + 1] = note("error: " .. st.err, { 4, 4 }, "ErrorMsg")
    elseif not st.entries then
      items[#items + 1] = note("Loading…", { 4, 4 })
    elseif #list == 0 then
      items[#items + 1] = note(st.page == "tui" and "No GUIs in the catalog yet (its libraries are for the terminal: /tui)"
        or "The catalog is empty", { 4, 4 })
    end
    local selected, last
    for i, e in ipairs(list) do
      if e.state ~= last then
        items[#items + 1] = widgets.heading(catalog.HEAD[e.state])
        last = e.state
      end
      local on, picked = i == st.sel, st.selected[e.name]
      -- Where it runs, said as the settings say it ("gui + core").
      local runs = catalog.supports(e):gsub("^both$", "tui + gui")
      local meta = (e.version or "") .. (runs ~= "" and ("  ·  " .. runs) or "")
      -- One line: the check, the name, its version, and the description cut
      -- to fit (all of it under the list for the selected one).
      local row = entry(on, {
        {
          direction = "row", align = "center", gap = style.space.md,
          widgets.checkbox(picked),
          { text = catalog.label(e), hl = "Normal", size = style.size.body, wrap = false, shrink = 0 },
          { text = meta, hl = "Dim", size = style.size.small, wrap = false, shrink = 0 },
          { text = e.blocked or e.description or "", hl = "Dim", size = style.size.small, grow = 1, width = 0,
            wrap = false, text_overflow = "ellipsis" },
        },
      }, click(i))
      items[#items + 1] = row
      if on then selected = #items end
    end
    -- Under the list, one space for what happened or the selected package's
    -- description: rows never change height as the selection moves.
    local status = page.note()
    local current = list[st.sel]
    local detail = status ~= "" and widgets.detail(status, st.ask and "Accent" or "Dim")
      or widgets.detail(current and current.description)
    -- The one dialog size, whichever page it shows.
    return style.as_dialog({ direction = "column", grow = 1, top, widgets.list("catalog", items, selected, {
      estimate = 28,
      footer = { direction = "column", detail, hints(page.hint(), { 0, 0 }) },
    }) })
  end

  return { node = node }
end

return M
