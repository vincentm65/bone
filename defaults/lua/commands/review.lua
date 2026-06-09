bone.register_command("review", {
  description = "Review unstaged git changes for bugs, dead code, mess, and improvements. Categorized by importance.",
  handler = function(args, ctx)
    -- Get unstaged diff
    local diff_result = ctx.shell("git diff 2>/dev/null")

    local diff_output
    if diff_result.exit_code ~= 0 then
      diff_output = "(no git repository or git diff failed: " .. diff_result.stderr .. ")"
    else
      diff_output = diff_result.stdout
    end

    if diff_output:match("^%s*$") then
      return [[Review the following unstaged diff for bugs, dead code, messy code, and improvements.

**Important: before filing any issue, read the actual source files referenced in the diff to verify your finding against the real code.** Do not speculate based on the diff alone — use the `read_file` tool to confirm context, callers, control flow, and whether something is truly dead/unreachable. False positives waste the reader's time.

Rules:
- Be concise. No filler. No fluff.
- Categorize findings by importance: CRITICAL > WARNING > INFO > SUGGESTION
- Only report real, verified issues — skip false positives and nitpicks
- Use short bullet points
- If you initially think something is an issue but code reading proves it's fine, do NOT report it

Diff:
```
]] .. diff_output .. [[```

If the diff is empty, say "No unstaged changes." and stop.

Format:

## CRITICAL
(bugs, logic errors, security issues)

## WARNING
(potential issues, unclear logic, subtle bugs)

## INFO
(dead code, unused imports, redundant logic)

## SUGGESTION
(cleaner patterns, minor improvements)

## Summary
(one short paragraph covering the overall quality and key takeaways)]]
    end

    return [[Review the following unstaged diff for bugs, dead code, messy code, and improvements.

**Important: before filing any issue, read the actual source files referenced in the diff to verify your finding against the real code.** Do not speculate based on the diff alone — use the `read_file` tool to confirm context, callers, control flow, and whether something is truly dead/unreachable. False positives waste the reader's time.

Rules:
- Be concise. No filler. No fluff.
- Categorize findings by importance: CRITICAL > WARNING > INFO > SUGGESTION
- Only report real, verified issues — skip false positives and nitpicks
- Use short bullet points
- If you initially think something is an issue but code reading proves it's fine, do NOT report it

Diff:
```
]] .. diff_output .. [[```

If the diff is empty, say "No unstaged changes." and stop.

Format:

## CRITICAL
(bugs, logic errors, security issues)

## WARNING
(potential issues, unclear logic, subtle bugs)

## INFO
(dead code, unused imports, redundant logic)

## SUGGESTION
(cleaner patterns, minor improvements)

## Summary
(one short paragraph covering the overall quality and key takeaways)]]
  end,
})
