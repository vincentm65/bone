# Memory System Plan

## Goal

Ship a default skill that periodically reviews past conversations, extracts user preferences, and writes them to `~/.bone-rust/memory.md`. The agent loads this file at startup, molding behavior to the user over time.

---

## What Already Exists

- **`defaults/skills/`** — YAML skills compiled into the binary, seeded on first launch
- **`defaults/tools/`** — YAML tools compiled in, auto-seeded to `~/.bone-rust/tools/`
- **Session DB** at `~/.bone-rust/data/conversations.db` — full message history with FTS5 search
- **Cron tool** — can schedule `bone run --prompt '/memory'` on any schedule
- **`src/llm/prompts.rs`** — builds the system prompt; extensible to load `memory.md`

## What We Need to Build

### 1. Default Skill: `defaults/skills/memory.yaml`

Ships with bone. Script queries the session DB for recent conversations. Prompt tells the agent to update `memory.md` using `edit_file`.

```yaml
name: memory
description: "Review recent conversations and update memory.md with user preferences"
enabled: true
script: |
  #!/usr/bin/env bash
  set -euo pipefail
  DB="$HOME/.bone-rust/data/conversations.db"
  if [ ! -f "$DB" ]; then
    echo "No session database found."
    exit 0
  fi
  # Get user messages from the last 20 conversations
  sqlite3 "$DB" "
    SELECT m.content
    FROM messages m
    JOIN (
      SELECT id FROM conversations ORDER BY started_at DESC LIMIT 20
    ) c ON m.conversation_id = c.id
    WHERE m.role = 'user'
    ORDER BY c.started_at DESC, m.seq ASC
    LIMIT 200;
  "
prompt: |
  You are a memory builder. Review these recent user messages and the current memory.md (if it exists at the config directory), then update memory.md with user preferences.

  Read the current memory.md first (if it exists). Then produce an updated version.

  Rules:
  - Only add preferences clearly demonstrated (observed 2+ times), not one-off choices.
  - Remove anything contradicted by newer conversations.
  - Keep it under 2KB. Compress if needed.
  - Use these sections: Communication, Coding Style, Tools & Workflow, Project Context, Dislikes.
  - Write the file using edit_file or write_file.
  - Output a brief summary of what changed.

  Recent user messages:
  ```
  {{script_output}}
  ```
```

### 2. System Prompt Extension: `src/llm/prompts.rs`

Load `memory.md` from config dir and inject into system prompt. ~10 lines of Rust.

```rust
pub fn system_prompt() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let bone = bone_dir().display().to_string();
    let memory = std::fs::read_to_string(bone_dir().join("memory.md"))
        .ok()
        .map(|m| format!("\n# User Memory\n{m}\n"))
        .unwrap_or_default();
    format!("{SYSTEM_PROMPT}Resolved config directory: {bone}\nCurrent working directory: {cwd}\n{memory}")
}
```

### 3. Cron Scheduling (opt-in, documented)

User adds a daily cron job:
```
cron(action=add, name=memory, time=03:00, approval=edit, prompt=/memory)
```

Or we add a hint in AGENTS.md:
> Run `/memory` manually, or schedule it: `cron(action=add, name=memory, time=03:00, approval=edit, prompt=/memory)`

---

## Implementation Steps

- [ ] Create `defaults/skills/memory.yaml` with script + prompt
- [ ] Update `src/llm/prompts.rs` to load and inject `memory.md`
- [ ] Add `memory` skill docs to `defaults/AGENTS.md`
- [ ] Test: manually run `/memory`, verify `memory.md` gets created/updated
- [ ] Test: start new session, verify memory is injected into system prompt
- [ ] Test: schedule via cron, verify headless execution works

## Seedability

Everything ships as defaults:
- `defaults/skills/memory.yaml` — compiled into binary, seeded on first launch
- `memory.md` — created by the skill on first run (doesn't need to exist beforehand)
- System prompt code — loads `memory.md` silently if present
- Cron — opt-in; user adds when ready, or we could auto-seed a default cron entry

## Open Questions

- **Auto-seed cron?** We could auto-add a daily memory cron on first launch, but that's opinionated. Documenting it in AGENTS.md is lighter.
- **Session count vs time window?** Current script uses last 20 conversations. Could also do "last 7 days" or make it configurable via args.
- **Memory scope?** Just preferences, or also project facts, file locations, architecture notes?
- **Validation?** Should memory.md changes require user approval? The cron `approval=edit` means the agent can write files without prompting.
