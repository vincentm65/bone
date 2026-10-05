# retry

Retries model calls that fail for passing reasons (HTTP 429/5xx, "overloaded",
timeouts, dropped connections) with exponential backoff, and can move to
another provider after a few tries.

```sh
cp -r examples/plugins/retry ~/.bone/plugins/
```

```lua
bone.config.retry = { attempts = 4, delay = 2000, fallback = "backup", fallback_after = 2 }
```

Only calls that fail before any output arrived are retried (the core's rule
for `request_error` hooks), so nothing streams twice. Its hook has a low
priority, so other `request_error` hooks decide first.
