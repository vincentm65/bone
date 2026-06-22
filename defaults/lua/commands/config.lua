-- /config — interactive settings editor.
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

local function edit_provider(ctx, provider)
   local entry = {
      label = provider.label or "",
      model = provider.model or "",
      base_url = provider.base_url or "",
      endpoint = provider.endpoint or "",
      handler = provider.handler or "openai",
      api_key = provider.api_key or "",
   }

   while true do
      local labels = {
         "label \u{00b7} " .. entry.label,
         "model \u{00b7} " .. entry.model,
         "base_url \u{00b7} " .. entry.base_url,
         "endpoint \u{00b7} " .. entry.endpoint,
         "handler \u{00b7} " .. entry.handler,
         "api_key \u{00b7} " .. mask_secret(entry.api_key),
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
         if value ~= nil and value ~= "" then entry.api_key = value end
      elseif choice == labels[7] then
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

-- Build one styled line from a list of spans.
local function line_of(spans)
   return { spans = spans }
end

-- Spans for a single selectable row.
local function row_spans(row, selected)
   local cur = selected and ">" or " "
   local fg = selected and COL.text or COL.muted
   local mods = selected and { "bold" } or nil
   local sp = { span(" " .. cur .. " ", selected and COL.accent or COL.dim, mods) }

   if row.kind == "field" then
      local f = row.field
      local label = f.label or f.key
      if f.type == "bool" then
         local on = f.value == "true"
         sp[#sp + 1] = span(on and "\u{25cf} " or "\u{25cb} ", on and COL.green or COL.dim)
         sp[#sp + 1] = span(label, fg, mods)
         sp[#sp + 1] = span("  " .. (on and "on" or "off"), COL.dim)
      elseif f.type == "enum" then
         sp[#sp + 1] = span(label, fg, mods)
         sp[#sp + 1] = span("  [", COL.dim)
         sp[#sp + 1] = span(tostring(f.value or "?"), COL.amber, mods)
         sp[#sp + 1] = span("]", COL.dim)
      else
         sp[#sp + 1] = span(label, fg, mods)
         local v = tostring(f.value or "")
         if v == "" then v = "(unset)" end
         sp[#sp + 1] = span("  " .. v, f.type == "number" and COL.blue or COL.amber, mods)
      end
   else -- provider
      local pr = row.provider
      local active = pr.active
      sp[#sp + 1] = span(active and "\u{25cf} " or "\u{25cb} ", active and COL.green or COL.dim)
      sp[#sp + 1] = span(pr.id, fg, mods)
      sp[#sp + 1] = span("  ", COL.dim)
      sp[#sp + 1] = span(pr.model or "", selected and COL.amber or COL.dim, mods)
      sp[#sp + 1] = span("  ", COL.dim)
      sp[#sp + 1] = span(pr.handler or "openai", COL.blue)
      local url = pr.base_url or ""
      if #url > 38 then url = url:sub(1, 36) .. "\u{2026}" end
      sp[#sp + 1] = span("  " .. url, COL.dim)
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
   local cursor = {}   -- per-namespace selection memory (restored on tab change)
   local cur_ns = nil  -- namespace shown last render; detects tab switches
   local p = pane.new(ctx, { id = "interact", title = "Config" })
   -- Pane emits up to 20 visible rows; reserve ~5 for chrome
   -- (tabs, scroll indicators, blank line, hints).
   local body_rows = 15

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

      -- Styled tabs.
      local tspans = {}
      for i, pg in ipairs(pages) do
         if i > 1 then tspans[#tspans + 1] = span("   ", COL.dim) end
         local label = pg.title or pg.namespace
         if i == tab then
            tspans[#tspans + 1] = span("\u{258c} ", COL.accent, { "bold" })
            tspans[#tspans + 1] = span(label, COL.text, { "bold" })
         else
            tspans[#tspans + 1] = span("  ", COL.dim)
            tspans[#tspans + 1] = span(label, COL.dim)
         end
      end
      lines[#lines + 1] = line_of(tspans)

      if total == 0 then
         lines[#lines + 1] = line_of({ span(
            "  Nothing to configure here \u{2014} manage via /tools or /commands",
            COL.dim, { "italic" }
         ) })
      else
         if first > 1 then
            lines[#lines + 1] = line_of({ span("  \u{2191} " .. (first - 1) .. " more", COL.dim) })
         end
         for i = first, last do
            lines[#lines + 1] = line_of(row_spans(rows[i], i == sel))
         end
         if last < total then
            lines[#lines + 1] = line_of({ span("  \u{2193} " .. (total - last) .. " more", COL.dim) })
         end
      end

      lines[#lines + 1] = line_of({})
      local enter_label = is_providers and "switch provider" or "edit"
      lines[#lines + 1] = line_of({ span(string.format(
         "\u{2191}\u{2193} move  \u{00b7}  Enter %s  \u{00b7}  \u{2190}\u{2192} switch tab%s  \u{00b7}  Esc exit",
         enter_label, is_providers and "  \u{00b7}  e edit provider" or ""
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
      elseif code == "Left" then
         tab = tab > 1 and tab - 1 or #pages
      elseif code == "Right" then
         tab = tab < #pages and tab + 1 or 1
      elseif code == "PageUp" then
         sel = clamp(sel - 5, 1, math.max(1, total))
      elseif code == "PageDown" then
         sel = clamp(sel + 5, 1, math.max(1, total))
      elseif code == "Home" then
         sel = 1
      elseif code == "End" then
         sel = math.max(1, total)
      elseif code == "Enter" then
         local row = rows[sel]
         if row then
            if row.kind == "provider" then
               menu.clear(ctx)
               return { action = "config.switch_provider", provider = row.provider.id, submit = false }
            else
               local f = row.field
               if f.type == "bool" or f.type == "enum" then
                  local nv = ctx.config.cycle_field(ns, f.key, f.value or "")
                  if nv then
                     ctx.config.set_value(ns, f.key, nv)
                     changed = true
                  end
               else
                  local v = edit_text(ctx, f.label or f.key, f.value or "")
                  if v ~= nil then
                     ctx.config.set_value(ns, f.key, v)
                     changed = true
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
   if changed then return { action = "config.apply", submit = false } end
   return nil
end

bone.register_command("config", {
   description = "edit configuration",
   handler = function(arg, ctx)
      local words = split_args(arg)
      if words[1] == "tools" and words[2] == "reload" then
         return { action = "config.reload_tools", submit = false }
      end
      return run(ctx, words[1])
   end,
})
