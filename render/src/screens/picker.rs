//! Shared primitives for the fullscreen checklist screens (the onboarding
//! wizard and `/catalog`): the palette, the `Item` row model, and the
//! two-column list/detail renderer. Keeping these in one place means both
//! screens look and behave identically.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::theme::Theme;

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

/// Map a visible checklist line back to its item index.
///
/// The renderer expands section headings into their own lines before applying
/// the same cursor-centred window. A heading is intentionally treated as a hit
/// for the item immediately below it, so a section label remains a useful
/// touch target without adding a new selection path.
pub fn item_at_visible_row(
    items: &[Item],
    cursor: usize,
    height: usize,
    visible_row: usize,
) -> Option<usize> {
    if visible_row >= height || items.is_empty() {
        return None;
    }

    let mut total = 0usize;
    let mut selected_row = 0usize;
    for (index, item) in items.iter().enumerate() {
        if item.section.is_some() {
            total += 1;
        }
        if index == cursor {
            selected_row = total;
        }
        total += 1;
    }

    let start = if total <= height {
        0
    } else {
        selected_row.saturating_sub(height / 2).min(total - height)
    };
    let line = start + visible_row;

    let mut line_index = 0usize;
    for (index, item) in items.iter().enumerate() {
        if item.section.is_some() {
            if line == line_index {
                return Some(index);
            }
            line_index += 1;
        }
        if line == line_index {
            return Some(index);
        }
        line_index += 1;
    }
    None
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
            fit_width(hint, area.width as usize),
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

/// Fit `text` in `width` terminal columns, ending with `…` when it had to be
/// cut, so a one-row line never stops silently mid-word.
pub(crate) fn fit_width(text: &str, width: usize) -> std::borrow::Cow<'_, str> {
    if unicode_width::UnicodeWidthStr::width(text) <= width {
        return std::borrow::Cow::Borrowed(text);
    }
    if width == 0 {
        return std::borrow::Cow::Borrowed("");
    }
    std::borrow::Cow::Owned(format!(
        "{}…",
        crate::messages::truncate_to_display_width(text, width - 1)
    ))
}

#[cfg(test)]
mod fit_width_tests {
    use super::fit_width;

    #[test]
    fn long_lines_end_with_an_ellipsis_within_the_width() {
        assert_eq!(fit_width("short", 10), "short");
        assert_eq!(fit_width("exactly10!", 10), "exactly10!");
        assert_eq!(fit_width("a longer hint line", 8), "a longe…");
        assert_eq!(fit_width("日本語テキスト", 5), "日本…");
        assert_eq!(fit_width("anything", 0), "");
    }
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

/// Return the footer token/label span containing a terminal column.
///
/// The ranges mirror [`draw_footer`], including its padding, so callers can
/// make the existing hint itself the touch target without changing its paint.
pub fn footer_hit(col: u16, keys: &[(&str, &str)]) -> Option<(usize, u16, u16)> {
    let mut start = 0u16;
    for (index, (key, label)) in keys.iter().enumerate() {
        let width = (key.chars().count() + label.chars().count() + 6) as u16;
        let end = start.saturating_add(width);
        if (start..end).contains(&col) {
            return Some((index, start, end));
        }
        start = end;
    }
    None
}

/// Whether `col` falls in the item pane of the 1:2 list/detail split drawn over `list`.
pub fn left_pane_hit(list: Rect, col: u16) -> bool {
    let pane = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 3), Constraint::Ratio(2, 3)])
        .split(list)[0];
    (pane.x..pane.x.saturating_add(pane.width)).contains(&col)
}

/// Whether `col` is in the first half of a footer hint spanning `start..end`,
/// for hints such as `a/n` that carry two actions.
pub fn first_half(col: u16, start: u16, end: u16) -> bool {
    col < start.saturating_add(end.saturating_sub(start) / 2)
}

#[cfg(test)]
#[path = "picker_tests.rs"]
mod tests;
