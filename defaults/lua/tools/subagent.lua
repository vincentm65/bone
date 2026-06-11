-- Sub-agent tool: discovery, dispatch, and status reporting.
--
-- Only active when sub-agents are registered via bone.register_subagent in
-- init.lua.  When no agents are registered, this file is a no-op (zero
-- overhead) — the tool and pane are never created.

-- cjson is a global injected by Rust (encode/decode via serde_json)

-- ---------------------------------------------------------------------------
-- Early exit: no registered agents → no tool, no pane.
-- ---------------------------------------------------------------------------

local subagents = bone._subagents
if not subagents or #subagents == 0 then
    return
end

-- ---------------------------------------------------------------------------
-- Helpers
-- ---------------------------------------------------------------------------

--- Build a human-readable status string for a single job.
local function job_status(job)
    if not job then
        return "○ idle"
    end
    local elapsed = os.time() - job.started_at
    local elapsed_s = string.format("%ds", elapsed)

    if job.status == "running" then
        local task = job.task or ""
        if #task > 40 then
            task = task:sub(1, 37) .. "..."
        end
        local sent = job.token_sent or 0
        local received = job.token_received or 0
        return string.format("◑ running %s (%s) %s/%s in/out", task, elapsed_s, sent, received)
    end
    -- done → treat as idle
    if job.status == "done" then
        local sent = job.token_sent or 0
        local received = job.token_received or 0
        if sent > 0 or received > 0 then
            return string.format("○ idle (%s/%s in/out)", sent, received)
        end
        return "○ idle"
    end
    -- error
    return "✗ error"
end

--- Build a status summary string for one agent (latest job).
local function agent_status(agents, jobs)
    local latest = nil
    for _, j in ipairs(jobs) do
        if j.agent == agents.name then
            if not latest or j.started_at > (latest.started_at or 0) then
                latest = j
            end
        end
    end
    return job_status(latest)
end

-- ---------------------------------------------------------------------------
-- render_pane(jobs) → pane table
-- ---------------------------------------------------------------------------

local function render_pane(jobs)
    local lines = {}
    local agent_count = #subagents

    for i, agent in ipairs(subagents) do
        local status = agent_status(agent, jobs)
        local icon
        if status:find("^○") then
            icon = "○"
        elseif status:find("^◑") then
            icon = "◑"
        else
            icon = "✗"
        end
        local name = agent.name
        local line = {
            spans = {
                { text = string.format(" %s ", icon), fg = "white", modifiers = { "bold" } },
                { text = name, fg = "white" },
                { text = " ", fg = "dark_gray" },
                { text = status:gsub("^[✗◑○] ", ""), fg = "dark_gray" },
            },
        }
        table.insert(lines, line)
    end

    return {
        source = "subagents",
        title = string.format("Agents (%d)", agent_count),
        visible_rows = 8,
        scroll = 0,
        lines = lines,
    }
end

-- ---------------------------------------------------------------------------
-- Export hook for Rust: bone._subagents_render(jobs) → pane table
-- ---------------------------------------------------------------------------

bone._subagents_render = function(jobs)
    return render_pane(jobs)
end

-- ---------------------------------------------------------------------------
-- Build dynamic tool description
-- ---------------------------------------------------------------------------

local function build_description()
    local parts = {
        "Dispatch tasks to registered sub-agents for parallel work — research, multi-step edits, long plans, etc. Returns immediately; results are injected automatically when the agent is idle. Use it to split tasks across agents. Never poll or wait for results.",
        "",
        "Registered agents:",
    }
    for _, agent in ipairs(subagents) do
        parts[#parts + 1] = string.format("  - %s: %s", agent.name, agent.description)
    end
    parts[#parts + 1] = ""
    parts[#parts + 1] = "Results arrive automatically in a later turn (auto-injected)."
    parts[#parts + 1] = "Never poll or wait for results."
    return table.concat(parts, "\n")
end

-- ---------------------------------------------------------------------------
-- Tool execute function
-- ---------------------------------------------------------------------------

local function execute(params, ctx)
    local action = params.action or ""

    if action == "dispatch" then
        local tasks = params.tasks or {}
        if #tasks == 0 then
            return "ERROR: Provide tasks for 'dispatch'."
        end

        local results = {}
        local ok_count = 0
        local err_count = 0

        for _, t in ipairs(tasks) do
            local agent_name = t.agent or ""
            local task_desc = t.task or ""

            -- Look up the agent definition
            local agent_def = nil
            for _, a in ipairs(subagents) do
                if a.name == agent_name then
                    agent_def = a
                    break
                end
            end

            if not agent_def then
                results[#results + 1] = string.format(
                    "REJECTED: unknown agent '%s'", agent_name
                )
                err_count = err_count + 1
                goto continue
            end

            -- Check if agent is already running
            local jobs = ctx.agent.jobs()
            local running = false
            for _, j in ipairs(jobs) do
                if j.agent == agent_name and j.status == "running" then
                    results[#results + 1] = string.format(
                        "REJECTED: agent '%s' already running", agent_name
                    )
                    running = true
                    err_count = err_count + 1
                    break
                end
            end
            if running then
                goto continue
            end

            -- Build spawn opts from the agent definition
            local opts = {
                agent = agent_name,
            }
            if agent_def.system_prompt then
                opts.system_prompt = agent_def.system_prompt
            end
            if agent_def.provider then
                opts.provider = agent_def.provider
            end
            if agent_def.model then
                opts.model = agent_def.model
            end
            if agent_def.approval then
                opts.approval = agent_def.approval
            end

            local result = ctx.agent.spawn(task_desc, opts)
            if result.ok then
                results[#results + 1] = string.format(
                    "dispatched %s → %s", result.id, agent_name
                )
                ok_count = ok_count + 1
            else
                results[#results + 1] = string.format(
                    "ERROR: %s — %s", agent_name, result.error or "unknown"
                )
                err_count = err_count + 1
            end

            ::continue::
        end

        local summary = string.format(
            "Dispatched %d, rejected %d", ok_count, err_count
        )
        local jobs = ctx.agent.jobs()
        local pane = render_pane(jobs)

        return cjson.encode({
            content = summary,
            pane = pane,
        })
    end

    if action == "status" then
        local jobs = ctx.agent.jobs()
        local pane = render_pane(jobs)

        -- Build a summary
        local parts = { "Sub-agent status:" }
        for _, agent in ipairs(subagents) do
            parts[#parts + 1] = string.format("  %s: %s", agent.name, agent_status(agent, jobs))
        end
        local summary = table.concat(parts, "\n")

        return cjson.encode({
            content = summary,
            pane = pane,
        })
    end

    return "ERROR: Action must be 'dispatch' or 'status'."
end

-- ---------------------------------------------------------------------------
-- Register the tool
-- ---------------------------------------------------------------------------

bone.register_tool({
    name = "subagent",
    description = build_description(),
    safety = "read_only",
    parameters = {
        type = "object",
        properties = {
            action = {
                type = "string",
                description = "dispatch or status",
            },
            tasks = {
                type = "array",
                description = "List of tasks to dispatch. Each item: {agent: string, task: string}",
                items = {
                    type = "object",
                    properties = {
                        agent = {
                            type = "string",
                            description = "Registered agent name",
                        },
                        task = {
                            type = "string",
                            description = "Task description for the agent",
                        },
                    },
                    required = { "agent", "task" },
                    additionalProperties = false,
                },
            },
        },
        required = { "action" },
        additionalProperties = false,
    },
    display = {
        show = true,
        show_result = false,
        args = { "action", "tasks" },
    },
    execute = execute,
})
