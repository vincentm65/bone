# ping

A minimal sample plugin. It shows the core plugin API in one file: a config
option, a `tool_call` hook, and a command.

```sh
cp -r examples/plugins/ping ~/.bone/plugins/
```

- `/ping` — prints the current time in a notification
- `/ping log` — prints the name of the last tool call

Disable the notifications with `bone.config.ping.enabled = false` (in
`~/.bone/core.lua`).