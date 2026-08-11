//! First-launch onboarding wizard.
//!
//! A fullscreen, `/stats`-style takeover (see `crate::ui::stats`) that walks a
//! new user through: picking a provider + API key (skippable), choosing optional
//! tools/commands from the catalog (auto-downloaded), and whether `init.lua`
//! is auto-populated or blank. The populated choice stores its starter agent in
//! canonical `subagents.yaml`. The daemon supplies the snapshot and persists
//! the returned plan; this module owns only interaction and rendering.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::ui::catalog as catalog_ui;
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::picker::{self, Item};
use crate::ui::theme::Theme;
use bone_protocol::{CatalogAction, CatalogItem, InitChoice, SetupSnapshot};

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
    config_revision: u64,
    catalog_revision: String,
    /// Available providers as `(id, label)`.
    providers: Vec<(String, String)>,
    provider_cursor: usize,
    /// In-progress API key text for the focused provider.
    api_key: String,
    /// Catalog entries and the matching checklist rows.
    cat_entries: Vec<CatalogItem>,
    cat_items: Vec<Item>,
    cat_cursor: usize,
    init_options: Vec<(&'static str, &'static str, InitChoice)>,
    init_cursor: usize,
    /// True on a genuine first-launch onboarding; only affects skip/cancel copy.
    fresh: bool,
}

impl State {
    fn new(fresh: bool, snapshot: SetupSnapshot, theme: &Theme) -> Self {
        let mut providers: Vec<(String, String)> = snapshot
            .providers
            .into_iter()
            .map(|entry| {
                let label = if entry.label.is_empty() {
                    entry.id.clone()
                } else {
                    entry.label
                };
                (entry.id, label)
            })
            .collect();
        providers.sort_by(|a, b| a.0.cmp(&b.0));
        let provider_cursor = providers
            .iter()
            .position(|(id, _)| id == &snapshot.active_provider)
            .unwrap_or(0);
        let catalog_revision = snapshot.catalog.revision.clone();
        let cat_entries = snapshot.catalog.items;
        let cat_items = catalog_ui::build_items(&cat_entries, theme);
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
        if snapshot.init_exists {
            init_options.push((
                "Keep current",
                "Leave my existing init.lua untouched.",
                InitChoice::Keep,
            ));
        }

        Self {
            step: Step::Welcome,
            config_revision: snapshot.config_revision,
            catalog_revision,
            providers,
            provider_cursor,
            api_key: String::new(),
            cat_entries,
            cat_items,
            cat_cursor: 0,
            init_options,
            init_cursor: 0,
            fresh,
        }
    }

