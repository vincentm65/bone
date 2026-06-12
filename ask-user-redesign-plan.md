# Ask User Tool Redesign — Implementation Plan

## Goal
Replace the current text-based `ask_user` tool with a rich, interactive pane-based UI that renders questions in the bottom pane, supports keyboard-driven option selection, custom text input, and multi-select — all managed from Lua.

---

## Current State

- `ask_user` (Lua) returns plain text to the LLM. The LLM reads it and the user must type a response inline.
- `PanePage` is a static display-only data structure: `source`, `title`, `content (Line[])`, `visible_rows`, `scroll`.
- The bottom pane renders pages, supports Tab cycling and scrolling — but **no input interception**.
- Tool execution uses `mpsc::unbounded_channel<ToolLiveEvent>` to stream `Pane` and `StateUpdate` events back to the TUI loop.
- `wait_for_tool_future_live` polls keys (for cancel/mode toggle) but never routes keypresses into the tool itself.

---

## Architecture Changes

### 1. `PanePage` → Add `interaction` field (Rust)

Extend `PanePage` with an optional `interaction` field:

```rust
pub struct PaneInteraction {
    /// What kind of interaction
    pub mode: InteractionMode,
    /// Currently focused/highlighted index (for single/multi select)
    pub selected: usize,
    /// Checked indices (for multi-select only)
    pub checked: Vec<bool>,
    /// Text input buffer (for text input modes)
    pub input_buffer: String,
    /// Cursor position within input_buffer (char offset)
    pub cursor_pos: usize,
    /// Whether the interaction is still active (false = user submitted)
    pub active: bool,
    /// Result channel: the Lua tool blocks on this
    pub result_tx: Mutex<Option<tokio::sync::oneshot::Sender<serde_json::Value>>>,
    /// Whether a freeform text input row is shown (allow_custom)
    pub allow_custom: bool,
    /// Whether the custom input row is currently focused
    pub custom_focused: bool,
}

pub enum InteractionMode {
    /// Select one option (Up/Down + Enter)
    SingleSelect,
    /// Select multiple options (Up/Down + Space to toggle + Enter to confirm)
    MultiSelect,
    /// Freeform text input only (no options list)
    TextInput,
}
```

**Key insight:** The interaction struct lives in `PanePage` and is shared between:
- The **TUI key handler** (writes selection state)
- The **tool execution** (blocks reading the oneshot channel)

### 2. No new `ToolLiveEvent` variant needed

The interactive pane is just a regular `ToolLiveEvent::Pane(page)` where `page.interaction.is_some()`. The TUI loop checks for the presence of an interaction to decide key routing.

### 3. TUI Key Interception for Interactive Panes

In `handle_key` / `wait_for_tool_future_live`:

- If the **active page** has `interaction.active == true`, route key events to the interaction handler instead of the normal input/prompt handler.
- Supported keys:
  - `Up/Down` — move selection cursor
  - `Space` — toggle item (multi-select)
  - `Enter` — submit selection
  - `Esc` — cancel (submit with `cancelled: true`)
  - Characters — append to `input_buffer` (in text input mode, or when custom row focused)
  - `Backspace` — delete from input buffer
- On submit: set `active = false`, send the result through `result_tx`.

### 4. Lua API: `ctx.ui.interact(opts)`

A generic interactive pane API — not tied to "asking questions". The `ask_user` tool is just one consumer.

```lua
-- Blocking call from within a Lua tool execute function.
-- Returns the user's interaction result as a Lua table.
local result = ctx.ui.interact({
    question = "Which files should I refactor?",
    type = "multi_select",     -- "single_select" | "multi_select" | "text_input"
    options = { "auth.lua", "ctx.rs", "mod.rs" },
    default = 1,               -- pre-selected option index
    allow_custom = false,      -- adds a freeform text input row
})

-- Return values by type:
-- single_select:  { value = "auth.lua" }  or  { value = "custom text", custom = true }
-- multi_select:   { values = { "auth.lua", "ctx.rs" } }  or  { values = {...}, custom = "text" }
-- text_input:     { value = "typed text" }
-- cancelled:      { cancelled = true }
```

Any Lua tool can call `ctx.ui.interact()` — it's not specific to `ask_user`. Examples:

```lua
-- Simple confirmation
local result = ctx.ui.interact({
    question = "Delete these files?",
    type = "single_select",
    options = { "Yes", "No" },
})

-- Multi-select picker
local result = ctx.ui.interact({
    question = "Select files to stage",
    type = "multi_select",
    options = { "main.rs", "lib.rs", "mod.rs" },
})

-- Freeform input
local result = ctx.ui.interact({
    question = "Branch name?",
    type = "text_input",
})

-- Single select + custom input
local result = ctx.ui.interact({
    question = "Pick a color",
    type = "single_select",
    options = { "red", "green", "blue" },
    allow_custom = true,
})
```

### 5. How It Works End-to-End

1. LLM calls `ask_user` tool.
2. Lua `execute()` calls `ctx.ui.interact(opts)`.
3. `ctx.ui.interact` (Rust ctx function):
   - Creates a `oneshot::channel()`.
   - Builds a `PanePage` with styled content + `PaneInteraction`.
   - Sends it as `ToolLiveEvent::Pane(page)`.
   - **Blocks** on `oneshot::Receiver::recv()` (via `block_in_place`).
