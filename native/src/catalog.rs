//! Native renderer for the daemon extension catalog (Phase 6).
//!
//! The daemon answers `HostRequest::Catalog` with a `CatalogSnapshot` and
//! `HostRequest::CatalogApply` with a `CatalogApplyResult`, both stored by the
//! caller. The Catalog screen offers search, filters, item details, explicit
//! per-item actions, and optional batch changes. Mutations still use the
//! daemon's revisioned CatalogApply request.

use std::collections::{HashMap, HashSet};

use bone_protocol::{CatalogApplyResult, CatalogItem, CatalogItemOutcome, CatalogSnapshot};
use eframe::egui;

use crate::theme::{self, Palette};

/// A user action emitted by the Catalog dialog, applied by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogUiAction {
    /// Apply all touched toggles at once, as `(name, install?)` pairs.
    Apply(Vec<(String, bool)>),
}

/// Human label for one applied item, mirroring the TUI's `catalog_action_message`.
pub fn action_message(name: &str, outcome: &CatalogItemOutcome) -> String {
    match outcome {
        CatalogItemOutcome::Installed => format!("Catalog item installed: {name}"),
        CatalogItemOutcome::Removed => format!("Catalog item removed: {name}"),
        CatalogItemOutcome::Failed { message } => {
            format!("Catalog action failed for {name}: {message}")
        }
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

/// Persistent multi-select state for the Catalog dialog. Installed and
/// update-available items start checked; only rows the user toggled are sent on
/// apply, so untouched items are preserved.
#[derive(Debug, Default)]
pub struct CatalogView {
    /// Snapshot revision the current selection was seeded from.
    revision: String,
    query: String,
    filter: Filter,
    selected: Option<String>,
    bulk: bool,
    /// Checked state per item name.
    checked: HashMap<String, bool>,
    /// Item names the user explicitly toggled.
    touched: HashSet<String>,
    /// Per-item outcome from the last apply, keyed by name.
    results: HashMap<String, CatalogItemOutcome>,
    /// One-line banner shown in the read-only result phase.
    banner: Option<String>,
}

impl CatalogView {
    /// Adopt a fresh snapshot: reseed the selection (installed/update items
    /// checked) whenever the revision changes.
    fn sync(&mut self, snapshot: &CatalogSnapshot) {
        if self.revision == snapshot.revision {
            return;
        }
        self.revision = snapshot.revision.clone();
        self.reseed(snapshot);
    }

    fn reseed(&mut self, snapshot: &CatalogSnapshot) {
        self.checked.clear();
        self.touched.clear();
        self.results.clear();
        self.banner = None;
        for item in &snapshot.items {
            self.checked
                .insert(item.name.clone(), item.installed || item.update_available);
        }
    }

    /// Record an apply result: adopt its snapshot, reseed the selection, overlay
    /// per-item outcome tags, and enter the read-only result phase.
    pub fn apply_result(&mut self, result: &CatalogApplyResult) {
        self.revision = result.snapshot.revision.clone();
        self.reseed(&result.snapshot);
        for item in &result.results {
            self.results.insert(item.name.clone(), item.outcome.clone());
        }
        self.banner = Some(result_banner(result));
    }

    /// Leave the read-only result phase (clears the banner and outcome tags).
    pub fn clear_result(&mut self) {
        self.banner = None;
        self.results.clear();
    }

    fn checked(&self, name: &str, fallback: bool) -> bool {
        *self.checked.get(name).unwrap_or(&fallback)
    }

    fn set_checked(&mut self, name: &str, value: bool) {
        self.checked.insert(name.to_string(), value);
        self.touched.insert(name.to_string());
    }

    /// The touched rows whose checked state differs from the snapshot, as
    /// `(name, install?)` pairs ready for `CatalogApply`.
    fn pending(&self, snapshot: &CatalogSnapshot) -> Vec<(String, bool)> {
        snapshot
            .items
            .iter()
            .filter_map(|item| {
                if !self.touched.contains(&item.name) {
                    return None;
                }
                let checked = self.checked(&item.name, item.installed);
                if checked != item.installed || (checked && item.update_available) {
                    Some((item.name.clone(), checked))
                } else {
                    None
                }
            })
            .collect()
    }
}

/// One-line banner for the result phase, counting installed/removed/failed.
fn result_banner(result: &CatalogApplyResult) -> String {
    let (mut installed, mut removed, mut failed) = (0, 0, 0);
    for item in &result.results {
        match &item.outcome {
            CatalogItemOutcome::Installed => installed += 1,
            CatalogItemOutcome::Removed => removed += 1,
            CatalogItemOutcome::Failed { .. } => failed += 1,
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

fn color(value: &Option<String>, fallback: egui::Color32) -> egui::Color32 {
    value
        .as_deref()
        .and_then(theme::parse_color)
        .unwrap_or(fallback)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Filter {
    #[default]
    Browse,
    Installed,
    Updates,
}

fn matches_item(item: &CatalogItem, filter: Filter, query: &str) -> bool {
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

/// Browsing, item details, and explicit actions share one screen. Batch mode is
/// optional, and only touched items are ever included in its apply request.
pub fn render(
    ui: &mut egui::Ui,
    snapshot: &CatalogSnapshot,
    view: &mut CatalogView,
    palette: &Palette,
) -> Vec<CatalogUiAction> {
    let mut actions = Vec::new();
    view.sync(snapshot);
    let accent = color(&palette.accent, ui.visuals().hyperlink_color);
    let good = color(&palette.good, egui::Color32::from_rgb(90, 200, 120));
    let error = ui.visuals().error_fg_color;
    ui.add(
        egui::TextEdit::singleline(&mut view.query)
            .hint_text("Search extensions…")
            .margin(egui::vec2(12.0, 9.0))
            .desired_width(f32::INFINITY),
    );
    ui.horizontal_wrapped(|ui| {
        for (filter, title, count) in [
            (Filter::Browse, "Browse", snapshot.items.len()),
            (
                Filter::Installed,
                "Installed",
                snapshot.items.iter().filter(|i| i.installed).count(),
            ),
            (
                Filter::Updates,
                "Updates",
                snapshot.items.iter().filter(|i| i.update_available).count(),
            ),
        ] {
            ui.selectable_value(&mut view.filter, filter, format!("{title}  {count}"));
        }
        if ui.selectable_label(view.bulk, "Batch changes").clicked() {
            view.bulk = !view.bulk;
        }
    });
    if let Some(banner) = view.banner.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                if view
                    .results
                    .values()
                    .any(|v| matches!(v, CatalogItemOutcome::Failed { .. }))
                {
                    error
                } else {
                    good
                },
                banner,
            );
            if ui.small_button("Dismiss").clicked() {
                view.clear_result();
            }
        });
    }
    let query = view.query.trim().to_lowercase();
    let items: Vec<_> = snapshot
        .items
        .iter()
        .filter(|i| matches_item(i, view.filter, &query))
        .collect();
    let height = (ui.available_height()
        - if ui.available_width() < 500.0 {
            116.0
        } else {
            86.0
        })
    .max(48.0);
    let wide = ui.available_width() >= 760.0;
    if wide
        && !items
            .iter()
            .any(|i| Some(&i.name) == view.selected.as_ref())
    {
        view.selected = items.first().map(|i| i.name.clone());
    }
    ui.separator();
    if items.is_empty() {
        ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
            crate::surface::empty(
                ui,
                if snapshot.items.is_empty() {
                    "No extensions available"
                } else {
                    "No matching extensions"
                },
                if view.filter == Filter::Updates && query.is_empty() {
                    "Your installed extensions are up to date."
                } else {
                    "Try a different search or browse another tab."
                },
            );
        });
    } else if !wide && view.selected.is_some() {
        egui::ScrollArea::vertical()
            .id_salt("catalog-details")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if ui.button("Back to extensions").clicked() {
                    view.selected = None;
                }
                if let Some(item) = snapshot
                    .items
                    .iter()
                    .find(|i| Some(&i.name) == view.selected.as_ref())
                {
                    details(ui, item, view, &mut actions, good, accent);
                }
            });
    } else {
        ui.horizontal_top(|ui| {
            let list_width = if wide {
                ui.available_width() - 300.0
            } else {
                ui.available_width()
            };
            ui.allocate_ui_with_layout(
                egui::vec2(list_width, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("catalog-list", view.filter as u8, &query))
                        // Reserve the scrollbar lane from the first frame. Its animated
                        // appearance otherwise changes the list/detail widths and makes the
                        // centered modal visibly shiver on fractional display scales.
                        .scroll_bar_visibility(
                            egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible,
                        )
                        .max_height(height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for item in &items {
                                ui.push_id(&item.name, |ui| {
                                    let row = ui.scope_builder(
                                        egui::UiBuilder::new().sense(egui::Sense::click()),
                                        |ui| {
                                            egui::Frame::new()
                                                .inner_margin(12)
                                                .corner_radius(7)
                                                .fill(
                                                    if view.selected.as_deref() == Some(&item.name)
                                                    {
                                                        ui.visuals().widgets.inactive.weak_bg_fill
                                                    } else {
                                                        egui::Color32::TRANSPARENT
                                                    },
                                                )
                                                .show(ui, |ui| {
                                                    ui.set_width((list_width - 32.0).max(80.0));
                                                    ui.spacing_mut().item_spacing.y = 6.0;
                                                    if !view.bulk {
                                                        ui.horizontal(|ui| {
                                                            let width = (ui.available_width()
                                                                - 100.0)
                                                                .max(60.0);
                                                            let chosen = ui
                                                                .allocate_ui_with_layout(
                                                                    egui::vec2(width, 34.0),
                                                                    egui::Layout::left_to_right(
                                                                        egui::Align::Center,
                                                                    ),
                                                                    |ui| {
                                                                        ui.set_width(width);
                                                                        ui.add(
                                                                    egui::Button::new(
                                                                        egui::RichText::new(
                                                                            &item.name,
                                                                        )
                                                                        .strong(),
                                                                    )
                                                                    .frame(false)
                                                                    .truncate(),
                                                                )
                                                                .clicked()
                                                                    },
                                                                )
                                                                .inner;
                                                            if chosen {
                                                                view.selected =
                                                                    Some(item.name.clone());
                                                            }
                                                            ui.with_layout(
                                                                egui::Layout::right_to_left(
                                                                    egui::Align::Center,
                                                                ),
                                                                |ui| {
                                                                    item_action(
                                                                        ui,
                                                                        item,
                                                                        view,
                                                                        &mut actions,
                                                                    );
                                                                },
                                                            );
                                                        });
                                                    } else if ui
                                                        .add(
                                                            egui::Button::new(
                                                                egui::RichText::new(&item.name)
                                                                    .strong(),
                                                            )
                                                            .frame(false),
                                                        )
                                                        .clicked()
                                                    {
                                                        view.selected = Some(item.name.clone());
                                                    }
                                                    ui.horizontal_wrapped(|ui| {
                                                        ui.weak(&item.kind);
                                                        if item.update_available {
                                                            ui.colored_label(
                                                                accent,
                                                                "Update available",
                                                            );
                                                        } else if item.installed {
                                                            ui.colored_label(good, "Installed");
                                                        }
                                                        if view.bulk {
                                                            item_action(
                                                                ui,
                                                                item,
                                                                view,
                                                                &mut actions,
                                                            );
                                                        }
                                                    });
                                                    if let Some(CatalogItemOutcome::Failed {
                                                        message,
                                                    }) = view.results.get(&item.name)
                                                    {
                                                        ui.colored_label(error, message);
                                                    }
                                                })
                                        },
                                    );
                                    // Buttons inside the row have priority over this parent
                                    // response, so action clicks do not also open the details.
                                    if row.response.clicked() {
                                        view.selected = Some(item.name.clone());
                                    }
                                    ui.add_space(4.0);
                                });
                            }
                        });
                },
            );
            if wide {
                crate::surface::divider(ui, height);
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("catalog-details")
                            .scroll_bar_visibility(
                                egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .max_height(height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if let Some(item) = snapshot
                                    .items
                                    .iter()
                                    .find(|i| Some(&i.name) == view.selected.as_ref())
                                {
                                    details(ui, item, view, &mut actions, good, accent);
                                }
                            });
                    },
                );
            }
        });
    }
    ui.separator();
    let pending = view.pending(snapshot);
    ui.horizontal_wrapped(|ui| {
        if !pending.is_empty() {
            ui.weak(format!("{} changes queued", pending.len()));
            if crate::surface::primary(ui, format!("Apply {} changes", pending.len())).clicked() {
                actions.push(CatalogUiAction::Apply(pending));
            }
            if ui.button("Discard").clicked() {
                view.reseed(snapshot);
            }
        } else {
            ui.weak(if view.bulk {
                "Queue actions, then apply them together."
            } else {
                "Select an extension to see its details."
            });
            let updates: Vec<_> = snapshot
                .items
                .iter()
                .filter(|i| i.update_available)
                .map(|i| (i.name.clone(), true))
                .collect();
            if !updates.is_empty()
                && ui
                    .button(format!("Update all ({})", updates.len()))
                    .clicked()
            {
                actions.push(CatalogUiAction::Apply(updates));
            }
        }
    });
    actions
}

