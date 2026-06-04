# Subagent Live State Cleanup Refactor Plan

## Goal

Make subagent panel cleanup robust without hardcoding subagent-specific cleanup logic in Rust.

The subagent tool should continue to own subagent-specific behavior. Rust should only provide a generic lifecycle mechanism for dynamic tools that emit ephemeral live state.

## Problem

Subagents currently emit live pane state into the Rust panel and are expected to emit a final remove event when complete. If the wrapper crashes, is killed, times out, or exits before emitting removal, stale subagent rows can remain in the panel.

Current cleanup depends too much on child-process cooperation.

## Desired Design

Add a generic dynamic-tool live-state cleanup policy.

Example for `defaults/tools/subagent.yaml`:

```yaml
live_state:
  cleanup: on_finish
```

Default behavior for all other tools:

```yaml
live_state:
  cleanup: tool_managed
```

Meaning:

- `tool_managed`: the tool is responsible for cleanup. This preserves current behavior for persistent tools like task lists.
- `on_finish`: Rust tracks stateful live entries emitted by this tool call and removes them when the tool execution ends, regardless of success, failure, timeout, or cancellation.

Rust should not special-case `subagent`, `subagents`, or any specific pane source.

## Scope

### In scope

- Add dynamic tool config for live state cleanup.
- Track stateful live keys emitted during each dynamic tool execution.
- Automatically remove tracked keys for tools configured with `cleanup: on_finish`.
- Keep subagent-specific launch/status logic inside `subagent.yaml`.
- Keep Python-side remove events as a best-effort optimization.
- Prevent final JSONL result parsing from resurrecting stateful live pane events.
- Add regression tests for stale state cleanup.

### Out of scope

- Rewriting the subagent tool in Rust.
- Hardcoding cleanup for the `subagent` tool name.
- Changing task list persistence semantics.
- Adding lingering completed rows unless explicitly requested later.

## Implementation Plan

### 1. Add live state config types

In `src/tools/dynamic.rs`, add config fields to `DynamicTool`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LiveStateConfig {
    #[serde(default)]
    pub cleanup: LiveStateCleanup,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LiveStateCleanup {
    #[default]
    ToolManaged,
    OnFinish,
}
```

Then add to `DynamicTool`:

```rust
#[serde(default)]
pub live_state: LiveStateConfig,
```

### 2. Configure subagent as ephemeral live state

Update `defaults/tools/subagent.yaml`:

```yaml
live_state:
  cleanup: on_finish
```

Do not add this to `task_list`.

### 3. Track live state keys generically

`DynamicTool::execute_live` already tracks active state keys via parsed `ToolLiveEvent::StateUpdate` and `ToolLiveEvent::StateRemove`.

Keep this generic tracking by `(source, sub_key)`.

No code should check:

```rust
tool.name == "subagent"
```

or:

```rust
source == "subagents"
```

### 4. Add guaranteed cleanup for `on_finish`

Create a small guard or equivalent cleanup wrapper in `DynamicTool::execute_live`.

Behavior:

- If `self.live_state.cleanup == LiveStateCleanup::OnFinish`, cleanup all tracked state keys when execution ends.
- Cleanup should happen on:
  - successful exit;
  - non-zero exit;
  - timeout;
  - script runner error;
  - early return;
  - future drop/cancellation where possible.

Preferred shape:

```rust
struct LiveStateGuard {
    sender: Option<tokio::sync::mpsc::UnboundedSender<ToolLiveEvent>>,
    keys: Arc<Mutex<HashSet<(String, String)>>>,
    cleanup_on_drop: bool,
}

impl Drop for LiveStateGuard {
    fn drop(&mut self) {
        if !self.cleanup_on_drop {
            return;
        }
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        let keys = self.keys.lock().unwrap_or_else(|p| p.into_inner());
        for (source, sub_key) in keys.iter() {
            let _ = sender.send(ToolLiveEvent::StateRemove {
                source: source.clone(),
                sub_key: sub_key.clone(),
            });
        }
    }
}
```

Duplicate remove events must remain harmless.

### 5. Keep script-side cleanup as best effort

In `defaults/tools/subagent.yaml`, wrap the Python subprocess logic in `try/finally` so it attempts to emit:

```python
emit_pane(remove=True)
```

This is not the correctness mechanism; it is a fast-path cleanup if the wrapper exits normally.

### 6. Avoid final-result pane resurrection

Update `parse_jsonl_events` so stateful live pane events are not treated as final result panes.

Recommended behavior:

```rust
if event["type"].as_str() == Some("pane") {
    if event.get("state_key").is_none() {
        explicit_pane_page = pane_page_from_value(&event);
    }
    continue;
}
```

This preserves legacy non-stateful JSONL panes while ignoring stateful live panes that are already handled through live events.

### 7. Preserve task list behavior

Task lists should remain persistent.

They should not opt into:

```yaml
live_state:
  cleanup: on_finish
```

The cleanup guard should only remove keys seen through live `ToolLiveEvent::StateUpdate` for that tool execution. It should not remove normal `ToolResult.state` stored under the default session key.

### 8. Add tests

Add tests covering:

1. A JSONL dynamic tool with `live_state.cleanup: on_finish` emits a state update and exits successfully without remove; Rust removes the state.
2. Same tool exits non-zero without remove; Rust removes the state.
3. Same tool times out without remove; Rust removes the state.
4. A tool with default `tool_managed` emits a state update and exits without remove; Rust does not auto-remove it.
5. Stateful JSONL pane events are ignored by final result parsing and do not resurrect a removed pane.
6. Two parallel ephemeral tools emit different state keys; if one ends, only its keys are removed.
7. Existing task-list tests continue to pass unchanged.

## Acceptance Criteria

- No Rust code special-cases subagent names or the `subagents` pane source for cleanup.
- `subagent.yaml` declares ephemeral live-state cleanup via config.
- Stale subagent rows are removed when the dynamic tool execution ends, even if the Python wrapper does not emit remove.
- Task lists remain persistent and are unaffected.
- Duplicate remove events are safe.
- Existing tests pass.
- New cleanup regression tests pass.

## Future Optional Enhancements

- Add a generic TTL for stale live state entries as a final safety net.
- Add configurable linger time for completed ephemeral rows:

```yaml
live_state:
  cleanup: on_finish
  finish_delay_ms: 1000
```

- Add docs for dynamic-tool live state lifecycle semantics.
