use std::io;
use std::time::Instant;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::style::{Attribute, SetAttribute};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

const BG: Color = Color::Indexed(16);
const TEXT: Color = Color::Indexed(252);
const MUTED: Color = Color::Indexed(244);
const DIM: Color = Color::Indexed(239);
const BORDER: Color = Color::Indexed(238);
const ACCENT: Color = Color::Indexed(250);
const BAR: Color = Color::Cyan;
const BAR_EMPTY: Color = Color::Indexed(236);

use crate::session_db::{HourUsage, UsageBucket, UsageStatsSnapshot, ViewMode};
use crate::ui::render::backend::BoneBackend;

/// RAII guard that disables raw mode on drop.
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

pub fn run<F>(load: F) -> io::Result<()>
where
    F: FnMut() -> io::Result<UsageStatsSnapshot>,
{
    run_inner(load)
}

fn run_inner<F>(mut load: F) -> io::Result<()>
where
    F: FnMut() -> io::Result<UsageStatsSnapshot>,
{
    let _raw_guard = RawModeGuard::enable()?;

    // Use an inner closure so that LeaveAlternateScreen always runs,
    // even if Terminal::new or run_loop fails.
    let result = (|| -> io::Result<()> {
        crossterm::execute!(
            io::stdout(),
            SetAttribute(Attribute::Reset),
            EnterAlternateScreen
        )?;
        let backend = BoneBackend::new(io::stdout());
        let mut term = Terminal::new(backend)?;
        run_loop(&mut term, &mut load)
    })();

    let leave_result = crossterm::execute!(
        io::stdout(),
        SetAttribute(Attribute::Reset),
        LeaveAlternateScreen,
        SetAttribute(Attribute::Reset)
    );
    result.and(leave_result)
}

fn run_loop<F>(term: &mut Terminal<BoneBackend<io::Stdout>>, load: &mut F) -> io::Result<()>
where
    F: FnMut() -> io::Result<UsageStatsSnapshot>,
{
    let mut snapshot = load()?;
    let mut mode = ViewMode::SevenDays;
    let mut scroll = 0usize;
    let mut refreshed = Instant::now();

    // Draw once, then only redraw on events — this dashboard is static.
    term.draw(|frame| draw(frame, &snapshot, mode, scroll, refreshed))?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('d') | KeyCode::Char('1') => {
                    mode = ViewMode::Today;
                    scroll = 0;
                }
                KeyCode::Char('w') | KeyCode::Char('2') => {
                    mode = ViewMode::SevenDays;
                    scroll = 0;
                }
                KeyCode::Char('m') | KeyCode::Char('3') => {
                    mode = ViewMode::FourWeeks;
                    scroll = 0;
                }
                KeyCode::Char('a') | KeyCode::Char('4') => {
                    mode = ViewMode::Months;
                    scroll = 0;
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    mode = mode.prev();
                    scroll = 0;
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    mode = mode.next();
                    scroll = 0;
                }
                KeyCode::Char('r') => {
                    snapshot = load()?;
                    refreshed = Instant::now();
                }
                KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                KeyCode::PageDown => scroll = scroll.saturating_add(8),
                KeyCode::PageUp => scroll = scroll.saturating_sub(8),
                _ => continue,
            },
            Event::Resize(_, _) => {}
            _ => continue,
        }
        term.draw(|frame| draw(frame, &snapshot, mode, scroll, refreshed))?;
    }
    Ok(())
}

fn draw(
    frame: &mut ratatui::Frame,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
    scroll: usize,
    refreshed: Instant,
) {
    let screen = frame.area();
    let root = Block::default().style(Style::default().bg(BG));
    frame.render_widget(root, screen);
    let area = screen;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, vertical[0], data, refreshed, mode);
    draw_cards(frame, vertical[1], data, mode);

    if vertical[2].width < 110 {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(6),
                Constraint::Length(5),
                Constraint::Length(8),
                Constraint::Min(10),
            ])
            .split(vertical[2]);
        draw_chart(frame, sections[0], data, mode, scroll);
        draw_hourly_chart(frame, sections[1], data, mode);
        draw_models(frame, sections[2], data, mode);
        draw_daily_activity(frame, sections[3], data);
    } else {
        let lower = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(53), Constraint::Percentage(47)])
            .split(vertical[2]);
        draw_chart(frame, lower[0], data, mode, scroll);

        let bottom = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(lower[1]);
        draw_models(frame, bottom[0], data, mode);
        draw_heat_and_conversations(frame, bottom[1], data, mode);
    }

    let footer = Line::from(vec![
        Span::styled(" q/Esc ", key_style()),
        Span::styled("quit  ", dim()),
        Span::styled(" 1-4 d/w/m/a ←→ ", key_style()),
        Span::styled("view  ", dim()),
        Span::styled(" r ", key_style()),
        Span::styled("refresh  ", dim()),
        Span::styled(" ↑↓ PgUp/PgDn ", key_style()),
        Span::styled("scroll", dim()),
    ]);
    frame.render_widget(Paragraph::new(footer), vertical[3]);
}

