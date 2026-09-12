//! Native renderer for the unified Plugins surface.
//!
//! Under the "everything is a plugin" model a catalog package is a plugin,
//! whether it contributes tools, commands, hooks, themes, or settings; the
//! package `kind` stays an internal attribute rather than a user-facing
//! category. This screen lists every catalog item and offers
//! install/update/remove, plus enable/disable for the items the daemon can
//! toggle (plugins only), reusing the daemon's revisioned `CatalogApply`
//! request. It shares the catalog snapshot's filter, search, outcome-banner,
//! and palette helpers rather than duplicating them.

use std::collections::HashMap;

use bone_protocol::{
    CatalogAction, CatalogActionKind, CatalogApplyResult, CatalogItem, CatalogItemOutcome,
    CatalogSnapshot,
};
use eframe::egui;

use crate::catalog::{Filter, color, matches_item, result_banner};
use crate::theme::Palette;

/// Persistent state for the Plugins surface: search/filter, the selected
/// plugin, and the per-plugin outcome banner from the last apply.
#[derive(Debug, Default)]
pub struct PluginsView {
    /// Snapshot revision the current selection was seeded from.
    revision: String,
    query: String,
    filter: Filter,
    selected: Option<String>,
    /// Per-plugin outcome from the last apply, keyed by name.
    results: HashMap<String, CatalogItemOutcome>,
    /// One-line banner shown in the read-only result phase.
    banner: Option<String>,
}

impl PluginsView {
    fn sync(&mut self, snapshot: &CatalogSnapshot) {
        if self.revision == snapshot.revision {
            return;
        }
        self.revision = snapshot.revision.clone();
        self.reseed();
    }

    fn reseed(&mut self) {
        self.results.clear();
        self.banner = None;
    }

