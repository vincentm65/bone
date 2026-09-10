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

    /// Whether a tool/command is enabled. Enablement is tracked separately from
    /// `values` in the snapshot's disabled lists, so `tools.<name>` /
    /// `commands.<name>` schema fields cannot read it from [`Self::value`].
    pub fn is_enabled(&self, namespace: &str, name: &str) -> bool {
        match namespace {
            "tools" => !self.disabled_tools().iter().any(|entry| entry == name),
            "commands" => !self.disabled_commands().iter().any(|entry| entry == name),
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
    ui.add(
        egui::TextEdit::singleline(&mut state.query)
            .hint_text("Search settings…")
            .margin(egui::vec2(12.0, 9.0))
            .desired_width(f32::INFINITY),
    );
    ui.add_space(6.0);
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
                            ui.weak("PREFERENCES");
                            for page in &schema.pages {
                                if ui
                                    .add_sized(
                                        [164.0, 36.0],
                                        egui::Button::selectable(
                                            query.is_empty() && state.selected == page.namespace,
                                            &page.title,
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
    if !fields.is_empty() {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(&title).size(18.0).strong());
        ui.add_space(6.0);
        for field in fields {
            ui.push_id(&field.path, |ui| {
                render_field(ui, field, view, edits, action);
            });
            ui.separator();
        }
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

fn render_field(
    ui: &mut egui::Ui,
    field: &SettingDefinition,
    view: &ConfigView,
    edits: &mut HashMap<String, String>,
    action: &mut Option<ConfigUiAction>,
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
        });
    let label = |ui: &mut egui::Ui| {
        ui.label(egui::RichText::new(&field.label).strong())
            .on_hover_text(&field.path);
        let hint = field_hint(field);
        if !hint.is_empty() {
            ui.weak(hint);
        }
    };
    let mut control = |ui: &mut egui::Ui| {
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
        } else {
            match field.value_type.as_str() {
                "bool" => {
                    let mut value = view.value(field).as_bool().unwrap_or(false);
                    let text = if value { "On" } else { "Off" };
                    if ui.checkbox(&mut value, text).changed() {
                        *action = Some(ConfigUiAction::Set {
                            path: field.path.clone(),
                            value: value.into(),
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
                    ui.horizontal(|ui| {
                        let response = ui.add(
                            egui::TextEdit::singleline(buffer)
                                .margin(egui::vec2(8.0, 7.0))
                                .desired_width((ui.available_width() - 72.0).max(60.0)),
                        );
                        let enter =
                            response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (ui
                            .add_enabled(changed && parsed.is_ok(), egui::Button::new("Save"))
                            .clicked()
                            || (enter && changed))
                            && let Ok(value) = parse_config_value(field, buffer.trim())
                        {
                            *action = Some(ConfigUiAction::Set {
                                path: field.path.clone(),
                                value,
                            });
                        }
                    });
                    if *buffer != current {
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
            if view.value(field) != field.default && ui.small_button("Reset to default").clicked() {
                edits.remove(&field.path);
                *action = Some(ConfigUiAction::Reset {
                    path: field.path.clone(),
                });
            }
        }
    };
    ui.add_space(6.0);
    if ui.available_width() >= 460.0 {
        let width = ui.available_width();
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(width * 0.46, 36.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(width * 0.46);
                    label(ui);
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 36.0),
                egui::Layout::top_down(egui::Align::Min),
                &mut control,
            );
        });
    } else {
        label(ui);
        control(ui);
    }
    ui.add_space(6.0);
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
        let view = ConfigView::new(None, Some(snapshot(serde_json::json!({}))));
        assert_eq!(view.disabled_tools(), ["shell".to_string()]);
        assert_eq!(view.disabled_commands(), ["/model".to_string()]);
        assert!(ConfigView::default().disabled_tools().is_empty());
    }

    #[test]
    fn is_enabled_reads_disabled_lists() {
        let view = ConfigView::new(None, Some(snapshot(serde_json::json!({}))));
        assert!(!view.is_enabled("tools", "shell"));
        assert!(view.is_enabled("tools", "read_file"));
        assert!(!view.is_enabled("commands", "/model"));
        assert!(view.is_enabled("commands", "/help"));
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
