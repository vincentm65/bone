//! Shared primitives for the fullscreen checklist screens (the onboarding
//! wizard and `/catalog`): the palette, the `Item` row model, and the
//! two-column list/detail renderer. Keeping these in one place means both
//! screens look and behave identically.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::ui::theme::Theme;

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
    /// Explicit color for `tag`; `None` uses the active theme accent.
    pub tag_color: Option<Color>,
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
            tag_color: None,
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
    theme: &Theme,
) {
    let p = &theme.palette;
    let area = pad(area);
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
            title,
            Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
        ))),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(p.subtle),
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
                Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
            )));
        }
        if i == cursor {
            selected_row = all_lines.len();
        }
        let selected = i == cursor;
        let cursor_span = Span::styled(
            if selected { " ▸ " } else { "   " },
            Style::default().fg(if selected { p.accent } else { p.subtle }),
        );
        let check_span = Span::styled(
            if item.checked { "[x] " } else { "[ ] " },
            Style::default().fg(if item.checked { p.good } else { p.subtle }),
        );
        let name = item.name.strip_suffix(".lua").unwrap_or(&item.name);
        let name_style = if selected {
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
        } else if item.checked {
            Style::default().fg(p.fg)
        } else {
            Style::default().fg(p.muted)
        };
        let mut spans = vec![
            cursor_span,
            check_span,
            Span::styled(name.to_string(), name_style),
        ];
        if !item.category.is_empty() {
            spans.push(Span::styled(
                format!("  ·{}", item.category),
                Style::default().fg(p.subtle),
            ));
        }
        if let Some(tag) = &item.tag {
            spans.push(Span::styled(
                format!("  {tag}"),
                Style::default()
                    .fg(item.tag_color.unwrap_or(p.accent))
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
    let detail_lines = if let Some(item) = items.get(cursor) {
        let name = item.name.strip_suffix(".lua").unwrap_or(&item.name);
        let mut lines = vec![
            Line::from(Span::styled(
                name.to_string(),
                Style::default().fg(p.fg).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        if item.desc.is_empty() {
            lines.push(Line::from(Span::styled(
                "No description.",
                Style::default().fg(p.subtle),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                item.desc.clone(),
                Style::default().fg(p.muted),
            )));
        }
        if !item.details.is_empty() {
            lines.push(Line::from(""));
            for (label, value) in &item.details {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{label}: "),
                        Style::default().fg(p.subtle).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(value.clone(), Style::default().fg(p.fg)),
                ]));
            }
        }
        if let Some(long_desc) = &item.long_desc {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                long_desc.clone(),
                Style::default().fg(p.muted),
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
                    .border_style(Style::default().fg(p.border))
                    .padding(ratatui::widgets::Padding::horizontal(2)),
            ),
        cols[1],
    );
}

/// Render a one-line key-bindings footer under a top border. Each `(key, label)`
/// pair is shown as a highlighted key token followed by its label. Shared by the
/// onboarding wizard and `/catalog` so both screens share an identical footer.
pub fn draw_footer(frame: &mut ratatui::Frame, area: Rect, keys: &[(&str, &str)], theme: &Theme) {
    let p = &theme.palette;
    let mut spans: Vec<Span> = Vec::new();
    for (k, label) in keys {
        spans.push(Span::styled(
            format!(" {k} "),
            Style::default()
                .fg(p.bg.unwrap_or(p.fg))
                .bg(p.muted)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}   "),
            Style::default().fg(p.subtle),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Left)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(p.border)),
            ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_renderer_uses_semantic_roles_and_preserves_explicit_tag_color() {
        let mut theme = Theme::default();
        theme.palette.fg = Color::Rgb(1, 2, 3);
        theme.palette.muted = Color::Rgb(4, 5, 6);
        theme.palette.subtle = Color::Rgb(7, 8, 9);
        theme.palette.accent = Color::Rgb(10, 11, 12);
        theme.palette.good = Color::Rgb(13, 14, 15);
        theme.palette.border = Color::Rgb(16, 17, 18);
        let tag_color = Color::Rgb(19, 20, 21);

        let mut selected = Item::new("selected.lua".into(), "Summary".into(), true);
        selected.category = "tool";
        selected.tag = Some("custom".into());
        selected.tag_color = Some(tag_color);
        selected.section = Some("Section".into());
        selected.details.push(("Version".into(), "1.0".into()));
        selected.long_desc = Some("Long description".into());
        let inactive = Item::new("inactive.lua".into(), String::new(), false);

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 16)).unwrap();
        terminal
            .draw(|frame| {
                draw_list(
                    frame,
                    frame.area(),
                    "Picker title",
                    "Picker hint",
                    &[selected, inactive],
                    0,
                    &theme,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer.cell((2, 1)).unwrap().fg, theme.palette.fg);
        assert_eq!(buffer.cell((2, 2)).unwrap().fg, theme.palette.subtle);
        assert_eq!(buffer.cell((2, 4)).unwrap().fg, theme.palette.fg);
        assert_eq!(buffer.cell((3, 5)).unwrap().fg, theme.palette.accent);
        assert_eq!(buffer.cell((5, 5)).unwrap().fg, theme.palette.good);
        assert_eq!(buffer.cell((9, 5)).unwrap().fg, theme.palette.accent);
        assert_eq!(buffer.cell((26, 5)).unwrap().fg, tag_color);
        assert_eq!(buffer.cell((5, 6)).unwrap().fg, theme.palette.subtle);
        assert_eq!(buffer.cell((9, 6)).unwrap().fg, theme.palette.muted);
        assert_eq!(buffer.cell((31, 4)).unwrap().fg, theme.palette.border);
        assert_eq!(buffer.cell((34, 4)).unwrap().fg, theme.palette.fg);
        assert_eq!(buffer.cell((34, 6)).unwrap().fg, theme.palette.muted);
        assert_eq!(buffer.cell((34, 8)).unwrap().fg, theme.palette.subtle);
        assert_eq!(buffer.cell((43, 8)).unwrap().fg, theme.palette.fg);
        assert_eq!(buffer.cell((34, 10)).unwrap().fg, theme.palette.muted);
    }
}