    /// Record an apply result: adopt its snapshot, overlay per-plugin outcome
    /// tags, and enter the read-only result phase.
    pub fn apply_result(&mut self, result: &CatalogApplyResult) {
        self.revision = result.snapshot.revision.clone();
        self.reseed();
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
}

/// Whether the daemon can enable/disable this item. Only `kind == "plugin"`
/// packages are toggleable; tools and commands are managed by install/remove.
fn toggleable(item: &CatalogItem) -> bool {
    item.kind == "plugin"
}

/// The enable/disable toggle offered for an item, if any. Only installed
/// `kind == "plugin"` packages can be toggled — the daemon rejects Enable and
/// Disable for every other kind — so tools and commands expose install/remove
/// only.
fn toggle(item: &CatalogItem) -> Option<(&'static str, CatalogActionKind)> {
    if !item.installed || !toggleable(item) {
        return None;
    }
    Some(if item.enabled {
        ("Disable", CatalogActionKind::Disable)
    } else {
        ("Enable", CatalogActionKind::Enable)
    })
}

fn install(name: &str) -> CatalogAction {
    CatalogAction {
        name: name.to_string(),
        action: CatalogActionKind::Install,
    }
}

/// Install/update/remove plus enable/disable controls for one plugin row.
/// Enable/disable is only offered for installed plugins (the daemon rejects it
/// for any other kind). Actions are emitted immediately (no batch phase).
fn plugin_actions(ui: &mut egui::Ui, item: &CatalogItem, actions: &mut Vec<CatalogAction>) {
    if let Some((label, action)) = toggle(item) {
        if crate::surface::primary(ui, label).clicked() {
            actions.push(CatalogAction {
                name: item.name.clone(),
                action,
            });
        }
    }
    if item.update_available {
        if ui.button("Update").clicked() {
            actions.push(install(&item.name));
        }
    } else if item.installed {
        if ui.button("Remove").clicked() {
            actions.push(CatalogAction {
                name: item.name.clone(),
                action: CatalogActionKind::Remove,
            });
        }
    } else if ui.button("Install").clicked() {
        actions.push(install(&item.name));
    }
}

/// Render the plugin list and details pane. Returns the actions the user
/// triggered (install/update/remove/enable/disable), ready for `CatalogApply`.
pub fn render(
    ui: &mut egui::Ui,
    snapshot: &CatalogSnapshot,
    view: &mut PluginsView,
    palette: &Palette,
) -> Vec<CatalogAction> {
    let mut actions = Vec::new();
    view.sync(snapshot);
    ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);
    ui.spacing_mut().interact_size.y = crate::theme::CONTROL_HEIGHT;
    let accent = color(&palette.accent, ui.visuals().hyperlink_color);
    let good = color(&palette.good, egui::Color32::from_rgb(90, 200, 120));
    let error = ui.visuals().error_fg_color;
    ui.add(
        egui::TextEdit::singleline(&mut view.query)
            .hint_text("Search plugins…")
            .margin(egui::vec2(12.0, 7.0))
            .desired_width(f32::INFINITY),
    );
    let plugins: Vec<&CatalogItem> = snapshot.items.iter().collect();
    ui.horizontal_wrapped(|ui| {
        for (filter, title, count) in [
            (Filter::Browse, "Browse", plugins.len()),
            (
                Filter::Installed,
                "Installed",
                plugins.iter().filter(|i| i.installed).count(),
            ),
            (
                Filter::Updates,
                "Updates",
                plugins.iter().filter(|i| i.update_available).count(),
            ),
        ] {
            if crate::surface::tab(ui, view.filter == filter, format!("{title}  {count}")).clicked()
            {
                view.filter = filter;
            }
        }
    });
    let enabled = plugins
        .iter()
        .filter(|item| item.installed && item.enabled)
        .count();
    let updates = plugins.iter().filter(|item| item.update_available).count();
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(format!("{} plugins", plugins.len()))
                .small()
                .weak(),
        );
        ui.label(
            egui::RichText::new(format!("{enabled} enabled"))
                .small()
                .weak(),
        );
        if updates > 0 {
            ui.colored_label(
                accent,
                format!("{updates} update{}", if updates == 1 { "" } else { "s" }),
            );
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
    let items: Vec<&CatalogItem> = plugins
        .iter()
        .filter(|i| matches_item(i, view.filter, &query))
        .copied()
        .collect();
    let height = (ui.available_height()
        - if ui.available_width() < 500.0 {
            92.0
        } else {
            62.0
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
                if plugins.is_empty() {
                    "No plugins available"
                } else {
                    "No matching plugins"
                },
                if view.filter == Filter::Updates && query.is_empty() {
                    "Your installed plugins are up to date."
                } else {
                    "Try a different search or browse another tab."
                },
            );
        });
    } else if !wide && view.selected.is_some() {
        egui::ScrollArea::vertical()
            .id_salt("plugins-details")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if ui.button("Back to plugins").clicked() {
                    view.selected = None;
                }
                if let Some(item) = items
                    .iter()
                    .find(|i| Some(&i.name) == view.selected.as_ref())
                {
                    details(ui, item, &mut actions, good, accent);
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
                        .id_salt(("plugins-list", view.filter as u8, &query))
                        .scroll_bar_visibility(
                            egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible,
                        )
                        .max_height(height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for item in &items {
                                ui.push_id(&item.name, |ui| {
                                    egui::Frame::new()
                                        .inner_margin(8)
                                        .corner_radius(7)
                                        .fill(if view.selected.as_deref() == Some(&item.name) {
                                            ui.visuals().widgets.inactive.weak_bg_fill
                                        } else {
                                            egui::Color32::TRANSPARENT
                                        })
                                        .show(ui, |ui| {
                                            ui.set_width((list_width - 32.0).max(80.0));
                                            ui.spacing_mut().item_spacing.y = 3.0;
                                            ui.horizontal(|ui| {
                                                let width =
                                                    (ui.available_width() - 220.0).max(60.0);
                                                let chosen = ui
                                                    .allocate_ui_with_layout(
                                                        egui::vec2(width, 30.0),
                                                        egui::Layout::left_to_right(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            ui.set_width(width);
                                                            ui.add(
                                                                egui::Button::new(
                                                                    egui::RichText::new(&item.name)
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
                                                    view.selected = Some(item.name.clone());
                                                }
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        plugin_actions(ui, item, &mut actions);
                                                    },
                                                );
                                            });
                                            ui.horizontal_wrapped(|ui| {
                                                if item.installed && item.enabled {
                                                    ui.colored_label(good, "Enabled");
                                                } else if item.installed {
                                                    ui.weak("Disabled");
                                                }
                                                if item.update_available {
                                                    ui.colored_label(accent, "Update available");
                                                } else if item.installed {
                                                    ui.colored_label(good, "Installed");
                                                }
                                            });
                                            let summary = item
                                                .description
                                                .split(['.', '\n'])
                                                .next()
                                                .unwrap_or("")
                                                .trim();
                                            if !summary.is_empty() {
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(summary).small().weak(),
                                                    )
                                                    .truncate(),
                                                );
                                            }
                                            if let Some(CatalogItemOutcome::Failed { message }) =
                                                view.results.get(&item.name)
                                            {
                                                ui.colored_label(error, message);
                                            }
                                        });
                                    ui.add_space(2.0);
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
                            .id_salt("plugins-details")
                            .scroll_bar_visibility(
                                egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible,
                            )
                            .max_height(height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if let Some(item) = items
                                    .iter()
                                    .find(|i| Some(&i.name) == view.selected.as_ref())
                                {
                                    details(ui, item, &mut actions, good, accent);
                                }
                            });
                    },
                );
            }
        });
    }
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.weak("Enable or disable plugins; install, update, or remove them.");
        let updates: Vec<_> = plugins
            .iter()
            .filter(|i| i.update_available)
            .map(|i| install(&i.name))
            .collect();
        if !updates.is_empty()
            && ui
                .button(format!("Update all ({})", updates.len()))
                .clicked()
        {
            actions.extend(updates);
        }
    });
    actions
}

