//! First-launch onboarding wizard.
//!
//! A fullscreen, `/stats`-style takeover (see `crate::ui::stats`) that lets a
//! new user pick which bundled tools and commands get seeded into their
//! `~/.bone-rust/` copy, and whether `init.lua` is auto-populated or blank.
//! The choices are persisted via `config::apply_onboarding`, which doubles as
//! the "already onboarded" marker.

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::style::{Attribute, SetAttribute};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::config::{self, InitChoice, SetupSelection};
use crate::ui::render::backend::BoneBackend;

const BG: Color = Color::Indexed(16);
const TEXT: Color = Color::Indexed(252);
const MUTED: Color = Color::Indexed(244);
const DIM: Color = Color::Indexed(239);
const BORDER: Color = Color::Indexed(238);
const ACCENT: Color = Color::Cyan;
const GOOD: Color = Color::Indexed(71);

/// RAII guard that disables raw mode on drop. Mirrors `stats::RawModeGuard`.
struct RawModeGuard {
    was_enabled: bool,
}

impl RawModeGuard {
    fn enable() -> io::Result<Self> {
        let was_enabled = crossterm::terminal::is_raw_mode_enabled()?;
        if !was_enabled {
            crossterm::terminal::enable_raw_mode()?;
        }
        Ok(Self { was_enabled })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if !self.was_enabled
            && let Err(e) = crossterm::terminal::disable_raw_mode()
        {
            eprintln!("bone: warning: failed to disable raw mode: {e}");
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Tools,
    Commands,
    Init,
    Confirm,
}

struct Item {
    name: String,
    desc: String,
    checked: bool,
}

struct State {
    step: Step,
    tools: Vec<Item>,
    commands: Vec<Item>,
    /// Cursor within the active list (Tools/Commands) or option list (Init).
    cursor: usize,
    init_options: Vec<(&'static str, &'static str, InitChoice)>,
    init_cursor: usize,
    completed: bool,
    /// True on a genuine first-launch onboarding, where pressing Esc to skip
    /// falls through to seeding every bundled tool/command. When re-run via
    /// `/setup` or `bone setup`, Esc just cancels and leaves config untouched.
    fresh: bool,
}

impl State {
    fn new(fresh: bool) -> Self {
        let to_items = |catalog: Vec<(&'static str, String)>| {
            catalog
                .into_iter()
                .map(|(name, desc)| Item {
                    name: name.to_string(),
                    desc,
                    checked: true,
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

        Self {
            step: Step::Welcome,
            tools: to_items(crate::ext::default_tool_catalog()),
            commands: to_items(crate::ext::default_command_catalog()),
            cursor: 0,
            init_options,
            init_cursor: 0,
            completed: false,
            fresh,
        }
    }

    fn active_list(&mut self) -> Option<&mut Vec<Item>> {
        match self.step {
            Step::Tools => Some(&mut self.tools),
            Step::Commands => Some(&mut self.commands),
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
        };
    }
}

/// Run the onboarding wizard fullscreen. Returns `Ok(true)` if the user
/// completed it (choices applied), `Ok(false)` if they cancelled.
///
/// `fresh` marks a genuine first-launch onboarding (vs. a `/setup` re-run); it
/// only affects the skip/cancel copy, since fresh-launch skip seeds everything.
pub fn run(fresh: bool) -> io::Result<bool> {
    let _raw_guard = RawModeGuard::enable()?;

    let result = (|| -> io::Result<bool> {
        crossterm::execute!(
            io::stdout(),
            SetAttribute(Attribute::Reset),
            EnterAlternateScreen
        )?;
        let backend = BoneBackend::new(io::stdout());
        let mut term = Terminal::new(backend)?;
        run_loop(&mut term, fresh)
    })();

    let leave_result = crossterm::execute!(
        io::stdout(),
        SetAttribute(Attribute::Reset),
        LeaveAlternateScreen,
        SetAttribute(Attribute::Reset)
    );

    let completed = result?;
    leave_result?;
    Ok(completed)
}

fn run_loop(term: &mut Terminal<BoneBackend<io::Stdout>>, fresh: bool) -> io::Result<bool> {
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
                KeyCode::Enter => {
                    if state.step == Step::Confirm {
                        apply(&mut state)?;
                        return Ok(state.completed);
                    }
                    advance(&mut state)?;
                }
                _ => continue,
            },
            Event::Resize(_, _) => {}
            _ => continue,
        }
        term.draw(|frame| draw(frame, &state))?;
    }
}

fn move_cursor(state: &mut State, delta: i32) {
    if state.step == Step::Init {
        let len = state.init_options.len() as i32;
        state.init_cursor = ((state.init_cursor as i32 + delta).rem_euclid(len.max(1))) as usize;
        return;
    }
    let len = match state.step {
        Step::Tools => state.tools.len(),
        Step::Commands => state.commands.len(),
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
    state.next_step();
    Ok(())
}

fn apply(state: &mut State) -> io::Result<()> {
    config::apply_onboarding(&state.selection(), state.init_choice())?;
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
            format!("Setup · step {step_n} of 5"),
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
        list_lines.push(Line::from(vec![
            cursor_span,
            check_span,
            Span::styled(name.to_string(), name_style),
        ]));
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
            push("→/enter", "start", &mut keys);
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
