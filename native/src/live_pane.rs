//! The live pane: the page region between the input and the status bar,
//! mirroring the TUI's bottom-pane pages. Pages are built by the shared
//! renderer (daemon panes, the approval prompt, agents, processes, the queue,
//! and live reasoning) and painted on the terminal cell grid. One page shows
//! at a time; a newly arrived page becomes active, Tab cycles pages, and
//! PageUp/PageDown scroll.
use std::collections::{HashMap, VecDeque};

use bone_protocol::{Component, JobSnapshot, PaneLineSpec, ProcessSnapshot, ViewModel};
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

/// What a click on a live-pane row opens or answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Open {
    Process(String),
    Job(String),
    /// A daemon pane line's `click` value (a `ui.menu` option).
    Click(String),
    /// An approval choice by index.
    Approval(usize),
}

pub(crate) struct Page {
    pub page: PanePage,
    /// What clicking each content line opens.
    pub targets: Vec<Option<Open>>,
    /// Per content line, tappable spans as (start column, end column, target).
    pub spans: Vec<Vec<(usize, usize, Open)>>,
}

impl Page {
    fn plain(page: PanePage) -> Self {
        Self::with_targets(page, Vec::new())
    }

    fn with_targets(page: PanePage, targets: Vec<Option<Open>>) -> Self {
        Self {
            page,
            targets,
            spans: Vec::new(),
        }
    }

