//! Conversation-local flat home for native activity and daemon live content.
use std::collections::HashMap;

use bone_protocol::{
    Component, JobSnapshot, JobStatus, PanePresentation, ProcessSnapshot, ProcessState, ViewModel,
};
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

const MIN_PANE_HEIGHT: f32 = 96.0;
const COLLAPSED_PANE_HEIGHT: f32 = 48.0;
const HEADER_HEIGHT: f32 = 30.0;

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

fn page_line_count(id: &PageId, view: &ViewModel, jobs: &[JobSnapshot]) -> usize {
    match id {
        PageId::Agents => jobs.len(),
        PageId::Extension(id) => view
            .components
            .iter()
            .find(|component| component.id() == id)
            .and_then(|component| match component {
                Component::Float { lines, .. } => Some(lines.len()),
                _ => None,
            })
            .unwrap_or_default(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LiveAction {
    OpenJob(String),
    OpenProcess(String),
}

pub(crate) struct LivePane {
    pub selected: Option<PageId>,
    pub inspecting: bool,
    // Offset and last daemon scroll, independently retained for each page.
    scroll: HashMap<PageId, (f32, usize)>,
    // Follow/new-output state is kept per page so switching tabs does not lose
    // the user's position in another live stream.
    follow: HashMap<PageId, bool>,
    line_counts: HashMap<PageId, usize>,
    new_output: HashMap<PageId, usize>,
    // The divider is intentionally conversation-local, like the selected page.
    height: Option<f32>,
}

impl Default for LivePane {
    fn default() -> Self {
        Self {
            selected: None,
            inspecting: false,
            scroll: HashMap::new(),
            follow: HashMap::new(),
            line_counts: HashMap::new(),
            new_output: HashMap::new(),
            height: None,
        }
    }
}

impl LivePane {
    pub fn sync(&mut self, ids: &[PageId]) {
        if self.selected.as_ref().is_none_or(|id| !ids.contains(id)) {
            self.selected = ids.first().cloned();
        }
        self.scroll.retain(|id, _| ids.contains(id));
        self.follow.retain(|id, _| ids.contains(id));
        self.line_counts.retain(|id, _| ids.contains(id));
        self.new_output.retain(|id, _| ids.contains(id));
    }

    pub fn select(&mut self, id: PageId) {
        self.selected = Some(id);
    }

    pub fn scroll_by(&mut self, rows: i64) {
        if let Some(id) = &self.selected {
            let offset = self.scroll.entry(id.clone()).or_default();
            offset.0 = (offset.0 + rows as f32 * 24.0).max(0.0);
            if rows != 0 {
                self.follow.insert(id.clone(), false);
            }
        }
    }

    #[cfg(test)]
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        view: &ViewModel,
        jobs: &[JobSnapshot],
        processes: &[ProcessSnapshot],
        palette: &Palette,
        max_height: f32,
    ) -> Option<LiveAction> {
        self.render_with_bounds(ui, view, jobs, processes, palette, max_height, max_height)
    }

    pub fn render_with_bounds(
        &mut self,
        ui: &mut egui::Ui,
        view: &ViewModel,
        jobs: &[JobSnapshot],
        processes: &[ProcessSnapshot],
        palette: &Palette,
        default_height: f32,
        max_height: f32,
    ) -> Option<LiveAction> {
        let ids = page_ids(view, jobs);
        self.sync(&ids);
        let running_commands = processes
            .iter()
            .filter(|process| process.state == ProcessState::Running)
            .count();
        if (ids.is_empty() && running_commands == 0) || max_height < COLLAPSED_PANE_HEIGHT {
            return None;
        }

        // Snapshot counts are tracked for every page, including inactive tabs, so
        // switching pages never loses the amount of output that arrived while it
        // was out of view.
        for id in &ids {
            let count = page_line_count(id, view, jobs);
            let previous = self.line_counts.insert(id.clone(), count).unwrap_or(count);
            if count > previous {
                let delta = count - previous;
                if !self.follow.get(id).copied().unwrap_or(true) {
                    *self.new_output.entry(id.clone()).or_default() += delta;
                } else {
                    self.new_output.insert(id.clone(), 0);
                }
            }
        }

        let (_, fg, accent) = palette.resolved();
        let max_height = max_height.max(COLLAPSED_PANE_HEIGHT);
        let expanded_height = self
            .height
            .unwrap_or(default_height)
            .clamp(MIN_PANE_HEIGHT.min(max_height), max_height);
        let pane_height = if self.inspecting {
            expanded_height
        } else {
            COLLAPSED_PANE_HEIGHT.min(max_height)
        };
        let mut open_job = None;
        let mut open_process = None;
        let mut jump_to_latest = false;

        ui.push_id("live-pane", |ui| {
            ui.set_max_width(ui.available_width());
            // The surface is intentionally flat: no surrounding frame and no tab
            // strip. The default row is a status line; inspection is a compact
            // flat listing opened on demand and closed the same way.
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), pane_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    {
                        // The default surface is a status row; output is shown only by inspection.
                        let elapsed_ms = jobs
                            .iter()
                            .map(|job| {
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                                    .saturating_sub(job.started_at)
                                    .saturating_mul(1000)
                            })
                            .chain(processes.iter().map(|process| {
                                if process.started_at == 0 {
                                    0
                                } else {
                                    process
                                        .finished_at
                                        .unwrap_or_else(|| {
                                            std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_millis()
                                                as u64
                                        })
                                        .saturating_sub(process.started_at)
                                }
                            }))
                            .max()
                            .unwrap_or_default();
                        let status = if jobs.iter().any(|job| job.status == JobStatus::Running)
                            || running_commands > 0
                        {
                            "Working"
                        } else {
                            "Activity"
                        };
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), HEADER_HEIGHT),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.colored_label(
                                    accent,
                                    if status == "Working" { "◑" } else { "·" },
                                );
                                ui.label(egui::RichText::new(status).small().strong());
                                if !jobs.is_empty() {
                                    ui.colored_label(
                                        fg.gamma_multiply(0.65),
                                        format!(
                                            "{} agent{}",
                                            jobs.len(),
                                            if jobs.len() == 1 { "" } else { "s" }
                                        ),
                                    );
                                }
                                if running_commands > 0 {
                                    ui.colored_label(
                                        fg.gamma_multiply(0.65),
                                        format!(
                                            "{} command{}",
                                            running_commands,
                                            if running_commands == 1 { "" } else { "s" }
                                        ),
                                    );
                                }
                                ui.colored_label(
                                    fg.gamma_multiply(0.55),
                                    activity::format_elapsed_ms(elapsed_ms),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button(if self.inspecting {
                                                "Close"
                                            } else {
                                                "Inspect"
                                            })
                                            .clicked()
                                        {
                                            self.inspecting = !self.inspecting;
                                        }
                                    },
                                );
                            },
                        );
                        if self.inspecting {
                            ui.separator();
                            ui.label(egui::RichText::new("Activity").small().strong());
                            for process in processes {
                                let icon = if process.error.is_some() {
                                    "!"
                                } else if process.state == ProcessState::Running {
                                    "▸"
                                } else {
                                    "✓"
                                };
                                let elapsed = process
                                    .finished_at
                                    .unwrap_or_else(|| {
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_millis()
                                            as u64
                                    })
                                    .saturating_sub(process.started_at);
                                if ui
                                    .selectable_label(
                                        false,
                                        format!(
                                            "{icon} {} · {} · {}",
                                            activity::truncate(&process.command, 48),
                                            activity::process_state_label(process.state),
                                            activity::format_elapsed_ms(elapsed),
                                        ),
                                    )
                                    .clicked()
                                {
                                    open_process = Some(process.id.clone());
                                }
                            }
                            for job in jobs {
                                let elapsed = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                                    .saturating_sub(job.started_at);
                                let response = task_row(
                                    ui,
                                    if job.status == JobStatus::Running {
                                        RowIndicator::Glyph(activity::job_status_icon(job))
                                    } else {
                                        RowIndicator::Queued
                                    },
                                    accent,
                                    egui::RichText::new(format!(
                                        "{} · {} · {}",
                                        job.agent,
                                        activity::truncate(activity::job_label(job), 40),
                                        activity::format_elapsed_ms(elapsed.saturating_mul(1000)),
                                    ))
                                    .small()
                                    .color(fg),
                                    false,
                                );
                                if response.clicked() {
                                    open_job = Some(job.id.clone());
                                }
                            }
                            for id in ids.iter().filter(|id| matches!(id, PageId::Extension(_))) {
                                let label = match id {
                                    PageId::Extension(id) => id.as_str(),
                                    PageId::Agents => "Agents",
                                };
                                if ui
                                    .selectable_label(
                                        self.selected.as_ref() == Some(id),
                                        egui::RichText::new(format!("• {label}")).small(),
                                    )
                                    .clicked()
                                {
                                    self.select(id.clone());
                                }
                            }

                            if matches!(self.selected, Some(PageId::Agents)) {
                                self.follow.entry(PageId::Agents).or_insert(true);
                            }
                            if let Some(PageId::Extension(extension_id)) = self.selected.clone() {
                                let component = view
                                    .components
                                    .iter()
                                    .find(|component| component.id() == extension_id);
                                let (lines, daemon_scroll) = match component {
                                    Some(Component::Float { lines, scroll, .. }) => {
                                        (lines.as_slice(), *scroll)
                                    }
                                    _ => (&[][..], 0),
                                };
                                let offset = self
                                    .scroll
                                    .get(&PageId::Extension(extension_id.clone()))
                                    .map(|state| state.0)
                                    .unwrap_or_default();
                                let following = self
                                    .follow
                                    .get(&PageId::Extension(extension_id.clone()))
                                    .copied()
                                    .unwrap_or(true);
                                let body_height = (pane_height - HEADER_HEIGHT - 56.0).max(24.0);
                                let out = egui::ScrollArea::vertical()
                                    .id_salt(("content", &extension_id))
                                    .stick_to_bottom(following)
                                    .max_height(body_height)
                                    .min_scrolled_height(body_height)
                                    .auto_shrink([false, false])
                                    .vertical_scroll_offset(offset)
                                    .show(ui, |ui| {
                                        panes::render_lines(
                                            ui,
                                            &lines[daemon_scroll.min(lines.len())..],
                                            &view.highlights,
                                            palette,
                                        );
                                    });
                                let max_scroll =
                                    (out.content_size.y - out.inner_rect.height()).max(0.0);
                                let at_bottom = (max_scroll - out.state.offset.y).abs() < 8.0;
                                let page = PageId::Extension(extension_id.clone());
                                self.scroll
                                    .insert(page.clone(), (out.state.offset.y, daemon_scroll));
                                if !jump_to_latest {
                                    self.follow.insert(page.clone(), at_bottom);
                                    if at_bottom {
                                        self.new_output.insert(page.clone(), 0);
                                    }
                                }
                                if !self.follow.get(&page).copied().unwrap_or(true) {
                                    let mut overlay = ui.new_child(
                                        egui::UiBuilder::new()
                                            .id_salt(("live-jump-latest", &extension_id))
                                            .max_rect(out.inner_rect.shrink(8.0))
                                            .layout(egui::Layout::bottom_up(egui::Align::Center)),
                                    );
                                    overlay.set_clip_rect(ui.clip_rect().intersect(out.inner_rect));
                                    let count = self.new_output.get(&page).copied().unwrap_or(0);
                                    if overlay
                                        .add(egui::Button::new(if count > 0 {
                                            "↓ New output · Jump to latest"
                                        } else {
                                            "↓ Jump to latest"
                                        }))
                                        .clicked()
                                    {
                                        jump_to_latest = true;
                                    }
                                }
                                if jump_to_latest {
                                    let mut scroll = out.state;
                                    scroll.offset.y = max_scroll;
                                    scroll.store(ui.ctx(), out.id);
                                    self.scroll
                                        .insert(page.clone(), (max_scroll, daemon_scroll));
                                    self.follow.insert(page.clone(), true);
                                    self.new_output.insert(page, 0);
                                    ui.ctx().request_repaint();
                                }
                            }
                        }
                    }
                    return;
                },
            );
        });
        open_process
            .map(LiveAction::OpenProcess)
            .or_else(|| open_job.map(LiveAction::OpenJob))
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
        jobs[0].activity = Some("New progress".into());
        pane.sync(&page_ids(&view, &jobs));
        assert_eq!(pane.selected, Some("task_list".into()));
        assert_eq!(pane.scroll[&"task_list".into()].0, 72.0);
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
    fn follow_and_new_output_state_is_retained_per_page() {
        fn render_frame(
            ctx: &egui::Context,
            pane: &mut LivePane,
            view: &ViewModel,
            jobs: &[JobSnapshot],
        ) {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    pane.render(ui, view, jobs, &[], &Palette::default(), 180.0);
                },
            )
            .textures_delta
            .clear();
        }

        let mut view = ViewModel {
            components: vec![tasks()],
            ..Default::default()
        };
        let jobs = vec![job()];
        let mut pane = LivePane::default();
        pane.inspecting = true;
        pane.select("task_list".into());
        let ctx = egui::Context::default();
        render_frame(&ctx, &mut pane, &view, &jobs);
        assert_eq!(pane.line_counts[&"task_list".into()], 30);
        assert!(pane.follow[&"task_list".into()]);

        pane.scroll_by(-3);
        render_frame(&ctx, &mut pane, &view, &jobs);
        assert!(!pane.follow[&"task_list".into()]);
        let retained_offset = pane.scroll[&"task_list".into()].0;
        assert!(retained_offset > 0.0);

        if let Component::Float { lines, scroll, .. } = &mut view.components[0] {
            *scroll = 2;
            lines.push(PaneLineSpec::Plain("Task 30: newly arrived".into()));
        }
        render_frame(&ctx, &mut pane, &view, &jobs);
        assert_eq!(pane.new_output[&"task_list".into()], 1);
        assert!(pane.scroll[&"task_list".into()].0 > 0.0);

        pane.select(PageId::Agents);
        render_frame(&ctx, &mut pane, &view, &jobs);
        assert!(pane.follow[&PageId::Agents]);
        pane.select("task_list".into());
        assert!(!pane.follow[&"task_list".into()]);
        assert_eq!(pane.new_output[&"task_list".into()], 1);
    }

    #[test]
    fn jump_to_latest_clears_new_output_and_resumes_following() {
        fn render_frame(
            ctx: &egui::Context,
            pane: &mut LivePane,
            view: &ViewModel,
            jobs: &[JobSnapshot],
            events: Vec<egui::Event>,
        ) -> egui::FullOutput {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    pane.render(ui, view, jobs, &[], &Palette::default(), 180.0);
                },
            );
            output.textures_delta.clear();
            output
        }
        fn text_rect(output: &egui::FullOutput, needle: &str) -> Option<egui::Rect> {
            fn visit(shape: &egui::Shape, needle: &str) -> Option<egui::Rect> {
                match shape {
                    egui::Shape::Text(text) if text.galley.job.text.contains(needle) => {
                        Some(text.galley.rect.translate(text.pos.to_vec2()))
                    }
                    egui::Shape::Vec(shapes) => {
                        shapes.iter().find_map(|shape| visit(shape, needle))
                    }
                    _ => None,
                }
            }
            output
                .shapes
                .iter()
                .find_map(|shape| visit(&shape.shape, needle))
        }
        fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        }

        let view = ViewModel {
            components: vec![tasks()],
            ..Default::default()
        };
        let jobs = vec![job()];
        let mut pane = LivePane::default();
        pane.inspecting = true;
        pane.select("task_list".into());
        let ctx = egui::Context::default();
        render_frame(&ctx, &mut pane, &view, &jobs, vec![]);
        pane.scroll_by(-3);
        let output = render_frame(&ctx, &mut pane, &view, &jobs, vec![]);
        let jump = text_rect(&output, "↓ Jump to latest").expect("jump affordance is visible");
        for pressed in [true, false] {
            render_frame(
                &ctx,
                &mut pane,
                &view,
                &jobs,
                pointer(jump.center(), pressed),
            );
        }
        assert!(pane.follow[&"task_list".into()]);
        assert_eq!(pane.new_output[&"task_list".into()], 0);
        let max_scroll = pane.scroll[&"task_list".into()].0;
        assert!(max_scroll > 0.0);
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
                                        pane.render(
                                            ui,
                                            &view,
                                            &jobs,
                                            &[],
                                            &Palette::default(),
                                            180.0,
                                        );
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
