//! First-launch onboarding wizard.
//!
//! A fullscreen, `/stats`-style takeover (see `crate::ui::stats`) that lets a
//! new user pick which bundled tools and commands get seeded into their
//! `~/.bone-rust/` copy, and whether `init.lua` is auto-populated or blank.
//! The choices are persisted via `config::apply_onboarding`, which doubles as
//! the "already onboarded" marker.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::config::{self, InitChoice, SetupSelection};
use crate::ui::fullscreen::{self, FullscreenTerminal};

const BG: Color = Color::Indexed(16);
const TEXT: Color = Color::Indexed(252);
const MUTED: Color = Color::Indexed(244);
const DIM: Color = Color::Indexed(239);
const BORDER: Color = Color::Indexed(238);
const ACCENT: Color = Color::Cyan;
const GOOD: Color = Color::Indexed(71);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Tools,
    Commands,
    Init,
    Confirm,
    /// Confirm screen for the re-run "re-seed bundled files" action.
    Reseed,
}

/// Actions offered on the Welcome step when the wizard is re-run (`!fresh`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum WelcomeAction {
    Customize,
    Reseed,
}

struct Item {
    name: String,
    desc: String,
    checked: bool,
    /// Category tag shown in the re-seed checklist (e.g. "tool", "config").
    /// Empty for the tools/commands picker lists, which are single-category.
    category: &'static str,
}

struct State {
    step: Step,
    tools: Vec<Item>,
    commands: Vec<Item>,
    /// Cursor within the active list (Tools/Commands) or option list (Init).
    cursor: usize,
    init_options: Vec<(&'static str, &'static str, InitChoice)>,
    init_cursor: usize,
    /// Action radio list shown on the Welcome step for re-runs; empty on a
    /// genuine first-launch onboarding (which goes straight into the flow).
    welcome_actions: Vec<(&'static str, &'static str, WelcomeAction)>,
    welcome_cursor: usize,
    /// Flat checklist for the re-seed action: config pages, libs, tools, then
    /// commands, each tagged with its category. All checked by default.
    reseed_items: Vec<Item>,
    completed: bool,
    /// True on a genuine first-launch onboarding, where pressing Esc to skip
    /// falls through to seeding every bundled tool/command. When re-run via
    /// `/setup` or `bone setup`, Esc just cancels and leaves config untouched.
    fresh: bool,
}

impl State {
    fn new(fresh: bool) -> Self {
        let to_items = |catalog: Vec<(&'static str, String)>, category: &'static str| {
            catalog
                .into_iter()
                .map(|(name, desc)| Item {
                    name: name.to_string(),
                    desc,
                    checked: true,
                    category,
                })
                .collect::<Vec<_>>()
        };

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

        let welcome_actions = if fresh {
            Vec::new()
        } else {
            vec![
                (
                    "Customize setup",
                    "Pick which tools, commands, and init.lua bone installs.",
                    WelcomeAction::Customize,
                ),
                (
                    "Re-seed bundled files",
                    "Overwrite bundled tools, commands, libraries, and config pages with this version's defaults.",
                    WelcomeAction::Reseed,
                ),
            ]
        };

        // The re-seed checklist mirrors what `config::reseed_selected` writes:
        // config pages, libraries, then the selected tools and commands.
        let reseed_items = {
            let cat = config::reseed_catalog();
            let mut items = to_items(cat.config_pages, "config");
            items.extend(to_items(cat.libs, "lib"));
            items.extend(to_items(cat.tools, "tool"));
            items.extend(to_items(cat.commands, "command"));
            items
        };

        Self {
            step: Step::Welcome,
            tools: to_items(crate::ext::default_tool_catalog(), ""),
            commands: to_items(crate::ext::default_command_catalog(), ""),
            cursor: 0,
            init_options,
            init_cursor: 0,
            welcome_actions,
            welcome_cursor: 0,
            reseed_items,
            completed: false,
            fresh,
        }
    }

    fn active_list(&mut self) -> Option<&mut Vec<Item>> {
        match self.step {
            Step::Tools => Some(&mut self.tools),
            Step::Commands => Some(&mut self.commands),
            Step::Reseed => Some(&mut self.reseed_items),
            _ => None,
        }
    }

    fn selection(&self) -> SetupSelection {
        SetupSelection {
            tools: self
                .tools
                .iter()
                .filter(|i| i.checked)
                .map(|i| i.name.clone())
                .collect(),
            commands: self
                .commands
                .iter()
                .filter(|i| i.checked)
                .map(|i| i.name.clone())
                .collect(),
        }
    }

    fn init_choice(&self) -> InitChoice {
        self.init_options[self.init_cursor].2
    }

    fn next_step(&mut self) {
        self.cursor = 0;
        self.step = match self.step {
            Step::Welcome => Step::Tools,
            Step::Tools => Step::Commands,
            Step::Commands => Step::Init,
            Step::Init => Step::Confirm,
            Step::Confirm => Step::Confirm,
            Step::Reseed => Step::Reseed,
        };
    }

    fn prev_step(&mut self) {
        self.cursor = 0;
        self.step = match self.step {
            Step::Welcome => Step::Welcome,
            Step::Tools => Step::Welcome,
            Step::Commands => Step::Tools,
            Step::Init => Step::Commands,
            Step::Confirm => Step::Init,
            Step::Reseed => Step::Welcome,
        };
    }
}

/// Run the onboarding wizard fullscreen. Returns `Ok(true)` if the user
/// completed it (choices applied), `Ok(false)` if they cancelled.
///
/// `fresh` marks a genuine first-launch onboarding (vs. a `/setup` re-run); it
/// only affects the skip/cancel copy, since fresh-launch skip seeds everything.
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
                KeyCode::Up | KeyCode::Char('k') => move_cursor(&mut state, -1),
                KeyCode::Down | KeyCode::Char('j') => move_cursor(&mut state, 1),
                KeyCode::Char(' ') => toggle(&mut state),
                KeyCode::Char('a') => set_all(&mut state, true),
                KeyCode::Char('n') => set_all(&mut state, false),
                KeyCode::Left | KeyCode::Char('h') => state.prev_step(),
                KeyCode::Right | KeyCode::Char('l') => advance(&mut state)?,
                KeyCode::Enter => match state.step {
                    Step::Confirm => {
                        apply(&mut state)?;
                        return Ok(state.completed);
                    }
                    Step::Reseed => {
                        reseed(&mut state)?;
                        return Ok(state.completed);
                    }
                    _ => advance(&mut state)?,
                },
                _ => continue,
            },
            Event::Resize(_, _) => {}
            _ => continue,
        }
        term.draw(|frame| draw(frame, &state))?;
    }
}

