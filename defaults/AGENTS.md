# Bone Agent Reference

## Config Location

All file paths below are relative to the bone config directory. The resolved path is provided in the system prompt under "Resolved config directory".

```
init.lua              — Lua configuration and customization (optional)
lua/tools/            — Custom Lua tools
lua/commands/         — Custom Lua commands
lua/plugins/          — Lua plugins (optional)
providers.yaml        — LLM provider entries
command-policy.yaml   — Shell command safety tiers
memory.md             — User preferences (auto-maintained by /memory)
```

After editing `providers.yaml` or `command-policy.yaml`, tell the user to restart bone.

## Lua Extension System

Bone embeds Lua 5.4 for tool, command, config, theme, and keymap customization. If `init.lua` exists in the config directory, it runs at startup. If missing, bone behaves exactly as before.

### `init.lua` Location

```
~/.bone-rust/init.lua
```

Errors in `init.lua` are logged as warnings; bone continues without Lua support.

### `bone` Global API

```lua
-- Metadata (read-only)
bone.version        -- string: app version
bone.cwd            -- string: startup CWD
bone.config_dir     -- string: config directory path

-- Logging (outputs to stderr)
bone.log.info("message")
bone.log.warn("message")
bone.log.error("message")

-- Tool registration
bone.register_tool({ ... })

-- Command registration
bone.register_command("name", { description = "...", handler = function(args, ctx) ... end })
bone.register_command("name", function(args, ctx) ... end)  -- short form

-- Event hooks
bone.on("event_name", function(event, ctx) ... end)
```

### `cjson` Global

A `cjson` global is available for JSON encoding/decoding:
```lua
local json_str = cjson.encode({ key = "value" })
local table = cjson.decode(json_str)
```

### `ctx` API (passed to tool/command handlers)

```lua
ctx.cwd                  -- string: working directory
ctx.config_dir           -- string: config directory path

-- Shell execution (reuses native approval + policy)
ctx.shell(command, opts)
  -- opts: { timeout_ms = 120000 }  (min 1000, max 300000)
  -- returns: { stdout = "", stderr = "", exit_code = 0 }

-- File operations
ctx.read_file(path)             -- returns content string or nil
ctx.write_file(path, content)   -- returns true or nil (fails if file exists)

-- UI notifications
ctx.ui.notify(message, level)   -- level: "info" | "warn" | "error"

-- Session state (persists across calls within a session)
ctx.state.get(key)              -- returns string or nil
ctx.state.set(key, value)       -- value: string
ctx.state.clear(key)
```

## Pre-Seeded Tools

These tools are bundled with bone and seeded to `lua/tools/` on first launch.

### shell (native Rust)
Run a non-interactive shell command with bash -lc. Returns exit code, stdout, and stderr.
```lua
-- Native Rust tool, not Lua. Called by the LLM directly.
-- Parameters: command (string, required), classification (string: "read_only" or "danger"), timeout_ms (integer, optional)
```

### read_file (native Rust)
Read a UTF-8 text file. Optionally pass start_line and max_lines to read a range.
```lua
-- Native Rust tool. Parameters: path, start_line?, max_lines?
```

### write_file (native Rust)
Create a new UTF-8 text file. Fails if the file already exists.
```lua
-- Native Rust tool. Parameters: path, content
```

### edit_file (native Rust)
Edit an existing UTF-8 file. Use search+replace, edits[], or mode="rewrite".
```lua
-- Native Rust tool. Parameters: path, search?, replace?, edits?, mode?, content?, expected_hash?
```

### web_search (Lua)
Search the web via DuckDuckGo. Returns titles, URLs and summaries. Requires `uv` and the `ddgs` Python package.
```lua
bone.register_tool({
    name = "web_search",
    description = "Search the web for information using DuckDuckGo...",
    parameters = {
        type = "object",
        properties = {
            query = { type = "string", description = "The search query" },
            num_results = { type = "number", description = "Number of results (default 5, max 10)" },
        },
        required = { "query" },
        additionalProperties = false,
    },
    safety = "read_only",
})
```

### ask_user (Lua, interaction)
Ask the user a question with selectable options or a custom answer.
```lua
bone.register_tool({
    name = "ask_user",
    description = "Ask the user a question with selectable options or a custom answer",
    parameters = {
        type = "object",
        properties = {
            question = { type = "string", description = "The question to ask" },
            options = { type = "array", items = { type = "string" }, description = "List of options" },
            allow_custom = { type = "boolean", description = "Whether the user can type their own answer" },
        },
        required = { "question", "options" },
        additionalProperties = false,
    },
    safety = "read_only",
    display = { show = false, args = { "question" } },
})
```

