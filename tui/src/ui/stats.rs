//! In-app usage-stats popup view.

use std::io;
use std::time::Instant;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::session_db::{DateRange, HourUsage, UsageBucket, UsageStatsSnapshot, ViewMode};
use crate::ui::color::color_to_rgb;
use crate::ui::fullscreen::{self, FullscreenTerminal};
use crate::ui::theme::Theme;

pub fn run<F>(theme: &Theme, mut load: F) -> io::Result<()>
where
    F: FnMut(&Option<DateRange>) -> io::Result<UsageStatsSnapshot>,
{
    fullscreen::run(|term| run_loop(term, theme, &mut load))
}

fn run_loop<F>(term: &mut FullscreenTerminal, theme: &Theme, load: &mut F) -> io::Result<()>
where
    F: FnMut(&Option<DateRange>) -> io::Result<UsageStatsSnapshot>,
{
    let mut snapshot = load(&None)?;
    let heat_scale = HeatScale::new(theme);
    let mut mode = ViewMode::SevenDays;
    let mut custom: Option<DateRange> = None;
    let mut picker: Option<DatePick> = None;
    let mut scroll = 0usize;
    let mut refreshed = Instant::now();
    let mut error: Option<String> = None;

    // Draw once, then only redraw on events — this dashboard is static.
    term.draw(|frame| {
        draw(
            frame,
            theme,
            &snapshot,
            mode,
            custom.as_ref(),
            &picker,
            &error,
            scroll,
            refreshed,
            &heat_scale,
        )
    })?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(pick) = picker.as_mut() {
                    match handle_picker_key(key.code, pick) {
                        Some(PickerAction::Cancel) => {
                            picker = None;
                            error = None;
                        }
                        Some(PickerAction::Apply(range)) => match load(&Some(range.clone())) {
                            Ok(s) => {
                                snapshot = s;
                                custom = Some(range);
                                scroll = 0;
                                error = None;
                                picker = None;
                            }
                            Err(e) => error = Some(e.to_string()),
                        },
                        None => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('d') | KeyCode::Char('1') => {
                            custom = None;
                            mode = ViewMode::Today;
                            scroll = 0;
                        }
                        KeyCode::Char('w') | KeyCode::Char('2') => {
                            custom = None;
                            mode = ViewMode::SevenDays;
                            scroll = 0;
                        }
                        KeyCode::Char('m') | KeyCode::Char('3') => {
                            custom = None;
                            mode = ViewMode::FourWeeks;
                            scroll = 0;
                        }
                        KeyCode::Char('y') | KeyCode::Char('4') => {
                            custom = None;
                            mode = ViewMode::Yearly;
                            scroll = 0;
                        }
                        KeyCode::Char('a') | KeyCode::Char('5') => {
                            custom = None;
                            mode = ViewMode::Months;
                            scroll = 0;
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            custom = None;
                            mode = mode.prev();
                            scroll = 0;
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            custom = None;
                            mode = mode.next();
                            scroll = 0;
                        }
                        KeyCode::Char('t') => {
                            picker = Some(DatePick::for_snapshot(&snapshot));
                            error = None;
                        }
                        KeyCode::Char('r') => {
                            snapshot = load(&custom)?;
                            refreshed = Instant::now();
                        }
                        KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                        KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                        KeyCode::PageDown => scroll = scroll.saturating_add(8),
                        KeyCode::PageUp => scroll = scroll.saturating_sub(8),
                        _ => continue,
                    }
                }
            }
            Event::Resize(_, _) => {}
            _ => continue,
        }
        term.draw(|frame| {
            draw(
                frame,
                theme,
                &snapshot,
                mode,
                custom.as_ref(),
                &picker,
                &error,
                scroll,
                refreshed,
                &heat_scale,
            )
        })?;
    }
    Ok(())
}

/// Editable start/end fields for the custom date-range picker.
#[derive(Clone)]
struct DatePick {
    start: String,
    end: String,
    field: usize,
}

