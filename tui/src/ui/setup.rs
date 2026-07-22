//! First-launch onboarding wizard.
//!
//! A fullscreen, `/stats`-style takeover (see `crate::ui::stats`) that walks a
//! new user through: picking a provider + API key (skippable), choosing optional
//! tools/commands from the catalog (auto-downloaded), and whether `init.lua`
//! is auto-populated or blank. The populated choice stores its starter agent in
//! canonical `subagents.yaml`. Choices are persisted via
//! `config::apply_onboarding`, which doubles as the "already onboarded" marker.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::config::{self, InitChoice, SetupSelection};
use crate::ext::catalog::{self, CatalogEntry};
use crate::ui::catalog as catalog_ui;
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::picker::{self, ACCENT, BG, BORDER, DIM, GOOD, Item, MUTED, TEXT};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Provider,
    Catalog,
    Init,
    Confirm,
}

const STEP_COUNT: usize = 5;

struct State {
    step: Step,
    /// Daemon configuration authority used during pre-runtime onboarding.
    config: config::store::ConfigStore,
    /// Available providers as `(id, label)`.
    providers: Vec<(String, String)>,
    provider_cursor: usize,
    /// In-progress API key text for the focused provider.
    api_key: String,
    /// Provider id whose key we saved, for the confirm summary.
    provider_saved: Option<String>,
    /// Catalog entries and the matching checklist rows.
    cat_entries: Vec<CatalogEntry>,
    cat_items: Vec<Item>,
    cat_cursor: usize,
    init_options: Vec<(&'static str, &'static str, InitChoice)>,
    init_cursor: usize,
    completed: bool,
    /// True on a genuine first-launch onboarding; only affects skip/cancel copy.
    fresh: bool,
}

impl State {
    fn new(fresh: bool) -> Result<Self, String> {
        let config = config::store::ConfigStore::new(crate::ext::ExtensionManager::unloaded())?;
        let mut providers: Vec<(String, String)> = config
            .providers_config()
            .providers
            .into_iter()
            .map(|(id, entry)| {
                let label = if entry.label.is_empty() {
                    id.clone()
                } else {
                    entry.label
                };
                (id, label)
            })
            .collect();
        providers.sort_by(|a, b| a.0.cmp(&b.0));

        // Fetch the catalog index (blocking, cached fallback) so the picker is
        // populated; offline simply yields an empty list.
        let cat_entries = catalog::sync_quiet();
        let cat_items = catalog_ui::build_items(&cat_entries);

        let init_exists = config::setup_selection_path()
            .parent()
            .map(|d| d.join("init.lua").exists())
            .unwrap_or(false);
        let mut init_options = vec![
            (
                "Auto-populated",
                "Banner wiring plus a researcher in subagents.yaml, ready to dispatch.",
                InitChoice::Populated,
            ),
            (
                "Blank",
                "A minimal placeholder you fill in yourself.",
                InitChoice::Blank,
            ),
        ];
        if init_exists {
            init_options.push((
                "Keep current",
                "Leave my existing init.lua untouched.",
                InitChoice::Keep,
            ));
        }

        Ok(Self {
            step: Step::Welcome,
            config,
            providers,
            provider_cursor: 0,
            api_key: String::new(),
            provider_saved: None,
            cat_entries,
            cat_items,
            cat_cursor: 0,
            init_options,
            init_cursor: 0,
            completed: false,
            fresh,
        })
    }

    fn init_choice(&self) -> InitChoice {
        self.init_options[self.init_cursor].2
    }

    /// Onboarding seeds no Lua tools (all optional ones live in the catalog)
    /// and all bundled core commands. The selection file doubles as the
    /// onboarding-complete marker.
    fn selection(&self) -> SetupSelection {
        let commands = crate::ext::default_command_catalog()
            .into_iter()
            .map(|(name, _)| name.to_string())
            .collect();
        SetupSelection {
            tools: Vec::new(),
            commands,
        }
    }

    fn next_step(&mut self) {
        self.step = match self.step {
            Step::Welcome => Step::Provider,
            Step::Provider => Step::Catalog,
            Step::Catalog => Step::Init,
            Step::Init => Step::Confirm,
            Step::Confirm => Step::Confirm,
        };
    }

    fn prev_step(&mut self) {
        self.step = match self.step {
            Step::Welcome => Step::Welcome,
            Step::Provider => Step::Welcome,
            Step::Catalog => Step::Provider,
            Step::Init => Step::Catalog,
            Step::Confirm => Step::Init,
        };
    }

