# ctx.keymap — Unified Keymap Context System

## Problem

The keymap system is scattered across multiple layers with no unified Lua-accessible surface:

- **`keymap.rs`**: `handle_keymap_action()` only handles 4 hardcoded actions (`toggle_panes`, `cycle_approval_mode`, `cursor_to_start`, `cursor_to_end`)
- **`api.rs`**: `bone.api.keymap.set/del/get` — runtime Lua keymap table mutation (no action execution)
- **`mod.rs` `handle_key()`**: checks Lua keymap → falls through to `InputAction` dispatch (not Lua-accessible)
- **`input.rs`**: `InputAction` enum — `Redraw`, `Submit`, `Cancel`, `ClearQueue`, `CycleMode`, `Escape`, `OpenEditor`
- **`stream/mod.rs` `drain_keys()`**: processes keys during tool execution (not Lua-accessible)
- **`ctx.rs`**: no keymap surface at all

The agent has zero ability to control the TUI's input pipeline from Lua tools, commands, or hooks.

## Goal

A single `ctx.keymap` table that consolidates all keymap operations into one unified context object, usable from tools, commands, and hooks.

## API Design

```lua
-- Inject text into the input buffer
ctx.keymap.send("hello world")

-- Trigger a named action
ctx.keymap.action("submit")       -- sends the input buffer
ctx.keymap.action("cancel")       -- cancels streaming / interrupts
ctx.keymap.action("clear_queue")  -- clears the message queue
ctx.keymap.action("cycle_mode")   -- cycles approval mode
ctx.keymap.action("open_editor")  -- opens the system editor
ctx.keymap.action("toggle_panes") -- toggles pane visibility
ctx.keymap.action("cursor_start") -- moves cursor to start
ctx.keymap.action("cursor_end")   -- moves cursor to end

-- Manage keymap bindings (mutates live bone.keymap)
ctx.keymap.bindings("n")          -- get normal-mode bindings table
ctx.keymap.bindings.set("n", "alt-enter", "submit")
ctx.keymap.bindings.del("n", "alt-enter")

-- Query input state
ctx.keymap.state()                -- returns { buffer, cursor_pos, history_len, streaming, has_queue }
```

## Architecture

```
Lua tool/command/hook
       │
       ▼
  ctx.keymap.send("text")
  ctx.keymap.action("submit")
  ctx.keymap.bindings("n")
  ctx.keymap.state()
       │
       ▼
  mpsc::unbounded_channel<KeymapEvent>
       │
       ▼
  TUI event loop (drive_live / handle_key)
       │
       ▼
  App state mutation + redraw
```

