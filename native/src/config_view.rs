//! Native mirror of the TUI's `ConfigView` (`tui/src/ui/app/mod.rs`): resolves
//! setting values from the daemon's schema/snapshot and validates edits before
//! they are sent. Keeping this logic separate from `main.rs` makes it testable
//! without an egui context.

use bone_protocol::{ConfigSchema, ConfigSnapshot, SettingDefinition};
use eframe::egui;
use std::collections::HashMap;

/// Schema + snapshot pair backing the config dialog.
#[derive(Debug, Clone, Default)]
pub struct ConfigView {
    pub schema: Option<ConfigSchema>,
    pub snapshot: Option<ConfigSnapshot>,
}

impl ConfigView {
    pub fn new(schema: Option<ConfigSchema>, snapshot: Option<ConfigSnapshot>) -> Self {
        Self { schema, snapshot }
    }

    #[cfg(test)]
    pub fn revision(&self) -> u64 {
        self.snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.revision)
    }

    /// Find a setting definition by dotted path, searching nested pages.
    /// Test-only helper: the renderer reads fields directly from the schema.
    #[cfg(test)]
    pub fn field(&self, path: &str) -> Option<&SettingDefinition> {
        fn find<'a>(
            pages: &'a [bone_protocol::ConfigPage],
            path: &str,
        ) -> Option<&'a SettingDefinition> {
            pages.iter().find_map(|page| {
                page.fields
                    .iter()
                    .find(|field| field.path == path)
                    .or_else(|| find(&page.pages, path))
            })
        }
        self.schema
            .as_ref()
            .and_then(|schema| find(&schema.pages, path))
    }

    /// Current value for a field: snapshot value, else the schema's own value,
    /// else the field default.
    pub fn value(&self, field: &SettingDefinition) -> serde_json::Value {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| {
                field
                    .path
                    .split('.')
                    .try_fold(&snapshot.values, |value, part| value.get(part))
                    .cloned()
            })
            .or_else(|| field.value.clone())
            .unwrap_or_else(|| field.default.clone())
    }

    /// Value rendered as a compact string (strings unquoted).
    pub fn render_value(&self, field: &SettingDefinition) -> String {
        let value = self.value(field);
        value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string())
    }

    pub fn disabled_tools(&self) -> &[String] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.disabled_tools.as_slice())
    }

    pub fn disabled_commands(&self) -> &[String] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.disabled_commands.as_slice())
    }

    pub fn disabled_plugins(&self) -> &[String] {
        self.snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.disabled_plugins.as_slice())
    }

    /// Whether a tool/command/plugin is enabled. Enablement is tracked separately
    /// from `values` in the snapshot's disabled lists, so `tools.<name>` /
    /// `commands.<name>` / `plugins.<name>` schema fields cannot read it from
    /// [`Self::value`].
    pub fn is_enabled(&self, namespace: &str, name: &str) -> bool {
        match namespace {
            "tools" => !self.disabled_tools().iter().any(|entry| entry == name),
            "commands" => !self.disabled_commands().iter().any(|entry| entry == name),
            "plugins" => !self.disabled_plugins().iter().any(|entry| entry == name),
            _ => true,
        }
    }
}

/// Validate raw text against a field's declared type, mirroring the TUI.
pub fn parse_config_value(
    field: &SettingDefinition,
    input: &str,
) -> Result<serde_json::Value, String> {
    fn validate_bounds(field: &SettingDefinition, value: f64) -> Result<(), String> {
        if field.min.is_some_and(|min| value < min) {
            return Err(format!("must be at least {}", field.min.unwrap()));
        }
        if field.max.is_some_and(|max| value > max) {
            return Err(format!("must be at most {}", field.max.unwrap()));
        }
        Ok(())
    }

    match field.value_type.as_str() {
        "bool" => match input.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" => Ok(serde_json::Value::Bool(true)),
            "false" | "off" | "no" => Ok(serde_json::Value::Bool(false)),
            _ => Err("expected true/false, on/off, or yes/no".into()),
        },
        "number" => {
            if field.integer == Some(true) {
                let value = input
                    .parse::<i64>()
                    .map_err(|_| "expected an integer".to_string())?;
                validate_bounds(field, value as f64)?;
                Ok(serde_json::Value::from(value))
            } else {
                let value = input
                    .parse::<f64>()
                    .map_err(|_| "expected a number".to_string())?;
                if !value.is_finite() {
                    return Err("expected a finite number".into());
                }
                validate_bounds(field, value)?;
                Ok(serde_json::Value::from(value))
            }
        }
        "enum" => {
            if field.options.iter().any(|option| option == input) {
                Ok(serde_json::Value::String(input.into()))
            } else {
                Err(format!("expected one of: {}", field.options.join(", ")))
            }
        }
        "string" => Ok(serde_json::Value::String(input.into())),
        other => Err(format!("unsupported setting type `{other}`")),
    }
}

