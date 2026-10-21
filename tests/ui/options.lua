-- bone.o: dynamic options (shared ops in bone-ui).
local seen = {}
bone.o.define("conf_mode", "a", {
  desc = "a test option",
  choices = { "a", "b" },
  on_change = function(new, old)
    -- A callback re-enters the API from inside an op.
    seen[#seen + 1] = old .. ">" .. new .. ":" .. bone.o.conf_mode
  end,
})
bone.o.define("conf_flag", false)

eq(bone.o.conf_mode, "a", "default")
bone.o.conf_mode = "b"
eq(bone.o.conf_mode, "b", "set")
eq(seen[1], "a>b:b", "on_change ran with the new value set")
eq(pcall(function() bone.o.conf_mode = "c" end), false, "choices are enforced")

eq(bone.o.apply("conf_flag"), nil, "apply enables")
eq(bone.o.conf_flag, true, "enabled")
bone.o.apply("noconf_flag")
eq(bone.o.conf_flag, false, "disabled")
eq(bone.o.apply("conf_mode?"), "conf_mode=b", "query")

local info = bone.o.info("conf_mode")
eq(info.type, "string", "info type")
eq(info.default, "a", "info default")
eq(info.desc, "a test option", "info desc")

local names = table.concat(bone.o.names(), " ")
eq(names:find("conf_flag", 1, true) ~= nil, true, "names lists it")
eq(bone.o.del("conf_flag"), true, "deleted")
eq(bone.o.del("conf_flag"), false, "already gone")