fn move_cursor(state: &mut State, delta: i32) {
    if state.step == Step::Welcome {
        let len = state.welcome_actions.len() as i32;
        if len > 0 {
            state.welcome_cursor =
                ((state.welcome_cursor as i32 + delta).rem_euclid(len)) as usize;
        }
        return;
    }
    if state.step == Step::Init {
        let len = state.init_options.len() as i32;
        state.init_cursor = ((state.init_cursor as i32 + delta).rem_euclid(len.max(1))) as usize;
        return;
    }
    let len = match state.step {
        Step::Tools => state.tools.len(),
        Step::Commands => state.commands.len(),
        Step::Reseed => state.reseed_items.len(),
        _ => return,
    } as i32;
    if len == 0 {
        return;
    }
    state.cursor = ((state.cursor as i32 + delta).rem_euclid(len)) as usize;
}

fn toggle(state: &mut State) {
    let cursor = state.cursor;
    if let Some(list) = state.active_list()
        && let Some(item) = list.get_mut(cursor)
    {
        item.checked = !item.checked;
    }
}

fn set_all(state: &mut State, checked: bool) {
    if let Some(list) = state.active_list() {
        for item in list.iter_mut() {
            item.checked = checked;
        }
    }
}

fn advance(state: &mut State) -> io::Result<()> {
    // On a re-run, the Welcome step is an action picker: route to the normal
    // flow or to the re-seed confirm screen based on the selection.
    if state.step == Step::Welcome && !state.welcome_actions.is_empty() {
        state.cursor = 0;
        state.step = match state.welcome_actions[state.welcome_cursor].2 {
            WelcomeAction::Customize => Step::Tools,
            WelcomeAction::Reseed => Step::Reseed,
        };
        return Ok(());
    }
    state.next_step();
    Ok(())
}

fn apply(state: &mut State) -> io::Result<()> {
    config::apply_onboarding(&state.selection(), state.init_choice())?;
    state.completed = true;
    Ok(())
}

fn reseed(state: &mut State) -> io::Result<()> {
    let chosen = |category: &str| {
        state
            .reseed_items
            .iter()
            .filter(|i| i.category == category && i.checked)
            .map(|i| i.name.clone())
            .collect::<std::collections::HashSet<String>>()
    };
    config::reseed_selected(
        &chosen("config"),
        &chosen("lib"),
        &chosen("tool"),
        &chosen("command"),
    )?;
    state.completed = true;
    Ok(())
}

