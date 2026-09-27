//! Conversation history sidebar rows, modeled on the TUI `/history` picker:
//! the conversation title on one line, and a relative timestamp with counts
//! below it. The full `/history` description line is the row's tooltip.

use bone_protocol::{ConversationMeta, ConversationStatus};
use eframe::egui;

use crate::grid::to_egui;
use crate::theme::CONTROL_RADIUS;

const PADDING: egui::Vec2 = egui::vec2(6.0, 3.0);
const LINE_GAP: f32 = 1.0;
/// Space left of the title for the running spinner or unread dot, reserved on
/// every row so titles stay aligned as indicators come and go.
const GUTTER: f32 = 14.0;
const DOT_RADIUS: f32 = 3.5;

/// What the desktop's open tabs know about a listed conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Activity {
    /// Open in a chat tab.
    pub open: bool,
    /// A turn is in progress in that tab.
    pub running: bool,
    /// A turn finished while its tab was not being viewed.
    pub unread: bool,
}

/// One clickable two-line history row: a spinner (running) or green dot
/// (unread) beside the title, then the relative timestamp and counts. `now`
/// is Unix seconds for the timestamp.
pub(crate) fn row(
    ui: &mut egui::Ui,
    meta: &ConversationMeta,
    activity: Activity,
    theme: &bone_render::theme::Theme,
    now: i64,
) -> egui::Response {
    let visuals = ui.visuals().clone();
    let fg = visuals.text_color();
    let weak = visuals.weak_text_color();
    let color = |color| to_egui(color).unwrap_or(fg);
    let title_font = egui::TextStyle::Body.resolve(ui.style());
    let meta_font = egui::TextStyle::Small.resolve(ui.style());
    let width = ui.available_width();
    let wrap =
        egui::text::TextWrapping::truncate_at_width((width - 2.0 * PADDING.x - GUTTER).max(0.0));

    let title = display_title(meta);
    let mut title_job = egui::text::LayoutJob::default();
    let title_format = if activity.unread {
        egui::TextFormat::simple(
            egui::FontId::new(title_font.size, egui::FontFamily::Name("semibold".into())),
            fg,
        )
    } else {
        egui::TextFormat::simple(title_font, fg)
    };
    title_job.append(title, 0.0, title_format);
    title_job.wrap = wrap.clone();

    let when = format_when(&meta.updated_at, &meta.updated_at_local, now);
    let mut meta_job = egui::text::LayoutJob::default();
    let counts = color(theme.palette.warn);
    meta_job.append(
        &when,
        0.0,
        egui::TextFormat::simple(meta_font.clone(), weak),
    );
    let mut detail = |text: String| {
        meta_job.append(
            " · ",
            0.0,
            egui::TextFormat::simple(meta_font.clone(), weak),
        );
        meta_job.append(
            &text,
            0.0,
            egui::TextFormat::simple(meta_font.clone(), counts),
        );
    };
    detail(format!("{} msgs", meta.message_count.max(0)));
    if meta.token_count > 0 {
        detail(format!("{} tok", compact_count(meta.token_count)));
    }
    meta_job.wrap = wrap;

    let (title_galley, meta_galley) =
        ui.fonts_mut(|fonts| (fonts.layout_job(title_job), fonts.layout_job(meta_job)));
    let height = 2.0 * PADDING.y + title_galley.size().y + LINE_GAP + meta_galley.size().y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, activity.open, title)
    });
    if ui.is_rect_visible(rect) {
        let fill = if activity.open {
            Some(visuals.selection.bg_fill)
        } else if response.hovered() || response.has_focus() {
            Some(visuals.widgets.hovered.weak_bg_fill)
        } else {
            None
        };
        if let Some(fill) = fill {
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(CONTROL_RADIUS), fill);
        }
        let title_pos = rect.min + PADDING + egui::vec2(GUTTER, 0.0);
        let meta_pos = title_pos + egui::vec2(0.0, title_galley.size().y + LINE_GAP);
        let indicator = egui::Rect::from_min_size(
            rect.min + PADDING,
            egui::vec2(GUTTER, title_galley.size().y),
        );
        if activity.running {
            // `paint_at` keeps requesting repaints while it is visible.
            egui::Spinner::new()
                .size(GUTTER - 4.0)
                .color(color(theme.palette.accent))
                .paint_at(
                    ui,
                    egui::Rect::from_center_size(
                        indicator.center() - egui::vec2(2.0, 0.0),
                        egui::Vec2::splat(GUTTER - 4.0),
                    ),
                );
        } else if activity.unread {
            ui.painter().circle_filled(
                indicator.center() - egui::vec2(2.0, 0.0),
                DOT_RADIUS,
                color(theme.palette.good),
            );
        }
        ui.painter().galley(title_pos, title_galley, fg);
        ui.painter().galley(meta_pos, meta_galley, fg);
    }
    let mut hover = tooltip(meta, &when);
    if activity.running {
        hover.push_str("\nRunning");
    } else if activity.unread {
        hover.push_str("\nNew reply");
    }
    response.on_hover_text(hover)
}

