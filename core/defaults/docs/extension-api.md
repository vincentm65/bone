# Extension API

Bone embeds Lua 5.4 for tools, commands, settings, themes, keymaps, event hooks,
plugins, and UI components. If `init.lua` is absent, the runtime behaves as
before. Errors in startup Lua are warnings; core continues without that wiring.

Use the namespaced APIs below. Keep `init.lua` as wiring and put implementations
in `lua/tools/`, `lua/commands/`, `lua/themes/`, `lua/lib/`, or
`lua/plugins/<name>/init.lua` as appropriate.

## Registration

```lua
bone.tool.register({ name = "...", description = "...", parameters = schema,
    safety = "read_only", execute = function(params, ctx) ... end })
bone.command.register("name", { description = "...", handler = function(args, ctx) ... end })
bone.keymap.set("<C-p>", "toggle_panes")
bone.theme.load("name")
bone.settings.define("namespace", { title = "...", fields = { ... } })
bone.on("event_name", function(event, ctx) ... end)
```

The global metadata includes `bone.version`, `bone.cwd`, `bone.config_dir`,
`bone.agent_depth`, `bone.headless`, `bone.model`, and `bone.provider`. Logging
is available through `bone.log.info`, `bone.log.warn`, and `bone.log.error`.

## Context

Tool `execute(params, ctx)` and command `handler(args, ctx)` receive a context
with the following core groups:

- `ctx.config_dir`, `ctx.cwd`, `ctx.call_id`, and `ctx.log.*`;
- read-only filesystem queries in `ctx.fs.*`, plus `ctx.read_file` and
  `ctx.create_file`;
- approved shell through `ctx.shell` and `ctx.shell_streaming`;
- bounded waits and binary-safe codecs under `ctx.time.*` and `ctx.codec.*`;
- `ctx.ui.*` for notifications, panes, view diffs, key input, and terminal width;
- `ctx.runtime.info()` and request-scoped model metadata;
- session/conversation inspection through `ctx.session.*`, `ctx.conversation.*`,
  and read-only `ctx.db.query`;
- session-scoped state in `ctx.state.*`;
- typed tool calls through `ctx.tools.definitions()` and `ctx.tools.call()`;
- delegated work through `ctx.agent.*`, and managed processes through
  `ctx.process.*`; and
- daemon-owned settings/configuration access through `ctx.settings.*` and
  `ctx.config.*`.

Event handlers normally receive a minimal context containing notifications and
configuration metadata. `before_turn` receives the full context. Event handlers
must not block; this restriction keeps lifecycle callbacks safe in the event
loop.

All file writes and shell commands still pass through Bone's native approval and
policy path. The Lua API does not grant an extension a bypass.

## Tools

A tool's `name`, description, JSON-Schema `parameters`, safety level, and
`execute` function define its agent contract. Native tools cannot be overridden.
Use `read_only` for inspection and safe external reads; use `danger` only when
the operation genuinely mutates or executes untrusted-side effects.

Tools return a string to the agent. A JSON return envelope may additionally
contain `content`, serialized `state`, and a `pane` with `source`, `title`,
`lines`, `visible_rows`, and `scroll`. A pane with the same source replaces the
previous pane; empty lines remove it. `ctx.ui.pane` can upsert panes while the
tool runs. Clean up panes when work completes; cancellation automatically clears
host-owned panes.

`ctx.tools.call(name, args, { approval = "safe" | "read_only" | "danger" })`
uses the normal registry and approval pipeline. Lua tool nesting is bounded.

## Commands and return actions

Commands are invoked as `/name args`. A command can return:

- `nil` to handle the command without submitting a prompt;
- a string to inject as the next prompt/output;
- a display table with `display`, `reply`, or `content` and `submit = false`; or
- an action table to request a supported daemon state mutation.

The `conversation.replace` action replaces the model-facing transcript with
validated `user`, `assistant`, and `tool` messages. Core recomputes the context
estimate, persists a checkpoint, and keeps complete SQLite display history.
Commands and `before_turn` handlers may also return `system_prompt_append`, a
transient `turn_message`, and a per-turn `tool_filter` allow-list. These fields
shape the request; they do not mutate the global configuration.

Core command names remain protected and cannot be overridden. A command with
settings should register its namespace through `bone.settings.define` rather
than writing YAML directly.

## before_turn return values

`before_turn` may return the same validated request-shaping values as commands:

- `{ action = "conversation.replace", messages = {...} }` replaces the model-facing transcript;
- `system_prompt_append` and `turn_message` are transient per-turn values; and
- `tool_filter` is a per-turn allow-list of tool names.

Handlers run in registration order. Their `system_prompt_append` values accumulate;
other fields use the normal hook merge rules.

Compaction is implemented in catalog Lua rather than as a dedicated Rust action.
Lua owns thresholds, history selection, prompts, repair, checkpoint formatting,
continuation wording, notices, and replacement policy. It supplies explicit messages,
tools, and an optional positive `max_tokens` to `ctx.llm.complete`, which performs
exactly one private provider request with no agent/tool loop. Private text is not
surfaced, and returned tool calls are exposed to Lua without execution. Usage and
cancellation are accounted by the authoritative Driver turn or daemon command path.
Transcript mutation occurs only when the validated `conversation.replace` result is
applied and persisted by the daemon.

Private completion is intentionally unavailable during `bone run` slash-command
expansion: that path has no durable conversation or command usage owner. It remains
available to `before_turn` hooks during the headless agent turn itself.

## Events

```lua
bone.on("tool_call", function(event, ctx)
    if event.name == "shell" then
        -- return { block = true, reason = "..." } to stop this call
    end
end)
```

Core events are `session_start`, `session_end`, `message`, `tool_call`,
`tool_result`, `mode_change`, `turn_start`, `token_usage`, `turn_end`, and
`before_turn`. Handlers run in registration order. The first blocking result
stops a `tool_call`; handler errors fail open and do not block. Registration in a
subagent is ignored by default; pass `{ subagents = true }` when that is
intentional.

## UI API

Lua UI is declarative. `ctx.ui.pane` and `ctx.ui.apply` emit shared view updates;
`bone.api.ui` provides lower-level floats, status-line segments, and live
highlights. Stable component ids make updates idempotent. A frontend renders the
same protocol `ViewDiff` whether the update came from Rust or Lua.

```lua
bone.api.ui.open_float({ id = "help", title = "Help", lines = { "text" },
    width = 40, height = 10, anchor = "center" })
bone.api.ui.set_lines("help", { "updated" })
bone.api.ui.set_statusline("stats", { { text = "ready", align = "right" } })
bone.api.ui.set_highlight("input_border", "#e0a050")
bone.api.ui.close("help")
```

## Plugins and loading

Plugins live under `lua/plugins/<name>/init.lua` and must be loaded explicitly
from `init.lua` with `bone.plugin.load(name)`. The core plugin API also exposes
list, install, update, and remove operations. A plugin is still subject to the
same tool, command, settings, filesystem, shell, and approval contracts as any
other Lua extension.

For debugging, use `bone.log.*`, inspect `ctx.runtime.info()`, enumerate tool
and subagent definitions, and use the protocol/event stream rather than relying
on frontend-local state. See [Agents](agents.md) for delegated execution and
[UI](ui.md) for frontend behavior.
