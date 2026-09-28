//! The one-row status bar: model, incognito, approval mode, token metrics,
//! queue, timer, and spinner, followed by Lua-defined status segments.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::theme::Theme;

/// Spinner frames used when the configured style has no preset.
pub const FALLBACK_SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const FALLBACK_SPINNER_SPEED_MS: u64 = 80;

/// Status bar info passed from App to Renderer for each draw.
pub struct StatusInfo {
    pub model: String,
    pub token_stats: bone_protocol::TokenStats,
    /// Live cumulative output-token estimate during streaming.
    pub streaming_completion_tokens: Option<u64>,
    pub streaming: bool,
    /// Approval mode label (`Safe`/`Danger`) and whether it is Danger.
    pub approval_label: String,
    pub approval_danger: bool,
    pub queue_len: usize,
    /// Whether the session is in incognito mode (no durable writes while on).
    pub incognito: bool,
    pub status_show: std::collections::HashMap<String, bool>,
    /// Formatted elapsed time string (e.g. "1:23") for the current turn.
    pub elapsed: Option<String>,
    /// Lua-defined status segments (`bone.api.ui.set_statusline`), appended to
    /// the native status bar. Empty when Lua has not set one.
    pub lua_status: Vec<bone_protocol::StatusSegment>,
    /// Resolved spinner frames for the currently-selected style.
    pub spinner_frames: Vec<String>,
    /// Resolved frame speed in ms (override or style default).
    pub spinner_speed_ms: u64,
    /// Resolved rotating thinking-text phrases for the selected preset.
    pub spinner_texts: Vec<String>,
    /// Whether thinking-text phrases rotate while streaming.
    pub spinner_text_rotate: bool,
    /// Thinking-text rotation speed in ms/phrase; 0 means one phrase per spinner cycle.
    pub spinner_text_speed_ms: u64,
    /// Raw elapsed milliseconds of the current turn (for frame indexing).
    pub spinner_elapsed_ms: u64,
}

impl StatusInfo {
    pub fn show(&self, key: &str) -> bool {
        self.status_show.get(key).copied().unwrap_or(true)
    }
}

/// Current spinner frame string for the running-commands strip, mirroring
/// the status bar's spinner computation.
pub fn spinner_frame(status_info: &StatusInfo) -> Option<String> {
    let frames = &status_info.spinner_frames;
    if frames.is_empty() {
        return None;
    }
    let speed = if status_info.spinner_speed_ms > 0 {
        status_info.spinner_speed_ms
    } else {
        80
    };
    let idx = (status_info.spinner_elapsed_ms / speed) as usize % frames.len();
    Some(frames[idx].clone())
}

pub fn draw_status_bar(frame: &mut Frame, status_info: &StatusInfo, theme: &Theme, area: Rect) {
    draw_status_bar_inner(frame, status_info, theme, area, false);
}

/// Draw a touch-friendly one-row status bar. It keeps safety and activity
/// indicators visible while dropping the lower-priority token breakdown that
/// otherwise consumes most of a phone-width row.
pub fn draw_status_bar_compact(
    frame: &mut Frame,
    status_info: &StatusInfo,
    theme: &Theme,
    area: Rect,
) {
    draw_status_bar_inner(frame, status_info, theme, area, true);
}