    /// Persist the focused provider with the typed key. No-op if the key is
    /// blank (the step is skippable) or the provider entry can't be found.
    fn save_provider(&mut self) {
        let key = self.api_key.trim();
        if key.is_empty() {
            return;
        }
        let Some((id, _)) = self.providers.get(self.provider_cursor).cloned() else {
            return;
        };
        let config = self.config.providers_config();
        if let Some(entry) = config.providers.get(&id) {
            let revision = self.config.snapshot().revision;
            let update = bone_protocol::ProviderUpdate {
                id: id.clone(),
                label: entry.label.clone(),
                base_url: entry.base_url.clone(),
                model: entry.model.clone(),
                endpoint: entry.endpoint.clone(),
                handler: entry.handler.clone(),
                context_window_tokens: entry.context_window_tokens,
                reasoning_effort: entry.reasoning_effort.clone(),
                api_key: Some(key.to_string()),
            };
            if self.config.upsert_provider(update, revision).is_ok()
                && self
                    .config
                    .set_active_provider(&id, revision.saturating_add(1))
                    .is_ok()
            {
                self.provider_saved = Some(id);
            }
        }
    }
}

/// Run the onboarding wizard fullscreen. Returns `Ok(true)` if the user
/// completed it (choices applied), `Ok(false)` if they cancelled.
pub fn run(fresh: bool) -> io::Result<bool> {
    fullscreen::run(|term| run_loop(term, fresh))
}

fn run_loop(term: &mut FullscreenTerminal, fresh: bool) -> io::Result<bool> {
    let mut state = State::new(fresh).map_err(io::Error::other)?;
    term.draw(|frame| draw(frame, &state))?;

    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(false),
                KeyCode::Up => move_cursor(&mut state, -1),
                KeyCode::Down => move_cursor(&mut state, 1),
                KeyCode::Left => state.prev_step(),
                KeyCode::Right => advance(&mut state),
                KeyCode::Enter => match state.step {
                    Step::Confirm => {
                        apply(&mut state)?;
                        return Ok(state.completed);
                    }
                    _ => advance(&mut state),
                },
                KeyCode::Backspace if state.step == Step::Provider => {
                    state.api_key.pop();
                }
                KeyCode::Char(c) => handle_char(&mut state, c),
                _ => {}
            },
            Event::Resize(_, _) => {}
            _ => {}
        }
        term.draw(|frame| draw(frame, &state))?;
    }
}

/// Character keys are step-sensitive: the Provider step captures them as API-key
/// text; other steps use them as shortcuts (vim nav, toggle, all/none).
fn handle_char(state: &mut State, c: char) {
    match state.step {
        Step::Provider => state.api_key.push(c),
        Step::Catalog => match c {
            ' ' => toggle_catalog(state),
            'a' => set_all_catalog(state, true),
            'n' => set_all_catalog(state, false),
            'k' => move_cursor(state, -1),
            'j' => move_cursor(state, 1),
            _ => {}
        },
        _ => match c {
            'k' => move_cursor(state, -1),
            'j' => move_cursor(state, 1),
            _ => {}
        },
    }
}

fn move_cursor(state: &mut State, delta: i32) {
    let (cursor, len) = match state.step {
        Step::Provider => (&mut state.provider_cursor, state.providers.len()),
        Step::Catalog => (&mut state.cat_cursor, state.cat_items.len()),
        Step::Init => (&mut state.init_cursor, state.init_options.len()),
        _ => return,
    };
    if len == 0 {
        return;
    }
    *cursor = ((*cursor as i32 + delta).rem_euclid(len as i32)) as usize;
}

fn toggle_catalog(state: &mut State) {
    if let Some(item) = state.cat_items.get_mut(state.cat_cursor) {
        item.checked = !item.checked;
        item.user_touched = true;
    }
}

fn set_all_catalog(state: &mut State, checked: bool) {
    for item in state.cat_items.iter_mut() {
        item.checked = checked;
        item.user_touched = true;
    }
}

fn activate_provider(config: &config::store::ConfigStore, id: &str) -> bool {
    let revision = config.snapshot().revision;
    config.set_active_provider(id, revision).is_ok()
}

fn advance(state: &mut State) {
    // Persist the provider choice as we leave the Provider step.
    if state.step == Step::Provider {
        state.save_provider();
        // Even without an API key, set the selected provider as active so
        // `last_provider` points to an existing entry and Bone can launch
        // without falling back to the undefined "local" default.
        if state.provider_saved.is_none() && !state.providers.is_empty() {
            if let Some((id, _)) = state.providers.get(state.provider_cursor).cloned()
                && activate_provider(&state.config, &id)
            {
                state.provider_saved = Some(id);
            }
        }
    }
    state.next_step();
}

fn apply(state: &mut State) -> io::Result<()> {
    config::apply_onboarding(&state.selection(), state.init_choice())?;
    // Install the catalog picks (best-effort; failures don't abort onboarding).
    let _ = catalog_ui::apply(&state.cat_entries, &state.cat_items, false);
    state.completed = true;
    Ok(())
}

