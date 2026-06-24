use rusqlite::{Connection, params};
use std::path::Path;

/// Returns the path to the conversations database.
/// Centralizes the path so all callers (TUI, headless, stats-popup) stay in sync.
/// Uses `~/.bone-rust` directly (not XDG) so existing databases aren't orphaned.
pub fn db_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".bone-rust")
        .join("data")
        .join("conversations.db")
}

/// A search hit from FTS5 query.
/// Summary of a conversation for listing.
#[derive(Clone, Debug)]
pub(crate) struct ConversationSummary {
    pub id: i64,
    pub provider: String,
    pub model: String,
    pub started_at: String,
    pub ended_at: Option<String>,
}

/// A stored message for retrieval.
#[derive(Clone, Debug)]
pub(crate) struct StoredMessage {
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<String>,
    /// JSON array of `{media_type, data}` image attachments, if any.
    pub images: Option<String>,
}

/// Aggregated usage for one conversation.
#[derive(Clone, Debug)]
pub struct UsageSummary {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
    pub request_count: i64,
}

/// Usage broken down by provider/model.
#[derive(Clone, Debug)]
pub struct ProviderUsage {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
    pub request_count: i64,
}

/// One time-bucket row for historical usage charts.
#[derive(Clone, Debug)]
pub struct UsageBucket {
    pub label: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
    pub request_count: i64,
}

/// One hour-of-day aggregate row.
#[derive(Clone, Debug)]
pub struct HourUsage {
    pub hour: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub request_count: i64,
}

/// A custom `[start, end]` date range (inclusive, `YYYY-MM-DD`, local time).
/// Used by the stats dashboard to query an arbitrary window on demand.
#[derive(Clone, Debug)]
pub struct DateRange {
    pub start: String,
    pub end: String,
}

/// Full historical usage snapshot for the stats dashboard.
#[derive(Clone, Debug)]
pub struct UsageStatsSnapshot {
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub total: UsageSummary,
    pub by_model_today: Vec<ProviderUsage>,
    pub by_model_7d: Vec<ProviderUsage>,
    pub by_model_4w: Vec<ProviderUsage>,
    pub by_model_all: Vec<ProviderUsage>,
    pub daily: Vec<UsageBucket>,
    pub weekly: Vec<UsageBucket>,
    pub monthly: Vec<UsageBucket>,
    pub all_time: Vec<UsageBucket>,
    pub yearly: Vec<UsageBucket>,
    pub hourly_today: Vec<HourUsage>,
    pub hourly_7d: Vec<HourUsage>,
    pub hourly_4w: Vec<HourUsage>,
    pub hourly_all: Vec<HourUsage>,
    pub daily_activity: Vec<UsageBucket>,
}

/// Time range selector shared between session_db and stats UI.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Today,
    SevenDays,
    FourWeeks,
    Yearly,
    Months,
}

impl ViewMode {
    const ALL: [Self; 5] = [
        Self::Today,
        Self::SevenDays,
        Self::FourWeeks,
        Self::Yearly,
        Self::Months,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&m| m == self).unwrap()
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::SevenDays => "7 days",
            Self::FourWeeks => "4 weeks",
            Self::Yearly => "Yearly",
            Self::Months => "All time",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Today => "1",
            Self::SevenDays => "2",
            Self::FourWeeks => "3",
            Self::Yearly => "4",
            Self::Months => "5",
        }
    }

    pub fn prev(self) -> Self {
        let idx = self.index();
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn next(self) -> Self {
        let idx = self.index();
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }
}

impl UsageStatsSnapshot {
    /// Select the usage buckets for a given view mode.
    pub fn buckets(&self, mode: ViewMode) -> &[UsageBucket] {
        match mode {
            ViewMode::Today => &self.daily,
            ViewMode::SevenDays => &self.weekly,
            ViewMode::FourWeeks => &self.monthly,
            ViewMode::Yearly => &self.yearly,
            ViewMode::Months => &self.all_time,
        }
    }

