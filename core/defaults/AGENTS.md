<!-- bone-agents-reference-version: 6 -->
# Bone Self-Modification Guide

This is not a project `AGENTS.md`. It is Bone's guide for an agent changing
Bone itself: its source, bundled Lua, configuration, or reference docs. It lives
in the resolved config directory named in the system prompt, never in the
working directory.

In tool paths, a leading `.bone-rust/` resolves to the resolved config
directory, so the document paths below work exactly as written from any
working directory.

## Start Here

Read the relevant document before changing code, and update that document when
core behavior changes.

| Task | Document |
|---|---|
| Understand ownership, runtime flow, sessions, and persistence | `.bone-rust/docs/architecture.md` |
| Change settings, providers, policies, themes, or keymaps | `.bone-rust/docs/configuration.md` |
| Add or change Lua tools, commands, hooks, or UI APIs (all shipped as `lua/plugins/<name>/` packages) | `.bone-rust/docs/extension-api.md` |
| Change delegation, approvals, cancellation, or background jobs | `.bone-rust/docs/agents.md` |
| Change TUI, desktop, daemon connections, events, or rendering | `.bone-rust/docs/ui.md` |
| Build, test, validate, or update bundled documentation | `.bone-rust/docs/development.md` |

This guide and those documents are bundled from `core/defaults/` in the Bone
source repository and rewritten under the config directory at startup to match
the running build. Edit the bundled source (`core/defaults/AGENTS.md` and
`core/defaults/docs/`), never the generated copies. The bundled core reference
documents platform contracts only. Optional installed extensions own their
feature behavior and documentation; do not describe them as built-in core
behavior.

## Universal operating rules

- Keep one core `Driver`: the daemon owns sessions, transcripts, approvals,
  tools, jobs, configuration, and durable state. Frontends are thin clients of
  the protocol.
- A path starting with `.bone-rust/` resolves inside the resolved config
  directory; every other relative path resolves against the working directory,
  and absolute paths are used as written. Preserve unrelated user data and
  working-tree changes.
- Prefer native file tools for file contents. Read before editing; use `shell`
  for commands and only when a dedicated file operation cannot express the job.
- After directly editing `providers.yaml`, `subagents.yaml`, `extensions.yaml`,
  `config.yaml`, or `command-policy.yaml`, tell the user to restart Bone.
  Prefer `/config` or another daemon mutation API when available.
- Keep approvals, cancellation, protocol boundaries, and generated/reference
  files intact. Validate focused behavior, formatting, and tests before claiming
  completion.
