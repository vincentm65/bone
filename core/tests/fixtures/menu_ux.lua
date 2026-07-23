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
            set_lines = function(_, lines) ctx.renders[#ctx.renders + 1] = lines end,
            close = function() end,
        }
    end
    return P
end

local menu = dofile("core/defaults/lua/lib/ui/menu.lua")

local function context(keys, width)
    local index = 0
    local ctx = { renders = {} }
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

-- Questions and descriptions wrap at the pane width.
do
    local ctx = context({ key("Enter") }, 16)
    menu.select(ctx, {
        question = "Choose one of these options",
        options = { { label = "Alpha", description = "a description that must wrap" } },
    })
    local lines = ctx.renders[1]
    local description_rows = 0
    for _, rendered in ipairs(lines) do
        if type(rendered) == "table" and rendered.spans and rendered.spans[1]
            and rendered.spans[1].text == "     " then
            description_rows = description_rows + 1
        end
    end
    assert(description_rows > 1, "description did not wrap")
    assert(lines[1].spans[1].text ~= "Choose one of these options", "question did not wrap")
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
