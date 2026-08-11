//! Transport-neutral adapters for daemon-host UI operations.

use bone_protocol::{
    CatalogAction, CatalogActionKind, CatalogApplyResult, CatalogItemOutcome, CatalogSnapshot,
    HostRequest, HostResponse, SetupApplyResult, SetupSnapshot, UsageStatsSnapshot,
};

macro_rules! response {
    ($name:ident, $variant:ident, $output:ty, $label:literal) => {
        pub fn $name(response: Result<HostResponse, String>) -> Result<$output, String> {
            match response? {
                HostResponse::$variant(value) => Ok(value),
                HostResponse::Error { message, .. } => Err(message),
                _ => Err(concat!("daemon returned an unexpected ", $label, " response").into()),
            }
        }
    };
}

response!(catalog_snapshot, Catalog, CatalogSnapshot, "catalog");
response!(
    catalog_applied,
    CatalogApplied,
    CatalogApplyResult,
    "catalog"
);
response!(setup_snapshot, Setup, SetupSnapshot, "setup");
response!(setup_applied, SetupApplied, SetupApplyResult, "setup");

pub fn stats(response: Result<HostResponse, String>) -> Result<UsageStatsSnapshot, String> {
    match response? {
        HostResponse::Stats(snapshot) => Ok(*snapshot),
        HostResponse::Error { message, .. } => Err(message),
        _ => Err("daemon returned an unexpected stats response".into()),
    }
}

pub fn catalog_apply_request(
    expected_revision: String,
    actions: Vec<CatalogAction>,
) -> HostRequest {
    HostRequest::CatalogApply {
        expected_revision,
        actions,
    }
}

pub fn setup_apply_request(plan: super::setup::Plan) -> HostRequest {
    HostRequest::SetupApply {
        expected_config_revision: plan.expected_config_revision,
        expected_catalog_revision: plan.expected_catalog_revision,
        provider_id: plan.provider_id,
        api_key: plan.api_key,
        catalog: plan.catalog,
        init: plan.init,
    }
}

pub fn catalog_action_message(
    action: CatalogActionKind,
    name: &str,
    result: &CatalogApplyResult,
) -> String {
    match result.results.first().map(|item| &item.outcome) {
        Some(CatalogItemOutcome::Installed) => format!("Catalog item installed: {name}"),
        Some(CatalogItemOutcome::Removed) => format!("Catalog item removed: {name}"),
        Some(CatalogItemOutcome::Failed { message }) => {
            format!("Catalog action failed for {name}: {message}")
        }
        Some(CatalogItemOutcome::Unchanged) | None => match action {
            CatalogActionKind::Install => format!("Catalog item already installed: {name}"),
            CatalogActionKind::Remove => format!("Catalog item is not installed: {name}"),
        },
    }
}
