//! Native renderer for daemon-owned background processes and jobs (Phase 5).
//!
//! The daemon broadcasts `ProcessesSnapshot`/`JobsSnapshot`, already reduced
//! into `State.processes`/`State.jobs`. This module maps those snapshots onto
//! egui widgets inside the Activity window, mirroring the TUI's
//! `processes_pane`, `jobs_pane`, and `process_view`. It changes no protocol
//! types; cancel actions are returned to the caller, which sends the matching
//! `RuntimeCommand`.

use bone_protocol::{JobEventSnapshot, JobSnapshot, JobStatus, ProcessSnapshot, ProcessState};
use eframe::egui;

use crate::theme::{self, Palette};

/// A user action emitted by the Activity dialog, applied by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityAction {
    /// Cancel a running background process by id.
    CancelProcess(String),
    /// Cancel a running/queued background job by id.
    CancelJob(String),
    /// Open the fullscreen live-output viewer for a process id.
    OpenProcess(String),
    /// Open the full transcript viewer for a job id.
    OpenJob(String),
    /// Select a row by its flat index (processes first, then jobs).
    Select(usize),
}

/// State for the fullscreen live-output process viewer, mirroring the TUI's
/// `process_view`: `follow` pins the view to the newest output.
#[derive(Debug, Clone)]
pub struct ProcessViewer {
    pub tab_id: u64,
    pub id: String,
    pub follow: bool,
}

impl ProcessViewer {
    pub fn new(tab_id: u64, id: impl Into<String>) -> Self {
        Self {
            tab_id,
            id: id.into(),
            follow: true,
        }
    }
}

/// State for the full job-transcript viewer, mirroring the TUI's `open_job`.
#[derive(Debug, Clone)]
pub struct JobViewer {
    pub tab_id: u64,
    pub id: String,
    pub follow: bool,
}

impl JobViewer {
    pub fn new(tab_id: u64, id: impl Into<String>) -> Self {
        Self {
            tab_id,
            id: id.into(),
            follow: true,
        }
    }
}

/// Compact token count, mirroring the TUI's `format_tokens`.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}m", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n >= 1_000 {
        let s = n.to_string();
        let mut out = String::with_capacity(s.len() + s.len() / 3);
        for (i, c) in s.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
        out.chars().rev().collect()
    } else {
        n.to_string()
    }
}

/// Human elapsed time from a millisecond count, mirroring the TUI's
/// `format_elapsed_ms`.
pub fn format_elapsed_ms(elapsed_ms: u64) -> String {
    if elapsed_ms < 60_000 {
        format!("{}s", elapsed_ms / 1000)
    } else {
        format!("{}m{}s", elapsed_ms / 60_000, (elapsed_ms / 1000) % 60)
    }
}

/// Lowercase lifecycle label for a process state.
pub fn process_state_label(state: ProcessState) -> &'static str {
    match state {
        ProcessState::Running => "running",
        ProcessState::Exited => "exited",
        ProcessState::TimedOut => "timed out",
        ProcessState::Cancelled => "cancelled",
    }
}

/// Status glyph for a job, mirroring the TUI's `job_status_icon`.
pub fn job_status_icon(job: &JobSnapshot) -> &'static str {
    match job.status {
        JobStatus::Running => "◑",
        JobStatus::Queued => "⧗",
    }
}

/// Lowercase status label for a job.
pub fn job_status_label(job: &JobSnapshot) -> &'static str {
    match job.status {
        JobStatus::Running => "running",
        JobStatus::Queued => "queued",
    }
}

/// Display label for a job: the model-supplied title when present, otherwise
/// the raw task prompt.
pub fn job_label(job: &JobSnapshot) -> &str {
    if job.title.is_empty() {
        &job.task
    } else {
        &job.title
    }
}

