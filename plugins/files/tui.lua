-- The file explorer and the editor are windows of the desktop app; the
-- terminal has neither. The commands are here so they answer, instead of
-- being unknown.

bone.cmd.create("files", function()
  bone.notify("The file explorer is in the desktop app (bone --gui).", "info")
end, { desc = "browse the files (in the desktop app)", aliases = { "explorer" } })

bone.cmd.create("editor", function()
  bone.notify("The file editor is in the desktop app (bone --gui).", "info")
end, { desc = "edit a file (in the desktop app)", aliases = { "edit" } })
