local function comma(n)
  n = math.floor(tonumber(n) or 0)
  local s = tostring(n)
  local sign = ""
  if s:sub(1, 1) == "-" then
    sign = "-"
    s = s:sub(2)
  end
  local out = s
  while true do
    local next_out, changed = out:gsub("^(-?%d+)(%d%d%d)", "%1,%2")
    out = next_out
    if changed == 0 then break end
  end
  return sign .. out
end

local function tokens(n)
  return comma(n)
end

local function money(n)
  n = tonumber(n) or 0
  if n <= 0 then return nil end
  return string.format("$%.4f", n)
end

local DIM   = "\x1b[2m"
local CYAN  = "\x1b[36m"
local WHITE = "\x1b[37m"
local RESET = "\x1b[0m"

local function header(title)
  return string.format("%s%s%s", CYAN, title, RESET)
end

local function section(title)
  return string.format("%s%s%s", CYAN, title, RESET)
end

local function klabel(label)
  return string.format("%s%-12s%s", DIM, label .. ":", RESET)
end

local function kvalue(v)
  return string.format("%s%s%s", WHITE, v, RESET)
end

local function kdim(v)
  return string.format("%s%s%s", DIM, v, RESET)
end

local function sep()
  return string.format("%s%s%s", DIM, string.rep("─", 52), RESET)
end

bone.register_command("usage", {
  description = "Show token usage for current conversation",
  handler = function(_, ctx)
    local usage = ctx.usage and ctx.usage.snapshot and ctx.usage.snapshot() or nil
    if not usage then
      return { display = "Usage data is unavailable in this context.", submit = false }
    end

    local total = (usage.sent or 0) + (usage.received or 0)
    local lines = {
      header("Conversation usage"),
      sep(),
      klabel("Requests") .. kvalue(comma(usage.request_count)),
      klabel("Tokens")   .. kvalue(tokens(total) .. " total"),
      klabel("Input")    .. kvalue(tokens(usage.sent)),
      klabel("Output")   .. kvalue(tokens(usage.received)),
      klabel("Context")  .. kvalue(tokens(usage.context_length) .. " current"),
    }

    if (usage.cached or 0) > 0 then
      table.insert(lines, klabel("Cached") .. kvalue(tokens(usage.cached)))
    end
    local cost = money(usage.cost)
    if cost then
      table.insert(lines, klabel("Cost") .. kvalue(cost))
    end
    if (usage.request_count or 0) > 0 then
      table.insert(lines, klabel("Avg/req") .. kvalue(tokens((usage.sent or 0) / usage.request_count) .. " in / " .. tokens((usage.received or 0) / usage.request_count) .. " out"))
    end

    table.insert(lines, "")
    table.insert(lines, section("Prompt overhead"))
    table.insert(lines, sep())
    table.insert(lines, klabel("Tools") .. kvalue(comma(usage.tool_count) .. " tools, ~" .. tokens(usage.tool_schema_tokens) .. " tokens (" .. kdim(comma(usage.tool_schema_chars) .. " chars)") .. ")"))
    table.insert(lines, klabel("System") .. kvalue("~" .. tokens(usage.system_prompt_tokens) .. " tokens (" .. kdim(comma(usage.system_prompt_chars) .. " chars)") .. ")"))

    if usage.by_provider and #usage.by_provider > 1 then
      table.insert(lines, "")
      table.insert(lines, section("By provider/model"))
      table.insert(lines, sep())
      for _, p in ipairs(usage.by_provider) do
        local row = string.format(
          "  %s / %s — %s in / %s out",
          kdim(p.provider or "unknown"),
          kvalue(p.model or "unknown"),
          tokens(p.prompt_tokens),
          tokens(p.completion_tokens)
        )
        if (p.cached_tokens or 0) > 0 then
          row = row .. " / " .. tokens(p.cached_tokens) .. " cached"
        end
        local provider_cost = money(p.cost)
        if provider_cost then
          row = row .. " / " .. kvalue(provider_cost)
        end
        table.insert(lines, row)
      end
    end

    return { display = table.concat(lines, "\n"), submit = false }
  end,
})
