-- /cd and bone.api.cwd/cd: the directory new sessions start in (shared ops
-- in bone-ui, the command in the runtime).
local started = bone.api.cwd()
eq(type(started), "string", "new sessions start somewhere")
assert(bone.fs.mkdir(started .. "/cd-test/inner"))

bone.cmd("/cd cd-test/inner")
eq(bone.api.cwd(), started .. "/cd-test/inner", "a path relative to where sessions start")
bone.cmd("/cd ..")
eq(bone.api.cwd(), started .. "/cd-test", "/cd .. goes up")
bone.cmd("/cd ./inner/../inner/")
eq(bone.api.cwd(), started .. "/cd-test/inner", "dots and a trailing slash are cleaned away")
bone.cmd("/cd " .. started)
eq(bone.api.cwd(), started, "an absolute path")
bone.cmd("/cd no-such-dir")
eq(bone.api.cwd(), started, "a directory that is not there is refused")

local hits = bone.cmd.complete("cd", { token = "cd-test/in", text = "/cd cd-test/in" })
eq(hits and hits[1], "cd-test/inner/", "completion lists the directories under what is typed")

-- The API takes the path as given.
bone.api.cd(started .. "/cd-test")
eq(bone.api.cwd(), started .. "/cd-test", "bone.api.cd sets it")
bone.api.cd(started)
return true
