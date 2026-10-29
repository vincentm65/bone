-- The desktop library's /catalog: plugins and GUIs to browse, install,
-- update and remove, as a card in the middle of the screen. Each package is
-- a one-line entry (checked to choose several), under Updates, Installed and
-- Available. The state and keys are bone.catalog's page; this draws it.
local M = {}

local catalog = require("bone.catalog")
local style = require("bone.nodes.style")
local widgets = require("bone.nodes.widgets")
local title, note, entry, hints = widgets.title, widgets.note, widgets.entry, widgets.hints

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
    local items = { title("Catalog") }
    items[#items + 1] = note(("%s   ·   %d update%s   ·   %d selected"):format(
      catalog.source(), updates, updates == 1 and "" or "s", #page.selected_rows(list)), { 0, 4 })
    -- Two pages: a press on the other one is the tab key.
    items[#items + 1] = widgets.tabs({ "Plugins", "TUIs" }, st.page == "tui" and 2 or 1, function()
      page.on_key("tab")
    end)
    if st.err then
      items[#items + 1] = note("error: " .. st.err, { 4, 4 }, "ErrorMsg")
    elseif not st.entries then
      items[#items + 1] = note("Loading…", { 4, 4 })
    elseif #list == 0 then
      items[#items + 1] = note(st.page == "tui" and "No TUIs in the catalog" or "The catalog is empty", { 4, 4 })
    end
    local selected, last
    for i, e in ipairs(list) do
      if e.state ~= last then
        items[#items + 1] = { text = catalog.HEAD[e.state]:upper(), hl = "Dim", bold = true, size = style.size.micro,
          padding = { 2, 8 }, margin = { 6, 0, 2, 0 } }
        last = e.state
      end
      local on, picked = i == st.sel, st.selected[e.name]
      local meta = (e.version or "") .. (catalog.supports(e) ~= "" and ("  ·  " .. catalog.supports(e)) or "")
      -- One line: the check, the name, its version, and the description cut
      -- to fit (all of it under the list for the selected one).
      local row = entry(on, {
        {
          direction = "row", align = "center", gap = style.space.md,
          { spans = { widgets.check_span(picked), { text = catalog.label(e), hl = "Normal", bold = on } },
            size = style.size.body, wrap = false, shrink = 0 },
          { text = meta, hl = "Dim", size = style.size.small, wrap = false, shrink = 0 },
          { text = e.description or "", hl = "Dim", size = style.size.small, grow = 1, width = 0,
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
    return style.as_dialog(widgets.list("catalog", items, selected, {
      estimate = 28,
      footer = { direction = "column", widgets.divider({ margin = { 4, 0, 0, 0 } }), detail, hints(page.hint(), { 0, 0 }) },
    }))
  end

  return { node = node }
end

return M
