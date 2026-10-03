-- git plugin, TUI side: how git_status calls look, and a /git command.

local status_hl = { M = "DiffAdd", A = "DiffAdd", D = "DiffDelete", ["?"] = "Dim" }

-- One header row with the branch, then each changed file colored by status.
bone.ui.tool_views.git_status = function(ev)
  if not ev.done or ev.is_error then
    return { title = { { "git status", "ToolName" } } }
  end
  local branch, files = nil, {}
  for line in ev.output:gmatch("[^\n]+") do
    if line:sub(1, 2) == "##" then
      branch = line:sub(4)
    else
      local code = line:sub(1, 2):gsub(" ", "")
      files[#files + 1] = { { line:sub(1, 2) .. " ", status_hl[code:sub(1, 1)] or "ToolOutput" }, { line:sub(4), "ToolPath" } }
    end
  end
  local title = { { "git status ", "ToolName" }, { branch or "", "ToolSummary" } }
  if #files == 0 then
    title[#title + 1] = { "  clean", "Dim" }
  end
  return { title = title, lines = files }
end

bone.cmd.create("git", function()
  local s = bone.api.session()
  -- In the background, so a slow repository never freezes the UI.
  bone.system("git status --short --branch 2>&1", { cwd = s and s.cwd or "." }, function(r, err)
    local out = r and r.stdout or err
    bone.notify(out ~= "" and out:gsub("\n$", "") or "clean")
  end)
end, { desc = "git status of the session's directory" })
