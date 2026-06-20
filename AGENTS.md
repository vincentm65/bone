# Bone — repo-internal notes

Only relevant when editing this repository (`~/projects/bone`).

## Architecture
- The TUI uses the core Driver as its only turn loop. The old non-Driver TUI
  loop and the `BONE_DRIVER` toggle are gone.
- Text streaming is via `RuntimeEvent::TextDelta`. `AgentRunEvent` is only a
  compatibility alias to `RuntimeEvent` for now.
