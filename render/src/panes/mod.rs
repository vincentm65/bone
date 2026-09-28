//! Live-pane pages shared by every frontend: daemon-provided content and the
//! native agents, processes, and queue lists, as styled terminal lines.

pub mod jobs;
pub mod processes;
pub mod queue;

/// Rows shown when a page does not request a height.
pub const DEFAULT_PANE_ROWS: usize = 8;
/// Upper safety cap for rows of pane page content visible at once.
pub const MAX_PANE_ROWS: usize = 24;
/// Rows a selectable list page shows.
pub const SELECTABLE_ROWS: usize = 8;

pub fn clamped_pane_visible_rows(visible_rows: usize) -> usize {
    visible_rows.clamp(1, MAX_PANE_ROWS)
}

use crate::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// A single page of tool-provided content rendered in the bottom pane.
#[derive(Clone, Debug)]
pub struct PanePage {
    /// Unique source identifier, e.g. "build_status".
    pub source: String,
    /// Short label shown in the tab indicator, e.g. "jobs (3)".
    pub title: String,
    /// The content lines to render.
    pub content: Vec<Line<'static>>,
    /// Maximum content rows this page wants visible at once.
    pub visible_rows: usize,
    /// Scroll offset for this page (rows into content).
    pub scroll: usize,
}

impl PanePage {
    /// Maximum scroll offset: rows of content beyond what fits in the pane.
    pub fn max_scroll(&self) -> usize {
        self.content
            .len()
            .saturating_sub(clamped_pane_visible_rows(self.visible_rows))
    }

    /// Convert pure-data `PaneContent` into a renderable `PanePage`.
    pub fn from_content(content: &bone_protocol::PaneContent) -> Self {
        use bone_protocol::PaneLineSpec;

        let lines: Vec<Line<'static>> = content
            .lines
            .iter()
            .map(|spec| match spec {
                PaneLineSpec::Plain(text) => Line::from(text.clone()),
                PaneLineSpec::Spans { spans, bg, .. } => {
                    let ratatui_spans: Vec<Span<'static>> = spans
                        .iter()
                        .map(|s| {
                            let mut style = Style::default();
                            if let Some(fg) = &s.fg
                                && let Some(c) = crate::color::parse_color(fg)
                            {
                                style = style.fg(c);
                            }
                            for m in &s.modifiers {
                                match m.as_str() {
                                    "bold" => style = style.add_modifier(Modifier::BOLD),
                                    "dim" => style = style.add_modifier(Modifier::DIM),
                                    "italic" => style = style.add_modifier(Modifier::ITALIC),
                                    "strike" | "crossed_out" => {
                                        style = style.add_modifier(Modifier::CROSSED_OUT);
                                    }
                                    _ => {}
                                }
                            }
                            Span::styled(s.text.clone(), style)
                        })
                        .collect();
                    let mut line = if ratatui_spans.is_empty() {
                        Line::from("")
                    } else {
                        Line::from(ratatui_spans)
                    };
                    if let Some(bg) = bg
                        && let Some(c) = crate::color::parse_color(bg)
                    {
                        line = line.style(Style::default().bg(c));
                    }
                    line
                }
            })
            .collect();

        PanePage {
            source: content.source.clone(),
            title: content.title.clone(),
            content: lines,
            visible_rows: content.visible_rows,
            scroll: content.scroll,
        }
    }

    /// Remove a page by source name. Returns the new active_page index.
    pub fn remove(pages: &mut Vec<PanePage>, source: &str, active_page: usize) -> usize {
        if let Some(pos) = pages.iter().position(|p| p.source == source) {
            pages.remove(pos);
            if pages.is_empty() {
                0
            } else if active_page >= pages.len() {
                pages.len() - 1
            } else if pos < active_page {
                active_page - 1
            } else {
                active_page
            }
        } else {
            active_page
        }
    }

