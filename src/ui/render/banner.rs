use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use std::path::Path;

use super::BoneTerminal;

/// Render startup banner into native scrollback above the inline viewport.
pub fn render(term: &mut BoneTerminal, provider: &str, model: &str) -> std::io::Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let dir_display = format_short_dir(&cwd);

    let dim = Style::default().fg(Color::DarkGray);
    let bold_white = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let accent = Style::default().fg(Color::Cyan);
    let muted = Style::default().fg(Color::Gray);

    let term_width = term.size().map(|s| s.width).unwrap_or(80) as usize;
    let inner = term_width.saturating_sub(3);

    // Content rows have " │ " (3) + " │" (2) = 5 chars framing,
    // while borders have " ╭" (2) + "╮" (1) = 3 chars — so content
    // gets 2 fewer characters of usable width.
    let content_w = inner.saturating_sub(2);

    // Row 1: bone ... v0.1.0
    let r1_left = "bone";
    let r1_right = format!("v{version}");
    let r1_pad = content_w.saturating_sub(r1_left.len() + r1_right.len() + 1); // +1 for 2 trailing spaces vs original 1

    // Row 2: provider · model ... dir
    let r2_left = format!("{provider} · {model}");
    let r2_right = dir_display;
    let r2_pad = content_w.saturating_sub(r2_left.len() + r2_right.len() + 1);

    let banner_lines: Vec<Line<'static>> = vec![
        Line::from(vec![
            Span::styled(" ╭", dim),
            Span::styled("─".repeat(inner), dim),
            Span::styled("╮", dim),
        ]),
        Line::from(vec![
            Span::styled(" │ ", dim),
            Span::styled(r1_left.to_string(), bold_white),
            Span::styled(" ".repeat(r1_pad), Style::default()),
            Span::styled(r1_right, muted),
            Span::raw("  "),
            Span::styled("│", dim),
        ]),
        Line::from(vec![
            Span::styled(" │ ", dim),
            Span::styled(r2_left, accent),
            Span::styled(" ".repeat(r2_pad), Style::default()),
            Span::styled(r2_right, dim),
            Span::raw("  "),
            Span::styled("│", dim),
        ]),
        Line::from(vec![
            Span::styled(" ╰", dim),
            Span::styled("─".repeat(inner), dim),
            Span::styled("╯", dim),
        ]),
        Line::raw(""),
    ];

    term.insert_before(banner_lines.len() as u16, |buf| {
        for (row, line) in banner_lines.iter().enumerate() {
            let area = ratatui::layout::Rect {
                x: 0,
                y: row as u16,
                width: buf.area.width,
                height: 1,
            };
            ratatui::widgets::Paragraph::new(line.clone()).render(area, buf);
        }
    })
}

/// Shorten a directory path to `first/.../last` for the banner display.
fn format_short_dir(path: &Path) -> String {
    let components: Vec<&std::ffi::OsStr> = path.iter().collect();
    if components.len() > 2 {
        let first = components[0].to_string_lossy();
        let last = components.last().unwrap().to_string_lossy();
        let sep = if first.ends_with('/') || first.ends_with('\\') {
            ""
        } else {
            "/"
        };
        format!("{first}{sep}.../{last}")
    } else {
        path.display().to_string()
    }
}
