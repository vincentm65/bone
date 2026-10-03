-- templates, core side: register the prompt templates in folders your
-- core.lua names.
--
--   bone.config.template_dirs = { "~/.bone/prompts" }   -- *.md files

bone.config.template_dirs = {}

bone.on_ready(function()
  for _, dir in ipairs(bone.config.template_dirs or {}) do
    local ok, err = pcall(bone.template.load_dir, dir)
    if not ok then
      print("templates plugin: " .. tostring(err))
    end
  end
end)
