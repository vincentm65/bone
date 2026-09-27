//! The live pane: the page region between the input and the status bar,
//! mirroring the TUI's bottom-pane pages. Pages are built by the shared
//! renderer (daemon panes, the approval prompt, agents, processes, the queue,
//! and live reasoning) and painted on the terminal cell grid. One page shows
//! at a time; a newly arrived page becomes active, Tab cycles pages, and
//! PageUp/PageDown scroll.
use std::collections::{HashMap, VecDeque};

use bone_protocol::{Component, JobSnapshot, ProcessSnapshot, ViewModel};
use bone_render::panes::{self, PanePage};
use bone_render::theme::Theme;
use eframe::egui;
use ratatui::text::Line;

use crate::grid;

pub use bone_render::panes::DEFAULT_PANE_ROWS;

const AGENTS: &str = "jobs";
const PROCESSES: &str = "processes";
const QUEUE: &str = "queue";
const THINKING_ROWS: usize = 10;

/// What a click on a live-pane row opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Open {
    Process(String),
    Job(String),
}

pub(crate) struct Page {
    pub page: PanePage,
    /// What clicking each content line opens.
    pub targets: Vec<Option<Open>>,
}

impl Page {
    fn plain(page: PanePage) -> Self {
        Self {
            page,
            targets: Vec::new(),
        }
    }
}

/// Everything the live pane shows for one chat.
pub(crate) struct Sources<'a> {
    pub view: &'a ViewModel,
    pub jobs: &'a [JobSnapshot],
    pub processes: &'a [ProcessSnapshot],
    pub thinking: Option<&'a str>,
    pub queue: &'a VecDeque<String>,
    /// The approval prompt's lines when a tool call awaits a decision.
    pub approval: Option<Vec<Line<'static>>>,
}

#[derive(Default)]
pub(crate) struct LivePane {
    pub active: Option<String>,
    /// Page ids seen last frame, so a newly arrived page can take focus.
    known: Vec<String>,
    /// Frontend scroll offset (rows) added to each page's own scroll.
    scroll: HashMap<String, i64>,
    /// Selected agent and process ids and queue index, like the TUI panes.
    pub job: Option<String>,
    pub process: Option<String>,
    pub queue: usize,
}

impl LivePane {
    pub fn pages(&self, sources: Sources<'_>, theme: &Theme) -> Vec<Page> {
        let mut pages: Vec<Page> = sources
            .view
            .components
            .iter()
            .filter_map(Component::as_pane_content)
            .filter(|content| !content.lines.is_empty())
            .map(|content| Page::plain(PanePage::from_content(&content)))
            .collect();
        if let Some(page) = panes::jobs::render_selected(theme, sources.jobs, self.job.as_deref()) {
            let targets = sources
                .jobs
                .iter()
                .map(|job| Some(Open::Job(job.id.clone())))
                .collect();
            pages.push(Page { page, targets });
        }
        if let Some(page) =
            panes::processes::render(theme, sources.processes, self.process.as_deref())
        {
            let targets = sources
                .processes
                .iter()
                .map(|process| Some(Open::Process(process.id.clone())))
                .collect();
            pages.push(Page { page, targets });
        }
        if let Some(page) = panes::queue::render(sources.queue, self.queue, theme) {
            pages.push(Page::plain(page));
        }
        if let Some(text) = sources.thinking.filter(|text| !text.trim().is_empty()) {
            pages.push(Page::plain(panes::thinking(text, THINKING_ROWS, theme)));
        }
        if let Some(content) = sources.approval {
            let visible_rows = content.len();
            pages.push(Page::plain(PanePage {
                source: "approval".into(),
                title: "approval".into(),
                content,
                visible_rows,
                scroll: 0,
            }));
        }
        pages
    }

    pub fn sync(&mut self, pages: &[Page], jobs: &[JobSnapshot], processes: &[ProcessSnapshot]) {
        let ids: Vec<String> = pages.iter().map(|page| page.page.source.clone()).collect();
        if let Some(new) = ids.iter().rev().find(|id| !self.known.contains(id)) {
            self.active = Some(new.clone());
        }
        if self.active.as_ref().is_none_or(|id| !ids.contains(id)) {
            self.active = ids.first().cloned();
        }
        self.scroll.retain(|id, _| ids.contains(id));
        self.known = ids;
        reconcile(&mut self.job, jobs.iter().map(|job| &job.id));
        reconcile(
            &mut self.process,
            processes.iter().map(|process| &process.id),
        );
    }

