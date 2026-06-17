-- /shotgun — multi-model review: search, parallel blast, judge.

-- All configuration lives here. Edit these values to customize behavior.
local CONFIG = {
  search_provider = "",
  judge_provider = "",
  blast_providers = '[{"provider":"openai"},{"provider":"openai","model":"gpt-4o"}]',
  search_prompt = "Search for relevant code, structure, and context. Read key files, check git diff, examine imports, and summarize what you find.",
  blast_prompt = "Review the following data and provide your analysis:\n\n{task}\n\n{search_results}",
  judge_prompt = "Synthesize the following analyses into a single coherent response:\n\n{task}\n\n{analyses}",

  max_context_chars = 90000,
  max_section_chars = 30000,
  max_result_chars = 50000,
}

local function trim(s)
  return tostring(s or ""):gsub("^%s+", ""):gsub("%s+$", "")
end

local function truncate(s, max)
  s = tostring(s or "")
  if #s <= max then return s end
  return s:sub(1, max) .. "\n... (truncated)"
end



local function status(ctx, msg)
  if ctx.ui and ctx.ui.status then ctx.ui.status("shotgun: " .. msg) end
end

local function shell_text(ctx, title, cmd, max_chars)
  local ok, result = pcall(ctx.shell, cmd, { timeout_ms = 120000 })
  if not ok then
    return "## " .. title .. "\nERROR: " .. tostring(result)
  end
  local out = result.stdout or ""
  local err = result.stderr or ""
  local body = trim(out)
  if result.exit_code ~= 0 then
    body = body .. "\n(exit " .. tostring(result.exit_code) .. ")\n" .. trim(err)
  end
  if body == "" then body = "(empty)" end
  return "## " .. title .. "\n" .. truncate(body, max_chars or CONFIG.max_section_chars)
end

local function read_if_exists(ctx, path, max_chars)
  if not (ctx.fs and ctx.fs.is_file and ctx.fs.is_file(path)) then return nil end
  local ok, content = pcall(ctx.read_file, path)
  if not ok then return "## " .. path .. "\nERROR: " .. tostring(content) end
  return "## " .. path .. "\n" .. truncate(content, max_chars or 12000)
end

local function gather_context(ctx)
  local parts = {
    shell_text(ctx, "git status", "git status --short --branch", 12000),
    shell_text(ctx, "git diff", "git diff --no-color", 40000),
    shell_text(ctx, "tracked files", "git ls-files | sed -n '1,300p'", 20000),
  }

  local files = {
    "AGENTS.md",
    "README.md",
    "Cargo.toml",
    "package.json",
    "src/main.rs",
    "src/lib.rs",
  }
  for _, path in ipairs(files) do
    local section = read_if_exists(ctx, path, 12000)
    if section then parts[#parts + 1] = section end
  end

  return truncate(table.concat(parts, "\n\n"), CONFIG.max_context_chars)
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

local function run_search(ctx, task, context)
  local prompt = table.concat({
    CONFIG.search_prompt,
    "",
    "Task:",
    task,
    "",
    "Context:",
    context,
  }, "\n")
  return ctx.agent.run(prompt, agent_opts(CONFIG.search_provider, nil, nil))
end

local function spawn_blast(ctx, task, search_results, entries, providers)
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
        search_results = search_results,
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
        analyses[#analyses + 1] = "## " .. label .. "\n" .. truncate(job.result or "", MAX_RESULT_CHARS)
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

local function judge(ctx, task, search_results, analyses, errors)
  local text = table.concat(analyses, "\n\n")
  if #errors > 0 then
    text = text .. "\n\n## Blast errors\n- " .. table.concat(errors, "\n- ")
  end
  if trim(text) == "" then
    text = "No blast model completed successfully. Search results:\n\n" .. search_results
  end

  local prompt = apply_template(CONFIG.judge_prompt, {
    task = task,
    analyses = text,
    search_results = search_results,
  })
  return ctx.agent.run(prompt, agent_opts(CONFIG.judge_provider, nil, nil))
end

bone.register_command("shotgun", {
  description = "Run a search/blast/judge multi-model AI review",
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

    status(ctx, "gathering context")
    local context = gather_context(ctx)

    status(ctx, "searching")
    local search = run_search(ctx, task, context)
    local search_results
    if search and search.ok then
      search_results = search.content or ""
    else
      local err = search and search.error or "search failed"
      ctx.ui.notify("shotgun search failed: " .. tostring(err), "warn")
      search_results = "Search failed: " .. tostring(err) .. "\n\nRaw context:\n" .. context
    end

    status(ctx, "blasting to " .. tostring(#entries) .. " models")
    local ids, labels, errors = spawn_blast(ctx, task, search_results, entries, providers)
    local waited = nil
    if #ids > 0 then
      waited = ctx.agent.wait(ids, { timeout_ms = 300000 })
      if waited and waited.cancelled then
        return { display = "shotgun: cancelled", submit = false }
      end
    end

    local analyses, wait_errors = collect_results(waited, labels)
    for _, err in ipairs(wait_errors) do errors[#errors + 1] = err end

    status(ctx, "judging")
    local judged = judge(ctx, task, search_results, analyses, errors)
    if not judged or not judged.ok then
      return {
        display = "shotgun judge failed: " .. tostring(judged and judged.error or "unknown"),
        submit = false,
      }
    end

    status(ctx, "done")
    return { display = judged.content or "", submit = true }
  end,
})