/// Flatten newlines and truncate to `max` characters (ellipsis included).
pub fn truncate(text: &str, max: usize) -> String {
    let flat = text.replace(['\n', '\r'], " ");
    if flat.chars().count() > max {
        let take = max.saturating_sub(3);
        format!("{}...", flat.chars().take(take).collect::<String>())
    } else {
        flat
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn process_elapsed_ms(process: &ProcessSnapshot) -> u64 {
    if process.started_at == 0 {
        return 0;
    }
    process
        .finished_at
        .unwrap_or_else(now_millis)
        .saturating_sub(process.started_at)
}

fn color(channel: Option<&String>, fallback: egui::Color32) -> egui::Color32 {
    channel
        .and_then(|value| theme::parse_color(value))
        .unwrap_or(fallback)
}

/// Render the process and job sections into `ui`, returning any cancel actions
/// the user triggered. `palette` colors the labels to match the daemon theme.
pub fn render(
    ui: &mut egui::Ui,
    processes: &[ProcessSnapshot],
    jobs: &[JobSnapshot],
    palette: &Palette,
    selected: Option<usize>,
) -> Vec<ActivityAction> {
    let mut actions = Vec::new();
    let (_, _, accent) = palette.resolved();
    let muted = color(palette.muted.as_ref(), egui::Color32::from_gray(150));
    let error = color(palette.error.as_ref(), egui::Color32::from_rgb(235, 90, 90));
    let warn = color(palette.warn.as_ref(), egui::Color32::from_rgb(230, 180, 80));

    ui.heading(format!("Processes ({})", processes.len()));
    let mut row = 0usize;
    if processes.is_empty() {
        ui.weak("No background processes.");
    } else {
        for process in processes {
            let is_selected = selected == Some(row);
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(accent, "◑");
                if ui
                    .selectable_label(is_selected, truncate(&process.command, 48))
                    .clicked()
                {
                    actions.push(ActivityAction::Select(row));
                }
                let elapsed = format_elapsed_ms(process_elapsed_ms(process));
                ui.colored_label(
                    muted,
                    format!("[{} · {elapsed}]", process_state_label(process.state)),
                );
                if ui
                    .small_button("View")
                    .on_hover_text("Full output")
                    .clicked()
                {
                    actions.push(ActivityAction::OpenProcess(process.id.clone()));
                }
                if process.state == ProcessState::Running && ui.small_button("Cancel").clicked() {
                    actions.push(ActivityAction::CancelProcess(process.id.clone()));
                }
            });
            let tail = process
                .stdout
                .lines()
                .last()
                .or_else(|| process.stderr.lines().last());
            if let Some(tail) = tail {
                let tail = truncate(tail, 80);
                if !tail.trim().is_empty() {
                    ui.colored_label(
                        muted,
                        egui::RichText::new(format!("    {tail}")).monospace(),
                    );
                }
            }
            if let Some(error_text) = &process.error {
                ui.colored_label(
                    error,
                    egui::RichText::new(format!("    {}", truncate(error_text, 120))).monospace(),
                );
            }
            ui.separator();
            row += 1;
        }
    }

    ui.add_space(4.0);
    ui.heading(format!("Jobs ({})", jobs.len()));
    if jobs.is_empty() {
        ui.weak("No background jobs.");
        return actions;
    }
    let now = now_seconds();
    for job in jobs {
        let is_selected = selected == Some(row);
        let icon_color = match job.status {
            JobStatus::Running => accent,
            JobStatus::Queued => warn,
        };
        let marker = if is_selected { "▸ " } else { "  " };
        let header = egui::RichText::new(format!(
            "{marker}{} {} — {}",
            job_status_icon(job),
            job.agent,
            truncate(job_label(job), 48)
        ))
        .color(icon_color);
        egui::CollapsingHeader::new(header)
            .id_salt(&job.id)
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(muted, job_status_label(job));
                    ui.colored_label(muted, format!("{} · ", job.provider));
                    let total = job.token_sent + job.token_received;
                    match job.status {
                        JobStatus::Running => {
                            let elapsed = now.saturating_sub(job.started_at);
                            ui.colored_label(
                                muted,
                                format!("{elapsed}s · {} tokens", format_tokens(total)),
                            );
                        }
                        JobStatus::Queued => {
                            ui.colored_label(muted, format!("{} tokens", format_tokens(total)));
                        }
                    }
                });
                if let Some(activity) = &job.activity {
                    ui.colored_label(muted, truncate(activity, 80));
                }
                for event in &job.events {
                    render_event(ui, event, accent, muted, error);
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .small_button("View transcript")
                        .on_hover_text("Full job transcript")
                        .clicked()
                    {
                        actions.push(ActivityAction::OpenJob(job.id.clone()));
                    }
                    if ui.small_button("Cancel job").clicked() {
                        actions.push(ActivityAction::CancelJob(job.id.clone()));
                    }
                });
            });
    }
    actions
}

