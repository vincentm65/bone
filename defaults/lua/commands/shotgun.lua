-- /shotgun — multi-model review: research, parallel blast, judge.

-- All configuration lives here. Edit these values to customize behavior.
local CONFIG = {
  research_provider = "local",
  judge_provider = "codex",
  blast_providers = '["codex","deepseek","glm_plan","minimax","openrouter","local"]',
  blast_prompt = "Review the following data and provide your analysis:\n\n{task}\n\n{research}",
  judge_prompt = "Synthesize the following analyses into a single coherent response:\n\n{task}\n\n{analyses}",

  max_result_chars = 50000,
}

local STATUS_ID = "shotgun"

-- Phase feedback in the status bar. `ctx.ui.status` only writes to stderr (hidden
-- in the TUI), so use the statusline view diff, which the live-command driver
-- drains and renders while the command runs.
local function set_status(text)
  if bone and bone.api and bone.api.ui and bone.api.ui.set_statusline then
    bone.api.ui.set_statusline(STATUS_ID, {
      { text = "shotgun: " .. text, fg = "cyan", align = "right" },
    })
  end
end

local function clear_status()
  if bone and bone.api and bone.api.ui and bone.api.ui.close then
    bone.api.ui.close(STATUS_ID)
  end
end

local function trim(s)
  return tostring(s or ""):gsub("^%s+", ""):gsub("%s+$", "")
end

local function truncate(s, max)
  s = tostring(s or "")
  if #s <= max then return s end
  return s:sub(1, max) .. "\n... (truncated)"
end

local function provider_map(ctx)
  local map = {}
  if not (ctx.config and ctx.config.list_providers) then return map end
  for _, provider in ipairs(ctx.config.list_providers() or {}) do
    map[provider.id] = provider
  end
  return map
end

local function decode_blast_providers(raw)
  local ok, decoded = pcall(cjson.decode, tostring(raw or CONFIG.blast_providers))
  if not ok or type(decoded) ~= "table" then
    return nil, "blast_providers must be a JSON array"
  end
  local entries = {}
  for _, item in ipairs(decoded) do
    if type(item) == "string" then
      entries[#entries + 1] = { provider = item }
    elseif type(item) == "table" and item.provider and item.provider ~= "" then
      entries[#entries + 1] = { provider = item.provider, model = item.model }
    end
  end
  if #entries == 0 then
    return nil, "blast_providers has no usable entries"
  end
  return entries, nil
end

local function apply_template(template, vars)
  local out = tostring(template or "")
  for key, value in pairs(vars) do
    out = out:gsub("{" .. key .. "}", tostring(value or ""))
  end
  return out
end

local function agent_opts(provider, model, label)
  local opts = { timeout_ms = 300000 }
  if provider and provider ~= "" then opts.provider = provider end
  if model and model ~= "" then opts.model = model end
  if label then opts.agent = label end
  return opts
end

local function run_research(ctx, task)
  return ctx.agent.run(
    "Research the codebase and gather relevant context for this task. Read files, check git diff, examine imports and structure. Summarize what you find.\n\nTask: " .. task,
    agent_opts(CONFIG.research_provider, nil, nil)
  )
end

local function spawn_blast(ctx, task, research, entries, providers)
  local template = CONFIG.blast_prompt
  local ids = {}
  local labels = {}
  local errors = {}

  for i, entry in ipairs(entries) do
    if not providers[entry.provider] then
      errors[#errors + 1] = "missing provider: " .. tostring(entry.provider)
    else
      local prompt = apply_template(template, {
        task = task,
        research = research,
      })
      local label = "shotgun-" .. tostring(os.time()) .. "-" .. tostring(i)
      local spawned = ctx.agent.spawn(prompt, agent_opts(entry.provider, entry.model, label))
      if spawned and spawned.ok then
        ids[#ids + 1] = spawned.id
        labels[spawned.id] = entry.provider .. (entry.model and ("/" .. entry.model) or "")
      else
        errors[#errors + 1] = tostring(entry.provider) .. ": " .. tostring(spawned and spawned.error or "spawn failed")
      end
    end
  end

  return ids, labels, errors
end

local function collect_results(waited, labels)
  local analyses = {}
  local errors = {}
  if waited and type(waited.jobs) == "table" then
    for _, job in ipairs(waited.jobs) do
      local label = labels[job.id] or job.agent or job.id
      if job.status == "done" then
        analyses[#analyses + 1] = "## " .. label .. "\n" .. truncate(job.result or "", CONFIG.max_result_chars)
      else
        errors[#errors + 1] = label .. ": " .. tostring(job.result or "error")
      end
    end
  end
  if waited and type(waited.pending) == "table" then
    for _, id in ipairs(waited.pending) do
      errors[#errors + 1] = (labels[id] or id) .. ": timed out"
    end
  end
  return analyses, errors
end

local function judge(ctx, task, research, analyses, errors)
  local text = table.concat(analyses, "\n\n")
  if #errors > 0 then
    text = text .. "\n\n## Blast errors\n- " .. table.concat(errors, "\n- ")
  end
  if trim(text) == "" then
    text = "No blast model completed successfully. Research:\n\n" .. research
  end

  local prompt = apply_template(CONFIG.judge_prompt, {
    task = task,
    analyses = text,
  })
  return ctx.agent.run(prompt, agent_opts(CONFIG.judge_provider, nil, nil))
end

bone.register_command("shotgun", {
  description = "Run a research/blast/judge multi-model AI review",
  handler = function(arg, ctx)
    local task = trim(arg)
    if task == "" then
      task = "Review the current working tree and provide findings."
    end

    local entries, parse_error = decode_blast_providers(CONFIG.blast_providers)
    if not entries then
      return { display = "shotgun: " .. parse_error, submit = false }
    end

    local providers = provider_map(ctx)

    set_status("researching…")
    local research = run_research(ctx, task)
    local research_text
    if research and research.ok then
      research_text = research.content or ""
    else
      local err = research and research.error or "research failed"
      ctx.ui.notify("shotgun research failed: " .. tostring(err), "warn")
      research_text = "Research failed: " .. tostring(err)
    end

    set_status("blasting " .. tostring(#entries) .. " models…")
    local ids, labels, errors = spawn_blast(ctx, task, research_text, entries, providers)
    local waited = nil
    if #ids > 0 then
      waited = ctx.agent.wait(ids, { timeout_ms = 300000 })
      if waited and waited.cancelled then
        clear_status()
        return { display = "shotgun: cancelled", submit = false }
      end
    end

    local analyses, wait_errors = collect_results(waited, labels)
    for _, err in ipairs(wait_errors) do errors[#errors + 1] = err end

    set_status("judging…")
    local judged = judge(ctx, task, research_text, analyses, errors)
    clear_status()
    if not judged or not judged.ok then
      return {
        display = "shotgun judge failed: " .. tostring(judged and judged.error or "unknown"),
        submit = false,
      }
    end

    return { display = judged.content or "", submit = false }
  end,
})