fn draw_status_bar_inner(
    frame: &mut Frame,
    status_info: &StatusInfo,
    theme: &Theme,
    area: Rect,
    compact: bool,
) {
    let mut status_spans: Vec<Span> = vec![];
    let sep = || Span::styled(" | ", Style::default().fg(theme.status_text));

    if status_info.show("status_show_model") {
        let model = if compact {
            compact_status_text(&status_info.model, 24)
        } else {
            status_info.model.clone()
        };
        status_spans.push(Span::styled(model, Style::default().fg(theme.status_text)));
        status_spans.push(sep());
    }

    // Incognito badge: rendered while the session is detached from durable
    // storage, so the user always sees that chats are not being saved.
    if status_info.incognito {
        status_spans.push(Span::styled(
            "INC",
            Style::default()
                .fg(theme.palette.warn)
                .add_modifier(Modifier::BOLD),
        ));
        status_spans.push(sep());
    }

    if status_info.show("status_show_approval") {
        status_spans.push(Span::styled(
            status_info.approval_label.clone(),
            Style::default().fg(match status_info.approval_danger {
                false => theme.approval_safe,
                true => theme.approval_danger,
            }),
        ));
        status_spans.push(sep());
    }

    use bone_protocol::format_tokens;

    let received = status_info
        .streaming_completion_tokens
        .unwrap_or(status_info.token_stats.received);
    let any_token_metric = status_info.show("status_show_tokens_curr")
        || status_info.show("status_show_tokens_in")
        || status_info.show("status_show_tokens_out")
        || status_info.show("status_show_tokens_total");

    if !compact && any_token_metric {
        let mut metric_parts: Vec<Span> = vec![];
        let s = Style::default().fg(theme.status_text);
        if status_info.show("status_show_tokens_curr") {
            push_metric(
                &mut metric_parts,
                s,
                &format!(
                    "curr {}",
                    format_tokens(status_info.token_stats.context_length)
                ),
            );
        }
        if status_info.show("status_show_tokens_in") {
            push_metric(
                &mut metric_parts,
                s,
                &format!("in {}", format_tokens(status_info.token_stats.sent)),
            );
        }
        if status_info.show("status_show_tokens_out") {
            push_metric(
                &mut metric_parts,
                s,
                &format!("out {}", format_tokens(received)),
            );
        }
        if status_info.show("status_show_tokens_total") {
            push_metric(
                &mut metric_parts,
                s,
                &format!(
                    "total {}",
                    format_tokens(status_info.token_stats.sent + received)
                ),
            );
        }
        status_spans.extend(metric_parts);
        status_spans.push(sep());
    }

    if status_info.show("status_show_queue") && status_info.queue_len > 0 {
        status_spans.push(Span::styled(
            format!("Q: {}", status_info.queue_len),
            Style::default().fg(theme.status_text),
        ));
        status_spans.push(sep());
    }

    if status_info.show("status_show_timer")
        && let Some(ref elapsed) = status_info.elapsed
    {
        status_spans.push(Span::styled(
            elapsed.clone(),
            Style::default().fg(theme.status_text),
        ));
        status_spans.push(sep());
    }

    if status_info.show("status_show_spinner") && status_info.streaming {
        let frames = &status_info.spinner_frames;
        if !frames.is_empty() {
            let speed = if status_info.spinner_speed_ms > 0 {
                status_info.spinner_speed_ms
            } else {
                80
            };
            let frame_idx = (status_info.spinner_elapsed_ms / speed) as usize % frames.len();
            status_spans.push(Span::styled(
                frames[frame_idx].clone(),
                Style::default().fg(theme.thinking),
            ));
            let texts = &status_info.spinner_texts;
            let label = if texts.is_empty() {
                " thinking".to_string()
            } else if !status_info.spinner_text_rotate || texts.len() == 1 {
                format!(" {}", texts[0])
            } else {
                let cycle = match status_info
                    .spinner_elapsed_ms
                    .checked_div(status_info.spinner_text_speed_ms)
                {
                    Some(c) => c as usize,
                    None => (status_info.spinner_elapsed_ms / speed) as usize / frames.len(),
                };
                let phrase = &texts[cycle % texts.len()];
                format!(" {phrase}")
            };
            status_spans.push(Span::styled(label, Style::default().fg(theme.status_text)));
        }
    }

    // Remove trailing separator if present
    if let Some(last) = status_spans.last()
        && last.content == " | "
    {
        status_spans.pop();
    }

    // Append Lua-defined status segments (`bone.api.ui.set_statusline`).
    // Left/center segments extend the native bar; right segments are drawn
    // right-aligned on the same row.
    use bone_protocol::Align;
    let seg_span = |seg: &bone_protocol::StatusSegment| {
        let color = seg
            .fg
            .as_deref()
            .and_then(crate::color::parse_color)
            .unwrap_or(theme.status_text);
        Span::styled(seg.text.clone(), Style::default().fg(color))
    };
    let mut right_spans: Vec<Span> = vec![];
    for seg in &status_info.lua_status {
        if matches!(seg.align, Align::Right) {
            right_spans.push(seg_span(seg));
        } else {
            if !status_spans.is_empty() {
                status_spans.push(sep());
            }
            status_spans.push(seg_span(seg));
        }
    }

    if area.height > 0 {
        let row = Rect {
            y: area.bottom() - 1,
            height: 1,
            ..area
        };
        let right_line = Line::from(right_spans);
        // Reserve the right-aligned segments' width so they never overwrite
        // the left/native content on the same row.
        let right_width = right_line.width() as u16;
        let left_row = if right_width > 0 {
            Rect {
                width: row.width.saturating_sub(right_width + 1),
                ..row
            }
        } else {
            row
        };
        frame.render_widget(Paragraph::new(Line::from(status_spans)), left_row);
        if right_width > 0 {
            frame.render_widget(
                Paragraph::new(right_line).alignment(ratatui::layout::Alignment::Right),
                row,
            );
        }
    }
}

