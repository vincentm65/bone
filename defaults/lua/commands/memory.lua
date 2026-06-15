-- /memory — incremental memory builder.
--
-- Processes conversations since the last run and feeds the result into an
-- updated memory.md. Earlier versions concatenated every conversation's full
-- transcript into a single prompt, which overflowed the model context window on
-- large histories. Instead, each conversation (or chunk of a very large one) is
-- distilled in an *isolated* sub-agent run (ctx.agent.run), and only the small
-- accumulated findings flow back into the final memory-update prompt.

-- Char budget per extraction sub-agent call (~4 chars/token, so ~20k tokens).
-- Small conversations are packed together up to this budget; a single
-- conversation larger than this is split into chunks.
local EXTRACT_BUDGET_CHARS = 80000

-- Hard cap on a single message's length. Prior /memory runs (and pasted files)
-- can store enormous individual messages in the DB; without this cap one giant
-- message becomes one giant extraction call and overflows the context window.
-- Preferences live near the start of a message, so a head truncation is fine.
local MAX_MSG_CHARS = 4000

--- Truncate a string to at most max_bytes without splitting a UTF-8 sequence
--- (mirrors compact.lua's truncate_utf8).
local function truncate_utf8(s, max_bytes)
  if #s <= max_bytes then
    return s
  end
  for cut = max_bytes, math.max(max_bytes - 4, 1), -1 do
    local chunk = s:sub(1, cut)
    local ok, len = pcall(utf8.len, chunk)
    if ok and len then
      return chunk .. "..."
    end
  end
  return "..."
end

--- Load the user+assistant transcript for one conversation as an array of
--- "[role] content" lines (skips tool messages). Each message is truncated so
--- no single line can exceed the per-call budget.
local function conversation_lines(ctx, cid)
  local msg_ok, msg_rows = pcall(ctx.db.query,
    "SELECT role, content FROM messages WHERE conversation_id = ? "
      .. "AND role IN ('user', 'assistant') AND tool_name IS NULL ORDER BY seq ASC",
    { cid })
  if not msg_ok or type(msg_rows) ~= "table" then
    return nil
  end
  local lines = {}
  for _, msg in ipairs(msg_rows) do
    local content = truncate_utf8(msg.content or "", MAX_MSG_CHARS)
    lines[#lines + 1] = "[" .. msg.role .. "] " .. content
  end
  return lines
end

--- Build the extraction prompt for one batch of transcript text.
local function extraction_prompt(transcript)
  return table.concat({
    "You are distilling durable user preferences from prior conversation transcripts.",
    "",
    "From the transcript below, extract ONLY durable, general signals about how the",
    "user likes to work: communication/verbosity preferences, coding style, preferred",
    "tools/workflows, and things they consistently dislike or avoid.",
    "",
    "Rules:",
    "- Output terse bullet points, one signal per line, prefixed with '- '.",
    "- Ignore one-off task details, project-specific context, and incidental remarks.",
    "- If there is nothing durable worth remembering, output exactly: NONE",
    "",
    "--- Transcript ---",
    transcript,
  }, "\n")
end

--- Run one extraction sub-agent call over a transcript batch. Returns the
--- distilled bullet text, or nil when there is nothing (or on failure).
local function extract(ctx, transcript)
  local run_result = ctx.agent.run(extraction_prompt(transcript), { timeout_ms = 120000 })
  if not run_result.ok then
    ctx.log.warn("memory: extraction failed: " .. (run_result.error or "unknown"))
    return nil
  end
  local content = (run_result.content or ""):gsub("^%s+", ""):gsub("%s+$", "")
  if content == "" or content:upper() == "NONE" then
    return nil
  end
  return content
end

bone.register_command("memory", {
  description = "Incremental memory builder. Processes all conversations since last run and updates memory.md.",
  handler = function(_, ctx)
    local bone_dir = ctx.config_dir
    local state_file = bone_dir .. "/memory.last_run"

    -- Read last run timestamp or default to epoch.
    local since = "1970-01-01T00:00:00Z"
    local state_stat = ctx.fs.stat(state_file)
    if state_stat and state_stat.kind == "file" then
      local ok, state_content = pcall(ctx.read_file, state_file)
      if ok and state_content then
        since = state_content:gsub("%s+", "")
      end
    end

    local now_utc = os.date("!%Y-%m-%dT%H:%M:%SZ")

    -- Conversation IDs since last run, oldest first.
    local cids_ok, cids_rows = pcall(ctx.db.query,
      "SELECT id FROM conversations WHERE started_at > ? ORDER BY started_at ASC",
      { since })

    if not cids_ok or type(cids_rows) ~= "table" then
      return {
        display = "Error querying conversations: " .. tostring(cids_rows),
        submit = false,
      }
    end

    local total = #cids_rows
    if total == 0 then
      return {
        display = "No new conversations since " .. since .. ".",
        submit = false,
      }
    end

    -- Distill each conversation in isolation. Pack small conversations into a
    -- shared batch up to EXTRACT_BUDGET_CHARS; split oversized conversations
    -- into chunks. Only the small bullet outputs are accumulated.
    local findings = {}
    local pending = {}        -- accumulated lines awaiting a flush
    local pending_chars = 0

    local function flush_pending()
      if pending_chars == 0 then return end
      local distilled = extract(ctx, table.concat(pending, "\n"))
      if distilled then
        findings[#findings + 1] = distilled
      end
      pending = {}
      pending_chars = 0
    end

    for _, row in ipairs(cids_rows) do
      local cid = row.id
      local lines = conversation_lines(ctx, cid)
      if lines then
        local header = "## Conversation " .. tostring(cid)
        local body = header .. "\n" .. table.concat(lines, "\n")

        if #body > EXTRACT_BUDGET_CHARS then
          -- Oversized single conversation: flush any packed batch first, then
          -- split this conversation's lines into budget-sized chunks.
          flush_pending()
          local chunk = { header }
          local chunk_chars = #header
          for _, line in ipairs(lines) do
            if chunk_chars + #line + 1 > EXTRACT_BUDGET_CHARS and #chunk > 1 then
              local distilled = extract(ctx, table.concat(chunk, "\n"))
              if distilled then
                findings[#findings + 1] = distilled
              end
              chunk = { header }
              chunk_chars = #header
            end
            chunk[#chunk + 1] = line
            chunk_chars = chunk_chars + #line + 1
          end
          if #chunk > 1 then
            local distilled = extract(ctx, table.concat(chunk, "\n"))
            if distilled then
              findings[#findings + 1] = distilled
            end
          end
        else
          -- Pack into the current batch, flushing first if it would overflow.
          if pending_chars + #body + 1 > EXTRACT_BUDGET_CHARS then
            flush_pending()
          end
          pending[#pending + 1] = body
          pending_chars = pending_chars + #body + 1
        end
      end
    end

    flush_pending()

    -- Nothing durable surfaced: still advance the checkpoint so the same
    -- conversations are not re-scanned next time.
    if #findings == 0 then
      local ok = pcall(ctx.write_file, state_file, now_utc .. "\n")
      if not ok then
        -- write_file refuses to overwrite; fall back to the agent path below.
        return [=[You are a memory builder. No durable preferences were found in the conversations processed this run.

## Your task
Write the value ]=] .. now_utc .. [=[ to `$HOME/.bone-rust/memory.last_run` (overwriting it) to advance the checkpoint, then say "No changes."]=]
      end
      return {
        display = string.format(
          "Processed %d conversation(s) since %s. No durable preferences found. Checkpoint advanced.",
          total, since),
        submit = false,
      }
    end

    -- Feed only the small distilled findings into the final memory update. This
    -- prompt is tiny, so it cannot overflow the context window.
    local findings_text = table.concat(findings, "\n\n")

    return [=[You are a memory builder finalizing an incremental run. The conversations since the last run have already been distilled (in isolation) into the findings below.

## Context
Conversations processed since ]=] .. since .. ": " .. total .. [=[
NEXT_RUN=]=] .. now_utc .. [=[

## Distilled findings
]=] .. findings_text .. [=[

## Your task
1. Read the current memory.md from the bone config directory. If it doesn't exist, start fresh.
2. Merge the distilled findings above into memory.md.
3. Write the updated memory.md using write_file or edit_file.
4. After updating memory.md (or deciding no changes are needed), write the value of NEXT_RUN (shown above) to `$HOME/.bone-rust/memory.last_run`. This advances the checkpoint so processed conversations aren't re-processed. Only do this last.

## Rules
- Only add preferences clearly demonstrated (seen 2+ times), not one-off remarks.
- Remove anything contradicted by newer findings.
- Keep the file under 400 tokens. Merge, compress, and drop lower-priority items to fit. Prefer short bullet points over prose. When the file exceeds this limit, consolidate by merging similar items and dropping the least important entries until it fits.
- Start the file with a metadata line: <!-- last_updated: YYYY-MM-DD -->
- Use these markdown sections (drop empty ones, add relevant ones):
  - Communication — how the user likes to communicate, verbosity preferences, response format preferences
  - Coding Style — language preferences, patterns, naming conventions, architecture tastes
  - Tools & Workflow — preferred tools, workflows, development habits
  - Dislikes — things the user consistently avoids or objects to
- Do NOT include project-specific context, task details, or one-off requirements. This file captures general preferences and habits, not what you are working on right now.
- If no meaningful changes are needed, leave memory.md as-is and say "No changes."
- Output a brief summary of what you added, changed, or removed (or "No changes.").]=]
  end,
})
