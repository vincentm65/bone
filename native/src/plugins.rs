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

/// Horizontal space reserved at the right edge of a compact plugin row for the
/// inline action cluster, sized per control. Reserving the cluster's width up
/// front lets the name column take the remainder instead of overflowing into
/// it (the cluster is laid out right-to-left, last).
const CLOSE_ACTION_WIDTH: f32 = 34.0;
const INSTALL_ACTION_WIDTH: f32 = 86.0;
const UPDATE_ACTION_WIDTH: f32 = 82.0;
const TOGGLE_ACTION_WIDTH: f32 = 90.0;
const CONFIRM_ACTIONS_WIDTH: f32 = 240.0;

/// Bounds for the row's name column. It tracks the width the inline action
/// cluster leaves, but never collapses below a readable minimum — past that the
/// cluster moves to its own line beneath the name instead.
const NAME_MIN_WIDTH: f32 = 56.0;
const NAME_MAX_WIDTH: f32 = 260.0;

/// Persistent state for the Plugins surface: search/filter, the selected
/// plugin, and the per-plugin outcome banner from the last apply.
#[derive(Debug, Default)]
pub struct PluginsView {
    /// Snapshot revision the current selection was seeded from.
    revision: String,
    query: String,
    filter: Filter,
    /// Plugin whose row is expanded to show its description and metadata.
    selected: Option<String>,
    /// Plugin awaiting an inline Remove confirmation.
    confirm_remove: Option<String>,
    /// Open Settings from an expanded plugin's management controls.
    pub open_settings: bool,
    /// Refresh requested from the toolbar.
    pub refresh: bool,
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
        self.confirm_remove = None;
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

fn remove(name: &str) -> CatalogAction {
    CatalogAction {
        name: name.to_string(),
        action: CatalogActionKind::Remove,
    }
}

/// Inline install/update plus a close (`×`) remove control and enable/disable
/// controls for one plugin row. Enable/disable is only offered for installed
/// plugins (the daemon rejects it for any other kind). Install/update emit
/// immediately; the `×` routes through `confirm_remove` so the row asks before
/// the destructive action. Rendered in a `right_to_left` layout, so the first
/// widget added lands at the right edge (the `×` remove control).
fn row_actions(
    ui: &mut egui::Ui,
    item: &CatalogItem,
    actions: &mut Vec<CatalogAction>,
    confirm_remove: &mut Option<String>,
) {
    if item.installed && ui.button("×").on_hover_text("Remove").clicked() {
        *confirm_remove = Some(item.name.clone());
    }
    if item.update_available {
        if ui.button("Update").clicked() {
            actions.push(install(&item.name));
        }
    } else if !item.installed && ui.button("Install").clicked() {
        actions.push(install(&item.name));
    }
    if let Some((label, action)) = toggle(item)
        && crate::surface::primary(ui, label).clicked()
    {
        actions.push(CatalogAction {
            name: item.name.clone(),
            action,
        });
    }
}

/// Width reserved for a row's inline action cluster, mirroring the controls
/// `row_actions` emits. The name column takes the remaining row width.
fn actions_reserve(item: &CatalogItem, spacing: f32) -> f32 {
    let mut width = 0.0;
    let mut controls = 0;
    if item.installed {
        width += CLOSE_ACTION_WIDTH;
        controls += 1;
    }
    if item.update_available {
        width += UPDATE_ACTION_WIDTH;
        controls += 1;
    } else if !item.installed {
        width += INSTALL_ACTION_WIDTH;
        controls += 1;
    }
    if toggle(item).is_some() {
        width += TOGGLE_ACTION_WIDTH;
        controls += 1;
    }
    if controls > 1 {
        width += spacing * (controls - 1) as f32;
    }
    width
}

/// The row's inline action cluster, right-aligned: either the Remove
/// confirmation controls or the install/update/remove/toggle controls.
fn row_cluster(
    ui: &mut egui::Ui,
    item: &CatalogItem,
    view: &mut PluginsView,
    actions: &mut Vec<CatalogAction>,
    confirming: bool,
) {
    if confirming {
        if ui.button("Remove").clicked() {
            actions.push(remove(&item.name));
            view.confirm_remove = None;
        }
        if ui.button("Cancel").clicked() {
            view.confirm_remove = None;
        }
        ui.weak("Remove?");
    } else {
        row_actions(ui, item, actions, &mut view.confirm_remove);
    }
}

/// Section heading above a group of rows, with the group's item count.
fn section_header(ui: &mut egui::Ui, title: &str, count: usize) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(title).small().strong()).selectable(false));
        ui.add(
            egui::Label::new(egui::RichText::new(count.to_string()).small().weak())
                .selectable(false),
        );
    });
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
    ui.horizontal(|ui| {
        let refresh_width = 88.0;
        let search_width =
            (ui.available_width() - refresh_width - ui.spacing().item_spacing.x).max(96.0);
        ui.add_sized(
            egui::vec2(search_width, crate::theme::CONTROL_HEIGHT),
            egui::TextEdit::singleline(&mut view.query)
                .hint_text("Search plugins…")
                .margin(egui::vec2(12.0, 7.0)),
        );
        if ui.add(egui::Button::new("Refresh")).clicked() {
            view.refresh = true;
        }
    });
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
    let footer_reserve = if ui.available_width() < 460.0 {
        72.0
    } else {
        50.0
    };
    let height = (ui.available_height() - footer_reserve).max(48.0);
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
    } else {
        // Order rows by state: enabled, then installed-but-disabled, then not
        // installed, each under its own heading. Within a group the catalog
        // order is preserved.
        let mut groups: [(&str, Vec<&CatalogItem>); 3] = [
            ("Installed", Vec::new()),
            ("Disabled", Vec::new()),
            ("Not installed", Vec::new()),
        ];
        for &item in &items {
            let index = if !item.installed {
                2
            } else if item.enabled {
                0
            } else {
                1
            };
            groups[index].1.push(item);
        }
        egui::ScrollArea::vertical()
            .id_salt(("plugins-list", view.filter as u8, &query))
            .scroll_bar_visibility(
                egui::containers::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
            )
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (title, group) in &groups {
                    if group.is_empty() {
                        continue;
                    }
                    section_header(ui, title, group.len());
                    for &item in group {
                        ui.push_id(&item.name, |ui| {
                            plugin_row(ui, item, view, &mut actions, accent, error);
                        });
                        ui.add_space(2.0);
                    }
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

/// One compact list row: name (click to expand) and inline actions, with the
/// expanded detail and any failed outcome rendered below.
fn plugin_row(
    ui: &mut egui::Ui,
    item: &CatalogItem,
    view: &mut PluginsView,
    actions: &mut Vec<CatalogAction>,
    accent: egui::Color32,
    error: egui::Color32,
) {
    let expanded = view.selected.as_deref() == Some(item.name.as_str());
    let confirming = view.confirm_remove.as_deref() == Some(item.name.as_str());
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(6, 3))
        .corner_radius(6)
        .fill(if expanded {
            ui.visuals().selection.bg_fill
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(if expanded {
            egui::Stroke::new(1.0, accent)
        } else {
            egui::Stroke::NONE
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let full = ui.available_width();
            // The name column takes whatever the inline action cluster leaves.
            // The cluster is laid out right-to-left after the name, so reserving
            // its width up front keeps it (including the Remove confirmation
            // strip) from overflowing the row. When the row is too narrow to
            // seat both, the cluster drops to its own line beneath the name.
            let gap = ui.spacing().item_spacing.x;
            let reserve = if confirming {
                CONFIRM_ACTIONS_WIDTH
            } else {
                actions_reserve(item, gap)
            };
            let inline = full - reserve - gap >= NAME_MIN_WIDTH;
            let name_width = if inline {
                (full - reserve - gap).clamp(NAME_MIN_WIDTH, NAME_MAX_WIDTH)
            } else {
                full
            };
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(name_width, crate::theme::CONTROL_HEIGHT),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        let response = ui.add(
                            egui::Label::new(egui::RichText::new(&item.name).strong())
                                .truncate()
                                .sense(egui::Sense::click()),
                        );
                        if response.clicked() {
                            view.selected = if expanded {
                                None
                            } else {
                                Some(item.name.clone())
                            };
                        }
                    },
                );
                if inline {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        row_cluster(ui, item, view, actions, confirming);
                    });
                }
            });
            if !inline {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        row_cluster(ui, item, view, actions, confirming);
                    });
                });
            }
            if expanded {
                ui.add_space(2.0);
                expanded_details(ui, item, view);
            }
            if let Some(CatalogItemOutcome::Failed { message }) = view.results.get(&item.name) {
                ui.colored_label(error, message);
            }
        });
}

