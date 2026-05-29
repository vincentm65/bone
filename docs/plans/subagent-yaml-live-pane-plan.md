# Plan: Update `subagent.yaml` to own its live pane

## Context

Rust now supports generic live pane events from tools.

A dynamic tool with:

```yaml
output:
  kind: jsonl_events
```

can print JSONL events like:

```json
{"type":"pane","pane":{"source":"my-tool","title":"my panel","lines":["hello"]}}
```

Bone will live-update the bottom pane. If `lines` is empty, the pane is removed:

```json
{"type":"pane","pane":{"source":"my-tool","title":"my panel","lines":[]}}
```

The remaining work is to make the YAML/script tool populate the pane itself.

## Goal

Update `defaults/tools/subagent.yaml` so the tool script emits live `type: "pane"` events while running `bone agent --events`.

Rust should not know how to format a subagent display. Rust only renders pane data from the tool.

## Files to edit

Primary:

```text
defaults/tools/subagent.yaml
```

Potential tests:

```text
tests/dynamic_tools_test.rs
```

Optional docs/examples:

```text
docs/
```

## Desired display

Simple compact pane content:

```text
MODE TIME MODEL  TOKENS TITLE                     NOW
ro   1:42 sonnet 9.6k   Review unstaged changes    git diff
```

For a single `subagent` tool, no `ID` is needed. For a future multi-agent orchestrator tool, add `ID`.

Pane title examples:

```text
subagent ro 1:42 9.6k tok
subagent edit 0:21 1.3k tok
```

## Pane event shape

The script should periodically print:

```json
{
  "type": "pane",
  "pane": {
    "source": "subagent-${call or approval}",
    "title": "subagent ro 1:42 9.6k tok",
    "visible_rows": 3,
    "lines": [
      "MODE TIME MODEL  TOKENS TITLE                     NOW",
      "ro   1:42 sonnet 9.6k   Review unstaged changes    git diff"
    ]
  }
}
```

Because the tool does not currently receive the tool call id as an env var, use a stable-enough source for now:

```text
subagent-${TOOL_APPROVAL}
```

Better follow-up: pass `TOOL_CALL_ID` to scripts, then use:

```text
subagent-${TOOL_CALL_ID}
```

## Script behavior

Current script:

```bash
exec bone "${args[@]}"
```

Replace with a wrapper that:

1. builds the same `args` array
2. starts `bone agent --events`
3. reads its JSONL stdout line-by-line
4. forwards non-pane agent events to stdout so final content parsing still works
5. updates local state from events
6. emits `type: "pane"` events after relevant state changes

Important: because `parse_jsonl_events` still parses the whole stdout at the end, keep forwarding original agent events.

## State to track in bash

Variables:

```bash
approval="${TOOL_APPROVAL:-read_only}"
mode="ro" # read_only -> ro, edit -> edit, danger -> danger
task="${TOOL_TASK:-}"
title="short task title"
model=""
now="starting"
sent=0
received=0
started_epoch=$(date +%s)
```

On events:

- `started`
  - model from `.model`
  - approval from `.approval` if present
  - task/title from `.task` if present
  - now = `thinking`

- `status`
  - message from `.message`
  - if `running shell: git diff`, now should become something compact like `shell git diff` or `git diff`
  - else now = message

- `tool_call`
  - name from `.name`
  - summary from `.summary`
  - now = compact `${name} ${summary}`

- `token_usage`
  - sent from `.sent`
  - received from `.received`

- `finished`
  - now = `done`
  - emit remove-pane event or final done pane

- `failed`
  - now = `failed: <message>`
  - keep pane visible until tool result processing, or remove it at end

## Helper formatting

Use `jq` if available. But avoid making `jq` required unless Bone already assumes it. Safer: use Python for JSON parsing/escaping if available, but Python may also be unavailable.

Recommended simple approach: embed a small Python wrapper in the bash script:

```bash
python3 - "$approval" "$task" -- "${args[@]}" <<'PY'
# Python launches bone agent, reads JSONL, prints original events and pane events.
PY
```

If avoiding Python dependency is required, use bash-only with minimal parsing, but JSON escaping will be fragile.

## Recommended YAML script structure