impl DatePick {
    fn for_snapshot(snapshot: &UsageStatsSnapshot) -> Self {
        let (start, end) = snapshot
            .daily_activity
            .first()
            .zip(snapshot.daily_activity.last())
            .map(|(a, b)| (a.label.clone(), b.label.clone()))
            .unwrap_or_default();
        Self {
            start,
            end,
            field: 0,
        }
    }

    fn current(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.start,
            _ => &mut self.end,
        }
    }
}

enum PickerAction {
    Apply(DateRange),
    Cancel,
}

fn handle_picker_key(code: KeyCode, pick: &mut DatePick) -> Option<PickerAction> {
    match code {
        KeyCode::Esc => Some(PickerAction::Cancel),
        KeyCode::Enter => {
            let clean = |s: &str| {
                let t = s.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            };
            let start = clean(&pick.start).unwrap_or_else(|| "0000-01-01".into());
            let end = clean(&pick.end).unwrap_or_else(|| "9999-12-31".into());
            Some(PickerAction::Apply(DateRange { start, end }))
        }
        KeyCode::Tab | KeyCode::Down => {
            pick.field = (pick.field + 1) % 2;
            None
        }
        KeyCode::BackTab | KeyCode::Up => {
            pick.field = (pick.field + 1) % 2;
            None
        }
        KeyCode::Backspace => {
            pick.current().pop();
            None
        }
        KeyCode::Char(c) => {
            if c.is_ascii_digit() || c == '-' {
                let s = pick.current();
                if s.chars().count() < 10 {
                    s.push(c);
                }
            }
            None
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    frame: &mut ratatui::Frame,
    theme: &Theme,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
    custom: Option<&DateRange>,
    picker: &Option<DatePick>,
    error: &Option<String>,
    scroll: usize,
    refreshed: Instant,
    heat_scale: &HeatScale,
) {
    let pal = theme;
    let screen = frame.area();
    let root = Block::default().style(match pal.palette.bg {
        Some(bg) => Style::default().bg(bg),
        None => Style::default(),
    });
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

    draw_header(frame, vertical[0], data, refreshed, mode, custom, pal);
    draw_cards(frame, vertical[1], data, mode, custom, pal);

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
        draw_chart(frame, sections[0], data, mode, scroll, pal);
        draw_hourly_chart(frame, sections[1], data, mode, pal, heat_scale);
        draw_models(frame, sections[2], data, mode, custom, pal);
        draw_daily_activity(frame, sections[3], data, pal, heat_scale);
    } else {
        let lower = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(53), Constraint::Percentage(47)])
            .split(vertical[2]);
        draw_chart(frame, lower[0], data, mode, scroll, pal);

        let bottom = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(lower[1]);
        draw_models(frame, bottom[0], data, mode, custom, pal);
        draw_heat_and_conversations(frame, bottom[1], data, mode, pal, heat_scale);
    }

    let footer = Line::from(vec![
        Span::styled(" q/Esc ", key_style(pal)),
        Span::styled("quit  ", dim(pal)),
        Span::styled(" 1-5 d/w/m/y/a ←→ ", key_style(pal)),
        Span::styled("view  ", dim(pal)),
        Span::styled(" t ", key_style(pal)),
        Span::styled("dates  ", dim(pal)),
        Span::styled(" r ", key_style(pal)),
        Span::styled("refresh  ", dim(pal)),
        Span::styled(" ↑↓ PgUp/PgDn ", key_style(pal)),
        Span::styled("scroll", dim(pal)),
    ]);
    frame.render_widget(Paragraph::new(footer), vertical[3]);

    if let Some(pick) = picker {
        draw_date_picker(frame, area, pick, pal);
    }
    if let Some(msg) = error {
        draw_error_overlay(frame, area, msg, pal);
    }
}

