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

-- Sub-agent registration
bone.register_subagent({ name = "...", description = "...", system_prompt = "...", provider = "...", model = "...", approval = "..." })

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

### `ctx` API

A `ctx` table is passed as the second argument to tool `execute(params, ctx)` and command `handler(args, ctx)` functions. Event handlers receive a smaller `ctx` (see [Context Availability](#context-availability) below).

To edit existing files, use `ctx.tools.call("edit_file", { path = "...", search = "...", replace = "..." })` which goes through the full approval pipeline. There is no convenience `ctx.edit_file` method.

#### Reference Table

| API | Returns | Description |
|---|---|---|
| `ctx.config_dir` | `string` | Bone config directory path |
| `ctx.cwd` | `string` | Startup working directory |
| `ctx.call_id` | `string\|nil` | Current tool call's unique ID (tools only) |
| **`ctx.log.*`** | | Log to stderr |
| `ctx.log.debug(val)` | | Log at debug level |
| `ctx.log.info(val)` | | Log at info level |
| `ctx.log.warn(val)` | | Log at warn level |
| `ctx.log.error(val)` | | Log at error level |
| **`ctx.fs.*`** | | Filesystem helpers (read-only queries) |
| `ctx.fs.exists(path)` | `bool` | Path exists check |
| `ctx.fs.is_file(path)` | `bool` | Path is a regular file |
| `ctx.fs.is_dir(path)` | `bool` | Path is a directory |
| `ctx.fs.read_dir(path)` | `array` | List `{name, path, kind}` entries, sorted by name |
| `ctx.fs.metadata(path)` | `table` | `{path, kind, len, readonly}` |
| **Shell** | | Run commands through native approval + policy |
| `ctx.shell(cmd, opts?)` | `table` | `{stdout, stderr, exit_code}` |
| `ctx.shell_streaming(cmd, cb, opts?)` | `table` | Calls `cb(line)` per stdout line; returns `{stdout, stderr, exit_code}` |
| **Files** | | Read/write |
| `ctx.read_file(path)` | `string` | Read entire file contents (raises Lua error on failure) |
| `ctx.write_file(path, content)` | `true` | Create new file; fails if file exists (raises Lua error) |

| **`ctx.ui.*`** | | UI output |
| `ctx.ui.notify(msg, level?)` | | Show notification (`"info"`, `"warn"`, `"error"`) |
| `ctx.ui.status(msg)` | | Write status line to stderr |
| `ctx.ui.pane(table)` | `true\|(false, string)` | Emit a live pane update (tools only) |
| **Live events** | | During `execute_output_live` only |
| `ctx.emit_pane(table)` | `true` | Same as `ctx.ui.pane` |
| `ctx.emit_state(src, key, json)` | `true` | Emit StateUpdate live event |
| `ctx.emit_state_remove(src, key)` | `true` | Emit StateRemove live event |
| **`ctx.usage.*`** | | Token usage |
| `ctx.usage.snapshot()` | `table\|nil` | See [Usage Snapshot](#usage-snapshot) below |
| **`ctx.state.*`** | | Session-scoped key-value store |
| `ctx.state.get(key)` | `string\|nil` | Get value |
| `ctx.state.set(key, value)` | `true` | Set value |
| `ctx.state.clear(key)` | `true` | Remove key |
| **`ctx.tools.*`** | | Call registered tools |
| `ctx.tools.definitions()` | `array` | `{name, description, input_schema}` for all tools |
| `ctx.tools.call(name, args, opts?)` | `table` | `{ok, name, call_id, content, is_error}` |
| **`ctx.agent.*`** | | Spawn subagents |
| `ctx.agent.run(prompt, opts?)` | `table` | `{ok, content, error}` |
| `ctx.agent.run_stream(prompt, opts?)` | `table` | Same with event callbacks |
| `ctx.agent.spawn(prompt, opts?)` | `table` | `{ok, id, error}` — non-blocking background job |
| `ctx.agent.jobs()` | `array` | Snapshot of all jobs (`{id, agent, task, status, result, started_at}`) |
| `ctx.agent.wait(ids?, opts?)` | `table` | `{ok, jobs, pending, timed_out, cancelled}` — block until jobs finish |
| **`ctx.config.*`** | | Read-only config access |
| `ctx.config.dir` | `string` | Same as `ctx.config_dir` |
| `ctx.config.get(section, key)` | `value\|nil` | Read a value from `config/<section>.yaml` |
| `ctx.config.get_table(section)` | `table\|nil` | Read entire config section as table |
| **`ctx.session.*`** | | Conversation history |
| `ctx.session.current()` | `table\|nil` | `{id, provider, model}` for current session |
| `ctx.session.list(opts?)` | `array` | Recent conversations (default limit 20, max 100) |
| `ctx.session.messages(id, opts?)` | `array` | Messages for a conversation (default limit 200, max 1000) |

#### Context Availability

Not all `ctx` fields are available in every handler type:

| | Tool `execute` | Command `handler` | Event `bone.on` handler |
|---|:---:|:---:|:---:|
| `config_dir` | yes | yes | yes |
| `cwd` | yes | yes | — |
| `log` | yes | yes | — |
| `fs` | yes | yes | — |
| `shell` / `shell_streaming` | yes | yes | — |
| `read_file` / `write_file` | yes | yes | — |
| `ui.notify` | yes | yes | yes |
| `ui.status` / `ui.pane` | yes | yes | — |
| `emit_pane` / `emit_state` | yes | — | — |
| `usage` | yes | yes | — |
| `state` | yes | yes | — |
| `tools` | yes | yes | — |
| `agent` | yes | yes | — |
| `config` | yes | yes | `config.dir` only |
| `session` | yes | yes | — |
| `call_id` | yes | — | — |

Event handlers receive a minimal `ctx` with only `config_dir`, `ui.notify`, and `config.dir`. They cannot execute shell commands, read files, or call tools. This is intentional — event handlers run inline during the event loop and must not block.

#### Shell Options

`ctx.shell` and `ctx.shell_streaming` accept an optional opts table:
```lua
{ timeout_ms = 120000 }  -- min 1000, max 300000
```
Default timeout: 120s for `ctx.shell`, 300s for `ctx.shell_streaming`. Commands run through the same approval and policy system as the native `shell` tool.

#### `ctx.tools.call`

Call a registered tool by name with typed arguments:
```lua
local result = ctx.tools.call("read_file", { path = "/tmp/test.txt" }, { approval = "safe" })
if result.ok then
    ctx.log.info(result.content)
else
    ctx.log.error(result.content)
end
```
Opts: `{ approval = "safe" | "read_only" | "danger" }`. Max nesting depth: 4 levels of tool calls from Lua.

#### `bone.register_subagent`

Declare a named sub-agent in `init.lua`. The `subagent` tool (auto-created when agents are registered) uses these definitions to dispatch tasks.

```lua
bone.register_subagent({
    name = "researcher",
    description = "Searches the web and summarizes findings",
    system_prompt = "You are a researcher.",
    provider = "openai",
    model = "gpt-4o",
    approval = "safe",
})
```

Fields: `name` (required, unique), `description` (required), `system_prompt`, `provider`, `model`, `approval`. Duplicates are skipped with a warning.

#### `ctx.agent.run` / `ctx.agent.run_stream`

Spawn a subagent:
```lua
local result = ctx.agent.run("Summarize this file", {
    approval = "safe",
    system_prompt = "You are a summarizer.",
    timeout_ms = 300000,
})
if result.ok then
    ctx.log.info(result.content)
end
```
Opts: `{ approval, provider, model, system_prompt, timeout_ms }`. Default timeout: 300s, max 900s. Max nesting depth: 3 levels.

`run_stream` accepts additional callback opts: `on_started`, `on_status`, `on_tool_call`, `on_tool_result`, `on_token_usage`, `on_finished`, `on_failed`. Each callback receives a table with event-specific fields.

#### `ctx.agent.spawn` / `ctx.agent.jobs` / `ctx.agent.wait`

Dispatch a non-blocking background job. Results are delivered in one of two ways: blocking on `ctx.agent.wait` (when the caller needs them now), or auto-injection into the conversation when the main agent goes idle.

```lua
local result = ctx.agent.spawn("Research Rust async runtimes", {
    agent = "researcher",
    system_prompt = "You are a researcher.",
    timeout_ms = 300000,
})
-- result: { ok = true, id = "job-1", error = nil }
```

Opts: `{ agent, approval, provider, model, system_prompt, timeout_ms }`. Sub-agents (`agent_depth > 0`) cannot spawn or wait on background jobs — use blocking `ctx.agent.run` instead.

Query all jobs:
```lua
local jobs = ctx.agent.jobs()
-- jobs: array of { id, agent, task, status, result, started_at, finished_at, consumed }
```

Block until jobs finish:
```lua
local outcome = ctx.agent.wait({ "job-1", "job-2" }, { timeout_ms = 300000 })
-- outcome: { ok = true, jobs = {...finished jobs...}, pending = {"job-2"},
--            timed_out = false, cancelled = false }
```

`ids` is optional — `ctx.agent.wait(nil)` waits on all currently running jobs. Default timeout 300s, max 900s. Jobs returned by `wait` are marked consumed so they are not auto-injected again. Esc cancels the wait (`cancelled = true`); the jobs themselves keep running and their results auto-inject later. Jobs still running at timeout are listed in `pending` and also auto-inject on completion.

**Auto-injection**: when a background job finishes unconsumed and the TUI is idle, results are injected as a new turn. The agent wakes up automatically — no polling needed. Results are truncated to 16k chars at injection time.

**The `subagent` tool** (auto-created when agents are registered) exposes this to the main agent as three actions: `dispatch` (with optional `wait=true` for dependent work), `wait` (collect pending results), and `status` (non-blocking snapshot). The intended workflow: batch independent tasks into one dispatch; wait when the next step depends on results; otherwise end the turn and let results auto-inject.

#### Usage Snapshot

`ctx.usage.snapshot()` returns a table with the current conversation's token usage:
```lua
local u = ctx.usage.snapshot()
-- u.request_count, u.sent, u.received, u.cached, u.cost
-- u.context_length, u.tool_count, u.tool_schema_chars, u.tool_schema_tokens
-- u.system_prompt_chars, u.system_prompt_tokens
-- u.by_provider: array of { provider, model, prompt_tokens, completion_tokens, cached_tokens, cost, request_count }
```
Returns `nil` if usage data is unavailable in the current context.

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

Protected built-ins (`/help`, `/quit`, `/exit`, `/new`, `/clear`, `/model`, `/provider`, `/config`, `/tools`, `/edit`, `/e`, `/stats`) cannot be overridden.

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
