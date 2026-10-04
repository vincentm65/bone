local state = bone.state.load("goal")
state.sessions = state.sessions or {}
local MAX_ITERATIONS = 50

local function get(id)
  id = id or "default"
  state.sessions[id] = state.sessions[id] or { active = false, iteration = 0 }
  return state.sessions[id]
end
local function save() bone.state.save("goal", state) end

bone.tool.register({
  name = "goal",
  description = "Start, inspect, resume, stop, or clear an autonomous goal.",
  parameters = {
    type = "object",
    properties = {
      action = { type = "string", enum = { "write", "status", "resume", "stop", "clear" } },
      goal = { type = "string" },
    },
    required = { "action" },
  },
  needs_approval = false,
  run = function(args, ctx)
    local g = get(ctx.session_id)
    if args.action == "write" then
      if type(args.goal) ~= "string" or args.goal:match("^%s*$") then return nil, "goal is required" end
      g.text, g.active, g.iteration = args.goal, true, 0
      save()
      return "Goal started. Work through it and emit [GOAL_DONE] when it is complete."
    elseif args.action == "status" then
      return g.active and ("active iteration " .. g.iteration .. ": " .. (g.text or "")) or "no active goal"
    elseif args.action == "resume" then
      if not g.text then return nil, "no saved goal" end
      g.active = true; save(); return "goal resumed"
    elseif args.action == "stop" then
      g.active = false; save(); return "goal stopped"
    elseif args.action == "clear" then
      state.sessions[ctx.session_id] = nil; save(); return "goal cleared"
    end
    return nil, "unknown goal action"
  end,
})

bone.hook("turn_start", function(ev)
  local g = get(ev.session_id)
  if not g.active or not g.text then return end
  return { text = ev.text .. "\n\nActive goal: " .. g.text ..
    "\nContinue making concrete progress. Emit [GOAL_DONE] only when complete." }
end)

bone.hook("turn_end", function(ev)
  local g = get(ev.session_id)
  if not g.active or ev.outcome.status ~= "completed" then return end
  local messages = bone.session.messages(ev.session_id)
  local last = messages[#messages]
  local text = last and last.content or ""
  if text:find("%[GOAL_DONE%]", 1, true) or g.iteration >= MAX_ITERATIONS then
    g.active = false
  else
    g.iteration = g.iteration + 1
    bone.queue.add(ev.session_id, "Continue the active goal. Verify the last step before starting the next one.", "next")
  end
  save()
end)
