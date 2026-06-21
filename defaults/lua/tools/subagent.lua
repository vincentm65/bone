-- Sub-agent tool: discovery, dispatch, and status reporting.
--
-- Only active when sub-agents are registered via bone.register_subagent in
-- init.lua.  When no agents are registered, this file is a no-op (zero
-- overhead) — the tool is never created.
--
-- The live status pane is rendered natively in Rust (src/ui/subagent_pane.rs)
-- so it stays responsive even while this tool blocks the Lua VM.

-- cjson is a global injected by Rust (encode/decode via serde_json)

-- ---------------------------------------------------------------------------
-- Early exits
-- ---------------------------------------------------------------------------

-- Sub-agents must not spawn nested sub-agents: never register the tool
-- inside a sub-agent VM.
if bone.agent_depth and bone.agent_depth > 0 then
    return
end

local subagents = bone._subagents
if not subagents or #subagents == 0 then
    return
end

-- Headless mode (CLI): background job auto-injection is unavailable, so
-- dispatch must always block for results.
local headless = bone.headless and true or false

-- ---------------------------------------------------------------------------
-- Helpers
-- ---------------------------------------------------------------------------

--- Numeric part of a "job-N" id (0 when malformed).
local function job_id_number(id)
    return tonumber((id or ""):match("(%d+)$")) or 0
end

--- Build a human-readable status string for a single job.
local function job_status(job)
    if not job then
        return "○ idle"
    end

    if job.status == "running" then
        local elapsed = os.time() - job.started_at
        local task = job.title ~= nil and job.title ~= "" and job.title or (job.task or "")
        if #task > 40 then
            task = task:sub(1, 37) .. "..."
        end
        local sent = job.token_sent or 0
        local received = job.token_received or 0
        return string.format("◑ running %s (%ds) %s/%s in/out", task, elapsed, sent, received)
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

--- Build a status summary string for one agent (latest job by id).
local function agent_status(agent, jobs)
    local latest = nil
    for _, j in ipairs(jobs) do
        if j.agent == agent.name then
            if not latest or job_id_number(j.id) > job_id_number(latest.id) then
                latest = j
            end
        end
    end
    return job_status(latest)
end

-- ---------------------------------------------------------------------------
-- Build dynamic tool description
-- ---------------------------------------------------------------------------

