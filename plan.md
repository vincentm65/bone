# Bone: Usage Tracking & Conversation Storage

## Goal

Track detailed token usage per request, display usage summaries in the terminal, log every conversation to a local SQLite database with FTS5 full-text search, and provide a foundation for recall/memory over past conversations.

Key distinction:

- A **conversation** is a logical transcript/session.
- A **usage event** is one provider request/response with token/cost metadata.

Provider or model switches do **not** end the conversation. They create separately attributed usage events so totals remain correct while the transcript stays coherent.

---

## Architecture

### Data Flow

```
Provider SSE response
  │
  ├─ prompt_tokens, completion_tokens
  ├─ prompt_tokens_details.cached_tokens   (OpenRouter, Anthropic where available)
  └─ cost                                  (OpenRouter where available)
        │
        ▼
  ChatEvent::TokenUsage { prompt_tokens, completion_tokens, cached_tokens, cost }
        │
        ▼
  TokenStats { sent, received, cached, cost, context_length, request_count }
        │
        ├─► Status bar (existing: curr | in | out)
        ├─► /usage command (full summary)
        ├─► /clear summary (one-liner)
        └─► SessionDb usage_events row with current provider/model
```

### Storage

Single SQLite database at `~/.bone-rust/data/conversations.db`.

- One row per logical conversation in `conversations`.
- One row per chat/tool/system message in `messages`.
- One row per provider response in `usage_events`.
- FTS5 virtual table over message content for full-text search.
- Messages and usage are written as they happen. Graceful shutdown may set `ended_at`, but correctness must not depend on shutdown running.

---

## Schema

```sql
CREATE TABLE IF NOT EXISTS conversations (
    id         INTEGER PRIMARY KEY,
    started_at TEXT NOT NULL,
    ended_at   TEXT,
    provider   TEXT NOT NULL, -- initial provider
    model      TEXT NOT NULL  -- initial model
);

CREATE TABLE IF NOT EXISTS messages (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id),
    role            TEXT NOT NULL,   -- user / assistant / system / tool
    content         TEXT NOT NULL,
    tool_name       TEXT,            -- null unless role = tool
    seq             INTEGER NOT NULL, -- order within conversation
    created_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS usage_events (
    id                INTEGER PRIMARY KEY,
    conversation_id   INTEGER NOT NULL REFERENCES conversations(id),
    provider          TEXT NOT NULL,
    model             TEXT NOT NULL,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    cached_tokens     INTEGER NOT NULL DEFAULT 0,
    cost              REAL    NOT NULL DEFAULT 0.0,
    created_at        TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content,
    role UNINDEXED,
    conversation_id UNINDEXED,
    tokenize='unicode61'
);
CREATE INDEX IF NOT EXISTS idx_usage_events_conversation
    ON usage_events(conversation_id);
CREATE INDEX IF NOT EXISTS idx_messages_conversation_seq
    ON messages(conversation_id, seq);
```

`append_message()` inserts into both `messages` and `messages_fts` explicitly. This is slightly less clever than trigger-based external-content FTS and easier to debug for v1.

Conversation totals are derived from `usage_events`, not duplicated in `conversations`:

```sql
SELECT
    SUM(prompt_tokens),
    SUM(completion_tokens),
    SUM(cached_tokens),
    SUM(cost),
    COUNT(*)
FROM usage_events
WHERE conversation_id = ?;
```

Provider/model breakdown:

```sql
SELECT
    provider,
    model,
    SUM(prompt_tokens),
    SUM(completion_tokens),
    SUM(cached_tokens),
    SUM(cost),
    COUNT(*)
FROM usage_events
WHERE conversation_id = ?
GROUP BY provider, model;
```

---

## New Dependency

```toml
rusqlite = { version = "0.34", features = ["bundled"] }
```

`bundled` compiles SQLite from C source. No system dependency needed. Adds roughly 2MB to the binary. No encryption feature for now; `bundled-sqlcipher` can be considered later.

---

## Implementation Steps

