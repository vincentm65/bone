-- ask_user — interactive question tool using ui.menu.
--
-- Supports single_select, multi_select, and text_input question types.
-- Questions are rendered in the bottom pane with keyboard-driven
-- selection and optional custom text input.
--
-- Two calling modes:
--   1. Single question: { question, options, allow_custom, type, default }
--   2. Multi-question:  { questions = { {question, options, allow_custom, type, default}, ... } }
--      Asks each question sequentially with backtracking navigation.
--      After answering, user can go back to previous questions or proceed.

local menu = require("ui.menu")

local function format_answer(result)
    if result.values then
        local parts = {}
        for _, v in ipairs(result.values) do
            table.insert(parts, "  - " .. v)
        end
        if result.custom and result.custom ~= "" then
            table.insert(parts, "  Custom: " .. result.custom)
        end
        return table.concat(parts, "\n")
    elseif result.value then
        if result.custom then
            return "Custom answer: " .. result.value
        else
            return result.value
        end
    end
    return "(no response)"
end

local function get_qtype(q)
    local qtype = q.type
    if not qtype then
        local options = q.options or {}
        local allow_custom = q.allow_custom or false
        if #options > 0 then
            if allow_custom or #options > 5 then
                qtype = "multi_select"
            else
                qtype = "single_select"
            end
        else
            qtype = "text_input"
        end
    end
    return qtype
end

-- Flatten object-form options ({label, description}) to plain strings.
local function flatten_options(options)
    local flat = {}
    for i, opt in ipairs(options) do
        if type(opt) == "table" then
            flat[i] = opt.label or tostring(opt)
        else
            flat[i] = opt
        end
    end
    return flat
end

local function ask_one(q, ctx)
    local question = q.question
    local options = flatten_options(q.options or {})
    local allow_custom = q.allow_custom or false
    local qtype = get_qtype(q)

    local spec = {
        question = question,
        options = options,
        default = q.default,
        allow_custom = allow_custom,
    }
    local fn = menu.text_input
    if qtype == "single_select" or qtype == "single" then
        fn = menu.select
    elseif qtype == "multi_select" or qtype == "multi" then
        fn = menu.multi_select
    end
    local ok, result = pcall(fn, ctx, spec)

    if not ok then
        return nil, "interact failed: " .. tostring(result)
    end

    if result.cancelled then
        return nil, "cancelled"
    end

    return format_answer(result)
end

