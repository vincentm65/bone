-- /history — pick a recent conversation and load it as the current chat.
--
-- Uses ctx.ui.interact() for an arrow-key picker. This works in a slash command
-- because the command runner now drives the handler through the shared
-- live-pane loop (App::drive_live), the same loop tools use — so the pane
-- renders and keystrokes reach it.

local function truncate(s, max_len)
    s = tostring(s or "")
    s = s:gsub("%s+", " ")
    if #s <= max_len then return s end
    return s:sub(1, max_len - 3) .. "..."
end

-- Stored timestamps are UTC ISO 8601 (e.g. 2026-06-14T14:15:32Z). Convert to
-- the user's local time and render a compact "YYYY-MM-DD HH:MM".
local function format_when(when)
    when = tostring(when or "")
    local y, mo, d, h, mi = when:match(
        "^(%d%d%d%d)%-(%d%d)%-(%d%d)[T ](%d%d):(%d%d)"
    )
    if not y then
        -- Unrecognized format: fall back to a trimmed slice of the raw string.
        return truncate(when, 16)
    end

    -- `os.time` interprets a broken-down table as *local* time, so this is the
    -- UTC fields read as local — an approximation that's exact except within the
    -- ~1h DST transition window (fine for display).
    local approx = os.time({
        year = tonumber(y), month = tonumber(mo), day = tonumber(d),
        hour = tonumber(h), min = tonumber(mi), sec = 0,
    })
    if not approx then
        return truncate(when, 16)
    end
    -- Local UTC offset at that time, e.g. "-0400". Using strftime %z reflects
    -- DST correctly (unlike re-encoding os.date("!*t"), which loses the flag).
    local sign, oh, om = os.date("%z", approx):match("([%+%-])(%d%d)(%d%d)")
    local offset = 0
    if sign then
        offset = (tonumber(oh) * 3600 + tonumber(om) * 60)
            * (sign == "-" and -1 or 1)
    end
    return os.date("%Y-%m-%d %H:%M", approx + offset)
end

-- Compaction injects synthetic messages whose content starts with this marker
-- (see compact.lua). They're noise in the picker and the restored chat, so we
-- skip them in both the preview and the loaded transcript.
local function is_compaction(content)
    return type(content) == "string" and content:sub(1, 17) == "[Context summary]"
end

local function first_user_preview(messages)
    for _, msg in ipairs(messages or {}) do
        if msg.role == "user" and msg.content and msg.content ~= ""
            and not is_compaction(msg.content) then
            return truncate(msg.content, 60)
        end
    end
    return "(no preview)"
end

-- A conversation is only worth showing if we actually drove it. The
-- conversations table also accumulates noise the picker should hide:
--   * empty placeholders — the TUI creates a row on startup/clear/new even if
--     no message is ever sent (0 messages),
--   * compaction-only rows — synthetic "[Context summary]" injections,
--   * trivial rows — anything with a single message and no real user turn.
-- Require more than one non-compaction message AND at least one real user turn
-- (so it's a conversation we initiated, not scaffolding). Sub-agent runs can't
-- be told apart in the DB, but most are caught by this same bar.
local function is_relevant(messages)
    local count = 0
    local has_user_turn = false
    for _, msg in ipairs(messages or {}) do
        if not is_compaction(msg.content) then
            count = count + 1
            if msg.role == "user" and msg.content and msg.content ~= "" then
                has_user_turn = true
            end
        end
    end
    return count > 1 and has_user_turn
end

local function label_for(session, preview)
    local provider = session.provider or "?"
    local model = session.model or "?"
    return string.format("%s  %s/%s  #%s  %s",
        format_when(session.started_at),
        truncate(provider, 14),
        truncate(model, 24),
        tostring(session.id),
        preview)
end

local function valid_message(msg)
    if type(msg) ~= "table" then return nil end
    if msg.role ~= "user" and msg.role ~= "assistant" and msg.role ~= "tool" then return nil end

    local out = {
        role = msg.role,
        content = msg.content or "",
    }

    if msg.name then out.name = msg.name end
    if msg.tool_name then out.name = msg.tool_name end
    if msg.tool_call_id then out.tool_call_id = msg.tool_call_id end
    if msg.tool_calls then out.tool_calls = msg.tool_calls end

    return out
end

bone.register_command("history", {
    description = "Pick a recent conversation and load it as the current chat.",
    handler = function(_, ctx)
        -- Pull a wide window: most rows are empty placeholders, so we
        -- over-fetch and filter down to the conversations actually worth showing.
        local ok, sessions = pcall(ctx.session.list, { limit = 100 })
        if not ok then
            ctx.ui.notify("Failed to list history: " .. tostring(sessions), "error")
            return nil
        end

        if not sessions or #sessions == 0 then
            ctx.ui.notify("No conversation history found.", "warn")
            return nil
        end

        local options = {}
        local by_label = {}
        for _, session in ipairs(sessions) do
            local msg_ok, sample = pcall(ctx.session.messages, session.id, { limit = 20 })
            if msg_ok and is_relevant(sample) then
                local label = label_for(session, first_user_preview(sample))
                options[#options + 1] = label
                by_label[label] = session
            end
        end

        if #options == 0 then
            ctx.ui.notify("No conversations with messages found.", "warn")
            return nil
        end

        local ask_ok, result = pcall(ctx.ui.interact, {
            question = "Load conversation history. Use arrows/PageUp/PageDown, Enter to load, Esc to cancel.",
            type = "single_select",
            options = options,
            default = 1,
            allow_custom = false,
        })

        pcall(ctx.ui.pane, { source = "interact", title = "", lines = {} })

        if not ask_ok then
            ctx.ui.notify("History picker failed: " .. tostring(result), "error")
            return nil
        end
        if type(result) ~= "table" then
            ctx.ui.notify("History picker unavailable: " .. tostring(result), "error")
            return nil
        end
        if result.cancelled then
            ctx.ui.notify("History selection cancelled.", "info")
            return nil
        end

        local selected = by_label[result.value]
        if not selected then
            ctx.ui.notify("No history item selected.", "warn")
            return nil
        end

        local msgs_ok, stored = pcall(ctx.session.messages, selected.id, { limit = 1000 })
        if not msgs_ok then
            ctx.ui.notify("Failed to load conversation: " .. tostring(stored), "error")
            return nil
        end

        local messages = {}
        for _, msg in ipairs(stored or {}) do
            if not is_compaction(msg.content) then
                local out = valid_message(msg)
                if out then table.insert(messages, out) end
            end
        end

        if #messages == 0 then
            ctx.ui.notify("Selected conversation has no valid messages.", "warn")
            return nil
        end

        return {
            action = "conversation.load",
            messages = messages,
            conversation_id = selected.id,
            display = "Loaded conversation #" .. tostring(selected.id) .. " from history.",
            submit = false,
        }
    end,
})
