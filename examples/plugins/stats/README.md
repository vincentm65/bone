# stats

A full-screen dashboard of what your sessions used and did, read from the
session index with `store/query`.

```sh
cp -r examples/plugins/stats ~/.bone/plugins/
```

`/usage` (or `/stats`) opens the last 7 days; `/usage today`, `week`,
`month`, `year`, `all` or `/usage 2026-09-01..2026-09-30` pick another
period. It shows:

- cards for tokens, input, output, cached (hit rate), requests, sessions and
  tool calls, each with the change from the period before;
- tokens per hour, day, week or month as a column chart, with the peak and
  average and the current bucket highlighted;
- models with requests, tokens, cache rate and share;
- tools with calls, failure rate and average output;
- a weekday × hour punchcard and a calendar of daily activity with your
  current and longest streak;
- the busiest sessions and where they ran.

Keys: `1`-`5` or `←`/`→` (also `d w m y a`) switch period, `t` types a date
range, `j`/`k`, page keys and the wheel scroll, `r` refreshes, `q`/`esc`
closes. Wide terminals (110+ columns) get two columns.

Usage is recorded per model call from the version that added it on; older
sessions show tool calls but no tokens. It is only TUI Lua: read-only SQL
queries sent with `bone.request("store/query", { sql, params }, callback)` and
a full-screen `bone.ui.popup` drawn with highlight groups (`UsageHeat0`-`4`,
`UsageBar`, `UsageValue`, derived from your colorscheme).