/// Render the fullscreen live-output process viewer body into `ui`, returning
/// any cancel action the user triggered. Mirrors the TUI's `process_view`:
/// command header, full stdout/stderr/error, and exit/state/elapsed footer.
pub fn render_process_viewer(
    ui: &mut egui::Ui,
    process: &ProcessSnapshot,
    viewer: &mut ProcessViewer,
    palette: &Palette,
) -> Vec<ActivityAction> {
    let mut actions = Vec::new();
    let (_, fg, accent) = palette.resolved();
    let muted = color(palette.muted.as_ref(), egui::Color32::from_gray(150));
    let error = color(palette.error.as_ref(), egui::Color32::from_rgb(235, 90, 90));

    ui.horizontal_wrapped(|ui| {
        ui.colored_label(accent, egui::RichText::new("$").monospace().strong());
        ui.colored_label(
            fg,
            egui::RichText::new(&process.command).monospace().strong(),
        );
    });
    ui.horizontal_wrapped(|ui| {
        ui.colored_label(
            muted,
            format!(
                "{} · {}",
                process_state_label(process.state),
                format_elapsed_ms(process_elapsed_ms(process))
            ),
        );
        if process.state == ProcessState::Running && ui.small_button("Cancel").clicked() {
            actions.push(ActivityAction::CancelProcess(process.id.clone()));
        }
        let follow_label = if viewer.follow { "Following" } else { "Follow" };
        if ui
            .selectable_label(viewer.follow, follow_label)
            .on_hover_text("Pin the view to the newest output")
            .clicked()
        {
            viewer.follow = !viewer.follow;
        }
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt(("process-view", viewer.id.as_str()))
        .stick_to_bottom(viewer.follow)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            render_output_block(ui, &process.stdout, fg);
            if !process.stderr.is_empty() {
                render_output_block(ui, &process.stderr, error);
            }
            if let Some(error_text) = &process.error {
                render_output_block(ui, error_text, error);
            }
            if process.state != ProcessState::Running {
                if let Some(code) = process.exit_code {
                    ui.colored_label(muted, format!("exit code: {code}"));
                }
                if let Some(signal) = process.signal {
                    ui.colored_label(muted, format!("signal: {signal}"));
                }
                ui.colored_label(
                    muted,
                    format!("state: {}", process_state_label(process.state)),
                );
                ui.colored_label(
                    muted,
                    format!(
                        "elapsed: {}",
                        format_elapsed_ms(process_elapsed_ms(process))
                    ),
                );
                if process.exit_code.is_none()
                    && process.signal.is_none()
                    && process.error.is_none()
                {
                    ui.colored_label(muted, "finished");
                }
            }
        });
    actions
}

