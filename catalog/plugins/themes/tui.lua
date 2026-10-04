local names = { "ayu", "black", "catppuccin", "dracula", "everforest", "gruvbox", "nord", "one-dark", "rose-pine", "solarized", "tokyo-night" }
bone.cmd.create("themes", function(c)
  if c.args ~= "" then
    local ok, err = pcall(bone.colorscheme, c.args)
    return bone.notify(ok and ("Theme applied: " .. c.args) or ("themes: " .. tostring(err)), ok and "info" or "error")
  end
  bone.ui.select(names, {
    prompt = "Theme",
    on_choice = function(name)
      if name then
        local ok, err = pcall(bone.colorscheme, name)
        bone.notify(ok and ("Theme applied: " .. name) or ("themes: " .. tostring(err)), ok and "info" or "error")
      end
    end,
  })
end, { desc = "pick or apply a color theme", complete = function() return names end })