fn display_title(meta: &ConversationMeta) -> &str {
    if meta.title.trim().is_empty() {
        "Untitled"
    } else {
        meta.title.as_str()
    }
}

/// The untruncated title over the `/history` description line.
fn tooltip(meta: &ConversationMeta, when: &str) -> String {
    let title = if meta.full_title.is_empty() {
        display_title(meta)
    } else {
        &meta.full_title
    };
    let mut parts = vec![
        when.to_string(),
        format!("{}/{}", meta.provider, meta.model),
        format!("#{}", meta.id),
        format!("{} messages", grouped_count(meta.message_count)),
        format!("{} tokens", grouped_count(meta.token_count)),
    ];
    match meta.status {
        ConversationStatus::Completed => parts.push("Completed".into()),
        ConversationStatus::Interrupted => parts.push("No response".into()),
        ConversationStatus::Empty => parts.push("Empty".into()),
        ConversationStatus::Unknown => {}
    }
    format!("{title}\n{}", parts.join(" · "))
}

/// `4210` → `4,210`.
fn grouped_count(value: i64) -> String {
    let digits = value.max(0).to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `950` → `950`, `4210` → `4.2k`, `1_250_000` → `1.3M`.
fn compact_count(value: i64) -> String {
    let value = value.max(0);
    let scaled = |divisor: f64, suffix: &str| {
        let scaled = value as f64 / divisor;
        if scaled < 10.0 {
            format!("{:.1}{suffix}", scaled).replace(&format!(".0{suffix}"), suffix)
        } else {
            format!("{}{suffix}", scaled.round() as i64)
        }
    };
    match value {
        0..1_000 => value.to_string(),
        1_000..1_000_000 => scaled(1_000.0, "k"),
        _ => scaled(1_000_000.0, "M"),
    }
}

/// Relative timestamp matching the TUI `/history` picker: `just now`, `5m ago`,
/// `3h ago` (same day), `Yesterday 14:22`, `Sep 3 14:22`, `2025-01-02 14:22`.
///
/// `utc` is the daemon's ISO-UTC `updated_at`; `local` is the same instant in
/// the daemon's local time. Their difference gives the local UTC offset, so
/// calendar days are compared in local time without a timezone database.
/// Older daemons send no `local`, which falls back to UTC.
pub(crate) fn format_when(utc: &str, local: &str, now: i64) -> String {
    let Some(epoch) = parse_naive(utc) else {
        return if local.is_empty() { utc } else { local }.to_string();
    };
    let age = (now - epoch).max(0);
    if age < 60 {
        return "just now".into();
    }
    if age < 3600 {
        return format!("{}m ago", age / 60);
    }
    let local_epoch = parse_naive(local).unwrap_or(epoch);
    let offset = local_epoch - epoch;
    let day = local_epoch.div_euclid(86_400);
    let today = (now + offset).div_euclid(86_400);
    let (year, month, date) = civil_from_days(day);
    let clock = local_epoch.rem_euclid(86_400);
    let time = format!("{:02}:{:02}", clock / 3600, clock % 3600 / 60);
    if day == today {
        format!("{}h ago", age / 3600)
    } else if day + 1 == today {
        format!("Yesterday {time}")
    } else if year == civil_from_days(today).0 {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!("{} {date} {time}", MONTHS[month as usize - 1])
    } else {
        format!("{year:04}-{month:02}-{date:02} {time}")
    }
}

/// Seconds since the epoch for `YYYY-MM-DD[T ]HH:MM[:SS]`, ignoring any
/// fraction or zone suffix.
fn parse_naive(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let text = value.get(range)?;
        text.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| text.parse().ok())
            .flatten()
    };
    if bytes.len() < 16
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b' ')
        || bytes[13] != b':'
    {
        return None;
    }
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute) = (number(11..13)?, number(14..16)?);
    let second = if bytes.get(16) == Some(&b':') {
        number(17..19)?
    } else {
        0
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-27 15:00:00 UTC.
    fn now() -> i64 {
        parse_naive("2026-09-27T15:00:00Z").unwrap()
    }

    #[test]
    fn civil_round_trips() {
        for days in [-1, 0, 59, 10_957, 20_723, 20_724] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(parse_naive("2000-03-01 00:00"), Some(11_017 * 86_400));
    }

    #[test]
    fn relative_times_follow_the_history_picker() {
        let when = |utc, local| format_when(utc, local, now());
        assert_eq!(when("2026-09-27T14:59:30Z", ""), "just now");
        assert_eq!(when("2026-09-27T14:45:00Z", ""), "15m ago");
        assert_eq!(when("2026-09-27T12:00:00Z", ""), "3h ago");
        assert_eq!(when("2026-09-26T09:05:00Z", ""), "Yesterday 09:05");
        assert_eq!(when("2026-09-03T14:22:00Z", ""), "Sep 3 14:22");
        assert_eq!(when("2025-01-02T14:22:00Z", ""), "2025-01-02 14:22");
        // Future stamps (clock skew) read as fresh, not negative.
        assert_eq!(when("2026-09-27T16:00:00Z", ""), "just now");
    }

    #[test]
    fn calendar_days_use_the_daemons_local_offset() {
        // UTC-7: 03:00 UTC on the 27th is 20:00 on the 26th locally, while
        // "now" is 08:00 on the 27th locally, so it is yesterday.
        assert_eq!(
            format_when("2026-09-27T03:00:00Z", "2026-09-26T20:00:00", now()),
            "Yesterday 20:00"
        );
        // UTC+10: 15:00 UTC on the 26th is 01:00 on the 27th locally, and now
        // is 01:00 on the 28th locally.
        assert_eq!(
            format_when("2026-09-26T15:00:00Z", "2026-09-27T01:00:00", now()),
            "Yesterday 01:00"
        );
    }

    #[test]
    fn unparseable_stamps_are_shown_verbatim() {
        assert_eq!(format_when("garbage", "", now()), "garbage");
        assert_eq!(format_when("garbage", "local", now()), "local");
    }

    #[test]
    fn counts_are_grouped_and_compacted() {
        assert_eq!(grouped_count(0), "0");
        assert_eq!(grouped_count(4_210), "4,210");
        assert_eq!(grouped_count(1_234_567), "1,234,567");
        assert_eq!(compact_count(950), "950");
        assert_eq!(compact_count(4_210), "4.2k");
        assert_eq!(compact_count(4_000), "4k");
        assert_eq!(compact_count(48_700), "49k");
        assert_eq!(compact_count(1_260_000), "1.3M");
    }

    #[test]
    fn tooltip_carries_the_full_history_description() {
        let meta = ConversationMeta {
            id: 7,
            title: "fix the flaky…".into(),
            full_title: "fix the flaky test".into(),
            updated_at: "2026-09-27T12:00:00Z".into(),
            updated_at_local: String::new(),
            message_count: 12,
            provider: "openai".into(),
            model: "gpt".into(),
            token_count: 4_210,
            status: ConversationStatus::Interrupted,
        };
        assert_eq!(
            tooltip(&meta, "3h ago"),
            "fix the flaky test\n3h ago · openai/gpt · #7 · 12 messages · 4,210 tokens · No response"
        );
    }
}