### Step 1: Extend `ChatEvent::TokenUsage`

**File:** `src/llm/provider.rs`

Current:
```rust
TokenUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}
```

New:
```rust
TokenUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    cached_tokens: Option<u32>,
    cost: Option<f64>,
}
```

`Option` fields because not all providers return cached-token or cost data.

### Step 2: Parse extended usage from SSE

**File:** `src/llm/providers/openai_compat/mod.rs`

In the `[DONE]` handler where `last_usage` is already captured, extract additional fields:

```rust
yield ChatEvent::TokenUsage {
    prompt_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
    completion_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
    cached_tokens: usage
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32),
    cost: usage.get("cost").and_then(|v| v.as_f64()),
};
```

All providers degrade gracefully when the optional fields are absent.

### Step 3: Extend `TokenStats`

**File:** `src/llm/token_tracker.rs`

Add fields:
```rust
pub struct TokenStats {
    pub sent: u64,
    pub received: u64,
    pub cached: u64,
    pub cost: f64,
    pub request_count: u64,
    pub context_length: u64,
}
```

Update `record_request`:
```rust
pub fn record_request(
    &mut self,
    prompt_tokens: u32,
    completion_tokens: u32,
    cached_tokens: Option<u32>,
    cost: Option<f64>,
)
```

Add methods:

- `summary() -> String` -- multi-line for `/usage`.
- `one_liner() -> String` -- single line for `/clear` display.
- `reset(&mut self)` -- zero cumulative fields.

Keep `display()` as-is for the status bar. No cached/cost data in the status bar.

### Step 4: Wire through stream consumers

**Files:** `src/ui/app/stream/mod.rs`, `src/agent.rs`

Pass the new fields through from `ChatEvent::TokenUsage` to `TokenStats::record_request()`.

Both TUI and headless stream consumers write to the DB. Headless mode gets the same full storage as TUI mode.

```rust
ChatEvent::TokenUsage {
    prompt_tokens,
    completion_tokens,
    cached_tokens,
    cost,
} => {
    self.token_stats.record_request(
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cost,
    );
    if let Some(ref db) = self.session_db {
        db.record_usage(
            self.conversation_id,
            &self.current_provider,
            &self.current_model,
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            cost,
        );
    }
}
```

### Step 5: Create `session_db.rs`

**File:** `src/session_db.rs` (new)

Keep this as a small storage layer. Do not make it own the active conversation lifecycle; `App` owns `conversation_id`.

```rust
pub struct SessionDb {
    conn: rusqlite::Connection,
}
```

Methods:

| Method | Purpose |
|---|---|
| `open() -> Result<Self>` | Open/create DB, run schema setup |
| `create_conversation(provider, model) -> i64` | Insert conversation row |
| `append_message(conversation_id, role, content, tool_name, seq)` | Insert into `messages` and `messages_fts` |
| `record_usage(conversation_id, provider, model, prompt_tokens, completion_tokens, cached_tokens, cost)` | Insert one `usage_events` row |
| `end_conversation(conversation_id)` | Set `ended_at`; optional best-effort lifecycle metadata |
| `conversation_usage(conversation_id) -> UsageSummary` | Sum `usage_events` |
| `usage_by_provider(conversation_id) -> Vec<ProviderUsage>` | Group usage by provider/model |
| `search(query, limit) -> Vec<SearchHit>` | FTS5 search over previous messages |
| `delete_message(message_id)` | Delete from both `messages` and `messages_fts` |

All methods are synchronous. SQLite is fast enough that async is unnecessary.

Migration strategy: `CREATE TABLE IF NOT EXISTS` statements for v1. Versioned migrations can be added once the schema starts changing.

### Step 6: Wire DB into `App`

**File:** `src/ui/app/mod.rs`

- Add `session_db: Option<SessionDb>` field to `App`.
- Add `conversation_id: Option<i64>` field.
- Add `session_seq: i64` counter for message ordering.
- `App::new()` opens the DB and creates one conversation with the initial provider/model.
- If DB open fails, continue running and show/log a warning rather than making chat unusable.
- On each completed user/assistant/tool/system message that should be remembered, call `append_message()`.
- On each `ChatEvent::TokenUsage`, call `record_usage()` with the current provider/model.