// ---- rendering ----------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, state: &State) {
    let screen = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), screen);

    // Center a fixed-width card.
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
        Step::Tools => 2,
        Step::Commands => 3,
        Step::Init => 4,
        Step::Confirm => 5,
        Step::Reseed => 1,
    };
    // The re-seed branch isn't part of the linear 5-step flow, so it gets its
    // own subtitle instead of a misleading step counter.
    let subtitle = if state.step == Step::Reseed {
        "Re-seed bundled files".to_string()
    } else {
        format!("Setup · step {step_n} of 5")
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("Welcome to ", Style::default().fg(MUTED)),
            Span::styled(
                "bone",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(subtitle, Style::default().fg(DIM))),
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
        Step::Welcome => draw_welcome(frame, area, state),
        Step::Reseed => draw_list(
            frame,
            area,
            "Which files should bone re-seed?",
            "Overwrites the checked files with this version's defaults. Untick any you've customized.",
            &state.reseed_items,
            state.cursor,
        ),
        Step::Tools => draw_list(
            frame,
            area,
            "Which tools should bone install?",
            "Each is a Lua tool the model can call. Toggle with Space.",
            &state.tools,
            state.cursor,
        ),
        Step::Commands => draw_list(
            frame,
            area,
            "Which slash commands should bone install?",
            "Run on demand from the chat, like /compact or /memory.",
            &state.commands,
            state.cursor,
        ),
        Step::Init => draw_init(frame, area, state),
        Step::Confirm => draw_confirm(frame, area, state),
    }
}

const LOGO: [&str; 3] = [
    "┏┓ ┏━┓┏┓╻┏━╸   ┏━┓┏━╸┏━╸┏┓╻╺┳╸",
    "┣┻┓┃ ┃┃┗┫┣╸    ┣━┫┃╺┓┣╸ ┃┗┫ ┃ ",
    "┗━┛┗━┛╹ ╹┗━╸   ╹ ╹┗━┛┗━╸╹ ╹ ╹ ",
];

fn draw_welcome(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    // Re-runs show an action picker (customize vs. re-seed) instead of the
    // first-launch intro.
    if !state.welcome_actions.is_empty() {
        draw_welcome_actions(frame, area, state);
        return;
    }
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
                "This quick setup seeds your {} config. You'll pick:",
                config::bone_dir().display()
            ),
            Style::default().fg(MUTED),
        )),
        Line::from(""),
        bullet("Tools", "Lua functions the model can call."),
        bullet("Commands", "Slash commands you run from the chat."),
        bullet("init.lua", "Startup script — banner, sub-agents, hooks."),
        Line::from(""),
        Line::from(Span::styled(
            "Everything is editable later — just ask bone, or run /setup again.",
            Style::default().fg(DIM),
        )),
    ]);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), pad(area));
}

