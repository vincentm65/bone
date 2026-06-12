-- ask_user — interactive question tool using ctx.ui.interact()
--
-- Supports single_select, multi_select, and text_input question types.
-- Questions are rendered in the bottom pane with keyboard-driven
-- selection and optional custom text input.
--
-- Two calling modes:
--   1. Single question: { question, options, allow_custom, type, default }
--   2. Multi-question:  { questions = { {question, options, allow_custom, type, default}, ... } }
--      Asks each question sequentially, collecting all answers.


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

local function ask_one(params, ctx)
    local question = params.question
    local options = params.options or {}
    local allow_custom = params.allow_custom or false

    local qtype = params.type
    if not qtype then
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

    local ok, result = pcall(ctx.ui.interact, {
        question = question,
        type = qtype,
        options = options,
        default = params.default,
        allow_custom = allow_custom,
    })

    if not ok then
        return nil, "interact failed: " .. tostring(result)
    end

    if result.cancelled then
        return nil, "cancelled"
    end

    return format_answer(result)
end

local function execute(params, ctx)
    if not params.question and not (params.questions and #params.questions > 0) then
        return "error: provide either 'question' or 'questions' parameter"
    end

    -- Multi-question mode
    if params.questions and #params.questions > 0 then
        local answers = {}
        for i, q in ipairs(params.questions) do
            local answer, err = ask_one(q, ctx)
            if err == "cancelled" then
                table.insert(answers, (i == 1 and "" or "\n") .. "Q" .. i .. ": [cancelled]")
                -- Continue asking remaining questions even if one is cancelled
            elseif err then
                table.insert(answers, (i == 1 and "" or "\n") .. "Q" .. i .. ": error: " .. err)
            else
                table.insert(answers, (i == 1 and "" or "\n") .. "Q" .. i .. ": " .. answer)
            end
        end
        local result = table.concat(answers, "")
        -- Clear pane after all questions are done (not between each)
        pcall(ctx.ui.pane, { source = "interact", title = "", lines = {} })
        ctx.ui.notify(result, "info")
        return result
    end

    -- Single-question mode (backward compat)
    local answer, err = ask_one(params, ctx)
    -- Clear pane after the question is answered
    pcall(ctx.ui.pane, { source = "interact", title = "", lines = {} })
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
                description = "Multiple questions to ask sequentially. Each item is an object with {question, options, allow_custom, type, default}. Use this instead of top-level question+options to batch several prompts into one call.",
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
