-- /compact — manual context compaction and automatic before-turn reduction.
--
-- Implemented entirely in Lua. Remove or edit this file to disable or
-- customize compaction behaviour.
--
-- Requires: ctx.conversation.history(), ctx.agent.run(), ctx.usage.snapshot(),
--           action = "conversation.replace", bone.on("before_turn", ...)

-- ---------------------------------------------------------------------------
-- Default configuration (overridable via bone.config in init.lua)
-- ---------------------------------------------------------------------------

local AUTO_TOKENS       = 8000   -- trigger auto-compact when context_length >= this
local KEEP_MESSAGES     = 12     -- preserve the last N user/assistant messages
local SUMMARY_TARGET    = 1200   -- target token budget for the summary

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
        "- Keep the summary under " .. SUMMARY_TARGET .. " tokens.",
        "- Write in plain prose, no markdown headings.",
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

--- Run compaction on the current transcript. Returns the replacement messages
--- table, or nil on failure / when history is already small enough.
local function compact(history, ctx)
    if not history or #history == 0 then
        return nil
    end

    -- Filter to user+assistant for the keep window; tool messages between
    -- user/assistant pairs are fragile to reorder, so for v1 we drop them
    -- from the replacement and let the model see only user/assistant.
    local keep = {}
    local older = {}
    local kept = 0

    -- Walk backward to find the KEEP_MESSAGES most recent user/assistant.
    for i = #history, 1, -1 do
        local msg = history[i]
        if msg.role == "user" or msg.role == "assistant" then
            kept = kept + 1
            if kept <= KEEP_MESSAGES then
                keep[#keep + 1] = msg
            else
                older[#older + 1] = msg
            end
        elseif kept > 0 and kept <= KEEP_MESSAGES then
            -- Tool messages within the keep window: preserve them too.
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
    local prompt = summarization_prompt(older, KEEP_MESSAGES)
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

    return messages
end

-- ---------------------------------------------------------------------------
-- Auto-compaction: before_turn hook
-- ---------------------------------------------------------------------------

local last_auto_context = 0

bone.on("before_turn", function(_, ctx)
    -- Safety: skip if usage or conversation APIs are unavailable.
    if not ctx.usage or not ctx.usage.snapshot then
        return nil
    end
    if not ctx.conversation or not ctx.conversation.history then
        return nil
    end

    local snapshot = ctx.usage.snapshot()
    if not snapshot then
        return nil
    end

    local context_length = snapshot.context_length or 0
    if context_length < AUTO_TOKENS then
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

    local messages = compact(history, ctx)
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

        local history = ctx.conversation.history()
        if not history or #history == 0 then
            return { display = "Nothing to compact.", submit = false }
        end

        -- Check if there's enough to compact: need more than KEEP_MESSAGES messages.
        local user_assistant_count = 0
        for _, msg in ipairs(history) do
            if msg.role == "user" or msg.role == "assistant" then
                user_assistant_count = user_assistant_count + 1
            end
        end
        if user_assistant_count <= KEEP_MESSAGES then
            return {
                display = string.format(
                    "History is already small (%d user+assistant messages; threshold: %d).",
                    user_assistant_count, KEEP_MESSAGES
                ),
                submit = false,
            }
        end

        local messages = compact(history, ctx)
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
