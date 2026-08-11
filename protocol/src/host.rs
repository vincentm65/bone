//! Daemon-host services shared by local and remote frontends.
//!
//! These types describe data and mutations whose authority lives beside the
//! daemon: its usage database, extension catalog, and setup/config files. UI
//! navigation and rendering intentionally remain frontend concerns.

use serde::{Deserialize, Serialize};

/// Current daemon-host request/response contract advertised in `FrontendState`.
pub const HOST_API_VERSION: u16 = 1;

/// A custom `[start, end]` date range (inclusive, `YYYY-MM-DD`, daemon-local
/// time).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateRange {
    pub start: String,
    pub end: String,
}

/// Aggregated token usage.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
    pub request_count: i64,
}

/// Usage broken down by provider/model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageBucket {
    pub label: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cost: f64,
    pub request_count: i64,
}

/// One hour-of-day aggregate row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HourUsage {
    pub hour: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub request_count: i64,
}

/// Full historical usage snapshot for a stats dashboard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

impl UsageStatsSnapshot {
    /// Select usage buckets by a frontend-local view index: today, seven days,
    /// four weeks, yearly, then all time.
    pub fn buckets(&self, mode: impl Into<usize>) -> &[UsageBucket] {
        match mode.into() {
            0 => &self.daily,
            1 => &self.weekly,
            2 => &self.monthly,
            3 => &self.yearly,
            _ => &self.all_time,
        }
    }

    /// Select hourly data by the same frontend-local view index.
    pub fn hourly(&self, mode: impl Into<usize>) -> &[HourUsage] {
        match mode.into() {
            0 => &self.hourly_today,
            1 => &self.hourly_7d,
            2 => &self.hourly_4w,
            _ => &self.hourly_all,
        }
    }

    /// Compute a summary for a frontend-local view by aggregating its buckets.
    pub fn range_summary(&self, mode: impl Into<usize>) -> UsageSummary {
        self.buckets(mode)
            .iter()
            .fold(UsageSummary::default(), |mut summary, bucket| {
                summary.prompt_tokens += bucket.prompt_tokens;
                summary.completion_tokens += bucket.completion_tokens;
                summary.cached_tokens += bucket.cached_tokens;
                summary.cost += bucket.cost;
                summary.request_count += bucket.request_count;
                summary
            })
    }

    /// Select provider/model rows by the same frontend-local view index.
    pub fn range_models(&self, mode: impl Into<usize>) -> &[ProviderUsage] {
        match mode.into() {
            0 => &self.by_model_today,
            1 => &self.by_model_7d,
            2 => &self.by_model_4w,
            _ => &self.by_model_all,
        }
    }
}

/// Display-safe catalog entry. Integrity hashes, bundled paths, and other
/// daemon implementation details deliberately stay off the wire.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogItem {
    pub name: String,
    pub kind: String,
    pub description: String,
    pub version: Option<String>,
    pub updated_at: Option<String>,
    pub author: Option<String>,
    pub repository: Option<String>,
    pub documentation: Option<String>,
    pub min_bone_version: Option<String>,
    pub dependencies: Vec<String>,
    pub permissions: Vec<String>,
    pub long_description: Option<String>,
    pub installed: bool,
    pub update_available: bool,
}

/// One revisioned catalog projection from the daemon host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    pub revision: String,
    pub items: Vec<CatalogItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogActionKind {
    Install,
    Remove,
}

/// One user-requested catalog mutation, addressed by stable item name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogAction {
    pub name: String,
    pub action: CatalogActionKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogItemOutcome {
    Unchanged,
    Installed,
    Removed,
    Failed { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogItemResult {
    pub name: String,
    pub outcome: CatalogItemOutcome,
}

/// Authoritative post-apply catalog state and per-item outcomes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogApplyResult {
    pub snapshot: CatalogSnapshot,
    pub results: Vec<CatalogItemResult>,
    pub changed: bool,
    pub extensions_reloaded: bool,
}

/// Minimal provider data needed by onboarding; credentials remain redacted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderChoice {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub api_key_configured: bool,
}

/// Initial daemon-owned data for the setup wizard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSnapshot {
    pub config_revision: u64,
    pub providers: Vec<ProviderChoice>,
    pub active_provider: String,
    pub init_exists: bool,
    pub needs_onboarding: bool,
    pub catalog: CatalogSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InitChoice {
    Populated,
    Blank,
    Keep,
}

/// Result of applying the setup plan on the daemon host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupApplyResult {
    pub config_revision: u64,
    pub catalog: CatalogApplyResult,
    pub restart_required: bool,
    pub message: String,
}

/// Typed daemon-host operations. Each request is wrapped in a correlated
/// [`crate::RuntimeCommand::HostRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostRequest {
    Stats {
        #[serde(default)]
        range: Option<DateRange>,
    },
    Catalog {
        #[serde(default)]
        refresh: bool,
    },
    CatalogApply {
        expected_revision: String,
        actions: Vec<CatalogAction>,
    },
    Setup,
    SetupApply {
        expected_config_revision: u64,
        expected_catalog_revision: String,
        provider_id: Option<String>,
        api_key: Option<String>,
        catalog: Vec<CatalogAction>,
        init: InitChoice,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostErrorCode {
    Unsupported,
    Busy,
    Stale,
    Invalid,
    Unavailable,
    Internal,
}

/// Correlated result of a [`HostRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostResponse {
    Stats(Box<UsageStatsSnapshot>),
    Catalog(CatalogSnapshot),
    CatalogApplied(CatalogApplyResult),
    Setup(SetupSnapshot),
    SetupApplied(SetupApplyResult),
    Error {
        code: HostErrorCode,
        message: String,
    },
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
