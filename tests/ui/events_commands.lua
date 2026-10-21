-- bone.on/off and bone.cmd (shared ops in bone-ui).
local heard = {}
local id = bone.on("settings/changed", function(ev)
  heard[#heard + 1] = ev.path
end)
local gone = bone.on("settings/changed", function()
  error("removed handlers must not run")
end)
bone.off(gone)
bone.settings.set("conformance.flag", true)

local ran = {}
bone.cmd.create("conf-echo", function(ctx)
  ran[#ran + 1] = ctx.arguments.word .. "/" .. tostring(ctx.arguments.n)
end, {
  desc = "echo for the conformance suite",
  aliases = { "cecho" },
  args = { { name = "word", required = true }, { name = "n", type = "integer", default = 2 } },
  complete = function(ctx)
    return { "alpha", "beta" }
  end,
})
eq(bone.cmd.find("cecho"), "conf-echo", "alias finds the command")
eq(bone.cmd.find("nope"), nil, "unknown")
local listed
for _, c in ipairs(bone.cmd.list()) do
  if c.name == "conf-echo" then
    listed = c
  end
end
eq(listed and listed.desc, "echo for the conformance suite", "listed with desc")
eq(listed.complete, true, "has completion")
eq(bone.cmd.complete("conf-echo", { text = "" })[2], "beta", "completion runs")
eq(pcall(bone.cmd.create, "Bad Name", function() end), false, "invalid names are refused")

bone.cmd("/cecho hi")
eq(ran[1], "hi/2", "typed arguments with a default")

return {
  function()
    eq(heard[1], "conformance.flag", "settings/changed reached the handler")
    eq(#heard, 1, "once, and not the removed one")
    bone.off(id)
    bone.cmd.del("cecho")
    eq(bone.cmd.find("conf-echo"), nil, "deleted by alias")
  end,
}
