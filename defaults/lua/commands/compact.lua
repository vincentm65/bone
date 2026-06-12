-- /compact — manual context compaction and automatic before-turn reduction.
--
-- Implemented entirely in Lua. Remove or edit this file to disable or
-- customize compaction behaviour.
--
-- Requires: ctx.conversation.history(), ctx.agent.run(), ctx.usage.snapshot(),
--           action = "conversation.replace", bone.on("before_turn", ...)

-- ---------------------------------------------------------------------------
-- Configuration — read from config/general.yaml.
-- ---------------------------------------------------------------------------

local function config_int(ctx, key)
    if not ctx.config or not ctx.config.get then
        return nil
    end

    local value = ctx.config.get("general", key)
    if value == nil then
        return nil
    end
    if type(value) == "string" then
        value = value:gsub("^%s+", ""):gsub("%s+$", "")
        if value == "" then
            return nil
        end
    end

    local number = tonumber(value)
    if not number or number < 1 or number ~= math.floor(number) then
        return nil
    end
    return number
end

local function compact_config(ctx)
    return {
        auto_tokens = config_int(ctx, "auto_compact_tokens"),
        keep_messages = config_int(ctx, "auto_compact_keep_messages"),
    }
end

-- ---------------------------------------------------------------------------
-- Helpers
-- ---------------------------------------------------------------------------