// ---- rendering ----------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, state: &State) {
    let screen = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), screen);

    let width = screen.width.min(90);
    let area = Rect {
        x: screen.x + (screen.width.saturating_sub(width)) / 2,
        y: screen.y,
        width,
        height: screen.height,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // header
            Constraint::Min(10),   // body
            Constraint::Length(2), // footer
        ])
        .split(area);

    draw_header(frame, chunks[0], state);
    draw_body(frame, chunks[1], state);
    draw_footer(frame, chunks[2], state);
}

fn draw_header(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    let step_n = match state.step {
        Step::Welcome => 1,
        Step::Provider => 2,
        Step::Catalog => 3,
        Step::Init => 4,
        Step::Confirm => 5,
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("Welcome to ", Style::default().fg(MUTED)),
            Span::styled(
                "bone",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            format!("Setup · step {step_n} of {STEP_COUNT}"),
            Style::default().fg(DIM),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(BORDER))
                .padding(ratatui::widgets::Padding::new(2, 0, 1, 0)),
        ),
        area,
    );
}

fn draw_body(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    match state.step {
        Step::Welcome => draw_welcome(frame, area),
        Step::Provider => draw_provider(frame, area, state),
        Step::Catalog => {
            if state.cat_items.is_empty() {
                let lines = vec![
                    Line::from(Span::styled(
                        "Catalog unavailable.",
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "bone couldn't reach the catalog (you may be offline). \
                         Skip for now and add tools later with /catalog.",
                        Style::default().fg(MUTED),
                    )),
                ];
                frame.render_widget(
                    Paragraph::new(lines).wrap(Wrap { trim: false }),
                    picker::pad(area),
                );
            } else {
                picker::draw_list(
                    frame,
                    area,
                    "Pick optional tools & commands",
                    "They download once selected. Toggle with Space; → to continue.",
                    &state.cat_items,
                    state.cat_cursor,
                );
            }
        }
        Step::Init => draw_init(frame, area, state),
        Step::Confirm => draw_confirm(frame, area, state),
    }
}

const LOGO: [&str; 3] = [
    "┏┓ ┏━┓┏┓╻┏━╸   ┏━┓┏━╸┏━╸┏┓╻╺┳╸",
    "┣┻┓┃ ┃┃┗┫┣╸    ┣━┫┃╺┓┣╸ ┃┗┫ ┃ ",
    "┗━┛┗━┛╹ ╹┗━╸   ╹ ╹┗━┛┗━╸╹ ╹ ╹ ",
];

fn draw_welcome(frame: &mut ratatui::Frame, area: Rect) {
    let mut lines = vec![];
    for row in LOGO {
        lines.push(Line::from(Span::styled(
            row,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    lines.extend(vec![
        Line::from(Span::styled(
            "bone is yours to shape.",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "This quick setup seeds your {} config. You'll set:",
                config::bone_dir().display()
            ),
            Style::default().fg(MUTED),
        )),
        Line::from(""),
        bullet("Provider", "Pick one and drop in an API key (optional)."),
        bullet(
            "Catalog",
            "Optional tools & commands, downloaded on demand.",
        ),
        bullet("init.lua", "Startup script — banner and advanced hooks."),
        Line::from(""),
        Line::from(Span::styled(
            "Everything is editable later — just ask bone, or run /setup again.",
            Style::default().fg(DIM),
        )),
    ]);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        picker::pad(area),
    );
}

fn draw_provider(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    let area = picker::pad(area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // hint
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // provider list
            Constraint::Length(2), // key field
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Pick a provider and add a key to get started",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑/↓ choose · type your API key · → to continue (or skip).",
            Style::default().fg(DIM),
        ))),
        rows[1],
    );

    if state.providers.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No providers configured. Add one with /config later.",
                Style::default().fg(MUTED),
            )))
            .wrap(Wrap { trim: false }),
            rows[3],
        );
        return;
    }

    let (start, end) = picker::visible_window(
        state.providers.len(),
        state.provider_cursor,
        rows[3].height as usize,
    );
    let mut list_lines = Vec::with_capacity(end - start);
    for (i, (id, label)) in state.providers.iter().enumerate().take(end).skip(start) {
        let selected = i == state.provider_cursor;
        let marker = if selected { " ● " } else { " ○ " };
        list_lines.push(Line::from(vec![
            Span::styled(
                marker,
                Style::default().fg(if selected { GOOD } else { DIM }),
            ),
            Span::styled(
                label.clone(),
                if selected {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(TEXT)
                },
            ),
            Span::styled(format!("  ({id})"), Style::default().fg(DIM)),
        ]));
    }
    frame.render_widget(Paragraph::new(list_lines), rows[3]);

    // Masked key field.
    let masked = "•".repeat(state.api_key.chars().count());
    let key_line = Line::from(vec![
        Span::styled("API key  ", Style::default().fg(MUTED)),
        Span::styled(
            if masked.is_empty() {
                "(leave blank to skip)".to_string()
            } else {
                masked
            },
            Style::default().fg(if state.api_key.is_empty() { DIM } else { TEXT }),
        ),
    ]);
    frame.render_widget(Paragraph::new(key_line), rows[4]);
}