fn draw_header(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    refreshed: Instant,
    mode: ViewMode,
    custom: Option<&DateRange>,
    pal: &Theme,
) {
    let range = match custom {
        Some(r) => {
            if r.start == r.end {
                r.start.clone()
            } else {
                format!("{} → {}", r.start, r.end)
            }
        }
        None => range_label(data, mode),
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                " Token stats ",
                Style::default()
                    .fg(pal.palette.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(range, Style::default().fg(pal.palette.muted)),
        ]),
        Line::from(vec![
            tabs(mode, custom.is_some(), pal),
            Span::styled(
                format!("  refreshed {}s ago", refreshed.elapsed().as_secs()),
                dim(pal),
            ),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(panel("Overview", pal.palette.border, pal.palette.fg)),
        area,
    );
}

fn draw_cards(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
    custom: Option<&DateRange>,
    pal: &Theme,
) {
    let total = match custom {
        Some(_) => data.total.clone(),
        None => data.range_summary(mode),
    };
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
            pal.palette.accent,
        ),
        (
            "Prompt",
            compact_number(total.prompt_tokens as u64),
            pal.palette.accent,
        ),
        (
            "Completion",
            compact_number(total.completion_tokens as u64),
            pal.palette.accent,
        ),
        (
            "Cached",
            compact_number(total.cached_tokens as u64),
            pal.palette.accent,
        ),
        ("Total", compact_number(tokens as u64), pal.palette.accent),
        ("Cache", format!("{cache_pct}%"), pal.palette.accent),
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
            Style::default()
                .fg(pal.palette.fg)
                .add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(
            Paragraph::new(line).block(panel(label, pal.palette.border, pal.palette.fg)),
            cols[idx],
        );
    }
}

/// Centered modal for entering a custom start/end date range.
fn draw_date_picker(frame: &mut ratatui::Frame, area: Rect, pick: &DatePick, pal: &Theme) {
    let w = 44u16;
    let h = 9u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(pal.chart))
        .title(Span::styled(
            " Custom date range (YYYY-MM-DD) ",
            Style::default()
                .fg(pal.palette.fg)
                .add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(block, popup);

    let inner = Rect::new(x + 2, y + 2, w.saturating_sub(4), h.saturating_sub(3));
    let field = |label: &str, value: &str, on: bool| -> Line<'static> {
        let style = if on {
            Style::default().fg(pal.chart).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(pal.palette.fg)
        };
        Line::from(vec![
            Span::styled(format!("{:<6}", label), dim(pal)),
            Span::styled(format!("{:<14}", value), style),
            if on {
                Span::styled("_", Style::default().fg(pal.chart))
            } else {
                Span::raw(" ")
            },
        ])
    };

    let lines = vec![
        Line::from(""),
        field("start:", &pick.start, pick.field == 0),
        Line::from(""),
        field("end:", &pick.end, pick.field == 1),
        Line::from(""),
        Line::from(Span::styled(
            " Tab switch · Enter apply · Esc cancel",
            dim(pal),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Small error toast shown at the bottom center.
fn draw_error_overlay(frame: &mut ratatui::Frame, area: Rect, msg: &str, pal: &Theme) {
    let text = format!(" error: {msg} ");
    let w = (text.chars().count() as u16 + 4).min(area.width);
    let h = 3u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + area.height.saturating_sub(h + 1);
    let popup = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                text,
                Style::default()
                    .fg(pal.palette.error)
                    .add_modifier(Modifier::BOLD),
            )),
        ])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(pal.palette.error)),
        ),
        popup,
    );
}

fn draw_chart(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
    scroll: usize,
    pal: &Theme,
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
        lines.push(usage_chart_line(b, max_tokens, bar_width, pal));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("No usage events yet.", dim(pal))));
    }
    frame.render_widget(
        Paragraph::new(lines).block(panel(
            &format!("{} usage", mode.title()),
            pal.palette.border,
            pal.palette.fg,
        )),
        area,
    );
}

fn usage_chart_line(
    bucket: &UsageBucket,
    max_tokens: i64,
    bar_width: usize,
    pal: &Theme,
) -> Line<'static> {
    let tokens = bucket_tokens(bucket);
    let filled = ((tokens as f64 / max_tokens as f64) * bar_width as f64).round() as usize;
    Line::from(vec![
        Span::styled(
            format!("{:>12} ", bucket.label),
            Style::default().fg(pal.palette.muted),
        ),
        Span::styled(
            "█".repeat(filled),
            Style::default().fg(pal.chart).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "░".repeat(bar_width.saturating_sub(filled)),
            Style::default().fg(pal.chart_empty),
        ),
        Span::styled(
            format!(" {:>8}", compact_number(tokens as u64)),
            Style::default()
                .fg(pal.palette.fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {:>5}r", compact_number(bucket.request_count as u64)),
            dim(pal),
        ),
    ])
}

