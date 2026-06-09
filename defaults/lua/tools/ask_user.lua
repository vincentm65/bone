local function execute(params, ctx)
    local question = params.question
    local options = params.options or {}
    local allow_custom = params.allow_custom or false

    local lines = {}
    table.insert(lines, question)
    table.insert(lines, "")
    for i, opt in ipairs(options) do
        table.insert(lines, string.format("  %d. %s", i, opt))
    end
    if allow_custom then
        table.insert(lines, string.format("  %d. (custom answer)", #options + 1))
    end
    table.insert(lines, "")
    table.insert(lines, "Reply with your choice (number or text).")

    return table.concat(lines, "\n")
end

bone.register_tool({
    name = "ask_user",
    description = "Ask the user a question with selectable options or a custom answer (interaction tool)",
    parameters = {
        type = "object",
        properties = {
            question = {
                type = "string",
                description = "The question to ask",
            },
            options = {
                type = "array",
                description = "List of options for the user to choose from",
                items = { type = "string" },
            },
            allow_custom = {
                type = "boolean",
                description = "Whether the user can type their own answer",
            },
        },
        required = { "question", "options" },
        additionalProperties = false,
    },
    safety = "read_only",
    display = {
        show = false,
        args = { "question" },
    },
    execute = execute,
})
