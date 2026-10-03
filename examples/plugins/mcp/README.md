# mcp

Loads MCP servers from JSON files and shows them in the TUI.

```sh
cp -r examples/plugins/mcp ~/.bone/plugins/
```

```lua
-- ~/.bone/core.lua
bone.config.mcp_files = { "~/.config/mcp.json" }   -- { "mcpServers": { ... } } files
bone.config.mcp_options = { lazy = true }          -- start each server on first use
```

`/mcp` opens a panel with every server, its state, its last error and its
tools (`r` refreshes, `esc` closes). Without the plugin, `bone.mcp.add` and
`bone.mcp.load` in `core.lua` do the same loading; this only saves the call
and adds the panel.
