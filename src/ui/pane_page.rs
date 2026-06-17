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
            .saturating_sub(crate::ui::render::clamped_pane_visible_rows(
                self.visible_rows,
            ))
    }

    /// Convert pure-data `PaneContent` into a renderable `PanePage`.
    pub fn from_content(content: &crate::pane_content::PaneContent) -> Self {
        use crate::pane_content::PaneLineSpec;

        let lines: Vec<Line<'static>> = content
            .lines
            .iter()
            .map(|spec| match spec {
                PaneLineSpec::Plain(text) => Line::from(text.clone()),
                PaneLineSpec::Spans { spans } => {
                    let ratatui_spans: Vec<Span<'static>> = spans
                        .iter()
                        .map(|s| {
                            let mut style = Style::default();
                            if let Some(fg) = &s.fg
                                && let Some(c) = crate::ui::color::parse_color(fg)
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
                    if ratatui_spans.is_empty() {
                        Line::from("")
                    } else {
                        Line::from(ratatui_spans)
                    }
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
