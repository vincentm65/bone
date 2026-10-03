-- git plugin, core side: a read-only git_status tool for the model.
bone.tool.register {
  name = "git_status",
  description = "Show the git branch and changed files in the working directory (git status --short --branch).",
  parameters = { type = "object", properties = {} },
  needs_approval = false,
  run = function(_, ctx)
    local r = bone.system("git status --short --branch", { cwd = ctx.cwd })
    if r.code ~= 0 then
      return nil, r.stderr ~= "" and r.stderr or "not a git repository"
    end
    return r.stdout
  end,
}
