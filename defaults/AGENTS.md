# Bone Agent Reference

## Config Location

All file paths below are relative to the bone config directory. The resolved path is provided in the system prompt under "Resolved config directory".

```
config/*.yaml         — Config pages (general, subagent, tools, etc.)
providers.yaml        — LLM provider entries
command-policy.yaml   — Shell command safety tiers
skills/*.yaml         — Skill definitions
tools/*.yaml          — Custom tool definitions
```

After editing `providers.yaml` or `command-policy.yaml`, tell the user to restart bone.
After creating/editing a skill YAML, tell the user to run `/skills reload`.
After creating/editing a tool YAML, tell the user to run `/tools reload`.

## Pre-Seeded Tools

These tools are bundled with bone and seeded to the `tools/` subdirectory of the config directory on first launch.

### ask_user (interaction)
Ask the user a question with selectable options or a custom answer.
```yaml
name: ask_user
version: 1
interaction: select
args:
  - name: question
    type: string
    required: true
  - name: options
    type: array
    required: true
  - name: allow_custom
    type: boolean
    required: false
```

### web_search (script)
Search the web via DuckDuckGo. Returns titles, URLs, summaries.
```yaml
name: web_search
version: 3
args:
  - name: query
    type: string
    required: true
  - name: num_results
    type: integer
    required: false
script: |
  uv run --with ddgs -- python3 -c "..."
```

### subagent (script)
Spawn a sub-agent for isolated or parallel tasks.
```yaml
name: subagent
version: 3
safety: read_only
output:
  kind: jsonl_events
args:
  - name: approval
    type: string
    required: true
  - name: task
    type: string
    required: true
  - name: provider
    type: string
    required: false
  - name: model
    type: string
    required: false
```

### task_list (script, json_envelope, session state)
Manage a named visible task list with TUI pane rendering.
```yaml
name: task_list
version: 12
safety: safe
output:
  kind: json_envelope
display:
  show: false
args:
  - name: action
    type: string
    required: true
  - name: texts
    type: array
    required: false
  - name: name
    type: string
    required: false
  - name: index
    type: number
    required: false
  - name: indices
    type: array
    required: false
```

### cron (script)
Manage scheduled bone jobs via crontab.
```yaml
name: cron
version: 1
safety: edit
args:
  - name: action
    type: string
    required: true
  - name: name
    type: string
    required: false
  - name: time
    type: string
    required: false
  - name: approval
    type: string
    required: false
  - name: prompt
    type: string
    required: false
  - name: cwd
    type: string
    required: false
  - name: tail
    type: integer
    required: false
  - name: allow_skill_scripts
    type: boolean
    required: false
```

## Pre-Seeded Skill

### commit (prompt-only)
Draft a concise commit message from a change summary.
```yaml
name: commit
description: "Draft a concise commit message from the provided change summary"
enabled: true
prompt: |
  Draft a concise conventional-style git commit message for these changes:
  {{args}}
```

## Creating Custom Tools

Tools live as YAML files in the `tools/` subdirectory of the config directory. The agent calls them as typed functions with args, and they return script output to the agent.

### Minimal Tool

```yaml
name: my_tool
version: 1
description: "Short description of what the tool does and when to use it."
args:
  - name: query
    type: string
    description: "What this arg is for"
    required: true
script: |
  set -euo pipefail
  echo "Query is: $TOOL_QUERY"
```

### Arg Reference

- Args are passed as env vars: `TOOL_<UPPERCASE_NAME>`. Non-alphanumeric chars become `_`.
  - `query` → `$TOOL_QUERY`, `output_dir` → `$TOOL_OUTPUT_DIR`
- Types: `string`, `integer`, `boolean`, `array`.
- Required args must have values; optional args may be absent (check with `${TOOL_FOO:-default}`).

### Script vs Interaction Tools

There are two tool modes, controlled by which fields are present:

**Script tool** — has a `script:` field. Runs bash, stdout is returned to the agent.
```yaml
script: |
  set -euo pipefail
  some-command "$TOOL_QUERY"
```

**Interaction tool** — has `interaction: select`, no script. Shows options in the TUI app pane and returns the user's selection.
```yaml
interaction: select
args:
  - name: question
    type: string
    required: true
  - name: options
    type: array
    required: true
  - name: allow_custom
    type: boolean
    required: false
```

### Tool Output

- **Scripted tools:** stdout is captured and returned to the agent as the tool result. stderr is shown to the user. Exit 0 = success, non-zero = error shown to the agent.
- **Interaction tools:** the user's selection is returned as the tool result.

### App Pane (TUI Display)

Tools can render content in the bone TUI's tool pane. Use `output.kind: json_envelope` and include a `pane` object in the JSON output:

```yaml
output:
  kind: json_envelope
```

Then your script prints a JSON object with `content` (text returned to agent), optional `state` (see below), and `pane`:

```json
{
  "content": "Result text for the agent",
  "pane": {
    "source": "my_tool",
    "title": "My Tool",
    "visible_rows": 8,
    "scroll": 0,
    "lines": [
      {"spans": [{"text": "Label: ", "fg": "dark_gray"}, {"text": "value", "fg": "white"}]}
    ]
  }
}
```

Pane fields:
- `source` — unique identifier for this tool's pane area
- `title` — pane header
- `visible_rows` — pane height in terminal rows
- `scroll` — scroll offset (0 = top)
- `lines` — array of line objects. Each line is either a string or an object with `spans` (array of `{text, fg?, bg?, modifiers?}`). Colors: `white`, `dark_gray`, `green`, `red`, `yellow`, `blue`, `cyan`, `magenta`. Modifiers: `bold`, `dim`, `italic`, `underline`, `strike`.

For live-updating streaming panes, use `output.kind: jsonl_events` with `StateUpdate`/`StateRemove` events.

### Session State (Persistence Across Calls)

Tools that need to remember data between invocations (e.g., task lists) use session state:

1. Your script emits JSON with a `state` field (a string, typically `json.dumps(data)`).
2. The host stores it keyed by `(pane.source, "default")`.
3. On the next call, the host sets `TOOL_SESSION_STATE` to the stored string.
4. Your script reads it, modifies it, and emits a new `state`.

Requires `output.kind: json_envelope`. State does not persist across bone restarts.

### Display Config

```yaml
display:
  show: true             # Show a pane for this tool (default: true)
  template: "{action}"   # Format string for pane title using {arg_name}
  show_result: true      # Show the result in the pane
  args: [action, name]   # Which arg values to show in the call summary
```

### Safety

```yaml
safety: read_only    # read_only | edit | danger
```

Overrides the approval level required to call this tool, regardless of what the script does.

### Version

`version` is a cache key. Bump it after editing so bone picks up changes.

## Creating Skills

Skills are prompt templates invoked as `/<name> [args]`. They live in the `skills/` subdirectory of the config directory.

```yaml
name: my_skill
description: "What this skill does"
enabled: true
prompt: |
  Instructions for the agent when this skill is invoked.
  User provided: {{args}}

# Optional: run a script first, inject output into prompt
# script: |
#   some-command {{args}}
```

- `{{args}}` — substituted with the user's arguments
- `{{script_output}}` — substituted with the script's stdout (if `script:` is present)
- `enabled: false` — disables without deleting
- Skill names: letters, digits, hyphens, underscores. Must not collide with builtins (`help`, `clear`, `new`, `model`, `provider`, `tools`, `config`, `skills`, `edit`, `e`, `quit`, `exit`).

## Skill vs Tool

- **Skill** — User invokes `/<name>`. Agent receives an expanded prompt. Good for workflows, reviews, templates, content generation.
- **Tool** — Agent calls as a function with typed args. Returns script output. Good for integrations, searches, state management, TUI panes.