    fn init_choice(&self) -> InitChoice {
        self.init_options[self.init_cursor].2
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
}

/// Complete setup mutation collected by the frontend and applied by the daemon.
pub struct Plan {
    pub expected_config_revision: u64,
    pub expected_catalog_revision: String,
    pub provider_id: Option<String>,
    pub api_key: Option<String>,
    pub catalog: Vec<CatalogAction>,
    pub init: InitChoice,
}

/// Run the onboarding wizard and return its mutation plan, or `None` on cancel.
pub fn run(theme: &Theme, fresh: bool, snapshot: SetupSnapshot) -> io::Result<Option<Plan>> {
    fullscreen::run(|term| run_loop(term, fresh, snapshot, theme))
}

fn run_loop(
    term: &mut FullscreenTerminal,
    fresh: bool,
    snapshot: SetupSnapshot,
    theme: &Theme,
) -> io::Result<Option<Plan>> {
    let mut state = State::new(fresh, snapshot, theme);
    term.draw(|frame| draw(frame, &state, theme))?;

    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Up => move_cursor(&mut state, -1),
                KeyCode::Down => move_cursor(&mut state, 1),
                KeyCode::Left => state.prev_step(),
                KeyCode::Right => advance(&mut state),
                KeyCode::Enter => match state.step {
                    Step::Confirm => {
                        return Ok(Some(plan(&state)));
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
        term.draw(|frame| draw(frame, &state, theme))?;
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

fn advance(state: &mut State) {
    state.next_step();
}

fn plan(state: &State) -> Plan {
    Plan {
        expected_config_revision: state.config_revision,
        expected_catalog_revision: state.catalog_revision.clone(),
        provider_id: state
            .providers
            .get(state.provider_cursor)
            .map(|(id, _)| id.clone()),
        api_key: (!state.api_key.trim().is_empty()).then(|| state.api_key.trim().to_string()),
        catalog: catalog_ui::actions(&state.cat_entries, &state.cat_items, false),
        init: state.init_choice(),
    }
}

// ---- rendering ----------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, state: &State, theme: &Theme) {
    let p = &theme.palette;
    let screen = frame.area();
    if let Some(bg) = p.bg {
        frame.render_widget(Block::default().style(Style::default().bg(bg)), screen);
    }

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

    draw_header(frame, chunks[0], state, theme);
    draw_body(frame, chunks[1], state, theme);
    draw_footer(frame, chunks[2], state, theme);
}

fn draw_header(frame: &mut ratatui::Frame, area: Rect, state: &State, theme: &Theme) {
    let p = &theme.palette;
    let step_n = match state.step {
        Step::Welcome => 1,
        Step::Provider => 2,
        Step::Catalog => 3,
        Step::Init => 4,
        Step::Confirm => 5,
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("Welcome to ", Style::default().fg(p.muted)),
            Span::styled(
                "bone",
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            format!("Setup · step {step_n} of {STEP_COUNT}"),
            Style::default().fg(p.subtle),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(p.border))
                .padding(ratatui::widgets::Padding::new(2, 0, 1, 0)),
        ),
        area,
    );
}

fn draw_body(frame: &mut ratatui::Frame, area: Rect, state: &State, theme: &Theme) {
    let p = &theme.palette;
    match state.step {
        Step::Welcome => draw_welcome(frame, area, theme),
        Step::Provider => draw_provider(frame, area, state, theme),
        Step::Catalog => {
            if state.cat_items.is_empty() {
                let lines = vec![
                    Line::from(Span::styled(
                        "Catalog unavailable.",
                        Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "bone couldn't reach the catalog (you may be offline). \
                         Skip for now and add tools later with /catalog.",
                        Style::default().fg(p.muted),
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
                    theme,
                );
            }
        }
        Step::Init => draw_init(frame, area, state, theme),
        Step::Confirm => draw_confirm(frame, area, state, theme),
    }
}

const LOGO: [&str; 3] = [
    "┏┓ ┏━┓┏┓╻┏━╸   ┏━┓┏━╸┏━╸┏┓╻╺┳╸",
    "┣┻┓┃ ┃┃┗┫┣╸    ┣━┫┃╺┓┣╸ ┃┗┫ ┃ ",
    "┗━┛┗━┛╹ ╹┗━╸   ╹ ╹┗━┛┗━╸╹ ╹ ╹ ",
];

fn draw_welcome(frame: &mut ratatui::Frame, area: Rect, theme: &Theme) {
    let p = &theme.palette;
    let mut lines = vec![];
    for row in LOGO {
        lines.push(Line::from(Span::styled(
            row,
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));
    lines.extend(vec![
        Line::from(Span::styled(
            "bone is yours to shape.",
            Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "This quick setup configures the daemon host. You'll set:",
            Style::default().fg(p.muted),
        )),
        Line::from(""),
        bullet(
            "Provider",
            "Pick one and drop in an API key (optional).",
            theme,
        ),
        bullet(
            "Catalog",
            "Optional tools & commands, downloaded on demand.",
            theme,
        ),
        bullet(
            "init.lua",
            "Startup script — banner and advanced hooks.",
            theme,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Everything is editable later — just ask bone, or run /setup again.",
            Style::default().fg(p.subtle),
        )),
    ]);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        picker::pad(area),
    );
}

fn draw_provider(frame: &mut ratatui::Frame, area: Rect, state: &State, theme: &Theme) {
    let p = &theme.palette;
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
            Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑/↓ choose · type your API key · → to continue (or skip).",
            Style::default().fg(p.subtle),
        ))),
        rows[1],
    );

