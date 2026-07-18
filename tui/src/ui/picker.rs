//! Shared primitives for the fullscreen checklist screens (the onboarding
//! wizard and `/catalog`): the palette, the `Item` row model, and the
//! two-column list/detail renderer. Keeping these in one place means both
//! screens look and behave identically.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

pub const BG: Color = Color::Indexed(16);
pub const TEXT: Color = Color::Indexed(252);
pub const MUTED: Color = Color::Indexed(244);
pub const DIM: Color = Color::Indexed(239);
pub const BORDER: Color = Color::Indexed(238);
pub const ACCENT: Color = Color::Cyan;
pub const GOOD: Color = Color::Indexed(71);
pub const BAD: Color = Color::Indexed(167);

/// One toggleable row in a checklist.
pub struct Item {
    pub name: String,
    pub desc: String,
    pub checked: bool,
    /// True once the user has explicitly toggled this item. Used by `apply`
    /// to distinguish "user unchecked" from "was unchecked by default".
    pub user_touched: bool,
    /// Category tag shown after the name (e.g. "tool"/"config"). Empty to hide.
    pub category: &'static str,
    /// Optional status tag shown at the end of the row (e.g. "update"). Rendered
    /// in `tag_color`; `None` to hide.
    pub tag: Option<String>,
    /// Color for `tag`. Defaults to the accent color.
    pub tag_color: Color,
    /// Optional section heading rendered immediately before this row.
    pub section: Option<String>,
    /// Label/value metadata rendered in the detail pane.
    pub details: Vec<(String, String)>,
    /// Optional extended description rendered after the summary and metadata.
    pub long_desc: Option<String>,
}

impl Item {
    pub fn new(name: String, desc: String, checked: bool) -> Self {
        Self {
            name,
            desc,
            checked,
            user_touched: false,
            category: "",
            tag: None,
            tag_color: ACCENT,
            section: None,
            details: Vec::new(),
            long_desc: None,
        }
    }
}

/// Indent the body region by two columns for breathing room.
pub fn pad(area: Rect) -> Rect {
    Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
    }
}

/// Compute the `[start, end)` slice of a `len`-item list to render in a
/// viewport of `height` rows, keeping `cursor` visible and roughly centered.
pub fn visible_window(len: usize, cursor: usize, height: usize) -> (usize, usize) {
    if height == 0 || len <= height {
        return (0, len);
    }
    let start = cursor.saturating_sub(height / 2).min(len - height);
    (start, start + height)
}

/// Render a title + hint and a two-column checkbox list / detail pane.
pub fn draw_list(
    frame: &mut ratatui::Frame,
    area: Rect,
    title: &str,
    hint: &str,
    items: &[Item],
    cursor: usize,
) {
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

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 3), Constraint::Ratio(2, 3)])
        .split(rows[3]);

    let mut all_lines = Vec::with_capacity(items.len() * 2);
    let mut selected_row = 0;
    for (i, item) in items.iter().enumerate() {
        if let Some(section) = &item.section {
            all_lines.push(Line::from(Span::styled(
                section.clone(),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )));
        }
        if i == cursor {
            selected_row = all_lines.len();
        }
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
        let mut spans = vec![
            cursor_span,
            check_span,
            Span::styled(name.to_string(), name_style),
        ];
        if !item.category.is_empty() {
            spans.push(Span::styled(
                format!("  ·{}", item.category),
                Style::default().fg(DIM),
            ));
        }
        if let Some(tag) = &item.tag {
            spans.push(Span::styled(
                format!("  {tag}"),
                Style::default()
                    .fg(item.tag_color)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        all_lines.push(Line::from(spans));
    }
    let height = cols[0].height as usize;
    let start = if all_lines.len() <= height {
        0
    } else {
        selected_row
            .saturating_sub(height / 2)
            .min(all_lines.len() - height)
    };
    let end = (start + height).min(all_lines.len());
    frame.render_widget(Paragraph::new(all_lines[start..end].to_vec()), cols[0]);

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
        if !item.details.is_empty() {
            lines.push(Line::from(""));
            for (label, value) in &item.details {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{label}: "),
                        Style::default().fg(DIM).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(value.clone(), Style::default().fg(TEXT)),
                ]));
            }
        }
        if let Some(long_desc) = &item.long_desc {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                long_desc.clone(),
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

/// Render a one-line key-bindings footer under a top border. Each `(key, label)`
/// pair is shown as a highlighted key token followed by its label. Shared by the
/// onboarding wizard and `/catalog` so both screens share an identical footer.
pub fn draw_footer(frame: &mut ratatui::Frame, area: Rect, keys: &[(&str, &str)]) {
    let mut spans: Vec<Span> = Vec::new();
    for (k, label) in keys {
        spans.push(Span::styled(
            format!(" {k} "),
            Style::default()
                .fg(BG)
                .bg(MUTED)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}   "),
            Style::default().fg(DIM),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Left)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(BORDER)),
            ),
        area,
    );
}