### task_list (Lua, session state, TUI pane)
Manage a named visible task list with TUI pane rendering.
```lua
bone.register_tool({
    name = "task_list",
    description = "Manage a named visible task list. State is held by the host; no state arg needed. Actions: create (pass texts and optional name, max 15 tasks), complete (pass index/indices), kill.",
    safety = "read_only",
    parameters = {
        type = "object",
        properties = {
            action = { type = "string", description = "create, complete, or kill" },
            name = { type = "string", description = "Optional task list name for create." },
            texts = { type = "array", items = { type = "string" }, description = "Task strings for create." },
            index = { type = "number", description = "Single 1-based task index for complete." },
            indices = { type = "array", items = { type = "number" }, description = "Multiple 1-based task indices for complete." },
        },
        required = { "action" },
        additionalProperties = false,
    },
    display = { show = false, show_result = false, args = { "action", "name", "texts", "index", "indices" } },
})
```

### cron (Lua)
Manage scheduled bone jobs via crontab. Requires `crontab`, `uv`, and Python.
```lua
bone.register_tool({
    name = "cron",
    description = "Manage Bone scheduled jobs for the user...",
    safety = "danger",
    parameters = {
        type = "object",
        properties = {
            action = { type = "string", description = "add, list, remove, logs, help" },
            name = { type = "string", description = "Job name (letters, numbers, '-' or '_')" },
            time = { type = "string", description = "HH:MM 24-hour local time" },
            approval = { type = "string", description = "safe or danger. Defaults to safe." },
            prompt = { type = "string", description = "Prompt or command invocation for add." },
            cwd = { type = "string", description = "Working directory for add." },
            tail = { type = "number", description = "Number of log lines for logs." },
        },
        required = { "action" },
        additionalProperties = false,
    },
})
```

## Pre-Seeded Commands

### /memory
Incremental memory builder. Processes all conversations since last run and updates `memory.md`. If `memory.md` exists in the config directory, its contents are loaded into every conversation's system prompt.

Run manually with `/memory`, or schedule daily:
```
cron(action=add, name=memory, time=03:00, approval=danger, prompt=/memory)
```

Disable by removing `lua/commands/memory.lua` from the config directory.

## Creating Custom Tools

Tools are Lua files in `lua/tools/` that call `bone.register_tool()`. The agent calls them as typed functions with args, and they return a string to the agent.

### Minimal Tool

```lua
bone.register_tool({
    name = "my_tool",
    description = "Short description of what the tool does and when to use it.",
    parameters = {
        type = "object",
        properties = {
            query = {
                type = "string",
                description = "What this arg is for",
            },
        },
        required = { "query" },
        additionalProperties = false,
    },
    safety = "read_only",
    execute = function(params, ctx)
        local result = ctx.shell("some-command " .. params.query)
        return result.stdout
    end,
})
```

### Tool Fields

- **name** — unique string identifier. Native tools (`shell`, `read_file`, `write_file`, `edit_file`) cannot be overridden.
- **description** — shown to the LLM when deciding which tool to call.
- **parameters** — JSON Schema object describing the tool's arguments.
- **safety** — `"read_only"` or `"danger"`. In safe mode only `read_only` tools auto-run; in danger mode everything auto-runs.
- **display** — optional table controlling TUI visibility:
  ```lua
  display = {
      show = true,           -- show a pane for this tool
      show_result = true,    -- show the result in the pane
      args = { "action" },   -- which arg values to display
      template = "{action}", -- format string for pane title
  }
  ```
- **execute** — `function(params, ctx) -> string`. The function body. Returns the tool result string. Errors are caught and returned as tool errors to the LLM.

### Tool Output

- **Default:** the string returned from `execute` is sent to the agent as the tool result.
- **JSON envelope (for TUI panes):** return a JSON-encoded object with `content`, optional `state`, and optional `pane`:
  ```lua
  local output = {
      content = "Result text for the agent",
      state = cjson.encode(state_table),    -- persisted across calls
      pane = {
          source = "my_tool",
          title = "My Tool",
          visible_rows = 8,
          scroll = 0,
          lines = {
              { spans = { { text = "Label: ", fg = "dark_gray" }, { text = "value", fg = "white" } } },
          },
      },
  }
  return cjson.encode(output)
  ```
  Pane span fields: `text` (required), `fg` (optional), `bg` (optional), `modifiers` (optional array: `"bold"`, `"dim"`, `"italic"`, `"underline"`, `"strike"`). Colors: `"white"`, `"dark_gray"`, `"green"`, `"red"`, `"yellow"`, `"blue"`, `"cyan"`, `"magenta"`.

### Session State

Tools that need to remember data between invocations use `ctx.state`:

1. Your tool calls `ctx.state.set("key", serialized_state)`.
2. The host stores it for the session.
3. On the next call, `ctx.state.get("key")` returns the stored string.
4. State does not persist across bone restarts.

## Creating Custom Commands

Commands are slash-commands (`/name args`) registered via `bone.register_command()`. They live as Lua files in `lua/commands/`.

### Long Form

```lua
bone.register_command("deploy", {
    description = "Deploy current project",
    handler = function(args, ctx)
        local result = ctx.shell("./deploy.sh " .. args)
        if result.exit_code ~= 0 then
            ctx.ui.notify(result.stderr, "error")
            return nil
        end
        return result.stdout
    end,
})
```

### Short Form