    if state.providers.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No providers configured. Add one with /config later.",
                Style::default().fg(p.muted),
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
        let mut line = radio_option(label.clone(), selected, theme);
        line.spans.push(Span::styled(
            format!("  ({id})"),
            Style::default().fg(p.subtle),
        ));
        list_lines.push(line);
    }
    frame.render_widget(Paragraph::new(list_lines), rows[3]);

    // Masked key field.
    let masked = "•".repeat(state.api_key.chars().count());
    let key_line = Line::from(vec![
        Span::styled("API key  ", Style::default().fg(p.muted)),
        Span::styled(
            if masked.is_empty() {
                "(leave blank to skip)".to_string()
            } else {
                masked
            },
            Style::default().fg(if state.api_key.is_empty() {
                p.subtle
            } else {
                p.fg
            }),
        ),
    ]);
    frame.render_widget(Paragraph::new(key_line), rows[4]);
}

fn bullet(head: &str, rest: &str, theme: &Theme) -> Line<'static> {
    let p = &theme.palette;
    Line::from(vec![
        Span::styled("  • ", Style::default().fg(p.accent)),
        Span::styled(
            format!("{head}  "),
            Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(rest.to_string(), Style::default().fg(p.muted)),
    ])
}

fn radio_option(label: String, selected: bool, theme: &Theme) -> Line<'static> {
    let p = &theme.palette;
    Line::from(vec![
        Span::styled(
            if selected { " ● " } else { " ○ " },
            Style::default().fg(if selected { p.good } else { p.subtle }),
        ),
        Span::styled(
            label,
            if selected {
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.fg)
            },
        ),
    ])
}

fn draw_init(frame: &mut ratatui::Frame, area: Rect, state: &State, theme: &Theme) {
    let p = &theme.palette;
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
            Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "init.lua runs once at launch. Pick with ↑/↓, confirm with →.",
            Style::default().fg(p.subtle),
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
        list_lines.push(radio_option(label.to_string(), selected, theme));
    }
    frame.render_widget(Paragraph::new(list_lines), cols[0]);

    let detail_lines = if let Some((label, desc, _)) = state.init_options.get(state.init_cursor) {
        vec![
            Line::from(Span::styled(
                label.to_string(),
                Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(desc.to_string(), Style::default().fg(p.muted))),
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
                    .border_style(Style::default().fg(p.border))
                    .padding(ratatui::widgets::Padding::horizontal(2)),
            ),
        cols[1],
    );
}

fn draw_confirm(frame: &mut ratatui::Frame, area: Rect, state: &State, theme: &Theme) {
    let p = &theme.palette;
    let n_cat = state.cat_items.iter().filter(|i| i.checked).count();
    let init_label = state.init_options[state.init_cursor].0;
    let provider = state
        .providers
        .get(state.provider_cursor)
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| "skipped".to_string());

    let lines = vec![
        Line::from(Span::styled(
            "Ready to set up bone.",
            Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        summary("Provider", provider, theme),
        summary("Catalog", format!("{n_cat} selected"), theme),
        summary("init.lua", init_label.to_string(), theme),
        Line::from(""),
        Line::from(Span::styled(
            "Press Enter to apply these on the daemon host.",
            Style::default().fg(p.good),
        )),
        Line::from(Span::styled(
            if state.fresh {
                "← to go back, Esc to skip (seeds defaults)."
            } else {
                "← to go back, Esc to cancel (leaves config unchanged)."
            },
            Style::default().fg(p.subtle),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        picker::pad(area),
    );
}

fn summary(head: &str, value: String, theme: &Theme) -> Line<'static> {
    let p = &theme.palette;
    Line::from(vec![
        Span::styled(format!("  {head:<10} "), Style::default().fg(p.muted)),
        Span::styled(
            value,
            Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn draw_footer(frame: &mut ratatui::Frame, area: Rect, state: &State, theme: &Theme) {
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
    picker::draw_footer(frame, area, keys, theme);
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
