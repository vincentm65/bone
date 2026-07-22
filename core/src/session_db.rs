//! SQLite persistence for conversations and per-provider usage records.

use crate::llm::{ChatMessage, ChatRole};
use crate::runtime::UsageRecord;
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Returns the path to the conversations database.
/// Centralizes the path so all callers (TUI, headless, stats-popup) stay in sync.
/// Uses [`crate::config::bone_dir`] so XDG/`HOME` isolation matches config/Lua.
/// One-time: when using the default/XDG root, copy the pre-XDG legacy database
/// with SQLite's backup API so existing history isn't orphaned.
pub fn db_path() -> std::path::PathBuf {
    db_path_with_legacy(legacy_db_path().as_deref())
}

fn db_path_with_legacy(legacy: Option<&Path>) -> PathBuf {
    let path = crate::config::bone_dir()
        .join("data")
        .join("conversations.db");
    let explicit_root = matches!(std::env::var("BONE_DIR"), Ok(dir) if !dir.is_empty());
    if !explicit_root
        && let Some(legacy) = legacy
        && migrate_legacy_db_if_needed(legacy, &path).is_err()
    {
        // Keep using the legacy database when snapshotting fails. Returning the
        // new path would let SessionDb::open create an empty database there and
        // suppress every future migration attempt.
        return legacy.to_path_buf();
    }
    path
}

/// Pre-unification location: always `~/.bone-rust/data/conversations.db`,
/// ignoring `XDG_CONFIG_HOME`. Kept only for the one-shot migrate.
fn legacy_db_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".bone-rust/data/conversations.db"))
}

