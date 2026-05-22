use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use std::path::Path;
use unicode_width::UnicodeWidthStr;

use super::BoneTerminal;

/// Render startup banner into native scrollback above the inline viewport.
pub fn render(term: &mut BoneTerminal, provider: &str, model: &str) -> std::io::Result<()> {
    let term_width = term.size().map(|s| s.width).unwrap_or(80);
    let banner_lines = lines(provider, model, term_width);

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

fn lines(provider: &str, model: &str, term_width: u16) -> Vec<Line<'static>> {
    let version = env!("CARGO_PKG_VERSION");
    let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let dir_display = format_short_dir(&cwd);

    let dim = Style::default().fg(Color::DarkGray);
    let bold_white = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let accent = Style::default().fg(Color::DarkGray);
    let muted = Style::default().fg(Color::Gray);

    let term_width = term_width as usize;
    let inner = term_width.saturating_sub(2);

    // Content rows have "│ " (2) + "│" (1) = 3 chars framing,
    // while borders have "╭" (1) + "╮" (1) = 2 chars — so content
    // gets 1 fewer character of usable width.
    let content_w = inner.saturating_sub(1);

    // Row 1: bone ... v0.1.0
    let r1_left = "bone";
    let r1_right = format!("v{version}");
    let r1_pad = content_w.saturating_sub(
        UnicodeWidthStr::width(r1_left) + UnicodeWidthStr::width(r1_right.as_str()),
    );

    // Row 2: provider · model ... dir
    let r2_left = format!("{provider} · {model}");
    let r2_right = dir_display;
    let r2_pad = content_w.saturating_sub(
        UnicodeWidthStr::width(r2_left.as_str()) + UnicodeWidthStr::width(r2_right.as_str()),
    );

    vec![
        Line::from(Span::styled(format!("╭{}╮", "─".repeat(inner)), dim)),
        Line::from(vec![
            Span::styled("│ ", dim),
            Span::styled(r1_left.to_string(), bold_white),
            Span::raw(" ".repeat(r1_pad - 1)),
            Span::styled(r1_right, muted),
            Span::raw(" "),
            Span::styled("│", dim),
        ]),
        Line::from(vec![
            Span::styled("│ ", dim),
            Span::styled(r2_left, accent),
            Span::raw(" ".repeat(r2_pad - 1)),
            Span::styled(r2_right, muted),
            Span::raw(" "),
            Span::styled("│", dim),
        ]),
        Line::from(Span::styled(format!("╰{}╯", "─".repeat(inner)), dim)),
        Line::from(""),
    ]
}

/// Shorten a directory path to `first/.../last` for the banner display.
fn format_short_dir(path: &Path) -> String {
    let components: Vec<&std::ffi::OsStr> = path.iter().collect();
    if components.len() > 2 {
        let first = components[0].to_string_lossy();
        let last = components
            .last()
            .expect("Path::components is non-empty for absolute or relative paths")
            .to_string_lossy();
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
