//! First-launch onboarding wizard.
//!
//! A fullscreen, `/stats`-style takeover (see `crate::ui::stats`) that walks a
//! new user through: picking a provider + API key (skippable), choosing optional
//! tools/commands from the catalogue (auto-downloaded), and whether `init.lua`
//! is auto-populated or blank. The choices are persisted via
//! `config::apply_onboarding`, which doubles as the "already onboarded" marker.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::config::{self, InitChoice, SetupSelection};
use crate::ext::catalog::{self, CatalogEntry};
use crate::ui::catalogue;
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::picker::{self, Item, ACCENT, BG, BORDER, DIM, GOOD, MUTED, TEXT};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Provider,
    Catalogue,
    Init,
    Confirm,
}

const STEP_COUNT: usize = 5;

struct State {
    step: Step,
    /// Loaded config — provider edits are persisted through it immediately.
    custom: config::custom::CustomConfigs,
    /// Available providers as `(id, label)`.
    providers: Vec<(String, String)>,
    provider_cursor: usize,
    /// In-progress API key text for the focused provider.
    api_key: String,
    /// Provider id whose key we saved, for the confirm summary.
    provider_saved: Option<String>,
    /// Catalogue entries and the matching checklist rows.
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
    fn new(fresh: bool) -> Self {
        let custom = config::custom::CustomConfigs::load();
        let mut providers: Vec<(String, String)> = custom
            .derive_providers_config()
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

        // Fetch the catalogue index (blocking, cached fallback) so the picker is
        // populated; offline simply yields an empty list.
        let cat_entries = catalog::sync_quiet();
        let cat_items = catalogue::build_items(&cat_entries);

        let init_exists = config::setup_selection_path()
            .parent()
            .map(|d| d.join("init.lua").exists())
            .unwrap_or(false);
        let mut init_options = vec![
            (
                "Auto-populated",
                "Banner + a live researcher sub-agent you can dispatch right away.",
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

        Self {
            step: Step::Welcome,
            custom,
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
        }
    }

    fn init_choice(&self) -> InitChoice {
        self.init_options[self.init_cursor].2
    }

    /// Onboarding seeds no Lua tools (all optional ones live in the catalogue)
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
            Step::Provider => Step::Catalogue,
            Step::Catalogue => Step::Init,
            Step::Init => Step::Confirm,
            Step::Confirm => Step::Confirm,
        };
    }

    fn prev_step(&mut self) {
        self.step = match self.step {
            Step::Welcome => Step::Welcome,
            Step::Provider => Step::Welcome,
            Step::Catalogue => Step::Provider,
            Step::Init => Step::Catalogue,
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
        if let Some(mut entry) = self.custom.get_provider_entry("providers", &id) {
            entry.api_key = key.to_string();
            self.custom.set_provider_entry("providers", &id, &entry);
            self.custom.set_last_provider(&id);
            self.provider_saved = Some(id);
        }
    }
}

/// Run the onboarding wizard fullscreen. Returns `Ok(true)` if the user
/// completed it (choices applied), `Ok(false)` if they cancelled.
pub fn run(fresh: bool) -> io::Result<bool> {
    fullscreen::run(|term| run_loop(term, fresh))
}

fn run_loop(term: &mut FullscreenTerminal, fresh: bool) -> io::Result<bool> {
    let mut state = State::new(fresh);
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
        Step::Catalogue => match c {
            ' ' => toggle_catalogue(state),
            'a' => set_all_catalogue(state, true),
            'n' => set_all_catalogue(state, false),
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
        Step::Catalogue => (&mut state.cat_cursor, state.cat_items.len()),
        Step::Init => (&mut state.init_cursor, state.init_options.len()),
        _ => return,
    };
    if len == 0 {
        return;
    }
    *cursor = ((*cursor as i32 + delta).rem_euclid(len as i32)) as usize;
}

fn toggle_catalogue(state: &mut State) {
    if let Some(item) = state.cat_items.get_mut(state.cat_cursor) {
        item.checked = !item.checked;
    }
}

fn set_all_catalogue(state: &mut State, checked: bool) {
    for item in state.cat_items.iter_mut() {
        item.checked = checked;
    }
}

fn advance(state: &mut State) {
    // Persist the provider choice as we leave the Provider step.
    if state.step == Step::Provider {
        state.save_provider();
    }
    state.next_step();
}

fn apply(state: &mut State) -> io::Result<()> {
    config::apply_onboarding(&state.selection(), state.init_choice())?;
    // Install the catalogue picks (best-effort; failures don't abort onboarding).
    let _ = catalogue::apply(&state.cat_entries, &state.cat_items);
    state.completed = true;
    Ok(())
}

// ---- rendering ----------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, state: &State) {
    let screen = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), screen);

    let width = screen.width.min(76);
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
            Constraint::Min(6),    // body
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
        Step::Catalogue => 3,
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
        Step::Catalogue => {
            if state.cat_items.is_empty() {
                let lines = vec![
                    Line::from(Span::styled(
                        "Catalogue unavailable.",
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "bone couldn't reach the catalogue (you may be offline). \
                         Skip for now and add tools later with /catalogue.",
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
        bullet("Catalogue", "Optional tools & commands, downloaded on demand."),
        bullet("init.lua", "Startup script — banner, sub-agents, hooks."),
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
                "No providers configured. Edit config/providers.yaml later.",
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
            Span::styled(marker, Style::default().fg(if selected { GOOD } else { DIM })),
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
            Span::styled(marker, Style::default().fg(if selected { GOOD } else { DIM })),
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
        Paragraph::new(detail_lines).wrap(Wrap { trim: false }).block(
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
        summary("Catalogue", format!("{n_cat} selected")),
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
        Span::styled(value, Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
    ])
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    let mut keys: Vec<Span> = Vec::new();
    let mut push = |k: &str, label: &str| {
        keys.push(Span::styled(
            format!(" {k} "),
            Style::default().fg(BG).bg(MUTED).add_modifier(Modifier::BOLD),
        ));
        keys.push(Span::styled(format!(" {label}   "), Style::default().fg(DIM)));
    };

    let cancel_label = if state.fresh { "skip" } else { "cancel" };
    match state.step {
        Step::Welcome => {
            push("→/enter", "start");
            push("esc", cancel_label);
        }
        Step::Provider => {
            push("↑↓", "choose");
            push("type", "key");
            push("→", "next");
            push("←", "back");
            push("esc", cancel_label);
        }
        Step::Catalogue => {
            push("↑↓", "move");
            push("space", "toggle");
            push("a/n", "all/none");
            push("→", "next");
            push("←", "back");
        }
        Step::Init => {
            push("↑↓", "choose");
            push("→", "next");
            push("←", "back");
        }
        Step::Confirm => {
            push("enter", "apply");
            push("←", "back");
            push("esc", cancel_label);
        }
    }

    frame.render_widget(
        Paragraph::new(Line::from(keys)).alignment(Alignment::Left).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
        ),
        area,
    );
}