/// Welcome screen for a `/setup` re-run: a radio list of actions with a detail
/// pane, mirroring `draw_init`'s layout.
fn draw_welcome_actions(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    let area = pad(area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // hint
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // columns
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "What would you like to do?",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Pick with ↑/↓, confirm with →.",
            Style::default().fg(DIM),
        ))),
        rows[1],
    );

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 3), Constraint::Ratio(2, 3)])
        .split(rows[3]);

    let mut list_lines = Vec::with_capacity(state.welcome_actions.len());
    for (i, (label, _, _)) in state.welcome_actions.iter().enumerate() {
        let selected = i == state.welcome_cursor;
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

    let detail_lines = if let Some((label, desc, _)) = state.welcome_actions.get(state.welcome_cursor)
    {
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

fn draw_list(
    frame: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    hint: &str,
    items: &[Item],
    cursor: usize,
) {
    let area = pad(area);

    // Title + hint span the full width; the list/detail columns sit below.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // hint
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // columns
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title.to_string(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(DIM),
        ))),
        rows[1],
    );

    // Left third: the checkbox list. Right two-thirds: the focused item's
    // description, so long text wraps cleanly instead of overflowing the row.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 3), Constraint::Ratio(2, 3)])
        .split(rows[3]);

    // Window the list around the cursor so it never scrolls off-screen when
    // there are more items than visible rows.
    let (start, end) = visible_window(items.len(), cursor, cols[0].height as usize);
    let mut list_lines = Vec::with_capacity(end - start);
    for (i, item) in items.iter().enumerate().take(end).skip(start) {
        let selected = i == cursor;
        let cursor_span = Span::styled(
            if selected { " ▸ " } else { "   " },
            Style::default().fg(if selected { ACCENT } else { DIM }),
        );
        let check = if item.checked { "[x] " } else { "[ ] " };
        let check_span = Span::styled(
            check,
            Style::default().fg(if item.checked { GOOD } else { DIM }),
        );
        let name = item.name.strip_suffix(".lua").unwrap_or(&item.name);
        let name_style = if selected {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else if item.checked {
            Style::default().fg(TEXT)
        } else {
            Style::default().fg(MUTED)
        };
        let mut spans = vec![cursor_span, check_span, Span::styled(name.to_string(), name_style)];
        if !item.category.is_empty() {
            spans.push(Span::styled(
                format!("  ·{}", item.category),
                Style::default().fg(DIM),
            ));
        }
        list_lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(list_lines), cols[0]);

    // Detail pane: name as a heading, then the wrapped description.
    let detail = cols[1];
    let detail_lines = if let Some(item) = items.get(cursor) {
        let name = item.name.strip_suffix(".lua").unwrap_or(&item.name);
        let mut lines = vec![
            Line::from(Span::styled(
                name.to_string(),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        if item.desc.is_empty() {
            lines.push(Line::from(Span::styled(
                "No description.",
                Style::default().fg(DIM),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                item.desc.clone(),
                Style::default().fg(MUTED),
            )));
        }
        lines
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
        detail,
    );
}

fn draw_init(frame: &mut ratatui::Frame, area: Rect, state: &State) {
    let area = pad(area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // hint
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // columns
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

    // Left third: the radio list of options.
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

    // Right two-thirds: the focused option's description.
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
    let n_tools = state.tools.iter().filter(|i| i.checked).count();
    let n_cmds = state.commands.iter().filter(|i| i.checked).count();
    let init_label = state.init_options[state.init_cursor].0;

    let lines = vec![
        Line::from(Span::styled(
            "Ready to set up bone.",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        summary("Tools", format!("{n_tools} of {}", state.tools.len())),
        summary("Commands", format!("{n_cmds} of {}", state.commands.len())),
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
                "← to go back, Esc to skip (seeds everything)."
            } else {
                "← to go back, Esc to cancel (leaves config unchanged)."
            },
            Style::default().fg(DIM),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), pad(area));
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
    let mut keys: Vec<Span> = Vec::new();
    let push = |k: &str, label: &str, keys: &mut Vec<Span>| {
        keys.push(Span::styled(
            format!(" {k} "),
            Style::default()
                .fg(BG)
                .bg(MUTED)
                .add_modifier(Modifier::BOLD),
        ));
        keys.push(Span::styled(
            format!(" {label}   "),
            Style::default().fg(DIM),
        ));
    };

    let cancel_label = if state.fresh { "skip" } else { "cancel" };
    match state.step {
        Step::Tools | Step::Commands => {
            push("↑↓", "move", &mut keys);
            push("space", "toggle", &mut keys);
            push("a/n", "all/none", &mut keys);
            push("→", "next", &mut keys);
        }
        Step::Init => {
            push("↑↓", "choose", &mut keys);
            push("→", "next", &mut keys);
            push("←", "back", &mut keys);
        }
        Step::Confirm => {
            push("enter", "apply", &mut keys);
            push("←", "back", &mut keys);
            push("esc", cancel_label, &mut keys);
        }
        Step::Welcome => {
            if state.welcome_actions.is_empty() {
                push("→/enter", "start", &mut keys);
                push("esc", cancel_label, &mut keys);
            } else {
                push("↑↓", "choose", &mut keys);
                push("→/enter", "select", &mut keys);
                push("esc", cancel_label, &mut keys);
            }
        }
        Step::Reseed => {
            push("↑↓", "move", &mut keys);
            push("space", "toggle", &mut keys);
            push("a/n", "all/none", &mut keys);
            push("enter", "re-seed", &mut keys);
            push("esc", cancel_label, &mut keys);
        }
    }

    frame.render_widget(
        Paragraph::new(Line::from(keys))
            .alignment(Alignment::Left)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(BORDER)),
            ),
        area,
    );
}

/// Compute the `[start, end)` slice of a `len`-item list to render in a
/// viewport of `height` rows, keeping `cursor` visible and roughly centered.
fn visible_window(len: usize, cursor: usize, height: usize) -> (usize, usize) {
    if height == 0 || len <= height {
        return (0, len);
    }
    let start = cursor.saturating_sub(height / 2).min(len - height);
    (start, start + height)
}

/// Indent the body region by two columns for breathing room.
fn pad(area: Rect) -> Rect {
    Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    }
}
