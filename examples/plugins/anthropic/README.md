# anthropic

A model provider for the Anthropic Messages API, written entirely in Lua
(`bone.provider.register` + `bone.http_stream`). It streams text and
extended thinking, and handles tool calls.

```sh
cp -r examples/plugins/anthropic ~/.bone/plugins/
```

Then in `~/.bone/core.lua`:

```lua
bone.config.providers.claude = {
  type = "anthropic",
  model = "claude-sonnet-5-5",
  api_key = os.getenv("ANTHROPIC_API_KEY"),
  -- max_tokens = 16000,
  -- thinking = 8000,   -- extended thinking budget in tokens
}
bone.config.provider = "claude"
```

Extended thinking is not sent back to the API on later turns, so leave
`thinking` off for long tool-using sessions.