fn bullet(head: &str, rest: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("  • ", Style::default().fg(ACCENT)),
        Span::styled(
            format!("{head}  "),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(rest.to_string(), Style::default().fg(MUTED)),
    ])
}

fn draw_init(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    let area = picker::pad(area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "How should your init.lua start?",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "init.lua runs once at launch. Pick with ↑/↓, confirm with →.",
            Style::default().fg(DIM),
        ))),
        rows[1],
    );

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 3), Constraint::Ratio(2, 3)])
        .split(rows[3]);

    let mut list_lines = Vec::with_capacity(state.init_options.len());
    for (i, (label, _, _)) in state.init_options.iter().enumerate() {
        let selected = i == state.init_cursor;
        let marker = if selected { " ● " } else { " ○ " };
        list_lines.push(Line::from(vec![
            Span::styled(
                marker,
                Style::default().fg(if selected { GOOD } else { DIM }),
            ),
            Span::styled(
                label.to_string(),
                if selected {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(TEXT)
                },
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(list_lines), cols[0]);

    let detail_lines = if let Some((label, desc, _)) = state.init_options.get(state.init_cursor) {
        vec![
            Line::from(Span::styled(
                label.to_string(),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(desc.to_string(), Style::default().fg(MUTED))),
        ]
    } else {
        Vec::new()
    };
    frame.render_widget(
        Paragraph::new(detail_lines)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::LEFT)
                    .border_style(Style::default().fg(BORDER))
                    .padding(ratatui::widgets::Padding::horizontal(2)),
            ),
        cols[1],
    );
}

fn draw_confirm(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    let n_cat = state.cat_items.iter().filter(|i| i.checked).count();
    let init_label = state.init_options[state.init_cursor].0;
    let provider = state
        .provider_saved
        .clone()
        .unwrap_or_else(|| "skipped".to_string());

    let lines = vec![
        Line::from(Span::styled(
            "Ready to set up bone.",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        summary("Provider", provider),
        summary("Catalog", format!("{n_cat} selected")),
        summary("init.lua", init_label.to_string()),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "Press Enter to write these into {}.",
                config::bone_dir().display()
            ),
            Style::default().fg(GOOD),
        )),
        Line::from(Span::styled(
            if state.fresh {
                "← to go back, Esc to skip (seeds defaults)."
            } else {
                "← to go back, Esc to cancel (leaves config unchanged)."
            },
            Style::default().fg(DIM),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        picker::pad(area),
    );
}

fn summary(head: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {head:<10} "), Style::default().fg(MUTED)),
        Span::styled(
            value,
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    let cancel_label = if state.fresh { "skip" } else { "cancel" };
    let keys: &[(&str, &str)] = match state.step {
        Step::Welcome => &[("→/enter", "start"), ("esc", cancel_label)],
        Step::Provider => &[
            ("↑↓", "choose"),
            ("type", "key"),
            ("→", "next"),
            ("←", "back"),
            ("esc", cancel_label),
        ],
        Step::Catalog => &[
            ("↑↓", "move"),
            ("space", "toggle"),
            ("a/n", "all/none"),
            ("→", "next"),
            ("←", "back"),
        ],
        Step::Init => &[("↑↓", "choose"), ("→", "next"), ("←", "back")],
        Step::Confirm => &[("enter", "apply"), ("←", "back"), ("esc", cancel_label)],
    };
    picker::draw_footer(frame, area, keys);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_provider_can_be_activated_without_api_key() {
        let _guard = crate::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var_os("BONE_DIR");
        let root = std::env::temp_dir().join(format!(
            "bone-setup-provider-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        unsafe { std::env::set_var("BONE_DIR", &root) };

        let store = config::store::ConfigStore::new(crate::ext::ExtensionManager::unloaded())
            .expect("seed fresh configuration");
        assert!(
            store.providers_config().providers["local"]
                .api_key
                .is_empty()
        );
        assert!(activate_provider(&store, "local"));
        assert_eq!(store.providers_config().last_provider, "local");

        std::fs::remove_dir_all(root).ok();
        unsafe {
            match previous {
                Some(value) => std::env::set_var("BONE_DIR", value),
                None => std::env::remove_var("BONE_DIR"),
            }
        }
    }
}