    /// A daemon pane: its lines' and spans' `click` values become targets.
    fn from_content(content: &bone_protocol::PaneContent) -> Self {
        let mut targets = Vec::new();
        let mut spans = Vec::new();
        for line in &content.lines {
            let PaneLineSpec::Spans {
                spans: line_spans,
                click,
                ..
            } = line
            else {
                targets.push(None);
                spans.push(Vec::new());
                continue;
            };
            targets.push(click.clone().map(Open::Click));
            let mut column = 0;
            let mut tappable = Vec::new();
            for span in line_spans {
                let width = unicode_width::UnicodeWidthStr::width(span.text.as_str());
                if let Some(value) = &span.click {
                    tappable.push((column, column + width, Open::Click(value.clone())));
                }
                column += width;
            }
            spans.push(tappable);
        }
        Self {
            page: PanePage::from_content(content),
            targets,
            spans,
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
    /// The approval prompt's lines when a tool call awaits a decision, and how
    /// many trailing lines are its choices.
    pub approval: Option<(Vec<Line<'static>>, usize)>,
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
    /// Selection is highlighted only while its list has keyboard focus.
    pub job_focused: bool,
    pub process_focused: bool,
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
            .map(|content| Page::from_content(&content))
            .collect();
        let selected_job = if self.job_focused {
            self.job.as_deref()
        } else {
            None
        };
        if let Some(page) = panes::jobs::render_selected(theme, sources.jobs, selected_job) {
            let targets = sources
                .jobs
                .iter()
                .map(|job| Some(Open::Job(job.id.clone())))
                .collect();
            pages.push(Page::with_targets(page, targets));
        }
        let selected_process = if self.process_focused {
            self.process.as_deref()
        } else {
            None
        };
        if let Some(page) = panes::processes::render(theme, sources.processes, selected_process) {
            let targets = sources
                .processes
                .iter()
                .map(|process| Some(Open::Process(process.id.clone())))
                .collect();
            pages.push(Page::with_targets(page, targets));
        }
        if let Some(page) = panes::queue::render(sources.queue, self.queue, theme) {
            pages.push(Page::plain(page));
        }
        if let Some(text) = sources.thinking.filter(|text| !text.trim().is_empty()) {
            pages.push(Page::plain(panes::thinking(text, THINKING_ROWS, theme)));
        }
        if let Some((content, choices)) = sources.approval {
            let visible_rows = content.len();
            let first_choice = content.len().saturating_sub(choices);
            let targets = (0..content.len())
                .map(|row| (row >= first_choice).then(|| Open::Approval(row - first_choice)))
                .collect();
            pages.push(Page::with_targets(
                PanePage {
                    source: "approval".into(),
                    title: "approval".into(),
                    content,
                    visible_rows,
                    scroll: 0,
                },
                targets,
            ));
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
        self.known = ids.clone();
        reconcile(&mut self.job, jobs.iter().map(|job| &job.id));
        reconcile(
            &mut self.process,
            processes.iter().map(|process| &process.id),
        );
        if !ids.iter().any(|id| id == AGENTS) {
            self.job = None;
            self.job_focused = false;
        }
        if !ids.iter().any(|id| id == PROCESSES) {
            self.process = None;
            self.process_focused = false;
        }
    }

    /// Clear keyboard focus while retaining the last row selections.
    pub fn clear_focus(&mut self) {
        self.job_focused = false;
        self.process_focused = false;
    }

    /// Reset transient pane state when attaching to a new conversation/daemon.
    pub fn reset_for_attach(&mut self) {
        self.active = None;
        self.known.clear();
        self.scroll.clear();
        self.job = None;
        self.process = None;
        self.queue = 0;
        self.clear_focus();
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
        let cell = grid::metrics(ui).cell;
        let mut open = None;
        for (offset, rect) in rects.into_iter().enumerate() {
            let row = start + offset;
            let line_target = page.targets.get(row).cloned().flatten();
            let spans = page.spans.get(row).map(Vec::as_slice).unwrap_or_default();
            if line_target.is_none() && spans.is_empty() {
                continue;
            }
            let response = ui
                .interact(rect, ui.id().with(("live-row", row)), egui::Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.clicked() {
                // A tapped span (one tab on a tab row) wins over its line.
                let column = response
                    .interact_pointer_pos()
                    .map(|pos| ((pos.x - rect.left()) / cell).max(0.0) as usize);
                let span = column.and_then(|column| {
                    spans
                        .iter()
                        .find(|(from, to, _)| (*from..*to).contains(&column))
                        .map(|(_, _, target)| target.clone())
                });
                open = span.or(line_target);
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

    #[test]
    fn menu_lines_and_approval_choices_are_tap_targets() {
        let menu = Component::Float {
            id: "menu".into(),
            presentation: Default::default(),
            title: "menu".into(),
            lines: vec![
                PaneLineSpec::Plain("Pick one".into()),
                PaneLineSpec::Spans {
                    spans: Vec::new(),
                    bg: None,
                    click: Some("1".into()),
                },
            ],
            rect: bone_protocol::FloatRect {
                anchor: Default::default(),
                width: 0,
                height: 4,
                col: 0,
                row: 0,
            },
            z: 0,
            border: false,
            scroll: 0,
            placement: None,
            owner: None,
        };
        let view = ViewModel {
            components: vec![menu],
            ..Default::default()
        };
        let approval: Vec<Line<'static>> = ["title", "Approve", "Advise", "Deny"]
            .map(Line::from)
            .to_vec();
        let pages = LivePane::default().pages(
            Sources {
                view: &view,
                jobs: &[],
                processes: &[],
                thinking: None,
                queue: &VecDeque::new(),
                approval: Some((approval, 3)),
            },
            &Theme::default(),
        );
        assert_eq!(pages[0].targets, [None, Some(Open::Click("1".into()))]);
        assert_eq!(
            pages[1].targets,
            [
                None,
                Some(Open::Approval(0)),
                Some(Open::Approval(1)),
                Some(Open::Approval(2))
            ]
        );
    }

    #[test]
    fn tappable_spans_record_their_columns() {
        let span = |text: &str, click: Option<&str>| bone_protocol::PaneSpanSpec {
            text: text.into(),
            fg: None,
            modifiers: Vec::new(),
            click: click.map(Into::into),
        };
        let page = Page::from_content(&bone_protocol::PaneContent {
            source: "config".into(),
            title: "Config".into(),
            lines: vec![PaneLineSpec::Spans {
                spans: vec![
                    span("  ", None),
                    span("UI", Some("tab:1")),
                    span("  │  ", None),
                    span("Tools", Some("tab:2")),
                ],
                bg: None,
                click: None,
            }],
            visible_rows: 1,
            scroll: 0,
            placement: None,
            owner: None,
        });
        assert_eq!(page.targets, [None]);
        assert_eq!(
            page.spans[0],
            [
                (2, 4, Open::Click("tab:1".into())),
                (9, 14, Open::Click("tab:2".into()))
            ]
        );
    }
}
