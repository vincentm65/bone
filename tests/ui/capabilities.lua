-- Every frontend reports the shared UI API, by the same names.
local side = bone.api_info().side
eq(side == "tui" or side == "gui", true, "a frontend side")
for _, name in ipairs({
  "ui.options", "ui.events", "ui.commands", "ui.keymaps",
  "ui.themes", "ui.chat_data", "ui.views", "ui.request",
  "ui.layout", "ui.windows", "ui.panels", "ui.input",
  "ui.prompt", "ui.jobs", "ui.plugins", "ui.session",
  "ui.local_events", "ui.command_specs", "ui.dynamic_options", "ui.pickers",
  "ui.model", "ui.rpc", "ui.project", "jobs.streaming", "plugins.lifecycle",
}) do
  -- The GUI draws a node tree: it has no cell layout or docked cell panels.
  local cells = name == "ui.layout" or name == "ui.panels"
  eq(bone.has_capability(name), not (side == "gui" and cells), name)
end
eq(bone.has_capability("ui.no_such_feature"), false, "unknown names")

-- What only one frontend has is reported only there.
eq(bone.has_capability("gui.window"), side == "gui", "gui.window")
eq(bone.has_capability("gui.nodes"), side == "gui", "gui.nodes")
eq(bone.has_capability("tui.keymaps"), side == "tui", "the TUI's older names")
