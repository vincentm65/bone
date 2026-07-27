package.preload["ui.pane"] = function()
    local P = {}
    P.span = function(text, fg, modifiers) return { text = text, fg = fg, modifiers = modifiers } end
    P.line = function(...) return { spans = { ... } } end
    P.clamp = function(n, lo, hi) return math.max(lo, math.min(n, hi)) end
    P.wait_key = function(ctx) return ctx.ui.key() end
    P.key_name = function(key) return key.code end
    P.is_text_key = function(key)
        return key.code == "Char" and key.char and not key.ctrl and not key.alt
    end
    P.new = function(ctx)
        return {
            ctx = ctx,
            set_lines = function(_, lines, visible_rows)
                ctx.renders[#ctx.renders + 1] = lines
                ctx.visible_rows[#ctx.visible_rows + 1] = visible_rows
            end,
            close = function() end,
        }
    end
    return P
end

local menu = dofile("core/defaults/lua/lib/ui/menu.lua")

local function context(keys, width)
    local index = 0
    local ctx = { renders = {}, visible_rows = {} }
    ctx.ui = {
        width = function() return width or 80 end,
        pane = function() end,
        key = function()
            index = index + 1
            return keys[index]
        end,
    }
    return ctx
end

local function char_count(value)
    local count = 0
    for _ in tostring(value or ""):gmatch("[\1-\127\194-\244][\128-\191]*") do count = count + 1 end
    return count
end

local function line_text(rendered)
    if type(rendered) == "string" then return rendered end
    local values = {}
    for _, value in ipairs(rendered.spans or {}) do values[#values + 1] = value.text or "" end
    return table.concat(values)
end

local function assert_wrapped(ctx, width)
    for render_index, rendered_lines in ipairs(ctx.renders) do
        for row, rendered in ipairs(rendered_lines) do
            assert(char_count(line_text(rendered)) <= width,
                string.format("render %d row %d exceeds width: %s", render_index, row, line_text(rendered)))
        end
    end
end

local function key(code, char, extra)
    local value = extra or {}
    value.code, value.char = code, char
    return value
end

-- UTF-8 deletion and insertion operate on characters, not bytes.
do
    local ctx = context({ key("Left"), key("Backspace"), key("Char", "x"), key("Enter") })
    local result = menu.text_input(ctx, { initial = "aé" })
    assert(result.value == "xé", result.value)
end

-- Questions, option labels, and descriptions wrap at the pane width.
do
    local label = "Alpha option label that must wrap"
    local ctx = context({ key("Enter") }, 16)
    menu.select(ctx, {
        question = "Choose one of these options",
        options = { { label = label, description = "a description that must wrap" } },
    })
    local lines = ctx.renders[1]
    local description_rows = 0
    local rendered_label = ""
    for _, rendered in ipairs(lines) do
        if type(rendered) == "table" and rendered.spans and rendered.spans[1]
            and rendered.spans[1].text == "     " then
            description_rows = description_rows + 1
        elseif type(rendered) == "table" and rendered.spans and rendered.spans[3]
            and (rendered.spans[1].text == " > " or rendered.spans[1].text == "   ") then
            rendered_label = rendered_label .. rendered.spans[3].text
        end
    end
    assert(description_rows > 1, "description did not wrap")
    assert(rendered_label == label, "option label did not wrap: " .. rendered_label)
    assert(lines[1].spans[1].text ~= "Choose one of these options", "question did not wrap")
end

-- Preview menus wrap option labels too, rather than clipping them.
do
    local label = "Preview option label that is much wider than the pane"
    local ctx = context({ key("Esc") }, 24)
    menu.select(ctx, {
        preview = { layout = "stacked" },
        options = { { label = label, preview = { lines = { "preview" } } } },
    })
    local rendered_label = ""
    for _, rendered in ipairs(ctx.renders[1]) do
        if type(rendered) == "table" and rendered.spans and rendered.spans[3]
            and (rendered.spans[1].text == " > " or rendered.spans[1].text == "   ") then
            rendered_label = rendered_label .. rendered.spans[3].text
        end
    end
    assert(rendered_label == label, "preview option label did not wrap: " .. rendered_label)
    assert_wrapped(ctx, 24)
end

-- Tabs, notices, overflow chrome, and hints all stay within the pane.
do
    local ctx = context({ key("Enter"), key("Esc") }, 22)
    menu.multi_select(ctx, {
        tabs = { { title = "A very long first tab" }, { title = "Second long tab" } },
        options = {
            "First option with a long label",
            "Second option with a long label",
            "Third option with a long label",
            "Fourth option with a long label",
        },
        visible_rows = 10,
    })
    assert(#ctx.renders == 2, "expected notice render")
    assert_wrapped(ctx, 22)
end

-- Stacked preview titles and styled content wrap without losing span styling.
do
    local title = "A preview title that wraps fully"
    local red, green = "styled red preview text ", "followed by green preview text"
    local ctx = context({ key("Esc") }, 20)
    menu.select(ctx, {
        visible_rows = 12,
        preview = { layout = "stacked" },
        options = { {
            label = "Alpha",
            preview = {
                title = title,
                lines = { { spans = {
                    { text = red, fg = "red", modifiers = { "bold" } },
                    { text = green, fg = "green" },
                } } },
            },
        } },
    })
    local rendered_title, rendered_red, rendered_green = "", "", ""
    for _, rendered in ipairs(ctx.renders[1]) do
        for _, value in ipairs(type(rendered) == "table" and rendered.spans or {}) do
            if value.modifiers and value.modifiers[1] == "bold" and value.fg == "white" then
                rendered_title = rendered_title .. value.text
            elseif value.fg == "red" then
                rendered_red = rendered_red .. value.text
            elseif value.fg == "green" then
                rendered_green = rendered_green .. value.text
            end
        end
    end
    assert(rendered_title:find(title, 1, true), "preview title was lost: " .. rendered_title)
    assert(rendered_red == red and rendered_green == green, "styled preview content was lost")
    assert_wrapped(ctx, 20)
end

-- Forced split previews wrap both columns, including custom input, at narrow widths.
do
    local input = "custom answer that wraps across the left column"
    local ctx = context({ key("Esc") }, 30)
    menu.select(ctx, {
        visible_rows = 18,
        preview = { layout = "split" },
        allow_custom = true,
        initial = input,
        initial_custom = true,
        options = { {
            label = "A selected option label that wraps in the left column",
            preview = {
                title = "A long split preview title",
                lines = { "Split preview content that wraps across several right-side rows" },
            },
        } },
    })
    local rendered_input = ""
    for _, rendered in ipairs(ctx.renders[1]) do
        if type(rendered) == "table" and rendered.spans then
            for _, value in ipairs(rendered.spans) do
                if value.fg == "white" and not value.modifiers then
                    rendered_input = rendered_input .. value.text
                end
            end
        end
    end
    assert(rendered_input:find(input, 1, true), "split custom input was lost: " .. rendered_input)
    assert_wrapped(ctx, 30)
    assert(#ctx.renders[1] == ctx.visible_rows[1], "split preview exceeded its row budget")
end

-- Wrapped-row scrolling keeps every row of the selected preview option visible.
do
    local selected_label = "Eighth selected option label wraps completely"
    local options = {}
    for i = 1, 7 do
        options[i] = { label = "Long option number " .. i .. " wraps", preview = { lines = { "preview" } } }
    end
    options[8] = { label = selected_label, preview = { lines = { "preview" } } }
    local ctx = context({ key("End"), key("Esc") }, 24)
    menu.select(ctx, { visible_rows = 12, preview = { layout = "stacked" }, options = options })
    local rendered_label = ""
    for _, rendered in ipairs(ctx.renders[#ctx.renders]) do
        if type(rendered) == "table" and rendered.bg == "#3A3F4B" and rendered.spans[3] then
            rendered_label = rendered_label .. rendered.spans[3].text
        end
    end
    assert(rendered_label == selected_label, "selected wrapped option was hidden: " .. rendered_label)
    assert_wrapped(ctx, 24)
    assert(#ctx.renders[#ctx.renders] == ctx.visible_rows[#ctx.visible_rows], "stacked preview exceeded its row budget")
end

-- Automatic preview height grows for every wrapped notice row.
do
    local ctx = context({ key("Enter"), key("Esc") }, 20)
    menu.multi_select(ctx, {
        preview = { layout = "stacked" },
        options = { { label = "Alpha", preview = { lines = { "preview" } } } },
    })
    local notice_rows = 0
    for _, rendered in ipairs(ctx.renders[2]) do
        if type(rendered) == "table" and rendered.spans and rendered.spans[1]
            and rendered.spans[1].fg == "#E5C07B" then
            notice_rows = notice_rows + 1
        end
    end
    assert(notice_rows > 1, "preview notice did not wrap")
    assert(ctx.visible_rows[2] - ctx.visible_rows[1] == notice_rows,
        "automatic preview height did not reserve wrapped notice rows")
    assert(#ctx.renders[2] == ctx.visible_rows[2], "automatic preview exceeded its row budget")
end

-- An option taller than an explicit viewport is bounded by that viewport.
do
    local ctx = context({ key("Esc") }, 30)
    menu.select(ctx, {
        visible_rows = 8,
        preview = { layout = "stacked" },
        options = { {
            label = string.rep("oversized option label ", 12),
            preview = { lines = { "preview" } },
        } },
    })
    assert(ctx.visible_rows[1] == 8, "explicit preview height changed")
    assert(#ctx.renders[1] == 8, "oversized option exceeded its row budget")
    assert_wrapped(ctx, 30)
end

-- A custom field without a preview remains one Tab away.
do
    local ctx = context({ key("Tab"), key("Char", "x"), key("Enter") })
    local result = menu.select(ctx, { allow_custom = true, options = { "Alpha" } })
    assert(result.custom == true and result.value == "x")
end

-- Tab cycles options -> preview -> custom; custom editing keeps an in-place cursor.
do
    local ctx = context({ key("Tab"), key("Tab"), key("Char", "x"), key("Left"), key("Char", "é"), key("Enter") }, 100)
    local result = menu.select(ctx, {
        allow_custom = true,
        options = { { label = "Alpha", preview = { lines = { "preview" } } } },
    })
    assert(result.custom == true)
    assert(result.value == "éx", result.value)
end

-- Alt+Right submits and advances; Alt+Left backtracking preserves drafts and completed answers.
do
    local ctx = context({
        key("Right", nil, { alt = true }),
        key("Char", "z"), key("Left", nil, { alt = true }),
        key("Down"), key("Enter"),
        key("Left"), key("Char", "x"), key("Enter"),
    })
    local result = menu.questions(ctx, {
        { question = "First?", options = { "a", "b" } },
        { question = "Second?", type = "text_input" },
    })
    assert(result.answers[1].value == "b")
    assert(result.answers[2].value == "xz", result.answers[2].value)
    local saw_progress = false
    for _, render in ipairs(ctx.renders) do
        if render[1] and render[1].spans and render[1].spans[1].text == "Question 2 of 2" then
            saw_progress = true
        end
    end
    assert(saw_progress, "question progress was not rendered")
end

print("menu UX tests passed")