/// Render the full job-transcript viewer body into `ui`, returning any cancel
/// action the user triggered. Mirrors the TUI's `open_job` transcript: the task
/// prompt, streamed answer text, reasoning, tool calls with full arguments and
/// edit previews, full (untruncated) tool results, and failures.
pub fn render_job_viewer(
    ui: &mut egui::Ui,
    job: &JobSnapshot,
    viewer: &mut JobViewer,
    palette: &Palette,
) -> Vec<ActivityAction> {
    let mut actions = Vec::new();
    let (_, fg, accent) = palette.resolved();
    let muted = color(palette.muted.as_ref(), egui::Color32::from_gray(150));
    let error = color(palette.error.as_ref(), egui::Color32::from_rgb(235, 90, 90));
    let warn = color(palette.warn.as_ref(), egui::Color32::from_rgb(230, 180, 80));

    ui.horizontal_wrapped(|ui| {
        let icon_color = match job.status {
            JobStatus::Running => accent,
            JobStatus::Queued => warn,
        };
        ui.colored_label(
            icon_color,
            format!("{} {}", job_status_icon(job), job_status_label(job)),
        );
        ui.colored_label(muted, format!("{} · {}", job.agent, job.provider));
        let total = job.token_sent + job.token_received;
        match job.status {
            JobStatus::Running => {
                let elapsed = now_seconds().saturating_sub(job.started_at);
                ui.colored_label(
                    muted,
                    format!("{elapsed}s · {} tokens", format_tokens(total)),
                );
            }
            JobStatus::Queued => {
                ui.colored_label(muted, format!("{} tokens", format_tokens(total)));
            }
        }
        let follow_label = if viewer.follow { "Following" } else { "Follow" };
        if ui.selectable_label(viewer.follow, follow_label).clicked() {
            viewer.follow = !viewer.follow;
        }
        if ui.small_button("Cancel job").clicked() {
            actions.push(ActivityAction::CancelJob(job.id.clone()));
        }
    });
    if let Some(activity) = &job.activity {
        ui.colored_label(muted, activity);
    }
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt(("job-view", viewer.tab_id, viewer.id.as_str()))
        .stick_to_bottom(viewer.follow)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.colored_label(fg, egui::RichText::new(&job.task).strong());
            let mut answer = String::new();
            for event in &job.events {
                match event {
                    JobEventSnapshot::TextDelta { text } => answer.push_str(text),
                    JobEventSnapshot::ReasoningDelta { text } if !text.is_empty() => {
                        ui.colored_label(
                            muted,
                            egui::RichText::new(format!("thinking: {text}")).italics(),
                        );
                    }
                    JobEventSnapshot::ReasoningDelta { .. } => {}
                    JobEventSnapshot::ToolCall {
                        name,
                        arguments,
                        edit_preview,
                        ..
                    } => {
                        if !answer.trim().is_empty() {
                            ui.label(&answer);
                            answer.clear();
                        }
                        ui.colored_label(accent, egui::RichText::new(format!("→ {name}")).strong());
                        if !arguments.is_null() {
                            let args = serde_json::to_string_pretty(arguments)
                                .unwrap_or_else(|_| arguments.to_string());
                            ui.colored_label(muted, egui::RichText::new(args).monospace());
                        }
                        if let Some(diff) = edit_preview {
                            ui.colored_label(muted, egui::RichText::new(diff).monospace());
                        }
                    }
                    JobEventSnapshot::ToolResult {
                        name,
                        is_error,
                        content,
                        ..
                    } => {
                        let color = if *is_error { error } else { muted };
                        ui.colored_label(color, egui::RichText::new(format!("← {name}")).strong());
                        render_output_block(ui, content, color);
                    }
                    JobEventSnapshot::Failed { message } => {
                        ui.colored_label(error, format!("✗ {message}"));
                    }
                }
            }
            if !answer.trim().is_empty() {
                ui.label(&answer);
            }
        });
    actions
}

/// Render a monospace, selectable, wrapped output block, skipped when empty.
fn render_output_block(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    if text.is_empty() {
        return;
    }
    ui.colored_label(color, egui::RichText::new(text).monospace());
}

