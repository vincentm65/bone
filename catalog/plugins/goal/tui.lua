bone.cmd.create("goal", function(c)
  if c.args == "" then
    bone.prompt.set("Use the goal tool to start an autonomous goal.")
  elseif c.args == "stop" then
    bone.prompt.set("Use the goal tool with action stop.")
  else
    bone.prompt.set("Start an autonomous goal: " .. c.args)
  end
end, { desc = "start or stop an autonomous goal" })
