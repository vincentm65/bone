bone.register_command("memory", {
  description = "Incremental memory builder. Processes all conversations since last run and updates memory.md.",
  handler = function(args, ctx)
    local bone_dir = ctx.config_dir
    local db = bone_dir .. "/data/conversations.db"
    local state_file = bone_dir .. "/memory.last_run"

    -- Check if database exists
    if not ctx.fs.is_file(db) then
      return [=[You are a memory builder running in "dream" mode — processing conversations that happened since your last run.

## Context
No conversation database found. Nothing to process.

## Your task
1. Read the current memory.md from the bone config directory. If it doesn't exist, start fresh.
2. Review each conversation above for user preferences, patterns, and context.
3. Write an updated memory.md using write_file or edit_file.
4. After updating memory.md (or deciding no changes are needed), write the value of NEXT_RUN (shown above) to `$HOME/.bone-rust/memory.last_run`. This advances the checkpoint so processed conversations aren't re-processed. Only do this last.

## Rules
- Only add preferences clearly demonstrated (seen 2+ times across conversations), not one-off remarks.
- Remove anything contradicted by newer conversations.
- Keep the file under 400 tokens. Merge, compress, and drop lower-priority items to fit. Prefer short bullet points over prose. When the file exceeds this limit, consolidate by merging similar items and dropping the least important entries until it fits.
- Start the file with a metadata line: <!-- last_updated: YYYY-MM-DD -->
- Use these markdown sections (drop empty ones, add relevant ones):
  - Communication — how the user likes to communicate, verbosity preferences, response format preferences
  - Coding Style — language preferences, patterns, naming conventions, architecture tastes
  - Tools & Workflow — preferred tools, workflows, development habits
  - Dislikes — things the user consistently avoids or objects to
- Do NOT include project-specific context, task details, or one-off requirements. This file captures general preferences and habits, not what the user is working on right now.
- If no meaningful changes are needed, leave memory.md as-is and say "No changes."
- Output a brief summary of what you added, changed, or removed (or "No changes.").]=]
    end

    -- Read last run timestamp or default to epoch
    local since = "1970-01-01T00:00:00Z"
    if ctx.fs.is_file(state_file) then
      local ok, state_content = pcall(ctx.read_file, state_file)
      if ok and state_content then
        since = state_content:gsub("%s+", "")
      end
    end

    -- Get conversation IDs since last run
    local cids_query = string.format(
      "SELECT id FROM conversations WHERE started_at > '%s' ORDER BY started_at ASC;",
      since
    )
    local cids_result = ctx.shell("sqlite3 " .. db .. " '" .. cids_query .. "'")
    if cids_result.exit_code ~= 0 then
      return [=[You are a memory builder running in "dream" mode — processing conversations that happened since your last run.

## Context
Error querying conversations: ]=] .. cids_result.stderr .. [=[

## Your task
1. Read the current memory.md from the bone config directory. If it doesn't exist, start fresh.
2. Review each conversation above for user preferences, patterns, and context.
3. Write an updated memory.md using write_file or edit_file.
4. After updating memory.md (or deciding no changes are needed), write the value of NEXT_RUN (shown above) to `$HOME/.bone-rust/memory.last_run`. This advances the checkpoint so processed conversations aren't re-processed. Only do this last.

## Rules
- Only add preferences clearly demonstrated (seen 2+ times across conversations), not one-off remarks.
- Remove anything contradicted by newer conversations.
- Keep the file under 400 tokens. Merge, compress, and drop lower-priority items to fit. Prefer short bullet points over prose. When the file exceeds this limit, consolidate by merging similar items and dropping the least important entries until it fits.
- Start the file with a metadata line: <!-- last_updated: YYYY-MM-DD -->
- Use these markdown sections (drop empty ones, add relevant ones):
  - Communication — how the user likes to communicate, verbosity preferences, response format preferences
  - Coding Style — language preferences, patterns, naming conventions, architecture tastes
  - Tools & Workflow — preferred tools, workflows, development habits
  - Dislikes — things the user consistently avoids or objects to
- Do NOT include project-specific context, task details, or one-off requirements. This file captures general preferences and habits, not what the user is working on right now.
- If no meaningful changes are needed, leave memory.md as-is and say "No changes."
- Output a brief summary of what you added, changed, or removed (or "No changes.").]=]
    end

    local cids = cids_result.stdout:match("^%s*(.-)%s*$")
    if cids == "" then
      return [=[You are a memory builder running in "dream" mode — processing conversations that happened since your last run.

## Context
No new conversations since ]=] .. since .. [=[:.

## Your task
1. Read the current memory.md from the bone config directory. If it doesn't exist, start fresh.
2. Review each conversation above for user preferences, patterns, and context.
3. Write an updated memory.md using write_file or edit_file.
4. After updating memory.md (or deciding no changes are needed), write the value of NEXT_RUN (shown above) to `$HOME/.bone-rust/memory.last_run`. This advances the checkpoint so processed conversations aren't re-processed. Only do this last.

## Rules
- Only add preferences clearly demonstrated (seen 2+ times across conversations), not one-off remarks.
- Remove anything contradicted by newer conversations.
- Keep the file under 400 tokens. Merge, compress, and drop lower-priority items to fit. Prefer short bullet points over prose. When the file exceeds this limit, consolidate by merging similar items and dropping the least important entries until it fits.
- Start the file with a metadata line: <!-- last_updated: YYYY-MM-DD -->
- Use these markdown sections (drop empty ones, add relevant ones):
  - Communication — how the user likes to communicate, verbosity preferences, response format preferences
  - Coding Style — language preferences, patterns, naming conventions, architecture tastes
  - Tools & Workflow — preferred tools, workflows, development habits
  - Dislikes — things the user consistently avoids or objects to
- Do NOT include project-specific context, task details, or one-off requirements. This file captures general preferences and habits, not what the user is working on right now.
- If no meaningful changes are needed, leave memory.md as-is and say "No changes."
- Output a brief summary of what you added, changed, or removed (or "No changes.").]=]
    end

    -- Count conversations
    local count = 0
    for _ in cids:gmatch("[^\n]+") do
      count = count + 1
    end

    -- Get next run timestamp
    local now_result = ctx.shell("date -u +\"%Y-%m-%dT%H:%M:%SZ\"")
    local next_run = now_result.stdout:match("^%s*(.-)%s*$")
    if next_run == "" then
      next_run = "unknown"
    end

    -- Build conversation blocks
    local conv_blocks = ""
    for cid in cids:gmatch("[^\n]+") do
      cid = cid:match("^%s*(.-)%s*$")
      local msg_query = string.format(
        "SELECT '[' || m.role || '] ' || m.content FROM messages WHERE m.conversation_id = %s AND m.role IN ('user', 'assistant') AND m.tool_name IS NULL ORDER BY m.seq ASC;",
        cid
      )
      local msg_result = ctx.shell("sqlite3 " .. db .. " '" .. msg_query .. "'")
      local block = "## Conversation " .. cid .. "\n"
      if msg_result.exit_code == 0 then
        block = block .. msg_result.stdout
      else
        block = block .. "(failed to read conversation " .. cid .. ")"
      end
      block = block .. "\n"
      conv_blocks = conv_blocks .. block
    end

    return [=[You are a memory builder running in "dream" mode — processing conversations that happened since your last run.

## Context
Conversations since ]=] .. since .. ": " .. count .. [=[
---
NEXT_RUN=]=] .. next_run .. conv_blocks .. [=[
## Your task
1. Read the current memory.md from the bone config directory. If it doesn't exist, start fresh.
2. Review each conversation above for user preferences, patterns, and context.
3. Write an updated memory.md using write_file or edit_file.
4. After updating memory.md (or deciding no changes are needed), write the value of NEXT_RUN (shown above) to `$HOME/.bone-rust/memory.last_run`. This advances the checkpoint so processed conversations aren't re-processed. Only do this last.

## Rules
- Only add preferences clearly demonstrated (seen 2+ times across conversations), not one-off remarks.
- Remove anything contradicted by newer conversations.
- Keep the file under 400 tokens. Merge, compress, and drop lower-priority items to fit. Prefer short bullet points over prose. When the file exceeds this limit, consolidate by merging similar items and dropping the least important entries until it fits.
- Start the file with a metadata line: <!-- last_updated: YYYY-MM-DD -->
- Use these markdown sections (drop empty ones, add relevant ones):
  - Communication — how the user likes to communicate, verbosity preferences, response format preferences
  - Coding Style — language preferences, patterns, naming conventions, architecture tastes
  - Tools & Workflow — preferred tools, workflows, development habits
  - Dislikes — things the user consistently avoids or objects to
- Do NOT include project-specific context, task details, or one-off requirements. This file captures general preferences and habits, not what the user is working on right now.
- If no meaningful changes are needed, leave memory.md as-is and say "No changes."
- Output a brief summary of what you added, changed, or removed (or "No changes.").]=]
  end,
})
