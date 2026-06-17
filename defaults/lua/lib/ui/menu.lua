local M = {}

local SOURCE = "interact"
local MAX_ROWS = 12

local function span(text, fg, modifiers)
    return { text = tostring(text or ""), fg = fg, modifiers = modifiers or {} }
end

local function line(...)
    return { spans = { ... } }
end

local function clamp(n, lo, hi)
    if n < lo then return lo end
    if n > hi then return hi end
    return n
end

local function clear(ctx)
    pcall(ctx.ui.pane, { source = SOURCE, title = "", lines = {} })
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

local function key_name(key)
    if type(key) ~= "table" then return nil end
    return key.code
end

local function next_key(ctx)
    if not ctx or not ctx.ui or type(ctx.ui.key) ~= "function" then
        return nil
    end
    local ok, key = pcall(ctx.ui.key)
    if not ok or type(key) ~= "table" then
        return nil
    end
    return key
end

local function is_text_key(key)
    return type(key) == "table" and key.code == "Char" and key.char and not key.ctrl and not key.alt
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

local function render_select(ctx, state)
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
        local marker = selected and "●" or "○"
        local check = ""
        if state.multi then check = checked and "[x] " or "[ ] " end
        local fg = selected and "white" or "darkgray"
        local marker_fg = selected and "green" or "darkgray"
        local existing_marker, label = split_leading_circle(opt.label)
        if existing_marker and not state.multi then
            local shown_marker = selected and "●" or existing_marker
            lines[#lines + 1] = line(
                span("  " .. shown_marker .. " ", selected and "green" or "darkgray", selected and { "bold" } or {}),
                span(label, fg, selected and { "bold" } or {})
            )
        else
            lines[#lines + 1] = line(
                span("  " .. marker .. " ", marker_fg, selected and { "bold" } or {}),
                span(check, checked and "green" or "darkgray", checked and { "bold" } or {}),
                span(opt.label, fg, selected and { "bold" } or {})
            )
        end
    end
    if last < total then
        lines[#lines + 1] = line(span("    ↓ " .. tostring(total - last) .. " more", "darkgray"))
    end
    if state.allow_custom then
        local marker = state.custom_focused and "●" or "○"
        local fg = state.custom_focused and "white" or "darkgray"
        lines[#lines + 1] = line(
            span("  " .. marker .. " Custom: ", state.custom_focused and "green" or "darkgray", { "bold" }),
            span(state.input .. (state.custom_focused and "█" or ""), fg, state.custom_focused and { "bold" } or {})
        )
    end
    lines[#lines + 1] = ""
    ctx.ui.pane({ source = SOURCE, title = state.title or "Menu", lines = lines, visible_rows = math.min(24, math.max(3, #lines)) })
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
        multi = multi,
        scroll = 0,
    }
    if #state.options == 0 and not state.allow_custom then
        return { cancelled = true }
    end
    state.selected = clamp(state.selected, 1, math.max(1, #state.options))

    while true do
        render_select(ctx, state)
        local key = next_key(ctx)
        if not key then return { cancelled = true } end
        local code = key_name(key)
        local nav = handle_tab_nav(state, code)
        if nav then return { value = nav, navigation = true } end

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
                return result
            end
            if state.custom_focused then
                return { value = state.input, custom = true }
            end
            return { value = state.options[state.selected].value }
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
    local input = tostring(spec.initial or "")
    while true do
        local lines = {
            line(span(spec.question or "", "white", { "bold" })),
            line(span("> " .. input .. "█", "white", { "bold" })),
            "",
        }
        ctx.ui.pane({ source = SOURCE, title = spec.title or "Input", lines = lines, visible_rows = #lines })
        local key = next_key(ctx)
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
    clear(ctx)
end

return M
