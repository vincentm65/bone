-- /customize — quick-start guide for asking bone to customize itself.

local H = "\x1b[1;36m" -- bold cyan  — banner title
local C = "\x1b[36m"   -- cyan       — section titles
local D = "\x1b[2m"    -- dim        — borders, dividers, secondary text
local Y = "\x1b[33m"   -- yellow     — example prompts
local G = "\x1b[32m"   -- green      — call to action
local R = "\x1b[0m"    -- reset

local guide = [[
  ~D~┏━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓~R~
  ~D~┃~R~     ~H~C U S T O M I Z E   ·   b o n e~R~     ~D~┃~R~
  ~D~┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛~R~

  Describe the outcome you want in plain language.
  Bone inspects the config, explains the options, makes
  the change, and tells you how to reload.

  ~D~════ ~C~Configs~R~ ~D~═════════════════════════════════~R~
  ~D~YAML files in ~/.bone-rust/ that shape behavior.~R~

  providers.yaml        ~D~LLM providers & models~R~
  command-policy.yaml   ~D~Shell command safety tiers~R~
  config/*.yaml         ~D~Feature toggles & thresholds~R~

  ~D~════ ~C~Commands~R~ ~D~═══════════════════════════════~R~
  ~D~Lua scripts in lua/commands/ — slash commands like~R~
  ~D~/compact or /memory (via catalog), run on demand from the chat.~R~

    rename / rework bundled commands
    add your own — /release, /git-status, /find-dead-code
    full ctx: shell, files, agents, session history

  ~D~════ ~C~Tools~R~ ~D~══════════════════════════════════~R~
  ~D~Lua scripts in lua/tools/ that extend the LLM.~R~

    typed params + execute function + safety level
    tweak bundled tools or write new ones
    control TUI panes — show/hide, pick args & results
    safety: ~D~"read_only" auto-runs · "danger" needs approval~R~

  ~D~════ ~C~Startup~R~ ~D~════════════════════════════════~R~
  ~D~init.lua runs once at launch.~R~

    register subagents — researcher, reviewer, test-verifier
    register event hooks — before_turn, on_error, lifecycle
    errors are non-fatal — bone logs a warning and continues

  ~D~════ ~C~ctx~R~ ~D~════════════════════════════════════~R~
  ~D~The context object passed to tools & command handlers.~R~

    shell · read_file / write_file · tools.call
    agent.run / agent.spawn · state · conversation
    config · usage.snapshot · ui.notify

  ~D~Tools & commands get the full ctx.~R~
  ~D~Event hooks get a minimal ctx (config_dir, ui.notify).~R~

  ~D~════ ~C~Try saying~R~ ~D~═════════════════════════════~R~

  ~Y~"Swap my default model to claude-sonnet."~R~
  ~Y~"Add a /release checklist command."~R~
  ~Y~"Create a tool that searches my issues."~R~
  ~Y~"Make reviews stricter about race conditions."~R~
  ~Y~"Mark git commands safe to skip approval."~R~
  ~Y~"Add a subagent for test verification."~R~
  ~Y~"Remember I prefer small, targeted fixes."~R~

  ~D~───────────────────────────────────────────────~R~
  ~D~Unsure where to start?~R~

  ~G~"Look at my bone config and suggest practical~R~
  ~G~ customizations for how I work."~R~

]]

guide = guide
  :gsub("~H~", H):gsub("~C~", C):gsub("~D~", D)
  :gsub("~Y~", Y):gsub("~G~", G):gsub("~R~", R)

bone.register_command("customize", {
  description = "Quick-start guide to customizing bone",
  handler = function()
    return { display = guide, submit = false }
  end,
})
