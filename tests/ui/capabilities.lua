-- Every frontend reports the shared UI API, by the same names.
local side = bone.api_info().side
eq(side == "tui" or side == "gui", true, "a frontend side")
for _, name in ipairs({
  "ui.options", "ui.events", "ui.commands", "ui.keymaps",
  "ui.themes", "ui.chat_data", "ui.views", "ui.request",
}) do
  eq(bone.has_capability(name), true, name)
end
eq(bone.has_capability("ui.no_such_feature"), false, "unknown names")