-- Build navigation options after answering a question in multi-question mode
-- Returns: list of option strings for the nav interact call
local function build_nav_options(questions, answers, current_idx)
    local opts = {}

    -- Show summary of answered questions as pickable options to revise
    for i = 1, #questions do
        local short_q = questions[i].question
        if #short_q > 50 then
            short_q = short_q:sub(1, 47) .. "..."
        end
        if answers[i] then
            local short_a = answers[i]
            if #short_a > 40 then
                short_a = short_a:sub(1, 37) .. "..."
            end
            table.insert(opts, string.format("Q%d: %s → %s", i, short_q, short_a))
        else
            table.insert(opts, string.format("Q%d: %s (unanswered)", i, short_q))
        end
    end

    table.insert(opts, "✓ Submit all answers")
    -- Move submit to the top so the cursor starts there
    table.remove(opts, #opts)
    table.insert(opts, 1, "✓ Submit all answers")

    return opts
end

-- Parse what the user picked from navigation
-- Returns: "submit", or a number (question index to jump to)
local function parse_nav_choice(result, num_questions)
    if result.custom then
        local val = tonumber(result.value)
        if val and val >= 1 and val <= num_questions then
            return val
        end
        return "submit" -- fallback
    end

    local selected = result.value
    if not selected then return "submit" end

    if selected == "✓ Submit all answers" then
        return "submit"
    end

    -- Extract question number from "Q%d: ..."
    local qnum = selected:match("^Q(%d+):")
    if qnum then
        return tonumber(qnum)
    end

    return "submit"
end

local function ask_multi_with_backtrack(questions, ctx)
    local answers = {}
    local total = #questions

    -- Fill answers table with nils
    for _ = 1, total do table.insert(answers, nil) end

    -- Start from first unanswered question
    local function find_first_unanswered(start)
        for i = start, total do
            if not answers[i] then return i end
        end
        return nil
    end

    -- Main loop: ask questions, then show nav, repeat until submit
    local ask_from = 1
    while true do
        -- Ask all unanswered questions from ask_from forward
        local idx = find_first_unanswered(ask_from)
        while idx do
            local answer, err = ask_one(questions[idx], ctx)
            if err == "cancelled" then
                -- Treat cancellation as skip (nil answer)
                answers[idx] = nil
            elseif err then
                answers[idx] = "error: " .. err
            else
                answers[idx] = answer
            end
            idx = find_first_unanswered(idx + 1)
        end

        -- Show navigation pane with summary
        local nav_opts = build_nav_options(questions, answers, nil)
        local ok, result = pcall(menu.select, ctx, {
            question = "Review your answers. Pick a question to revise, or submit.",
            options = nav_opts,
            allow_custom = false,
        })

        if not ok or (result and result.cancelled) then
            -- Submit on cancel/escape
            break
        end

        local choice = parse_nav_choice(result, total)
        if choice == "submit" then
            break
        else
            -- Jump back to revise that question
            answers[choice] = nil
            ask_from = choice
        end
    end

    return answers
end

local function execute(params, ctx)
    if not params.question and not (params.questions and #params.questions > 0) then
        return "error: provide either 'question' or 'questions' parameter"
    end

    -- Multi-question mode
    if params.questions and #params.questions > 0 then
        local answers = ask_multi_with_backtrack(params.questions, ctx)

        local parts = {}
        for i, answer in ipairs(answers) do
            if answer then
                table.insert(parts, "Q" .. i .. ": " .. answer)
            else
                table.insert(parts, "Q" .. i .. ": [skipped]")
            end
        end

        local result = table.concat(parts, "\n")
        -- Clear pane after all questions are done
        menu.clear(ctx)
        ctx.ui.notify(result, "info")
        return result
    end

    -- Single-question mode (backward compat)
    local answer, err = ask_one(params, ctx)
    -- Clear pane after the question is answered
    menu.clear(ctx)
    if err == "cancelled" then
        return "[user cancelled]"
    elseif err then
        return err
    end

    local display = answer
    if answer:sub(1, 1) == " " then
        display = "Selected:\n" .. answer
    end
    ctx.ui.notify(display, "info")
    return display
end

bone.register_tool({
    name = "ask_user",
    description = "Ask the user one or more questions with selectable options or custom answers. Use the 'questions' array to ask several questions back-to-back in a single call, or use top-level 'question' + 'options' for a single question.",
    parameters = {
        type = "object",
        properties = {
            question = {
                type = "string",
                description = "The question to ask (single-question mode).",
            },
            options = {
                type = "array",
                description = "List of options for the user to choose from (single-question mode).",
                items = { type = "string" },
            },
            allow_custom = {
                type = "boolean",
                description = "Whether the user can type their own answer (single-question mode).",
            },
            questions = {
                type = "array",
                description = "Multiple questions to ask sequentially with backtracking. After answering all, you can revise any question. Each item is an object with {question, options, allow_custom, type, default}.",
                items = {
                    type = "object",
                    properties = {
                        question = { type = "string", description = "The question to ask." },
                        options = {
                            type = "array",
                            items = { type = "string" },
                            description = "List of options to choose from.",
                        },
                        allow_custom = {
                            type = "boolean",
                            description = "Whether the user can type their own answer.",
                        },
                        type = {
                            type = "string",
                            enum = { "single_select", "multi_select", "text_input" },
                            description = "Question type. Auto-detected if omitted.",
                        },
                        default = {
                            type = "number",
                            description = "Default selected option index (1-based).",
                        },
                    },
                    required = { "question" },
                    additionalProperties = false,
                },
            },
        },
        additionalProperties = false,
    },
    safety = "read_only",
    display = {
        show = false,
        args = { "question", "questions" },
    },
    execute = execute,
})
