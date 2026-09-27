//! A terminal cell grid painted with egui: ratatui buffers become rows of
//! styled runs placed on fixed character cells, so anything laid out for the
//! TUI renders the same way in the desktop.
use eframe::egui::{self, Ui};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

/// A run of cells sharing a style, placed at its starting column. A wide
/// glyph is always its own run so every run starts on the cell grid.
#[derive(Clone)]
pub(crate) struct Run {
    pub col: u16,
    pub cells: u16,
    pub text: String,
    pub style: Style,
}

/// One terminal row.
pub(crate) type TermRow = Vec<Run>;

/// The monospace font and the size of one character cell.
pub(crate) struct Metrics {
    pub font: egui::FontId,
    pub row_height: f32,
    pub cell: f32,
}

pub(crate) fn metrics(ui: &Ui) -> Metrics {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
    // The real per-cell advance (kerning and spacing included), so lines laid
    // out at `cols` cells never overrun the pane.
    let cell = ui
        .fonts_mut(|fonts| {
            let run = "M".repeat(100);
            fonts
                .layout_no_wrap(run, font.clone(), egui::Color32::WHITE)
                .size()
                .x
                / 100.0
        })
        .max(1.0);
    Metrics {
        font,
        row_height,
        cell,
    }
}

/// Convert a rendered buffer into rows of styled runs.
pub(crate) fn buffer_rows(buffer: &Buffer) -> Vec<TermRow> {
    let cols = buffer.area.width;
    let height = buffer.area.height;
    (0..height)
        .map(|y| {
            let mut row: TermRow = Vec::new();
            let mut x = 0;
            let mut wide_before = false;
            while x < cols {
                let cell = &buffer[(buffer.area.x + x, buffer.area.y + y)];
                let symbol = cell.symbol();
                let width = unicode_width::UnicodeWidthStr::width(symbol).max(1) as u16;
                let style = Style::default()
                    .fg(cell.fg)
                    .bg(cell.bg)
                    .add_modifier(cell.modifier);
                let wide = width > 1;
                match row.last_mut() {
                    Some(run) if run.style == style && !wide && !wide_before => {
                        run.text.push_str(symbol);
                        run.cells += width;
                    }
                    _ => row.push(Run {
                        col: x,
                        cells: width,
                        text: symbol.to_string(),
                        style,
                    }),
                }
                wide_before = wide;
                // A wide glyph owns the following cell.
                x += width;
            }
            // Trailing blank cells without a background add nothing.
            while row.last().is_some_and(|run| {
                run.text.trim().is_empty() && run.style.bg.is_none_or(|bg| bg == Color::Reset)
            }) {
                row.pop();
            }
            row
        })
        .collect()
}

/// Paint lines as terminal rows across the available width, one row per line
/// and clipped like a terminal line. Returns each row's rectangle.
pub(crate) fn paint_lines(ui: &mut Ui, lines: &[ratatui::text::Line<'static>]) -> Vec<egui::Rect> {
    let metrics = metrics(ui);
    let width = ui.available_width();
    let cols = ((width / metrics.cell).floor() as u16).max(1);
    let height = lines.len() as u16;
    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, cols, height));
    for (y, line) in lines.iter().enumerate() {
        buffer.set_line(0, y as u16, line, cols);
    }
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, height as f32 * metrics.row_height),
        egui::Sense::hover(),
    );
    let default_fg = ui.visuals().text_color();
    buffer_rows(&buffer)
        .iter()
        .enumerate()
        .map(|(y, row)| {
            let line = egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.top() + y as f32 * metrics.row_height),
                egui::vec2(width, metrics.row_height),
            );
            paint_row(ui, line, &metrics, row, default_fg);
            line
        })
        .collect()
}

/// Draw a full-screen ratatui view into `rect` and paint it on the cell grid.
pub(crate) fn paint_screen(ui: &mut Ui, rect: egui::Rect, draw: impl FnOnce(&mut ratatui::Frame)) {
    let metrics = metrics(ui);
    let cols = ((rect.width() / metrics.cell).floor() as u16).max(1);
    let rows = ((rect.height() / metrics.row_height).floor() as u16).max(1);
    let Ok(mut terminal) = ratatui::Terminal::new(TestBackend::new(cols, rows)) else {
        return;
    };
    if terminal.draw(draw).is_err() {
        return;
    }
    let default_fg = ui.visuals().text_color();
    for (y, row) in buffer_rows(terminal.backend().buffer()).iter().enumerate() {
        let line = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + y as f32 * metrics.row_height),
            egui::vec2(rect.width(), metrics.row_height),
        );
        paint_row(ui, line, &metrics, row, default_fg);
    }
}

/// Paint one terminal row: span colors and modifiers, plus a line background
/// filling the whole row like a terminal cell run.
pub(crate) fn paint_row(
    ui: &Ui,
    rect: egui::Rect,
    metrics: &Metrics,
    line: &TermRow,
    default_fg: egui::Color32,
) {
    let (cell, font) = (metrics.cell, &metrics.font);
    for run in line {
        let style = run.style;
        let mut fg = style.fg.and_then(to_egui).unwrap_or(default_fg);
        let mut bg = style.bg.and_then(to_egui);
        if style.add_modifier.contains(Modifier::REVERSED) {
            let back = bg.unwrap_or(ui.visuals().panel_fill);
            bg = Some(fg);
            fg = back;
        }
        if style.add_modifier.contains(Modifier::DIM) {
            fg = fg.gamma_multiply(0.6);
        }
        let left = rect.left() + run.col as f32 * cell;
        let span = egui::Rect::from_min_size(
            egui::pos2(left, rect.top()),
            egui::vec2(run.cells as f32 * cell, rect.height()),
        );
        if let Some(bg) = bg {
            ui.painter().rect_filled(span, 0.0, bg);
        }
        if run.text.trim().is_empty() {
            continue;
        }
        let mut format = egui::TextFormat::simple(font.clone(), fg);
        format.italics = style.add_modifier.contains(Modifier::ITALIC);
        if style.add_modifier.contains(Modifier::UNDERLINED) {
            format.underline = egui::Stroke::new(1.0, fg);
        }
        if style.add_modifier.contains(Modifier::CROSSED_OUT) {
            format.strikethrough = egui::Stroke::new(1.0, fg);
        }
        let mut job = egui::text::LayoutJob::single_section(run.text.clone(), format);
        job.wrap.max_width = f32::INFINITY;
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        // A wide glyph from a fallback font may not be exactly two cells:
        // center it in its cells instead of letting it push the row.
        let x = if run.cells > 1 && run.text.chars().count() == 1 {
            span.center().x - galley.size().x / 2.0
        } else {
            left
        };
        ui.painter()
            .galley(egui::pos2(x, rect.top()), galley, default_fg);
    }
}

/// Terminal color to egui. `Reset` means "the terminal default" and keeps the
/// caller's color.
pub(crate) fn to_egui(color: Color) -> Option<egui::Color32> {
    if let Color::Indexed(index) = color {
        return Some(indexed(index));
    }
    bone_render::color::color_to_rgb(color).map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
}

/// The xterm 256-color palette.
fn indexed(index: u8) -> egui::Color32 {
    const BASE: [Color; 16] = [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    match index {
        0..16 => bone_render::color::color_to_rgb(BASE[index as usize])
            .map(|(r, g, b)| egui::Color32::from_rgb(r, g, b))
            .unwrap_or(egui::Color32::GRAY),
        16..232 => {
            let n = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            egui::Color32::from_rgb(level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        _ => {
            let gray = 8 + (index - 232) * 10;
            egui::Color32::from_gray(gray)
        }
    }
}
