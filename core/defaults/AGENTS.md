<!-- bone-agents-reference-version: 4 -->
# Bone Core Reference

Bone refreshes this concise index and the focused core documents from the
running build. Paths below are relative to the resolved Bone config directory
unless explicitly absolute.

## Start Here

Read the relevant document before changing code, and update that document when
core behavior changes.

| Task | Document |
|---|---|
| Understand ownership, runtime flow, sessions, and persistence | `docs/architecture.md` |
| Change settings, providers, policies, themes, or keymaps | `docs/configuration.md` |
| Add or change Lua tools, commands, hooks, plugins, or UI APIs | `docs/extension-api.md` |
| Change delegation, approvals, cancellation, or background jobs | `docs/agents.md` |
| Change TUI, web, daemon connections, events, or rendering | `docs/ui.md` |
| Build, test, validate, or update bundled documentation | `docs/development.md` |

These files are materialized under the resolved config directory at startup.
The bundled core reference documents platform contracts only. Optional installed
extensions own their feature behavior and documentation; do not describe them as
built-in core behavior.

## Universal operating rules

- Keep one core `Driver`: the daemon owns sessions, transcripts, approvals,
  tools, jobs, configuration, and durable state. Frontends are thin clients of
  the protocol.
- Treat paths as relative to the resolved config directory unless a path is
  explicitly absolute. Preserve unrelated user data and working-tree changes.
- Prefer native file tools for file contents. Read before editing; use `shell`
  for commands and only when a dedicated file operation cannot express the job.
- After directly editing `providers.yaml`, `subagents.yaml`, `extensions.yaml`,
  `config.yaml`, or `command-policy.yaml`, tell the user to restart Bone.
  Prefer `/config` or another daemon mutation API when available.
- Keep approvals, cancellation, protocol boundaries, and generated/reference
  files intact. Validate focused behavior, formatting, and tests before claiming
  completion.
