-- /review — review unstaged changes for code smells, bugs, logic errors,
-- dead code, and clean code.
--
-- Requires: ctx.shell("git diff --no-color"), submit=true to trigger LLM.

bone.register_command("review", {
  description = "Review unstaged changes for bugs, smells, dead code, and clean code",
  handler = function(_, ctx)
    local result = ctx.shell("git diff --no-color")
    if result.exit_code ~= 0 or (result.stdout:gsub("^%s*", "") == "") then
      return { display = "No unstaged changes to review.", submit = false }
    end

    local diff = result.stdout
    -- Truncate very large diffs to avoid blowing the prompt
    if #diff > 50000 then
      diff = diff:sub(1, 50000) .. "\n... (truncated, diff exceeds 50k chars)"
    end

    local prompt = [[Review these unstaged changes for code smells, bugs, logic errors, dead code, and clean code:
- Point out any bugs or potential bugs (null dereferences, off-by-one, resource leaks, etc.)
- Identify code smells (long functions, deep nesting, duplicated logic, God classes, etc.)
- Flag dead or unreachable code
- Suggest specific improvements for readability and maintainability
- Note any security concerns

Be thorough but organized. Group findings by file, and quote relevant code snippets.]]

    return { display = prompt .. "\n\n" .. diff, submit = true }
  end,
})