The channel is the bridge between the Lua context (which runs inside the agent's turn loop) and the TUI event loop (which owns `App` state). Same pattern as `pane_sender` for panes/interactions.

## Files to Change

### 1. `src/tools/types.rs` — New event type

```rust
/// Events sent from Lua tools/commands/hooks to the TUI via the keymap channel.
#[derive(Debug, Clone)]
pub enum KeymapEvent {
    /// Inject text into the input buffer at the current cursor position.
    SendText(String),
    /// Trigger a named keymap action.
    Action(String),
    /// Get bindings for a mode (response sent via oneshot).
    GetBindings { mode: String, reply: oneshot::Sender<LuaKeymapSnapshot> },
    /// Set a binding.
    SetBinding { mode: String, key: String, action: String },
    /// Delete a binding.
    DeleteBinding { mode: String, key: String },
    /// Get input state (response sent via oneshot).
    GetState { reply: oneshot::Sender<KeymapState> },
}

/// Snapshot of input state for ctx.keymap.state().
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeymapState {
    pub buffer: String,
    pub cursor_pos: usize,
    pub history_len: usize,
    pub streaming: bool,
    pub queue_len: usize,
    pub approval_mode: String,
    pub panes_visible: bool,
}
```

Extend `ToolLiveEvent`:
```rust
pub enum ToolLiveEvent {
    Pane(PaneContent),
    Interact(InteractRequest),
    Keymap(KeymapEvent),  // NEW
}
```

### 2. `src/ext/ctx.rs` — Add keymap surface to ctx

Add `keymap_tx` field to `CtxConfig`:
```rust
pub(crate) struct CtxConfig {
    // ... existing fields ...
    pub keymap_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::tools::types::KeymapEvent>>,
}
```

Create `ctx.keymap` table in `create_ctx_table()`:
```rust
// ctx.keymap — unified keymap control
let keymap_table = lua.create_table()?;

if let Some(km_tx) = cfg.keymap_tx.clone() {
    // ctx.keymap.send(text)
    let send_fn = lua.create_function(move |_, text: String| {
        km_tx.send(KeymapEvent::SendText(text))
            .map_err(|e| mlua::Error::external(format!("keymap send failed: {e}")))
            .map(|_| true)
    })?;
    keymap_table.set("send", send_fn)?;

    // ctx.keymap.action(name)
    let action_fn = lua.create_function(move |_, name: String| {
        km_tx.send(KeymapEvent::Action(name))
            .map_err(|e| mlua::Error::external(format!("keymap action failed: {e}")))
            .map(|_| true)
    })?;
    keymap_table.set("action", action_fn)?;

    // ctx.keymap.bindings(mode) → table
    let bindings_get = lua.create_function(move |lua, mode: String| {
        km_tx.send(KeymapEvent::GetBindings {
            mode,
            reply: /* oneshot */,
        })
        // ... await reply, convert to Lua table ...
    })?;
    keymap_table.set("bindings", bindings_get)?;

    // ctx.keymap.state() → table
    let state_fn = lua.create_function(move |lua, _: ()| {
        km_tx.send(KeymapEvent::GetState {
            reply: /* oneshot */,
        })
        // ... await reply, convert to Lua table ...
    })?;
    keymap_table.set("state", state_fn)?;
} else {
    // Non-TUI context (headless): stub functions that return false/error
    let stub = lua.create_function(|_, _: ()| Ok((false, "keymap unavailable")))?;
    keymap_table.set("send", stub.clone())?;
    keymap_table.set("action", stub.clone())?;
    keymap_table.set("bindings", stub.clone())?;
    keymap_table.set("state", stub)?;
}

ctx.set("keymap", keymap_table)?;
```

### 3. `src/ui/app/mod.rs` — Wire keymap_tx into CtxConfig

When building `CtxConfig` for tool invocations, pass the keymap channel:
```rust
let mut cfg = CtxConfig::new(/* ... */);
cfg.keymap_tx = self.keymap_tx.clone();  // NEW
```

Add `keymap_tx` field to `App`:
```rust
pub struct App {
    // ... existing fields ...
    keymap_tx: Option<tokio::sync::mpsc::UnboundedSender<KeymapEvent>>,
}
```

Process `KeymapEvent` in `handle_key()` (after Lua keymap lookup, before InputAction dispatch):
```rust
// Process any pending keymap events from Lua tools
if let Some(ref tx) = self.keymap_tx {
    // Drain and apply keymap events
    while let Ok(event) = rx.try_recv() {
        self.apply_keymap_event(event);
    }
}
```

Add `apply_keymap_event()`:
```rust
fn apply_keymap_event(&mut self, event: KeymapEvent) {
    match event {
        KeymapEvent::SendText(text) => {
            self.input.insert_text(&text);
            self.redraw(term).ok();
        }
        KeymapEvent::Action(name) => {
            self.handle_keymap_action(name).ok();
        }
        KeymapEvent::GetBindings { mode, reply } => {
            let snapshot = self.lua_keymap.clone();
            let _ = reply.send(/* bindings for mode */);
        }
        KeymapEvent::GetState { reply } => {
            let state = KeymapState {
                buffer: self.input.buffer.clone(),
                cursor_pos: self.input.cursor_pos,
                history_len: self.input.history.len(),
                streaming: self.streaming,
                queue_len: self.queue.len(),
                approval_mode: format!("{:?}", self.approval_mode),
                panes_visible: self.panes_visible,
            };
            let _ = reply.send(state);
        }
        KeymapEvent::SetBinding { mode, key, action } => {
            // Mutate live bone.keymap table
            let km = self.extensions.keymap_snapshot_live();
            // ... set binding ...
        }
        KeymapEvent::DeleteBinding { mode, key } => {
            // ... delete binding ...
        }
    }
}
```

### 4. `src/ui/app/stream/mod.rs` — Process KeymapEvent in drive_live

In `drive_live()`, process `KeymapEvent` alongside `ToolLiveEvent`:
```rust
loop {
    tokio::select! {
        results = &mut future => {
            // Drain all event types
            while let Ok(event) = rx.try_recv() {
                match event {
                    ToolLiveEvent::Pane(pc) => { /* existing */ }
                    ToolLiveEvent::Interact(req) => { /* existing */ }
                    ToolLiveEvent::Keymap(km) => self.apply_keymap_event(km),
                }
            }
            return Ok(results);
        }
        Some(event) = rx.recv() => {
            match event {
                ToolLiveEvent::Pane(pc) => { /* existing */ }
                ToolLiveEvent::Interact(req) => { /* existing */ }
                ToolLiveEvent::Keymap(km) => self.apply_keymap_event(km),
            }
        }
        // ... spinner tick, drain_keys, cancel handling (existing) ...
    }
}
```

### 5. `src/ext/types.rs` — Expose keymap_tx for hooks

Add `keymap_tx` to `ExtensionManager`:
```rust
pub struct ExtensionManager {
    // ... existing fields ...
    keymap_tx: Option<tokio::sync::mpsc::UnboundedSender<KeymapEvent>>,
}

impl ExtensionManager {
    pub fn keymap_tx(&self) -> Option<&tokio::sync::mpsc::UnboundedSender<KeymapEvent>> {
        self.keymap_tx.as_ref()
    }
}
```

Hooks can then use it via `bone._manager.keymap_tx` or a dedicated method. The `before_turn` hook has full `ctx` access and can call `ctx.keymap.action()` if the channel is available.

## Non-TUI Contexts (headless / RPC)

When `keymap_tx` is `None` (headless agent, RPC server without TUI frontend), the `ctx.keymap` methods return `(false, "keymap unavailable")`. This is consistent with how `ctx.ui.pane` and `ctx.ui.interact` already degrade gracefully.

## Edge Cases

1. **Concurrent keymap events**: `mpsc::UnboundedSender` handles concurrent sends from multiple tools in the same turn. Events are processed in order.

2. **Blocking on oneshot replies**: `GetBindings` and `GetState` use `oneshot::Sender` for synchronous responses. The TUI processes the event and sends the reply immediately. The Lua caller blocks on `rx.await` (via `block_in_place`).

3. **Keymap events during interactive panes**: `apply_keymap_event()` runs in the same context as `drain_keys()`. If an interactive pane is active, keymap events are processed but don't interfere with the pane's own key handling.

4. **Cancellation**: If the user cancels during a tool that's using `ctx.keymap`, the `drive_live` loop exits and any pending keymap events are dropped. This is correct — the tool is cancelled.

5. **Depth limits**: `ctx.keymap.action("submit")` does not trigger a new tool invocation — it just mutates App state. No depth limit needed.

## Order of Operations

In `handle_key()` / `drive_live`, keymap events from Lua tools should be processed:

1. After interactive pane key interception
2. After Lua keymap lookup (so Lua bindings still take priority for raw key events)
3. Before InputAction dispatch (so Lua tools can override default behavior)

This means a Lua tool's `ctx.keymap.action("submit")` will submit the current input buffer, even if the agent was in the middle of a turn.

## Testing

- Unit tests for `KeymapEvent` serialization
- Integration test: Lua tool calls `ctx.keymap.send("text")` → verify input buffer
- Integration test: Lua tool calls `ctx.keymap.action("cancel")` → verify streaming cancelled
- Graceful degradation test: headless context returns `(false, "keymap unavailable")`
