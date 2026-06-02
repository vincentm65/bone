use ratatui::text::Line;

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