fn compact_status_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max_chars.saturating_sub(1)).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn push_metric(parts: &mut Vec<Span<'static>>, style: Style, label: &str) {
    if !parts.is_empty() {
        parts.push(Span::styled(" / ", style));
    }
    parts.push(Span::styled(label.to_string(), style));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> StatusInfo {
        StatusInfo {
            model: "abcdefghijklmnopqrstuvwxyz".into(),
            token_stats: bone_protocol::TokenStats {
                sent: 1_234,
                received: 5_678,
                context_length: 9_999,
                ..Default::default()
            },
            streaming_completion_tokens: Some(42),
            streaming: true,
            approval_label: "Danger".into(),
            approval_danger: true,
            queue_len: 2,
            incognito: true,
            status_show: std::collections::HashMap::new(),
            elapsed: Some("1:23".into()),
            lua_status: Vec::new(),
            spinner_frames: vec!["*".into()],
            spinner_speed_ms: 80,
            spinner_texts: vec!["thinking".into()],
            spinner_text_rotate: false,
            spinner_text_speed_ms: 0,
            spinner_elapsed_ms: 0,
        }
    }

    fn row_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        (0..terminal.backend().buffer().area().width)
            .map(|column| {
                terminal
                    .backend()
                    .buffer()
                    .cell((column, 0))
                    .unwrap()
                    .symbol()
            })
            .collect()
    }

    #[test]
    fn compact_status_keeps_safety_and_activity_but_hides_token_breakdown() {
        let status = status();
        let theme = Theme::default();
        let mut compact =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 1)).unwrap();
        compact
            .draw(|frame| draw_status_bar_compact(frame, &status, &theme, frame.area()))
            .unwrap();
        let compact = row_text(&compact);

        assert!(compact.contains("abcdefghijklmnopqrstuvw…"), "{compact}");
        assert!(compact.contains("INC"), "{compact}");
        assert!(compact.contains("Danger"), "{compact}");
        assert!(compact.contains("Q: 2"), "{compact}");
        assert!(compact.contains("1:23"), "{compact}");
        assert!(compact.contains("* thinking"), "{compact}");
        assert!(!compact.contains("curr"), "{compact}");
        assert!(!compact.contains("total"), "{compact}");

        let mut normal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 1)).unwrap();
        normal
            .draw(|frame| draw_status_bar(frame, &status, &theme, frame.area()))
            .unwrap();
        let normal = row_text(&normal);
        assert!(normal.contains(&status.model), "{normal}");
        assert!(normal.contains("curr"), "{normal}");
        assert!(normal.contains("total"), "{normal}");
    }
}