fn details(
    ui: &mut egui::Ui,
    item: &CatalogItem,
    actions: &mut Vec<CatalogAction>,
    good: egui::Color32,
    accent: egui::Color32,
) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new(&item.name).size(18.0).strong());
    ui.horizontal_wrapped(|ui| {
        if let Some(version) = &item.version {
            ui.weak(format!("v{version}"));
        }
        if item.installed && item.enabled {
            ui.colored_label(good, "Enabled");
        } else if item.installed {
            ui.weak("Disabled");
        }
        if item.update_available {
            ui.colored_label(accent, "Update available");
        } else if item.installed {
            ui.colored_label(good, "Installed");
        }
    });
    ui.label(&item.description);
    ui.weak("Bone Lua plugins run unsandboxed — only install plugins you trust.");
    ui.horizontal_wrapped(|ui| {
        plugin_actions(ui, item, actions);
    });
    if item.installed && item.update_available && ui.small_button("Remove instead").clicked() {
        actions.push(CatalogAction {
            name: item.name.clone(),
            action: CatalogActionKind::Remove,
        });
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

    fn plugin(name: &str, installed: bool, enabled: bool) -> CatalogItem {
        CatalogItem {
            name: name.into(),
            kind: "plugin".into(),
            description: format!("{name} plugin"),
            installed,
            enabled,
            ..CatalogItem::default()
        }
    }

    #[test]
    fn render_lists_all_items_and_returns_no_actions_without_clicks() {
        let ctx = egui::Context::default();
        let snapshot = CatalogSnapshot {
            revision: "r1".into(),
            items: vec![
                plugin("alpha", true, true),
                CatalogItem {
                    name: "some-tool".into(),
                    kind: "tool".into(),
                    ..CatalogItem::default()
                },
            ],
        };
        let palette = Palette::default();
        let mut view = PluginsView::default();
        let mut actions = Vec::new();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            actions = render(ui, &snapshot, &mut view, &palette);
        })
        .textures_delta
        .clear();
        assert!(actions.is_empty());
    }

    /// The merged surface lists every catalog item, not just `kind == "plugin"`
    /// packages, so a tool-only snapshot still renders (and selects) its row.
    #[test]
    fn render_lists_non_plugin_items() {
        let ctx = egui::Context::default();
        let snapshot = CatalogSnapshot {
            revision: "r1".into(),
            items: vec![CatalogItem {
                name: "some-tool".into(),
                kind: "tool".into(),
                installed: true,
                ..CatalogItem::default()
            }],
        };
        let mut view = PluginsView::default();
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui, &snapshot, &mut view, &Palette::default());
            },
        )
        .textures_delta
        .clear();
        assert_eq!(view.selected.as_deref(), Some("some-tool"));
    }

    /// Enable/disable is only offered for installed plugin packages; the daemon
    /// rejects it for tools and commands, so those never get a toggle.
    #[test]
    fn enable_disable_only_offered_for_installed_plugins() {
        let installed_tool = CatalogItem {
            name: "tool".into(),
            kind: "tool".into(),
            installed: true,
            ..CatalogItem::default()
        };
        assert_eq!(toggle(&installed_tool), None);

        let not_installed_plugin = CatalogItem {
            name: "plugin".into(),
            kind: "plugin".into(),
            installed: false,
            ..CatalogItem::default()
        };
        assert_eq!(toggle(&not_installed_plugin), None);

        assert_eq!(
            toggle(&plugin("alpha", true, true)),
            Some(("Disable", CatalogActionKind::Disable))
        );
        assert_eq!(
            toggle(&plugin("beta", true, false)),
            Some(("Enable", CatalogActionKind::Enable))
        );
    }

    #[test]
    fn apply_result_tags_outcomes_and_clear_resets() {
        let snapshot = CatalogSnapshot {
            revision: "r2".into(),
            items: vec![plugin("alpha", true, false)],
        };
        let result = CatalogApplyResult {
            snapshot,
            results: vec![bone_protocol::CatalogItemResult {
                name: "alpha".into(),
                outcome: CatalogItemOutcome::Disabled,
            }],
            changed: true,
            extensions_reloaded: true,
        };
        let mut view = PluginsView::default();
        view.apply_result(&result);
        assert!(view.banner.is_some());
        assert_eq!(
            view.results.get("alpha"),
            Some(&CatalogItemOutcome::Disabled)
        );
        view.clear_result();
        assert!(view.banner.is_none());
        assert!(view.results.is_empty());
    }
}
