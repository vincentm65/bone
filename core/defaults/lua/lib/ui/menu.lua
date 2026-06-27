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
local MAX_ROWS = 12

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

local function normalize_options(options)
    local out = {}
    for i, opt in ipairs(options or {}) do
        if type(opt) == "table" then
            out[i] = {
                label = tostring(opt.label or opt.value or i),
                value = opt.value or opt.label or tostring(i),
                action = opt.action,
            }
        else
            out[i] = { label = tostring(opt), value = opt }
        end
    end
    return out
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
    return state.visible_rows or MAX_ROWS
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

local function render_select(p, state)
    local lines = {}
    render_tabs(lines, state.tabs, state.active_tab)
    if state.question and state.question ~= "" then
        lines[#lines + 1] = line(span(state.question, "white", { "bold" }))
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
    local option_rows = math.max(1, rows_for(state) - #lines - reserved)
    if total > option_rows then
        state.scroll = clamp(state.scroll or 0, 0, math.max(0, total - option_rows))
        if state.selected <= state.scroll then state.scroll = state.selected - 1 end
        if state.selected > state.scroll + option_rows then state.scroll = state.selected - option_rows end
    else
        state.scroll = 0
    end

    local first = (state.scroll or 0) + 1
    local last = math.min(total, first + option_rows - 1)
    if first > 1 then
        lines[#lines + 1] = line(span("    ↑ " .. tostring(first - 1) .. " more", "darkgray"))
    end
    for i = first, last do
        local opt = state.options[i]
        local selected = i == state.selected and not state.custom_focused
        local checked = state.checked and state.checked[i]
        local cursor = selected and ">" or " "
        local cursor_fg = selected and "cyan" or "darkgray"
        local cursor_mods = selected and { "bold" } or {}
        local check = ""
        if state.multi then check = checked and "[x] " or "[ ] " end
        local fg = selected and "white" or "darkgray"
        local existing_marker, label = split_leading_circle(opt.label)
        if existing_marker and not state.multi then
            local dot = existing_marker
            local dot_fg = existing_marker == "●" and "#78B373" or "darkgray"
            lines[#lines + 1] = line(
                span(" " .. cursor .. " ", cursor_fg, cursor_mods),
                span(dot .. " ", dot_fg),
                span(label, fg, selected and { "bold" } or {})
            )
        else
            lines[#lines + 1] = line(
                span(" " .. cursor .. " ", cursor_fg, cursor_mods),
                span(check, checked and "#78B373" or "darkgray", checked and { "bold" } or {}),
                span(opt.label, fg, selected and { "bold" } or {})
            )
        end
    end
    if last < total then
        lines[#lines + 1] = line(span("    ↓ " .. tostring(total - last) .. " more", "darkgray"))
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
    local hints = { "↑↓ move" }
    if state.multi then hints[#hints + 1] = "Space toggle" end
    hints[#hints + 1] = state.multi and "Enter submit" or "Enter select"
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
    local state = {
        title = spec.title,
        question = spec.question,
        options = normalize_options(spec.options),
        selected = math.max(1, tonumber(spec.default or 1) or 1),
        checked = {},
        allow_custom = spec.allow_custom or false,
        input = tostring(spec.initial or ""),
        tabs = spec.tabs,
        active_tab = spec.active_tab or 1,
        left_value = spec.left_value,
        right_value = spec.right_value,
        visible_rows = spec.visible_rows,
        action_keys = spec.action_keys or {},
        multi = multi,
        scroll = 0,
    }
    if #state.options == 0 and not state.allow_custom then
        return { cancelled = true }
    end
    state.selected = clamp(state.selected, 1, math.max(1, #state.options))

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

        if state.custom_focused and is_text_key(key) then
            state.input = state.input .. key.char
        elseif state.custom_focused and code == "Backspace" then
            state.input = state.input:sub(1, -2)
        elseif code == "Esc" then
            return { cancelled = true }
        elseif code == "Up" then
            if state.custom_focused then
                state.custom_focused = false
                state.selected = #state.options
            elseif state.selected > 1 then
                state.selected = state.selected - 1
            elseif state.allow_custom then
                state.custom_focused = true
            end
        elseif code == "Down" then
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
        elseif code == "Char" and key.char == " " and multi and not state.custom_focused then
            state.checked[state.selected] = not state.checked[state.selected]
        elseif code == "Enter" then
            if multi then
                local values = {}
                for i, opt in ipairs(state.options) do
                    if state.checked[i] then values[#values + 1] = opt.value end
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
        if state.selected ~= prev and spec.on_change then
            spec.on_change(state.options[state.selected].value, state)
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
