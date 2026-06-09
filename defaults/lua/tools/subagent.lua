-- lua/tools/subagent.lua
--
-- Spawn an independent bone agent via `bone run --events`.
-- The subagent has its own conversation loop, tool access, and Lua extensions.
-- Recursion depth is enforced by bone (max 3 levels).

local STRATEGIES = {}

STRATEGIES.code = {
    system = [[You are a focused coding agent. Complete the assigned task precisely.
Rules:
- Make only the changes needed — no scope creep.
- After completing the task, output a brief summary: files changed and what was done.
- If the task fails, output "FAILED:" followed by the error.]],
    suffix = "\n\nOutput only the result summary.",
}

STRATEGIES.review = {
    system = [[You are a code reviewer. Read the relevant files and review thoroughly.
Rules:
- Be concise. No filler.
- Categorize findings: CRITICAL > WARNING > INFO > SUGGESTION.
- Only report verified issues — read files before reporting, do not speculate.
- If no issues found, say "No issues." and stop.
- Output findings only, no preamble.]],
    suffix = "\n\nOutput findings only, categorized by severity.",
}

STRATEGIES.search = {
    system = [[You are a research agent. Find and synthesize information using web_search and file tools.
Rules:
- Return a structured summary with key findings.
- Cite sources where possible.
- Keep it concise.]],
    suffix = "\n\nReturn a structured summary with key findings.",
}

STRATEGIES.plan = {
    system = [[You are a planning agent. Break down the task into ordered, actionable steps.
Rules:
- Output an ordered numbered list.
- Each step must be independently verifiable.
- Note dependencies between steps.
- Be concrete — no vague steps.]],
    suffix = "\n\nOutput the plan as a numbered list only.",
}

STRATEGIES.custom = {
    system = nil,
    suffix = "",
}

local function shell_quote(s)
    return "'" .. s:gsub("'", "'\\''") .. "'"
end

local function truncate(s, max)
    if #s <= max then return s end
    return s:sub(1, max) .. "..."
end

-- Format token counts for display.
local function fmt_tokens(sent, received)
    local total = sent + received
    if total >= 1000000 then
        return string.format("%.1fM", total / 1000000)
    elseif total >= 1000 then
        return string.format("%.1fk", total / 1000)
    else
        return tostring(total)
    end
end

-- Safely parse a JSON line; returns table or nil.
local function parse_json(line)
    if not line or line == "" then return nil end
    local ok, obj = pcall(cjson.decode, line)
    if ok and type(obj) == "table" then return obj end
    return nil
end