fn draw_header(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    refreshed: Instant,
    mode: ViewMode,
) {
    let range = range_label(data, mode);
    let lines = vec![
        Line::from(vec![
            Span::styled(
                " Token stats ",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(range, Style::default().fg(MUTED)),
        ]),
        Line::from(vec![
            tabs(mode),
            Span::styled(
                format!("  refreshed {}s ago", refreshed.elapsed().as_secs()),
                dim(),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).block(panel("Overview", BORDER)), area);
}

fn draw_cards(frame: &mut ratatui::Frame, area: Rect, data: &UsageStatsSnapshot, mode: ViewMode) {
    let total = data.range_summary(mode);
    let tokens = total.prompt_tokens + total.completion_tokens;
    let cache_pct = if total.prompt_tokens > 0 {
        (total.cached_tokens as f64 / total.prompt_tokens as f64 * 100.0).round() as i64
    } else {
        0
    };
    let cards = [
        (
            "Requests",
            compact_number(total.request_count as u64),
            ACCENT,
        ),
        ("Prompt", compact_number(total.prompt_tokens as u64), ACCENT),
        (
            "Completion",
            compact_number(total.completion_tokens as u64),
            ACCENT,
        ),
        ("Cached", compact_number(total.cached_tokens as u64), ACCENT),
        ("Total", compact_number(tokens as u64), ACCENT),
        ("Cache", format!("{cache_pct}%"), ACCENT),
    ];
    let constraints = (0..cards.len())
        .map(|_| Constraint::Percentage(100 / cards.len() as u16))
        .collect::<Vec<_>>();
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);
    for (idx, (label, value, _color)) in cards.iter().enumerate() {
        let line = Line::from(Span::styled(
            value.clone(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(line).block(panel(label, BORDER)), cols[idx]);
    }
}

fn draw_chart(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
    scroll: usize,
) {
    let buckets: Vec<&UsageBucket> = data.buckets(mode).iter().rev().collect();
    let max_rows = area.height.saturating_sub(2) as usize;
    let max_tokens = buckets
        .iter()
        .map(|b| bucket_tokens(b))
        .max()
        .unwrap_or(1)
        .max(1);
    let start = scroll.min(buckets.len().saturating_sub(max_rows));
    let shown = &buckets[start..buckets.len().min(start + max_rows)];
    let bar_width = area.width.saturating_sub(31).max(6) as usize;

    let mut lines = Vec::new();
    for b in shown {
        let tokens = bucket_tokens(b);
        let filled = ((tokens as f64 / max_tokens as f64) * bar_width as f64).round() as usize;

        lines.push(Line::from(vec![
            Span::styled(format!("{:>12} ", b.label), Style::default().fg(MUTED)),
            Span::styled(
                "█".repeat(filled),
                Style::default().fg(BAR).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "░".repeat(bar_width.saturating_sub(filled)),
                Style::default().fg(BAR_EMPTY),
            ),
            Span::styled(
                format!(" {:>8}", compact_number(tokens as u64)),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {:>5}r", compact_number(b.request_count as u64)),
                dim(),
            ),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("No usage events yet.", dim())));
    }
    frame.render_widget(
        Paragraph::new(lines).block(panel(&format!("{} usage", mode.title()), BORDER)),
        area,
    );
}

fn draw_models(frame: &mut ratatui::Frame, area: Rect, data: &UsageStatsSnapshot, mode: ViewMode) {
    let models = data.range_models(mode);
    let max_rows = area.height.saturating_sub(3) as usize;
    let w = area.width.saturating_sub(4) as usize; // inner width minus borders
    let name_w = (w / 2).max(12);
    let num_w = w.saturating_sub(name_w);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{:<width$}", "provider / model", width = name_w),
            dim(),
        ),
        Span::styled(
            format!(
                "{:>5} {:>nw$} {:>5}",
                "req",
                "tokens",
                "cache",
                nw = num_w.saturating_sub(12).max(4)
            ),
            dim(),
        ),
    ])];
    for m in models.iter().take(max_rows) {
        let tokens = m.prompt_tokens + m.completion_tokens;
        let cache = if m.prompt_tokens > 0 {
            m.cached_tokens * 100 / m.prompt_tokens
        } else {
            0
        };
        let tok_w = num_w.saturating_sub(12).max(4);
        lines.push(Line::from(vec![
            Span::styled(
                trunc(&format!("{} / {}", m.provider, m.model), name_w),
                Style::default().fg(TEXT),
            ),
            Span::styled(
                format!("{:>5} ", m.request_count),
                Style::default().fg(ACCENT),
            ),
            Span::styled(
                format!("{:>tw$} ", compact_number(tokens as u64), tw = tok_w),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:>4}%", cache), Style::default().fg(ACCENT)),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).block(panel("Provider / model", BORDER)),
        area,
    );
}

fn draw_hourly_chart(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
) {
    let (heat, title) = hourly_chart_lines(data, mode);
    frame.render_widget(Paragraph::new(heat).block(panel(&title, BORDER)), area);
}

fn draw_heat_and_conversations(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(6)])
        .split(area);

    let (heat, title) = hourly_chart_lines(data, mode);
    frame.render_widget(Paragraph::new(heat).block(panel(&title, BORDER)), chunks[0]);

    draw_daily_activity(frame, chunks[1], data);
}

fn hourly_chart_lines(data: &UsageStatsSnapshot, mode: ViewMode) -> (Vec<Line<'static>>, String) {
    let hourly_data: &[HourUsage] = data.hourly(mode);
    let mut by_hour = [0i64; 24];
    for h in hourly_data {
        if (0..24).contains(&h.hour) {
            by_hour[h.hour as usize] = h.prompt_tokens + h.completion_tokens;
        }
    }
    let max_hour = by_hour.iter().copied().max().unwrap_or(1).max(1);
    let hour_labels = (0..24)
        .map(|hour| format!("{hour:02}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut heat = vec![Line::from(Span::styled(format!("  {hour_labels}"), dim()))];
    let mut spans = vec![Span::raw("  ")];
    for v in by_hour {
        let block = if v > 0 { "█  " } else { "·  " };
        spans.push(Span::styled(block, heat_style(v, max_hour)));
    }
    heat.push(Line::from(spans));
    let total_hourly: i64 = by_hour.iter().sum();
    let peak_idx = by_hour
        .iter()
        .enumerate()
        .max_by_key(|(_, v)| *v)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let peak_reqs = hourly_data
        .iter()
        .find(|h| h.hour == peak_idx as i64)
        .map(|h| h.request_count)
        .unwrap_or(0);
    heat.push(Line::from(vec![
        Span::styled(
            if total_hourly > 0 {
                format!("peak {:02}:00 · {} req", peak_idx, peak_reqs)
            } else {
                "no activity".to_string()
            },
            Style::default().fg(TEXT),
        ),
        Span::styled(
            format!("   total {}", compact_number(total_hourly as u64)),
            dim(),
        ),
    ]));
    let title = format!("{} by hour", mode.title());
    (heat, title)
}

fn draw_daily_activity(frame: &mut ratatui::Frame, area: Rect, data: &UsageStatsSnapshot) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    if inner_width == 0 || inner_height < 3 {
        frame.render_widget(
            Paragraph::new(Vec::<Line>::new()).block(panel("Daily activity", BORDER)),
            area,
        );
        return;
    }

    let stats_width = if inner_width >= 54 { 20 } else { 0 };
    let label_width = 4usize;
    let cell_width = 2usize;
    let grid_cols = inner_width
        .saturating_sub(stats_width + label_width + usize::from(stats_width > 0))
        .saturating_div(cell_width)
        .max(1);
    let grid_rows = inner_height
        .saturating_sub(usize::from(inner_height >= 9) + usize::from(inner_height >= 8))
        .min(7);
    let capacity = grid_cols.saturating_mul(7).max(1);
    let trailing = data
        .daily_activity
        .last()
        .and_then(|b| weekday_index(&b.label))
        .map(|idx| 6usize.saturating_sub(idx))
        .unwrap_or(0);
    let visible_days = capacity.saturating_sub(trailing).max(1);
    let start = data.daily_activity.len().saturating_sub(visible_days);
    let activity = &data.daily_activity[start..];

    let max_tokens = activity.iter().map(bucket_tokens).max().unwrap_or(1).max(1);
    let total_tokens: i64 = activity.iter().map(bucket_tokens).sum();
    let most_active = activity.iter().max_by_key(|b| bucket_tokens(b));
    let first_day = activity.first().map(|b| b.label.as_str()).unwrap_or("none");
    let last_day = activity.last().map(|b| b.label.as_str()).unwrap_or("none");

    let mut cells = vec![vec![None; grid_cols]; 7];
    let mut week_labels = vec![None; grid_cols];
    let leading = activity
        .first()
        .and_then(|b| weekday_index(&b.label))
        .unwrap_or(0);
    for (idx, bucket) in activity.iter().enumerate() {
        let slot = leading + idx;
        let col = slot / 7;
        let row = slot % 7;
        if col < grid_cols {
            cells[row][col] = Some(bucket_tokens(bucket));
            week_labels[col].get_or_insert(bucket.label.as_str());
        }
    }

    let mut lines = Vec::new();
    if inner_height >= 9 {
        lines.push(activity_header(
            first_day,
            last_day,
            activity.len(),
            max_tokens,
            inner_width,
        ));
    }

    if inner_height >= 8 {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(label_width)),
            Span::styled(build_week_axis(&week_labels, cell_width), dim()),
        ]));
    }

    for (row, row_cells) in cells.iter().enumerate().take(grid_rows) {
        let mut spans = vec![Span::styled(weekday_label(row), dim())];
        for tokens in row_cells {
            let tokens = tokens.unwrap_or(0);
            spans.push(Span::styled("■ ", activity_style(tokens, max_tokens)));
        }
        if stats_width > 0 {
            spans.push(Span::raw(" "));
            match row {
                0 => spans.push(Span::styled("peak", dim())),
                1 => spans.push(Span::styled(
                    most_active
                        .map(|b| b.label.clone())
                        .unwrap_or_else(|| "none".to_string()),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                2 => spans.push(Span::styled(
                    most_active
                        .map(|b| compact_number(bucket_tokens(b) as u64))
                        .unwrap_or_else(|| "0".to_string()),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )),
                4 => spans.push(Span::styled(format!("{} days", activity.len()), dim())),
                5 => spans.push(Span::styled("total", dim())),
                6 => spans.push(Span::styled(
                    compact_number(total_tokens as u64),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                _ => {}
            }
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(
        Paragraph::new(lines).block(panel("Daily activity", BORDER)),
        area,
    );
}

fn activity_header(
    first_day: &str,
    last_day: &str,
    days: usize,
    max_tokens: i64,
    width: usize,
) -> Line<'static> {
    let full_range = format!("{first_day} → {last_day}");
    let compact_range = format!("{days} days → {last_day}");
    let range = if full_range.len() <= width {
        full_range
    } else if compact_range.len() <= width {
        compact_range
    } else {
        last_day.to_string()
    };
    let legend_width = "   less ■ ■ ■ more".len();
    if range.len() + legend_width > width {
        return Line::from(Span::styled(
            trunc(&range, width),
            Style::default().fg(TEXT),
        ));
    }
    Line::from(vec![
        Span::styled(range, Style::default().fg(TEXT)),
        Span::styled("   less ", dim()),
        Span::styled("■ ", activity_style(0, max_tokens)),
        Span::styled("■ ", activity_style(max_tokens / 2, max_tokens)),
        Span::styled("■", activity_style(max_tokens, max_tokens)),
        Span::styled(" more", dim()),
    ])
}

fn build_week_axis(labels: &[Option<&str>], cell_width: usize) -> String {
    let mut axis = vec![' '; labels.len().saturating_mul(cell_width)];
    let mut next_label_col = 0usize;
    for (col, label) in labels.iter().enumerate() {
        let Some(label) = label.and_then(short_month_day) else {
            continue;
        };
        if col < next_label_col {
            continue;
        }
        let start = col.saturating_mul(cell_width);
        if start + label.len() > axis.len() {
            continue;
        }
        for (idx, ch) in label.chars().enumerate() {
            axis[start + idx] = ch;
        }
        next_label_col = col + label.len().div_ceil(cell_width) + 1;
    }
    axis.into_iter().collect()
}

fn short_month_day(date: &str) -> Option<String> {
    Some(format!(
        "{}/{}",
        date.get(5..7)?.trim_start_matches('0'),
        date.get(8..10)?.trim_start_matches('0')
    ))
}

fn activity_style(tokens: i64, max: i64) -> Style {
    if tokens <= 0 {
        return Style::default().fg(Color::Indexed(240));
    }
    heat_style(tokens, max)
}

fn weekday_label(row: usize) -> &'static str {
    match row {
        0 => "Mon ",
        1 => "Tue ",
        2 => "Wed ",
        3 => "Thu ",
        4 => "Fri ",
        5 => "Sat ",
        _ => "Sun ",
    }
}

fn weekday_index(date: &str) -> Option<usize> {
    let year = date.get(0..4)?.parse::<i32>().ok()?;
    let month = date.get(5..7)?.parse::<i32>().ok()?;
    let day = date.get(8..10)?.parse::<i32>().ok()?;
    let offset = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = year - i32::from(month < 3);
    let sunday_based = (y + y / 4 - y / 100 + y / 400 + offset[(month - 1) as usize] + day) % 7;
    Some(((sunday_based + 6) % 7) as usize)
}

fn compact_number(count: u64) -> String {
    if count < 100_000 {
        return count.to_string();
    }

    let (value, suffix) = if count < 1_000_000 {
        (count as f64 / 1_000.0, "k")
    } else if count < 1_000_000_000 {
        (count as f64 / 1_000_000.0, "m")
    } else {
        (count as f64 / 1_000_000_000.0, "b")
    };
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}{suffix}")
    } else {
        format!("{rounded:.1}{suffix}")
    }
}

fn tabs(active: ViewMode) -> Span<'static> {
    let modes = [
        ViewMode::Today,
        ViewMode::SevenDays,
        ViewMode::FourWeeks,
        ViewMode::Months,
    ];
    let text = modes
        .iter()
        .map(|m| {
            if m.title() == active.title() {
                format!("[{} {}]", m.key(), m.title())
            } else {
                format!(" {} {} ", m.key(), m.title())
            }
        })
        .collect::<Vec<_>>()
        .join("  ");
    Span::styled(text, Style::default().fg(TEXT))
}

