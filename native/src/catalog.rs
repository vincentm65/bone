//! Shared helpers for the daemon extension catalog (Phase 6).
//!
//! The daemon answers `HostRequest::Catalog` with a `CatalogSnapshot` and
//! `HostRequest::CatalogApply` with a `CatalogApplyResult`, both stored by the
//! caller. The Plugins surface (`plugins.rs`) renders that snapshot; this module
//! keeps the filter/search, outcome-message, banner, and palette helpers it
//! shares with the rest of the desktop UI.

use bone_protocol::{CatalogApplyResult, CatalogItem, CatalogItemOutcome};
use eframe::egui;

use crate::theme;

/// Human label for one applied item, mirroring the TUI's `catalog_action_message`.
pub fn action_message(name: &str, outcome: &CatalogItemOutcome) -> String {
    match outcome {
        CatalogItemOutcome::Installed => format!("Catalog item installed: {name}"),
        CatalogItemOutcome::Removed => format!("Catalog item removed: {name}"),
        CatalogItemOutcome::Failed { message } => {
            format!("Catalog action failed for {name}: {message}")
        }
        CatalogItemOutcome::Enabled => format!("Catalog item enabled: {name}"),
        CatalogItemOutcome::Disabled => format!("Catalog item disabled: {name}"),
        CatalogItemOutcome::Unchanged => format!("Catalog item unchanged: {name}"),
    }
}

/// One-line summary of an apply result, including all outcomes for a batch.
pub fn applied_summary(result: &CatalogApplyResult) -> String {
    if result.results.len() > 1 {
        return result_banner(result);
    }
    match result.results.first() {
        Some(item) => action_message(&item.name, &item.outcome),
        None if result.changed => "Catalog updated.".to_string(),
        None => "Catalog: no changes.".to_string(),
    }
}

/// One-line banner for the result phase, counting installed/removed/failed.
pub(crate) fn result_banner(result: &CatalogApplyResult) -> String {
    let (mut installed, mut removed, mut failed) = (0, 0, 0);
    let (mut enabled, mut disabled) = (0, 0);
    for item in &result.results {
        match &item.outcome {
            CatalogItemOutcome::Installed => installed += 1,
            CatalogItemOutcome::Removed => removed += 1,
            CatalogItemOutcome::Failed { .. } => failed += 1,
            CatalogItemOutcome::Enabled => enabled += 1,
            CatalogItemOutcome::Disabled => disabled += 1,
            CatalogItemOutcome::Unchanged => {}
        }
    }
    let mut parts = Vec::new();
    if installed > 0 {
        parts.push(format!("installed {installed}"));
    }
    if removed > 0 {
        parts.push(format!("removed {removed}"));
    }
    if enabled > 0 {
        parts.push(format!("enabled {enabled}"));
    }
    if disabled > 0 {
        parts.push(format!("disabled {disabled}"));
    }
    let mut text = if parts.is_empty() {
        "No changes applied.".to_string()
    } else {
        format!("✓ {}", parts.join(", "))
    };
    if failed > 0 {
        if parts.is_empty() {
            text = format!("✗ {failed} failed");
        } else {
            text.push_str(&format!(" — {failed} failed"));
        }
    }
    text
}

pub(crate) fn color(value: &Option<String>, fallback: egui::Color32) -> egui::Color32 {
    value
        .as_deref()
        .and_then(theme::parse_color)
        .unwrap_or(fallback)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Filter {
    #[default]
    Browse,
    Installed,
    Updates,
}

pub(crate) fn matches_item(item: &CatalogItem, filter: Filter, query: &str) -> bool {
    let visible = match filter {
        Filter::Browse => true,
        Filter::Installed => item.installed,
        Filter::Updates => item.update_available,
    };
    visible
        && query.split_whitespace().all(|word| {
            item.name.to_lowercase().contains(word)
                || item.description.to_lowercase().contains(word)
                || item.kind.to_lowercase().contains(word)
                || item
                    .author
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(word)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_protocol::{CatalogItemResult, CatalogSnapshot};

    #[test]
    fn matches_item_filters_by_tab_and_query() {
        let memory = item("memory", true, true);
        let browser = item("browser", false, false);
        assert!(matches_item(&memory, Filter::Updates, "memory tool"));
        assert!(!matches_item(&browser, Filter::Installed, ""));
        assert!(!matches_item(&memory, Filter::Browse, "missing"));
        assert!(matches_item(&browser, Filter::Browse, ""));
    }

    fn item(name: &str, installed: bool, update_available: bool) -> CatalogItem {
        CatalogItem {
            name: name.into(),
            kind: "tool".into(),
            description: format!("{name} description"),
            installed,
            update_available,
            ..CatalogItem::default()
        }
    }

    #[test]
    fn action_message_covers_all_outcomes() {
        assert_eq!(
            action_message("x", &CatalogItemOutcome::Installed),
            "Catalog item installed: x"
        );
        assert_eq!(
            action_message("x", &CatalogItemOutcome::Removed),
            "Catalog item removed: x"
        );
        assert_eq!(
            action_message("x", &CatalogItemOutcome::Unchanged),
            "Catalog item unchanged: x"
        );
        assert_eq!(
            action_message(
                "x",
                &CatalogItemOutcome::Failed {
                    message: "boom".into()
                }
            ),
            "Catalog action failed for x: boom"
        );
    }

    #[test]
    fn applied_summary_uses_first_result_then_changed_flag() {
        let snapshot = CatalogSnapshot {
            revision: "r2".into(),
            items: Vec::new(),
        };
        let with_result = CatalogApplyResult {
            snapshot: snapshot.clone(),
            results: vec![CatalogItemResult {
                name: "x".into(),
                outcome: CatalogItemOutcome::Installed,
            }],
            changed: true,
            extensions_reloaded: true,
        };
        assert_eq!(applied_summary(&with_result), "Catalog item installed: x");

        let no_results = CatalogApplyResult {
            snapshot: snapshot.clone(),
            results: Vec::new(),
            changed: false,
            extensions_reloaded: false,
        };
        assert_eq!(applied_summary(&no_results), "Catalog: no changes.");
    }
}
