local DEFAULT_NAME = "Tasks"
local MAX_TASKS = 15

local VALID_STATUS = { pending = true, in_progress = true, done = true }

local function styled_line(text, status)
    if status == "done" then
        return {
            spans = {
                { text = "  ✓ ", fg = "#78B373", modifiers = { "bold" } },
                { text = text, fg = "dark_gray", modifiers = { "strike" } },
            },
        }
    end
    if status == "in_progress" then
        return {
            spans = {
                { text = "  ◐ ", fg = "#E5C07B", modifiers = { "bold" } },
                { text = text, fg = "white", modifiers = { "bold" } },
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

-- Normalize one task entry into { text = string, status = "pending"|... }.
-- Accepts a bare string (→ pending) or a table { text=, status= }.
local function normalize_task(entry)
    if type(entry) == "string" then
        return { text = entry, status = "pending" }
    end
    if type(entry) == "table" then
        local text = entry.text or entry[1]
        if type(text) ~= "string" or text == "" then
            return nil
        end
        local status = entry.status
        if not VALID_STATUS[status] then
            status = "pending"
        end
        return { text = text, status = status }
    end
    return nil
end

local function empty_pane(name)
    return {
        source = "task_list",
        title = name or DEFAULT_NAME,
        lines = {},
    }
end

local function emit(state, all_done_msg)
    local tasks = state.tasks or {}
    local done = 0
    for _, t in ipairs(tasks) do
        if t.status == "done" then done = done + 1 end
    end
    local total = #tasks
    local name = state.name or DEFAULT_NAME

    local lines = {}
    for _, t in ipairs(tasks) do
        table.insert(lines, styled_line(t.text, t.status))
    end

    local content = string.format("%d/%d done", done, total)
    if all_done_msg then content = all_done_msg end

    local output = {
        content = content,
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

local function execute(params, ctx)
    local action = params.action or ""

    if action == "clear" then
        ctx.state.clear("task_list")
        return cjson.encode({
            content = "Task list cleared.",
            pane = empty_pane(),
        })
    end

    if action == "write" then
        local raw_tasks = params.tasks
        if type(raw_tasks) ~= "table" then
            return "ERROR: 'write' requires a 'tasks' array."
        end
        if #raw_tasks == 0 then
            return "ERROR: Provide at least one task, or use action=clear to remove the list."
        end
        if #raw_tasks > MAX_TASKS then
            return string.format("ERROR: Maximum %d tasks allowed.", MAX_TASKS)
        end

        local tasks = {}
        local in_progress = 0
        for i, entry in ipairs(raw_tasks) do
            local t = normalize_task(entry)
            if not t then
                return string.format("ERROR: Task %d is invalid (need a non-empty string or {text, status}).", i)
            end
            if t.status == "in_progress" then in_progress = in_progress + 1 end
            table.insert(tasks, t)
        end
        if in_progress > 1 then
            return "ERROR: Keep at most one task 'in_progress' at a time."
        end

        local state = { name = params.name or DEFAULT_NAME, tasks = tasks }
        ctx.state.set("task_list", cjson.encode(state))

        local all_done = true
        for _, t in ipairs(tasks) do
            if t.status ~= "done" then all_done = false break end
        end
        if all_done then
            return emit(state, "All tasks complete.")
        end
        return emit(state)
    end

    return "ERROR: Action must be 'write' or 'clear'."
end

bone.register_tool({
    name = "task_list",
    description = "Maintain a visible checklist (TUI pane) for the user. Use it for any task with ~3+ distinct steps or work spanning multiple files. Call 'write' with the FULL list every time — it replaces the whole list, so there are no indices to track. Keep exactly one item 'in_progress' (the step you're working on now) and flip items to 'done' the moment they're finished, then write again. State is host-held; no state arg. Actions: write (pass tasks, optional name, max 15), clear.",
    safety = "read_only",
    -- Host-managed state: the host serializes batched calls and threads the
    -- prior list back in (state_key defaults to the tool name, "task_list").
    stateful = true,
    parameters = {
        type = "object",
        properties = {
            action = {
                type = "string",
                description = "'write' (replace the full list) or 'clear' (remove the list).",
                enum = { "write", "clear" },
            },
            name = {
                type = "string",
                description = "Optional list title shown in the pane.",
            },
            tasks = {
                type = "array",
                description = "Full ordered task list for 'write'. Each item is either a string (defaults to pending) or { text, status } where status is pending | in_progress | done.",
                items = {
                    oneOf = {
                        { type = "string" },
                        {
                            type = "object",
                            properties = {
                                text = { type = "string" },
                                status = {
                                    type = "string",
                                    enum = { "pending", "in_progress", "done" },
                                },
                            },
                            required = { "text" },
                        },
                    },
                },
            },
        },
        required = { "action" },
        additionalProperties = false,
    },
    display = {
        show = false,
        show_result = false,
        args = { "action", "name", "tasks" },
    },
    execute = execute,
})

-- ---------------------------------------------------------------------------
-- before_turn: keep the list salient and nudge the model to maintain it.
-- Root agent only (the pane renders only at depth 0). Uses turn_message (a
-- transient trailing input item), not system_prompt_append: this text changes
-- as the list changes, and a mutating system prompt busts the provider's
-- prefix cache for the whole conversation.
-- ---------------------------------------------------------------------------

bone.on("before_turn", function(_event, ctx)
    if bone.agent_depth ~= 0 then return end

    local raw = ctx.state.get("task_list")
    local state
    if raw and raw ~= "" then
        local ok, decoded = pcall(cjson.decode, raw)
        if ok then state = decoded end
    end

    -- No active list → brief suggestion to use one for multi-step work.
    if not state or type(state.tasks) ~= "table" or #state.tasks == 0 then
        return {
            turn_message =
                "For any task with ~3+ steps or multi-file work, call task_list (action=write) to track progress in a visible checklist.",
        }
    end

    local tasks = state.tasks
    local done = 0
    local current = nil
    for _, t in ipairs(tasks) do
        if t.status == "done" then
            done = done + 1
        elseif t.status == "in_progress" and not current then
            current = t.text
        end
    end

    -- All done → no reminder needed (offer to clear).
    if done >= #tasks then
        return {
            turn_message =
                "Your task list is complete. Call task_list (action=clear) once the user has confirmed you're finished.",
        }
    end

    local current_line = current
        and string.format(" Current in-progress item: \"%s\".", current)
        or " No item is marked in_progress — mark the one you're working on now."
    return {
        turn_message = string.format(
            "Active task list: %d/%d done.%s As you work, call task_list (action=write) with the full list to mark items in_progress/done. Keep exactly one item in_progress.",
            done, #tasks, current_line),
    }
end)