fn draw_models(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
    custom: Option<&DateRange>,
    pal: &Theme,
) {
    let models = match custom {
        Some(_) => &data.by_model_today,
        None => data.range_models(mode),
    };
    let max_rows = area.height.saturating_sub(3) as usize;
    let w = area.width.saturating_sub(4) as usize; // inner width minus borders
    let name_w = (w / 2).max(12);
    let num_w = w.saturating_sub(name_w);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{:<width$}", "provider / model", width = name_w),
            dim(pal),
        ),
        Span::styled(
            format!(
                "{:>5} {:>nw$} {:>5}",
                "req",
                "tokens",
                "cache",
                nw = num_w.saturating_sub(12).max(4)
            ),
            dim(pal),
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
                Style::default().fg(pal.palette.fg),
            ),
            Span::styled(
                format!("{:>5} ", m.request_count),
                Style::default().fg(pal.palette.accent),
            ),
            Span::styled(
                format!("{:>tw$} ", compact_number(tokens as u64), tw = tok_w),
                Style::default()
                    .fg(pal.palette.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>4}%", cache),
                Style::default().fg(pal.palette.accent),
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).block(panel(
            "Provider / model",
            pal.palette.border,
            pal.palette.fg,
        )),
        area,
    );
}

fn draw_hourly_chart(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
    pal: &Theme,
    heat_scale: &HeatScale,
) {
    let (heat, title) = hourly_chart_lines(data, mode, pal, heat_scale);
    frame.render_widget(
        Paragraph::new(heat).block(panel(&title, pal.palette.border, pal.palette.fg)),
        area,
    );
}

fn draw_heat_and_conversations(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    mode: ViewMode,
    pal: &Theme,
    heat_scale: &HeatScale,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(6)])
        .split(area);

    let (heat, title) = hourly_chart_lines(data, mode, pal, heat_scale);
    frame.render_widget(
        Paragraph::new(heat).block(panel(&title, pal.palette.border, pal.palette.fg)),
        chunks[0],
    );

    draw_daily_activity(frame, chunks[1], data, pal, heat_scale);
}

fn hourly_chart_lines(
    data: &UsageStatsSnapshot,
    mode: ViewMode,
    pal: &Theme,
    heat_scale: &HeatScale,
) -> (Vec<Line<'static>>, String) {
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
    let mut heat = vec![Line::from(Span::styled(
        format!("  {hour_labels}"),
        dim(pal),
    ))];
    let mut spans = vec![Span::raw("  ")];
    for v in by_hour {
        let block = if v > 0 { "█  " } else { "·  " };
        spans.push(Span::styled(block, heat_scale.style(v, max_hour)));
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
            Style::default().fg(pal.palette.fg),
        ),
        Span::styled(
            format!("   total {}", compact_number(total_hourly as u64)),
            dim(pal),
        ),
    ]));
    let title = format!("{} by hour", mode.title());
    (heat, title)
}