/// A mutation requested by the config dialog, applied after the window closes.
pub enum ConfigUiAction {
    Set {
        path: String,
        value: serde_json::Value,
    },
    Reset {
        path: String,
    },
    /// Tool/command enablement toggle (uses the dedicated commands, since the
    /// value does not live in `snapshot.values`).
    SetEnabled {
        namespace: String,
        name: String,
        enabled: bool,
    },
}

/// Navigation and baseline values survive daemon refreshes and closing Settings.
#[derive(Debug, Default)]
pub struct SettingsUi {
    pub query: String,
    selected: String,
    baseline: HashMap<String, String>,
}

impl SettingsUi {
    pub fn sync(&mut self, view: &ConfigView, edits: &mut HashMap<String, String>) {
        fn visit(
            pages: &[bone_protocol::ConfigPage],
            view: &ConfigView,
            values: &mut HashMap<String, String>,
        ) {
            for page in pages {
                for field in &page.fields {
                    values.insert(field.path.clone(), view.render_value(field));
                }
                visit(&page.pages, view, values);
            }
        }
        let Some(schema) = &view.schema else {
            return;
        };
        let mut next = HashMap::new();
        visit(&schema.pages, view, &mut next);
        // Only replace untouched buffers. A change to another setting must not
        // destroy a draft, even when both changes arrive in the same snapshot.
        edits.retain(|path, text| {
            if let Some(value) = next.get(path) {
                if self.baseline.get(path) == Some(text) {
                    *text = value.clone();
                }
                true
            } else {
                false
            }
        });
        self.baseline = next;
    }
}

fn matches(field: &SettingDefinition, page: &str, query: &str) -> bool {
    query.split_whitespace().all(|word| {
        field.label.to_lowercase().contains(word)
            || field.path.to_lowercase().contains(word)
            || page.to_lowercase().contains(word)
    })
}

fn page_field_count(page: &bone_protocol::ConfigPage) -> usize {
    page.fields.len() + page.pages.iter().map(page_field_count).sum::<usize>()
}

/// Search spans every category, including extension-defined nested pages.
pub fn render_pages(
    ui: &mut egui::Ui,
    view: &ConfigView,
    edits: &mut HashMap<String, String>,
    state: &mut SettingsUi,
    action: &mut Option<ConfigUiAction>,
) {
    let Some(schema) = &view.schema else {
        return;
    };
    state.sync(view, edits);
    ui.spacing_mut().item_spacing = egui::vec2(10.0, 6.0);
    ui.spacing_mut().interact_size.y = crate::theme::CONTROL_HEIGHT;
    ui.add(
        egui::TextEdit::singleline(&mut state.query)
            .hint_text("Search settings…")
            .margin(egui::vec2(12.0, 7.0))
            .desired_width(f32::INFINITY),
    );
    ui.add_space(2.0);
    if !schema.pages.iter().any(|p| p.namespace == state.selected) {
        state.selected = schema
            .pages
            .first()
            .map(|p| p.namespace.clone())
            .unwrap_or_default();
    }
    let query = state.query.trim().to_lowercase();
    let height = (ui.available_height() - 8.0).max(80.0);
    let render_content = |ui: &mut egui::Ui,
                          selected: &str,
                          edits: &mut HashMap<String, String>,
                          action: &mut Option<ConfigUiAction>| {
        let height = (ui.available_height() - 8.0).max(48.0);
        egui::ScrollArea::vertical()
            .id_salt(("settings-fields", selected, &query))
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut count = 0;
                for page in &schema.pages {
                    if !query.is_empty() || page.namespace == selected {
                        count += render_page(ui, page, view, edits, action, &query, "");
                    }
                }
                if count == 0 {
                    crate::surface::empty(
                        ui,
                        "No matching settings",
                        "Try another name or a shorter search.",
                    );
                }
            });
    };
    if ui.available_width() >= 640.0 {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(172.0, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("settings-navigation")
                        .max_height(height)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("PREFERENCES").small().weak());
                            for page in &schema.pages {
                                let count = page_field_count(page);
                                let label = if count > 0 {
                                    format!("{}  {count}", page.title)
                                } else {
                                    page.title.clone()
                                };
                                if ui
                                    .add_sized(
                                        [164.0, crate::theme::CONTROL_HEIGHT],
                                        egui::Button::selectable(
                                            query.is_empty() && state.selected == page.namespace,
                                            label,
                                        ),
                                    )
                                    .clicked()
                                {
                                    state.selected = page.namespace.clone();
                                    state.query.clear();
                                }
                            }
                        });
                },
            );
            crate::surface::divider(ui, height);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    render_content(ui, &state.selected, edits, action);
                },
            );
        });
    } else {
        egui::ComboBox::from_id_salt("settings-category")
            .selected_text(
                schema
                    .pages
                    .iter()
                    .find(|p| p.namespace == state.selected)
                    .map_or("Settings", |p| p.title.as_str()),
            )
            .show_ui(ui, |ui| {
                for page in &schema.pages {
                    if ui
                        .selectable_value(&mut state.selected, page.namespace.clone(), &page.title)
                        .changed()
                    {
                        state.query.clear();
                    }
                }
            });
        render_content(ui, &state.selected, edits, action);
    }
}

