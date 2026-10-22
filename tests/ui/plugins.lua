-- bone.plugin: a plugin's lifecycle, the same in every frontend. A plugin
-- with a ui.lua runs in both; its tui.lua or gui.lua only in that one.
local side = bone.api_info().side
local function write(path, text)
  local f = assert(io.open(path, "w"))
  f:write(text)
  f:close()
end
local root = bone.config_dir .. "/plugins"
bone.fs.mkdir(root .. "/conf_both")
write(root .. "/conf_both/ui.lua", [[
bone.cmd.create("conf-both", function() end, { desc = "from a shared plugin" })
bone.keymap.set("ctrl+x ctrl+b", function() end, { desc = "conf both key" })
]])
bone.fs.mkdir(root .. "/conf_split")
write(root .. "/conf_split/tui.lua", [[conf_split_side = "tui"]])
write(root .. "/conf_split/gui.lua", [[conf_split_side = "gui"]])
write(root .. "/conf_split/core.lua", [[-- core half]])

bone.plugin.load("conf_both")
eq(bone.cmd.find("conf-both"), "conf-both", "its ui.lua ran")
local info
for _, p in ipairs(bone.plugin.list()) do
  if p.name == "conf_both" then info = p end
end
eq(info.loaded, true, "loaded")
eq(table.concat(info.frontends, ","), "tui,gui", "a ui.lua runs in both frontends")
eq(info.core, false, "no core half")

bone.plugin.load("conf_split")
eq(conf_split_side, side, "only this frontend's file ran")
for _, p in ipairs(bone.plugin.list()) do
  if p.name == "conf_split" then info = p end
end
eq(table.concat(info.frontends, ","), "tui,gui", "tui.lua and gui.lua: both, one file each")
eq(info.core, true, "and a core half")

bone.plugin.unload("conf_both")
eq(bone.cmd.find("conf-both"), nil, "unloading removed its command")
local keys = {}
for _, m in ipairs(bone.keymap.list()) do keys[m.desc] = true end
eq(keys["conf both key"], nil, "and its keys")
bone.plugin.unload("conf_split")