```lua
bone.register_command("hello", function(args, ctx)
    return "Hello " .. args
end)
```

### Command Return Semantics

| Return | Behavior |
|---|---|
| `nil` | Command handled, no prompt submitted |
| string | Injected as user prompt/output |
| table with `display`/`reply`/`content` and `submit = false` | Show message in UI without submitting a prompt |
| error | Show error in UI |

Protected built-ins (`/help`, `/quit`, `/exit`, `/new`, `/clear`, `/compact`, `/model`, `/provider`, `/config`, `/tools`, `/edit`, `/e`, `/stats`) cannot be overridden.

## Event Hooks

Register callbacks for lifecycle events via `bone.on()`:

```lua
-- Block dangerous shell commands
bone.on("tool_call", function(event, ctx)
    if event.name == "shell" and event.arguments.command:find("rm %-rf") then
        return { block = true, reason = "blocked by Lua policy" }
    end
end)

-- Observe tool failures
bone.on("tool_result", function(event, ctx)
    if event.is_error then
        ctx.ui.notify("tool failed: " .. event.name, "warn")
    end
end)
```

### Events

| Event | When | Blockable |
|---|---|---|
| `session_start` | New session starts | no |
| `session_end` | Session ends | no |
| `message` | LLM/user message observed | no |
| `tool_call` | Before tool execution | **yes** |
| `tool_result` | After tool execution | no |
| `mode_change` | Approval mode changes | no |

Handlers run in registration order. First `block` stops the chain. Handler runtime errors do not block (fail-open).

## Config, Theme, and Keymaps

Set these in `init.lua`. Rust snapshots them once at boot; no per-frame Lua reads.

### Config

```lua
bone.config = {
    approval_mode = "safe",               -- "safe" | "danger"
    auto_compact_tokens = 8000,           -- token threshold for auto-compact
    auto_compact_keep_messages = 12,      -- messages to keep after compact
    status_show = {
        model = true, approval = true, tokens_curr = true,
        tokens_in = true, tokens_out = true, tokens_total = true,
        tps = true, queue = true, spinner = true, timer = true,
    },
}
```

Invalid values warn and fall back to Rust defaults. `init.lua` is the source of truth.

### Theme

```lua
bone.theme = {
    user_msg = "#ffffff",
    user_msg_bg = "#303030",
    status_text = "#808080",
    input_border = "#808080",
    system_msg = "#ffffff",
    approval_safe = "#78b373",
    approval_danger = "#e05050",
    tool_call = "#808080",
    tool_error = "#ff0000",
    diff_removed = "#870101",
    diff_added = "#005f00",
    thinking = "#8cdcdc",
    tab_active = "#00ffff",
}
```

Colors: hex (`#RRGGBB`) or named (`white`, `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`, `darkgray`, `lightred`, `lightgreen`, `lightyellow`, `lightblue`, `lightmagenta`, `lightcyan`). Missing keys fall back to Rust defaults.

### Keymaps

```lua
bone.keymap = {
    n = {
        ["<C-p>"] = "toggle_panes",
        ["<S-Tab>"] = "cycle_approval_mode",
    },
    i = {
        ["<C-a>"] = "cursor_to_start",
        ["<C-e>"] = "cursor_to_end",
    },
}
```

Modes: `n` (normal), `i` (insert). Values are built-in action names. Unknown actions are ignored with a warning.

## Plugin System

Plugins live in `lua/plugins/<name>/init.lua`. They must be loaded explicitly from `init.lua`:

```lua
bone.plugin.load("tokyonight")
bone.plugin.install("user/repo")      -- git clone
bone.plugin.install("/local/path")    -- symlink or copy
bone.plugin.remove("tokyonight")
bone.plugin.list()
bone.plugin.update("tokyonight")
```

Plugins do not auto-run. Repeated `load` is a no-op.

## File Layout

```
~/.bone-rust/
  init.lua                     -- main Lua config (optional)
  providers.yaml               -- LLM providers (Rust-managed)
  command-policy.yaml           -- shell command safety (Rust-managed)
  memory.md                    -- user preferences (auto-maintained)
  memory.last_run              -- /memory checkpoint timestamp
  AGENTS.md                    -- this reference file
  config/
    general.yaml               -- general settings (approval mode, status bar)
    tools.yaml                 -- tool enable/disable toggles
  lua/
    tools/
      web_search.lua           -- seeded default
      ask_user.lua             -- seeded default
      task_list.lua            -- seeded default
      cron.lua                 -- seeded default
      my_custom_tool.lua       -- user-created
    commands/
      memory.lua               -- seeded default
      my_custom_command.lua    -- user-created
    plugins/
      tokyonight/
        init.lua
```

Default Lua files are seeded on first launch and never overwrite existing files.

## Tool vs Command

- **Tool** — The LLM calls as a function with typed args. Returns a string result. Good for integrations, searches, state management, TUI panes.
- **Command** — User invokes `/name [args]`. Returns a string injected as prompt. Good for workflows, reviews, templates, content generation.