fn render_page(
    ui: &mut egui::Ui,
    page: &bone_protocol::ConfigPage,
    view: &ConfigView,
    edits: &mut HashMap<String, String>,
    action: &mut Option<ConfigUiAction>,
    query: &str,
    parent: &str,
) -> usize {
    let title = if parent.is_empty() {
        page.title.clone()
    } else {
        format!("{parent} / {}", page.title)
    };
    let fields: Vec<_> = page
        .fields
        .iter()
        .filter(|f| matches(f, &title, query))
        .collect();
    let mut count = fields.len();
    let has_type = fields.iter().any(|field| field.kind.is_some());
    if !fields.is_empty() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            // The title sits outside the card so the card reads as content, and
            // uses the app's semibold family to establish a clear type step above
            // the 15.0 body text.
            ui.label(
                egui::RichText::new(&title)
                    .size(17.0)
                    .family(egui::FontFamily::Name("semibold".into())),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{} setting{}",
                    count,
                    if count == 1 { "" } else { "s" }
                ))
                .small()
                .weak(),
            );
        });
        ui.add_space(4.0);
        if has_type {
            render_type_header(ui);
        }
        // All of a page's rows share one card, separated by dividers. This keeps
        // the page a short scannable list instead of a stack of boxes.
        crate::surface::card(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    ui.separator();
                }
                ui.push_id(&field.path, |ui| {
                    render_field(ui, field, view, edits, action, has_type)
                });
            }
        });
    }
    for child in &page.pages {
        count += render_page(ui, child, view, edits, action, query, &title);
    }
    count
}

fn field_hint(field: &SettingDefinition) -> String {
    let mut parts = Vec::new();
    match (field.min, field.max) {
        (Some(min), Some(max)) => parts.push(format!("Range: {min}–{max}")),
        (Some(min), None) => parts.push(format!("Minimum: {min}")),
        (None, Some(max)) => parts.push(format!("Maximum: {max}")),
        _ => {}
    }
    match field.reload_behavior.as_str() {
        "restart" | "restart_required" => parts.push("Requires a restart".into()),
        "reload" | "reload_extensions" => parts.push("Reloads extensions".into()),
        "next_turn" => parts.push("Applies next turn".into()),
        _ => {}
    }
    parts.join(" · ")
}

/// Width reserved for the trailing reset affordance.
const RESET_WIDTH: f32 = 34.0;

