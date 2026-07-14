# Bone Agent Reference

## Config Location

All file paths below are relative to the bone config directory. The resolved path is provided in the system prompt under "Resolved config directory".

```
init.lua              — Lua configuration and customization (optional)
lua/tools/            — Custom + catalog Lua tools (installed via /catalog)
lua/commands/         — Custom + bundled/catalog Lua commands
lua/plugins/          — Lua plugins (optional)
lua/lib/              — Lua library modules (optional, bundled: history.lua, ui/)
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
bone.agent_depth    -- integer: 0 for the main agent, >0 inside a sub-agent
bone.headless       -- boolean: true outside the interactive TUI

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
bone.on("event_name", function(event, ctx) ... end, { subagents = true })  -- also register inside sub-agents
```

### `cjson` Global

A `cjson` global is available for JSON encoding/decoding:
```lua
local json_str = cjson.encode({ key = "value" })
local table = cjson.decode(json_str)
```

### `ctx` API

A `ctx` table is passed as the second argument to tool `execute(params, ctx)` and command `handler(args, ctx)` functions. Event handlers receive a smaller `ctx` (see [Context Availability](#context-availability) below).

To edit existing files, first call `read_file`, then use `ctx.tools.call("edit_file", { path = path, old_text = old, new_text = new })` which goes through the full approval pipeline. There is no convenience `ctx.edit_file` method.

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
| `ctx.ui.notify(msg, level?)` | | Show notification (`"info"`, `"warn"`, `"error"`); forwarded to the frontend as a status line when one is attached |
| `ctx.ui.status(msg)` | | Surface a *transient* live status line to the attached frontend (TUI); may be replaced. Stderr fallback when headless |
| `ctx.ui.notice(msg)` | | Surface a *persistent* notice that the frontend keeps in the conversation scrollback (e.g. an auto-compaction announcement). Stderr fallback when headless |
| `ctx.ui.pane(table)` | `true\|(false, string)` | Upsert/clear a live pane (tools only) — see [Live Panes](#live-panes) |
| `ctx.ui.key()` | `table` | Block for one key event: `{code, char, ctrl, alt, shift}` — see [Live Panes](#live-panes) |
| **Live events** | | During `execute_output_live` only |
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
| `ctx.agent.followup(id, prompt, opts?)` | `table` | `{ok, id, error}` — continue a completed job from its saved transcript |
| `ctx.agent.jobs()` | `array` | Snapshot of all jobs (`{id, agent, task, status, result, started_at}`) |
| `ctx.agent.wait(ids?, opts?)` | `table` | `{ok, jobs, pending, timed_out, cancelled}` — block until jobs finish |
| **`ctx.config.*`** | | Config access |
| `ctx.config.dir` | `string` | Same as `ctx.config_dir` |
| `ctx.config.get(section, key)` | `value\|nil` | Read a value from `config/<section>.yaml` |
| `ctx.config.get_table(section)` | `table\|nil` | Read entire config section as table |
| `ctx.config.get_pages()` | `array` | Read ordered custom config pages and fields |
| `ctx.config.set_value(section, key, value)` | `true` | Persist a scalar config field |
| `ctx.config.cycle_field(section, key, current)` | `string\|nil` | Next bool/enum value |
| `ctx.config.list_providers()` | `array` | Provider rows with active marker |
| `ctx.config.set_provider_entry(id, entry)` | `true` | Persist provider fields |
| **`ctx.session.*`** | | Conversation history |
| `ctx.session.current()` | `table\|nil` | `{id, provider, model}` for current session |
| `ctx.session.list(opts?)` | `array` | Recent conversations (default limit 20, max 100) |
| `ctx.session.messages(id, opts?)` | `array` | Messages for a conversation (default limit 200, max 1000) |
| **`ctx.conversation.*`** | | Active conversation transcript (not SQLite) |
| `ctx.conversation.current()` | `table\|nil` | `{id, provider, model}` for the active conversation |
| `ctx.conversation.history()` | `array` | In-memory transcript: `{role, content, tool_calls?, name?, tool_call_id?}` |

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
| `usage` | yes | yes | — |
| `state` | yes | yes | — |
| `tools` | yes | yes | — |
| `agent` | yes | yes | — |
| `config` | yes | yes | `config.dir` only |
| `session` | yes | yes | — |
| `conversation` | yes | yes | — |
| `call_id` | yes | — | — |

Event handlers receive a minimal `ctx` with only `config_dir`, `ui.notify`, and `config.dir`. They cannot execute shell commands, read files, or call tools. This is intentional — event handlers run inline during the event loop and must not block.

**Exception:** `before_turn` handlers receive a **full** `ctx` — the same as tool `execute` and command `handler`. See [before_turn](#before_turn) below.

#### `ctx.conversation`

Provides a snapshot of the active in-memory conversation transcript (not the SQLite history). Available in tool `execute` and command `handler` contexts.

```lua
-- Get the active conversation metadata
local conv = ctx.conversation.current()
-- conv.id, conv.provider, conv.model  (all nil when no active conversation)

-- Get the current transcript messages
local messages = ctx.conversation.history()
-- array of { role = "user"|"assistant"|"tool", content = string, tool_calls?, name?, tool_call_id? }
-- The system prompt is NOT included.
```

The transcript returned by `history()` is the live in-memory history used for the next provider request. It can be modified via return actions (see [Return Actions](#command-return-semantics)).

#### Shell Options

`ctx.shell` and `ctx.shell_streaming` accept an optional opts table:
```lua
{ timeout_ms = 120000 }  -- min 1000, max 300000
```
Default timeout: 120s for `ctx.shell`, 300s for `ctx.shell_streaming`. Commands run through the same approval and policy system as the native `shell` tool.

#### Managed processes

Use `ctx.process` for work that must outlive a Lua handler. Bone owns the
process group, cancellation, and captured output; extensions receive an id,
not a raw OS handle.

```lua
local job = ctx.process.spawn("npm run dev", { timeout_ms = 3600000 })
local state = ctx.process.status(job.id) -- running, stdout, stderr, exit_code
local output = ctx.process.output(job.id)
local jobs = ctx.process.list()
ctx.process.kill(job.id)
```

The native `shell` tool also accepts `{ background = true }`, returning a
managed process id immediately. Use this only for intentionally detached work
(downloads, servers, long builds); normal commands remain foreground and can
be cancelled with Ctrl+C.

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
    timeout_ms = 300000,
})
```

Fields: `name` (required, unique), `description` (required), `system_prompt`, `provider`, `model`, `approval`, `timeout_ms`. Duplicates are skipped with a warning.

Sub-agents cannot spawn nested sub-agents. When `bone.agent_depth > 0`, the default `subagent` delegation tool is not registered, and Rust also rejects attempts to spawn another agent from inside a sub-agent.

`bone.headless` is true in non-TUI flows. In headless mode, the default `subagent` tool waits for dispatched work because there is no interactive pane or auto-injection loop to deliver background results later.

#### `ctx.agent.run` / `ctx.agent.run_stream`

Spawn a subagent:
```lua
local result = ctx.agent.run("Summarize this file", {
    approval = "safe",
    system_prompt = "You are a summarizer.",
    timeout_ms = 300000,
    max_tokens = 2048,
})
if result.ok then
    ctx.log.info(result.content)
end
```
Opts: `{ approval, provider, model, system_prompt, timeout_ms, max_tokens }`. Default timeout: 300s, max 900s. Agent requests use an inactivity timeout: an active sub-agent is not stopped merely because a hard wall-clock duration elapsed while it is still streaming output or tool results. `max_tokens` caps the subagent's output tokens (sent as the provider's `max_tokens`); omit it to let the provider/server apply its own default. The cap is applied to the freshly-constructed provider, so it never affects the main turn. Use it to bound a model whose output could run away — e.g. compaction summaries.

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

Opts: `{ agent, approval, provider, model, system_prompt, timeout_ms }`. Sub-agents (`agent_depth > 0`) cannot spawn or wait on background jobs.

Query all jobs:
```lua
local jobs = ctx.agent.jobs()
-- jobs: array of { id, agent, task, status, result, started_at, finished_at,
--                  consumed, token_sent, token_received, result_file }
-- status is one of: "queued", "running", "done", "error"
```

Continue a completed job from its saved transcript:
```lua
local next = ctx.agent.followup("job-1", "Now implement the best option", {
    agent = "researcher",
    timeout_ms = 300000,
})
-- next: { ok = true, id = "job-2", error = nil }
```

`followup` is scoped to the current conversation: it can only resume jobs spawned in the same session, and only jobs that completed with a saved transcript.

Block until jobs finish:
```lua
local outcome = ctx.agent.wait({ "job-1", "job-2" }, { timeout_ms = 300000 })
-- outcome: { ok = true, jobs = {...finished jobs...}, pending = {"job-2"},
--            timed_out = false, cancelled = false }
```

`ids` is optional — `ctx.agent.wait(nil)` waits on all currently running jobs. Default timeout 300s, max 900s. Jobs returned by `wait` are marked consumed so they are not auto-injected again. Esc cancels the wait (`cancelled = true`); the jobs themselves keep running and their results auto-inject later. Jobs still running at timeout are listed in `pending` and also auto-inject on completion.

**Auto-injection**: when a background job finishes unconsumed and the TUI is idle, results are injected as a new turn. The agent wakes up automatically — no polling needed. Results are truncated to 16k chars at injection time. Full results are spilled under the system temp directory as `bone-jobs/job-N.txt`; the job's `result_file` field points to that path when present.

**Sub-agent pane**: the interactive TUI pane is rendered by Rust from the job registry, so it keeps updating even while Lua is blocked in a wait. The old Lua hook `bone._subagents_render` is no longer used.

**Quit guard**: if background sub-agent jobs are still running, the first quit request warns instead of exiting. Quit again to exit anyway; running jobs are terminated with the process.

**The `subagent` tool** (auto-created when agents are registered) exposes this to the main agent as three actions: `dispatch` (with optional `wait=true` for dependent work), `wait` (collect pending results), and `status` (non-blocking snapshot). The intended workflow: batch independent tasks into one dispatch; wait when the next step depends on results; otherwise end the turn and let results auto-inject.

Existing user config files are not overwritten when catalog items are installed. If you need the latest version of a catalog tool, run `/catalog` to re-install it.

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

## Tools

Bone has two categories of tools:

### Native Rust Tools (always available)

These are compiled into bone and do not require any seeding or installation:

- **shell** — Run a non-interactive shell command with bash -lc
- **read_file** — Read a UTF-8 text file
- **write_file** — Create a new UTF-8 text file
- **edit_file** — Replace one exact unique text block in an existing file (`{ path, old_text, new_text }`)

Use the dedicated file tools as the default interface for file contents:

- Use `read_file` rather than `cat`, `head`, `tail`, or `sed`.
- Use `write_file` rather than `tee`, `printf`, heredocs, or redirection.
- Read first, then use `edit_file` rather than `sed -i`, scripts, heredocs, or redirection.
- Use `shell` only when a file tool explicitly recommends it, for a bulk
  multi-file operation, or when no dedicated file tool supports the operation.
  If a file tool fails, follow its error instead of immediately retrying the
  same operation through `shell`.

### Catalog Lua Tools (optional, installed via `/catalog`)

Optional Lua tools live in the [`bone-catalog`](https://github.com/vincentm65/bone-catalog) repository. They are fetched from raw GitHub content at `https://raw.githubusercontent.com/vincentm65/bone-catalog/main` (overridable via `BONE_CATALOG_URL`) and installed into `~/.bone-rust/lua/tools/` on demand — during onboarding or via the `/catalog` command. Once on disk they are loaded by the normal Lua loader like any user file.

Available catalog tools include:

- **web_search** — Search the web via DuckDuckGo
- **ask_user** — Ask the user a question with selectable options
- **task_list** — Maintain a visible checklist with TUI pane rendering
- **cron** — Manage scheduled bone jobs via crontab
- **browser** — Drive a persistent browser through observe/target actions

To browse and install catalog tools interactively, run `/catalog` in the TUI. To override the catalog source URL, set the `BONE_CATALOG_URL` environment variable to an `http(s)://` base or a local filesystem path.

### Native Rust Tool Details

```lua
-- Native Rust tool, not Lua. Called by the LLM directly.
-- Parameters: command (string, required), classification (string: "read_only" or "danger"), timeout_ms (integer, optional)
```

```lua
-- Native Rust tool. Parameters: path, start_line?, max_lines?
-- Output includes the resolved path, shown range, and numbered lines.
```

```lua
-- Native Rust tool. Parameters: path, content
-- Creates a new file and refuses to overwrite an existing path.
```

```lua
-- Native Rust tool. Parameters: path, old_text, new_text
-- Read the file first. old_text must be an exact unique block from the shown
-- lines; new_text replaces it and may be empty to delete it.
```

## Commands

### /compact

Manual context compaction via summarization. Summarizes older conversation messages and replaces the transcript with a compact version, keeping recent messages verbatim.

```
/compact
```

- Requires `ctx.conversation.history()` — shows an error if unavailable.
- Skips when there are fewer user+assistant messages than the keep threshold.
- Uses `ctx.agent.run()` to generate a summary of older messages, capped via `max_tokens` so a runaway/looping model can't emit a summary larger than the context it is meant to shrink.
- Discards the result if the compacted context would not be smaller than the original (`new_context >= context_length`) — installing a larger transcript could push the next request past the model's context window (an unrecoverable provider 400). Both the manual and automatic paths enforce this.
- Returns a `conversation.replace` action (see [Return Actions](#command-return-semantics)).
- The default file `lua/commands/compact.lua` also registers a `before_turn` handler for automatic compaction.

Configuration:
- `auto_compact_tokens` — token threshold for auto-compact. Blank/unset disables auto-compact.
- `auto_compact_keep_messages` — recent user/assistant message count to preserve after compaction. Blank/unset disables manual and automatic compaction.

Auto-compaction runs after a user message is appended and before the provider request is built. It triggers only when both config values are positive integers and the current context estimate is at or above `auto_compact_tokens`.

Auto-compaction announces itself to the attached frontend via `ctx.ui.notice` (a persistent transcript line, not a transient status): a `Compacting context…` notice before the summarization call and a `Compacted: N → M messages (~X → ~Y tokens)` notice with the savings afterwards. The Driver runs the `before_turn` hook on a blocking thread so the UI stays responsive (spinner animates, Esc cancels) during the summarization, and threads the turn cancel flag so Esc aborts an in-flight compaction. `ctx.ui.notify` at info level is forwarded to the frontend the same way (no longer a silent no-op).

**Known limitation:** compaction preserves only complete tool-call chains. If the keep boundary would leave a `tool` result without its matching assistant `tool_calls`, or an assistant `tool_calls` entry without its matching result, that incomplete chain is dropped from the compacted transcript to keep provider history valid.

**Disable:** clear `auto_compact_tokens`, clear `auto_compact_keep_messages`, or remove `lua/commands/compact.lua` from the config directory. Removing the file stops both manual `/compact` and auto-compaction.

**Implementation:** summarization policy is entirely in Lua. Rust provides the generic APIs (`ctx.conversation`, `before_turn`, `conversation.replace` action) and durably checkpoints the resulting model-facing context without rewriting full history.

### /config

Open the interactive config editor. See [Config, Theme, and Keymaps](#config-theme-and-keymaps) for the full API.

### /memory

Incremental memory builder. Processes new conversations and queued explicit preference signals, then quietly updates scoped memory files without submitting a follow-up chat turn. Global memory lives in `memory/global.md`; current-project memory lives in `memory/projects/<cwd-key>.md`; pending cheap captures live in `memory/inbox.jsonl`; checkpoints live in `memory/state.json`. A legacy `memory.md` is still read as a fallback until scoped memory exists.

Usage:
- `/memory` — process new conversations and queued preference signals.
- `/memory show`, `/memory view`, `/memory list` — display global and current-project memory.
- `/memory remember <text>` — queue an explicit memory signal and run the normal merge.
- `/memory remember --global <text>` — force the signal into global memory.
- `/memory remember --project <text>` — force the signal into current-project memory.

If scoped memory exists in the config directory, global memory plus the current project's memory are loaded into the system prompt with size caps.

Bundled as `lua/commands/memory.lua` and seeded into the config directory when enabled by setup.

Run manually with `/memory`, or schedule daily:
```
cron(action=add, name=memory, time=03:00, approval=danger, prompt=/memory)
```

### /usage

Show token usage stats for the current session.



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
      template = "{action}", -- format string for the row label
      eager = false,         -- render the row at call time, not on result
  }
  ```
  - **template** — `{key}` interpolates a scalar argument. `{items[].field}`
    expands the chosen field of each element of array arg `items` (quoted,
    joined); use `{items[].a|b}` to try fields `a` then `b` per element. When an
    array placeholder resolves to nothing, the template is skipped and the row
    falls back to the `args` label.
  - **eager** — set `true` for tools whose calls block (e.g. dispatching
    background agents and waiting on them) so the row shows immediately rather
    than only when the call returns. The later result row is suppressed.
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

### Live Panes

The bottom of the TUI hosts a tab-switchable region of panes. Every pane is
identified by a stable `source` string and carries `{ title, lines, visible_rows?, scroll? }`.
The `lines` format is the same as the return-envelope `pane` above (plain
strings or `{ spans = {...} }`).

There are two ways to show a pane:

- **Return-envelope `pane`** (above) — a single snapshot shown *after* `execute`
  returns. Good for static results.
- **`ctx.ui.pane{}`** — emitted *while* the tool runs, as many times as you
  like. This is how you stream progress. Only
  available during tool execution.

Re-emitting the same `source` **replaces** that pane in place (upsert); emitting
it with empty `lines` **removes** it:

```lua
for i = 1, n do
    ctx.ui.pane{ source = "scan", title = ("Scanning (%d/%d)"):format(i, n),
                 lines = { ("checked %d files"):format(i) } }
    -- ...do work...
end
ctx.ui.pane{ source = "scan", title = "", lines = {} }   -- clear when done
```

#### Lua menus — `ui.menu`

Menus are Lua-rendered panes driven by raw key events. Use the bundled
`ui.menu` module for standard select, multi-select, and text input flows:

```lua
local menu = require("ui.menu")

local result = menu.select(ctx, {
    question = "Which branch?",
    options = { "main", "dev" },
    default = 1,                 -- 1-based initial selection (optional)
    allow_custom = true,         -- offer a free-text "Custom:" row (optional)
})
-- single_select → { value = "main" }  or  { value = "...", custom = true }
-- multi_select  → menu.multi_select(ctx, spec) returns { values = {...}, custom? = "..." }
-- text_input    → menu.text_input(ctx, spec) returns { value = "typed text" }
-- cancelled     → { cancelled = true }
```

For lower-level input, `ctx.ui.key()` blocks until the next key and returns a
table such as `{ code = "Up", char = nil, ctrl = false, alt = false, shift = false }`.
Ctrl+C remains host-owned cancellation; Esc is delivered to Lua.

The catalog `ask_user` tool is built on `ui.menu`; install it with `/catalog`
for a worked example of multi-question flows.

#### Lifecycle & cancellation

- Clean up panes you create by emitting empty `lines` when done.
- Pressing **Esc** in bundled Lua menus cancels just that menu (returns
  `{ cancelled = true }`), not the whole turn, so your cleanup code still runs.
- If the user **hard-cancels** the turn (Ctrl+C) while your tool is mid-run, its
  execution is dropped before cleanup can run; the host automatically removes any
  panes the tool emitted, so nothing lingers.
- The **sub-agent pane** (`source = "subagents"`) is rendered by Rust from the
  job registry, not via `ctx.ui.pane`. Don't use that `source` for your own panes.

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
| table with `display_role = "assistant"` and `submit = false` | Show `display`/`reply`/`content` as assistant Markdown instead of plain system text |
| table with `action` field | Apply a state-mutating action (see below) |
| error | Show error in UI |

Display-only command output defaults to system text, which is plain-wrapped.
Use `display_role = "assistant"` for Markdown-rendered reports:

```lua
return {
    display = "## Result\n\n- Rendered as Markdown",
    submit = false,
    display_role = "assistant",
}
```

#### Return Actions

Commands and `before_turn` hooks can return a table with an `action` field to mutate conversation state:

```lua
return {
    action = "conversation.replace",
    messages = {
        { role = "user", content = "Summary of earlier context" },
        { role = "assistant", content = "I understand the summary." },
    },
    display = "Context compacted.",  -- optional UI message
    submit = false,                    -- optional, defaults to true
}
```

**`conversation.replace`** — Replaces the active model-facing transcript with the given `messages` array. Each message must have `role` (`"user"`, `"assistant"`, or `"tool"`) and `content`. Optional fields: `tool_calls`, `name`, `tool_call_id`. Invalid/unknown roles are skipped; if no valid messages remain, the action is ignored. The replacement is stored as a context checkpoint so it survives restart; the complete SQLite message history is retained unchanged for display, search, and export.

When `conversation.replace` is applied:
- The transcript is replaced with the validated messages.
- The context length estimate is recomputed.
- Cumulative token/cost/request counts are unchanged.
- A system message is added to the scrollback noting the replacement.

Multiple return actions from `before_turn` handlers apply in registration order.

**Turn shaping (`before_turn` only).** Independently of `action`, a `before_turn`
handler can return three fields that shape the upcoming provider request:

```lua
return {
    system_prompt_append = "Plan only. Do not edit files; outline the steps.",
    turn_message = "Task list: 2/5 done. Mark items done as you finish them.",
    tool_filter = { "read_file", "grep", "shell" },  -- only these are exposed
}
```

- **`system_prompt_append`** — text appended to the system prompt for this turn
  (stacks after the base prompt; multiple handlers concatenate in order). Use
  only for text that stays constant across the conversation: the system prompt
  renders before the whole history, so turn-to-turn variation here invalidates
  the provider's prefix cache for every request.
- **`turn_message`** — transient message appended as the *last* input item of
  this turn's requests (wrapped in `<system-reminder>` tags, never persisted to
  the transcript; multiple handlers concatenate in order). Because it sits at
  the prompt tail, its content can change every turn at a cost of only its own
  tokens — use it for turn-varying nudges like task-list state or iteration
  counters.
- **`tool_filter`** — a per-turn allow-list of tool names. Only these tools are
  shown to the model this turn; an empty list hides every tool. This filters
  what the model *sees* — it does not change the approval policy. Omit (or
  return `nil`) to expose the full toolset. When several `before_turn` handlers
  return a filter, the last in registration order wins.

Both reset every turn, so a handler that reads a flag (e.g. from `ctx.state`)
can implement a toggled "plan mode" entirely in Lua.

Protected built-ins (`/catalog`, `/clear`, `/config`, `/edit`, `/e`, `/exit`, `/help`, `/model`, `/new`, `/provider`, `/quit`, `/setup`, `/stats`, `/tools`, `/update`) cannot be overridden.

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

| Event | When | Blockable | Full ctx |
|---|---|---|---|
| `session_start` | New session starts | no | no |
| `session_end` | Session ends | no | no |
| `message` | LLM/user message observed | no | no |
| `tool_call` | Before tool execution | **yes** | no |
| `tool_result` | After tool execution | no | no |
| `mode_change` | Approval mode changes | no | no |
| `turn_start` | A turn begins | no | no |
| `token_usage` | After each provider response, with token counts | no | no |
| `turn_end` | A turn finishes (success or failure) | no | no |
| `before_turn` | After user message, before provider request | no | **yes** |

Handlers run in registration order. First `block` stops the chain. Handler runtime errors do not block (fail-open).

By default, `bone.on` calls inside sub-agents are ignored so sub-agent prompts do not accidentally duplicate host hooks. Pass `{ subagents = true }` as the third argument to register a handler from sub-agent contexts too.

The turn-lifecycle events carry these payloads:

| Event | `event` fields |
|---|---|
| `turn_start` | `task` (user prompt), `model`, `approval` |
| `token_usage` | `sent`, `received`, `context_length` |
| `turn_end` | `ok` (bool); then `content` on success or `error` on failure |

```lua
-- Live stats: keep a status segment in sync with token usage.
bone.on("token_usage", function(event, ctx)
    bone.api.ui.set_statusline("stats", {
        { text = "ctx " .. event.context_length, align = "right" },
    })
end)
```

#### `before_turn`

The `before_turn` event fires after the user message is appended to the transcript and before the provider request is built. It receives a **full ctx** (same as tool `execute` and command `handler`) including `usage`, `state`, `agent`, `tools`, `config`, `session`, and `conversation`.

```lua
bone.on("before_turn", function(event, ctx)
    local snapshot = ctx.usage.snapshot()
    if snapshot and snapshot.context_length >= 8000 then
        -- Summarize older messages and replace the transcript.
        local history = ctx.conversation.history()
        -- ... build summary via ctx.agent.run() ...
        return {
            action = "conversation.replace",
            messages = new_messages,
        }
    end
    -- Return nil to do nothing.
end)
```

Key differences from other events:
- **Full ctx** — Access to `ctx.agent.run()`, `ctx.tools`, `ctx.usage`, `ctx.conversation`, etc.
- **Return actions** — Can return `action = "conversation.replace"` to mutate the transcript before the provider sees it.
- **Not blockable** — Unlike `tool_call`, `before_turn` cannot block the turn (only mutate it).
- **Multiple handlers** — All handlers run; return actions apply in registration order.

This is the mechanism behind automatic context compaction. The default `lua/commands/compact.lua` registers a `before_turn` handler that summarizes older messages when context exceeds a threshold. It preserves complete tool-call chains and drops incomplete chains at the compaction boundary so provider history stays valid.

## Runtime API (`bone.api`)

Where `ctx.*` is handed to a tool/command only while it runs, `bone.api` is the
always-available runtime surface — usable from `init.lua`, autocmd handlers, and
tools alike.

```lua
bone.api.autocmd(event, handler)   -- alias of bone.on
bone.api.emit(event, payload?)     -- fire an event's handlers synchronously
bone.api.submit(text)              -- queue a prompt as if typed by the user
bone.api.keymap.set/del/get(...)   -- mutate bone.keymap at runtime
bone.api.config.set/get(...)       -- mutate bone.config at runtime
```

### `bone.api.submit(text)`

Queues `text` for the frontend to submit like typed input. When the app is idle
it submits immediately; mid-turn it waits in the input queue (shown as `Q:` in
the status bar) and drains when the active turn ends. Works from any Lua context
— a tool, a command, or an autocmd — so a plugin can steer the agent without a
frontend handle. The text follows normal input rules (a leading `/` runs a
command). This is the primitive behind a `/btw`-style steering command.

### `bone.api.ui` — drawing UI from Lua

Lua draws UI by emitting view updates. Floats render as panes; a status line
appends to the native status bar; highlights recolor the live theme.

```lua
bone.api.ui.open_float({ id, title, lines, width, height, border, anchor })
bone.api.ui.set_lines(id, lines)        -- replace a float's lines
bone.api.ui.close(id)
bone.api.ui.set_statusline(id, segments) -- segments: { {text, fg?, align?}, ... }
bone.api.ui.set_highlight(name, color)   -- color string, or nil to reset
bone.api.ui.term_width()                 -- terminal columns (80 when headless)
```

**`set_statusline(id, segments)`** — each segment is `{ text, fg?, align? }`,
where `fg` is a color string and `align` is `"left"` (default) or `"right"`.
Right-aligned segments are drawn at the right edge of the status row; others
extend the native bar. Replacing the same `id` updates it; the segments persist
until changed.

**`set_highlight(name, color)`** — recolors a named highlight group live. The
group `name` is one of the [theme](#theme) field names (`user_msg`,
`user_msg_bg`, `input_border`, `status_text`, `approval_safe`, …); `color` is a
hex/named string, or `nil` to reset that group to its default. This is the
runtime counterpart to the boot-time `bone.theme` table — use it to color the
input border, user messages, or status text in response to events.

```lua
-- Tint the input border while a turn is running.
bone.on("turn_start", function() bone.api.ui.set_highlight("input_border", "#e0a050") end)
bone.on("turn_end",   function() bone.api.ui.set_highlight("input_border", nil) end)
```

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

Most users only need the palette:

```lua
bone.theme = {
    palette = {
        -- bg is optional; omit it to keep your terminal background.
        -- When set, supported terminals use it while bone is running and reset on exit.
        fg = "#ffffff",
        muted = "#808080",
        subtle = "#303030",
        border = "#808080",
        accent = "#8cdcdc",
        good = "#78b373",
        warn = "#d7ba7d",
        error = "#e05050",
        selection = "#303030",
    },
}
```

Shell command and code-block colors are separate so the basic palette stays small:

```lua
bone.theme = {
    shell = {
        program = "#b4c896",
        separator = "#5a5a5a",
        redirect = "#787878",
        flag = "#96b4dc",
        string = "#c8aa78",
        variable = "#b4a0dc",
        comment = "#808080",
        path = "#8cbebe",
    },

    syntax = {
        text = "#d4d4d4",
        comment = "#6a9955",
        string = "#ce9178",
        number = "#b5cea8",
        constant = "#569cd6",
        escape = "#d7ba7d",
        regex = "#646695",
        keyword = "#569cd6",
        keyword_control = "#c586c0",
        type = "#4ec9b0",
        function_name = "#dcdcaa",
        variable = "#9cdcfe",
        tag = "#569cd6",
        attribute = "#9cdcfe",
        punctuation = "#d4d4d4",
        subtle = "#808080",
        markup = "#569cd6",
        invalid = "#f44747",
    },
}
```

Advanced exact UI-role overrides use `highlights`. Values can be colors or palette names:

```lua
bone.theme = {
    palette = { accent = "#8cdcdc" },
    highlights = {
        user_msg = { fg = "fg", bg = "selection" },
        tool_error = "error",
        syntax_keyword = "accent",
    },
}
```

Colors: hex (`#RRGGBB`) or named (`white`, `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `gray`, `darkgray`, `lightred`, `lightgreen`, `lightyellow`, `lightblue`, `lightmagenta`, `lightcyan`). Missing keys fall back to defaults. Legacy flat keys (`user_msg`, `shell_program`, `syntax_keyword`, etc.) still work.

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
        ["<C-v>"] = "paste_image",
        ["<A-v>"] = "paste_image",
        ["<C-S-v>"] = "paste_image",
    },
}
```

Modes: `n` (normal), `i` (insert). Values are built-in action names. Unknown actions are ignored with a warning.

Built-in actions:
  - `toggle_panes` — show/hide the bottom pane area (normal mode)
  - `cycle_approval_mode` — rotate through approval modes (normal mode)
  - `cursor_to_start` — move cursor to start of line (insert mode)
  - `cursor_to_end` — move cursor to end of line (insert mode)
  - `paste_image` — paste clipboard image as attachment (insert mode; hardcoded to <C-v>, <A-v>, and <C-S-v> when no Lua binding set)
  - any custom action name registered via `bone.api.keymap.set(<mode>, <key>, <name>)`

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
      my_custom_tool.lua       -- user-created or installed via /catalog
    commands/
      compact.lua              -- seeded default
      config.lua               -- seeded default
      usage.lua                -- seeded default
      memory.lua               -- optional catalog command
      my_custom_command.lua    -- user-created
    lib/
      history.lua              -- seeded default
      ui/                      -- seeded default UI helpers
    plugins/
      tokyonight/
        init.lua
```

Seeded Lua files are created on first launch and never overwrite existing files. Catalog tools/commands are installed only when selected during onboarding or via `/catalog`.

## Tool vs Command

- **Tool** — The LLM calls as a function with typed args. Returns a string result. Good for integrations, searches, state management, TUI panes.
- **Command** — User invokes `/name [args]`. Returns a string injected as prompt. Good for workflows, reviews, templates, content generation.
