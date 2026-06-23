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
                PaneLineSpec::Spans { spans, bg } => {
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
                    let mut line = if ratatui_spans.is_empty() {
                        Line::from("")
                    } else {
                        Line::from(ratatui_spans)
                    };
                    if let Some(bg) = bg
                        && let Some(c) = crate::ui::color::parse_color(bg)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn page(source: &str) -> PanePage {
        PanePage {
            source: source.to_string(),
            title: source.to_string(),
            content: Vec::new(),
            visible_rows: 1,
            scroll: 0,
        }
    }

    fn pages(sources: &[&str]) -> Vec<PanePage> {
        sources.iter().map(|s| page(s)).collect()
    }

    #[test]
    fn remove_clamps_active_when_active_page_was_last() {
        // Active page points at the page being removed (the last one); after
        // removal active_page must fall back to the new last index, not dangle
        // past the end (the out-of-bounds panic this guards against).
        let mut p = pages(&["interact", "subagent"]);
        let active = PanePage::remove(&mut p, "subagent", 1);
        assert_eq!(p.len(), 1);
        assert_eq!(active, 0);
    }

    #[test]
    fn remove_shifts_active_when_lower_page_removed() {
        // Removing a page below the active one shifts active_page down by one
        // so it keeps pointing at the same logical page.
        let mut p = pages(&["subagent", "interact"]);
        let active = PanePage::remove(&mut p, "subagent", 1);
        assert_eq!(p.len(), 1);
        assert_eq!(active, 0);
    }

    #[test]
    fn remove_leaves_active_when_higher_page_removed() {
        let mut p = pages(&["interact", "subagent"]);
        let active = PanePage::remove(&mut p, "subagent", 0);
        assert_eq!(active, 0);
    }

    #[test]
    fn remove_resets_active_when_emptied() {
        let mut p = pages(&["interact"]);
        let active = PanePage::remove(&mut p, "interact", 0);
        assert!(p.is_empty());
        assert_eq!(active, 0);
    }

    #[test]
    fn remove_missing_source_is_noop() {
        let mut p = pages(&["interact", "subagent"]);
        let active = PanePage::remove(&mut p, "nope", 1);
        assert_eq!(p.len(), 2);
        assert_eq!(active, 1);
    }
}