fn render_field(
    ui: &mut egui::Ui,
    field: &SettingDefinition,
    view: &ConfigView,
    edits: &mut HashMap<String, String>,
    action: &mut Option<ConfigUiAction>,
    has_type: bool,
) {
    let enablement = field
        .path
        .strip_prefix("tools.")
        .map(|n| ("tools", n))
        .or_else(|| {
            field
                .path
                .strip_prefix("commands.")
                .map(|n| ("commands", n))
        })
        .or_else(|| field.path.strip_prefix("plugins.").map(|n| ("plugins", n)));
    // Enablement rows have no value to reset; only offer reset when the current
    // value actually differs from the schema default.
    let reset = enablement.is_none() && view.value(field) != field.default;
    let label = |ui: &mut egui::Ui| {
        if has_type {
            ui.horizontal(|ui| {
                type_cell(ui, field.kind.as_deref());
                ui.label(egui::RichText::new(&field.label).strong())
                    .on_hover_text(&field.path);
            });
        } else {
            ui.label(egui::RichText::new(&field.label).strong())
                .on_hover_text(&field.path);
        }
        let hint = field_hint(field);
        if !hint.is_empty() {
            ui.label(egui::RichText::new(hint).small().weak());
        }
    };
    let multiline = field.path == "general.system_prompt";
    let row_height = if multiline {
        132.0
    } else {
        crate::theme::CONTROL_HEIGHT
    };
    if ui.available_width() >= 460.0 {
        let width = ui.available_width();
        // The control column keeps a fixed width across every row so values line
        // up in one scannable column; reset always reserves its trailing slot,
        // whether or not a glyph is drawn, so controls stay aligned. Account for
        // the two horizontal-layout gaps so the row never grows its parent width
        // on successive egui layout passes.
        let label_width = width * 0.48;
        let gaps = ui.spacing().item_spacing.x * 2.0;
        let control_width = (width - label_width - RESET_WIDTH - gaps).max(60.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(label_width, row_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(label_width);
                    label(ui);
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(control_width, row_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_width(control_width);
                    field_control(ui, field, view, enablement, edits, action);
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(RESET_WIDTH, row_height),
                egui::Layout::right_to_left(egui::Align::Min),
                |ui| {
                    if reset {
                        reset_button(ui, field, edits, action);
                    }
                },
            );
        });
    } else {
        label(ui);
        field_control(ui, field, view, enablement, edits, action);
        if reset {
            reset_button(ui, field, edits, action);
        }
    }
}

/// Width reserved for the leading "Type" column on the unified Plugins page.
const TYPE_WIDTH: f32 = 60.0;

/// Reserve the fixed-width "Type" column slot and, when a type is present,
/// render it dim and left-aligned. Rows without a type leave the cell blank but
/// still reserve its width so labels align down the column.
fn type_cell(ui: &mut egui::Ui, value: Option<&str>) {
    ui.allocate_ui_with_layout(
        egui::vec2(TYPE_WIDTH, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(TYPE_WIDTH);
            if let Some(value) = value {
                ui.label(egui::RichText::new(value).small().weak());
            }
        },
    );
}

/// The dim "Type"/"Name" column header shown above the unified Plugins page's
/// card. The card insets its content by 12px, so the header offsets to match.
fn render_type_header(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        type_cell(ui, Some("Type"));
        ui.label(egui::RichText::new("Name").small().weak());
    });
}

