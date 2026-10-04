# stats

What your sessions used and did, read from the session index with
`store/query`: tokens by model and by day, the busiest sessions, and how
often each tool was called and failed.

```sh
cp -r examples/plugins/stats ~/.bone/plugins/
```

- `/stats` shows the last 30 days in a pager; `/stats today`, `week`,
  `month` or `all` pick another period.
- Usage is recorded per model call from the version that added it on;
  older sessions show messages and tool calls but no tokens.

It is only TUI Lua: a few read-only SQL queries sent with
`bone.request("store/query", { sql, params }, callback)`, and
`bone.ui.pager` to show the result. The tables are described in
`docs/architecture.md`; write your own queries the same way.
