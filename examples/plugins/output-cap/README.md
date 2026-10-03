# output-cap

Keeps any one tool result from flooding the model's context: results longer
than the limit keep their start and end, with a note of how much was left
out. The built-in `shell` tool already cuts its own output; this covers every
tool, including Lua and MCP tools and `read_file`.

```sh
cp -r examples/plugins/output-cap ~/.bone/plugins/
```

```lua
bone.config.output_cap = { max = 100000 }   -- bytes
```

It is one `tool_result` hook with the lowest priority, so other hooks see the
full output first.