/// The value editor for one row. Enablement rows toggle a tool/command; other
/// rows edit by declared type.
fn field_control(
    ui: &mut egui::Ui,
    field: &SettingDefinition,
    view: &ConfigView,
    enablement: Option<(&str, &str)>,
    edits: &mut HashMap<String, String>,
    action: &mut Option<ConfigUiAction>,
) {
    if let Some((namespace, name)) = enablement {
        let mut enabled = view.is_enabled(namespace, name);
        let text = if enabled { "Enabled" } else { "Disabled" };
        if ui.checkbox(&mut enabled, text).changed() {
            *action = Some(ConfigUiAction::SetEnabled {
                namespace: namespace.into(),
                name: name.into(),
                enabled,
            });
        }
        return;
    }
    match field.value_type.as_str() {
        // A toggle button states the value once (On/Off) and carries the accent
        // fill when on, instead of a checkbox plus a redundant trailing label.
        "bool" => {
            let value = view.value(field).as_bool().unwrap_or(false);
            let text = if value { "On" } else { "Off" };
            if ui
                .add(
                    egui::Button::selectable(value, text)
                        .min_size(egui::vec2(88.0, crate::theme::CONTROL_HEIGHT)),
                )
                .clicked()
            {
                *action = Some(ConfigUiAction::Set {
                    path: field.path.clone(),
                    value: (!value).into(),
                });
            }
        }
        "enum" => {
            let current = view.render_value(field);
            let mut selected = current.clone();
            egui::ComboBox::from_id_salt("value")
                .width(ui.available_width().min(220.0))
                .selected_text(&selected)
                .show_ui(ui, |ui| {
                    for option in &field.options {
                        ui.selectable_value(&mut selected, option.clone(), option);
                    }
                });
            if selected != current {
                *action = Some(ConfigUiAction::Set {
                    path: field.path.clone(),
                    value: selected.into(),
                });
            }
        }
        _ => {
            let current = view.render_value(field);
            let buffer = edits
                .entry(field.path.clone())
                .or_insert_with(|| current.clone());
            let changed = *buffer != current;
            let parsed = parse_config_value(field, buffer.trim());
            let response = if field.path == "general.system_prompt" {
                ui.add(
                    egui::TextEdit::multiline(buffer)
                        .margin(egui::vec2(8.0, 7.0))
                        .desired_rows(5)
                        .desired_width(f32::INFINITY),
                )
            } else {
                ui.add(
                    egui::TextEdit::singleline(buffer)
                        .margin(egui::vec2(8.0, 7.0))
                        .desired_width(f32::INFINITY),
                )
            };
            // Text edits commit on Enter or blur; there is no explicit Save step.
            if response.lost_focus()
                && changed
                && let Ok(value) = parsed
            {
                *action = Some(ConfigUiAction::Set {
                    path: field.path.clone(),
                    value,
                });
            }
            if changed {
                match parse_config_value(field, buffer.trim()) {
                    Err(error) => {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    Ok(_) => {
                        ui.weak("Unsaved change");
                    }
                }
            }
        }
    }
}

/// Trailing reset affordance: a borderless glyph that returns the setting to its
/// schema default. Shown only when the current value differs from the default.
fn reset_button(
    ui: &mut egui::Ui,
    field: &SettingDefinition,
    edits: &mut HashMap<String, String>,
    action: &mut Option<ConfigUiAction>,
) {
    let response = ui
        .add(
            egui::Button::new(egui::RichText::new("↺").size(16.0))
                .frame(false)
                .min_size(egui::vec2(RESET_WIDTH, crate::theme::CONTROL_HEIGHT)),
        )
        .on_hover_text("Reset to default");
    if response.clicked() {
        edits.remove(&field.path);
        *action = Some(ConfigUiAction::Reset {
            path: field.path.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_protocol::{ConfigPage, ConfigSchema, ConfigSnapshot};

    #[test]
    fn refresh_preserves_dirty_fields_and_updates_untouched_buffers() {
        let schema = schema_with(vec![
            field("general.a", "string"),
            field("general.b", "string"),
        ]);
        let view = ConfigView::new(
            Some(schema.clone()),
            Some(snapshot(
                serde_json::json!({"general":{"a":"old", "b":"old"}}),
            )),
        );
        let mut state = SettingsUi::default();
        let mut edits = HashMap::from([
            ("general.a".into(), "old".into()),
            ("general.b".into(), "old".into()),
        ]);
        state.sync(&view, &mut edits);
        edits.insert("general.a".into(), "my draft".into());
        let next = ConfigView::new(
            Some(schema),
            Some(snapshot(
                serde_json::json!({"general":{"a":"remote", "b":"new"}}),
            )),
        );
        state.sync(&next, &mut edits);
        assert_eq!(edits["general.a"], "my draft");
        assert_eq!(edits["general.b"], "new");
        state.sync(&next, &mut edits);
        assert_eq!(edits["general.a"], "my draft");
    }

    #[test]
    fn search_matches_category_and_field_across_words() {
        let mut f = field("extensions.memory.limit", "number");
        f.label = "History limit".into();
        assert!(matches(&f, "Extensions / Memory", "memory history"));
        assert!(!matches(&f, "Extensions / Memory", "memory unknown"));
    }

    fn field(path: &str, value_type: &str) -> SettingDefinition {
        SettingDefinition {
            path: path.into(),
            key: path.rsplit('.').next().unwrap_or(path).into(),
            label: path.into(),
            value_type: value_type.into(),
            options: Vec::new(),
            default: serde_json::json!(null),
            value: None,
            integer: None,
            min: None,
            max: None,
            kind: None,
            reload_behavior: String::new(),
        }
    }

    fn schema_with(fields: Vec<SettingDefinition>) -> ConfigSchema {
        ConfigSchema {
            pages: vec![ConfigPage {
                namespace: "general".into(),
                title: "General".into(),
                fields,
                pages: Vec::new(),
            }],
        }
    }

    fn snapshot(values: serde_json::Value) -> ConfigSnapshot {
        ConfigSnapshot {
            revision: 7,
            values,
            providers: Vec::new(),
            active_provider: String::new(),
            disabled_tools: vec!["shell".into()],
            disabled_commands: vec!["/model".into()],
            disabled_plugins: Vec::new(),
        }
    }

    #[test]
    fn value_prefers_snapshot_then_schema_then_default() {
        let mut f = field("general.approval", "enum");
        f.default = serde_json::json!("safe");
        let view = ConfigView::new(
            Some(schema_with(vec![f.clone()])),
            Some(snapshot(
                serde_json::json!({ "general": { "approval": "danger" } }),
            )),
        );
        assert_eq!(view.revision(), 7);
        assert_eq!(view.render_value(&f), "danger");
        // Nested lookup by dotted path resolves the same definition.
        assert!(view.field("general.approval").is_some());
        assert!(view.field("general.missing").is_none());

        // No snapshot: fall back to the schema value, then the default.
        f.value = Some(serde_json::json!("schema"));
        let view = ConfigView::new(Some(schema_with(vec![f.clone()])), None);
        assert_eq!(view.render_value(&f), "schema");
        f.value = None;
        let view = ConfigView::new(Some(schema_with(vec![f.clone()])), None);
        assert_eq!(view.render_value(&f), "safe");
    }

    #[test]
    fn disabled_lists_come_from_snapshot() {
        let mut snap = snapshot(serde_json::json!({}));
        snap.disabled_plugins = vec!["sample".into()];
        let view = ConfigView::new(None, Some(snap));
        assert_eq!(view.disabled_tools(), ["shell".to_string()]);
        assert_eq!(view.disabled_commands(), ["/model".to_string()]);
        assert_eq!(view.disabled_plugins(), ["sample".to_string()]);
        assert!(ConfigView::default().disabled_tools().is_empty());
        assert!(ConfigView::default().disabled_plugins().is_empty());
    }

    #[test]
    fn is_enabled_reads_disabled_lists() {
        let mut snap = snapshot(serde_json::json!({}));
        snap.disabled_plugins = vec!["sample".into()];
        let view = ConfigView::new(None, Some(snap));
        assert!(!view.is_enabled("tools", "shell"));
        assert!(view.is_enabled("tools", "read_file"));
        assert!(!view.is_enabled("commands", "/model"));
        assert!(view.is_enabled("commands", "/help"));
        assert!(!view.is_enabled("plugins", "sample"));
        assert!(view.is_enabled("plugins", "other"));
        // Unknown namespace and a missing snapshot default to enabled.
        assert!(view.is_enabled("other", "x"));
        assert!(ConfigView::default().is_enabled("tools", "shell"));
    }

    #[test]
    fn parse_config_value_validates_each_type() {
        let boolean = field("general.x", "bool");
        assert_eq!(
            parse_config_value(&boolean, "on").unwrap(),
            serde_json::json!(true)
        );
        assert_eq!(
            parse_config_value(&boolean, "off").unwrap(),
            serde_json::json!(false)
        );
        assert!(parse_config_value(&boolean, "maybe").is_err());
        let mut integer = field("general.n", "number");
        integer.integer = Some(true);
        integer.min = Some(1.0);
        integer.max = Some(10.0);
        assert_eq!(
            parse_config_value(&integer, "5").unwrap(),
            serde_json::json!(5)
        );
        assert!(parse_config_value(&integer, "0").is_err());
        assert!(parse_config_value(&integer, "11").is_err());
        assert!(parse_config_value(&integer, "2.5").is_err());

        let float = field("general.f", "number");
        assert_eq!(
            parse_config_value(&float, "2.5").unwrap(),
            serde_json::json!(2.5)
        );
        assert!(parse_config_value(&float, "nan").is_err());

        let mut enumerated = field("general.e", "enum");
        enumerated.options = vec!["safe".into(), "danger".into()];
        assert_eq!(
            parse_config_value(&enumerated, "danger").unwrap(),
            serde_json::json!("danger")
        );
        assert!(parse_config_value(&enumerated, "other").is_err());

        let text = field("general.s", "string");
        assert_eq!(
            parse_config_value(&text, "hello").unwrap(),
            serde_json::json!("hello")
        );

        let unknown = field("general.u", "widget");
        assert!(parse_config_value(&unknown, "x").is_err());
    }
}