local function execute(params, ctx)
    local task = params.task or ""
    local strategy = params.strategy or "code"
    local approval = params.approval or "read_only"
    local model = params.model or ""

    if task == "" then
        return "ERROR: task is required"
    end

    local tmpl = STRATEGIES[strategy]
    if not tmpl then
        local valid = {}
        for k, _ in pairs(STRATEGIES) do table.insert(valid, k) end
        return "ERROR: unknown strategy '" .. strategy .. "'. Available: " .. table.concat(valid, ", ")
    end

    local model_label = model ~= "" and model or "default"

    -- Build bone run command with --events for JSONL output
    local cmd_parts = { "bone", "run", "--events", "--approval", approval }

    if model ~= "" then
        table.insert(cmd_parts, "--model")
        table.insert(cmd_parts, shell_quote(model))
    end

    if tmpl.system then
        local tmp_result = ctx.shell("mktemp")
        if tmp_result.exit_code ~= 0 then
            return "ERROR: could not create temp file: " .. (tmp_result.stderr or "")
        end
        local tmp_path = tmp_result.stdout:match("^%s*(.-)%s*$")

        local write_result = ctx.shell(
            "cat > " .. shell_quote(tmp_path) .. " << 'BONESYSEOF'\n"
            .. tmpl.system .. "\nBONESYSEOF"
        )
        if write_result.exit_code ~= 0 then
            return "ERROR: could not write system prompt: " .. (write_result.stderr or "")
        end

        table.insert(cmd_parts, "--system-prompt")
        table.insert(cmd_parts, "\"$(cat " .. shell_quote(tmp_path) .. ")\"")
    end
    local prompt = task .. (tmpl.suffix or "")
    local cmd = table.concat(cmd_parts, " ")
    local full_cmd = "printf '%s' " .. shell_quote(prompt) .. " | " .. cmd

    -- Live state tracked across JSONL events
    local live_model = model_label
    local live_sent = 0
    local live_received = 0
    local last_content = ""
    local started_at = os.time()

    -- Unique sub_key for this subagent call (used by the merged pane).
    local sub_key = ctx.call_id or "unknown"

    -- Emit AgentState via StateUpdate events for the merged subagents pane.
    local function emit_agent_state(done)
        if not ctx.emit_state then return end
        local state = cjson.encode({
            mode = strategy,
            model = live_model,
            title = truncate(task, 60),
            sent = live_sent,
            received = live_received,
            done = done or false,
            started = started_at,
        })
        ctx.emit_state("subagents", sub_key, state)
    end

    -- Emit initial state
    emit_agent_state(false)

    -- Callback for each stdout line from bone run --events
    local function on_line(line)
        local evt = parse_json(line)
        if not evt then return end

        local evt_type = evt.type or ""

        if evt_type == "started" then
            if evt.model and evt.model ~= "" then
                live_model = evt.model
            end

        elseif evt_type == "token_usage" then
            live_sent = tonumber(evt.sent) or live_sent
            live_received = tonumber(evt.received) or live_received
            emit_agent_state(false)

        elseif evt_type == "finished" then
            last_content = evt.content or ""

        elseif evt_type == "failed" then
            last_content = "FAILED: " .. (evt.message or "unknown error")
        end
    end

    -- Use shell_streaming if available, fall back to plain shell
    local result
    if ctx.shell_streaming then
        result = ctx.shell_streaming(full_cmd, on_line, { timeout_ms = 300000 })
    else
        result = ctx.shell(full_cmd, { timeout_ms = 300000 })
        -- Fallback: try to parse all stdout as JSONL
        if result.exit_code == 0 then
            for line in (result.stdout or ""):gmatch("[^\n]+") do
                on_line(line)
            end
        end
    end

    local ok = result.exit_code == 0
    local output = last_content
    if output == "" then
        output = (result.stdout or ""):match("^%s*(.-)%s*$")
    end
    if output == "" then
        output = ok and "(no output)" or ("FAILED: " .. (result.stderr or "unknown error"))
    end

    -- Emit final state (done=true) so the merged pane shows completion.
    emit_agent_state(true)

    return output
end

bone.register_tool({
    name = "subagent",
    description = [[Spawn an independent AI sub-agent to handle an isolated task. The subagent has its own conversation loop and full tool access (shell, read/write/edit files, web search, etc.).

Use for:
- Parallel tasks (e.g. "fix bug X in file A" while parent handles file B)
- Code review of specific files
- Research tasks requiring web search
- Breaking complex tasks into planned steps
- Any task benefiting from isolation from the parent conversation

Strategies shape the subagent's system prompt:
- "code" (default) — focused coding, returns result summary
- "review" — code review, returns categorized findings
- "search" — research with web search, returns structured summary
- "plan" — breaks down task into ordered actionable steps
- "custom" — raw prompt, no system prompt added

Max recursion depth: 3 levels.]],
    parameters = {
        type = "object",
        properties = {
            task = {
                type = "string",
                description = "The task description for the subagent.",
            },
            strategy = {
                type = "string",
                description = 'Strategy: "code", "review", "search", "plan", or "custom". Default: "code".',
            },
            approval = {
                type = "string",
                description = 'Approval mode for subagent: "read_only" or "danger". Default: "read_only".',
            },
            model = {
                type = "string",
                description = "Override model for the subagent (optional). Uses subagent config default if omitted.",
            },
        },
        required = { "task" },
        additionalProperties = false,
    },
    display = {
        show = true,
        show_result = false,
        args = { "strategy", "approval", "model" },
    },
    safety = "danger",
    execute = execute,
})
