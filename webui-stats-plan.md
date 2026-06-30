# Web UI Stats Implementation Plan

**Status:** Proposed  
**Scope:** Bridge endpoint + web UI stats panel  
**Goal:** Bring the web UI's token stats to parity with the TUI's stats dashboard

## Current State

### TUI Stats Dashboard
- **Cards:** Requests, Prompt, Completion, Cached, Total, Cache %
- **Charts:** Token usage over time (daily/weekly/monthly/yearly bars)
- **Hourly:** Usage by hour of day
- **Models:** Per-provider/model breakdown (today, 7d, 4w, all time)
- **Daily Activity:** Conversation count per day
- **View Modes:** Today, 7 days, 4 weeks, Yearly, All time
- **Features:** Date range picker, refresh, scroll, keyboard nav
- **Data Source:** SQLite (`conversations.db`) via `SessionDb`

### Web UI Current
- **Meter:** Context length + cost only
- **Events:** `token_usage` updates meter with `context_length`, `sent+received`
- **No:** Historical data, charts, model breakdown, request count, cached tokens

## Gap Analysis

| Feature | TUI | Web UI |
|---------|-----|--------|
| Request count | ✓ | ✗ |
| Prompt tokens | ✓ | ✗ |
| Completion tokens | ✓ | ✗ |
| Cached tokens | ✓ | ✗ |
| Cache % | ✓ | ✗ |
| Cost | ✓ (meter) | ✓ (meter) |
| Context length | ✓ (meter) | ✓ (meter) |
| Bar charts | ✓ | ✗ |
| Hourly chart | ✓ | ✗ |
| Model breakdown | ✓ | ✗ |
| Daily activity | ✓ | ✗ |
| Date range picker | ✓ | ✗ |
| View modes | ✓ | ✗ |

## Implementation Plan

### Phase 1: Bridge Endpoint

Add `/api/stats` to `bridge.mjs`:

```javascript
// GET /api/stats?mode=today|7d|4w|yearly|all|custom&start=YYYY-MM-DD&end=YYYY-MM-DD
```

Returns `UsageStatsSnapshot` as JSON:

```typescript
interface UsageStatsSnapshot {
  started_at: string | null;
  ended_at: string | null;
  total: UsageSummary;
  by_model_today: ProviderUsage[];
  by_model_7d: ProviderUsage[];
  by_model_4w: ProviderUsage[];
  by_model_all: ProviderUsage[];
  daily: UsageBucket[];
  weekly: UsageBucket[];
  monthly: UsageBucket[];
  all_time: UsageBucket[];
  yearly: UsageBucket[];
  hourly_today: HourUsage[];
  hourly_7d: HourUsage[];
  hourly_4w: HourUsage[];
  hourly_all: HourUsage[];
  daily_activity: UsageBucket[];
}

interface UsageSummary {
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  cost: number;
  request_count: number;
}

interface ProviderUsage {
  provider: string;
  model: string;
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  cost: number;
  request_count: number;
}

interface UsageBucket {
  label: string;
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  cost: number;
  request_count: number;
}

interface HourUsage {
  hour: number;
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
  request_count: number;
}
```

Implementation:
- Read `conversations.db` directly (same as `listConversations`)
- Reuse SQL queries from `core/src/session_db.rs` (port to Node)
- Support `mode` query param for view mode
- Support `start`/`end` for custom date ranges

### Phase 2: Web UI Stats Panel

Add to `app.js`:

1. **Stats button** in header/sidebar to open stats panel
2. **Stats panel** with:
   - Header with range label + refresh
   - 6 cards (Requests, Prompt, Completion, Cached, Total, Cache %)
   - Bar chart (token usage over time)
   - Hourly chart
   - Model breakdown table
   - Daily activity
   - View mode tabs (Today, 7d, 4w, Yearly, All time)
   - Date range picker (optional, can defer)

3. **Data loading:**
   - Fetch `/api/stats?mode=...` on panel open
   - Cache snapshot, refetch on mode change
   - Update meter with real-time `token_usage` events (existing)

### Phase 3: Polish

- Keyboard navigation (1-5 for modes, r for refresh)
- Scroll for long lists
- Responsive layout (narrow vs wide)
- Loading states
- Error handling

## Files to Modify

1. `webui/bridge.mjs` — add `/api/stats` endpoint
2. `webui/public/app.js` — add stats panel + rendering
3. `webui/public/index.html` — add stats button + panel markup

## Risks

- SQLite read from Node requires `better-sqlite3` or built-in `DatabaseSync` (Node 20+)
- Bridge is zero-dep; `DatabaseSync` is built-in (Node 20.0+)
- SQL queries from Rust need porting to Node SQL strings
- Stats panel adds UI complexity; keep it simple v1

## Timeline

- Phase 1: 1-2 hours (bridge endpoint)
- Phase 2: 2-3 hours (stats panel)
- Phase 3: 1 hour (polish)

Total: ~4-6 hours
