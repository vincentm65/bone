//! Synchronous daemon-host services for stats, catalog, and setup requests.
//!
//! RPC owns scheduling and correlation; this module owns the blocking database,
//! network, and filesystem work so callers can move one cloneable service call
//! wholesale into `spawn_blocking`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bone_protocol::{
    CatalogAction, CatalogActionKind, CatalogApplyResult, CatalogItem, CatalogItemOutcome,
    CatalogItemResult, CatalogSnapshot, HostErrorCode, HostRequest, HostResponse, InitChoice,
    ProviderChoice, ProviderUpdate, SetupApplyResult, SetupSnapshot,
};
use sha2::{Digest, Sha256};

use crate::config::{self, SetupSelection, store::ConfigStore};
use crate::ext::catalog::{self, CatalogEntry};
use crate::session_db::SessionDb;

#[derive(Default)]
struct HostState {
    catalog: Option<Vec<CatalogEntry>>,
}

/// Cloneable authority for daemon-global storage operations.
#[derive(Clone)]
pub struct HostService {
    config: ConfigStore,
    db_path: PathBuf,
    state: Arc<Mutex<HostState>>,
}

impl HostService {
    pub fn new(config: ConfigStore) -> Self {
        Self::with_db_path(config, crate::session_db::db_path())
    }

    /// Construct against an explicit database, primarily for embedding/tests.
    pub fn with_db_path(config: ConfigStore, db_path: PathBuf) -> Self {
        Self {
            config,
            db_path,
            state: Arc::default(),
        }
    }

    /// Execute one typed request synchronously on the daemon host.
    pub fn execute(&self, request: HostRequest) -> HostResponse {
        match request {
            HostRequest::Stats { range } => self.stats(range),
            HostRequest::Catalog { refresh } => self.catalog(refresh),
            HostRequest::CatalogApply {
                expected_revision,
                actions,
            } => self.catalog_apply(expected_revision, actions),
            HostRequest::Setup => self.setup(),
            HostRequest::SetupApply {
                expected_config_revision,
                expected_catalog_revision,
                provider_id,
                api_key,
                catalog,
                init,
            } => self.setup_apply(
                expected_config_revision,
                expected_catalog_revision,
                provider_id,
                api_key,
                catalog,
                init,
            ),
        }
    }

    fn stats(&self, range: Option<bone_protocol::DateRange>) -> HostResponse {
        let result = SessionDb::open(&self.db_path).and_then(|db| match range {
            Some(range) => db.usage_stats_range(&range.start, &range.end),
            None => db.usage_stats_snapshot(),
        });
        match result {
            Ok(snapshot) => HostResponse::Stats(Box::new(snapshot)),
            Err(error) => host_error(HostErrorCode::Unavailable, error),
        }
    }

    fn catalog(&self, refresh: bool) -> HostResponse {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        HostResponse::Catalog(catalog_snapshot(load_catalog(&mut state, refresh)))
    }

    fn catalog_apply(
        &self,
        expected_revision: String,
        actions: Vec<CatalogAction>,
    ) -> HostResponse {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let entries = load_catalog(&mut state, false);
        let current = catalog_revision(entries);
        if current != expected_revision {
            return host_error(
                HostErrorCode::Stale,
                format!("catalog changed; expected {expected_revision}, current {current}"),
            );
        }
        HostResponse::CatalogApplied(apply_catalog(entries, &actions))
    }

