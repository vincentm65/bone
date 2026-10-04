-- Cron management stays a normal Lua tool.  Execution is intentionally
-- configurable because a headless Bone 3 process needs a user-provided
-- launcher command (BONE3_CRON_COMMAND) rather than an interactive TUI.
local function clean(value, label, max)
  if type(value) ~= "string" or value == "" or #value > max or value:find("[%z\1-\31\127]") then
    error(label .. " is invalid", 0)
  end
  return value
end
local function quote(value)
  return "'" .. tostring(value):gsub("'", "'\"'\"'") .. "'"
end
local function run(command, opts)
  local r = bone.system(command, opts or {})
  if not r then return nil, "cancelled" end
  return r
end
local function crontab()
  local r = run("crontab -l", { timeout = 10000 })
  if r and r.code == 0 then return r.stdout end
  return ""
end
local function write_tab(text)
  local r = run("crontab -", { stdin = text, timeout = 10000 })
  if not r or r.code ~= 0 then return nil, (r and r.stderr) or "crontab update failed" end
  return true
end

bone.tool.register({
  name = "cron",
  description = "Add, list, remove, or inspect recurring Bone 3 cron jobs.",
  parameters = {
    type = "object",
    properties = {
      action = { type = "string", enum = { "add", "list", "remove", "logs" } },
      name = { type = "string" }, time = { type = "string", description = "daily HH:MM" },
      prompt = { type = "string" }, tail = { type = "integer", minimum = 1, maximum = 1000 },
    },
    required = { "action" },
  },
  needs_approval = true,
  run = function(args)
    local action = args.action
    if action == "list" then
      local text = crontab()
      local rows = {}
      for line in text:gmatch("[^\n]+") do
        if line:find("# BONE3:", 1, true) then rows[#rows + 1] = line end
      end
      return #rows > 0 and table.concat(rows, "\n") or "No Bone 3 cron jobs."
    end
    local name = clean(args.name or "", "name", 80)
    local marker = "# BONE3:" .. name
    local text = crontab()
    if action == "remove" then
      local out = {}
      for line in text:gmatch("[^\n]*\n?") do
        if line ~= "" and not line:find(marker, 1, true) then out[#out + 1] = line end
      end
      local ok, err = write_tab(table.concat(out))
      if ok then return "Removed " .. name end
      return nil, err
    elseif action == "logs" then
      local tail = math.max(1, math.min(1000, tonumber(args.tail) or 100))
      local path = bone.config_dir .. "/cron/" .. name .. ".log"
      local r = run("tail -n " .. tail .. " -- " .. quote(path), { timeout = 10000 })
      return r and r.code == 0 and r.stdout or "No log for " .. name
    elseif action == "add" then
      local time = clean(args.time or "", "time", 5)
      if not time:match("^%d%d:%d%d$") then error("time must be HH:MM", 0) end
      local hour, minute = tonumber(time:sub(1, 2)), tonumber(time:sub(4, 5))
      if hour > 23 or minute > 59 then error("time must be a valid 24-hour time", 0) end
      local prompt = clean(args.prompt or "", "prompt", 16384)
      local launcher = os.getenv("BONE3_CRON_COMMAND") or "bone3 --headless"
      local line = string.format("%s %s * * * %s %s >> %s 2>&1 %s\n",
        time:sub(4), time:sub(1, 2), launcher, quote(prompt),
        quote(bone.config_dir .. "/cron/" .. name .. ".log"), marker)
      local out = {}
      for existing in text:gmatch("[^\n]*\n?") do
        if existing ~= "" and not existing:find(marker, 1, true) then out[#out + 1] = existing end
      end
      out[#out + 1] = line
      local ok, err = write_tab(table.concat(out))
      if ok then return "Scheduled " .. name .. " at " .. time end
      return nil, err
    end
    error("action must be add, list, remove, or logs", 0)
  end,
})
