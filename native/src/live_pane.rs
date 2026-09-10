//! Conversation-local, tabbed home for native activity and daemon live content.
use std::collections::HashMap;

use bone_protocol::{Component, JobSnapshot, JobStatus, PanePresentation, ViewModel};
use eframe::egui;

use crate::{
    activity, panes,
    task_row::{RowIndicator, task_row},
    theme::Palette,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PageId {
    Agents,
    Extension(String),
}

impl From<&str> for PageId {
    fn from(id: &str) -> Self {
        Self::Extension(id.into())
    }
}

pub(crate) fn page_ids(view: &ViewModel, jobs: &[JobSnapshot]) -> Vec<PageId> {
    let mut ids = Vec::new();
    if !jobs.is_empty() {
        ids.push(PageId::Agents);
    }
    ids.extend(
        view.components
            .iter()
            .filter_map(|component| match component {
                Component::Float {
                    id,
                    presentation: PanePresentation::Live,
                    lines,
                    ..
                } if !lines.is_empty() => Some(PageId::Extension(id.clone())),
                _ => None,
            }),
    );
    ids
}

#[derive(Default)]
pub(crate) struct LivePane {
    pub selected: Option<PageId>,
    pub collapsed: bool,
    // Offset and last daemon scroll, independently retained for each page.
    scroll: HashMap<PageId, (f32, usize)>,
}

impl LivePane {
    pub fn sync(&mut self, ids: &[PageId]) {
        if self.selected.as_ref().is_none_or(|id| !ids.contains(id)) {
            self.selected = ids.first().cloned();
        }
        self.scroll.retain(|id, _| ids.contains(id));
    }

    pub fn select(&mut self, id: PageId) {
        self.selected = Some(id);
        self.collapsed = false;
    }

    pub fn scroll_by(&mut self, rows: i64) {
        if let Some(id) = &self.selected {
            let offset = self.scroll.entry(id.clone()).or_default();
            offset.0 = (offset.0 + rows as f32 * 24.0).max(0.0);
        }
    }

    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        view: &ViewModel,
        jobs: &[JobSnapshot],
        palette: &Palette,
        max_height: f32,
    ) -> Option<String> {
        let ids = page_ids(view, jobs);
        self.sync(&ids);
        if ids.is_empty() || max_height < 32.0 {
            return None;
        }
        let mut open_job = None;
        ui.push_id("live-pane", |ui| {
            ui.set_max_width(ui.available_width());
            ui.horizontal(|ui| {
                if crate::icons::button(
                    ui,
                    if self.collapsed {
                        crate::icons::Icon::ChevronRight
                    } else {
                        crate::icons::Icon::ChevronDown
                    },
                    if self.collapsed {
                        "Expand live pane"
                    } else {
                        "Collapse live pane"
                    },
                )
                .clicked()
                {
                    self.collapsed = !self.collapsed;
                }
                // Many extension pages scroll horizontally rather than widening chat.
                egui::ScrollArea::horizontal()
                    .id_salt("pages")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for id in &ids {
                                let label = match id {
                                    PageId::Agents => format!("Agents ({})", jobs.len()),
                                    PageId::Extension(id) => view
                                        .components
                                        .iter()
                                        .find_map(|c| match c {
                                            Component::Float { id: key, title, .. }
                                                if key == id =>
                                            {
                                                Some(if title.is_empty() {
                                                    id.clone()
                                                } else {
                                                    title.clone()
                                                })
                                            }
                                            _ => None,
                                        })
                                        .unwrap_or_default(),
                                };
                                let response = ui
                                    .push_id(id, |ui| {
                                        ui.add(
                                            egui::Button::selectable(
                                                self.selected.as_ref() == Some(id),
                                                egui::RichText::new(&label).small(),
                                            )
                                            .truncate(),
                                        )
                                    })
                                    .inner
                                    .on_hover_text(&label);
                                if response.clicked() {
                                    self.select(id.clone());
                                }
                            }
                        });
                    });
            });
            if self.collapsed {
                return;
            }
            let Some(selected) = self.selected.clone() else {
                return;
            };
            let component = match &selected {
                PageId::Extension(id) => view.components.iter().find(|c| c.id() == id),
                PageId::Agents => None,
            };
            let (visible_rows, daemon_scroll) = match component {
                Some(Component::Float { rect, scroll, .. }) => {
                    (usize::from(rect.height.max(1)), *scroll)
                }
                _ => (8, 0),
            };
            let offset = self
                .scroll
                .entry(selected.clone())
                .or_insert((0.0, daemon_scroll));
            if offset.1 != daemon_scroll {
                *offset = (0.0, daemon_scroll);
            }
            let content_height =
                (visible_rows.min(8) as f32 * 24.0).min((max_height - 32.0).max(0.0));
            let out = egui::ScrollArea::vertical()
                .id_salt(("content", &selected))
                .max_height(content_height)
                .min_scrolled_height(content_height)
                .auto_shrink([false, true])
                .vertical_scroll_offset(offset.0)
                .show(ui, |ui| match &selected {
                    PageId::Agents => {
                        for job in jobs {
                            ui.push_id(&job.id, |ui| {
                                let (_, fg, accent) = palette.resolved();
                                let muted = crate::theme::resolve_color("muted", palette)
                                    .unwrap_or(fg.gamma_multiply(0.65));
                                let indicator = match job.status {
                                    JobStatus::Running => RowIndicator::Spinner,
                                    JobStatus::Queued => RowIndicator::Queued,
                                };
                                let state = if job.status == JobStatus::Queued {
                                    " · queued"
                                } else {
                                    ""
                                };
                                let label = format!(
                                    "{} · {}{}",
                                    job.agent,
                                    activity::job_label(job),
                                    state
                                );
                                let response = task_row(
                                    ui,
                                    indicator,
                                    accent,
                                    egui::RichText::new(&label).size(14.0).color(fg),
                                    false,
                                );
                                let elapsed = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                                    .saturating_sub(job.started_at);
                                let response = response.on_hover_text(format!(
                                    "{}\n{} · {} · {} tokens\nOpen live transcript",
                                    label,
                                    job.provider,
                                    activity::format_elapsed_ms(elapsed.saturating_mul(1000)),
                                    activity::format_tokens(job.token_sent + job.token_received)
                                ));
                                if response.clicked() {
                                    open_job = Some(job.id.clone());
                                }
                                if let Some(activity) =
                                    job.activity.as_deref().filter(|s| !s.is_empty())
                                {
                                    ui.horizontal(|ui| {
                                        ui.add_space(22.0);
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(
                                                    activity.replace(['\n', '\r'], " "),
                                                )
                                                .small()
                                                .color(muted),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(activity);
                                    });
                                }
                            });
                        }
                    }
                    PageId::Extension(_) => {
                        if let Some(Component::Float { lines, .. }) = component {
                            panes::render_lines(
                                ui,
                                &lines[daemon_scroll.min(lines.len())..],
                                &view.highlights,
                                palette,
                            );
                        }
                    }
                });
            offset.0 = out.state.offset.y;
            ui.add_space(4.0);
        });
        open_job
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bone_protocol::{PaneContent, PaneLineSpec};

    pub(crate) fn job() -> JobSnapshot {
        JobSnapshot {
            id: "job-1".into(),
            agent: "Researcher".into(),
            task: "Inspect task display".into(),
            title: "Inspect task display".into(),
            status: JobStatus::Running,
            started_at: 1,
            token_sent: 100,
            token_received: 20,
            provider: "test".into(),
            activity: Some("Reading the implementation".into()),
            events: vec![],
        }
    }

    fn tasks() -> Component {
        Component::float_from_pane_content(&PaneContent {
            source: "task_list".into(),
            title: "Tasks (1/30)".into(),
            visible_rows: 8,
            scroll: 0,
            lines: (0..30)
                .map(|i| {
                    PaneLineSpec::Plain(format!("Task {i}: {}", "Long task label ".repeat(10)))
                })
                .collect(),
        })
    }

    #[test]
    fn pages_follow_snapshots_without_losing_selection_or_scroll() {
        let mut view = ViewModel {
            components: vec![tasks()],
            ..Default::default()
        };
        let mut jobs = vec![job()];
        let mut pane = LivePane::default();
        let ids = page_ids(&view, &jobs);
        assert_eq!(ids, vec![PageId::Agents, "task_list".into()]);
        pane.sync(&ids);
        pane.select("task_list".into());
        pane.scroll_by(3);
        pane.collapsed = true;
        jobs[0].activity = Some("New progress".into());
        pane.sync(&page_ids(&view, &jobs));
        assert_eq!(pane.selected, Some("task_list".into()));
        assert_eq!(pane.scroll[&"task_list".into()].0, 72.0);
        assert!(pane.collapsed);
        // Removing the selected page selects the remaining native page.
        view.components.clear();
        pane.sync(&page_ids(&view, &jobs));
        assert_eq!(pane.selected, Some(PageId::Agents));
        assert!(pane.scroll.is_empty());
        jobs.clear();
        pane.sync(&page_ids(&view, &jobs));
        assert!(pane.selected.is_none());
        // Legacy/explicit overlays never appear in the dock.
        let mut overlay = tasks();
        if let Component::Float { presentation, .. } = &mut overlay {
            *presentation = PanePresentation::Overlay;
        }
        view.components = vec![overlay];
        assert!(page_ids(&view, &jobs).is_empty());
    }

    #[test]
    fn live_content_stays_bounded_in_narrow_and_scaled_conversations() {
        let view = ViewModel {
            components: vec![tasks()],
            ..Default::default()
        };
        let mut jobs = vec![job(); 15];
        for (i, job) in jobs.iter_mut().enumerate() {
            job.id = format!("job-{i}");
            job.title = "A long agent task title ".repeat(20);
        }
        for width in [240.0, 800.0] {
            for zoom in [1.0, 1.8] {
                for page in [PageId::Agents, "task_list".into()] {
                    let ctx = egui::Context::default();
                    crate::theme::install_fonts(&ctx);
                    ctx.set_zoom_factor(zoom);
                    let mut pane = LivePane::default();
                    pane.select(page);
                    let mut rect = egui::Rect::NOTHING;
                    for _ in 0..3 {
                        ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(width, 600.0),
                                )),
                                ..Default::default()
                            },
                            |ui| {
                                rect = ui
                                    .vertical(|ui| {
                                        pane.render(ui, &view, &jobs, &Palette::default(), 180.0);
                                    })
                                    .response
                                    .rect;
                                assert!(
                                    rect.width() <= ui.max_rect().width() + 1.0,
                                    "width={width} zoom={zoom} rect={rect:?}"
                                );
                            },
                        )
                        .textures_delta
                        .clear();
                    }
                    assert!(
                        rect.height() <= 184.0,
                        "width={width} zoom={zoom} rect={rect:?}"
                    );
                }
            }
        }
    }
}