    pub fn cycle(&mut self) {
        if self.known.is_empty() {
            return;
        }
        let index = self
            .active
            .as_ref()
            .and_then(|id| self.known.iter().position(|known| known == id))
            .map_or(0, |index| (index + 1) % self.known.len());
        self.active = Some(self.known[index].clone());
    }

    pub fn scroll_by(&mut self, rows: i64) {
        if let Some(id) = &self.active {
            *self.scroll.entry(id.clone()).or_default() += rows;
        }
    }

    pub fn has_pages(&self) -> bool {
        !self.known.is_empty()
    }

    /// The active page when it is one of the selectable native lists.
    pub fn active_list(&self) -> Option<&str> {
        self.active
            .as_deref()
            .filter(|id| [AGENTS, PROCESSES, QUEUE].contains(id))
    }

    /// Draw the active page. Returns what a clicked row opens.
    pub fn show(&mut self, ui: &mut egui::Ui, pages: &[Page]) -> Option<Open> {
        let index = self
            .active
            .as_ref()
            .and_then(|id| pages.iter().position(|page| &page.page.source == id))?;
        let page = &pages[index];
        if pages.len() > 1 {
            ui.label(
                egui::RichText::new(format!(
                    "{}  [{}/{}]  Tab to switch",
                    page.page.title,
                    index + 1,
                    pages.len()
                ))
                .weak(),
            );
        }
        let content = &page.page.content;
        let rows = panes::clamped_pane_visible_rows(page.page.visible_rows);
        let client = self.scroll.get(&page.page.source).copied().unwrap_or(0);
        let max_start = content.len().saturating_sub(rows);
        let start = ((page.page.scroll as i64 + client).max(0) as usize).min(max_start);
        self.scroll.insert(
            page.page.source.clone(),
            start as i64 - page.page.scroll as i64,
        );
        let end = (start + rows).min(content.len());
        let rects = grid::paint_lines(ui, &content[start..end]);
        let mut open = None;
        for (offset, rect) in rects.into_iter().enumerate() {
            let Some(Some(target)) = page.targets.get(start + offset) else {
                continue;
            };
            let response = ui
                .interact(
                    rect,
                    ui.id().with(("live-row", start + offset)),
                    egui::Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.clicked() {
                open = Some(target.clone());
            }
        }
        open
    }
}

/// Keep a list selection on an existing id, falling back to the first.
fn reconcile<'a>(selected: &mut Option<String>, ids: impl Iterator<Item = &'a String>) {
    let ids: Vec<&String> = ids.collect();
    if !selected.as_ref().is_some_and(|id| ids.contains(&id)) {
        *selected = ids.first().map(|id| (*id).clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_protocol::{PaneContent, PaneLineSpec};

    fn float(id: &str, rows: usize) -> Component {
        Component::float_from_pane_content(&PaneContent {
            source: id.into(),
            title: id.into(),
            visible_rows: 4,
            scroll: 0,
            placement: None,
            owner: None,
            lines: (0..rows)
                .map(|i| PaneLineSpec::Plain(format!("{id} {i}")))
                .collect(),
        })
    }

    fn pages(pane: &LivePane, view: &ViewModel, queue: &VecDeque<String>) -> Vec<Page> {
        pane.pages(
            Sources {
                view,
                jobs: &[],
                processes: &[],
                thinking: None,
                queue,
                approval: None,
            },
            &Theme::default(),
        )
    }

    #[test]
    fn new_pages_take_focus_and_tab_cycles() {
        let queue = VecDeque::new();
        let mut view = ViewModel {
            components: vec![float("tasks", 3)],
            ..Default::default()
        };
        let mut pane = LivePane::default();
        let built = pages(&pane, &view, &queue);
        pane.sync(&built, &[], &[]);
        assert_eq!(pane.active.as_deref(), Some("tasks"));
        view.components.push(float("config", 5));
        let built = pages(&pane, &view, &queue);
        pane.sync(&built, &[], &[]);
        assert_eq!(pane.active.as_deref(), Some("config"));
        pane.cycle();
        assert_eq!(pane.active.as_deref(), Some("tasks"));
        view.components.clear();
        let built = pages(&pane, &view, &queue);
        pane.sync(&built, &[], &[]);
        assert!(pane.active.is_none() && !pane.has_pages());
    }

    #[test]
    fn queued_prompts_are_a_selectable_list_page() {
        let queue: VecDeque<String> = ["first".to_string(), "second".to_string()].into();
        let mut pane = LivePane::default();
        let built = pages(&pane, &ViewModel::default(), &queue);
        pane.sync(&built, &[], &[]);
        assert_eq!(pane.active_list(), Some(QUEUE));
    }
}
