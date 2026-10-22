-- Every frontend reports the shared UI API, by the same names.
local side = bone.api_info().side
eq(side == "tui" or side == "gui", true, "a frontend side")
for _, name in ipairs({
  "ui.options", "ui.events", "ui.commands", "ui.keymaps",
  "ui.themes", "ui.chat_data", "ui.views", "ui.request",
  "ui.layout", "ui.windows", "ui.panels", "ui.input",
  "ui.prompt", "ui.jobs", "ui.plugins", "ui.session",
}) do
  eq(bone.has_capability(name), true, name)
end
eq(bone.has_capability("ui.no_such_feature"), false, "unknown names")

-- What only one frontend has is reported only there.
eq(bone.has_capability("gui.window"), side == "gui", "gui.window")
eq(bone.has_capability("gui.menus"), side == "gui", "gui.menus")
eq(bone.has_capability("tui.keymaps"), side == "tui", "the TUI's older names")