--- Build a summary prompt for the model to condense older messages.
local function summarization_prompt(older, recent_count)
    local parts = {
        "You are a context summarizer. Summarize the conversation below into a compact description.",
        "",
        "Instructions:",
        "- Capture key facts, decisions, and user preferences.",
        "- Include file paths, code changes, and errors when relevant.",
        "- Write a concise summary in plain prose, no markdown headings.",
        "",
        "The last " .. recent_count .. " messages are preserved verbatim and will follow this summary.",
        "",
        "--- Conversation to summarize ---",
    }

    for _, msg in ipairs(older) do
        local role = msg.role or "unknown"
        local content = msg.content or ""
        if #content > 2000 then
            content = content:sub(1, 1997) .. "..."
        end
        parts[#parts + 1] = string.format("[%s] %s", role, content)
    end

    return table.concat(parts, "\n")
end

--- Count the approximate token count of a string (chars / 4).
local function estimate_tokens(s)
    return math.ceil(#s / 4)
end

-- ---------------------------------------------------------------------------
-- Core compaction logic
-- ---------------------------------------------------------------------------

local function sanitize_tool_chains(messages)
    -- Pass 1: collect tool_call_ids that have results.
    local result_ids = {}
    for _, msg in ipairs(messages) do
        if msg.role == "tool" and msg.tool_call_id then
            result_ids[msg.tool_call_id] = true
        end
    end

    -- Pass 2: filter assistant tool_calls; collect which ids are kept.
    local kept_call_ids = {}
    local filtered = {}
    for _, msg in ipairs(messages) do
        if msg.role == "assistant" and msg.tool_calls then
            local calls = {}
            for _, call in ipairs(msg.tool_calls) do
                if call.id and result_ids[call.id] then
                    calls[#calls + 1] = call
                    kept_call_ids[call.id] = true
                end
            end
            if #calls > 0 then
                local copy = {}
                for k, v in pairs(msg) do copy[k] = v end
                copy.tool_calls = calls
                filtered[#filtered + 1] = copy
            elseif msg.content and msg.content ~= "" then
                local copy = {}
                for k, v in pairs(msg) do copy[k] = v end
                copy.tool_calls = nil
                filtered[#filtered + 1] = copy
            end
        else
            filtered[#filtered + 1] = msg
        end
    end

    -- Pass 3: filter tool results to only those whose call id was kept.
    local result = {}
    for _, msg in ipairs(filtered) do
        if msg.role == "tool" then
            if msg.tool_call_id and kept_call_ids[msg.tool_call_id] then
                result[#result + 1] = msg
            end
        else
            result[#result + 1] = msg
        end
    end

    return result
end

--- Run compaction on the current transcript. Returns the replacement messages
--- table, or nil on failure / when history is already small enough.
local function compact(history, ctx, keep_messages)
    if not history or #history == 0 then
        return nil
    end

    -- Filter to user+assistant for the keep window; tool messages between
    -- user/assistant pairs are fragile to reorder, so for v1 we drop them
    -- from the replacement and let the model see only user/assistant.
    local keep = {}
    local older = {}

    -- Pass 1: walk backward to find which user/assistant messages are in the
    -- keep window, and collect tool_call_ids from kept assistants so we can
    -- correctly route tool results (a tool result should be kept only if its
    -- matching assistant is in keep).
    local keep_indices = {}
    local kept_call_ids = {}
    local kept = 0
    for i = #history, 1, -1 do
        local msg = history[i]
        if msg.role == "user" or msg.role == "assistant" then
            kept = kept + 1
            if kept <= keep_messages then
                keep_indices[i] = true
                if msg.tool_calls then
                    for _, call in ipairs(msg.tool_calls) do
                        if call.id then
                            kept_call_ids[call.id] = true
                        end
                    end
                end
            end
        end
    end

    -- Pass 2: assign messages to keep or older using the collected data.
    for i = #history, 1, -1 do
        local msg = history[i]
        if keep_indices[i] then
            keep[#keep + 1] = msg
        elseif msg.role == "tool" and msg.tool_call_id and kept_call_ids[msg.tool_call_id] then
            -- This tool result belongs to an assistant in keep. Keep it
            -- regardless of its position (it may trail the last kept user msg).
            keep[#keep + 1] = msg
        else
            older[#older + 1] = msg
        end
    end

    -- If nothing to compact, skip.
    if #older == 0 then
        return nil
    end

    -- Build the summary via ctx.agent.run().
    local prompt = summarization_prompt(older, keep_messages)
    local run_result = ctx.agent.run(prompt, { timeout_ms = 120000 })
    if not run_result.ok then
        ctx.ui.notify("compact: summarization failed: " .. (run_result.error or "unknown"), "warn")
        return nil
    end

    local summary = (run_result.content or ""):gsub("^%s+", ""):gsub("%s+$", "")
    if #summary == 0 then
        ctx.ui.notify("compact: empty summary, skipping", "warn")
        return nil
    end

    -- Build replacement messages: synthetic user summary + preserved messages
    -- (the keep array was built backward, so reverse it).
    local messages = {}
    messages[#messages + 1] = {
        role = "user",
        content = "[Context summary]\n" .. summary,
    }
    for i = #keep, 1, -1 do
        messages[#messages + 1] = keep[i]
    end

    return sanitize_tool_chains(messages)
end

-- ---------------------------------------------------------------------------
-- Auto-compaction: before_turn hook
-- ---------------------------------------------------------------------------

local last_auto_context = 0

bone.on("before_turn", function(event, ctx)
    -- Safety: skip if usage or conversation APIs are unavailable.
    if not ctx.usage or not ctx.usage.snapshot then
        return nil
    end
    if not ctx.conversation or not ctx.conversation.history then
        return nil
    end

    -- Check that the compact command is enabled (respects /config toggle).
    local compact_enabled = ctx.config.get("commands", "compact")
    if compact_enabled ~= true then
        return nil
    end

    local config = compact_config(ctx)
    if not config.auto_tokens or not config.keep_messages then
        return nil
    end

    local snapshot = ctx.usage.snapshot()
    if not snapshot then
        return nil
    end

    local context_length = snapshot.context_length or 0
    if context_length < config.auto_tokens then
        return nil
    end

    -- Avoid repeated runs when context_length hasn't changed meaningfully.
    if math.abs(context_length - last_auto_context) < 50 then
        return nil
    end
    last_auto_context = context_length

    local history = ctx.conversation.history()
    if not history then
        return nil
    end

    local messages = compact(history, ctx, config.keep_messages)
    if not messages then
        return nil
    end

    ctx.ui.notify(
        string.format(
            "compacting: %d messages → %d (context: %d → ~%d tokens)",
            #history, #messages, context_length, estimate_tokens(cjson.encode(messages))
        ),
        "info"
    )

    return {
        action = "conversation.replace",
        messages = messages,
    }
end)

-- ---------------------------------------------------------------------------
-- Manual /compact command
-- ---------------------------------------------------------------------------

bone.register_command("compact", {
    description = "Manually compact conversation context by summarizing older messages",
    handler = function(_, ctx)
        if not ctx.conversation or not ctx.conversation.history then
            return {
                display = "Conversation history not available in this context.",
                submit = false,
            }
        end

        local config = compact_config(ctx)
        if not config.keep_messages then
            return {
                display = "Compaction requires auto_compact_keep_messages in general config.",
                submit = false,
            }
        end

        local history = ctx.conversation.history()
        if not history or #history == 0 then
            return { display = "Nothing to compact.", submit = false }
        end

        -- Check if there's enough to compact: need more than configured keep messages.
        local user_assistant_count = 0
        for _, msg in ipairs(history) do
            if msg.role == "user" or msg.role == "assistant" then
                user_assistant_count = user_assistant_count + 1
            end
        end
        if user_assistant_count <= config.keep_messages then
            return {
                display = string.format(
                    "History is already small (%d user+assistant messages; threshold: %d).",
                    user_assistant_count, config.keep_messages
                ),
                submit = false,
            }
        end

        local messages = compact(history, ctx, config.keep_messages)
        if not messages then
            return { display = "Compaction produced no changes.", submit = false }
        end

        return {
            display = string.format(
                "Compacted: %d messages → %d (~%d tokens).",
                #history, #messages, estimate_tokens(cjson.encode(messages))
            ),
            action = "conversation.replace",
            messages = messages,
            submit = false,
        }
    end,
})
