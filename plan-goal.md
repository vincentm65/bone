# /goal — Codex-style autonomous goal loop

## Goal
Turn `/goal` into a persistent, self-checking autonomous loop: plan → act → verify → repeat, running in the main conversation until the agent declares done or is interrupted. Strictly Lua, one file: `lua/commands/goal.lua`.

## Core mechanism (2 hooks + 1 sentinel)

| Part | Mechanism |
|---|---|
| Objective persists across compaction | Goal checklist on disk (`goals/active.md`); `before_turn` re-injects it every turn via `system_prompt_append` |
| Autonomous loop | `turn_end` hook → `bone.api.submit("Continue")` on each successful `working` turn |
| Exit condition | Model ends message with `GOAL_STATUS: done|working|blocked`; `turn_end` parses it |
| Interruption | Esc cancels turn → `turn_end` gets `ok=false` → loop halts |

## Module-local state
```lua
local state = { active = false, path = nil, iteration = 0 }
```
Shared across command handler, `before_turn`, and `turn_end` via closure. No `ctx.state` needed (turn_end has minimal ctx).

## Commands
- `/goal <desc>` → write file, set active, return short kickoff prompt (starts turn 1)
- `/goal stop` → active = false
- `/goal resume` → active = true, submit "Continue the goal" (for post-interrupt recovery)
- `/goal status` / `/goal` (no args) → print file + iteration, no submit

## before_turn hook (fires every turn, full ctx)
Guard: `bone.agent_depth == 0` and `state.active`.
Re-reads the goal file from disk. Appends to system prompt:
- Goal description + checklist file path
- Per-turn discipline: pick next unchecked task → do → verify (build/test) → check off → log progress
- Sentinel contract: end every response with `GOAL_STATUS: working|done|blocked: <reason>`

## turn_end hook (fires every turn, minimal ctx)
```
if !state.active → return
if !event.ok → state.active = false; notify "interrupted"; return
status = match GOAL_STATUS in event.content
  done    → active=false; notify "goal complete"
  blocked → active=false; notify reason
  working/missing → iteration++; submit("Continue the goal")
```
Missing sentinel treated as `working` (no deadlock). No iteration cap (Esc or `/goal stop` is the stop).

## Interruption flow
```
Esc → turn cancels → turn_end { ok=false } → active=false → no submit → loop dead
File preserves whatever was checked off. /goal resume to continue.
```

## File format (unchanged — already good)
```markdown
# Goal
<description>

## Acceptance Criteria
- [ ] ...

## Tasks
- [ ] ...

## Progress
- [timestamp] did X
```

## Guardrails
1. `bone.agent_depth == 0` only (sub-agent turns never trigger loop)
2. Failed/cancelled turn halts loop
3. Missing sentinel → continue (no deadlock)
4. `/goal stop` always works

## Rejected
- Blocking sub-agent loop (`ctx.agent.run`) — hidden context, not interactive, 16k truncation. Wrong UX.
- Iteration cap — Esc + `/goal stop` sufficient.
- Stall detection — overkill for v1.