fn migrate_legacy_db_if_needed(
    legacy: &Path,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() || !legacy.exists() || legacy == path {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session database path has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;

    // Serialize migration across Bone processes. The destination only appears
    // after SQLite has produced and closed a complete snapshot, so no peer can
    // open a partially copied database.
    let lock_path = parent.join(".conversations.db.migrate.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    if path.exists() {
        return Ok(());
    }

    let source = Connection::open_with_flags(legacy, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let temp = tempfile::NamedTempFile::new_in(parent)?;
    let mut destination = Connection::open(temp.path())?;
    let backup = rusqlite::backup::Backup::new(&source, &mut destination)?;
    backup.run_to_completion(100, Duration::from_millis(10), None)?;
    drop(backup);
    drop(destination);
    drop(source);

    std::fs::set_permissions(temp.path(), std::fs::metadata(legacy)?.permissions())?;
    if path.exists() {
        return Ok(());
    }
    temp.persist(path)?;
    Ok(())
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
    /// True if this is a tool result that errored.
    pub is_error: bool,
}

/// Convert a stored DB row into a provider-neutral [`ChatMessage`], parsing
/// tool-calls and images from their JSON columns. Used by the daemon's
/// `LoadConversation` handler to rebuild a transcript from the session DB.
pub(crate) fn stored_to_chat_message(msg: StoredMessage) -> crate::llm::ChatMessage {
    use crate::llm::{ChatMessage, ChatRole};

    let role = match msg.role.as_str() {
        "assistant" => ChatRole::Assistant,
        "tool" => ChatRole::Tool,
        "system" => ChatRole::System,
        _ => ChatRole::User,
    };
    let tool_calls = msg
        .tool_calls
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    let images = msg
        .images
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    ChatMessage {
        role,
        content: msg.content,
        images,
        tool_calls,
        tool_call_id: msg.tool_call_id,
        name: msg.tool_name,
        is_error: msg.is_error,
        reasoning: None,
        reasoning_items: Vec::new(),
        output_sequence: Vec::new(),
    }
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
        is_error        INTEGER NOT NULL DEFAULT 0,
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

    CREATE TABLE IF NOT EXISTS conversation_context_checkpoints (
        id              INTEGER PRIMARY KEY,
        conversation_id INTEGER NOT NULL REFERENCES conversations(id),
        through_seq     INTEGER NOT NULL,
        messages_json   TEXT NOT NULL,
        created_at      TEXT NOT NULL
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

    CREATE INDEX IF NOT EXISTS idx_context_checkpoints_conversation
        ON conversation_context_checkpoints(conversation_id, id DESC);
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

/// Latest conversations.db schema version. Bumped when `setup_schema` gains a
/// new migration step; tests assert against this instead of a bare literal.
pub(crate) const SCHEMA_VERSION: u32 = 9;

const STARTUP_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const STARTUP_RETRY_DEADLINE: Duration = Duration::from_secs(3);
const NORMAL_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Database operation being performed when runtime startup failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupDbOperation {
    Open,
    SchemaSetup,
    Prune,
    ReadConversations,
    CreateConversation,
    LoadConversation,
}

impl std::fmt::Display for StartupDbOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Open => "open",
            Self::SchemaSetup => "schema setup",
            Self::Prune => "prune ended empty conversations",
            Self::ReadConversations => "read conversations",
            Self::CreateConversation => "create conversation",
            Self::LoadConversation => "load conversation",
        };
        f.write_str(name)
    }
}

/// Structured session-database startup failure.
#[derive(Debug)]
pub struct StartupDbError {
    pub operation: StartupDbOperation,
    pub path: PathBuf,
    pub elapsed: Duration,
    source: StartupDbErrorSource,
}

#[derive(Debug)]
enum StartupDbErrorSource {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

impl StartupDbError {
    fn sqlite(
        operation: StartupDbOperation,
        path: &Path,
        elapsed: Duration,
        source: rusqlite::Error,
    ) -> Self {
        Self {
            operation,
            path: path.to_path_buf(),
            elapsed,
            source: StartupDbErrorSource::Sqlite(source),
        }
    }

    fn io(operation: StartupDbOperation, path: &Path, source: std::io::Error) -> Self {
        Self {
            operation,
            path: path.to_path_buf(),
            elapsed: Duration::ZERO,
            source: StartupDbErrorSource::Io(source),
        }
    }

    pub(crate) fn from_sqlite(
        operation: StartupDbOperation,
        path: &Path,
        source: rusqlite::Error,
    ) -> Self {
        Self::sqlite(operation, path, Duration::ZERO, source)
    }

    /// Primary and extended SQLite result codes, when the source is SQLite.
    pub fn sqlite_codes(&self) -> Option<(rusqlite::ErrorCode, i32)> {
        match &self.source {
            StartupDbErrorSource::Sqlite(error) => error
                .sqlite_error()
                .map(|code| (code.code, code.extended_code)),
            StartupDbErrorSource::Io(_) => None,
        }
    }

    pub fn is_transient_contention(&self) -> bool {
        self.sqlite_codes().is_some_and(|(code, _)| {
            matches!(
                code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
        })
    }
}

impl std::fmt::Display for StartupDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session database {} failed for {} after {:.3}s",
            self.operation,
            self.path.display(),
            self.elapsed.as_secs_f64()
        )?;
        if let Some((code, extended)) = self.sqlite_codes() {
            write!(
                f,
                ": SQLite {:?} (extended code {extended}): {}",
                code, self.source
            )
        } else {
            write!(f, ": {}", self.source)
        }
    }
}

impl std::fmt::Display for StartupDbErrorSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for StartupDbError {}