4. TUI receives the pane event, renders the interactive page.
5. User navigates with keys. TUI updates `PaneInteraction.selected` / `.checked` / `.input_buffer`.
6. User presses Enter → TUI sends result through `oneshot::Sender`.
7. `ctx.ui.interact` unblocks, returns the result to Lua.
8. Lua returns the answer to the LLM.

---

## File Changes Summary

### Rust Changes

| File | Change |
|------|--------|
| `src/ui/pane_page.rs` | Add `PaneInteraction`, `InteractionMode`, extend `PanePage` with `interaction` field |
| `src/ext/ctx.rs` | Add `ctx.ui.interact(opts)` — creates interactive pane + blocks on oneshot |
| `src/ui/app/mod.rs` | In `handle_key`, detect interactive pane, route keys to it |
| `src/ui/app/stream/mod.rs` | In `wait_for_tool_future_live`, handle interactive pane key routing |
| `src/ui/render/bottom_pane.rs` | Render selection cursor, checkboxes, input field for interactive pages |

### Lua Changes

| File | Change |
|------|--------|
| `defaults/lua/tools/ask_user.lua` | Rewrite to call `ctx.ui.interact()` |

---

## Detailed Component Specs

### PaneInteraction (Rust struct)

```rust
/// Attached to a PanePage to make it interactive.
pub struct PaneInteraction {
    pub mode: InteractionMode,
    pub selected: usize,          // cursor position
    pub checked: Vec<bool>,       // per-option checked state (multi-select)
    pub input_buffer: String,     // freeform text
    pub cursor_pos: usize,        // char offset in input_buffer
    pub allow_custom: bool,       // show custom input row
    pub custom_focused: bool,     // custom input row has focus
    pub result_tx: Mutex<Option<tokio::sync::oneshot::Sender<serde_json::Value>>>,
}
```

### InteractionMode

```rust
pub enum InteractionMode {
    SingleSelect,     // Radio-style: pick one
    MultiSelect,      // Checkbox-style: pick N
    TextInput,        // Freeform text field (no options)
}
```

### ctx.ui.interact (Rust → Lua bridge)

Created in `create_ctx_table` under `ctx.ui.interact`:

```rust
// Pseudocode for the ctx function:
let (tx, rx) = tokio::sync::oneshot::channel();

let mode = match type_str {
    "single_select" => InteractionMode::SingleSelect,
    "multi_select"  => InteractionMode::MultiSelect,
    "text_input"    => InteractionMode::TextInput,
    _ => return Err("unknown type"),
};

// Build question lines
let lines = build_interaction_lines(&question, &options, &mode, allow_custom);

let interaction = PaneInteraction {
    mode,
    selected: default.unwrap_or(0),
    checked: vec![false; options.len()],
    input_buffer: String::new(),
    cursor_pos: 0,
    allow_custom,
    custom_focused: false,
    result_tx: Mutex::new(Some(tx)),
};

let page = PanePage {
    source: format!("interact_{}", call_id),
    title: "Question".into(),
    content: lines,
    visible_rows: computed,
    scroll: 0,
    interaction: Some(interaction),
};

pane_sender.send(ToolLiveEvent::Pane(page))?;

// Block until user answers
let result = tokio::task::block_in_place(|| {
    tokio::runtime::Handle::current().block_on(rx)
})?;

Ok(result) // return to Lua as table
```

### Rendering (bottom_pane.rs)

When `page.interaction.is_some()`:
- SingleSelect: render options with `›` cursor for current `selected`.
- MultiSelect: render `[x]` / `[ ]` before each option, `›` cursor.
- TextInput: render `> █` input field.
- When `allow_custom`: extra row at bottom labeled "Custom:" with input field.
- Active option highlighted in bold/white, inactive in dark gray.

### Key Routing (app/mod.rs + stream/mod.rs)

In the key polling loop:
```rust
if let Some(page) = self.pages.get(self.active_page) {
    if let Some(ref interaction) = page.interaction {
        if interaction.active {
            return self.handle_interactive_key(code, modifiers, term);
        }
    }
}
```

`handle_interactive_key` updates `interaction.selected`, `.checked[]`, `.input_buffer`, etc.
On Enter: build result JSON, send via `result_tx`, set `active = false`, remove pane.
On Esc: send `{ cancelled: true }`, remove pane.

---

## Phase Breakdown

### Phase 1: Core Infrastructure
- Add `PaneInteraction` and `InteractionMode` to `pane_page.rs`
- Extend `PanePage` with `interaction: Option<PaneInteraction>`
- Add key routing in `handle_key` and `wait_for_tool_future_live`

### Phase 2: Rendering
- Update `bottom_pane.rs` to render interactive elements (cursor, checkboxes, input field)
- Style consistently with existing approval prompt aesthetic

### Phase 3: ctx.ui.interact
- Implement `ctx.ui.interact()` in `ctx.rs`
- Wire oneshot channel, build pane, block on result

### Phase 4: Lua Tool Rewrite
- Rewrite `ask_user.lua` to use `ctx.ui.interact()`
- Support all question types: single, multi, text, custom

### Phase 5: Polish
- Handle edge cases (cancelled, timeout, no options)
- Remove interactive pane after answer
- Tab still cycles between subagent/task/interaction pages