    /// Select hourly data for a given view mode.
    pub fn hourly(&self, mode: ViewMode) -> &[HourUsage] {
        match mode {
            ViewMode::Today => &self.hourly_today,
            ViewMode::SevenDays => &self.hourly_7d,
            ViewMode::FourWeeks => &self.hourly_4w,
            ViewMode::Yearly | ViewMode::Months => &self.hourly_all,
        }
    }

    /// Compute a summary for the given time range by aggregating buckets.
    pub fn range_summary(&self, mode: ViewMode) -> UsageSummary {
        let buckets: &[UsageBucket] = self.buckets(mode);
        let mut s = UsageSummary {
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost: 0.0,
            request_count: 0,
        };
        for b in buckets {
            s.prompt_tokens += b.prompt_tokens;
            s.completion_tokens += b.completion_tokens;
            s.cached_tokens += b.cached_tokens;
            s.cost += b.cost;
            s.request_count += b.request_count;
        }
        s
    }

    pub fn range_models(&self, mode: ViewMode) -> &[ProviderUsage] {
        match mode {
            ViewMode::Today => &self.by_model_today,
            ViewMode::SevenDays => &self.by_model_7d,
            ViewMode::FourWeeks => &self.by_model_4w,
            ViewMode::Yearly | ViewMode::Months => &self.by_model_all,
        }
    }
}

