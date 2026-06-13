local DEFAULT_NAME = "Tasks"

local function styled_line(text, done)
    if done then
        return {
            spans = {
                { text = "  ✓ ", fg = "green", modifiers = { "bold" } },
                { text = text, fg = "dark_gray", modifiers = { "strike" } },
            },
        }
    end
    return {
        spans = {
            { text = "  ○ ", fg = "dark_gray" },
            { text = text, fg = "white" },
        },
    }
end

local function emit(state)
    local tasks = state.tasks or {}
    local done = 0
    for _, t in ipairs(tasks) do
        if t[2] then done = done + 1 end
    end
    local total = #tasks
    local name = state.name or DEFAULT_NAME

    local lines = {}
    for _, t in ipairs(tasks) do
        table.insert(lines, styled_line(t[1], t[2]))
    end

    local output = {
        content = string.format("%d/%d done", done, total),
        state = cjson.encode(state),
        pane = {
            source = "task_list",
            title = string.format("%s (%d/%d)", name, done, total),
            visible_rows = 8,
            scroll = 0,
            lines = lines,
        },
    }
    return cjson.encode(output)
end

local function get_texts(params)
    local texts = params.texts or {}
    return texts
end

local function get_indices(params)
    if params.indices then
        return params.indices
    end
    if params.index then
        return { params.index }
    end
    return {}
end

local function execute(params, ctx)
    local action = params.action or ""

    if action == "kill" then
        ctx.state.clear("task_list")
        return cjson.encode({
            content = "Task list cleared.",
            pane = {
                source = "task_list",
                title = DEFAULT_NAME,
                lines = {},
            },
        })
    end

    if action == "create" then
        local texts = get_texts(params)
        if #texts == 0 then
            return "ERROR: Provide texts for 'create'."
        end
        if #texts > 15 then
            return "ERROR: Maximum 15 tasks allowed."
        end
        local name = params.name or DEFAULT_NAME
        local tasks = {}
        for _, t in ipairs(texts) do
            table.insert(tasks, { t, false })
        end
        local state = { name = name, tasks = tasks }
        ctx.state.set("task_list", cjson.encode(state))
        return emit(state)
    end

    if action == "complete" then
        local raw = ctx.state.get("task_list")
        if not raw or raw == "" then
            return "ERROR: No task list state found. Create one first with action=create."
        end
        local state = cjson.decode(raw)
        if not state then
            return "ERROR: Invalid state JSON."
        end
        local indices = get_indices(params)
        if #indices == 0 then
            return "ERROR: Provide index or indices."
        end
        local tasks = state.tasks or {}
        local bad = {}
        for _, idx in ipairs(indices) do
            if idx < 1 or idx > #tasks then
                table.insert(bad, idx)
            end
        end
        if #bad > 0 then
            return string.format("ERROR: Invalid task index/indices: %s", cjson.encode(bad))
        end
        for _, idx in ipairs(indices) do
            tasks[idx][2] = true
        end
        state.tasks = tasks
        ctx.state.set("task_list", cjson.encode(state))

        local all_done = true
        for _, t in ipairs(tasks) do
            if not t[2] then all_done = false break end
        end
        if all_done then
            local summary = ""
            for i, t in ipairs(tasks) do
                if i > 1 then summary = summary .. ", " end
                summary = summary .. t[1]
            end
            return cjson.encode({
                content = string.format("All tasks completed: %s", summary),
                pane = {
                    source = "task_list",
                    title = DEFAULT_NAME,
                    lines = {},
                },
            })
        end
        return emit(state)
    end

    return "ERROR: Action must be create, complete, or kill."
end

bone.register_tool({
    name = "task_list",
    description = "Manage a named visible task list. State is held by the host; no state arg needed. Actions: create (pass texts and optional name, max 15 tasks), complete (pass index/indices), kill.",
    safety = "read_only",
    parameters = {
        type = "object",
        properties = {
            action = {
                type = "string",
                description = "create, complete, or kill",
            },
            name = {
                type = "string",
                description = "Optional task list name for create.",
            },
            texts = {
                type = "array",
                description = "Task strings for create.",
                items = { type = "string" },
            },
            index = {
                type = "number",
                description = "Single 1-based task index for complete.",
            },
            indices = {
                type = "array",
                description = "Multiple 1-based task indices for complete.",
                items = { type = "number" },
            },
        },
        required = { "action" },
        additionalProperties = false,
    },
    display = {
        show = false,
        show_result = false,
        args = { "action", "name", "texts", "index", "indices" },
    },
    execute = execute,
})
