//! The `/config` (and `/provider`, `/tools`) settings UI: a tabbed prompt for
//! editing general/provider/tool config pages, plus the per-provider editor and
//! the inline value-entry prompt they share.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use super::super::input::InputAction;
use super::super::prompt::Prompt;
use super::apply_input_key_with_paste_burst;
use super::{App, BoneTerminal, config, providers};

/// Provider ids from a [`ProvidersConfig`], sorted for stable display order.
fn sorted_provider_ids(cfg: &crate::config::ProvidersConfig) -> Vec<String> {
    let mut ids: Vec<String> = cfg.providers.keys().cloned().collect();
    ids.sort();
    ids
}

impl App {
    fn panel_key(&mut self, term: &mut BoneTerminal) -> io::Result<(KeyCode, KeyModifiers)> {
        loop {
            if event::poll(std::time::Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        return Ok((key.code, key.modifiers));
                    }
                    Event::Resize(_, _) => self.force_redraw(term)?,
                    _ => {}
                }
            }
        }
    }

    fn close_panel(&mut self, term: &mut BoneTerminal) -> io::Result<()> {
        self.active_prompt = None;
        self.redraw(term)
    }

    fn mask_secret(value: &str) -> String {
        if value.is_empty() {
            "(empty)".to_string()
        } else {
            "*".repeat(value.chars().count().clamp(4, 12))
        }
    }

    fn edit_value(
        &mut self,
        label: &str,
        initial: &str,
        secret: bool,
        term: &mut BoneTerminal,
    ) -> io::Result<Option<String>> {
        self.input.buffer = if secret {
            String::new()
        } else {
            initial.to_string()
        };
        self.input.cursor_pos = self.input.buffer.chars().count();
        loop {
            let value = if secret {
                Self::mask_secret(&self.input.buffer)
            } else {
                self.input.buffer.clone()
            };
            let mut prompt =
                Prompt::new(format!("Edit {label}"), vec![format!("{label}: {value}")]);
            prompt.hint = Some("Enter save value  Esc cancel".to_string());
            self.active_prompt = Some(prompt);
            self.redraw(term)?;
            let mut next = Some(event::read()?);
            while let Some(event) = next {
                next = None;
                match event {
                    Event::Paste(text) => self.input.insert_paste(&text),
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if key.code == KeyCode::Esc
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL))
                        {
                            self.input.clear_buffer();
                            return Ok(None);
                        }
                        let result = apply_input_key_with_paste_burst(&mut self.input, key)?;
                        next = result.trailing;
                        match result.action {
                            InputAction::Submit => {
                                let value = self.input.expanded();
                                self.input.clear_buffer();
                                return Ok(Some(value));
                            }
                            InputAction::None if key.code == KeyCode::Enter => {
                                let value = self.input.expanded();
                                self.input.clear_buffer();
                                return Ok(Some(value));
                            }
                            InputAction::Cancel | InputAction::Escape => {
                                self.input.clear_buffer();
                                return Ok(None);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn provider_editor(&mut self, id: String, term: &mut BoneTerminal) -> io::Result<()> {
        let entry = self
            .custom_configs
            .get_provider_entry("providers", &id)
            .ok_or_else(|| io::Error::other(format!("unknown provider `{id}`")))?;
        let mut entry = entry;
        let mut selected = 0usize;
        loop {
            let options = vec![
                format!("label · {}", entry.label),
                format!("model · {}", entry.model),
                format!("base_url · {}", entry.base_url),
                format!("endpoint · {}", entry.endpoint),
                format!("handler · {}", entry.handler),
                format!("api_key · {}", Self::mask_secret(&entry.api_key)),
                "Save changes".to_string(),
            ];
            let mut prompt = Prompt::new(format!("Edit provider: {id}"), options);
            prompt.set_selected(selected);
            prompt.hint = Some("Enter edit/select  Esc back".to_string());
            self.active_prompt = Some(prompt);
            self.redraw(term)?;
            let (code, modifiers) = self.panel_key(term)?;
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(());
            }
            if self.navigate_prompt(code, false, term)? {
                selected = self.active_prompt.as_ref().unwrap().selected;
                continue;
            }
            match code {
                KeyCode::Esc => return Ok(()),
                KeyCode::Enter => {
                    let selected = self.active_prompt.as_ref().unwrap().selected;
                    match selected {
                        0 => {
                            if let Some(value) =
                                self.edit_value("label", &entry.label, false, term)?
                            {
                                entry.label = value;
                            }
                        }
                        1 => {
                            if let Some(value) =
                                self.edit_value("model", &entry.model, false, term)?
                            {
                                entry.model = value;
                            }
                        }
                        2 => {
                            if let Some(value) =
                                self.edit_value("base_url", &entry.base_url, false, term)?
                            {
                                entry.base_url = value;
                            }
                        }
                        3 => {
                            if let Some(value) =
                                self.edit_value("endpoint", &entry.endpoint, false, term)?
                            {
                                entry.endpoint = value;
                            }
                        }
                        4 => {
                            entry.handler = if entry.handler == "codex" {
                                "openai".to_string()
                            } else {
                                "codex".to_string()
                            };
                        }
                        5 => {
                            if let Some(value) = self.edit_value("api_key", "", true, term)? {
                                entry.api_key = value;
                            }
                        }
                        6 => {
                            self.custom_configs
                                .set_provider_entry("providers", &id, &entry);
                            let reply = if self.llm.id() == id {
                                match providers::create_provider_with_config(
                                    &id,
                                    &self.custom_configs.derive_providers_config(),
                                ) {
                                    Ok(provider) => {
                                        self.provider =
                                            format!("{} ({})", provider.name(), provider.id());
                                        self.model = provider.model().to_string();
                                        self.llm = std::sync::Arc::from(provider);
                                        format!("Saved and reloaded provider {id}.")
                                    }
                                    Err(err) => format!(
                                        "Saved provider {id}, but active reload failed: {err}"
                                    ),
                                }
                            } else {
                                format!("Saved provider {id}.")
                            };
                            self.show_reply(reply, term)?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    pub(super) async fn handle_tools_command(
        &mut self,
        arg: &str,
        term: &mut BoneTerminal,
    ) -> io::Result<()> {
        let mut parts = arg.split_whitespace();
        let action = parts.next().unwrap_or("");
        match action {
            "reload" => {
                // Full reload: re-boot extensions and rebuild tool handler.
                let config_dir = crate::config::bone_dir();
                let cwd = std::env::current_dir().unwrap_or_default();
                let booted = crate::ext::boot_with_tools(
                    &config_dir,
                    &cwd,
                    &mut self.custom_configs,
                    true,
                    crate::ext::BootOptions {
                        agent_depth: 0,
                        headless: false,
                        model: self.model.clone(),
                        provider: self.provider.clone(),
                    },
                    &self.model,
                    &self.provider,
                );
                self.extensions = booted.manager;
                self.tools = booted.tools;

                self.user_config.enabled_tools = self.tools.enabled_names();
                let count = self.tools.definitions().len();
                self.show_reply(
                    format!("Tools and Lua extensions reloaded. {count} tools enabled."),
                    term,
                )
            }
            _ => self.config_picker(term, Some("tools")).await,
        }
    }

    pub(super) async fn config_picker(
        &mut self,
        term: &mut BoneTerminal,
        start_tab: Option<&str>,
    ) -> io::Result<()> {
        let mut custom = config::custom::CustomConfigs::load();

        let mut tabs: Vec<String> = Vec::new();
        let mut namespaces: Vec<String> = Vec::new();
        for ns in ["general", "providers", "tools"] {
            if let Some((_, page)) = custom.pages.iter().find(|(page_ns, _)| page_ns == ns) {
                tabs.push(page.title.clone());
                namespaces.push(ns.to_string());
            }
        }
        for (ns, page) in &custom.pages {
            if namespaces.iter().any(|existing| existing == ns) {
                continue;
            }
            tabs.push(page.title.clone());
            namespaces.push(ns.clone());
        }
        let providers_tab_idx = namespaces
            .iter()
            .position(|ns| ns == "providers")
            .unwrap_or(0);
        let num_tabs = tabs.len();

        let mut active = if let Some(tab) = start_tab {
            if tab == "providers" {
                providers_tab_idx
            } else {
                namespaces.iter().position(|ns| ns == tab).unwrap_or(0)
            }
        } else {
            0
        };
        let mut selected = 0usize;

        loop {
            let options = if active == providers_tab_idx {
                // Providers tab: list providers like the old provider_picker
                let providers_config = custom.derive_providers_config();
                let ids = sorted_provider_ids(&providers_config);
                ids.iter()
                    .map(|id| {
                        let entry = &providers_config.providers[id];
                        let active_marker = if id == self.llm.id() { "●" } else { "○" };
                        let kind = if entry.handler.is_empty() {
                            "openai"
                        } else {
                            entry.handler.as_str()
                        };
                        format!(
                            "{active_marker} {id} · {} · {} · {kind}",
                            entry.model, entry.label
                        )
                    })
                    .collect()
            } else if active < namespaces.len() {
                let ns = &namespaces[active];
                let page_idx = custom
                    .pages
                    .iter()
                    .position(|(page_ns, _)| page_ns == ns)
                    .unwrap();
                let page = &custom.pages[page_idx].1;
                page.fields
                    .iter()
                    .map(|field| {
                        let label = field.label.as_deref().unwrap_or(&field.key);
                        let value = custom.get_value(ns, &field.key);
                        let display = match field.field_type {
                            config::custom::ConfigFieldType::Bool => {
                                if value == "true" {
                                    "●".to_string()
                                } else {
                                    "○".to_string()
                                }
                            }
                            _ => value,
                        };
                        if matches!(field.field_type, config::custom::ConfigFieldType::Bool) {
                            format!("{display} {label}")
                        } else {
                            format!("{:<30} {}", label, display)
                        }
                    })
                    .collect()
            } else {
                vec![]
            };

            let mut prompt = Prompt::new(tabs[active].clone(), options);
            prompt.set_selected(selected);
            prompt.tabs = tabs.clone();
            prompt.active_tab = active;
            let hint = if active == providers_tab_idx {
                "Enter select  e edit  Esc close".to_string()
            } else {
                "Tab switch  Enter edit/cycle  Esc close".to_string()
            };
            prompt.hint = Some(hint);
            self.active_prompt = Some(prompt);
            self.redraw(term)?;

            let (code, modifiers) = self.panel_key(term)?;
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                return self.close_panel(term);
            }
            if self.navigate_prompt(code, false, term)? {
                selected = self.active_prompt.as_ref().unwrap().selected;
                continue;
            }

            match code {
                KeyCode::Esc => return self.close_panel(term),
                KeyCode::Tab => {
                    active = (active + 1) % num_tabs;
                    selected = 0;
                    continue;
                }
                KeyCode::BackTab => {
                    active = if active == 0 {
                        num_tabs - 1
                    } else {
                        active - 1
                    };
                    selected = 0;
                    continue;
                }
                KeyCode::Enter => {
                    if active == providers_tab_idx {
                        // Providers tab: select provider
                        let providers_config = custom.derive_providers_config();
                        let ids = sorted_provider_ids(&providers_config);
                        let Some(id) = ids.get(self.active_prompt.as_ref().unwrap().selected)
                        else {
                            continue;
                        };
                        let id = id.clone();
                        let reply =
                            match providers::create_provider_with_config(&id, &providers_config) {
                                Ok(new_provider) => match new_provider.validate().await {
                                    Ok(()) => {
                                        self.provider = format!(
                                            "{} ({})",
                                            new_provider.name(),
                                            new_provider.id()
                                        );
                                        self.model = new_provider.model().to_string();
                                        self.llm = std::sync::Arc::from(new_provider);
                                        custom.set_last_provider(&id);
                                        self.custom_configs = custom.clone();
                                        format!("Switched to {} ({})", self.model, self.provider)
                                    }
                                    Err(err) => format!("Provider validation failed: {err}"),
                                },
                                Err(err) => err.to_string(),
                            };
                        self.close_panel(term)?;
                        return self.show_reply(reply, term);
                    }
                    if active >= namespaces.len() {
                        continue;
                    }
                    let ns = namespaces[active].clone();
                    let page_idx = custom
                        .pages
                        .iter()
                        .position(|(page_ns, _)| page_ns == &ns)
                        .unwrap();
                    let page = &custom.pages[page_idx].1;
                    let idx = self.active_prompt.as_ref().unwrap().selected;

                    if idx >= page.fields.len() {
                        continue;
                    }
                    let field = page.fields[idx].clone();
                    let current = custom.get_value(&ns, &field.key);
                    match field.field_type {
                        config::custom::ConfigFieldType::Bool
                        | config::custom::ConfigFieldType::Enum => {
                            if let Some(next) = custom.cycle_field(&ns, &field.key, &current) {
                                custom.set_value(&ns, &field.key, next.clone());
                                self.apply_custom_configs_to_runtime(custom.clone());
                                if ns == "tools" {
                                    self.tools.set_enabled(&field.key, next == "true");
                                }
                            }
                        }
                        _ => {
                            let label = field.label.as_deref().unwrap_or(&field.key).to_string();
                            if let Some(val) = self.edit_value(&label, &current, false, term)? {
                                custom.set_value(&ns, &field.key, val.trim().to_string());
                                self.apply_custom_configs_to_runtime(custom.clone());
                            }
                        }
                    }
                }
                KeyCode::Char('e') | KeyCode::Char('E') if active == providers_tab_idx => {
                    let providers_config = custom.derive_providers_config();
                    let ids = sorted_provider_ids(&providers_config);
                    if let Some(id) = ids.get(self.active_prompt.as_ref().unwrap().selected) {
                        self.provider_editor(id.clone(), term)?;
                        custom = config::custom::CustomConfigs::load();
                    }
                }
                _ => {}
            }
        }
    }
}
