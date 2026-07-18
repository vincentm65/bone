-- ui.menu — interactive select / multi_select / text_input panes.
--
-- Built on `ui.pane`: the Pane object owns the channel transport (rendering
-- even while a tool blocks on `ctx.ui.key`), and the shared helpers (`span`,
-- `line`, `clamp`, `wait_key`, `is_text_key`) come from there too. This module
-- keeps only the menu-specific rendering and key-dispatch logic.

local pane = require("ui.pane")

local span, line, clamp = pane.span, pane.line, pane.clamp
local wait_key, key_name, is_text_key = pane.wait_key, pane.key_name, pane.is_text_key

local M = {}

local SOURCE = "interact"
local DEFAULT_ROWS = 12
local MAX_ROWS = 24
local SELECTED_BG = "#3A3F4B"

-- Current pane width in columns, or nil when the host can't report it (older
-- binary lacking `ctx.ui.width`, or not yet drawn). Callers that get nil skip
-- wrapping and fall back to the single-line behaviour.
local function pane_width(ctx)
    if not ctx or not ctx.ui or type(ctx.ui.width) ~= "function" then
        return nil
    end
    local ok, w = pcall(ctx.ui.width)
    if ok and type(w) == "number" and w > 0 then
        return math.floor(w)
    end
    return nil
end