Use `Option<SessionDb>` so a DB error does not break the terminal chat experience.

### Step 7: Lifecycle behavior

| Event | Action |
|---|---|
| `/clear` or `/new` | **Force-terminate** any in-flight stream, print token summary, end current conversation best-effort, create a new conversation, reset token stats and sequence |
| `/quit` or shutdown | Best-effort `end_conversation(conversation_id)` |
| Provider switch | Do **not** end the conversation. Update current provider/model labels. Future `usage_events` use the new provider/model. |
| Model switch | Do **not** end the conversation. Future `usage_events` use the new model. |

This preserves the logical transcript while accurately attributing usage. Example: one conversation can contain 1M GLM tokens and 5M Codestral tokens, and totals/breakdowns stay correct.

### Step 8: `/usage` command

**File:** `src/ui/commands/mod.rs`

Add `/usage` to command dispatch. Output from `TokenStats::summary()` for in-memory current conversation stats:

```text
Conversation stats
  Requests:  5
  Tokens in: 12,340
  Tokens out: 3,456
  Cached:    4,200
  Context:   3,892 (current)
  Cost:      $0.0423
  Avg/req:   2,468 in / 691 out
```

Hide `Cached` when zero. Hide `Cost` when zero.

If DB usage breakdown is wired into the command, append:

```text

By provider/model
  GLM / glm-4.6        1,000,000 in / 80,000 out / 200,000 cached / $0.12
  Codestral / latest   5,000,000 in / 340,000 out / $1.11
```

Also add to `help()` output:

```text
/usage     — show token usage for current conversation
```

### Step 9: `/clear` and `/new` summary

**File:** `src/ui/commands/mod.rs` `clear()` function

Before clearing, format and print the session summary from `TokenStats::one_liner()`:

```text
Session: 5 req | 12,340 in | 3,456 out | 4,200 cached | $0.04 | Chat cleared.
```

Then reset stats and start a new conversation row.

### Step 10: Optional minimal recall command

This is the smallest feature that proves the conversation store supports memory/recall.

**Command:** `/recall <query>`

Behavior:

- Search `messages_fts` across past messages.
- Return top 5 snippets with date/conversation id/role.
- Do not automatically inject results into the LLM context yet.

Example:

```text
Recall results for "provider switch tokens"
  2026-01-14 user: The idea with conversation ending on provider switches...
  2026-01-14 assistant: Better model: usage is recorded per request...
```

Automatic memory injection can remain future work.

**SearchHit struct:**

```rust
pub struct SearchHit {
    pub message_id: i64,
    pub conversation_id: i64,
    pub role: String,
    pub snippet: String,
    pub created_at: String,
}
```

**FTS JOIN query (searches across all conversations):**

```sql
SELECT
    m.id,
    m.conversation_id,
    m.role,
    snippet(messages_fts, 0, '<<', '>>', '...', 32) AS snippet,
    m.created_at
FROM messages_fts fts
JOIN messages m ON m.id = fts.rowid
WHERE messages_fts MATCH ?
ORDER BY rank
LIMIT ?;
```

### Step 11: Register commands/modules

**Files:** `src/ui/app/mod.rs`, `src/lib.rs` or `src/main.rs`

- Add `"usage"` to the command routing whitelist.
- Add `"recall"` if implementing the optional recall command.
- Declare the `session_db` module.
### Step 12: Unit tests for `session_db.rs`

**File:** `src/session_db.rs` (inline `#[cfg(test)]` module)

Test against an in-memory SQLite connection (`:memory:`). This keeps tests fast and isolated with no file I/O.

Coverage:

| Test | What it verifies |
|---|---|
| `create_and_end_conversation` | Insert, query back, set `ended_at` |
| `append_and_retrieve_messages` | Message ordering by `seq`, correct roles/content |
| `append_message_populates_fts` | FTS match after insert |
| `delete_message_removes_from_both` | Message gone from `messages` and `messages_fts` after delete |
| `record_and_sum_usage` | `conversation_usage` totals match inserted events |
| `usage_by_provider_grouping` | Multiple providers in one conversation, correct grouping |
| `search_returns_ranked_hits` | FTS search returns relevant snippets with metadata |
| `search_across_conversations` | Hits from multiple conversations, most recent first |

Helper: add a `SessionDb::open_in_memory()` method used only by tests that creates the schema on `:memory:`.

---

## Files Changed

| File | Change | Lines |
|---|---|---:|
| `Cargo.toml` | Add `rusqlite` dependency | 1 |
| `src/llm/provider.rs` | Extend `TokenUsage` variant | ~5 |
| `src/llm/token_tracker.rs` | Add fields/methods | ~50 |
| `src/llm/providers/openai_compat/mod.rs` | Parse cached tokens and cost | ~10 |
| `src/ui/app/stream/mod.rs` | Wire usage stats and DB usage event | ~10 |
| `src/agent.rs` | Wire new token fields to stats | ~5 |
| `src/session_db.rs` | New SQLite/FTS storage module, indexes, delete helper, SearchHit, unit tests | ~350 |
| `src/ui/app/mod.rs` | Add DB/conversation fields, lifecycle, command registration | ~50 |
| `src/ui/commands/mod.rs` | `/usage`, `/clear` summary (with stream force-terminate), `/recall` | ~80 |
| `src/lib.rs` or `src/main.rs` | Declare `session_db` module | 1 |
| **Total** | | **~550** |

---

## Provider-Specific Behavior

| Provider | prompt_tokens | completion_tokens | cached_tokens | cost |
|---|---|---|---|---|
| OpenRouter | Yes | Yes | Yes | Yes |
| Anthropic direct | Yes | Yes | Yes where exposed | No |
| OpenAI | Yes | Yes | Sometimes via details | No |
| Local llama.cpp | Varies | Varies | No | No |
| Others | Varies | Varies | No | No |

All fields degrade gracefully. `/usage` and `/clear` hide fields when zero or unavailable.

---

## Future Extensions

- Automatic recall injection before requests.
- `/memory` command for extracted preferences/facts.
- `/history` command listing conversations with date/model/stats.
- Conversation export to markdown.
- Auto-generated skills from repeated workflows.
- Preference extraction and summarization.
- Encrypted storage via `bundled-sqlcipher` feature flag.
- Versioned DB migrations.

---

## Key Design Decisions

1. **Conversation != provider billing bucket** -- Provider switches do not end a transcript. Per-request `usage_events` preserve exact provider/model attribution.

2. **SQLite over JSONL** -- Single file, queryable stats, FTS5, and a better base for recall/memory.

3. **Usage totals are derived from `usage_events`** -- Avoid duplicated token counters in `conversations` that can drift.

4. **All DB methods synchronous** -- SQLite writes are tiny and local.

5. **Explicit FTS inserts for v1** -- Easier to understand/debug than triggers. Triggers can be added later if desired.

6. **`Option` fields in `ChatEvent`** -- Optional provider metrics are represented honestly instead of pretending missing means zero.

7. **DB failure should not break chat** -- Use optional DB wiring and continue if storage is unavailable.

8. **No cached/cost clutter in status bar** -- Status bar keeps `curr | in | out`; detailed metrics live in `/usage` and `/clear`.
9. **Force-terminate streams on `/clear`** -- A `/clear` immediately kills any in-flight stream rather than waiting for it to finish. This gives snappy, predictable behavior.

10. **Full DB support in headless mode** -- Both TUI and headless modes write to the same SQLite store. No feature gap between modes.

11. **Explicit indexes from v1** -- Two indexes on foreign key columns prevent future slow queries and avoid a schema migration later.

12. **Defensive `delete_message` helper** -- Deletes from both `messages` and `messages_fts` so future pruning/cleanup is consistent.