/// Expanded detail for one plugin, shown inline beneath its row.
fn expanded_details(ui: &mut egui::Ui, item: &CatalogItem, view: &mut PluginsView) {
    if !item.description.is_empty() {
        ui.label(&item.description);
    }
    ui.weak("Bone Lua plugins run unsandboxed — only install plugins you trust.");
    let mut meta = Vec::new();
    if let Some(version) = item.version.as_deref().filter(|v| !v.is_empty()) {
        meta.push(format!("v{version}"));
    }
    if let Some(author) = item.author.as_deref().filter(|v| !v.is_empty()) {
        meta.push(format!("by {author}"));
    }
    if let Some(updated) = item.updated_at.as_deref().filter(|v| !v.is_empty()) {
        meta.push(format!("updated {updated}"));
    }
    if let Some(requires) = item.min_bone_version.as_deref().filter(|v| !v.is_empty()) {
        meta.push(format!("requires Bone {requires}"));
    }
    if !meta.is_empty() {
        ui.weak(meta.join(" · "));
    }
    if let Some(about) = &item.long_description {
        ui.label(about);
    }
    if !item.dependencies.is_empty() {
        ui.weak(format!("Dependencies: {}", item.dependencies.join(", ")));
    }
    if !item.permissions.is_empty() {
        ui.weak(format!("Permissions: {}", item.permissions.join(", ")));
    }
    ui.horizontal_wrapped(|ui| {
        if item.installed && toggleable(item) && ui.small_button("Configure in Settings…").clicked()
        {
            view.open_settings = true;
        }
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
    /// packages, so a tool-only snapshot still renders its row. Rows start
    /// collapsed — selection now happens only on an explicit click.
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
        assert!(view.selected.is_none());
    }

    /// Rows are grouped under state headings — enabled, then installed-but-
    /// disabled, then not installed — each with its own count, and every heading
    /// precedes its rows.
    #[test]
    fn render_groups_items_by_state_under_headings() {
        let ctx = egui::Context::default();
        let snapshot = CatalogSnapshot {
            revision: "r1".into(),
            items: vec![
                plugin("enabled-one", true, true),
                CatalogItem {
                    name: "not-installed".into(),
                    kind: "tool".into(),
                    ..CatalogItem::default()
                },
                plugin("disabled-one", true, false),
            ],
        };
        let mut view = PluginsView::default();
        let mut texts: Vec<String> = Vec::new();
        for _ in 0..3 {
            let mut output = ctx.run_ui(
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
            );
            texts = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            output.textures_delta.clear();
        }
        for heading in ["Installed", "Disabled", "Not installed"] {
            assert!(
                texts.iter().any(|t| t == heading),
                "missing heading {heading}: {texts:?}"
            );
        }
        let index = |name: &str| texts.iter().position(|t| t == name).unwrap();
        assert!(index("Installed") < index("enabled-one"));
        assert!(index("Disabled") < index("disabled-one"));
        assert!(index("Not installed") < index("not-installed"));
        assert!(index("enabled-one") < index("disabled-one"));
        assert!(index("disabled-one") < index("not-installed"));
    }

    /// On a panel too narrow to seat the name beside the Remove confirmation,
    /// the controls drop to their own line instead of overlapping the name.
    #[test]
    fn confirm_controls_drop_below_name_when_row_is_narrow() {
        let ctx = egui::Context::default();
        let snapshot = CatalogSnapshot {
            revision: "r1".into(),
            items: vec![plugin("ask_user.lua", true, true)],
        };
        let mut view = PluginsView::default();
        // Seed the view from the snapshot first: `sync` reseeds and clears any
        // pending confirmation on the frame it adopts a new revision.
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 600.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui, &snapshot, &mut view, &Palette::default());
            },
        )
        .textures_delta
        .clear();
        view.confirm_remove = Some("ask_user.lua".into());
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(200.0, 600.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui, &snapshot, &mut view, &Palette::default());
            },
        );
        let texts: Vec<(String, egui::Rect)> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_owned(),
                    egui::Rect::from_min_size(t.pos, t.galley.size()),
                )),
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        let rect = |needle: &str| {
            texts
                .iter()
                .find(|(text, _)| text == needle)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("missing {needle}: {:?}", texts.len()))
        };
        let name = rect("ask_user.lua");
        for control in ["Remove", "Cancel", "Remove?"] {
            let control = rect(control);
            assert!(
                name.bottom() <= control.top() || !name.intersects(control),
                "confirm controls overlap the name on a narrow row: {name:?} vs {control:?}"
            );
        }
        // On this narrow row the controls sit below the name entirely.
        assert!(rect("Remove").top() >= name.bottom());
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