fn item_action(
    ui: &mut egui::Ui,
    item: &CatalogItem,
    view: &mut CatalogView,
    actions: &mut Vec<CatalogUiAction>,
) {
    let pending = view.touched.contains(&item.name)
        && (view.checked(&item.name, item.installed) != item.installed
            || (view.checked(&item.name, item.installed) && item.update_available));
    if pending {
        ui.weak(if !view.checked(&item.name, item.installed) {
            "Removal queued"
        } else if item.installed {
            "Update queued"
        } else {
            "Install queued"
        });
        if ui.button("Undo").clicked() {
            view.touched.remove(&item.name);
            view.checked.insert(item.name.clone(), item.installed);
        }
        return;
    }
    let install = !item.installed || item.update_available;
    let label = if item.update_available {
        "Update"
    } else if item.installed {
        "Remove"
    } else {
        "Install"
    };
    let label = if view.bulk {
        format!("Queue {}", label.to_lowercase())
    } else {
        label.into()
    };
    if ui.button(label).clicked() {
        if view.bulk {
            view.set_checked(&item.name, install);
        } else {
            actions.push(CatalogUiAction::Apply(vec![(item.name.clone(), install)]));
        }
    }
}

fn details(
    ui: &mut egui::Ui,
    item: &CatalogItem,
    view: &mut CatalogView,
    actions: &mut Vec<CatalogUiAction>,
    good: egui::Color32,
    accent: egui::Color32,
) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(&item.name).size(20.0).strong());
    ui.horizontal_wrapped(|ui| {
        ui.weak(&item.kind);
        if let Some(version) = &item.version {
            ui.weak(format!("v{version}"));
        }
        if item.update_available {
            ui.colored_label(accent, "Update available");
        } else if item.installed {
            ui.colored_label(good, "Installed");
        }
    });
    ui.label(&item.description);
    item_action(ui, item, view, actions);
    if item.installed && item.update_available && ui.small_button("Remove instead").clicked() {
        if view.bulk {
            view.set_checked(&item.name, false);
        } else {
            actions.push(CatalogUiAction::Apply(vec![(item.name.clone(), false)]));
        }
    }
    ui.separator();
    if let Some(about) = &item.long_description {
        ui.label(about);
    }
    for (label, value) in [
        ("Author", &item.author),
        ("Updated", &item.updated_at),
        ("Requires Bone", &item.min_bone_version),
    ] {
        if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
            ui.weak(format!("{label}: {value}"));
        }
    }
    if !item.dependencies.is_empty() {
        ui.strong("Dependencies");
        ui.label(item.dependencies.join(", "));
    }
    if !item.permissions.is_empty() {
        ui.strong("Permissions");
        ui.label(item.permissions.join(", "));
    }
    ui.horizontal_wrapped(|ui| {
        if let Some(url) = &item.repository {
            ui.hyperlink_to("Repository", url);
        }
        if let Some(url) = &item.documentation {
            ui.hyperlink_to("Documentation", url);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_protocol::{CatalogItemResult, CatalogSnapshot};

    #[test]
    fn search_and_tabs_filter_without_changing_pending_actions() {
        let snapshot = CatalogSnapshot {
            revision: "1".into(),
            items: vec![item("memory", true, true), item("browser", false, false)],
        };
        let mut view = CatalogView::default();
        view.sync(&snapshot);
        view.set_checked("browser", true);
        assert!(matches_item(
            &snapshot.items[0],
            Filter::Updates,
            "memory tool"
        ));
        assert!(!matches_item(&snapshot.items[1], Filter::Installed, ""));
        assert!(!matches_item(&snapshot.items[0], Filter::Browse, "missing"));
        view.query = "memory".into();
        view.filter = Filter::Installed;
        view.sync(&snapshot);
        assert_eq!(view.pending(&snapshot), vec![("browser".into(), true)]);
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

    #[test]
    fn render_without_clicks_returns_no_actions() {
        let ctx = egui::Context::default();
        let snapshot = CatalogSnapshot {
            revision: "r1".into(),
            items: vec![
                item("beta", true, true),
                item("alpha", true, false),
                item("gamma", false, false),
            ],
        };
        let palette = Palette::default();
        let mut view = CatalogView::default();
        let mut actions = Vec::new();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            actions = render(ui, &snapshot, &mut view, &palette);
        })
        .textures_delta
        .clear();
        assert!(actions.is_empty());
    }

    #[test]
    fn reseed_checks_installed_and_update_items_with_nothing_pending() {
        let snapshot = CatalogSnapshot {
            revision: "r1".into(),
            items: vec![
                item("beta", true, true),
                item("alpha", true, false),
                item("gamma", false, false),
            ],
        };
        let mut view = CatalogView::default();
        view.sync(&snapshot);
        // Installed and update-available rows start checked; available does not.
        assert!(view.checked("beta", false));
        assert!(view.checked("alpha", false));
        assert!(!view.checked("gamma", true));
        // Freshly seeded selection matches the snapshot, so nothing is pending.
        assert!(view.pending(&snapshot).is_empty());
    }

    #[test]
    fn pending_reports_only_touched_differences() {
        let snapshot = CatalogSnapshot {
            revision: "r1".into(),
            items: vec![
                item("beta", true, true),
                item("alpha", true, false),
                item("gamma", false, false),
            ],
        };
        let mut view = CatalogView::default();
        view.sync(&snapshot);
        // Install gamma, remove alpha; leave beta untouched.
        view.set_checked("gamma", true);
        view.set_checked("alpha", false);
        let mut pending = view.pending(&snapshot);
        pending.sort();
        assert_eq!(
            pending,
            vec![("alpha".to_string(), false), ("gamma".to_string(), true),]
        );
    }

    #[test]
    fn apply_result_enters_result_phase_then_clear_resets() {
        let snapshot = CatalogSnapshot {
            revision: "r2".into(),
            items: vec![item("gamma", true, false)],
        };
        let result = CatalogApplyResult {
            snapshot,
            results: vec![CatalogItemResult {
                name: "gamma".into(),
                outcome: CatalogItemOutcome::Installed,
            }],
            changed: true,
            extensions_reloaded: true,
        };
        let mut view = CatalogView::default();
        view.apply_result(&result);
        assert!(view.banner.is_some());
        assert_eq!(
            view.results.get("gamma"),
            Some(&CatalogItemOutcome::Installed)
        );
        view.clear_result();
        assert!(view.banner.is_none());
        assert!(view.results.is_empty());
    }
}