fn heat_style(value: i64, max: i64) -> Style {
    if value <= 0 {
        return Style::default().fg(DIM);
    }
    const GREENS: [Color; 15] = [
        Color::Rgb(0, 36, 0),
        Color::Rgb(0, 50, 0),
        Color::Rgb(0, 64, 0),
        Color::Rgb(0, 78, 0),
        Color::Rgb(0, 92, 0),
        Color::Rgb(0, 108, 0),
        Color::Rgb(0, 124, 0),
        Color::Rgb(0, 142, 0),
        Color::Rgb(0, 160, 0),
        Color::Rgb(0, 178, 0),
        Color::Rgb(0, 196, 0),
        Color::Rgb(0, 214, 0),
        Color::Rgb(12, 232, 8),
        Color::Rgb(35, 246, 14),
        Color::Rgb(57, 255, 20),
    ];
    let ratio = value as f64 / max as f64;
    let idx = ((ratio * GREENS.len() as f64).ceil() as usize).saturating_sub(1);
    Style::default()
        .fg(GREENS[idx.min(GREENS.len() - 1)])
        .add_modifier(Modifier::BOLD)
}

fn range_label(data: &UsageStatsSnapshot, mode: ViewMode) -> String {
    let buckets: &[UsageBucket] = data.buckets(mode);
    let first = buckets.first().map(|b| b.label.as_str()).unwrap_or("");
    let last = buckets.last().map(|b| b.label.as_str()).unwrap_or("");
    if first.is_empty() {
        return "no usage events yet".to_string();
    }
    if first == last {
        first.to_string()
    } else {
        format!("{} → {}", first, last)
    }
}

fn key_style() -> Style {
    Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
}

fn bucket_tokens(b: &UsageBucket) -> i64 {
    b.prompt_tokens + b.completion_tokens
}

fn panel(title: &str, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ))
}

fn dim() -> Style {
    Style::default().fg(MUTED)
}

fn trunc(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    let len = out.chars().count();
    if s.chars().count() > width && width > 1 {
        out.pop();
        out.push('…');
    }
    format!("{out:<width$}", width = width.max(len))
}