fn draw_daily_activity(
    frame: &mut ratatui::Frame,
    area: Rect,
    data: &UsageStatsSnapshot,
    pal: &Theme,
    heat_scale: &HeatScale,
) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    if inner_width == 0 || inner_height < 3 {
        frame.render_widget(
            Paragraph::new(Vec::<Line>::new()).block(panel(
                "Daily activity",
                pal.palette.border,
                pal.palette.fg,
            )),
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
            pal,
            heat_scale,
        ));
    }

    if inner_height >= 8 {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(label_width)),
            Span::styled(build_week_axis(&week_labels, cell_width), dim(pal)),
        ]));
    }

    for (row, row_cells) in cells.iter().enumerate().take(grid_rows) {
        let mut spans = vec![Span::styled(weekday_label(row), dim(pal))];
        for tokens in row_cells {
            let tokens = tokens.unwrap_or(0);
            spans.push(Span::styled(
                "■ ",
                activity_style(tokens, max_tokens, heat_scale),
            ));
        }
        if stats_width > 0 {
            spans.push(Span::raw(" "));
            match row {
                0 => spans.push(Span::styled("peak", dim(pal))),
                1 => spans.push(Span::styled(
                    most_active
                        .map(|b| b.label.clone())
                        .unwrap_or_else(|| "none".to_string()),
                    Style::default()
                        .fg(pal.palette.fg)
                        .add_modifier(Modifier::BOLD),
                )),
                2 => spans.push(Span::styled(
                    most_active
                        .map(|b| compact_number(bucket_tokens(b) as u64))
                        .unwrap_or_else(|| "0".to_string()),
                    Style::default()
                        .fg(pal.palette.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                4 => spans.push(Span::styled(format!("{} days", activity.len()), dim(pal))),
                5 => spans.push(Span::styled("total", dim(pal))),
                6 => spans.push(Span::styled(
                    compact_number(total_tokens as u64),
                    Style::default()
                        .fg(pal.palette.fg)
                        .add_modifier(Modifier::BOLD),
                )),
                _ => {}
            }
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(
        Paragraph::new(lines).block(panel("Daily activity", pal.palette.border, pal.palette.fg)),
        area,
    );
}

fn activity_header(
    first_day: &str,
    last_day: &str,
    days: usize,
    max_tokens: i64,
    width: usize,
    pal: &Theme,
    heat_scale: &HeatScale,
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
            Style::default().fg(pal.palette.fg),
        ));
    }
    Line::from(vec![
        Span::styled(range, Style::default().fg(pal.palette.fg)),
        Span::styled("   less ", dim(pal)),
        Span::styled("■ ", activity_style(0, max_tokens, heat_scale)),
        Span::styled("■ ", activity_style(max_tokens / 2, max_tokens, heat_scale)),
        Span::styled("■", activity_style(max_tokens, max_tokens, heat_scale)),
        Span::styled(" more", dim(pal)),
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

fn activity_style(tokens: i64, max: i64, heat_scale: &HeatScale) -> Style {
    heat_scale.style(tokens, max)
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

fn tabs(active: ViewMode, custom: bool, pal: &Theme) -> Span<'static> {
    let modes = [
        ViewMode::Today,
        ViewMode::SevenDays,
        ViewMode::FourWeeks,
        ViewMode::Yearly,
        ViewMode::Months,
    ];
    let mut text = modes
        .iter()
        .map(|m| {
            if !custom && m.title() == active.title() {
                format!("[{} {}]", m.key(), m.title())
            } else {
                format!(" {} {} ", m.key(), m.title())
            }
        })
        .collect::<Vec<_>>()
        .join("  ");
    if custom {
        text.push_str("  [custom range]");
    }
    Span::styled(
        text,
        Style::default().fg(if custom {
            pal.palette.muted
        } else {
            pal.palette.fg
        }),
    )
}

const HEAT_LEVELS: usize = 15;

/// Build a heat gradient from `theme.heat_low` to `theme.heat_high` using
/// `color_to_rgb`. Valid themes always provide RGB-convertible endpoints; the
/// repeated semantic endpoint is only a defensive fallback for direct callers.
fn build_heat_gradient(pal: &Theme) -> [Color; HEAT_LEVELS] {
    let Some(low_rgb) = color_to_rgb(pal.heat_low) else {
        return [pal.heat_low; HEAT_LEVELS];
    };
    let Some(high_rgb) = color_to_rgb(pal.heat_high) else {
        return [pal.heat_high; HEAT_LEVELS];
    };

    std::array::from_fn(|index| {
        let t = index as f64 / (HEAT_LEVELS - 1) as f64;
        let channel =
            |low: u8, high: u8| (low as f64 + (high as f64 - low as f64) * t).round() as u8;
        Color::Rgb(
            channel(low_rgb.0, high_rgb.0),
            channel(low_rgb.1, high_rgb.1),
            channel(low_rgb.2, high_rgb.2),
        )
    })
}

/// Cached for the lifetime of the stats screen; heat cells only select a
/// precomputed color.
struct HeatScale {
    colors: [Color; HEAT_LEVELS],
    empty: Color,
}

impl HeatScale {
    fn new(pal: &Theme) -> Self {
        Self {
            colors: build_heat_gradient(pal),
            empty: pal.palette.subtle,
        }
    }

    fn style(&self, value: i64, max: i64) -> Style {
        if value <= 0 {
            return Style::default().fg(self.empty);
        }
        let ratio = value as f64 / max.max(1) as f64;
        let idx = ((ratio * self.colors.len() as f64).ceil() as usize).saturating_sub(1);
        Style::default()
            .fg(self.colors[idx.min(self.colors.len() - 1)])
            .add_modifier(Modifier::BOLD)
    }
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

fn key_style(pal: &Theme) -> Style {
    Style::default()
        .fg(pal.palette.fg)
        .add_modifier(Modifier::BOLD)
}

fn bucket_tokens(b: &UsageBucket) -> i64 {
    b.prompt_tokens + b.completion_tokens
}

fn panel(title: &str, border: Color, title_fg: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(title_fg).add_modifier(Modifier::BOLD),
        ))
}

fn dim(pal: &Theme) -> Style {
    Style::default().fg(pal.palette.muted)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Theme;

    #[test]
    fn distinct_theme_produces_different_heat_gradient() {
        let mut theme = Theme::default();
        theme.heat_low = Color::Rgb(0, 0, 50);
        theme.heat_high = Color::Rgb(0, 100, 255);

        let gradient = build_heat_gradient(&theme);

        assert_eq!(gradient.len(), 15);
        assert_eq!(gradient[0], theme.heat_low);
        assert_eq!(gradient[14], theme.heat_high);
        assert_ne!(gradient, build_heat_gradient(&Theme::default()));
    }

    #[test]
    fn chart_line_uses_chart_and_chart_empty_roles() {
        let mut theme = Theme::default();
        theme.chart = Color::Rgb(1, 2, 3);
        theme.chart_empty = Color::Rgb(4, 5, 6);
        let bucket = UsageBucket {
            label: "today".into(),
            prompt_tokens: 25,
            completion_tokens: 25,
            cached_tokens: 0,
            cost: 0.0,
            request_count: 1,
        };

        let line = usage_chart_line(&bucket, 100, 10, &theme);

        assert_eq!(line.spans[1].style.fg, Some(theme.chart));
        assert_eq!(line.spans[2].style.fg, Some(theme.chart_empty));
        assert_eq!(line.spans[1].content, "█████");
        assert_eq!(line.spans[2].content, "░░░░░");
    }

    #[test]
    fn non_rgb_heat_endpoints_repeat_the_semantic_fallback() {
        let mut theme = Theme::default();
        theme.heat_low = Color::Indexed(7);
        assert_eq!(
            build_heat_gradient(&theme),
            [Color::Indexed(7); HEAT_LEVELS]
        );

        theme.heat_low = Color::Rgb(1, 2, 3);
        theme.heat_high = Color::Reset;
        assert_eq!(build_heat_gradient(&theme), [Color::Reset; HEAT_LEVELS]);
    }

    #[test]
    fn heat_style_uses_low_subtle_and_high_colors() {
        let mut theme = Theme::default();
        theme.palette.subtle = Color::Rgb(13, 14, 15);
        theme.heat_low = Color::Rgb(7, 8, 9);
        theme.heat_high = Color::Rgb(10, 11, 12);
        let heat_scale = HeatScale::new(&theme);

        assert_eq!(heat_scale.style(0, 100).fg, Some(theme.palette.subtle));
        assert_eq!(heat_scale.style(1, 100).fg, Some(theme.heat_low));
        assert_eq!(heat_scale.style(100, 100).fg, Some(theme.heat_high));
    }
}
