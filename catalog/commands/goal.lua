-- /goal — Codex-style autonomous goal loop.
--
-- Persists a goal checklist on disk. Two hooks drive an autonomous loop:
--   before_turn → re-injects the goal + procedure via system_prompt_append
--   turn_end    → parses GOAL_STATUS sentinel, submits "Continue" or halts
--
-- Esc (failed/cancelled turn) halts the loop automatically. /goal resume
-- picks back up without rewriting the file.
--
-- Lua only. No Rust, no ctx.state (turn_end has minimal ctx).

-- ---------------------------------------------------------------------------
-- State (module-local — shared across handler + hooks via closure)
-- ---------------------------------------------------------------------------

local state = { active = false, path = nil, iteration = 0 }

-- Session-scoped so multiple CLIs on the same machine/project don't clobber
-- each other's goal file. Falls back to "default" when no session is active.
local function goal_path(ctx)
    local sess = ctx.session and ctx.session.current and ctx.session.current()
    local id = (sess and sess.id) or "default"
    -- Sanitize to filename-safe characters.
    id = id:gsub("[^%w%-]", "-")
    return ctx.config_dir .. "/goals/" .. id .. ".md"
end

-- ---------------------------------------------------------------------------
-- Helpers
-- ---------------------------------------------------------------------------

local function now_ts()
    return os.date("%Y-%m-%d %H:%M")
end

local function build_md(description)
    return string.format(
        "# Goal\n\n%s\n\nCreated: %s\n\n" ..
        "## Acceptance Criteria\n" ..
        "<!-- All must be [x] before done. -->\n" ..
        "- [ ] \n\n" ..
        "## Tasks\n" ..
        "<!-- Small, verifiable steps. Check off as you complete. -->\n" ..
        "- [ ] \n\n" ..
        "## Progress\n" ..
        "<!-- One line per completed task, with timestamp. -->\n",
        description, now_ts()
    )
end

local function shell_quote(s)
    return "'" .. s:gsub("'", "'\\''") .. "'"
end

-- ---------------------------------------------------------------------------
-- before_turn: re-inject goal + procedure every turn
-- ---------------------------------------------------------------------------

bone.on("before_turn", function(_event, ctx)
    if bone.agent_depth ~= 0 then return end
    if not state.active or not state.path then return end

    local content = ""
    if ctx.fs.exists(state.path) then
        local ok, text = pcall(ctx.read_file, state.path)
        if ok then content = text end
    end

    local append = string.format([==[
## Active Goal

You are in an autonomous goal loop (iteration %d). Your source of truth:

%s

## Procedure (every turn)
1. Re-read the checklist above.
2. Pick the next unchecked task (or acceptance criterion if all tasks are done).
3. Do the task.
4. Verify: run builds, tests, or linters to confirm it actually works.
5. Check off completed items (replace [ ] with [x] via edit_file).
6. Append a one-line Progress entry with a timestamp.
7. If ALL acceptance criteria are checked, verify the whole goal once more, then end your response with exactly: GOAL_STATUS: done
8. If you hit a genuine blocker you cannot resolve, end with: GOAL_STATUS: blocked: <reason>
9. Otherwise end with: GOAL_STATUS: working
]==], state.iteration, content)

    return { system_prompt_append = append }
end)

-- ---------------------------------------------------------------------------
-- turn_end: the autonomous loop driver
-- ---------------------------------------------------------------------------

bone.on("turn_end", function(event, _ctx)
    if not state.active then return end

    -- Esc / failure / cancellation halts the loop.
    if not event.ok then
        state.active = false
        bone.log.warn("goal: turn failed/cancelled — loop halted at iteration " .. state.iteration)
        return
    end

    local content = event.content or ""
    state.iteration = state.iteration + 1

    local status = content:match("GOAL_STATUS:%s*(%a+)")
    if status == "done" then
        state.active = false
        bone.log.info("goal: complete after " .. state.iteration .. " iterations")
        return
    elseif status == "blocked" then
        state.active = false
        local reason = content:match("GOAL_STATUS:%s*blocked:%s*(.+)$")
        reason = reason and reason:gsub("^%s+", ""):gsub("%s+$", "")
        bone.log.warn("goal: blocked — " .. (reason or "no reason given"))
        return
    end

    -- working or missing sentinel → continue
    bone.api.submit("Continue the goal.")
end)

-- ---------------------------------------------------------------------------
-- /goal command
-- ---------------------------------------------------------------------------

bone.register_command("goal", {
    description = "Start, resume, check, or stop an autonomous goal.",
    handler = function(args, ctx)
        local arg = (args or ""):gsub("^%s+", ""):gsub("%s+$", "")
        local path = goal_path(ctx)

        -- /goal stop
        if arg == "stop" then
            if not state.active and ctx.fs.exists(path) ~= true then
                return { display = "No active goal.", submit = false }
            end
            state.active = false
            return { display = "Goal loop stopped. File preserved at " .. path, submit = false }
        end

        -- /goal resume
        if arg == "resume" then
            if ctx.fs.exists(path) ~= true then
                return { display = "No goal file to resume. Use /goal <description>.", submit = false }
            end
            state.active = true
            state.path = path
            state.iteration = 0
            return "Resume the autonomous goal. Re-read the checklist, pick up where you left off, and follow the procedure in the system prompt."
        end

        -- /goal status (or bare /goal)
        if arg == "" or arg == "status" then
            if ctx.fs.exists(path) ~= true then
                return { display = "No active goal. Use /goal <description> to start.", submit = false }
            end
            local ok, content = pcall(ctx.read_file, path)
            if not ok then
                return { display = "Could not read: " .. path, submit = false }
            end
            local header = string.format("Iteration: %d  Active: %s\n\n",
                state.iteration, state.active and "yes" or "no")
            return { display = header .. content, submit = false }
        end

        -- /goal <description> — start new goal
        state.active = false -- stop any existing loop first
        local dir = ctx.config_dir .. "/goals"
        if ctx.fs.exists(dir) ~= true then
            ctx.shell("mkdir -p " .. shell_quote(dir))
        end
        local md = build_md(arg)
        if ctx.fs.exists(path) then
            ctx.tools.call("edit_file", { path = path, mode = "rewrite", content = md })
        else
            ctx.write_file(path, md)
        end

        state.active = true
        state.path = path
        state.iteration = 0

        return string.format(
            "I've set up an autonomous goal. The checklist is at %s. " ..
            "Begin working through it following the procedure in the system prompt.",
            path)
    end,
})
