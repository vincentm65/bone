-- /config — interactive settings editor.
-- canonical-config-v8
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

local REASONING_EFFORTS = {
   { label = "Low", value = "low" },
   { label = "Med", value = "medium" },
   { label = "High", value = "high" },
   { label = "XHigh", value = "xhigh" },
}

local PROVIDER_HANDLERS = { "openai", "anthropic", "codex", "grok_build" }

local function next_provider_handler(current)
   for i, handler in ipairs(PROVIDER_HANDLERS) do
      if handler == current then
         return PROVIDER_HANDLERS[(i % #PROVIDER_HANDLERS) + 1]
      end
   end
   return PROVIDER_HANDLERS[1]
end

local function save_provider_field(ctx, provider_id, entry, field)
   local ok, result = pcall(ctx.config.set_provider_entry, provider_id, entry)
   if not ok then
      ctx.ui.notify("Could not save " .. field .. ": " .. tostring(result), "error")
      return false
   end
   if result ~= true then
      ctx.ui.notify("Could not save " .. field .. ": save was rejected", "error")
      return false
   end
   ctx.ui.notify("Saved " .. field .. ".", "info")
   return true
end

local function update_provider_field(ctx, provider_id, entry, field, value)
   local previous = entry[field]
   if previous == value then return false end
   entry[field] = value
   if save_provider_field(ctx, provider_id, entry, field) then return true end
   entry[field] = previous
   return false
end

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
      max_concurrency = provider.max_concurrency,
      reasoning_effort = provider.reasoning_effort or "",
      fast_mode = provider.fast_mode == true,
   }

   local selected = 1
   local changed = false

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
         "max_concurrency \u{00b7} " .. tostring(entry.max_concurrency or "unlimited"),
         "reasoning_effort \u{00b7} " .. (entry.reasoning_effort ~= "" and entry.reasoning_effort or "default"),
      }
      local fast_index = nil
      if entry.handler == "codex" then
         labels[#labels + 1] = "fast_mode \u{00b7} " .. (entry.fast_mode and "on" or "off")
         fast_index = #labels
      end
      local result = ask(ctx, {
         question = "Edit provider: " .. provider.id .. "  \u{00b7}  changes save immediately",
         type = "single_select",
         options = labels,
         default = selected,
         allow_custom = false,
      })
      if not result then return changed end
      selected = result.selected or selected
      local choice = result.value
      if choice == labels[1] then
         local value = edit_text(ctx, "label", entry.label)
         if value ~= nil then
            changed = update_provider_field(ctx, provider.id, entry, "label", value) or changed
         end
      elseif choice == labels[2] then
         local value = edit_text(ctx, "model", entry.model)
         if value ~= nil then
            changed = update_provider_field(ctx, provider.id, entry, "model", value) or changed
         end
      elseif choice == labels[3] then
         local value = edit_text(ctx, "base_url", entry.base_url)
         if value ~= nil then
            changed = update_provider_field(ctx, provider.id, entry, "base_url", value) or changed
         end
      elseif choice == labels[4] then
         local value = edit_text(ctx, "endpoint", entry.endpoint)
         if value ~= nil then
            changed = update_provider_field(ctx, provider.id, entry, "endpoint", value) or changed
         end
      elseif choice == labels[5] then
         local previous_handler = entry.handler
         local previous_fast_mode = entry.fast_mode
         entry.handler = next_provider_handler(entry.handler)
         if entry.handler ~= "codex" then entry.fast_mode = false end
         if save_provider_field(ctx, provider.id, entry, "handler") then
            changed = true
         else
            entry.handler = previous_handler
            entry.fast_mode = previous_fast_mode
         end
      elseif choice == labels[6] then
         local value = edit_text(ctx, "api_key", "")
         if value ~= nil and value ~= "" then
            entry.api_key = value
            if save_provider_field(ctx, provider.id, entry, "api_key") then
               entry.api_key = ""
               entry.api_key_configured = true
               changed = true
            else
               entry.api_key = ""
            end
         end
      elseif choice == labels[7] then
         local value = edit_text(ctx, "context_window_tokens", entry.context_window_tokens or "")
         if value ~= nil then
            local tokens = value == "" and nil or tonumber(value)
            if value ~= "" and (not tokens or tokens < 1 or tokens ~= math.floor(tokens)) then
               ctx.ui.notify("Context window must be blank or a positive integer", "error")
            else
               changed = update_provider_field(
                  ctx, provider.id, entry, "context_window_tokens", tokens
               ) or changed
            end
         end
      elseif choice == labels[8] then
         local value = edit_text(ctx, "max_concurrency", entry.max_concurrency or "")
         if value ~= nil then
            local limit = nil
            if value == "" then
               limit = nil
            else
               limit = tonumber(value)
               if limit and limit >= 1 and limit == math.floor(limit) then
                  -- valid; save below
               else
                  ctx.ui.notify("Max concurrency must be blank or a positive integer", "error")
                  limit = false
               end
            end
            if limit ~= false then
               changed = update_provider_field(
                  ctx, provider.id, entry, "max_concurrency", limit
               ) or changed
            end
         end
      elseif choice == labels[9] then
         local current = entry.reasoning_effort
         local selected = nil
         for i, option in ipairs(REASONING_EFFORTS) do
            if option.value == current then selected = i end
         end
         local result = ask(ctx, {
            question = "Select reasoning_effort",
            type = "single_select",
            options = REASONING_EFFORTS,
            default = selected or 1,
            allow_custom = true,
            initial = selected and "" or current,
            initial_custom = selected == nil and current ~= "",
         })
         if result then
            changed = update_provider_field(
               ctx, provider.id, entry, "reasoning_effort", result.value
            ) or changed
         end
      elseif fast_index and choice == labels[fast_index] then
         changed = update_provider_field(
            ctx, provider.id, entry, "fast_mode", not entry.fast_mode
         ) or changed
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
      sp[#sp + 1] = span(pad(pr.handler or "openai", 10) .. "  ", COL.blue)
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
      local is_providers = ns == "providers"
      -- Only re-seed `sel` when we actually switch tabs; otherwise keep the
      -- live cursor (so Up/Down mutations survive the next iteration instead
      -- of being overwritten by a stale saved value).
      if ns ~= cur_ns then
         sel = cursor[ns]
         if not sel and is_providers then
            for i, row in ipairs(rows) do
               if row.provider.active then
                  sel = i
                  break
               end
            end
         end
         sel = sel or 1
         scroll_first = 1
         cur_ns = ns
      end
      sel = clamp(sel, 1, math.max(1, total))
      cursor[ns] = sel
      local visible_body_rows = is_providers and body_rows - 1 or body_rows

      -- Windowing so the cursor stays in view without user scrolling.
      local first, last
      if total <= visible_body_rows then
         first, last, scroll_first = 1, total, 1
      else
         scroll_first = clamp(scroll_first, 1, total - visible_body_rows + 1)
         if sel < scroll_first then scroll_first = sel end
         if sel > scroll_first + visible_body_rows - 1 then
            scroll_first = sel - visible_body_rows + 1
         end
         first, last = scroll_first, scroll_first + visible_body_rows - 1
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
         if is_providers then
            pad_w = math.max(pad_w, #"Provider")
            lines[#lines + 1] = line_of({
               span("   ", COL.dim),
               span(pad("Provider", pad_w) .. "  ", COL.dim, { "bold" }),
               span(pad("Model", 18) .. "  ", COL.dim, { "bold" }),
               span(pad("Handler", 10) .. "  ", COL.dim, { "bold" }),
               span("Base URL", COL.dim, { "bold" }),
            })
         end
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
