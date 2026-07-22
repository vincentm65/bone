-- /config — interactive settings editor.
-- canonical-config-v5
--
-- Renders its own styled bottom pane (full span control) for the tabbed
-- settings overview, and reuses `ui.menu` only for the isolated sub-prompts
-- (free-text edits, the provider detail editor).

local menu = require("ui.menu")
local pane = require("ui.pane")

local span, clamp, wait_key, key_name, is_text_key = pane.span, pane.clamp, pane.wait_key, pane.key_name, pane.is_text_key

-- Palette shared across the config view.
local COL = {
   accent = "cyan",
   green  = "#78B373",
   amber  = "#E5C07B",
   blue   = "#61AFEF",
   dim    = "darkgray",
   text   = "white",
   muted  = "gray",
   sel_bg = "#3A3F4B",  -- subtle highlight behind the selected row
}

local function split_args(arg)
   local words = {}
   for word in tostring(arg or ""):gmatch("%S+") do
      words[#words + 1] = word
   end
   return words
end

local function mask_secret(value)
   if not value or value == "" then return "(empty)" end
   local len = math.min(12, math.max(4, #tostring(value)))
   return string.rep("*", len)
end

local function ask(ctx, opts)
   local fn = menu.select
   if opts.type == "text_input" or opts.type == "text" then
      fn = menu.text_input
   elseif opts.type == "multi_select" or opts.type == "multi" then
      fn = menu.multi_select
   end
   local ok, result = pcall(fn, ctx, opts)
   if not ok then
      ctx.ui.notify("Config picker failed: " .. tostring(result), "error")
      return nil
   end
   if type(result) ~= "table" or result.cancelled then
      return nil
   end
   return result
end

-- Free-text edit. Prefills the current value so the user edits in place.
local function edit_text(ctx, label, initial)
   local result = ask(ctx, {
      question = "Edit " .. label .. "  \u{00b7}  Enter saves \u{00b7} Esc cancels",
      type = "text_input",
      initial = tostring(initial or ""),
      allow_custom = true,
   })
   if not result then return nil end
   return result.value or ""
end

local function save_value(ctx, namespace, key, value)
   local ok, result = pcall(ctx.config.set_value, namespace, key, value)
   if not ok then
      ctx.ui.notify("Could not save setting: " .. tostring(result), "error")
      return false
   end
   return result == true
end

local REASONING_EFFORTS = { "default", "none", "minimal", "low", "medium", "high", "xhigh", "max" }

local function edit_provider(ctx, provider)
   local entry = {
      label = provider.label or "",
      model = provider.model or "",
      base_url = provider.base_url or "",
      endpoint = provider.endpoint or "",
      handler = provider.handler or "openai",
      api_key = "",
      api_key_configured = provider.api_key_configured == true,
      context_window_tokens = provider.context_window_tokens,
      reasoning_effort = provider.reasoning_effort or "",
   }

   while true do
      local labels = {
         "label \u{00b7} " .. entry.label,
         "model \u{00b7} " .. entry.model,
         "base_url \u{00b7} " .. entry.base_url,
         "endpoint \u{00b7} " .. entry.endpoint,
         "handler \u{00b7} " .. entry.handler,
         "api_key \u{00b7} " .. (entry.api_key ~= "" and mask_secret(entry.api_key)
            or (entry.api_key_configured and "(configured)" or "(empty)")),
         "context_window_tokens \u{00b7} " .. tostring(entry.context_window_tokens or "unknown"),
         "reasoning_effort \u{00b7} " .. (entry.reasoning_effort ~= "" and entry.reasoning_effort or "default"),
         "Save changes",
      }
      local result = ask(ctx, {
         question = "Edit provider: " .. provider.id,
         type = "single_select",
         options = labels,
         allow_custom = false,
      })
      if not result then return false end
      local choice = result.value
      if choice == labels[1] then
         local value = edit_text(ctx, "label", entry.label)
         if value ~= nil then entry.label = value end
      elseif choice == labels[2] then
         local value = edit_text(ctx, "model", entry.model)
         if value ~= nil then entry.model = value end
      elseif choice == labels[3] then
         local value = edit_text(ctx, "base_url", entry.base_url)
         if value ~= nil then entry.base_url = value end
      elseif choice == labels[4] then
         local value = edit_text(ctx, "endpoint", entry.endpoint)
         if value ~= nil then entry.endpoint = value end
      elseif choice == labels[5] then
         entry.handler = entry.handler == "codex" and "openai" or "codex"
      elseif choice == labels[6] then
         local value = edit_text(ctx, "api_key", "")
         if value ~= nil and value ~= "" then
            entry.api_key = value
            entry.api_key_configured = true
         end
      elseif choice == labels[7] then
         local value = edit_text(ctx, "context_window_tokens", entry.context_window_tokens or "")
         if value ~= nil then entry.context_window_tokens = tonumber(value) end
      elseif choice == labels[8] then
         local result = ask(ctx, {
            question = "Select reasoning_effort",
            type = "single_select",
            options = REASONING_EFFORTS,
            allow_custom = false,
         })
         if result then
            entry.reasoning_effort = result.value == "default" and "" or result.value
         end
      elseif choice == labels[9] then
         ctx.config.set_provider_entry(provider.id, entry)
         return true
      end
   end
end

local function find_page_index(pages, namespace)
   for i, page in ipairs(pages or {}) do
      if page.namespace == namespace then return i end
   end
   return 1
end

-- Build one styled line from a list of spans, with an optional row background
-- (used to highlight the selected row edge-to-edge — see pane_content.rs).
local function line_of(spans, bg)
   return { spans = spans, bg = bg }
end

-- Right-pad `s` to `width` display columns (labels/ids are ASCII keys).
local function pad(s, width)
   s = tostring(s or "")
   local gap = width - #s
   if gap > 0 then s = s .. string.rep(" ", gap) end
   return s
end

-- Spans for a single selectable row. `pad_w` aligns the value column.
local function row_spans(row, selected, pad_w)
   local fg = selected and COL.text or COL.muted
   local mods = selected and { "bold" } or nil
   -- Accent bar marks the selected row; a blank gutter keeps others aligned.
   local sp = { span(selected and " \u{258c} " or "   ", COL.accent, mods) }

   if row.kind == "field" then
      local f = row.field
      local label = f.label or f.key
      sp[#sp + 1] = span(pad(label, pad_w) .. "  ", fg, mods)
      if f.type == "bool" then
         local on = f.value == true or f.value == "true"
         sp[#sp + 1] = span(on and "\u{25cf} " or "\u{25cb} ", on and COL.green or COL.dim)
         sp[#sp + 1] = span(on and "on" or "off", on and COL.green or COL.dim, mods)
      elseif f.type == "enum" then
         sp[#sp + 1] = span("[ ", COL.dim)
         sp[#sp + 1] = span(tostring(f.value or "?"), COL.amber, mods)
         sp[#sp + 1] = span(" ]", COL.dim)
      else
         local v = tostring(f.value or "")
         if v == "" then v = "(unset)" end
         sp[#sp + 1] = span(v, f.type == "number" and COL.blue or COL.amber, mods)
      end
   else -- provider
      local pr = row.provider
      local active = pr.active
      sp[#sp + 1] = span(active and "\u{25cf} " or "\u{25cb} ", active and COL.green or COL.dim)
      sp[#sp + 1] = span(pad(pr.id, pad_w) .. "  ", fg, mods)
      sp[#sp + 1] = span(pad(pr.model or "", 18) .. "  ", selected and COL.amber or COL.dim, mods)
      sp[#sp + 1] = span(pad(pr.handler or "openai", 7) .. "  ", COL.blue)
      local url = pr.base_url or ""
      if #url > 38 then url = url:sub(1, 36) .. "\u{2026}" end
      sp[#sp + 1] = span(url, COL.dim)
   end
   return sp
end

local function build_rows(ctx, page)
   local rows = {}
   if page.namespace == "providers" then
      for _, pr in ipairs(ctx.config.list_providers() or {}) do
         rows[#rows + 1] = { kind = "provider", provider = pr }
      end
   else
      for _, f in ipairs(page.fields or {}) do
         if f.type ~= "provider" then
            rows[#rows + 1] = { kind = "field", field = f }
         end
      end
   end
   return rows
end

-- Width of the label/id column so values line up across the page.
local function label_width(rows)
   local w = 0
   for _, row in ipairs(rows) do
      local s = row.kind == "provider" and row.provider.id or (row.field.label or row.field.key)
      w = math.max(w, #tostring(s or ""))
   end
   return w
end

local function run(ctx, start_ns)
   local pages = ctx.config.get_pages()
   if not pages or #pages == 0 then
      ctx.ui.notify("No config pages found.", "warn")
      return nil
   end

   local tab = find_page_index(pages, start_ns)
   local sel = 1
   local scroll_first = 1
   local changed = false
   local restart_required = false
   local cursor = {}   -- per-namespace selection memory (restored on tab change)
   local cur_ns = nil  -- namespace shown last render; detects tab switches
   local p = pane.new(ctx, { id = "interact", title = "Config" })
   -- Pane emits up to 20 visible rows; reserve ~7 for chrome
   -- (tabs, subtitle, blank line, scroll indicators, blank line, hints).
   local body_rows = 13

   while true do
      pages = ctx.config.get_pages()
      tab = clamp(tab, 1, #pages)
      local page = pages[tab]
      local ns = page.namespace
      local rows = build_rows(ctx, page)
      local total = #rows
      -- Only re-seed `sel` when we actually switch tabs; otherwise keep the
      -- live cursor (so Up/Down mutations survive the next iteration instead
      -- of being overwritten by a stale saved value).
      if ns ~= cur_ns then
         sel = cursor[ns] or 1
         scroll_first = 1
         cur_ns = ns
      end
      sel = clamp(sel, 1, math.max(1, total))
      cursor[ns] = sel
      local is_providers = ns == "providers"

      -- Windowing so the cursor stays in view without user scrolling.
      local first, last
      if total <= body_rows then
         first, last, scroll_first = 1, total, 1
      else
         scroll_first = clamp(scroll_first, 1, total - body_rows + 1)
         if sel < scroll_first then scroll_first = sel end
         if sel > scroll_first + body_rows - 1 then scroll_first = sel - body_rows + 1 end
         first, last = scroll_first, scroll_first + body_rows - 1
      end

      local lines = {}

      -- Styled tabs with ` │ ` separators.
      local tspans = { span("  ", COL.dim) }
      for i, pg in ipairs(pages) do
         if i > 1 then tspans[#tspans + 1] = span("  \u{2502}  ", COL.dim) end
         local label = pg.title or pg.namespace
         if i == tab then
            tspans[#tspans + 1] = span(label, COL.text, { "bold" })
         else
            tspans[#tspans + 1] = span(label, COL.dim)
         end
      end
      lines[#lines + 1] = line_of(tspans)

      -- Page subtitle + breathing room.
      lines[#lines + 1] = line_of({ span("  " .. (page.title or ns), COL.dim, { "italic" }) })
      lines[#lines + 1] = line_of({})

      if total == 0 then
         lines[#lines + 1] = line_of({ span(
            "  Nothing to configure here \u{2014} manage via /tools or /commands",
            COL.dim, { "italic" }
         ) })
      else
         local pad_w = label_width(rows)
         if first > 1 then
            lines[#lines + 1] = line_of({ span("  \u{2191} " .. (first - 1) .. " more", COL.dim) })
         end
         for i = first, last do
            local is_sel = i == sel
            lines[#lines + 1] = line_of(row_spans(rows[i], is_sel, pad_w), is_sel and COL.sel_bg or nil)
         end
         if last < total then
            lines[#lines + 1] = line_of({ span("  \u{2193} " .. (total - last) .. " more", COL.dim) })
         end
      end

      lines[#lines + 1] = line_of({})
      local enter_label = is_providers and "switch provider" or "edit"
      local toggle_hint = not is_providers and "  \u{00b7}  Space toggle" or ""
      lines[#lines + 1] = line_of({ span(string.format(
         "  \u{2191}\u{2193} move  \u{00b7}  Enter %s%s  \u{00b7}  Tab/\u{2190}\u{2192} switch tab%s  \u{00b7}  Esc exit",
         enter_label, toggle_hint, is_providers and "  \u{00b7}  e edit provider" or ""
      ), COL.dim) })

      p:set_lines(lines, math.min(20, #lines))

      local key = wait_key(ctx)
      if not key then break end
      local code = key_name(key)

      if code == "Esc" then
         break
      elseif code == "Up" then
         sel = sel > 1 and sel - 1 or math.max(1, total)
      elseif code == "Down" then
         sel = sel < total and sel + 1 or 1
      elseif code == "Left" or code == "BackTab" then
         tab = tab > 1 and tab - 1 or #pages
      elseif code == "Right" or code == "Tab" then
         tab = tab < #pages and tab + 1 or 1
      elseif code == "PageUp" then
         sel = clamp(sel - 5, 1, math.max(1, total))
      elseif code == "PageDown" then
         sel = clamp(sel + 5, 1, math.max(1, total))
      elseif code == "Home" then
         sel = 1
      elseif code == "End" then
         sel = math.max(1, total)
      elseif code == "Enter" or (code == "Char" and key.char == " ") then
         local row = rows[sel]
         local space_toggle = code == "Char" and row and row.kind == "field"
            and (row.field.type == "bool" or row.field.type == "enum")
         if row and (code == "Enter" or space_toggle) then
            if row.kind == "provider" then
               menu.clear(ctx)
               return { action = "config.switch_provider", provider = row.provider.id, submit = false }
            else
               local f = row.field
               if f.type == "bool" or f.type == "enum" then
                  local nv = ctx.config.cycle_field(ns, f.key, f.value)
                  if nv ~= nil and save_value(ctx, ns, f.key, nv) then
                     changed = true
                     restart_required = restart_required or ns == "tools" or ns == "commands"
                  end
               else
                  local v = edit_text(ctx, f.label or f.key, f.value or "")
                  if v ~= nil and f.type == "number" then v = tonumber(v) end
                  if v ~= nil and save_value(ctx, ns, f.key, v) then
                     changed = true
                     restart_required = restart_required or ns == "tools" or ns == "commands"
                  end
               end
            end
         end
      elseif is_text_key(key) and key.char == "e" and is_providers then
         local row = rows[sel]
         if row and row.kind == "provider" and edit_provider(ctx, row.provider) then
            changed = true
         end
      end
   end

   menu.clear(ctx)
   if restart_required then
      return { action = "config.reload_tools", submit = false }
   end
   if changed then return { action = "config.apply", submit = false } end
   return nil
end

bone.command.register("config", {
   description = "edit configuration",
   handler = function(arg, ctx)
      local words = split_args(arg)
      if words[1] == "tools" and words[2] == "reload" then
         return { action = "config.reload_tools", submit = false }
      end
      return run(ctx, words[1])
   end,
})
