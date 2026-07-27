-- ui.menu — interactive select / multi_select / text_input panes.
-- multi-space-toggle-v2
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
-- binary lacking `ctx.ui.width`, or not yet drawn). Renderers use 80 columns
-- as the compatibility fallback.
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

local function char_count(value)
    return #utf8_chars(tostring(value or ""))
end

local function edit_text(value, cursor, key, code)
    local chars = utf8_chars(value)
    cursor = clamp(tonumber(cursor) or #chars, 0, #chars)
    if is_text_key(key) then
        table.insert(chars, cursor + 1, key.char)
        return table.concat(chars), cursor + 1
    elseif code == "Backspace" and cursor > 0 then
        table.remove(chars, cursor)
        return table.concat(chars), cursor - 1
    elseif code == "Delete" and cursor < #chars then
        table.remove(chars, cursor + 1)
        return table.concat(chars), cursor
    elseif code == "Left" then
        return value, math.max(0, cursor - 1)
    elseif code == "Right" then
        return value, math.min(#chars, cursor + 1)
    elseif code == "Home" then
        return value, 0
    elseif code == "End" then
        return value, #chars
    end
    return nil, cursor
end

local function with_cursor(value, cursor)
    local chars = utf8_chars(value)
    cursor = clamp(tonumber(cursor) or #chars, 0, #chars)
    table.insert(chars, cursor + 1, "█")
    return table.concat(chars)
end

local function append_wrapped(lines, text, width, fg, modifiers)
    for _, segment in ipairs(wrap_input(tostring(text or ""), math.max(1, width))) do
        lines[#lines + 1] = line(span(segment, fg, modifiers))
    end
end

local function render_heading(lines, state, width)
    if state.progress then
        append_wrapped(lines, state.progress, width, "cyan", { "bold" })
    end
    if state.question and state.question ~= "" then
        append_wrapped(lines, state.question, width, "white", { "bold" })
    end
end

local function one_line(value)
    return tostring(value or ""):gsub("%s+", " ")
end

local function wrap_description(opt, max)
    max = math.max(1, max)
    if not opt.description_spans then
        local rows = {}
        for _, text in ipairs(wrap_input(opt.description or "", max)) do
            rows[#rows + 1] = { span(text, "gray") }
        end
        return rows
    end

    local rows, current, used = {}, {}, 0
    for _, value in ipairs(opt.description_spans) do
        for _, ch in ipairs(utf8_chars(tostring(value.text or ""))) do
            if used >= max then
                rows[#rows + 1], current, used = current, {}, 0
            end
            local last = current[#current]
            local modifiers = value.modifiers
            if last and last.fg == (value.fg or "gray") and last.modifiers == modifiers then
                last.text = last.text .. ch
            else
                current[#current + 1] = span(ch, value.fg or "gray", modifiers)
            end
            used = used + 1
        end
    end
    if #current > 0 then rows[#rows + 1] = current end
    if #rows == 0 then rows[1] = {} end
    return rows
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

local function append_span(rows, value, text)
    if text == "" then return end
    local last = rows[#rows]
    if last and last.fg == value.fg and last.modifiers == value.modifiers then
        last.text = last.text .. text
    else
        rows[#rows + 1] = span(text, value.fg, value.modifiers)
    end
end

local function wrap_line_spans(value, width)
    width = math.max(1, width)
    local source = type(value) == "string" and { span(value, "gray") } or (value and value.spans or {})
    local rows, current, used = {}, {}, 0
    for _, source_span in ipairs(source) do
        for _, ch in ipairs(utf8_chars(tostring(source_span.text or ""))) do
            if used == width then
                rows[#rows + 1] = { spans = current, bg = type(value) == "table" and value.bg or nil }
                current, used = {}, 0
            end
            append_span(current, source_span, ch)
            used = used + 1
        end
    end
    if #current > 0 or #rows == 0 then
        rows[#rows + 1] = { spans = current, bg = type(value) == "table" and value.bg or nil }
    end
    return rows
end

local function append_line_wrapped(lines, value, width)
    for _, wrapped in ipairs(wrap_line_spans(value, width)) do lines[#lines + 1] = wrapped end
end

local function render_tabs(lines, tabs, active, width)
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
    append_line_wrapped(lines, { spans = spans }, width)
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

local function line_spans(value)
    if type(value) == "string" then return { span(value, "gray") } end
    return value and value.spans or {}
end

local function spans_width(values)
    local width = 0
    for _, value in ipairs(values or {}) do width = width + #utf8_chars(tostring(value.text or "")) end
    return width
end

local function compact_option_lines(state, opt, selected, width)
    local checked = state.checked and state.checked[opt]
    local check = state.multi and (checked and "[x] " or "[ ] ") or ""
    local prefix_width = 3 + #check
    local labels = wrap_input(opt.label, math.max(1, width - prefix_width))
    local rows = {}
    for i, label in ipairs(labels) do
        local marker = i == 1 and (selected and ">" or " ") or " "
        local row = line(
            span(" " .. marker .. " ", selected and "cyan" or "darkgray", selected and { "bold" } or {}),
            span(i == 1 and check or string.rep(" ", #check), checked and "#78B373" or "darkgray", checked and { "bold" } or {}),
            span(label, "white", opt.label_modifiers or (selected and { "bold" } or {}))
        )
        if selected then row.bg = SELECTED_BG end
        rows[#rows + 1] = row
    end
    return rows
end

local function compact_custom_lines(state, width)
    local focused = state.custom_focused
    local value = focused and with_cursor(state.input, state.input_cursor) or state.input
    local segments = wrap_input(value, math.max(1, width - 11))
    local rows = {}
    for i, segment in ipairs(segments) do
        local prefix = i == 1 and (" " .. (focused and ">" or " ") .. " Custom: ") or string.rep(" ", 11)
        rows[#rows + 1] = line(
            span(prefix, focused and "cyan" or "darkgray", { "bold" }),
            span(segment, focused and "white" or "darkgray")
        )
    end
    return rows
end

local function join_columns(left, right, left_width, right_width, focused)
    local left_spans = line_spans(left, left_width)
    local out = {}
    for _, value in ipairs(left_spans) do out[#out + 1] = value end
    out[#out + 1] = span(string.rep(" ", math.max(0, left_width - spans_width(left_spans))), "darkgray")
    out[#out + 1] = span(" ┃ ", focused and "cyan" or "darkgray", focused and { "bold" } or {})
    for _, value in ipairs(line_spans(right, right_width)) do out[#out + 1] = value end
    return { spans = out, bg = type(left) == "table" and left.bg or nil }
end

local function selected_preview(state)
    if state.custom_focused then return nil, nil end
    local opt = state.options[state.selected]
    return opt and opt.preview or nil, opt
end

local function preview_values(preview, width)
    local values = {}
    for _, value in ipairs(preview and preview.lines or {}) do
        for _, wrapped in ipairs(wrap_line_spans(value, width)) do values[#values + 1] = wrapped end
    end
    return values
end

local function tallest_preview_rows(state, width)
    local rows = 1
    for _, opt in ipairs(state.all_options) do
        rows = math.max(rows, #preview_values(opt.preview, width))
    end
    return rows
end

local function compact_option_row_count(state, opt, width)
    return #compact_option_lines(state, opt, false, width)
end

local function compact_custom_row_count(state, width)
    return state.allow_custom and #compact_custom_lines(state, width) or 0
end

local function max_stacked_option_rows(state, width)
    local max_rows = 0
    for first = 1, #state.options do
        local rows = 0
        for i = first, math.min(#state.options, first + 3) do
            rows = rows + compact_option_row_count(state, state.options[i], width)
        end
        max_rows = math.max(max_rows, rows)
    end
    return max_rows
end

local function preview_title_rows(state, opt, width, stacked)
    local title = opt and opt.preview and opt.preview.title or (opt and opt.label or "Preview")
    local title_line = stacked
        and line(span("Preview ─ ", state.preview_focused and "cyan" or "darkgray"), span(title, "white", { "bold" }))
        or line(span(title, state.preview_focused and "cyan" or "white", { "bold" }))
    return #wrap_line_spans(title_line, width)
end

local function stacked_option_row_budget(state, opt, width, body_rows, custom_rows)
    local available_rows = math.max(1, body_rows - custom_rows)
    local budget = math.max(1, available_rows - preview_title_rows(state, opt, width, true) - 1)
    if opt then
        budget = math.min(available_rows, math.max(budget, compact_option_row_count(state, opt, width)))
    end
    return budget
end

local function preview_row_budget(state, width, header_rows, left_width, right_width)
    local use_columns = preview_uses_columns(state, width)
    local custom_rows
    local preview_rows
    local option_rows
    if use_columns then
        custom_rows = compact_custom_row_count(state, left_width)
        preview_rows = tallest_preview_rows(state, right_width)
        option_rows = 0
        for _, opt in ipairs(state.options) do
            option_rows = option_rows + compact_option_row_count(state, opt, left_width)
        end
    else
        custom_rows = compact_custom_row_count(state, width)
        preview_rows = tallest_preview_rows(state, width)
        option_rows = max_stacked_option_rows(state, width)
    end

    local max_title_rows = 1
    for _, opt in ipairs(state.all_options) do
        max_title_rows = math.max(max_title_rows, preview_title_rows(
            state,
            opt,
            use_columns and right_width or width,
            not use_columns
        ))
    end
    local raw_body = use_columns
        and math.max(4, preview_rows + max_title_rows, option_rows + custom_rows)
        or math.max(4, option_rows + custom_rows + max_title_rows + preview_rows)
    local raw_overflow = not use_columns and #state.options > math.min(4, #state.options)
    local notice_rows = state.notice and state.notice ~= "" and 1 or 0
    local target_rows
    if state.visible_rows then
        target_rows = clamp(math.floor(tonumber(state.visible_rows) or DEFAULT_ROWS), 3, MAX_ROWS)
    else
        target_rows = clamp(header_rows + raw_body + notice_rows + (raw_overflow and 1 or 0) + 2, 3, MAX_ROWS)
    end
    return target_rows, use_columns
end

local function option_window(state, width, row_budget, max_items)
    local total = #state.options
    if total == 0 then return 1, 0, {}, 0, 0 end
    row_budget = math.max(1, row_budget)
    max_items = max_items or total
    local first = clamp((state.scroll or 0) + 1, 1, total)
    if state.selected < first then first = state.selected end
    local function through_selected(start)
        local rows = 0
        for i = start, state.selected do
            rows = rows + compact_option_row_count(state, state.options[i], width)
        end
        return rows
    end
    while first < state.selected
        and (state.selected - first + 1 > max_items or through_selected(first) > row_budget) do
        first = first + 1
    end

    local rows, used, last = {}, 0, first - 1
    for i = first, total do
        if i - first + 1 > max_items then break end
        local option_lines = compact_option_lines(
            state,
            state.options[i],
            i == state.selected and not state.custom_focused,
            width
        )
        if used > 0 and used + #option_lines > row_budget then break end
        local remaining = row_budget - used
        for row = 1, math.min(#option_lines, remaining) do rows[#rows + 1] = option_lines[row] end
        used = used + math.min(#option_lines, remaining)
        last = i
        if used >= row_budget then break end
    end
    state.scroll = first - 1
    return first, last, rows, first - 1, math.max(0, total - last)
end

local function preview_window(state, width, rows)
    local preview, opt = selected_preview(state)
    local values = preview_values(preview, width)
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
    if #visible == 0 then
        visible = wrap_line_spans(line(span("No preview", "darkgray")), width)
    end
    return title, visible
end

local function preview_hints(state)
    local hints = { "↑↓/j/k " .. (state.preview_focused and "scroll" or "move") }
    if state.preview_focusable then
        hints[#hints + 1] = "Tab switch pane"
    elseif state.allow_custom then
        hints[#hints + 1] = "Tab custom"
    end
    if state.multi then hints[#hints + 1] = "Space toggle" end
    hints[#hints + 1] = state.multi and "Enter submit" or "Enter select"
    if state.allow_back then hints[#hints + 1] = "Alt+← back" end
    if state.allow_forward then hints[#hints + 1] = "Alt+→ next" end
    hints[#hints + 1] = "Esc cancel"
    return table.concat(hints, " · ")
end

local function render_preview_select(p, state)
    local lines = {}
    local width = pane_width(p.ctx) or 80
    render_tabs(lines, state.tabs, state.active_tab, width)
    render_heading(lines, state, width)

    local use_columns = preview_uses_columns(state, width)
    local left_width = use_columns
        and math.min(clamp(math.floor(width * 0.32), 20, 34), math.max(1, width - 4))
        or width
    local right_width = use_columns and math.max(1, width - left_width - 3) or width
    local target_rows = preview_row_budget(state, width, #lines, left_width, right_width)
    local notice_lines = {}
    if state.notice and state.notice ~= "" then append_wrapped(notice_lines, state.notice, width, "#E5C07B") end
    local hint_lines = {}
    append_wrapped(hint_lines, preview_hints(state), width, "darkgray")
    local body_rows = target_rows - #lines - #notice_lines - #hint_lines - 1
    if body_rows < 1 then
        target_rows = math.min(MAX_ROWS, target_rows + 1 - body_rows)
        body_rows = math.max(1, target_rows - #lines - #notice_lines - #hint_lines - 1)
    end

    local custom_lines = state.allow_custom and compact_custom_lines(state, left_width) or {}
    local option_budget
    if use_columns then
        option_budget = math.max(1, body_rows - #custom_lines)
    else
        local _, opt = selected_preview(state)
        option_budget = stacked_option_row_budget(state, opt, width, body_rows, #custom_lines)
    end
    local first, last, option_lines, above, below = option_window(
        state,
        left_width,
        option_budget,
        use_columns and nil or 4
    )
    local overflow_lines = {}
    if above > 0 or below > 0 then
        append_wrapped(
            overflow_lines,
            string.format("    ↑ %d more · ↓ %d more", above, below),
            width,
            "darkgray"
        )
        body_rows = math.max(1, body_rows - #overflow_lines)
        if use_columns then
            option_budget = math.max(1, body_rows - #custom_lines)
        else
            local _, opt = selected_preview(state)
            option_budget = stacked_option_row_budget(state, opt, width, body_rows, #custom_lines)
        end
        first, last, option_lines, above, below = option_window(
            state,
            left_width,
            option_budget,
            use_columns and nil or 4
        )
        overflow_lines = {}
        append_wrapped(
            overflow_lines,
            string.format("    ↑ %d more · ↓ %d more", above, below),
            width,
            "darkgray"
        )
    end

    if use_columns then
        local preview, opt = selected_preview(state)
        local base_title = preview and preview.title or (opt and opt.label or "Preview")
        local title_rows = wrap_line_spans(
            line(span(base_title, state.preview_focused and "cyan" or "white", { "bold" })),
            right_width
        )
        local preview_rows = math.max(1, body_rows - #title_rows)
        local title, preview_lines = preview_window(state, right_width, preview_rows)
        title_rows = wrap_line_spans(
            line(span(title, state.preview_focused and "cyan" or "white", { "bold" })),
            right_width
        )
        preview_rows = math.max(1, body_rows - #title_rows)
        title, preview_lines = preview_window(state, right_width, preview_rows)
        title_rows = wrap_line_spans(
            line(span(title, state.preview_focused and "cyan" or "white", { "bold" })),
            right_width
        )
        local right_lines = {}
        for _, value in ipairs(title_rows) do right_lines[#right_lines + 1] = value end
        for _, value in ipairs(preview_lines) do right_lines[#right_lines + 1] = value end
        local left_lines = option_lines
        for _, value in ipairs(custom_lines) do left_lines[#left_lines + 1] = value end
        for row = 1, body_rows do
            lines[#lines + 1] = join_columns(
                left_lines[row] or "",
                right_lines[row] or "",
                left_width,
                right_width,
                state.preview_focused
            )
        end
    else
        local body_start = #lines
        for _, value in ipairs(option_lines) do lines[#lines + 1] = value end
        for _, value in ipairs(custom_lines) do lines[#lines + 1] = value end
        local preview, opt = selected_preview(state)
        local base_title = preview and preview.title or (opt and opt.label or "Preview")
        local title_rows = wrap_line_spans(line(
            span("Preview ─ ", state.preview_focused and "cyan" or "darkgray"),
            span(base_title, "white", { "bold" })
        ), width)
        local preview_rows = math.max(1, body_rows - (#lines - body_start) - #title_rows)
        local title, preview_lines = preview_window(state, width, preview_rows)
        title_rows = wrap_line_spans(line(
            span("Preview ─ ", state.preview_focused and "cyan" or "darkgray"),
            span(title, "white", { "bold" })
        ), width)
        preview_rows = math.max(1, body_rows - (#lines - body_start) - #title_rows)
        title, preview_lines = preview_window(state, width, preview_rows)
        title_rows = wrap_line_spans(line(
            span("Preview ─ ", state.preview_focused and "cyan" or "darkgray"),
            span(title, "white", { "bold" })
        ), width)
        local available_title_rows = math.max(0, body_rows - (#lines - body_start) - 1)
        for row = 1, math.min(#title_rows, available_title_rows) do lines[#lines + 1] = title_rows[row] end
        local available_preview_rows = math.max(0, body_rows - (#lines - body_start))
        for row = 1, math.min(#preview_lines, available_preview_rows) do
            lines[#lines + 1] = preview_lines[row]
        end
        while #lines - body_start < body_rows do lines[#lines + 1] = "" end
    end

    for _, value in ipairs(overflow_lines) do lines[#lines + 1] = value end
    for _, value in ipairs(notice_lines) do lines[#lines + 1] = value end
    for _, value in ipairs(hint_lines) do lines[#lines + 1] = value end
    lines[#lines + 1] = ""
    p:set_lines(lines, target_rows)
end

local function option_row_count(state, opt, width)
    local existing_marker, label = split_leading_circle(opt.label)
    local marker_width = existing_marker and not state.multi and 2 or 0
    local check_width = state.multi and 4 or 0
    local rows = #wrap_input(label, math.max(1, width - 3 - check_width - marker_width))
    if opt.description and opt.description ~= "" then
        rows = rows + #wrap_description(opt, width - 5)
    end
    return rows
end

local function select_hints(state)
    local hints = { "↑↓/j/k move" }
    if state.multi then hints[#hints + 1] = "Space toggle" end
    hints[#hints + 1] = state.multi and "Enter submit" or "Enter select"
    if state.searchable then hints[#hints + 1] = "/ or type filter" end
    if state.allow_custom then hints[#hints + 1] = "Tab custom" end
    if state.allow_back then hints[#hints + 1] = "Alt+← back" end
    if state.allow_forward then hints[#hints + 1] = "Alt+→ next" end
    hints[#hints + 1] = "Esc cancel"
    return table.concat(hints, " · ")
end

local function render_select(p, state)
    if state.has_previews then return render_preview_select(p, state) end
    local lines = {}
    local width = pane_width(p.ctx) or 80
    render_tabs(lines, state.tabs, state.active_tab, width)
    render_heading(lines, state, width)
    if state.searchable then
        local cursor = state.filter_focused and "█" or ""
        local count = string.format("  %d/%d", #state.options, #state.all_options)
        append_line_wrapped(lines, line(
            span("Filter: ", "darkgray"),
            span(state.filter .. cursor, "white", state.filter_focused and { "bold" } or {}),
            span(count, "darkgray")
        ), width)
    end

    local total = #state.options
    -- Custom-input value wraps under the " > Custom: " label (11 cols); compute
    -- its rows once so both the reserve calc and the render agree.
    local CUSTOM_LABEL_W = 11
    local custom_segments
    if state.allow_custom then
        local displayed = state.custom_focused and with_cursor(state.input, state.input_cursor) or state.input
        custom_segments = wrap_input(displayed, math.max(1, width - CUSTOM_LABEL_W))
    end
    local custom_rows = custom_segments and #custom_segments or 1
    local notice_rows = state.notice and state.notice ~= ""
        and #wrap_input(state.notice, width) or 0
    local hint_text = select_hints(state)
    local hint_rows = #wrap_input(hint_text, width)
    local reserved = 1 + notice_rows + hint_rows + (state.allow_custom and custom_rows or 0)
    local base_available_rows = math.max(1, rows_for(state) - #lines - reserved)

    local function calculate_window(available_rows)
        local first = clamp((state.scroll or 0) + 1, 1, math.max(1, total))
        if state.selected < first then first = state.selected end
        local function rows_through_selected(start)
            local rows = 0
            for i = start, math.min(state.selected, total) do
                rows = rows + option_row_count(state, state.options[i], width)
            end
            return rows
        end
        while first < state.selected and rows_through_selected(first) > available_rows do
            first = first + 1
        end
        local used_rows, last = 0, first - 1
        for i = first, total do
            local rows = option_row_count(state, state.options[i], width)
            if used_rows > 0 and used_rows + rows > available_rows then break end
            used_rows = used_rows + rows
            last = i
            if used_rows >= available_rows then break end
        end
        return first, last
    end

    local first, last = calculate_window(base_available_rows)
    local function indicator_rows(window_first, window_last)
        local rows = 0
        if window_first > 1 then rows = rows + #wrap_input("    ↑ " .. tostring(window_first - 1) .. " more", width) end
        if window_last < total then rows = rows + #wrap_input("    ↓ " .. tostring(total - window_last) .. " more", width) end
        return rows
    end
    local indicators = indicator_rows(first, last)
    if indicators > 0 then
        first, last = calculate_window(math.max(1, base_available_rows - indicators))
        indicators = indicator_rows(first, last)
        first, last = calculate_window(math.max(1, base_available_rows - indicators))
    end
    state.scroll = first - 1

    if first > 1 then
        append_wrapped(lines, "    ↑ " .. tostring(first - 1) .. " more", width, "darkgray")
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
        local marker_width = existing_marker and not state.multi and 2 or 0
        local label_rows = wrap_input(label, math.max(1, width - 3 - #check - marker_width))
        for row_index, label_row in ipairs(label_rows) do
            local option_line
            local row_cursor = row_index == 1 and cursor or " "
            if existing_marker and not state.multi then
                local dot_fg = existing_marker == "●" and "#78B373" or "darkgray"
                option_line = line(
                    span(" " .. row_cursor .. " ", cursor_fg, cursor_mods),
                    span(row_index == 1 and (existing_marker .. " ") or "  ", dot_fg),
                    span(label_row, fg, label_mods)
                )
            else
                option_line = line(
                    span(" " .. row_cursor .. " ", cursor_fg, cursor_mods),
                    span(row_index == 1 and check or string.rep(" ", #check), checked and "#78B373" or "darkgray", checked and { "bold" } or {}),
                    span(label_row, fg, label_mods)
                )
            end
            if selected then option_line.bg = SELECTED_BG end
            lines[#lines + 1] = option_line
        end
        if opt.description and opt.description ~= "" then
            for _, wrapped_spans in ipairs(wrap_description(opt, width - 5)) do
                local description_spans = { span("     ", "gray") }
                for _, description_span in ipairs(wrapped_spans) do
                    description_spans[#description_spans + 1] = description_span
                end
                local description_line = { spans = description_spans }
                if selected then description_line.bg = SELECTED_BG end
                lines[#lines + 1] = description_line
            end
        end
    end
    if total == 0 then
        append_wrapped(lines, "   No matches", width, "darkgray")
    end
    if last < total then
        append_wrapped(lines, "    ↓ " .. tostring(total - last) .. " more", width, "darkgray")
    end
    if state.allow_custom then
        local cursor = state.custom_focused and ">" or " "
        local cursor_fg = state.custom_focused and "cyan" or "darkgray"
        local fg = state.custom_focused and "white" or "darkgray"
        local mods = state.custom_focused and { "bold" } or {}
        for i = 1, custom_rows do
            local seg = custom_segments[i] or ""
            -- First row carries the label; continuation rows indent to align
            -- under the value. The cursor block sits on the last row.
            local prefix = i == 1 and (" " .. cursor .. " Custom: ")
                or string.rep(" ", CUSTOM_LABEL_W)
            lines[#lines + 1] = line(
                span(prefix, cursor_fg, { "bold" }),
                span(seg, fg, mods)
            )
        end
    end
    -- Transient warning (e.g. an empty multi-select submit was blocked).
    if state.notice and state.notice ~= "" then
        append_wrapped(lines, state.notice, width, "#E5C07B")
    end
    -- Control legend so the keys aren't a guessing game.
    append_wrapped(lines, hint_text, width, "darkgray")
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

local function cycle_focus(state, reverse)
    local focuses = { "options" }
    if state.preview_focusable then focuses[#focuses + 1] = "preview" end
    if state.allow_custom then focuses[#focuses + 1] = "custom" end
    local current = state.preview_focused and "preview" or (state.custom_focused and "custom" or "options")
    local index = 1
    for i, value in ipairs(focuses) do
        if value == current then index = i break end
    end
    index = ((index - 1 + (reverse and -1 or 1)) % #focuses) + 1
    state.preview_focused = focuses[index] == "preview"
    state.custom_focused = focuses[index] == "custom"
    state.filter_focused = false
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
        progress = spec.progress,
        allow_back = spec.allow_back or false,
        allow_forward = spec.allow_forward or false,
        options = all_options,
        all_options = all_options,
        selected = math.max(1, tonumber(spec.default or 1) or 1),
        checked = {},
        allow_custom = spec.allow_custom or false,
        input = tostring(spec.initial or ""),
        input_cursor = char_count(spec.initial or ""),
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
        preview_focusable = has_previews and preview_interactive,
        preview_scrollable = has_previews and preview_interactive,
        preview_focused = false,
        custom_focused = spec.initial_custom or false,
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
        if state.allow_back and key.alt and code == "Left" then
            local result = { back = true, selected = state.selected }
            if multi then
                result.values = {}
                for _, opt in ipairs(state.all_options) do
                    if state.checked[opt] then result.values[#result.values + 1] = opt.value end
                end
                if state.input ~= "" then result.custom = state.input end
            elseif state.custom_focused then
                result.value, result.custom = state.input, true
            elseif state.options[state.selected] then
                result.value = state.options[state.selected].value
            end
            return result
        end
        if state.allow_forward and key.alt and code == "Right" then code = "Enter" end
        local nav = not state.custom_focused and handle_tab_nav(state, code) or nil
        if nav then return { value = nav, navigation = true } end

        local action = state.action_keys[code] or (code == "Char" and state.action_keys[key.char])
        if action and not state.custom_focused then
            return { value = action, selected = state.selected, action_key = true }
        end

        local filter_text
        if state.searchable and not state.custom_focused and is_text_key(key)
            and not (multi and key.char == " ") then
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
            state.filter = select(1, edit_text(state.filter, char_count(state.filter), key, code)) or state.filter
            apply_filter(state, selected_value)
        elseif state.custom_focused and (is_text_key(key) or code == "Backspace" or code == "Delete"
            or code == "Left" or code == "Right" or code == "Home" or code == "End") then
            local edited, cursor = edit_text(state.input, state.input_cursor, key, code)
            if edited ~= nil then state.input = edited end
            state.input_cursor = cursor
        elseif code == "Esc" then
            return { cancelled = true }
        elseif code == "Tab" and (state.preview_focusable or state.allow_custom) then
            cycle_focus(state, key.shift)
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
    local cursor = char_count(input)
    while true do
        local width = pane_width(ctx) or 80
        local lines = {}
        render_heading(lines, spec, width)
        local segments = wrap_input(with_cursor(input, cursor), math.max(1, width - 2))
        for i, segment in ipairs(segments) do
            local prefix = i == 1 and "> " or "  "
            lines[#lines + 1] = line(span(prefix .. segment, "white", { "bold" }))
        end
        local hints = { "←→ move", "Home/End", "Enter submit" }
        if spec.allow_back then hints[#hints + 1] = "Alt+← back" end
        if spec.allow_forward then hints[#hints + 1] = "Alt+→ next" end
        hints[#hints + 1] = "Esc cancel"
        append_wrapped(lines, table.concat(hints, " · "), width, "darkgray")
        lines[#lines + 1] = ""
        p:set_lines(lines, math.min(MAX_ROWS, #lines))
        local key = wait_key(ctx)
        if not key then return { cancelled = true } end
        local code = key_name(key)
        if spec.allow_back and key.alt and code == "Left" then
            return { back = true, value = input }
        elseif spec.allow_forward and key.alt and code == "Right" then
            return { value = input }
        elseif code == "Esc" then
            return { cancelled = true }
        elseif code == "Enter" then
            return { value = input }
        else
            local edited
            edited, cursor = edit_text(input, cursor, key, code)
            if edited ~= nil then input = edited end
        end
    end
end

local function copy_spec(value)
    local out = {}
    for key, item in pairs(value or {}) do out[key] = item end
    return out
end

function M.questions(ctx, spec)
    spec = spec or {}
    local questions = spec.questions or spec
    local answers = {}
    local index = 1
    while index <= #questions do
        local question = copy_spec(questions[index])
        local prior = answers[index]
        question.progress = question.progress or string.format("Question %d of %d", index, #questions)
        question.allow_back = index > 1
        question.allow_forward = index < #questions
        if prior then
            question.default = prior.selected or question.default
            question.initial = prior.custom or prior.value or question.initial
            question.initial_custom = prior.custom ~= nil
            question.initial_checked = prior.values or question.initial_checked
        end

        local kind = question.type
        if not kind then kind = question.options and "single_select" or "text_input" end
        local result
        if kind == "multi_select" or kind == "multi" then
            result = M.multi_select(ctx, question)
        elseif kind == "text_input" or kind == "text" then
            result = M.text_input(ctx, question)
        else
            result = M.select(ctx, question)
        end

        if result.cancelled then return result end
        if result.back then
            answers[index] = result
            index = index - 1
        else
            answers[index] = result
            index = index + 1
        end
    end
    return { answers = answers }
end

function M.clear(ctx)
    pane.new(ctx, { id = SOURCE }):close()
end

return M