```yaml
script: |
  set -euo pipefail

  approval="${TOOL_APPROVAL:-read_only}"
  task="${TOOL_TASK:-}"

  args=(agent --events --approval "$approval" --prompt "$task")
  if [ -n "${TOOL_PROVIDER:-}" ]; then
    args+=(--provider "$TOOL_PROVIDER")
  fi
  if [ -n "${TOOL_MODEL:-}" ]; then
    args+=(--model "$TOOL_MODEL")
  fi

  python3 - "$approval" "$task" "${args[@]}" <<'PY'
  import json, subprocess, sys, time

  approval = sys.argv[1]
  task = sys.argv[2]
  args = sys.argv[3:]

  def short_mode(v):
      return {"read_only": "ro", "edit": "edit", "danger": "danger"}.get(v, v[:6] or "?")

  def short_title(s):
      first = (s or "").splitlines()[0].strip()
      if len(first) > 28:
          first = first[:25] + "..."
      return first or "subagent"

  def fmt_elapsed(start):
      sec = max(0, int(time.time() - start))
      return f"{sec//60}:{sec%60:02d}"

  def fmt_tokens(sent, received):
      total = int(sent or 0) + int(received or 0)
      if total >= 1000:
          return f"{total/1000:.1f}k"
      return str(total)

  def clip(s, n):
      s = " ".join((s or "").split())
      return s if len(s) <= n else s[:max(0, n-3)] + "..."

  state = {
      "approval": approval,
      "mode": short_mode(approval),
      "task": task,
      "title": short_title(task),
      "model": "?",
      "now": "starting",
      "sent": 0,
      "received": 0,
      "start": time.time(),
  }

  source = f"subagent-{state['mode']}"

  def emit_pane(remove=False):
      if remove:
          pane = {"source": source, "title": "subagent", "lines": []}
      else:
          elapsed = fmt_elapsed(state["start"])
          tokens = fmt_tokens(state["sent"], state["received"])
          title = f"subagent {state['mode']} {elapsed} {tokens} tok"
          row = f"{state['mode']:<4} {elapsed:<5} {clip(state['model'], 7):<7} {tokens:<6} {clip(state['title'], 28):<28} {clip(state['now'], 32)}"
          pane = {
              "source": source,
              "title": title,
              "visible_rows": 2,
              "lines": [
                  "MODE TIME  MODEL   TOKENS TITLE                        NOW",
                  row,
              ],
          }
      print(json.dumps({"type": "pane", "pane": pane}), flush=True)

  emit_pane()

  proc = subprocess.Popen(
      ["bone"] + args,
      stdout=subprocess.PIPE,
      stderr=subprocess.PIPE,
      text=True,
      bufsize=1,
  )

  for line in proc.stdout:
      line = line.rstrip("\n")
      print(line, flush=True)  # forward original agent event
      try:
          event = json.loads(line)
      except Exception:
          continue

      typ = event.get("type")
      if typ == "started":
          state["approval"] = event.get("approval") or state["approval"]
          state["mode"] = short_mode(state["approval"])
          state["model"] = event.get("model") or state["model"]
          state["task"] = event.get("task") or state["task"]
          state["title"] = short_title(state["task"])
          state["now"] = "thinking"
      elif typ == "status":
          state["now"] = event.get("message") or "running"
      elif typ == "tool_call":
          name = event.get("name") or "tool"
          summary = event.get("summary") or ""
          state["now"] = f"{name} {summary}".strip()
      elif typ == "token_usage":
          state["sent"] = event.get("sent") or 0
          state["received"] = event.get("received") or 0
      elif typ == "finished":
          state["now"] = "done"
      elif typ == "failed":
          state["now"] = "failed: " + str(event.get("message") or "error")

      emit_pane()

  stderr = proc.stderr.read()
  code = proc.wait()
  if stderr:
      print(stderr, file=sys.stderr, end="")

  emit_pane(remove=True)
  sys.exit(code)
  PY
```

## Caveats

1. This introduces a `python3` dependency for the default `subagent` tool.
   - If that is not acceptable, implement a small `bone panel-wrap` helper in Rust later.

2. `source = subagent-${mode}` can collide when several same-mode subagents run at once.
   - Fix by passing tool call id into dynamic tool env as `TOOL_CALL_ID`.
   - Then source should be `subagent-${TOOL_CALL_ID}`.

3. A single `subagent` tool cannot produce a combined 50-agent table.
   - For that, create a separate orchestrator tool, e.g. `subagents.yaml` or `swarm.yaml`, that launches many agents and owns one pane source.

## Best follow-up before editing YAML

Add `TOOL_CALL_ID` support to dynamic tools.

Rust currently builds env from arguments only. Add call id to script env in the live execution path so tool scripts can uniquely name panes.

Potential API adjustment:

- pass `call_id` into `execute_output_live`, or
- add it to `ToolLiveEvent`/execution context, or
- have `ToolRegistry::execute_live` inject it if the tool is dynamic

Simplest likely change:

```rust
async fn execute_output_live(
    &self,
    arguments: Value,
    events: Option<mpsc::UnboundedSender<ToolLiveEvent>>,
    context: ToolExecutionContext,
)
```

Where:

```rust
struct ToolExecutionContext {
    call_id: String,
}
```

Then dynamic tools include:

```text
TOOL_CALL_ID=<call id>
```

If you skip this, use `source: subagent-${mode}` temporarily.

## Done criteria

- `defaults/tools/subagent.yaml` emits live pane events.
- The pane updates while the subagent runs.
- The original `bone agent --events` JSONL is still forwarded so final content works.
- The pane is removed on completion, or left with final status intentionally.
- `cargo fmt` and `cargo check` pass after any Rust changes.
- Manual test: ask Bone to run a subagent and observe the bottom pane updating from tool-emitted `type: pane` events.
