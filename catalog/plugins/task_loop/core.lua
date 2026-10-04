local state = bone.state.load("task_loop", { shared = true })
state.tasks = state.tasks or {}
state.active = state.active == true
local function reload()
  local latest = bone.state.load("task_loop", { shared = true })
  if type(latest) == "table" and latest.tasks then state = latest end
  state.tasks = state.tasks or {}
  state.active = state.active == true
end
local function save() bone.state.save("task_loop", state, { shared = true }) end
local function render()
  local out = {}
  for i, task in ipairs(state.tasks) do
    out[#out + 1] = string.format("%s %d. %s", task.done and "[x]" or "[ ]", i, task.text)
  end
  return #out > 0 and table.concat(out, "\n") or "no tasks"
end
bone.tool.register({
  name = "task_loop",
  description = "Write or advance an autonomous checklist.",
  parameters = {
    type = "object",
    properties = {
      action = { type = "string", enum = { "write", "advance", "complete", "stop", "resume", "clear", "status" } },
      tasks = { type = "array", items = { type = "string" } },
      index = { type = "integer", minimum = 1 },
    },
    required = { "action" },
  },
  needs_approval = false,
  run = function(args)
    reload()
    local action = args.action
    if action == "write" then
      if type(args.tasks) ~= "table" or #args.tasks == 0 then return nil, "write requires tasks" end
      state.tasks = {}
      for _, text in ipairs(args.tasks) do
        if type(text) ~= "string" or text:match("^%s*$") then return nil, "task text must be non-empty" end
        state.tasks[#state.tasks + 1] = { text = text, done = false }
      end
      state.active = true
    elseif action == "advance" then
      for _, task in ipairs(state.tasks) do if not task.done then task.done = true; break end end
      state.active = false
      for _, task in ipairs(state.tasks) do if not task.done then state.active = true; break end end
    elseif action == "complete" then
      local i = tonumber(args.index)
      if i and state.tasks[i] then state.tasks[i].done = true else return nil, "unknown task index" end
      state.active = false
      for _, task in ipairs(state.tasks) do if not task.done then state.active = true; break end end
    elseif action == "stop" then state.active = false
    elseif action == "resume" then state.active = #state.tasks > 0
    elseif action == "clear" then state.tasks, state.active = {}, false
    elseif action ~= "status" then return nil, "unknown task_loop action" end
    save()
    return render()
  end,
})
bone.hook("turn_start", function(ev)
  reload()
  if not state.active or #state.tasks == 0 then return end
  return { text = ev.text .. "\n\nTask loop:\n" .. render() ..
    "\nAdvance the current task, then call task_loop with action=advance when verified." }
end)
bone.hook("turn_end", function(ev)
  reload()
  if not state.active or ev.outcome.status ~= "completed" then return end
  bone.queue.add(ev.session_id, "Continue the task loop and verify the next actionable item.", "next")
end)
