# approve

Ask before tool calls that change things. bone runs every tool call without
asking unless this (or a plugin like it) is installed.

```sh
cp -r examples/plugins/approve ~/.bone/plugins/
```

- `core.lua`: a `tool_call` hook that calls `bone.ask` before `write_file`,
  `edit_file`, `shell` and Lua tools (unless registered with
  `needs_approval = false`), and denies the call unless the answer is
  `allow` or `always`.
- `tui.lua`: shows the question as a popup (`bone.ui.popup` + `bone.ui.box`):
  `y` allow, `a` always (this tool, this session), `n`/`esc` deny.

Settings, in `~/.bone/core.lua`:

```lua
bone.config.approve.enabled = false              -- never ask
bone.config.approve.tools.shell = false          -- don't ask for shell
bone.config.approve.allow = function(ev)         -- skip asking when true
  return ev.name == "shell" and ev.arguments.command:match("^git status")
end
```

`BONE_APPROVAL=auto` turns it off for one run.

Tools from MCP servers ask too, unless their server marks them read-only (`readOnlyHint`); `bone.config.approve.tools["server_tool"] = false` stops asking for one.