fn render_event(
    ui: &mut egui::Ui,
    event: &JobEventSnapshot,
    accent: egui::Color32,
    muted: egui::Color32,
    error: egui::Color32,
) {
    match event {
        JobEventSnapshot::TextDelta { text } => {
            ui.label(truncate(text, 400));
        }
        JobEventSnapshot::ReasoningDelta { text } => {
            ui.colored_label(muted, egui::RichText::new(truncate(text, 400)).italics());
        }
        JobEventSnapshot::ToolCall { name, .. } => {
            ui.colored_label(accent, format!("→ {name}"));
        }
        JobEventSnapshot::ToolResult {
            name,
            is_error,
            content,
            ..
        } => {
            let color = if *is_error { error } else { muted };
            ui.colored_label(color, format!("← {name}: {}", truncate(content, 200)));
        }
        JobEventSnapshot::Failed { message } => {
            ui.colored_label(error, format!("✗ {}", truncate(message, 200)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(id: &str, state: ProcessState) -> ProcessSnapshot {
        ProcessSnapshot {
            id: id.into(),
            command: "cargo test".into(),
            owner: "conversation".into(),
            running: state == ProcessState::Running,
            state,
            started_at: 1_000,
            finished_at: Some(2_500),
            stdout: "line one\nline two".into(),
            stderr: String::new(),
            exit_code: None,
            signal: None,
            error: None,
        }
    }

    fn job(id: &str, status: JobStatus) -> JobSnapshot {
        JobSnapshot {
            id: id.into(),
            agent: "explore".into(),
            task: "find the thing".into(),
            title: "Locate code".into(),
            status,
            started_at: 1_000,
            token_sent: 1_200,
            token_received: 3_400,
            provider: "anthropic".into(),
            activity: Some("reading files".into()),
            events: vec![JobEventSnapshot::ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: serde_json::Value::Null,
                edit_preview: None,
            }],
        }
    }

    #[test]
    fn format_tokens_matches_tui_buckets() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_000), "1,000");
        assert_eq!(format_tokens(12_345), "12.3k");
        assert_eq!(format_tokens(1_500_000), "1.50m");
    }

    #[test]
    fn format_elapsed_ms_switches_at_a_minute() {
        assert_eq!(format_elapsed_ms(0), "0s");
        assert_eq!(format_elapsed_ms(59_999), "59s");
        assert_eq!(format_elapsed_ms(60_000), "1m0s");
        assert_eq!(format_elapsed_ms(125_000), "2m5s");
    }

    #[test]
    fn labels_and_icons_cover_all_states() {
        assert_eq!(process_state_label(ProcessState::Running), "running");
        assert_eq!(process_state_label(ProcessState::TimedOut), "timed out");
        assert_eq!(job_status_icon(&job("j", JobStatus::Running)), "◑");
        assert_eq!(job_status_icon(&job("j", JobStatus::Queued)), "⧗");
        assert_eq!(job_status_label(&job("j", JobStatus::Queued)), "queued");
        assert_eq!(job_label(&job("j", JobStatus::Running)), "Locate code");
    }

    #[test]
    fn truncate_flattens_and_bounds() {
        assert_eq!(truncate("a\nb\r\nc", 10), "a b  c");
        assert_eq!(truncate("abcdefghij", 5), "ab...");
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn render_without_clicks_emits_no_actions() {
        let ctx = egui::Context::default();
        let processes = vec![process("p1", ProcessState::Running)];
        let jobs = vec![job("j1", JobStatus::Running)];
        let palette = Palette::default();
        let actions = std::cell::RefCell::new(Vec::new());
        ctx.run_ui(egui::RawInput::default(), |ui| {
            *actions.borrow_mut() = render(ui, &processes, &jobs, &palette, None);
        })
        .textures_delta
        .clear();
        assert!(actions.borrow().is_empty());
    }
}