local function build_description()
    local parts = {
        "Delegate tasks to registered sub-agents. Each agent runs in its own isolated context and only sees the task text you give it — write self-contained tasks (include file paths, goals, constraints, and the expected output format).",
        "",
        "Registered agents:",
    }
    for _, agent in ipairs(subagents) do
        local extras = {}
        if agent.approval then
            extras[#extras + 1] = "approval: " .. agent.approval
        end
        if agent.max_concurrency and agent.max_concurrency > 1 then
            extras[#extras + 1] = "concurrency: " .. agent.max_concurrency
        end
        local suffix = #extras > 0 and (" [" .. table.concat(extras, ", ") .. "]") or ""
        parts[#parts + 1] = string.format("  - %s: %s%s", agent.name, agent.description, suffix)
    end
    parts[#parts + 1] = ""
    if headless then
        parts[#parts + 1] = table.concat({
            "Actions:",
            '- dispatch: start one or more tasks (one tasks[] entry each, run in parallel). Blocks until all dispatched tasks finish and returns their results.',
            '- wait: block until previously dispatched jobs finish. Waits on the given ids[] (or all running jobs when omitted) and returns their results.',
            '- cancel: stop running jobs by id. Sets a per-job cancel flag.',
            '- status: non-blocking snapshot of job progress. Use sparingly; never call status in a loop.',
            "",
            "Rules:",
            "- Batch independent tasks into a single dispatch call to maximize parallelism.",
            "- Each agent runs up to its `max_concurrency` jobs at a time (default 1); dispatching beyond the cap is rejected.",
        }, "\n")
    else
        parts[#parts + 1] = table.concat({
            "Actions:",
            '- dispatch: start one or more tasks (one tasks[] entry each, run in parallel).',
            '  - If you need the results to continue, or you have nothing else productive to do, set wait=true: the call blocks and returns the results directly. This is the right choice for fan-out/fan-in work (e.g. dispatching research and then synthesizing it).',
            '  - Omit wait ONLY when you have separate, independent work to do that does NOT overlap the dispatched tasks: dispatch returns immediately and you continue on that other work. Finished results are delivered automatically in a later message — do NOT poll for them, and NEVER fabricate or assume results you have not received.',
            '- wait: block until previously dispatched jobs finish. Waits on the given ids[] (or all running jobs when omitted) and returns their results. Use when you reach a point where you need pending results before continuing.',
            '- cancel: stop running jobs by id. Sets a per-job cancel flag.',
            '- status: non-blocking snapshot of job progress. Use sparingly; never call status in a loop.',
            "",
            "Rules:",
            "- Batch independent tasks into a single dispatch call to maximize parallelism.",
            "- Each agent runs up to its `max_concurrency` jobs at a time (default 1); dispatching beyond the cap is rejected.",
            "- NEVER duplicate the work you delegated. Once a task is dispatched, do not read the same files, run the same searches, or research the same questions yourself — that wastes context and defeats the purpose of delegating. Let the sub-agent do it.",
        }, "\n")
    end
    return table.concat(parts, "\n")
end

-- ---------------------------------------------------------------------------
-- Result formatting helpers
-- ---------------------------------------------------------------------------

-- Per-job character budget for results returned by wait (mirrors the Rust
-- auto-injection limit).
local MAX_RESULT_CHARS = 16000

--- Truncate to a byte budget without splitting a UTF-8 sequence.
local function truncate_result(s, max)
    if #s <= max then
        return s
    end
    local cut = max
    while cut > 0 do
        local b = s:byte(cut + 1)
        if not b or b < 0x80 or b >= 0xC0 then
            break
        end
        cut = cut - 1
    end
    return s:sub(1, cut) .. "\n[... truncated]"
end

--- Format a ctx.agent.wait outcome into a report for the model.
local function format_wait_outcome(outcome)
    local parts = {}
    for _, job in ipairs(outcome.jobs or {}) do
        local sym = job.status == "done" and "done" or "ERROR"
        local body = truncate_result(job.result or "", MAX_RESULT_CHARS)
        if job.result_file then
            body = body .. string.format("\n[full output saved to: %s]", job.result_file)
        end
        parts[#parts + 1] = string.format(
            "## %s (%s) — %s\n%s",
            job.agent ~= "" and job.agent or "agent",
            job.id,
            sym,
            body
        )
    end
    if outcome.cancelled then
        parts[#parts + 1] = "Wait cancelled by user; jobs keep running and their results will arrive automatically."
    elseif outcome.timed_out then
        parts[#parts + 1] = string.format(
            "Wait timed out with jobs still running: %s. Their results will arrive automatically in a later message — do not assume their outcome.",
            table.concat(outcome.pending or {}, ", ")
        )
    end
    if #parts == 0 then
        parts[#parts + 1] = "No jobs to wait on."
    end
    return table.concat(parts, "\n\n")
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
        local dispatched_ids = {}
        local ok_count = 0
        local err_count = 0

        for _, t in ipairs(tasks) do
            local agent_name = t.agent or ""
            local task_desc = t.task or ""
            local title = t.title or ""

            -- Look up the agent definition
            local agent_def = nil
            for _, a in ipairs(subagents) do
                if a.name == agent_name then
                    agent_def = a
                    break
                end
            end

            if agent_def then
                -- Build spawn opts from the agent definition.
                local opts = {
                    agent = agent_name,
                    title = title,
                    system_prompt = agent_def.system_prompt,
                    provider = agent_def.provider,
                    model = agent_def.model,
                    approval = agent_def.approval,
                    timeout_ms = agent_def.timeout_ms,
                    max_concurrency = agent_def.max_concurrency or 1,
                }
                if agent_def.tools and #agent_def.tools > 0 then
                    opts.tools = agent_def.tools
                end

                local result = ctx.agent.spawn(task_desc, opts)
                if result.ok then
                    results[#results + 1] = string.format(
                        "dispatched %s → %s", result.id, agent_name
                    )
                    dispatched_ids[#dispatched_ids + 1] = result.id
                    ok_count = ok_count + 1
                else
                    results[#results + 1] = string.format(
                        "REJECTED: %s — %s", agent_name, result.error or "unknown"
                    )
                    err_count = err_count + 1
                end
            else
                results[#results + 1] = string.format(
                    "REJECTED: unknown agent '%s'", agent_name
                )
                err_count = err_count + 1
            end
        end

        local summary = string.format(
            "Dispatched %d, rejected %d", ok_count, err_count
        )
        if err_count > 0 then
            summary = summary .. "\n" .. table.concat(results, "\n")
        end

        -- Blocking dispatch: wait for the dispatched jobs and return results.
        -- Headless mode always blocks (no background auto-injection there).
        local should_wait = params.wait or headless
        if should_wait and #dispatched_ids > 0 then
            local outcome = ctx.agent.wait(dispatched_ids, { timeout_ms = params.timeout_ms })
            if outcome.ok then
                summary = summary .. "\n\n" .. format_wait_outcome(outcome)
            else
                summary = summary .. "\n\nERROR waiting: " .. (outcome.error or "unknown")
            end
        end

        return summary
    end

    if action == "wait" then
        local outcome = ctx.agent.wait(params.ids, { timeout_ms = params.timeout_ms })
        if not outcome.ok then
            return "ERROR: " .. (outcome.error or "unknown")
        end
        return format_wait_outcome(outcome)
    end

    if action == "cancel" then
        local ids = params.ids or {}
        if #ids == 0 then
            return { ok = false, error = "Provide ids for 'cancel'." }
        end
        local parts = {}
        for _, id in ipairs(ids) do
            local result = ctx.agent.cancel(id)
            parts[#parts + 1] = string.format(
                "%s: %s", id, result.ok and "cancelled" or "not found"
            )
        end
        return table.concat(parts, "\n")
    end

    if action == "status" then
        local jobs = ctx.agent.jobs()
        local parts = { "Sub-agent status:" }
        for _, agent in ipairs(subagents) do
            parts[#parts + 1] = string.format("  %s: %s", agent.name, agent_status(agent, jobs))
        end
        return table.concat(parts, "\n")
    end

    return "ERROR: Action must be 'dispatch', 'wait', 'cancel' or 'status'."
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
                enum = { "dispatch", "wait", "cancel", "status" },
                description = "dispatch (start tasks), wait (block for results), cancel (stop jobs by id), or status (non-blocking snapshot)",
            },
            tasks = {
                type = "array",
                description = "dispatch only: list of tasks to start in parallel. Each item: {agent: string, title: string, task: string}",
                items = {
                    type = "object",
                    properties = {
                        agent = {
                            type = "string",
                            description = "Registered agent name",
                        },
                        title = {
                            type = "string",
                            description = "Short (≤ ~8 word) human-readable summary of the task, shown in the UI (live pane + tool-call row). E.g. \"Review unstaged changes for bugs\".",
                        },
                        task = {
                            type = "string",
                            description = "Self-contained task description for the agent (it sees nothing else)",
                        },
                    },
                    required = { "agent", "title", "task" },
                    additionalProperties = false,
                },
            },
            wait = {
                type = "boolean",
                description = "dispatch only: block until the dispatched tasks finish and return the results. Use when your next step depends on them.",
            },
            ids = {
                type = "array",
                items = { type = "string" },
                description = "wait/cancel: job ids to wait for (omit to wait for all running jobs) / job ids to cancel",
            },
            timeout_ms = {
                type = "integer",
                description = "dispatch(wait=true)/wait: max time to block in milliseconds (default 300000)",
            },
        },
        required = { "action" },
        additionalProperties = false,
    },
    display = {
        show = true,
        show_result = false,
        -- Dispatch/wait calls block until the agents finish, so render the row
        -- at call time rather than on completion.
        eager = true,
        -- Dispatch label: each task's title (falling back to its task text).
        -- For non-dispatch actions `tasks` is absent, so the template yields
        -- nothing and the row falls back to the `args` label below.
        template = "dispatch: {tasks[].title|task}",
        args = { "action", "tasks", "wait", "ids" },
    },
    execute = execute,
})
