-- /customize — quick-start guide for asking bone to customize itself.

local guide = [[
  Customize bone
  ══════════════

  Ask for the outcome you want in plain language. Bone can inspect the
  config, explain the options, make the change, and tell you how to reload.

  ── Configs ──────────────────────────────────────────────────────────

  YAML files that control bone's behavior. Stored in your config
  directory (~/.bone-rust/).

  providers.yaml       — LLM providers and models. Change which
                         provider to use, switch models, set defaults.
                         Example: swap "openai" for "anthropic", or
                         change the default model from gpt-4o to claude-sonnet-4-20250514.

  command-policy.yaml  — Shell command safety tiers. Controls which
                         commands auto-run vs. require approval.
                         Example: mark git commands as "safe" to skip
                         approval, or make all file writes require "danger" clearance.

  config/*.yaml        — Feature toggles and thresholds.
                         Example: set auto-compaction token limits, adjust
                         memory update frequency, or change the max tool
                         nesting depth.

  ── Commands ─────────────────────────────────────────────────────────

  Lua scripts in lua/commands/ that add slash commands like /compact
  or /memory. Run on demand from the chat.

  What you can change:
  • Rename or remove bundled commands (e.g. drop /compact entirely).
  • Change command behavior — tighten compaction thresholds, alter
    what /memory summarizes, or change output formatting.
  • Add new commands — a /release checklist, a /git-status summary,
    a /find-dead-code helper.

  Commands get a full ctx with shell access, file I/O, agent spawning,
  and session history.

  ── Tools ────────────────────────────────────────────────────────────

  Lua scripts in lua/tools/ that extend what the LLM can do. Each
  tool has a name, description, typed parameters, and an execute
  function.

  What you can change:
  • Modify existing tools — tighten web_search result limits, add
    filtering to task_list, change cron's default timeout.
  • Add new tools — a GitHub issue search tool, a database query
    wrapper, a project-specific linter runner.
  • Change safety level — mark your custom tool "safe" for auto-run
    or "danger" for approval-gated execution.
  • Control TUI display — show/hide panes, customize what args and
    results appear in the interface.

  Tools are the LLM's primary interface to the outside world: shell,
  filesystem, other tools, subagents, and more.

  ── Lua (init.lua) ──────────────────────────────────────────────────

  A startup script in the config directory that runs once when bone
  launches. Use it to register custom tools, subagents, commands, and
  event hooks.

  What you can do:
  • Register subagents — declare a researcher, reviewer, or test
    verifier with its own system prompt, provider, and model.
  • Register event hooks — run code before each turn, after errors,
    or on other lifecycle events.
  • Set up one-time initialization — create config files, seed
    templates, or log startup diagnostics.

  Errors in init.lua are non-fatal — bone logs a warning and continues
  without Lua support.

  ── ctx (the context object) ────────────────────────────────────────

  Passed to every tool and command handler. Gives your Lua code access
  to bone's internals. Not all fields are available everywhere.

  Key capabilities:
  • ctx.config — read values from YAML config files (read-only).
  • ctx.fs, ctx.read_file, ctx.write_file — filesystem operations.
  • ctx.shell, ctx.shell_streaming — run commands through the approval
    pipeline.
  • ctx.tools.call — invoke other registered tools by name.
  • ctx.agent.run, ctx.agent.spawn — create and manage subagents.
  • ctx.state — session-scoped key-value store for persisting data
    across tool calls.
  • ctx.conversation — read the active chat transcript.
  • ctx.usage.snapshot — check token counts and costs.
  • ctx.ui.notify, ctx.ui.status — send messages to the user.

  Context availability varies:
  • Tools get the full ctx (shell, files, tools, agents, etc.).
  • Commands get most of the same, but no live event emission.
  • Event hooks get a minimal ctx — only config_dir, ui.notify, and
    config.dir. They cannot run shell commands or read files.

  ── Prompt examples ─────────────────────────────────────────────────

  Make reviews stricter about security and race conditions.
  Add a command that prepares a release checklist.
  Create a tool that searches my issue tracker.
  Use a quieter, more direct assistant style.
  Ask before running commands that modify files.
  Show me the config files involved before changing anything.
  Add a subagent for test verification.
  Remember that I prefer small targeted fixes.

  Helpful phrases:

  Explain the current behavior first, then change it.
  Keep this project-agnostic.
  Make the smallest change that works.
  Remove anything unused after the change.
  Verify it with the right command when done.

  Common areas to customize:

  Providers and models
  Tools and command approval
  Slash commands
  Subagents
  Memory and assistant style
  Status, usage, and UI settings

  If you are unsure what to ask, start with:

  Look at my bone config and suggest practical customizations for how I work.
]]

bone.register_command("customize", {
  description = "Quick-start guide to customizing bone",
  handler = function()
    return { display = guide, submit = false }
  end,
})
