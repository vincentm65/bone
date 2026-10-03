-- skills, core side: register the skills in folders your core.lua names.
--
--   bone.config.skill_dirs = { "~/.bone/skills", "~/work/team-skills" }

bone.config.skill_dirs = {}

bone.on_ready(function()
  for _, dir in ipairs(bone.config.skill_dirs or {}) do
    local ok, err = pcall(bone.skill.load_dir, dir)
    if not ok then
      print("skills plugin: " .. tostring(err))
    end
  end
end)