    /// Upsert a page. If a page with the same source exists, replace it.
    /// Returns the index of the page and the new active_page.
    pub fn upsert(pages: &mut Vec<PanePage>, active_page: usize, page: PanePage) -> (usize, usize) {
        if let Some(pos) = pages.iter().position(|p| p.source == page.source) {
            pages[pos] = page;
            (pos, active_page)
        } else {
            pages.push(page);
            let idx = pages.len() - 1;
            (idx, idx)
        }
    }
}

/// The live reasoning page: a pinned header and the latest `max_rows - 1`
/// lines of the reasoning tail.
pub fn thinking(tail: &str, max_rows: usize, theme: &Theme) -> PanePage {
    let mut lines: Vec<&str> = tail.rsplit('\n').take(max_rows - 1).collect();
    lines.reverse();
    thinking_page(lines.into_iter().map(str::to_string), 0, theme)
}

/// A "✻ Thinking" header over `body`, padded with blank rows to `min_rows`.
fn thinking_page(body: impl Iterator<Item = String>, min_rows: usize, theme: &Theme) -> PanePage {
    let header_style = Style::default()
        .fg(theme.thinking)
        .add_modifier(Modifier::BOLD);
    let body_style = Style::default().fg(theme.palette.muted);
    let mut content = vec![Line::styled("✻ Thinking", header_style)];
    content.extend(body.map(|row| Line::styled(row, body_style)));
    if content.len() < min_rows {
        content.resize_with(min_rows, || Line::from(""));
    }
    let visible_rows = content.len();
    PanePage {
        source: "thinking".to_string(),
        title: "thinking".to_string(),
        content,
        visible_rows,
        scroll: 0,
    }
}

/// Bytes of reasoning tail considered when building the fixed thinking page.
const THINKING_WINDOW: usize = 8192;

/// The live reasoning page at a constant height: a pinned header plus exactly
/// `max_rows - 1` body rows. The tail is hard-wrapped to `cols` cells and the
/// newest rows are kept, so streamed text scrolls inside a fixed-size box
/// instead of growing it; unused rows are blank padding below the text.
pub fn thinking_fixed(tail: &str, cols: usize, max_rows: usize, theme: &Theme) -> PanePage {
    use unicode_width::UnicodeWidthChar;

    let cols = cols.max(1);
    let max_rows = max_rows.max(2);
    let mut start = tail.len().saturating_sub(THINKING_WINDOW);
    while !tail.is_char_boundary(start) {
        start += 1;
    }
    let mut rows: Vec<String> = Vec::new();
    for line in tail[start..].trim_end().split('\n') {
        let mut current = String::new();
        let mut width = 0;
        for ch in line.chars().filter(|ch| *ch != '\r') {
            let w = ch.width().unwrap_or(0);
            if width + w > cols && !current.is_empty() {
                rows.push(std::mem::take(&mut current));
                width = 0;
            }
            current.push(ch);
            width += w;
        }
        rows.push(current);
    }
    let body_rows = max_rows - 1;
    let skip = rows.len().saturating_sub(body_rows);

    thinking_page(rows.into_iter().skip(skip), max_rows, theme)
}

/// A list page whose selected row is marked and highlighted.
pub fn selectable(
    theme: &Theme,
    source: &str,
    title: String,
    rows: Vec<(bool, Line<'static>)>,
) -> PanePage {
    let selected_index = rows.iter().position(|(selected, _)| *selected).unwrap_or(0);
    let content = rows
        .into_iter()
        .map(|(selected, mut line)| {
            line.spans.insert(
                0,
                Span::styled(
                    if selected { " › " } else { "   " },
                    Style::default().fg(if selected {
                        theme.palette.accent
                    } else {
                        theme.palette.muted
                    }),
                ),
            );
            if selected {
                line = line.style(Style::default().bg(theme.palette.selection));
            }
            line
        })
        .collect();

    PanePage {
        source: source.into(),
        title,
        content,
        visible_rows: SELECTABLE_ROWS,
        scroll: selected_index.saturating_sub(SELECTABLE_ROWS.saturating_sub(1)),
    }
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod pane_page_tests;