    fn setup(&self) -> HostResponse {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let config = self.config.snapshot();
        let providers = config
            .providers
            .into_iter()
            .map(|provider| ProviderChoice {
                id: provider.id,
                label: provider.label,
                api_key_configured: provider.api_key_configured,
            })
            .collect();
        HostResponse::Setup(SetupSnapshot {
            config_revision: config.revision,
            providers,
            active_provider: config.active_provider,
            init_exists: config::bone_dir().join("init.lua").exists(),
            needs_onboarding: config::needs_onboarding(),
            catalog: catalog_snapshot(load_catalog(&mut state, false)),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn setup_apply(
        &self,
        expected_config_revision: u64,
        expected_catalog_revision: String,
        provider_id: Option<String>,
        api_key: Option<String>,
        actions: Vec<CatalogAction>,
        init: InitChoice,
    ) -> HostResponse {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let entries = load_catalog(&mut state, false);
        let current_catalog = catalog_revision(entries);
        if current_catalog != expected_catalog_revision {
            return host_error(
                HostErrorCode::Stale,
                format!(
                    "catalog changed; expected {expected_catalog_revision}, current {current_catalog}"
                ),
            );
        }
        if let Err(error) = self.config.check_revision(expected_config_revision) {
            return config_error(expected_config_revision, error);
        }
        if provider_id.is_none() && api_key.is_some() {
            return host_error(
                HostErrorCode::Invalid,
                "an API key requires a selected provider",
            );
        }
        if let Some(id) = provider_id.as_deref()
            && !self.config.providers_config().providers.contains_key(id)
        {
            return host_error(HostErrorCode::Invalid, format!("unknown provider: {id}"));
        }
        if let Some(action) = actions
            .iter()
            .find(|action| find_catalog_entry(entries, &action.name).is_none())
        {
            return host_error(
                HostErrorCode::Invalid,
                format!("catalog item not found: {}", action.name),
            );
        }

        let mut revision = expected_config_revision;
        if let Some(id) = provider_id {
            if let Some(api_key) = api_key.filter(|key| !key.trim().is_empty()) {
                let providers = self.config.providers_config();
                let entry = &providers.providers[&id];
                let update = ProviderUpdate {
                    id: id.clone(),
                    label: entry.label.clone(),
                    base_url: entry.base_url.clone(),
                    model: entry.model.clone(),
                    endpoint: entry.endpoint.clone(),
                    handler: entry.handler.clone(),
                    context_window_tokens: entry.context_window_tokens,
                    max_concurrency: entry.max_concurrency,
                    reasoning_effort: entry.reasoning_effort.clone(),
                    fast_mode: Some(entry.fast_mode),
                    supports_prompt_cache_key: Some(entry.supports_prompt_cache_key),
                    api_key: Some(api_key),
                };
                if let Err(error) = self.config.upsert_provider(update, revision) {
                    return config_error(revision, error);
                }
                revision = revision.saturating_add(1);
            }
            if let Err(error) = self.config.set_active_provider(&id, revision) {
                return config_error(revision, error);
            }
            revision = revision.saturating_add(1);
        }

        let selection = SetupSelection {
            tools: Vec::new(),
            commands: crate::ext::default_command_catalog()
                .into_iter()
                .map(|(name, _)| name.to_string())
                .collect(),
        };
        let init = match init {
            InitChoice::Populated => config::InitChoice::Populated,
            InitChoice::Blank => config::InitChoice::Blank,
            InitChoice::Keep => config::InitChoice::Keep,
        };
        if init == config::InitChoice::Populated {
            if let Err(error) = self.config.apply_populated_onboarding(&selection, revision) {
                return config_error(revision, error);
            }
        } else if let Err(error) = config::apply_onboarding(&selection, init) {
            return host_error(HostErrorCode::Internal, error);
        }

        let catalog = apply_catalog(entries, &actions);
        let failures = catalog
            .results
            .iter()
            .filter(|result| matches!(result.outcome, CatalogItemOutcome::Failed { .. }))
            .count();
        HostResponse::SetupApplied(SetupApplyResult {
            config_revision: self.config.snapshot().revision,
            catalog,
            restart_required: true,
            message: if failures == 0 {
                "Setup saved.".into()
            } else {
                format!("Setup saved. {failures} catalog item(s) failed.")
            },
        })
    }
}

fn load_catalog(state: &mut HostState, refresh: bool) -> &[CatalogEntry] {
    if refresh || state.catalog.is_none() {
        state.catalog = Some(catalog::sync_quiet());
    }
    state.catalog.as_deref().unwrap_or_default()
}

fn catalog_revision(entries: &[CatalogEntry]) -> String {
    let mut digest = Sha256::new();
    digest.update(serde_json::to_vec(entries).unwrap_or_default());
    for entry in entries {
        digest.update([
            u8::from(catalog::is_installed(entry)),
            u8::from(catalog::needs_update(entry)),
        ]);
    }
    format!("{:x}", digest.finalize())
}

fn catalog_snapshot(entries: &[CatalogEntry]) -> CatalogSnapshot {
    CatalogSnapshot {
        revision: catalog_revision(entries),
        items: entries
            .iter()
            .map(|entry| CatalogItem {
                name: entry.name.clone(),
                kind: entry.kind.clone(),
                description: entry.description.clone(),
                version: entry.version.clone(),
                updated_at: entry.updated_at.clone(),
                author: entry.author.clone(),
                repository: entry.repository.clone(),
                documentation: entry.documentation.clone(),
                min_bone_version: entry.min_bone_version.clone(),
                dependencies: entry.dependencies.clone(),
                permissions: entry.permissions.clone(),
                long_description: entry.long_description.clone(),
                installed: catalog::is_installed(entry),
                update_available: catalog::is_installed(entry) && catalog::needs_update(entry),
            })
            .collect(),
    }
}

fn find_catalog_entry<'a>(entries: &'a [CatalogEntry], name: &str) -> Option<&'a CatalogEntry> {
    let requested = name.strip_suffix(".lua").unwrap_or(name);
    entries
        .iter()
        .find(|entry| entry.name.strip_suffix(".lua") == Some(requested))
}

fn apply_catalog(entries: &[CatalogEntry], actions: &[CatalogAction]) -> CatalogApplyResult {
    let mut changed = false;
    let results = actions
        .iter()
        .map(|action| {
            let outcome = match find_catalog_entry(entries, &action.name) {
                None => CatalogItemOutcome::Failed {
                    message: format!("catalog item not found: {}", action.name),
                },
                Some(entry) => apply_catalog_entry(entry, action.action),
            };
            changed |= matches!(
                outcome,
                CatalogItemOutcome::Installed | CatalogItemOutcome::Removed
            );
            CatalogItemResult {
                name: action.name.clone(),
                outcome,
            }
        })
        .collect::<Vec<_>>();
    CatalogApplyResult {
        snapshot: catalog_snapshot(entries),
        results,
        changed,
        extensions_reloaded: false,
    }
}

fn apply_catalog_entry(entry: &CatalogEntry, action: CatalogActionKind) -> CatalogItemOutcome {
    let result = match action {
        CatalogActionKind::Install
            if catalog::is_installed(entry) && !catalog::needs_update(entry) =>
        {
            return CatalogItemOutcome::Unchanged;
        }
        CatalogActionKind::Install => {
            catalog::install(entry).map(|()| CatalogItemOutcome::Installed)
        }
        CatalogActionKind::Remove if !catalog::has_installed_files(entry) => {
            return CatalogItemOutcome::Unchanged;
        }
        CatalogActionKind::Remove => catalog::remove(entry).map(|()| CatalogItemOutcome::Removed),
    };
    result.unwrap_or_else(|message| CatalogItemOutcome::Failed { message })
}

fn config_error(expected: u64, (current, message): (u64, String)) -> HostResponse {
    host_error(
        if current != expected {
            HostErrorCode::Stale
        } else {
            HostErrorCode::Internal
        },
        format!("{message} (current revision {current})"),
    )
}

fn host_error(code: HostErrorCode, error: impl std::fmt::Display) -> HostResponse {
    HostResponse::Error {
        code,
        message: error.to_string(),
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