/// Full schema at the latest version, used to initialize a fresh database.
/// Existing databases are migrated forward incrementally in `setup_schema`;
/// any column or index added here must also have a corresponding ALTER step.
const FULL_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS conversations (
        id         INTEGER PRIMARY KEY,
        started_at TEXT NOT NULL,
        ended_at   TEXT,
        provider   TEXT NOT NULL,
        model      TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS messages (
        id              INTEGER PRIMARY KEY,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id),
        role            TEXT NOT NULL,
        content         TEXT NOT NULL,
        tool_name       TEXT,
        tool_call_id    TEXT,
        tool_calls      TEXT,
        images          TEXT,
        seq             INTEGER NOT NULL,
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
        is_estimated      INTEGER NOT NULL DEFAULT 0,
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

    CREATE INDEX IF NOT EXISTS idx_usage_events_created_at
        ON usage_events(created_at);

    CREATE INDEX IF NOT EXISTS idx_messages_conversation_seq
        ON messages(conversation_id, seq);
";

/// A time range applied to `usage_events.created_at` (interpreted in local time).
#[derive(Clone, Copy)]
enum TimeWindow {
    /// The current local calendar day.
    Today,
    /// The last `n` days inclusive of today.
    SinceDaysAgo(u32),
    /// No time restriction.
    AllTime,
}

impl TimeWindow {
    /// Returns the SQL `WHERE` clause (with a leading space, or empty) and the
    /// single bound parameter it requires, if any. The clause never
    /// interpolates a value — the only variable part (the day offset) is passed
    /// as a bound parameter, so there is no SQL-injection surface.
    fn clause(self) -> (&'static str, Option<String>) {
        match self {
            TimeWindow::Today => (
                " WHERE date(created_at, 'localtime') = date('now', 'localtime')",
                None,
            ),
            TimeWindow::SinceDaysAgo(n) => (
                " WHERE date(created_at, 'localtime') >= date('now', 'localtime', ?1)",
                Some(format!("-{n} days")),
            ),
            TimeWindow::AllTime => ("", None),
        }
    }
}

/// Aggregate column list for a `(prompt, completion, cached, cost, count)` row,
/// consumed by [`SessionDb::read_summary_row`] / [`SessionDb::read_provider_row`].
const SUM_COLS: &str = "COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), \
     COALESCE(SUM(cached_tokens),0), COALESCE(SUM(cost),0.0), COUNT(*)";

/// Aggregate column list for a `(prompt, completion, cached, count)` row (no
/// cost), consumed by [`SessionDb::read_hour_row`].
const HOUR_SUM_COLS: &str = "COALESCE(SUM(prompt_tokens),0), \
     COALESCE(SUM(completion_tokens),0), COALESCE(SUM(cached_tokens),0), COUNT(*)";

/// Aliased aggregate columns for the inner `usage` CTE of a bucket query.
const BUCKET_AGG_COLS: &str = "COALESCE(SUM(prompt_tokens),0) AS prompt, \
     COALESCE(SUM(completion_tokens),0) AS completion, \
     COALESCE(SUM(cached_tokens),0) AS cached, \
     COALESCE(SUM(cost),0.0) AS cost, COUNT(*) AS requests";

/// Final projection pulling the gap-filled aggregates out of the `usage` CTE,
/// in the column order [`SessionDb::read_usage_bucket_row`] expects after the
/// bucket label.
const BUCKET_PROJECTION: &str = "COALESCE(usage.prompt,0), COALESCE(usage.completion,0), \
     COALESCE(usage.cached,0), COALESCE(usage.cost,0.0), COALESCE(usage.requests,0)";

/// SQLite-backed conversation and usage storage.
pub struct SessionDb {
    conn: Connection,
}

impl SessionDb {
    /// Borrow the inner connection for raw queries.
    pub fn conn_ref(&self) -> &Connection {
        &self.conn
    }

    /// Open (or create) the database at the given path and run schema setup.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        let db = Self { conn };
        db.setup_schema()?;
        Ok(db)
    }

    fn setup_schema(&self) -> rusqlite::Result<()> {
        /// Return true if `table` already has a column named `column`.
        fn column_exists(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
            conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                params![table, column],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
        }

        const SCHEMA_VERSION: u32 = 5;

        let current_version: u32 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?;

        if current_version == SCHEMA_VERSION {
            return Ok(());
        }

        // Data-preserving migration chain. A fresh database (user_version 0)
        // gets the full latest schema; existing databases step forward one
        // version at a time via ALTER. A database newer than this binary
        // (current_version > SCHEMA_VERSION) is left untouched rather than
        // destroyed. The whole chain runs in one transaction so a failure
        // never leaves a half-migrated database.
        let tx = self.conn.unchecked_transaction()?;
        let mut version = current_version;

        if version == 0 {
            // user_version defaults to 0 for BOTH a brand-new database and a
            // pre-versioning legacy database. Distinguish them by checking
            // whether our tables already exist. A truly empty database gets the
            // latest schema; a populated unversioned database is left entirely
            // untouched and the user is asked to recreate it — we never silently
            // stamp it current (which would skip the is_estimated column) and we
            // never drop their data.
            let has_app_tables: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master
                     WHERE type = 'table' AND name = 'conversations'
                 )",
                [],
                |row| row.get(0),
            )?;
            if has_app_tables {
                // Refuse to proceed; the transaction rolls back on drop, so the
                // database is left exactly as it was.
                return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                    std::io::Error::other(
                        "session database predates schema versioning (user_version = 0) \
                         and cannot be migrated automatically. Back up and remove the \
                         database file so a fresh one can be created.",
                    ),
                )));
            }
            tx.execute_batch(FULL_SCHEMA)?;
            version = SCHEMA_VERSION;
        }

        if version == 1 {
            // v1 -> v2: track whether usage was estimated vs. provider-reported.
            tx.execute_batch(
                "ALTER TABLE usage_events
                     ADD COLUMN is_estimated INTEGER NOT NULL DEFAULT 0;",
            )?;
            version = 2;
        }

        if version == 2 {
            // v2 -> v3: index usage_events by created_at for time-range queries.
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_usage_events_created_at
                     ON usage_events(created_at);",
            )?;
            version = 3;
        }

        if version == 3 {
            // v3 -> v4: add tool_calls column (JSON array of tool call objects).
            // Guard the ALTER: some pre-versioning dev databases already carry
            // this column while still reporting user_version = 3, and an
            // unconditional ADD COLUMN fails with "duplicate column name".
            if !column_exists(&tx, "messages", "tool_calls")? {
                tx.execute_batch("ALTER TABLE messages ADD COLUMN tool_calls TEXT;")?;
            }
            version = 4;
        }

        if version == 4 {
            // v4 -> v5: add images column (JSON array of {media_type, data}).
            if !column_exists(&tx, "messages", "images")? {
                tx.execute_batch("ALTER TABLE messages ADD COLUMN images TEXT;")?;
            }
            version = 5;
        }

        if version != current_version {
            tx.pragma_update(None, "user_version", version)?;
        }
        tx.commit()?;

        Ok(())
    }

    /// Create a new conversation and return its id.
    pub fn create_conversation(&self, provider: &str, model: &str) -> rusqlite::Result<i64> {
        let now = now_iso();
        self.conn.execute(
            "INSERT INTO conversations (started_at, provider, model) VALUES (?1, ?2, ?3)",
            params![now, provider, model],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Append a message to a conversation (inserts into both messages and messages_fts).
    #[allow(clippy::too_many_arguments)]
    pub fn append_message(
        &self,
        conversation_id: i64,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls: Option<&str>,
        images: Option<&str>,
        seq: i64,
    ) -> rusqlite::Result<i64> {
        let now = now_iso();
        self.conn.execute(
            "INSERT INTO messages (conversation_id, role, content, tool_name, tool_call_id, tool_calls, images, seq, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![conversation_id, role, content, tool_name, tool_call_id, tool_calls, images, seq, now],
        )?;
        let msg_id = self.conn.last_insert_rowid();
        // Build searchable text: include tool call info in FTS so it's findable
        let searchable = if let Some(tc) = tool_calls {
            format!("{content} TOOL_CALL {tc}")
        } else {
            content.to_string()
        };
        self.conn.execute(
            "INSERT INTO messages_fts (rowid, content, role, conversation_id) VALUES (?1, ?2, ?3, ?4)",
            params![msg_id, searchable, role, conversation_id],
        )?;
        Ok(msg_id)
    }

    /// Record a usage event.
    #[allow(clippy::too_many_arguments)]
    pub fn record_usage(
        &self,
        conversation_id: i64,
        provider: &str,
        model: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: Option<u32>,
        cost: Option<f64>,
        is_estimated: bool,
    ) -> rusqlite::Result<()> {
        let now = now_iso();
        self.conn.execute(
            "INSERT INTO usage_events (conversation_id, provider, model, prompt_tokens, completion_tokens, cached_tokens, cost, is_estimated, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                conversation_id,
                provider,
                model,
                prompt_tokens as i64,
                completion_tokens as i64,
                cached_tokens.unwrap_or(0) as i64,
                cost.unwrap_or(0.0),
                is_estimated as i64,
                now,
            ],
        )?;
        Ok(())
    }

    /// Mark a conversation as ended.
    pub fn end_conversation(&self, conversation_id: i64) -> rusqlite::Result<()> {
        let now = now_iso();
        self.conn.execute(
            "UPDATE conversations SET ended_at = ?1 WHERE id = ?2",
            params![now, conversation_id],
        )?;
        Ok(())
    }

    /// Clear the `ended_at` marker so a previously-ended conversation can be
    /// resumed (e.g. when `/history` loads it as the active chat).
    pub fn reopen_conversation(&self, conversation_id: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE conversations SET ended_at = NULL WHERE id = ?1",
            params![conversation_id],
        )?;
        Ok(())
    }

    /// Highest `seq` stored for a conversation, or 0 if it has no messages.
    /// Used to continue seq numbering when resuming a conversation.
    #[cfg_attr(not(feature = "ui"), allow(dead_code))]
    pub(crate) fn max_message_seq(&self, conversation_id: i64) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )
    }

    /// Get usage broken down by provider/model for a conversation.
    pub fn usage_by_provider(&self, conversation_id: i64) -> rusqlite::Result<Vec<ProviderUsage>> {
        let mut stmt = self.conn.prepare(
            "SELECT provider, model, COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(cached_tokens),0), COALESCE(SUM(cost),0.0), COUNT(*) FROM usage_events WHERE conversation_id = ?1 GROUP BY provider, model"
        )?;
        let rows = stmt.query_map(params![conversation_id], |row| {
            Ok(ProviderUsage {
                provider: row.get(0)?,
                model: row.get(1)?,
                prompt_tokens: row.get(2)?,
                completion_tokens: row.get(3)?,
                cached_tokens: row.get(4)?,
                cost: row.get(5)?,
                request_count: row.get(6)?,
            })
        })?;
        rows.collect()
    }

    /// Get a full historical usage snapshot for the native stats dashboard.
    pub fn usage_stats_snapshot(&self) -> rusqlite::Result<UsageStatsSnapshot> {
        let (started_at, ended_at): (Option<String>, Option<String>) = self.conn.query_row(
            "SELECT datetime(MIN(created_at), 'localtime'), datetime(MAX(created_at), 'localtime') FROM usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let total = self.conn.query_row(
            &format!("SELECT {SUM_COLS} FROM usage_events"),
            [],
            Self::read_summary_row,
        )?;

        let by_model_today = self.usage_by_model_since(TimeWindow::Today)?;
        let by_model_7d = self.usage_by_model_since(TimeWindow::SinceDaysAgo(6))?;
        let by_model_4w = self.usage_by_model_since(TimeWindow::SinceDaysAgo(27))?;
        let by_model_all = self.usage_by_model_since(TimeWindow::AllTime)?;
        let daily = self.usage_today_by_hour()?;
        let weekly = self.usage_recent_days(7)?;
        let monthly = self.usage_recent_weeks(4)?;
        let all_time = self.usage_buckets(36)?;
        let yearly = self.usage_by_year()?;
        let hourly_today = self.usage_by_hour_since(TimeWindow::Today)?;
        let hourly_7d = self.usage_by_hour_since(TimeWindow::SinceDaysAgo(6))?;
        let hourly_4w = self.usage_by_hour_since(TimeWindow::SinceDaysAgo(27))?;
        let hourly_all = self.usage_by_hour_since(TimeWindow::AllTime)?;
        let daily_activity = self.usage_recent_days(730)?;

        Ok(UsageStatsSnapshot {
            started_at,
            ended_at,
            total,
            by_model_today,
            by_model_7d,
            by_model_4w,
            by_model_all,
            daily,
            weekly,
            monthly,
            all_time,
            yearly,
            hourly_today,
            hourly_7d,
            hourly_4w,
            hourly_all,
            daily_activity,
        })
    }

    fn usage_by_model_since(&self, window: TimeWindow) -> rusqlite::Result<Vec<ProviderUsage>> {
        let (where_clause, param) = window.clause();
        let sql = format!(
            "SELECT provider, model, {SUM_COLS} \
             FROM usage_events{where_clause} \
             GROUP BY provider, model \
             ORDER BY (COALESCE(SUM(prompt_tokens),0) + COALESCE(SUM(completion_tokens),0)) DESC"
        );
        let params: Vec<String> = param.into_iter().collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&params), Self::read_provider_row)?;
        rows.collect()
    }

    fn read_usage_bucket_row(row: &rusqlite::Row) -> rusqlite::Result<UsageBucket> {
        Ok(UsageBucket {
            label: row.get(0)?,
            prompt_tokens: row.get(1)?,
            completion_tokens: row.get(2)?,
            cached_tokens: row.get(3)?,
            cost: row.get(4)?,
            request_count: row.get(5)?,
        })
    }

    /// Map a `(prompt, completion, cached, cost, count)` row — the [`SUM_COLS`]
    /// aggregate — into a [`UsageSummary`].
    fn read_summary_row(row: &rusqlite::Row) -> rusqlite::Result<UsageSummary> {
        Ok(UsageSummary {
            prompt_tokens: row.get(0)?,
            completion_tokens: row.get(1)?,
            cached_tokens: row.get(2)?,
            cost: row.get(3)?,
            request_count: row.get(4)?,
        })
    }

    /// Map a `(provider, model, prompt, completion, cached, cost, count)` row
    /// into a [`ProviderUsage`].
    fn read_provider_row(row: &rusqlite::Row) -> rusqlite::Result<ProviderUsage> {
        Ok(ProviderUsage {
            provider: row.get(0)?,
            model: row.get(1)?,
            prompt_tokens: row.get(2)?,
            completion_tokens: row.get(3)?,
            cached_tokens: row.get(4)?,
            cost: row.get(5)?,
            request_count: row.get(6)?,
        })
    }

    /// Map a `(hour, prompt, completion, cached, count)` row — the
    /// [`HOUR_SUM_COLS`] aggregate — into a [`HourUsage`].
    fn read_hour_row(row: &rusqlite::Row) -> rusqlite::Result<HourUsage> {
        Ok(HourUsage {
            hour: row.get(0)?,
            prompt_tokens: row.get(1)?,
            completion_tokens: row.get(2)?,
            cached_tokens: row.get(3)?,
            request_count: row.get(4)?,
        })
    }

    fn query_buckets(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> rusqlite::Result<Vec<UsageBucket>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, Self::read_usage_bucket_row)?;
        rows.collect()
    }

    fn usage_today_by_hour(&self) -> rusqlite::Result<Vec<UsageBucket>> {
        self.query_buckets(
            &format!(
                "WITH RECURSIVE hours(hour) AS (
                VALUES(0)
                UNION ALL SELECT hour + 1 FROM hours WHERE hour < 23
             ), usage AS (
                SELECT CAST(strftime('%H', created_at, 'localtime') AS INTEGER) AS hour, {BUCKET_AGG_COLS}
                FROM usage_events
                WHERE date(created_at, 'localtime') = date('now', 'localtime')
                GROUP BY hour
             )
             SELECT printf('%02d:00', hours.hour), {BUCKET_PROJECTION}
             FROM hours
             LEFT JOIN usage ON usage.hour = hours.hour
             ORDER BY hours.hour ASC"
            ),
            [],
        )
    }

    fn usage_recent_days(&self, days: i64) -> rusqlite::Result<Vec<UsageBucket>> {
        let modifier = format!("-{} days", days.saturating_sub(1));
        self.query_buckets(
            &format!(
                "WITH RECURSIVE series(n, day) AS (
                VALUES(0, date('now', 'localtime', ?1))
                UNION ALL SELECT n + 1, date(day, '+1 day') FROM series WHERE n + 1 < ?2
             ), usage AS (
                SELECT date(created_at, 'localtime') AS day, {BUCKET_AGG_COLS}
                FROM usage_events
                WHERE date(created_at, 'localtime') >= date('now', 'localtime', ?1)
                GROUP BY day
             )
             SELECT series.day, {BUCKET_PROJECTION}
             FROM series
             LEFT JOIN usage ON usage.day = series.day
             ORDER BY series.day ASC"
            ),
            params![modifier, days],
        )
    }

    fn usage_recent_weeks(&self, weeks: i64) -> rusqlite::Result<Vec<UsageBucket>> {
        let first_label_modifier = format!("-{} days", weeks.saturating_sub(1).saturating_mul(7));
        let usage_modifier = format!("-{} days", weeks.saturating_mul(7).saturating_sub(1));
        self.query_buckets(
            &format!(
                "WITH RECURSIVE series(n, week) AS (
                VALUES(0, strftime('%Y-W%W', date('now', 'localtime', ?1)))
                UNION ALL
                SELECT n + 1, strftime('%Y-W%W', date('now', 'localtime', printf('-%d days', (?2 - n - 2) * 7)))
                FROM series WHERE n + 1 < ?2
             ), usage AS (
                SELECT strftime('%Y-W%W', created_at, 'localtime') AS week, {BUCKET_AGG_COLS}
                FROM usage_events
                WHERE date(created_at, 'localtime') >= date('now', 'localtime', ?3)
                GROUP BY week
             )
             SELECT series.week, {BUCKET_PROJECTION}
             FROM series
             LEFT JOIN usage ON usage.week = series.week
             ORDER BY series.n ASC"
            ),
            params![first_label_modifier, weeks, usage_modifier],
        )
    }

    fn usage_buckets(&self, limit: i64) -> rusqlite::Result<Vec<UsageBucket>> {
        let modifier = format!("-{} months", limit.saturating_sub(1));
        self.query_buckets(
            &format!(
                "WITH RECURSIVE series(n, month) AS (
                VALUES(0, strftime('%Y-%m', date('now', 'localtime', ?1)))
                UNION ALL
                SELECT n + 1, strftime('%Y-%m', date(month || '-01', '+1 month'))
                FROM series WHERE n + 1 < ?2
             ), usage AS (
                SELECT strftime('%Y-%m', created_at, 'localtime') AS month, {BUCKET_AGG_COLS}
                FROM usage_events
                GROUP BY month
             )
             SELECT series.month, {BUCKET_PROJECTION}
             FROM series
             LEFT JOIN usage ON usage.month = series.month
             ORDER BY series.month ASC"
            ),
            params![modifier, limit],
        )
    }

    /// All-time usage grouped by calendar year (`YYYY`), oldest first.
    fn usage_by_year(&self) -> rusqlite::Result<Vec<UsageBucket>> {
        self.query_buckets(
            &format!(
                "SELECT strftime('%Y', created_at, 'localtime') AS year, {SUM_COLS} \
                 FROM usage_events GROUP BY year ORDER BY year ASC"
            ),
            [],
        )
    }

    /// Build a snapshot scoped to a custom `[start, end]` date range. Only the
    /// fields the dashboard consults in custom-range mode are populated; the
    /// daily buckets, model breakdown and hourly histogram carry the range, and
    /// `total` aggregates the window.
    pub fn usage_stats_range(
        &self,
        start: &str,
        end: &str,
    ) -> rusqlite::Result<UsageStatsSnapshot> {
        let daily = self.usage_range_days(start, end)?;
        let by_model_today = self.usage_by_model_range(start, end)?;
        let hourly_today = self.usage_by_hour_range(start, end)?;

        let (started_at, ended_at): (Option<String>, Option<String>) = self.conn.query_row(
            "SELECT datetime(MIN(created_at), 'localtime'), datetime(MAX(created_at), 'localtime') FROM usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let total = self.conn.query_row(
            &format!(
                "SELECT {SUM_COLS} FROM usage_events \
                 WHERE date(created_at, 'localtime') BETWEEN ?1 AND ?2"
            ),
            params![start, end],
            Self::read_summary_row,
        )?;

        let empty = Vec::new();
        Ok(UsageStatsSnapshot {
            started_at,
            ended_at,
            total,
            by_model_today,
            by_model_7d: empty.clone(),
            by_model_4w: empty.clone(),
            by_model_all: empty,
            daily: daily.clone(),
            weekly: Vec::new(),
            monthly: Vec::new(),
            all_time: Vec::new(),
            yearly: Vec::new(),
            hourly_today,
            hourly_7d: Vec::new(),
            hourly_4w: Vec::new(),
            hourly_all: Vec::new(),
            daily_activity: daily.clone(),
        })
    }

    /// Daily buckets for an explicit `[start, end]` inclusive range.
    fn usage_range_days(&self, start: &str, end: &str) -> rusqlite::Result<Vec<UsageBucket>> {
        self.query_buckets(
            &format!(
                "WITH RECURSIVE series(n, day) AS (
                VALUES(0, date(?1))
                UNION ALL SELECT n + 1, date(day, '+1 day')
                FROM series WHERE date(day, '+1 day') <= date(?2)
             ), usage AS (
                SELECT date(created_at, 'localtime') AS day, {BUCKET_AGG_COLS}
                FROM usage_events
                WHERE date(created_at, 'localtime') BETWEEN ?1 AND ?2
                GROUP BY day
             )
             SELECT series.day, {BUCKET_PROJECTION}
             FROM series
             LEFT JOIN usage ON usage.day = series.day
             ORDER BY series.day ASC"
            ),
            params![start, end],
        )
    }

    /// Provider/model breakdown for an explicit `[start, end]` range.
    fn usage_by_model_range(&self, start: &str, end: &str) -> rusqlite::Result<Vec<ProviderUsage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT provider, model, {SUM_COLS} \
             FROM usage_events \
             WHERE date(created_at, 'localtime') BETWEEN ?1 AND ?2 \
             GROUP BY provider, model \
             ORDER BY (COALESCE(SUM(prompt_tokens),0) + COALESCE(SUM(completion_tokens),0)) DESC",
        ))?;
        let rows = stmt.query_map(params![start, end], Self::read_provider_row)?;
        rows.collect()
    }

    /// Hour-of-day histogram for an explicit `[start, end]` range.
    fn usage_by_hour_range(&self, start: &str, end: &str) -> rusqlite::Result<Vec<HourUsage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT CAST(strftime('%H', created_at, 'localtime') AS INTEGER), {HOUR_SUM_COLS} \
             FROM usage_events \
             WHERE date(created_at, 'localtime') BETWEEN ?1 AND ?2 \
             GROUP BY 1 ORDER BY 1",
        ))?;
        let rows = stmt.query_map(params![start, end], Self::read_hour_row)?;
        rows.collect()
    }

    fn usage_by_hour_since(&self, window: TimeWindow) -> rusqlite::Result<Vec<HourUsage>> {
        let (where_clause, param) = window.clause();
        let sql = format!(
            "SELECT CAST(strftime('%H', created_at, 'localtime') AS INTEGER), {HOUR_SUM_COLS} \
             FROM usage_events{where_clause} GROUP BY 1 ORDER BY 1"
        );
        let params: Vec<String> = param.into_iter().collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(&params), Self::read_hour_row)?;
        rows.collect()
    }

    /// Full-text search across all conversations.
    /// List recent conversations, most recent first.
    pub(crate) fn list_conversations(
        &self,
        limit: usize,
    ) -> rusqlite::Result<Vec<ConversationSummary>> {
        let limit = limit.clamp(1, 100);
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, model, started_at, ended_at \
             FROM conversations ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(ConversationSummary {
                id: row.get(0)?,
                provider: row.get(1)?,
                model: row.get(2)?,
                started_at: row.get(3)?,
                ended_at: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// List messages for a conversation, ordered by seq ascending.
    pub(crate) fn list_messages(
        &self,
        conversation_id: i64,
        limit: usize,
    ) -> rusqlite::Result<Vec<StoredMessage>> {
        let limit = limit.clamp(1, 1000);
        let mut stmt = self.conn.prepare(
            "SELECT seq, role, content, tool_name, tool_call_id, tool_calls, images \
             FROM messages WHERE conversation_id = ?1 \
             ORDER BY seq ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![conversation_id, limit as i64], |row| {
            Ok(StoredMessage {
                seq: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                tool_name: row.get(3)?,
                tool_call_id: row.get(4)?,
                tool_calls: row.get(5)?,
                images: row.get(6)?,
            })
        })?;
        rows.collect()
    }
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    iso_from_unix_secs(secs)
}

fn iso_from_unix_secs(secs: u64) -> String {
    let days = secs / 86400;
    let tod = secs % 86400;
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Convert days since 1970-01-01 to (year, month, day) using
/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
#[path = "session_db_tests.rs"]
mod session_db_tests;