fn is_transient_sqlite(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn retry_startup_sqlite<T>(
    operation: StartupDbOperation,
    path: &Path,
    action: impl FnMut() -> rusqlite::Result<T>,
) -> Result<T, StartupDbError> {
    retry_startup_sqlite_with_deadline(operation, path, STARTUP_RETRY_DEADLINE, action)
}

fn retry_startup_sqlite_with_deadline<T>(
    operation: StartupDbOperation,
    path: &Path,
    deadline: Duration,
    mut action: impl FnMut() -> rusqlite::Result<T>,
) -> Result<T, StartupDbError> {
    let started = Instant::now();
    let mut backoff = Duration::from_millis(20);
    loop {
        match action() {
            Ok(value) => return Ok(value),
            Err(error) if is_transient_sqlite(&error) && started.elapsed() < deadline => {
                let remaining = deadline.saturating_sub(started.elapsed());
                std::thread::sleep(backoff.min(remaining));
                backoff = (backoff * 2).min(Duration::from_millis(200));
            }
            Err(error) => {
                return Err(StartupDbError::sqlite(
                    operation,
                    path,
                    started.elapsed(),
                    error,
                ));
            }
        }
    }
}

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
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;",
        )?;
        let db = Self { conn };
        db.setup_schema()?;
        db.prune_ended_empty_conversations()?;
        Ok(db)
    }

    /// Open the database while preserving startup operation and contention
    /// details. Startup uses short SQLite waits plus a single bounded retry
    /// deadline; normal runtime operations return to the five-second timeout.
    pub fn open_for_startup(path: &Path) -> Result<Self, StartupDbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| StartupDbError::io(StartupDbOperation::Open, path, error))?;
        }
        let conn = Connection::open(path).map_err(|error| {
            StartupDbError::sqlite(StartupDbOperation::Open, path, Duration::ZERO, error)
        })?;
        conn.busy_timeout(STARTUP_BUSY_TIMEOUT).map_err(|error| {
            StartupDbError::sqlite(StartupDbOperation::Open, path, Duration::ZERO, error)
        })?;
        retry_startup_sqlite(StartupDbOperation::Open, path, || {
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        })?;
        let db = Self { conn };
        retry_startup_sqlite(StartupDbOperation::SchemaSetup, path, || db.setup_schema())?;
        retry_startup_sqlite(StartupDbOperation::Prune, path, || {
            db.prune_ended_empty_conversations()
        })?;
        db.conn.busy_timeout(NORMAL_BUSY_TIMEOUT).map_err(|error| {
            StartupDbError::sqlite(StartupDbOperation::Open, path, Duration::ZERO, error)
        })?;
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

        // Acquire the migration write lock before reading user_version. Without
        // this, two startups can both observe an old version and the loser can
        // resume with stale migration state after the winner commits.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let current_version: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;

        if current_version == SCHEMA_VERSION {
            return tx.commit();
        }

        // Data-preserving migration chain. A fresh database (user_version 0)
        // gets the full latest schema; existing databases step forward one
        // version at a time via ALTER. A database newer than this binary
        // (current_version > SCHEMA_VERSION) is left untouched rather than
        // destroyed. The whole chain runs in one transaction so a failure
        // never leaves a half-migrated database.
        let mut version = current_version;

        if version == 0 {
            // user_version defaults to 0 for BOTH a brand-new database and a
            // pre-versioning legacy database. Distinguish them by checking
            // whether our tables already exist. A truly empty database gets the
            // latest schema. A populated unversioned database is upgraded in
            // place by adding only missing tables, columns, and indexes.
            let has_app_tables: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master
                     WHERE type = 'table' AND name = 'conversations'
                 )",
                [],
                |row| row.get(0),
            )?;
            if has_app_tables {
                tx.execute_batch(FULL_SCHEMA)?;
                if !column_exists(&tx, "usage_events", "is_estimated")? {
                    tx.execute_batch(
                        "ALTER TABLE usage_events
                             ADD COLUMN is_estimated INTEGER NOT NULL DEFAULT 0;",
                    )?;
                }
                if !column_exists(&tx, "messages", "tool_calls")? {
                    tx.execute_batch("ALTER TABLE messages ADD COLUMN tool_calls TEXT;")?;
                }
                if !column_exists(&tx, "messages", "images")? {
                    tx.execute_batch("ALTER TABLE messages ADD COLUMN images TEXT;")?;
                }
                if !column_exists(&tx, "messages", "is_error")? {
                    tx.execute_batch(
                        "ALTER TABLE messages ADD COLUMN is_error INTEGER NOT NULL DEFAULT 0;",
                    )?;
                }
                version = 6;
            } else {
                tx.execute_batch(FULL_SCHEMA)?;
                version = 6;
            }
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

        if version == 5 {
            // v5 -> v6: track whether a tool result errored, so restored
            // scrollback can style failed tool rows as errors.
            if !column_exists(&tx, "messages", "is_error")? {
                tx.execute_batch(
                    "ALTER TABLE messages ADD COLUMN is_error INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
            version = 6;
        }

        if version == 6 {
            // Rebuild only the derived search index. Older writers could leave
            // missing/stale FTS rowids; authoritative conversation data is not
            // deleted or rewritten by this migration.
            tx.execute_batch(
                "DELETE FROM messages_fts;
                 INSERT INTO messages_fts (rowid, content, role, conversation_id)
                 SELECT id,
                        CASE WHEN tool_calls IS NOT NULL
                             THEN content || ' TOOL_CALL ' || tool_calls
                             ELSE content END,
                        role, conversation_id
                 FROM messages;",
            )?;
            version = 7;
        }

        if version == 7 {
            // Preserve every message while repairing historical sequence
            // collisions. `id` is the stable insertion-order tie-breaker used
            // by replay, so renumbering only affected conversations makes that
            // order explicit. The unique index then prevents any writer from
            // reintroducing ambiguous sequence values.
            tx.execute_batch(
                "CREATE TEMP TABLE message_seq_repair (
                     id INTEGER PRIMARY KEY,
                     new_seq INTEGER NOT NULL
                 );
                 INSERT INTO message_seq_repair (id, new_seq)
                 SELECT id,
                        ROW_NUMBER() OVER (
                            PARTITION BY conversation_id ORDER BY seq, id
                        )
                 FROM messages
                 WHERE conversation_id IN (
                     SELECT conversation_id FROM messages
                     GROUP BY conversation_id
                     HAVING COUNT(*) != COUNT(DISTINCT seq)
                 );
                 UPDATE messages
                 SET seq = (SELECT new_seq FROM message_seq_repair
                            WHERE message_seq_repair.id = messages.id)
                 WHERE id IN (SELECT id FROM message_seq_repair);
                 DROP TABLE message_seq_repair;
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_conversation_seq_unique
                     ON messages(conversation_id, seq);",
            )?;
            version = 8;
        }

        if version == 8 {
            // Keep the immutable transcript in `messages`, while making the
            // derived, model-facing context durable across process restarts.
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS conversation_context_checkpoints (
                     id              INTEGER PRIMARY KEY,
                     conversation_id INTEGER NOT NULL REFERENCES conversations(id),
                     through_seq     INTEGER NOT NULL,
                     messages_json   TEXT NOT NULL,
                     created_at      TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_context_checkpoints_conversation
                     ON conversation_context_checkpoints(conversation_id, id DESC);",
            )?;
            version = 9;
        }

        if version != current_version {
            tx.pragma_update(None, "user_version", version)?;
        }
        tx.commit()?;

        Ok(())
    }

    /// The most recent conversation and whether it holds any messages, or
    /// `None` when the database is empty. Used at boot to resume the last
    /// conversation in place (a non-empty one) or recycle a trailing empty row
    /// instead of minting a fresh conversation on every launch.
    pub fn latest_conversation(&self) -> rusqlite::Result<Option<(i64, bool)>> {
        self.conn
            .query_row(
                "SELECT c.id, EXISTS(SELECT 1 FROM messages m WHERE m.conversation_id = c.id) \
                 FROM conversations c ORDER BY c.id DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
    }

    /// Whether a durable conversation row exists.
    pub fn conversation_exists(&self, id: i64) -> rusqlite::Result<bool> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE id = ?1)",
            params![id],
            |row| row.get(0),
        )
    }

    /// The provider id and model a conversation was created with.
    pub fn conversation_provider_model(
        &self,
        id: i64,
    ) -> rusqlite::Result<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT provider, model FROM conversations WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// Point a conversation's stored provider/model at the currently active
    /// provider. Called when switching provider so the sidebar and reopen path
    /// reflect the provider a chat is actually using, not the boot default it
    /// was minted with.
    pub fn set_conversation_provider(
        &self,
        id: i64,
        provider: &str,
        model: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE conversations SET provider = ?2, model = ?3 WHERE id = ?1",
            params![id, provider, model],
        )?;
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

    /// Create the startup conversation with bounded retries for SQLite lock
    /// contention only.
    pub fn create_conversation_for_startup(
        &self,
        path: &Path,
        provider: &str,
        model: &str,
    ) -> Result<i64, StartupDbError> {
        self.conn
            .busy_timeout(STARTUP_BUSY_TIMEOUT)
            .map_err(|error| {
                StartupDbError::sqlite(
                    StartupDbOperation::CreateConversation,
                    path,
                    Duration::ZERO,
                    error,
                )
            })?;
        let result = retry_startup_sqlite(StartupDbOperation::CreateConversation, path, || {
            self.create_conversation(provider, model)
        });
        let restore = self
            .conn
            .busy_timeout(NORMAL_BUSY_TIMEOUT)
            .map_err(|error| {
                StartupDbError::sqlite(
                    StartupDbOperation::CreateConversation,
                    path,
                    Duration::ZERO,
                    error,
                )
            });
        match result {
            Ok(id) => {
                restore?;
                Ok(id)
            }
            Err(error) => Err(error),
        }
    }

    /// Insert a message row plus its FTS index entry. Shared by `append_message`
    /// (top-level autocommit) and `append_turn` (batched transaction); `conn` is
    /// a bare `&Connection`, and a `&Transaction` coerces to it.
    #[allow(clippy::too_many_arguments)]
    fn insert_message_row(
        conn: &Connection,
        conversation_id: i64,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        tool_call_id: Option<&str>,
        tool_calls: Option<&str>,
        images: Option<&str>,
        is_error: bool,
        seq: i64,
        created_at: &str,
    ) -> rusqlite::Result<i64> {
        conn.execute(
            "INSERT INTO messages (conversation_id, role, content, tool_name, tool_call_id, tool_calls, images, is_error, seq, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![conversation_id, role, content, tool_name, tool_call_id, tool_calls, images, is_error, seq, created_at],
        )?;
        let msg_id = conn.last_insert_rowid();
        // Include tool-call info in the FTS index so it stays searchable.
        let searchable = if let Some(tc) = tool_calls {
            format!("{content} TOOL_CALL {tc}")
        } else {
            content.to_string()
        };
        conn.execute(
            "INSERT INTO messages_fts (rowid, content, role, conversation_id) VALUES (?1, ?2, ?3, ?4)",
            params![msg_id, searchable, role, conversation_id],
        )?;
        Ok(msg_id)
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
        is_error: bool,
        seq: i64,
    ) -> rusqlite::Result<i64> {
        // Allocate the sequence while holding the write lock. More than one
        // process can open a conversation, so a cached in-memory sequence is
        // only a hint and must not be allowed to create duplicate ordering.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let db_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )?;
        let allocated_seq = seq.max(db_seq.saturating_add(1));
        Self::insert_message_row(
            &tx,
            conversation_id,
            role,
            content,
            tool_name,
            tool_call_id,
            tool_calls,
            images,
            is_error,
            allocated_seq,
            &now_iso(),
        )?;
        tx.commit()?;
        Ok(allocated_seq)
    }

    /// Append every new message and usage record from a completed turn in a
    /// single transaction — one commit (one WAL sync) instead of one per row,
    /// and the whole turn is atomic: a mid-loop failure rolls everything back,
    /// so the DB can never hold a partial turn or desync `messages` from
    /// `messages_fts`. `seq` is advanced per written message and returned.
    pub fn append_turn(
        &self,
        conversation_id: i64,
        seq: i64,
        messages: &[ChatMessage],
        usage: &[UsageRecord],
    ) -> rusqlite::Result<i64> {
        self.append_turn_with_checkpoint(conversation_id, seq, messages, usage, None)
    }

    /// Append a turn and, when supplied, atomically persist the resulting
    /// model-facing context checkpoint at the turn's final sequence.
    pub(crate) fn append_turn_with_checkpoint(
        &self,
        conversation_id: i64,
        mut seq: i64,
        messages: &[ChatMessage],
        usage: &[UsageRecord],
        context_checkpoint: Option<&[ChatMessage]>,
    ) -> rusqlite::Result<i64> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let db_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )?;
        let checkpoint_source_is_current = seq == db_seq;
        seq = seq.max(db_seq);
        let now = now_iso();
        for msg in messages {
            let (role, tool_name, tool_call_id, tool_calls_json, images_json) = match msg.role {
                ChatRole::Assistant => (
                    "assistant",
                    None,
                    None,
                    if msg.tool_calls.is_empty() {
                        None
                    } else {
                        serde_json::to_string(&msg.tool_calls).ok()
                    },
                    None,
                ),
                // Default `tool_name`/`tool_call_id` to "tool"/"" when absent
                // to preserve the pre-rewrite row shape (consumers that read
                // these back expect non-null values).
                ChatRole::Tool => (
                    "tool",
                    Some(msg.name.as_deref().unwrap_or("tool")),
                    Some(msg.tool_call_id.as_deref().unwrap_or("")),
                    None,
                    None,
                ),
                ChatRole::User => (
                    "user",
                    None,
                    None,
                    None,
                    (!msg.images.is_empty())
                        .then(|| serde_json::to_string(&msg.images).ok())
                        .flatten(),
                ),
                ChatRole::System => ("system", None, None, None, None),
            };
            // Only tool results carry an error flag; other roles persist `false`.
            let is_error = msg.role == ChatRole::Tool && msg.is_error;
            seq += 1;
            Self::insert_message_row(
                &tx,
                conversation_id,
                role,
                &msg.content,
                tool_name,
                tool_call_id,
                tool_calls_json.as_deref(),
                images_json.as_deref(),
                is_error,
                seq,
                &now,
            )?;
        }
        for u in usage {
            tx.execute(
                "INSERT INTO usage_events (conversation_id, provider, model, prompt_tokens, completion_tokens, cached_tokens, cost, is_estimated, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    conversation_id,
                    &u.provider,
                    &u.model,
                    u.prompt_tokens as i64,
                    u.completion_tokens as i64,
                    u.cached_tokens.unwrap_or(0) as i64,
                    u.cost.unwrap_or(0.0),
                    u.is_estimated as i64,
                    now
                ],
            )?;
        }
        // A checkpoint produced from stale state must not cover messages
        // written by another actor. In that case retain the full transcript
        // fallback; a later compaction can establish a fresh checkpoint.
        if checkpoint_source_is_current && let Some(checkpoint) = context_checkpoint {
            let messages_json = serde_json::to_string(checkpoint)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
            tx.execute(
                "INSERT INTO conversation_context_checkpoints
                 (conversation_id, through_seq, messages_json, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![conversation_id, seq, messages_json, now],
            )?;
        }
        tx.commit()?;
        Ok(seq)
    }

    /// Persist an explicit `conversation.replace` performed while idle. The
    /// sequence comparison prevents a stale actor from hiding concurrent rows.
    pub(crate) fn save_context_checkpoint(
        &self,
        conversation_id: i64,
        through_seq: i64,
        messages: &[ChatMessage],
    ) -> rusqlite::Result<bool> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let db_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )?;
        if db_seq != through_seq {
            return Ok(false);
        }
        let messages_json = serde_json::to_string(messages)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
        tx.execute(
            "INSERT INTO conversation_context_checkpoints
             (conversation_id, through_seq, messages_json, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![conversation_id, through_seq, messages_json, now_iso()],
        )?;
        tx.commit()?;
        Ok(true)
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

    /// End a conversation, removing it instead when it never recorded any
    /// messages, usage, or context checkpoints.
    pub fn end_conversation(&self, conversation_id: i64) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let removed = tx.execute(
            "DELETE FROM conversations
             WHERE id = ?1
               AND NOT EXISTS (SELECT 1 FROM messages WHERE conversation_id = ?1)
               AND NOT EXISTS (SELECT 1 FROM usage_events WHERE conversation_id = ?1)
               AND NOT EXISTS (
                   SELECT 1 FROM conversation_context_checkpoints WHERE conversation_id = ?1
               )",
            params![conversation_id],
        )?;
        if removed == 0 {
            tx.execute(
                "UPDATE conversations SET ended_at = ?1 WHERE id = ?2",
                params![now_iso(), conversation_id],
            )?;
        }
        tx.commit()
    }

    /// Remove fully empty conversations left by older versions after they ended.
    fn prune_ended_empty_conversations(&self) -> rusqlite::Result<usize> {
        self.conn.execute(
            "DELETE FROM conversations
             WHERE ended_at IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM messages WHERE conversation_id = conversations.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM usage_events WHERE conversation_id = conversations.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM conversation_context_checkpoints
                   WHERE conversation_id = conversations.id
               )",
            [],
        )
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

    /// Aggregate token usage recorded for a single conversation. Used to
    /// restore the running token totals when a conversation is reloaded, so
    /// the meter reflects that chat's history instead of resetting to zero.
    pub fn conversation_usage(&self, conversation_id: i64) -> rusqlite::Result<UsageSummary> {
        let sql = format!("SELECT {SUM_COLS} FROM usage_events WHERE conversation_id = ?1");
        self.conn
            .query_row(&sql, params![conversation_id], Self::read_summary_row)
    }

    /// Highest `seq` stored for a conversation, or 0 if it has no messages.
    /// Used to continue seq numbering when resuming a conversation.
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    pub fn max_message_seq(&self, conversation_id: i64) -> rusqlite::Result<i64> {
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
        let all_time = self.usage_all_months()?;
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

    fn usage_all_months(&self) -> rusqlite::Result<Vec<UsageBucket>> {
        self.query_buckets(
            &format!(
                "WITH RECURSIVE bounds(first_month, current_month) AS (
                SELECT COALESCE(strftime('%Y-%m', MIN(created_at), 'localtime'),
                                strftime('%Y-%m', 'now', 'localtime')),
                       strftime('%Y-%m', 'now', 'localtime')
                FROM usage_events
             ), series(month) AS (
                SELECT first_month FROM bounds
                UNION ALL
                SELECT strftime('%Y-%m', date(month || '-01', '+1 month'))
                FROM series, bounds WHERE month < current_month
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
            [],
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
            "SELECT datetime(MIN(created_at), 'localtime'), datetime(MAX(created_at), 'localtime')
             FROM usage_events
             WHERE date(created_at, 'localtime') BETWEEN ?1 AND ?2",
            params![start, end],
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
        self.query_messages(conversation_id, Some(limit))
    }

    /// Load a complete durable transcript. Runtime replay must not silently
    /// truncate long conversations at the public history-query limit.
    pub(crate) fn load_messages(
        &self,
        conversation_id: i64,
    ) -> rusqlite::Result<Vec<StoredMessage>> {
        self.query_messages(conversation_id, None)
    }

    /// Load the durable model-facing context. The immutable message log remains
    /// authoritative for history/search; a checkpoint replaces only the prefix
    /// it summarizes, and later durable messages are replayed verbatim.
    pub(crate) fn load_effective_transcript(
        &self,
        conversation_id: i64,
    ) -> rusqlite::Result<Vec<ChatMessage>> {
        let max_seq = self.max_message_seq(conversation_id)?;
        let mut stmt = self.conn.prepare(
            "SELECT through_seq, messages_json
             FROM conversation_context_checkpoints
             WHERE conversation_id = ?1 AND through_seq <= ?2
             ORDER BY id DESC",
        )?;
        let checkpoints = stmt.query_map(params![conversation_id, max_seq], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        // Skip a malformed newest revision and recover from the latest older
        // valid one. Checkpoints are a derived projection, never the only copy.
        for checkpoint in checkpoints {
            let (through_seq, json) = checkpoint?;
            let Ok(mut transcript) = serde_json::from_str::<Vec<ChatMessage>>(&json) else {
                continue;
            };
            let mut tail = self.query_messages_after(conversation_id, through_seq)?;
            transcript.extend(tail.drain(..).map(stored_to_chat_message));
            return Ok(transcript);
        }

        self.load_messages(conversation_id)
            .map(|rows| rows.into_iter().map(stored_to_chat_message).collect())
    }

    /// Map a rusqlite row (seq, role, content, tool_name, tool_call_id,
    /// tool_calls, images, is_error) into a [`StoredMessage`].
    fn stored_message_from_row(row: &rusqlite::Row) -> rusqlite::Result<StoredMessage> {
        Ok(StoredMessage {
            seq: row.get(0)?,
            role: row.get(1)?,
            content: row.get(2)?,
            tool_name: row.get(3)?,
            tool_call_id: row.get(4)?,
            tool_calls: row.get(5)?,
            images: row.get(6)?,
            is_error: row.get::<_, i64>(7)? != 0,
        })
    }

    fn query_messages_after(
        &self,
        conversation_id: i64,
        after_seq: i64,
    ) -> rusqlite::Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, role, content, tool_name, tool_call_id, tool_calls, images, is_error
             FROM messages WHERE conversation_id = ?1 AND seq > ?2
             ORDER BY seq ASC, id ASC",
        )?;
        let rows = stmt.query_map(
            params![conversation_id, after_seq],
            Self::stored_message_from_row,
        )?;
        rows.collect()
    }

    fn query_messages(
        &self,
        conversation_id: i64,
        limit: Option<usize>,
    ) -> rusqlite::Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, role, content, tool_name, tool_call_id, tool_calls, images, is_error \
             FROM messages WHERE conversation_id = ?1 \
             ORDER BY seq ASC, id ASC LIMIT ?2",
        )?;
        let sql_limit = limit.map(|value| value as i64).unwrap_or(-1);
        let rows = stmt.query_map(
            params![conversation_id, sql_limit],
            Self::stored_message_from_row,
        )?;
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
