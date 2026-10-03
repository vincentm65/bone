# switch

Pick the model provider from the TUI, without restarting: `/provider` opens a
picker (or `/provider name`), and the next model call goes there.

```sh
cp -r examples/plugins/switch ~/.bone/plugins/
```

In `~/.bone/core.lua`, keep your providers and add a `switch` entry that
routes to them:

```lua
bone.config.providers.qwen = { base_url = "http://localhost:8080/v1", model = "qwen" }
bone.config.providers.deepseek = { base_url = "https://api.deepseek.com/v1", model = "deepseek-chat", api_key = os.getenv("DEEPSEEK_API_KEY") }
bone.config.providers.switch = { type = "switch", model = "switch", default = "qwen" }
bone.config.provider = "switch"
```

OpenAI-compatible entries are called by the plugin's own Lua client; an entry
with a `type` (for example the `anthropic` plugin's) goes to that provider.
The choice lives in `~/.bone/state/shared/switch.json`, so it applies to
every session and TUI on this config, and `/health` shows where requests go.
`bone.switch.current()` returns the name, for a statusline.

It uses `bone.on_ready` (to publish the final list of providers),
`bone.state` with `{ shared = true }` (the two halves' common state) and
`bone.provider.register` + `bone.http_stream`.
