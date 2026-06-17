# Plan: Migrate Config Picker to Lua

## Goal
Move the config picker UI from Rust (`config_picker.rs`) to a Lua command, eliminating ~300 lines of blocking event loop + prompt rendering code. The config UI uses the live pane system (`ctx.ui.interact` / `ctx.ui.pane`) like all other Lua commands.

## What Stays Rust
- Provider types (`OpenAiCompatProvider`, `CodexProvider`) and `LlmProvider` trait
- `create_provider_with_config` — constructs providers from config
- `boot_with_tools` — boots extensions, registers tools, builds `ToolHandler`
- Tool handler, safety checks, display logic

## What Moves to Lua
- Config picker UI (tabs, lists, editing, cycling)
- Provider selection & switching flow
- Tool reload invocation
- All prompt rendering (uses live pane instead of `active_prompt`)

## Steps

### 1. Expose context helpers on the Lua `ctx` table
Add 4 functions to the context that Lua commands can call:

| Function | Rust impl | Purpose |
|---|---|---|
| `ctx.config.get_pages()` | Reads `custom.pages` | List available config pages |
| `ctx.config.get_value(ns, key)` | `custom.get_value()` | Read a field's current value |
| `ctx.config.set_value(ns, key, val)` | `custom.set_value()` + `apply_custom_configs_to_runtime` | Save a field, apply to runtime |
| `ctx.config.cycle_field(ns, key, current)` | `custom.cycle_field()` | Cycle enum/bool values |
| `ctx.config.list_providers()` | `sorted_provider_ids` + derive config | List providers with labels/models |
| `ctx.config.create_provider(id)` | `create_provider_with_config` | Create a provider instance |
| `ctx.config.validate_provider(id)` | `provider.validate()` | Validate a provider (async) |
| `ctx.config.set_active_provider(id)` | Sets `self.llm`, `self.provider`, `self.model` | Switch active provider |
| `ctx.config.reload_tools()` | `boot_with_tools` | Re-boot extensions + tool handler |

### 2. Write the Lua config command
File: `~/.bone-rust/commands/config.lua`

```lua
function _bone.register()
  return { name = "config", description = "edit configuration" }
end

function _bone.handle(_, ctx)
  -- Load pages from config
  -- Render tab bar as a pane
  -- Use ctx.ui.interact for:
  --   - Tab selection
  --   - Field selection / cycling (bool/enum)
  --   - Value editing (text fields via TextInput)
  --   - Provider selection
  -- On save: call ctx.config.set_value + ctx.config.reload_tools
  -- On provider switch: call ctx.config.set_active_provider
end
```

### 3. Wire command dispatch in Rust
In `App::handle_command`:
```rust
if cmd == "config" {
    return self.run_lua_command("config", &arg, term).await;
}
```
Same pattern as `/history`.

### 4. Delete Rust code
- `src/ui/app/config_picker.rs` (entire file)
- `config_picker` method on `App`
- `provider_editor` method on `App`
- `edit_value` method on `App`
- `handle_tools_command` method on `App`
- `sorted_provider_ids` helper
- Prompt rendering for config tabs in `bottom_pane.rs` (the `full_command` / tab bar path — keep the rest)

### 5. Update `/tools` command
The `/tools` command currently calls `config_picker` for the UI and has a special `reload` subcommand. Move tool reload to Lua:
- `/tools reload` → Lua command calls `ctx.config.reload_tools()`
- `/tools` → Lua config picker with tools tab pre-selected

### 6. Update `/provider` command
Same treatment — `/provider` without args opens the config picker on the providers tab.

## Files Changed
- **Added:** `~/.bone-rust/commands/config.lua` (user config dir)
- **Modified:** `src/ext/ctx.rs` — add config helpers to context table
- **Modified:** `src/ui/app/mod.rs` — change `/config` dispatch, remove config_picker/tool_editor/edit_value/sorted_provider_ids
- **Modified:** `src/ui/render/bottom_pane.rs` — remove config tab bar rendering path
- **Deleted:** `src/ui/app/config_picker.rs`