-- Iterate the UTF-8 characters of `s` (input fields are usually ASCII, but a
-- char-aware split keeps multi-byte input from breaking mid-codepoint).
local function utf8_chars(s)
    local chars = {}
    for ch in s:gmatch("[\1-\127\194-\244][\128-\191]*") do
        chars[#chars + 1] = ch
    end
    return chars
end

-- Wrap `text` into segments of at most `max` characters each, preferring to
-- break on the last whitespace within the window. Returns { text } unchanged
-- when `max` is nil/non-positive or the text already fits.
local function wrap_input(text, max)
    if not max or max < 1 then return { text } end
    local chars = utf8_chars(text)
    if #chars <= max then return { text } end

    local segments = {}
    local start = 1
    while start <= #chars do
        local stop = math.min(start + max - 1, #chars)
        if stop < #chars then
            -- Look for a whitespace break inside [start, stop] to avoid
            -- splitting a word; only honour it when it isn't the very first
            -- char (which would make no forward progress).
            for i = stop, start + 1, -1 do
                if chars[i]:match("%s") then
                    stop = i
                    break
                end
            end
        end
        segments[#segments + 1] = table.concat(chars, "", start, stop)
        start = stop + 1
    end
    if #segments == 0 then segments[1] = "" end
    return segments
end

local function one_line(value)
    return tostring(value or ""):gsub("%s+", " ")
end

local function clip(value, max)
    local chars = utf8_chars(one_line(value))
    if not max or #chars <= max then return table.concat(chars) end
    if max < 2 then return "" end
    return table.concat(chars, "", 1, max - 1) .. "…"
end

local function clip_spans(values, max)
    local out = {}
    local remaining = max
    for _, value in ipairs(values or {}) do
        if remaining <= 0 then break end
        local chars = utf8_chars(one_line(value.text))
        local clipped = #chars > remaining
        local text
        if not clipped then
            text = table.concat(chars)
        elseif remaining >= 2 then
            text = table.concat(chars, "", 1, remaining - 1) .. "…"
        else
            text = ""
        end
        if text ~= "" then
            out[#out + 1] = span(text, value.fg or "gray", value.modifiers)
        end
        remaining = remaining - #utf8_chars(text)
        if clipped then break end
    end
    return out
end

local function normalize_preview(value)
    if type(value) ~= "table" or type(value.lines) ~= "table" then return nil end
    local lines = {}
    for _, raw in ipairs(value.lines) do
        if type(raw) == "string" then
            lines[#lines + 1] = line(span(raw, "gray"))
        elseif type(raw) == "table" and type(raw.spans) == "table" then
            local spans = {}
            for _, value_span in ipairs(raw.spans) do
                if type(value_span) == "table" and value_span.text ~= nil then
                    spans[#spans + 1] = span(value_span.text, value_span.fg or "gray", value_span.modifiers)
                end
            end
            lines[#lines + 1] = { spans = spans, bg = raw.bg }
        end
    end
    return { title = value.title and one_line(value.title) or nil, lines = lines }
end

local function normalize_options(options)
    local out = {}
    for i, opt in ipairs(options or {}) do
        if type(opt) == "table" then
            out[i] = {
                label = one_line(opt.label or opt.value or i),
                label_modifiers = opt.label_modifiers,
                description = opt.description and one_line(opt.description) or nil,
                description_spans = opt.description_spans,
                search_text = one_line(opt.search_text or ""),
                value = opt.value or opt.label or tostring(i),
                action = opt.action,
                preview = normalize_preview(opt.preview),
            }
        else
            out[i] = { label = one_line(opt), value = opt, search_text = "" }
        end
    end
    return out
end

local function apply_filter(state, selected_value)
    local query = state.filter:lower()
    local filtered = {}
    for _, opt in ipairs(state.all_options) do
        local haystack = (opt.label .. " " .. (opt.description or "") .. " " .. opt.search_text):lower()
        if query == "" or haystack:find(query, 1, true) then
            filtered[#filtered + 1] = opt
        end
    end
    state.options = filtered
    state.selected = clamp(state.selected, 1, math.max(1, #filtered))
    if selected_value ~= nil then
        for i, opt in ipairs(filtered) do
            if opt.value == selected_value then
                state.selected = i
                break
            end
        end
    end
    state.scroll = 0
end

local function render_tabs(lines, tabs, active)
    if not tabs or #tabs == 0 then return end
    local spans = {}
    for i, tab in ipairs(tabs) do
        if i > 1 then spans[#spans + 1] = span("  ", "darkgray") end
        local label = tab.title or tab.label or tostring(tab)
        if i == active then
            spans[#spans + 1] = span(label, "cyan", { "bold" })
        else
            spans[#spans + 1] = span(label, "darkgray")
        end
    end
    lines[#lines + 1] = { spans = spans }
end

local function rows_for(state)
    if state.visible_rows then return state.visible_rows end
    return DEFAULT_ROWS
end

local function preview_uses_columns(state, width)
    if state.preview_layout == "split" then return true end
    if state.preview_layout == "stacked" then return false end
    return width >= state.preview_min_width
end

local function split_leading_circle(label)
    local rest = label:match("^●%s+(.+)$")
    if rest then
        return "●", rest
    end
    rest = label:match("^○%s+(.+)$")
    if rest then
        return "○", rest
    end
    return nil, label
end

local function clip_line_spans(values, max)
    local out = {}
    local remaining = max
    for _, value in ipairs(values or {}) do
        if remaining <= 0 then break end
        local chars = utf8_chars(tostring(value.text or ""))
        local clipped = #chars > remaining
        local text
        if not clipped then
            text = table.concat(chars)
        elseif remaining >= 2 then
            text = table.concat(chars, "", 1, remaining - 1) .. "…"
        else
            text = ""
        end
        if text ~= "" then
            out[#out + 1] = span(text, value.fg or "gray", value.modifiers)
        end
        remaining = remaining - #utf8_chars(text)
        if clipped then break end
    end
    return out
end

local function line_spans(value, width)
    if type(value) == "string" then return { span(clip(value, width), "gray") } end
    return clip_line_spans(value and value.spans or {}, width)
end

local function spans_width(values)
    local width = 0
    for _, value in ipairs(values or {}) do width = width + #utf8_chars(tostring(value.text or "")) end
    return width
end

local function compact_option_line(state, opt, selected, width)
    local checked = state.checked and state.checked[opt]
    local check = state.multi and (checked and "[x] " or "[ ] ") or ""
    local marker = selected and ">" or " "
    return line(
        span(" " .. marker .. " ", selected and "cyan" or "darkgray", selected and { "bold" } or {}),
        span(check, checked and "#78B373" or "darkgray", checked and { "bold" } or {}),
        span(clip(opt.label, width - 3 - #check), "white", opt.label_modifiers or (selected and { "bold" } or {}))
    )
end

local function join_columns(left, right, left_width, right_width, focused)
    local left_spans = line_spans(left, left_width)
    local out = {}
    for _, value in ipairs(left_spans) do out[#out + 1] = value end
    out[#out + 1] = span(string.rep(" ", math.max(0, left_width - spans_width(left_spans))), "darkgray")
    out[#out + 1] = span(" ┃ ", focused and "cyan" or "darkgray", focused and { "bold" } or {})
    for _, value in ipairs(line_spans(right, right_width)) do out[#out + 1] = value end
    return { spans = out }
end

local function selected_preview(state)
    if state.custom_focused then return nil, nil end
    local opt = state.options[state.selected]
    return opt and opt.preview or nil, opt
end

local function tallest_preview_rows(state)
    local rows = 1
    for _, opt in ipairs(state.all_options) do
        if opt.preview and opt.preview.lines then
            rows = math.max(rows, #opt.preview.lines)
        end
    end
    return rows
end

local function preview_row_budget(state, width, header_rows)
    local use_columns = preview_uses_columns(state, width)
    local custom_rows = state.allow_custom and 1 or 0
    local preview_rows = tallest_preview_rows(state)
    local raw_body
    local raw_overflow
    if use_columns then
        raw_body = math.max(4, preview_rows + 1, #state.options + custom_rows)
        raw_overflow = false
    else
        local shown_options = math.min(4, #state.options)
        raw_body = math.max(4, shown_options + custom_rows + 1 + preview_rows)
        raw_overflow = #state.options > shown_options
    end

    local notice_rows = state.notice and state.notice ~= "" and 1 or 0
    local target_rows
    if state.visible_rows then
        target_rows = clamp(math.floor(tonumber(state.visible_rows) or DEFAULT_ROWS), 3, MAX_ROWS)
    else
        target_rows = clamp(header_rows + raw_body + notice_rows + (raw_overflow and 1 or 0) + 2, 3, MAX_ROWS)
    end

    local body_rows = math.max(4, target_rows - header_rows - notice_rows - 2)
    -- The overflow indicator consumes a row only after the option viewport is
    -- known. Reserve it from the body after calculating whether it is needed.
    local option_rows = use_columns
        and math.max(1, body_rows - custom_rows)
        or math.min(4, math.max(1, body_rows - custom_rows))
    if #state.options > option_rows and body_rows > 4 then body_rows = body_rows - 1 end
    return target_rows, body_rows, use_columns
end

local function preview_window(state, rows)
    local preview, opt = selected_preview(state)
    local values = preview and preview.lines or {}
    local max_scroll = state.preview_scrollable and math.max(0, #values - rows) or 0
    state.preview_scroll = clamp(state.preview_scroll or 0, 0, max_scroll)
    state.preview_page_rows = rows
    state.preview_max_scroll = max_scroll
    local title = preview and preview.title or (opt and opt.label or "Preview")
    if state.preview_scrollable and #values > rows then
        title = string.format("%s  %d/%d", title, state.preview_scroll + 1, #values)
    end
    local visible = {}
    for i = state.preview_scroll + 1, math.min(#values, state.preview_scroll + rows) do
        visible[#visible + 1] = values[i]
    end
    if #visible == 0 then visible[1] = line(span("No preview", "darkgray")) end
    return title, visible
end

local function render_preview_select(p, state)
    local lines = {}
    render_tabs(lines, state.tabs, state.active_tab)
    if state.question and state.question ~= "" then
        lines[#lines + 1] = line(span(state.question, "white", { "bold" }))
    end

    local width = pane_width(p.ctx) or 80
    local target_rows, body_rows, use_columns = preview_row_budget(state, width, #lines)
    local custom_rows = state.allow_custom and 1 or 0
    local option_rows = math.max(1, body_rows - custom_rows)
    if not use_columns then option_rows = math.min(option_rows, 4) end
    local total = #state.options
    state.scroll = clamp(state.scroll or 0, 0, math.max(0, total - option_rows))
    if state.selected <= state.scroll then state.scroll = state.selected - 1 end
    if state.selected > state.scroll + option_rows then state.scroll = state.selected - option_rows end
    local body_start = #lines

    if use_columns then
        local left_width = clamp(math.floor(width * 0.32), 20, 34)
        local right_width = math.max(1, width - left_width - 3)
        local title, preview_lines = preview_window(state, body_rows - 1)
        local right_lines = { line(span(title, state.preview_focused and "cyan" or "white", { "bold" })) }
        for _, value in ipairs(preview_lines) do right_lines[#right_lines + 1] = value end

        for row = 1, body_rows do
            local option_index = state.scroll + row
            local left
            if row <= option_rows and option_index <= total then
                left = compact_option_line(
                    state,
                    state.options[option_index],
                    option_index == state.selected and not state.custom_focused,
                    left_width
                )
            elseif state.allow_custom and row == body_rows then
                local marker = state.custom_focused and ">" or " "
                left = line(
                    span(" " .. marker .. " Custom: ", state.custom_focused and "cyan" or "darkgray", { "bold" }),
                    span(clip(state.input, left_width - 11), state.custom_focused and "white" or "darkgray")
                )
            else
                left = ""
            end
            lines[#lines + 1] = join_columns(
                left,
                right_lines[row] or "",
                left_width,
                right_width,
                state.preview_focused
            )
        end
    else
        local stacked_options = math.min(option_rows, 4, total)
        for row = 1, stacked_options do
            local option_index = state.scroll + row
            if option_index <= total then
                lines[#lines + 1] = compact_option_line(
                    state,
                    state.options[option_index],
                    option_index == state.selected and not state.custom_focused,
                    width
                )
            end
        end
        if state.allow_custom then
            local marker = state.custom_focused and ">" or " "
            lines[#lines + 1] = line(
                span(" " .. marker .. " Custom: ", state.custom_focused and "cyan" or "darkgray", { "bold" }),
                span(clip(state.input, width - 11), state.custom_focused and "white" or "darkgray")
            )
        end
        local preview_rows = math.max(1, body_rows - stacked_options - custom_rows - 1)
        local title, preview_lines = preview_window(state, preview_rows)
        lines[#lines + 1] = line(
            span("Preview ─ ", state.preview_focused and "cyan" or "darkgray"),
            span(title, "white", { "bold" })
        )
        for _, value in ipairs(preview_lines) do lines[#lines + 1] = value end
        while #lines - body_start < body_rows do lines[#lines + 1] = "" end
    end

    local above = state.scroll or 0
    local below = math.max(0, total - above - option_rows)
    if above > 0 or below > 0 then
        lines[#lines + 1] = line(span(
            string.format("    ↑ %d more · ↓ %d more", above, below),
            "darkgray"
        ))
    end
    if state.notice and state.notice ~= "" then
        lines[#lines + 1] = line(span(state.notice, "#E5C07B"))
    end
    local hints = { "↑↓/j/k " .. (state.preview_focused and "scroll" or "move") }
    if state.preview_focusable then
        hints[#hints + 1] = "Tab switch pane"
    elseif state.allow_custom then
        hints[#hints + 1] = "Tab custom"
    end
    if state.multi then hints[#hints + 1] = "Space toggle" end
    hints[#hints + 1] = state.multi and "Enter submit" or "Enter select"
    hints[#hints + 1] = "Esc cancel"
    lines[#lines + 1] = line(span(table.concat(hints, " · "), "darkgray"))
    lines[#lines + 1] = ""
    p:set_lines(lines, target_rows)
end

local function render_select(p, state)
    if state.has_previews then return render_preview_select(p, state) end
    local lines = {}
    render_tabs(lines, state.tabs, state.active_tab)
    if state.question and state.question ~= "" then
        lines[#lines + 1] = line(span(state.question, "white", { "bold" }))
    end
    if state.searchable then
        local cursor = state.filter_focused and "█" or ""
        local count = string.format("  %d/%d", #state.options, #state.all_options)
        lines[#lines + 1] = line(
            span("Filter: ", "darkgray"),
            span(state.filter .. cursor, "white", state.filter_focused and { "bold" } or {}),
            span(count, "darkgray")
        )
    end

    local total = #state.options
    -- Custom-input value wraps under the " > Custom: " label (11 cols); compute
    -- its rows once so both the reserve calc and the render agree.
    local CUSTOM_LABEL_W = 11
    local custom_segments
    if state.allow_custom then
        custom_segments = wrap_input(state.input, (pane_width(p.ctx) or math.huge) - CUSTOM_LABEL_W)
    end
    local custom_rows = custom_segments and math.min(#custom_segments, 4) or 1
    -- Reserve rows for the trailing chrome we render after the options: the
    -- hints legend (1) + trailing blank (1) + the custom-input rows (when
    -- enabled, possibly wrapped to several rows).
    local reserved = 2 + (state.allow_custom and custom_rows or 0)
    local available_rows = math.max(1, rows_for(state) - #lines - reserved)
    local has_descriptions = false
    for _, opt in ipairs(state.options) do
        if opt.description and opt.description ~= "" then
            has_descriptions = true
            break
        end
    end
    local option_rows = math.max(1, math.floor(available_rows / (has_descriptions and 2 or 1)))
    if total > option_rows then
        state.scroll = clamp(state.scroll or 0, 0, math.max(0, total - option_rows))
        if state.selected <= state.scroll then state.scroll = state.selected - 1 end
        if state.selected > state.scroll + option_rows then state.scroll = state.selected - option_rows end
    else
        state.scroll = 0
    end

    local first = (state.scroll or 0) + 1
    local last = math.min(total, first + option_rows - 1)
    local width = pane_width(p.ctx) or math.huge
    if first > 1 then
        lines[#lines + 1] = line(span(clip("    ↑ " .. tostring(first - 1) .. " more", width), "darkgray"))
    end
    for i = first, last do
        local opt = state.options[i]
        local selected = i == state.selected and not state.custom_focused
        local checked = state.checked and state.checked[opt]
        local cursor = selected and ">" or " "
        local cursor_fg = selected and "cyan" or "darkgray"
        local cursor_mods = selected and { "bold" } or {}
        local check = ""
        if state.multi then check = checked and "[x] " or "[ ] " end
        local fg = "white"
        local label_mods = opt.label_modifiers or (selected and { "bold" } or {})
        local existing_marker, label = split_leading_circle(opt.label)
        label = clip(label, width - 3 - #check - (existing_marker and 2 or 0))
        local option_line
        if existing_marker and not state.multi then
            local dot = existing_marker
            local dot_fg = existing_marker == "●" and "#78B373" or "darkgray"
            option_line = line(
                span(" " .. cursor .. " ", cursor_fg, cursor_mods),
                span(dot .. " ", dot_fg),
                span(label, fg, label_mods)
            )
        else
            option_line = line(
                span(" " .. cursor .. " ", cursor_fg, cursor_mods),
                span(check, checked and "#78B373" or "darkgray", checked and { "bold" } or {}),
                span(label, fg, label_mods)
            )
        end
        if selected then option_line.bg = SELECTED_BG end
        lines[#lines + 1] = option_line
        if opt.description and opt.description ~= "" then
            local description_spans = { span("     ", "gray") }
            if opt.description_spans then
                for _, description_span in ipairs(clip_spans(opt.description_spans, width - 5)) do
                    description_spans[#description_spans + 1] = description_span
                end
            else
                description_spans[#description_spans + 1] = span(clip(opt.description, width - 5), "gray")
            end
            local description_line = { spans = description_spans }
            if selected then description_line.bg = SELECTED_BG end
            lines[#lines + 1] = description_line
        end
    end
    if total == 0 then
        lines[#lines + 1] = line(span("   No matches", "darkgray"))
    end
    if last < total then
        lines[#lines + 1] = line(span(clip("    ↓ " .. tostring(total - last) .. " more", width), "darkgray"))
    end
    if state.allow_custom then
        local cursor = state.custom_focused and ">" or " "
        local cursor_fg = state.custom_focused and "cyan" or "darkgray"
        local fg = state.custom_focused and "white" or "darkgray"
        local mods = state.custom_focused and { "bold" } or {}
        for i = 1, custom_rows do
            local seg = custom_segments[i] or ""
            if i == custom_rows and #custom_segments > custom_rows then
                seg = seg .. "…"
            end
            -- First row carries the label; continuation rows indent to align
            -- under the value. The cursor block sits on the last row.
            local prefix = i == 1 and (" " .. cursor .. " Custom: ")
                or string.rep(" ", CUSTOM_LABEL_W)
            local tail = (i == custom_rows and state.custom_focused) and "█" or ""
            lines[#lines + 1] = line(
                span(prefix, cursor_fg, { "bold" }),
                span(seg .. tail, fg, mods)
            )
        end
    end
    -- Transient warning (e.g. an empty multi-select submit was blocked).
    if state.notice and state.notice ~= "" then
        lines[#lines + 1] = line(span(state.notice, "#E5C07B"))
    end
    -- One-line control legend so the keys aren't a guessing game.
    local hints = { "↑↓/j/k move" }
    if state.multi then hints[#hints + 1] = "Space toggle" end
    hints[#hints + 1] = state.multi and "Enter submit" or "Enter select"
    if state.searchable then hints[#hints + 1] = "/ or type filter" end
    if state.allow_custom then hints[#hints + 1] = "Tab custom" end
    hints[#hints + 1] = "Esc cancel"
    lines[#lines + 1] = line(span(table.concat(hints, " · "), "darkgray"))
    lines[#lines + 1] = ""
    p:set_lines(lines, math.min(24, math.max(3, #lines)))
end

local function handle_tab_nav(state, key)
    if key == "Left" and state.tabs and #state.tabs > 0 then
        state.active_tab = state.active_tab <= 1 and #state.tabs or state.active_tab - 1
        return "__prev_tab"
    elseif key == "Right" and state.tabs and #state.tabs > 0 then
        state.active_tab = state.active_tab >= #state.tabs and 1 or state.active_tab + 1
        return "__next_tab"
    end
    if key == "Left" and state.left_value then return state.left_value end
    if key == "Right" and state.right_value then return state.right_value end
    return nil
end

local function select_loop(ctx, spec, multi)
    local p = pane.new(ctx, { id = SOURCE, title = spec.title or "Menu" })
    local all_options = normalize_options(spec.options)
    local has_previews = false
    for _, opt in ipairs(all_options) do
        if opt.preview then
            has_previews = true
            break
        end
    end
    local preview_spec = type(spec.preview) == "table" and spec.preview or {}
    local preview_layout = preview_spec.layout or "auto"
    if preview_layout ~= "auto" and preview_layout ~= "split" and preview_layout ~= "stacked" then
        preview_layout = "auto"
    end
    local preview_interactive = preview_spec.focusable ~= false and preview_spec.scrollable ~= false
    local preview_min_width = math.max(1, math.floor(tonumber(preview_spec.min_width) or 64))
    local state = {
        title = spec.title,
        question = spec.question,
        options = all_options,
        all_options = all_options,
        selected = math.max(1, tonumber(spec.default or 1) or 1),
        checked = {},
        allow_custom = spec.allow_custom or false,
        input = tostring(spec.initial or ""),
        searchable = spec.searchable or false,
        filter = "",
        filter_focused = false,
        tabs = spec.tabs,
        active_tab = spec.active_tab or 1,
        left_value = spec.left_value,
        right_value = spec.right_value,
        visible_rows = spec.visible_rows,
        action_keys = spec.action_keys or {},
        multi = multi,
        scroll = 0,
        has_previews = has_previews,
        preview_layout = preview_layout,
        preview_min_width = preview_min_width,
        preview_focusable = preview_interactive,
        preview_scrollable = preview_interactive,
        preview_focused = false,
        preview_scroll = 0,
    }
    if #state.options == 0 and not state.allow_custom then
        return { cancelled = true }
    end
    state.selected = clamp(state.selected, 1, math.max(1, #state.options))
    if multi then
        for _, initial_value in ipairs(spec.initial_checked or {}) do
            for _, opt in ipairs(state.all_options) do
                if opt.value == initial_value then
                    state.checked[opt] = true
                    break
                end
            end
        end
    end

    while true do
        render_select(p, state)
        local key = wait_key(ctx)
        if not key then return { cancelled = true } end
        state.notice = nil -- clear any transient notice on the next keypress
        local code = key_name(key)
        local prev = state.selected
        local nav = handle_tab_nav(state, code)
        if nav then return { value = nav, navigation = true } end

        local action = state.action_keys[code] or (code == "Char" and state.action_keys[key.char])
        if action and not state.custom_focused then
            return { value = action, selected = state.selected, action_key = true }
        end

        local filter_text
        if state.searchable and not state.custom_focused and is_text_key(key) then
            if state.filter_focused then
                filter_text = key.char
            elseif key.char == "/" then
                state.filter_focused = true
            elseif key.char ~= "j" and key.char ~= "k" then
                state.filter_focused = true
                filter_text = key.char
            end
        end
        if filter_text then
            local selected_value = state.options[state.selected] and state.options[state.selected].value
            state.filter = state.filter .. filter_text
            apply_filter(state, selected_value)
        elseif state.searchable and state.filter_focused and code == "Backspace" then
            local selected_value = state.options[state.selected] and state.options[state.selected].value
            state.filter = state.filter:sub(1, -2)
            apply_filter(state, selected_value)
        elseif state.custom_focused and is_text_key(key) then
            state.input = state.input .. key.char
        elseif state.custom_focused and code == "Backspace" then
            state.input = state.input:sub(1, -2)
        elseif code == "Esc" then
            return { cancelled = true }
        elseif state.has_previews and state.preview_focusable and code == "Tab" then
            if key.shift then
                if state.custom_focused then
                    state.custom_focused = false
                    state.preview_focused = true
                elseif state.preview_focused then
                    state.preview_focused = false
                elseif state.allow_custom then
                    state.custom_focused = true
                else
                    state.preview_focused = true
                end
            elseif state.custom_focused then
                state.custom_focused = false
            elseif state.preview_focused then
                state.preview_focused = false
                state.custom_focused = state.allow_custom
            else
                state.preview_focused = true
            end
        elseif state.has_previews and state.preview_focused
            and (code == "Up" or (code == "Char" and key.char == "k")) then
            state.preview_scroll = clamp((state.preview_scroll or 0) - 1, 0, state.preview_max_scroll or 0)
        elseif state.has_previews and state.preview_focused
            and (code == "Down" or (code == "Char" and key.char == "j")) then
            state.preview_scroll = clamp((state.preview_scroll or 0) + 1, 0, state.preview_max_scroll or 0)
        elseif state.has_previews and state.preview_focused and code == "PageUp" then
            state.preview_scroll = clamp(
                (state.preview_scroll or 0) - (state.preview_page_rows or 1),
                0,
                state.preview_max_scroll or 0
            )
        elseif state.has_previews and state.preview_focused and code == "PageDown" then
            state.preview_scroll = clamp(
                (state.preview_scroll or 0) + (state.preview_page_rows or 1),
                0,
                state.preview_max_scroll or 0
            )
        elseif state.has_previews and state.preview_focused and code == "Home" then
            state.preview_scroll = 0
        elseif state.has_previews and state.preview_focused and code == "End" then
            state.preview_scroll = state.preview_max_scroll or 0
        elseif code == "Up" or (code == "Char" and key.char == "k" and not state.filter_focused) then
            state.filter_focused = false
            if state.custom_focused then
                state.custom_focused = false
                state.selected = #state.options
            elseif state.selected > 1 then
                state.selected = state.selected - 1
            elseif state.allow_custom then
                state.custom_focused = true
            end
        elseif code == "Down" or (code == "Char" and key.char == "j" and not state.filter_focused) then
            state.filter_focused = false
            if state.custom_focused then
                state.custom_focused = false
                state.selected = 1
            elseif state.selected < #state.options then
                state.selected = state.selected + 1
            elseif state.allow_custom then
                state.custom_focused = true
            end
        elseif code == "PageUp" then
            state.selected = clamp(state.selected - 10, 1, math.max(1, #state.options))
            state.custom_focused = false
        elseif code == "PageDown" then
            state.selected = clamp(state.selected + 10, 1, math.max(1, #state.options))
            state.custom_focused = false
        elseif code == "Home" then
            state.selected = 1
            state.custom_focused = false
        elseif code == "End" then
            state.selected = #state.options
            state.custom_focused = false
        elseif code == "Tab" and state.allow_custom then
            state.custom_focused = not state.custom_focused
        elseif code == "Char" and key.char == " " and multi and not state.custom_focused
            and state.options[state.selected] then
            local opt = state.options[state.selected]
            state.checked[opt] = not state.checked[opt]
        elseif code == "Enter" then
            if #state.options == 0 and not state.custom_focused then
                state.notice = "No matching options."
            elseif multi then
                local values = {}
                for _, opt in ipairs(state.all_options) do
                    if state.checked[opt] then values[#values + 1] = opt.value end
                end
                local custom = (state.allow_custom and state.input ~= "") and state.input or nil
                if #values == 0 and not custom then
                    -- Require an explicit choice before advancing.
                    state.notice = "Select at least one option (Space) or type a custom answer."
                else
                    local result = { values = values }
                    if custom then result.custom = custom end
                    result.selected = state.selected
                    return result
                end
            elseif state.custom_focused then
                return { value = state.input, custom = true, selected = state.selected }
            else
                return { value = state.options[state.selected].value, selected = state.selected }
            end
        end
        if state.selected ~= prev then
            state.preview_scroll = 0
            if spec.on_change and state.options[state.selected] then
                spec.on_change(state.options[state.selected].value, state)
            end
        end
    end
end

function M.select(ctx, spec)
    return select_loop(ctx, spec or {}, false)
end

function M.multi_select(ctx, spec)
    return select_loop(ctx, spec or {}, true)
end

function M.text_input(ctx, spec)
    spec = spec or {}
    local p = pane.new(ctx, { id = SOURCE, title = spec.title or "Input" })
    local input = tostring(spec.initial or "")
    while true do
        local lines = { line(span(spec.question or "", "white", { "bold" })) }
        -- Wrap the input under the "> " prefix (2 cols); continuation rows are
        -- indented to align under the text. The cursor block sits at the end of
        -- the last wrapped row.
        local segments = wrap_input(input, (pane_width(ctx) or math.huge) - 2)
        for i, seg in ipairs(segments) do
            local prefix = i == 1 and "> " or "  "
            local tail = i == #segments and "█" or ""
            lines[#lines + 1] = line(span(prefix .. seg .. tail, "white", { "bold" }))
        end
        lines[#lines + 1] = line(span("Enter submit · Esc cancel", "darkgray"))
        lines[#lines + 1] = ""
        p:set_lines(lines, #lines)
        local key = wait_key(ctx)
        if not key then return { cancelled = true } end
        local code = key_name(key)
        if is_text_key(key) then
            input = input .. key.char
        elseif code == "Backspace" then
            input = input:sub(1, -2)
        elseif code == "Esc" then
            return { cancelled = true }
        elseif code == "Enter" then
            return { value = input }
        end
    end
end

function M.clear(ctx)
    pane.new(ctx, { id = SOURCE }):close()
end

return M
