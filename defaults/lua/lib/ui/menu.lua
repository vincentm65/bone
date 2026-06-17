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
    local option_rows = math.max(1, rows_for(state) - #lines - (state.allow_custom and 2 or 1))
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
        local cursor_fg = selected and "white" or "darkgray"
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
        local cursor_fg = state.custom_focused and "white" or "darkgray"
        local fg = state.custom_focused and "white" or "darkgray"
        lines[#lines + 1] = line(
            span(" " .. cursor .. " Custom: ", cursor_fg, { "bold" }),
            span(state.input .. (state.custom_focused and "█" or ""), fg, state.custom_focused and { "bold" } or {})
        )
    end
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
        local code = key_name(key)
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
                local result = { values = values }
                if state.allow_custom and state.input ~= "" then result.custom = state.input end
                result.selected = state.selected
                return result
            end
            if state.custom_focused then
                return { value = state.input, custom = true, selected = state.selected }
            end
            return { value = state.options[state.selected].value, selected = state.selected }
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
        local lines = {
            line(span(spec.question or "", "white", { "bold" })),
            line(span("> " .. input .. "█", "white", { "bold" })),
            "",
        }
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
