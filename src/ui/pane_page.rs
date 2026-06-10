use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// A single page of tool-provided content rendered in the bottom pane.
#[derive(Clone, Debug)]
pub struct PanePage {
    /// Unique source identifier, e.g. "build_status".
    /// Tools update their existing page or insert a new one via upsert.
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
    /// Parse a pane definition from a serde_json::Value.
    pub fn from_json(val: &serde_json::Value) -> Result<Self, String> {
        let pane = val.as_object().ok_or("pane must be an object")?;
        let source = pane
            .get("source")
            .and_then(|v| v.as_str())
            .ok_or("pane missing source")?
            .to_string();
        let title = pane
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or("pane missing title")?
            .to_string();
        let visible_rows = pane
            .get("visible_rows")
            .and_then(|v| v.as_u64())
            .unwrap_or(8) as usize;
        let scroll = pane.get("scroll").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        let lines: Vec<Line<'static>> = pane
            .get("lines")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|line_val| {
                        if let Some(text) = line_val.as_str() {
                            Some(Line::from(text.to_string()))
                        } else if let Some(obj) = line_val.as_object() {
                            let spans: Vec<Span<'static>> = obj
                                .get("spans")
                                .and_then(|v| v.as_array())
                                .map(|spans_arr| {
                                    spans_arr
                                        .iter()
                                        .filter_map(|span_val| {
                                            let span_obj = span_val.as_object()?;
                                            let text = span_obj
                                                .get("text")
                                                .and_then(|v| v.as_str())?
                                                .to_string();
                                            let mut style = Style::default();
                                            if let Some(fg) =
                                                span_obj.get("fg").and_then(|v| v.as_str())
                                                && let Some(c) =
                                                    crate::ext::color::parse_color(fg)
                                            {
                                                style = style.fg(c);
                                            }
                                            if let Some(mods) =
                                                span_obj.get("modifiers").and_then(|v| v.as_array())
                                            {
                                                for m in mods {
                                                    if let Some(s) = m.as_str() {
                                                        match s {
                                                            "bold" => {
                                                                style = style
                                                                    .add_modifier(Modifier::BOLD)
                                                            }
                                                            "dim" => {
                                                                style = style
                                                                    .add_modifier(Modifier::DIM)
                                                            }
                                                            "italic" => {
                                                                style = style
                                                                    .add_modifier(Modifier::ITALIC)
                                                            }
                                                            "strike" | "crossed_out" => {
                                                                style = style.add_modifier(
                                                                    Modifier::CROSSED_OUT,
                                                                )
                                                            }
                                                            _ => {}
                                                        }
                                                    }
                                                }
                                            }
                                            Some(Span::styled(text, style))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            if spans.is_empty() {
                                Some(Line::from(""))
                            } else {
                                Some(Line::from(spans))
                            }
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(PanePage {
            source,
            title,
            content: lines,
            visible_rows,
            scroll,
        })
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
