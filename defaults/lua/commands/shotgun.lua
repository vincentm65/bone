-- /shotgun — fan a prompt out to several provider sub-agents, then let the
-- primary chat agent synthesize their answers into one final reply.
--
-- Configure the fan-out targets in your init.lua. Each entry is either a
-- provider id (uses that provider's default model) or a table pinning a model:
--
--   bone.config.shotgun = {
--     "deepseek",
--     "minimax",
--     { provider = "openrouter", model = "..." },
--   }
--
-- Usage: /shotgun <prompt>
--   The prompt is sent to every configured provider as an independent
--   sub-agent (visible in the normal subagent pane). When they finish, their
--   answers are handed back to the current chat model, which reviews them and
--   writes the final synthesized answer as its normal turn.

local CONFIG = {
  timeout_ms = 300000,
  max_result_chars = 50000,
  reviewer_guide = table.concat({
    "You are one of several models independently answering the prompt below.",
    "Give your own direct, well-reasoned answer. State key assumptions and call out anything you are unsure about.",
    "Do not assume what other models will say; this is your independent take.",
  }, "\n"),
  synthesis_guide = table.concat({
    "Below are independent answers from several models to my prompt.",
    "Synthesize a single best answer for me:",
    "- Note where the models agree (higher confidence) and where they diverge.",
    "- Flag anything only one model caught and weigh it on its merits.",
    "- End with a clear recommendation or final answer.",
    "Summarize in your own words; do not just paste the model answers back.",
  }, "\n"),
}

local function trim(s)
  return tostring(s or ""):gsub("^%s+", ""):gsub("%s+$", "")
end

local function truncate(s, max)
  s = tostring(s or "")
  if #s <= max then return s end
  return s:sub(1, max) .. "\n... (truncated)"
end

local function shotgun_targets()
  if not (bone and bone.config and type(bone.config.shotgun) == "table") then
    return {}
  end
  local entries = {}
  for _, item in ipairs(bone.config.shotgun) do
    if type(item) == "string" and item ~= "" then
      entries[#entries + 1] = { provider = item }
    elseif type(item) == "table" and item.provider and item.provider ~= "" then
      entries[#entries + 1] = { provider = item.provider, model = item.model }
    end
  end
  return entries
end

local function provider_map(ctx)
  local map = {}
  if not (ctx.config and ctx.config.list_providers) then return map end
  for _, provider in ipairs(ctx.config.list_providers() or {}) do
    map[provider.id] = provider
  end
  return map
end

local function model_for(entry, providers)
  if entry.model and entry.model ~= "" then return entry.model end
  local provider = providers[entry.provider]
  return provider and provider.model or nil
end

local function describe_entry(entry, providers)
  local model = model_for(entry, providers)
  if model and model ~= "" then
    return tostring(entry.provider) .. "/" .. tostring(model)
  end
  return tostring(entry.provider)
end

local function agent_opts(provider, model, label)
  local opts = { timeout_ms = CONFIG.timeout_ms }
  if provider and provider ~= "" then opts.provider = provider end
  if model and model ~= "" then opts.model = model end
  if label then opts.agent = label end
  return opts
end

local function reviewer_prompt(task)
  return CONFIG.reviewer_guide .. "\n\n## Prompt\n" .. task
end

local function synthesis_prompt(task, analyses, errors)
  local parts = { CONFIG.synthesis_guide, "", "## My prompt", task }
  for _, item in ipairs(analyses) do
    parts[#parts + 1] = ""
    parts[#parts + 1] = "## " .. item.label
    parts[#parts + 1] = item.text
  end
  if #errors > 0 then
    parts[#parts + 1] = ""
    parts[#parts + 1] = "## Reviewers that did not respond"
    for _, err in ipairs(errors) do
      parts[#parts + 1] = "- " .. err
    end
  end
  return table.concat(parts, "\n")
end

-- After cancellation/timeout, drain the still-pending jobs so their late
-- results don't auto-inject as a stray chat turn.
local function drain_pending(ctx, pending)
  if not (pending and #pending > 0) then return end
  for _, id in ipairs(pending) do
    ctx.agent.cancel(id)
  end
  ctx.agent.wait(pending, { timeout_ms = 10000 })
end

bone.register_command("shotgun", {
  description = "Fan a prompt out to configured providers; the chat agent synthesizes the answers",
  handler = function(arg, ctx)
    local task = trim(arg)
    if task == "" then
      return { display = "shotgun: provide a prompt, e.g. /shotgun should we cache this?", submit = false }
    end

    local entries = shotgun_targets()
    if #entries == 0 then
      return {
        display = table.concat({
          "shotgun: no targets configured. Add this to your init.lua:",
          "",
          "  bone.config.shotgun = { \"deepseek\", \"minimax\" }",
          "",
          "Each entry is a provider id, or { provider = \"...\", model = \"...\" }.",
        }, "\n"),
        submit = false,
      }
    end

    local providers = provider_map(ctx)
    local prompt = reviewer_prompt(task)
    local ids = {}
    local labels = {}
    local errors = {}

    for i, entry in ipairs(entries) do
      if not providers[entry.provider] then
        errors[#errors + 1] = tostring(entry.provider) .. ": unknown provider"
      else
        local label = describe_entry(entry, providers)
        local spawned = ctx.agent.spawn(prompt, agent_opts(entry.provider, entry.model, "shotgun " .. label .. " #" .. i))
        if spawned and spawned.ok then
          ids[#ids + 1] = spawned.id
          labels[spawned.id] = label
        else
          errors[#errors + 1] = label .. ": " .. tostring(spawned and spawned.error or "spawn failed")
        end
      end
    end

    if #ids == 0 then
      return { display = "shotgun: no reviewers started.\n- " .. table.concat(errors, "\n- "), submit = false }
    end

    local waited = ctx.agent.wait(ids, { timeout_ms = CONFIG.timeout_ms })
    if waited and waited.cancelled then
      return { display = "shotgun: cancelled", submit = false }
    end

    local analyses = {}
    if waited and type(waited.jobs) == "table" then
      for _, job in ipairs(waited.jobs) do
        local label = labels[job.id] or job.agent or job.id
        if job.status == "done" then
          analyses[#analyses + 1] = { label = label, text = truncate(job.result or "", CONFIG.max_result_chars) }
        else
          errors[#errors + 1] = label .. ": " .. tostring(job.result or "error")
        end
      end
    end
    if waited and type(waited.pending) == "table" then
      for _, id in ipairs(waited.pending) do
        errors[#errors + 1] = (labels[id] or id) .. ": timed out"
      end
      drain_pending(ctx, waited.pending)
    end

    if #analyses == 0 then
      return { display = "shotgun: no reviewer responded.\n- " .. table.concat(errors, "\n- "), submit = false }
    end

    return { content = synthesis_prompt(task, analyses, errors), submit = true }
  end,
})
