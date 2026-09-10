//! Native renderer for daemon usage statistics (Phase 6).
//!
//! The daemon answers `HostRequest::Stats` with a `UsageStatsSnapshot`, already
//! stored by the caller. This module maps that snapshot onto egui widgets inside
//! the Usage screen: summary cards, a time-series chart, a model table, and
//! optional activity heatmaps. It changes no
//! protocol types; view-index changes and refreshes are returned to the caller.

use bone_protocol::{
    DateRange, HourUsage, ProviderUsage, UsageBucket, UsageStatsSnapshot, UsageSummary,
};
use eframe::egui;

use crate::theme::{self, Palette};

/// Number of steps in the heat gradient (mirrors the TUI's `HEAT_LEVELS`).
pub const HEAT_LEVELS: usize = 15;

/// Frontend-local view titles, indexed to match
/// `UsageStatsSnapshot::{buckets,range_models}` (0 today … 4 all time).
pub const MODES: [&str; 5] = ["Today", "7 days", "4 weeks", "Yearly", "All time"];

/// A user action emitted by the Stats dialog, applied by the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatsAction {
    /// Re-request the snapshot from the daemon.
    Refresh,
    /// Switch the frontend-local view index (0..5).
    SetMode(usize),
    /// Open the custom date-range picker.
    OpenDatePicker,
}

/// Compact integer, mirroring the TUI's `compact_number`.
pub fn compact_number(count: u64) -> String {
    if count < 100_000 {
        return count.to_string();
    }
    let (value, suffix) = if count < 1_000_000 {
        (count as f64 / 1_000.0, "k")
    } else if count < 1_000_000_000 {
        (count as f64 / 1_000_000.0, "m")
    } else {
        (count as f64 / 1_000_000_000.0, "b")
    };
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}{suffix}")
    } else {
        format!("{rounded:.1}{suffix}")
    }
}

/// Total tokens recorded in a bucket (prompt + completion).
pub fn bucket_tokens(bucket: &UsageBucket) -> i64 {
    bucket.prompt_tokens + bucket.completion_tokens
}

/// Percent of prompt tokens served from cache, or 0 when no prompt tokens.
pub fn cache_percent(summary: &UsageSummary) -> i64 {
    if summary.prompt_tokens > 0 {
        (summary.cached_tokens as f64 / summary.prompt_tokens as f64 * 100.0).round() as i64
    } else {
        0
    }
}

fn color(value: &Option<String>, fallback: egui::Color32) -> egui::Color32 {
    value
        .as_deref()
        .and_then(theme::parse_color)
        .unwrap_or(fallback)
}

/// Frontend-local view state for one stats frame: the selected view index, the
/// applied custom range (if any), the snapshot age for the "refreshed" label,
/// and the resolved heat gradient.
pub struct StatsView<'a> {
    pub mode: usize,
    pub custom: Option<&'a DateRange>,
    pub refreshed_secs: Option<u64>,
    pub heat: &'a HeatScale,
}

/// Precomputed heat gradient from `heat_low` to `heat_high`, mirroring the TUI's
/// `HeatScale`. Cells with no usage paint the empty (`palette.subtle`) color.
pub struct HeatScale {
    colors: [egui::Color32; HEAT_LEVELS],
    empty: egui::Color32,
}

impl HeatScale {
    pub fn new(low: egui::Color32, high: egui::Color32, empty: egui::Color32) -> Self {
        let colors = std::array::from_fn(|index| {
            let t = index as f32 / (HEAT_LEVELS - 1) as f32;
            let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
            egui::Color32::from_rgb(
                lerp(low.r(), high.r()),
                lerp(low.g(), high.g()),
                lerp(low.b(), high.b()),
            )
        });
        Self { colors, empty }
    }

    /// Color for a cell of `tokens` against the column `max`.
    pub fn color(&self, tokens: i64, max: i64) -> egui::Color32 {
        if tokens <= 0 {
            return self.empty;
        }
        let ratio = tokens as f64 / max.max(1) as f64;
        let index = ((ratio * HEAT_LEVELS as f64).ceil() as usize).saturating_sub(1);
        self.colors[index.min(HEAT_LEVELS - 1)]
    }
}

/// Range label for a view index, mirroring the TUI's `range_label`: the first
/// and last bucket labels joined by `→`, a single label, or a placeholder.
fn range_label(snapshot: &UsageStatsSnapshot, mode: usize) -> String {
    let buckets = snapshot.buckets(mode);
    let first = buckets
        .first()
        .map(|bucket| bucket.label.as_str())
        .unwrap_or("");
    let last = buckets
        .last()
        .map(|bucket| bucket.label.as_str())
        .unwrap_or("");
    if first.is_empty() {
        return "no usage events yet".to_string();
    }
    if first == last {
        first.to_string()
    } else {
        format!("{first} → {last}")
    }
}

/// Monday-based weekday index for a validated Gregorian date.
fn weekday_index(date: &str) -> Option<usize> {
    Some((date_number(date)? - 1).rem_euclid(7) as usize)
}

fn weekday_label(row: usize) -> &'static str {
    match row {
        0 => "Mon ",
        1 => "Tue ",
        2 => "Wed ",
        3 => "Thu ",
        4 => "Fri ",
        5 => "Sat ",
        _ => "Sun ",
    }
}

/// Render a dashboard with fixed range controls and one scrolling content area.
pub fn render(
    ui: &mut egui::Ui,
    snapshot: &UsageStatsSnapshot,
    view: &StatsView,
    palette: &Palette,
) -> Option<StatsAction> {
    let mut action = None;
    let accent = color(&palette.accent, ui.visuals().hyperlink_color);
    ui.horizontal_wrapped(|ui| {
        for (index, title) in MODES.iter().enumerate() {
            if ui
                .selectable_label(view.custom.is_none() && view.mode == index, *title)
                .clicked()
            {
                action = Some(StatsAction::SetMode(index));
            }
        }
        if ui
            .selectable_label(view.custom.is_some(), "Custom range")
            .clicked()
        {
            action = Some(StatsAction::OpenDatePicker);
        }
        if ui.button("Refresh").clicked() {
            action = Some(StatsAction::Refresh);
        }
    });
    let range = match view.custom {
        Some(range) => format!(
            "{} – {}",
            if range.start == "0000-01-01" {
                "Beginning"
            } else {
                &range.start
            },
            if range.end == "9999-12-31" {
                "Present"
            } else {
                &range.end
            }
        ),
        None => range_label(snapshot, view.mode),
    };
    ui.horizontal_wrapped(|ui| {
        ui.weak(range);
        if let Some(seconds) = view.refreshed_secs {
            ui.weak(if seconds < 60 {
                "Updated just now".into()
            } else {
                format!("Updated {} min ago", seconds / 60)
            });
        }
    });
    let summary = if view.custom.is_some() {
        snapshot.total.clone()
    } else {
        snapshot.range_summary(view.mode)
    };
    let buckets = if view.custom.is_some() {
        snapshot.daily.as_slice()
    } else {
        snapshot.buckets(view.mode)
    };
    egui::ScrollArea::vertical()
        .id_salt("usage-content")
        .max_height(ui.available_height())
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(8.0);
            let metrics = [
                (
                    "Total tokens",
                    compact_number(
                        (summary.prompt_tokens + summary.completion_tokens).max(0) as u64
                    ),
                ),
                (
                    "Requests",
                    compact_number(summary.request_count.max(0) as u64),
                ),
                ("Cache hit rate", format!("{}%", cache_percent(&summary))),
                (
                    "Output tokens",
                    compact_number(summary.completion_tokens.max(0) as u64),
                ),
            ];
            let columns = if ui.available_width() >= 620.0 { 4 } else { 2 };
            for row in metrics.chunks(columns) {
                ui.columns(columns, |columns| {
                    for (column, (label, value)) in columns.iter_mut().zip(row) {
                        egui::Frame::new()
                            .fill(column.visuals().widgets.inactive.weak_bg_fill)
                            .corner_radius(8)
                            .inner_margin(16)
                            .show(column, |ui| {
                                ui.set_min_width((ui.available_width() - 1.0).max(20.0));
                                ui.weak(*label);
                                ui.label(egui::RichText::new(value).size(26.0).strong());
                            });
                    }
                });
                ui.add_space(4.0);
            }
            ui.weak(format!(
                "{} input tokens · {} served from cache",
                compact_number(summary.prompt_tokens.max(0) as u64),
                compact_number(summary.cached_tokens.max(0) as u64)
            ));
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Token usage").size(18.0).strong());
            if buckets.is_empty() {
                crate::surface::empty(
                    ui,
                    "No usage in this period",
                    "Usage appears here after a model request completes.",
                );
            } else {
                render_chart(ui, buckets, accent);
            }
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Models").size(18.0).strong());
            let models = if view.custom.is_some() {
                snapshot.by_model_today.as_slice()
            } else {
                snapshot.range_models(view.mode)
            };
            render_models(ui, models);
            ui.add_space(24.0);
            ui.label(egui::RichText::new("Activity patterns").size(18.0).strong());
            let hourly = if view.custom.is_some() {
                &snapshot.hourly_today
            } else {
                snapshot.hourly(view.mode)
            };
            render_activity(ui, hourly, &snapshot.daily_activity, view.heat, accent);
        });
    action
}

fn render_chart(ui: &mut egui::Ui, buckets: &[UsageBucket], accent: egui::Color32) {
    let width = ui.available_width();
    let capacity = ((width - 56.0) / 10.0).max(1.0) as usize;
    let chunks: Vec<_> = buckets.chunks(buckets.len().div_ceil(capacity)).collect();
    let max = chunks
        .iter()
        .map(|c| c.iter().map(bucket_tokens).sum::<i64>())
        .max()
        .unwrap_or(1)
        .max(1);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 218.0), egui::Sense::hover());
    let plot = egui::Rect::from_min_max(
        rect.min + egui::vec2(52.0, 12.0),
        rect.max - egui::vec2(2.0, 28.0),
    );
    let muted = ui.visuals().weak_text_color();
    let font = egui::FontId::proportional(11.0);
    for fraction in [0.0, 0.5, 1.0] {
        let y = plot.bottom() - plot.height() * fraction;
        ui.painter().hline(
            plot.x_range(),
            y,
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        ui.painter().text(
            egui::pos2(plot.left() - 8.0, y),
            egui::Align2::RIGHT_CENTER,
            compact_number((max as f32 * fraction) as u64),
            font.clone(),
            muted,
        );
    }
    let stride = plot.width() / chunks.len().max(1) as f32;
    for (index, chunk) in chunks.iter().enumerate() {
        let tokens: i64 = chunk.iter().map(bucket_tokens).sum();
        let requests: i64 = chunk.iter().map(|b| b.request_count).sum();
        let x = plot.left() + index as f32 * stride;
        let height = plot.height() * (tokens.max(0) as f32 / max as f32);
        let bar = egui::Rect::from_min_max(
            egui::pos2(x + 2.0, plot.bottom() - height),
            egui::pos2(x + stride - 2.0, plot.bottom()),
        );
        let hit = egui::Rect::from_min_max(
            egui::pos2(x, plot.top()),
            egui::pos2(x + stride, plot.bottom()),
        );
        let range = if chunk.len() == 1 {
            chunk[0].label.clone()
        } else {
            format!("{} – {}", chunk[0].label, chunk.last().unwrap().label)
        };
        let response = ui
            .interact(hit, ui.id().with(("bucket", index)), egui::Sense::hover())
            .on_hover_text(format!("{range}\n{tokens} tokens · {requests} requests"));
        if tokens > 0 {
            ui.painter().rect_filled(
                bar,
                3.0,
                if response.hovered() {
                    accent.gamma_multiply(1.2)
                } else {
                    accent
                },
            );
        }
    }
    for (label, pos, align) in [
        (
            &buckets[0].label,
            plot.left_bottom() + egui::vec2(0.0, 10.0),
            egui::Align2::LEFT_TOP,
        ),
        (
            &buckets[buckets.len() - 1].label,
            plot.right_bottom() + egui::vec2(0.0, 10.0),
            egui::Align2::RIGHT_TOP,
        ),
    ] {
        ui.painter().text(pos, align, label, font.clone(), muted);
    }
}

/// Exact, grouped values keep table columns readable without hiding precision.
fn grouped_number(value: i64) -> String {
    let digits = value.max(0).to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

fn axis_number(value: i64) -> String {
    if (1_000..100_000).contains(&value) {
        format!("{:.1}k", value as f64 / 1_000.0).replace(".0k", "k")
    } else {
        compact_number(value.max(0) as u64)
    }
}

fn model_cache(model: &ProviderUsage) -> String {
    if model.prompt_tokens > 0 {
        format!(
            "{}%",
            (model.cached_tokens as f64 / model.prompt_tokens as f64 * 100.0).round() as i64
        )
    } else {
        "—".into()
    }
}

/// Full-width rows reserve space for the model name and right-align all numbers.
/// Narrow windows use labeled metrics under each model instead of a squeezed table.
pub(super) fn render_models(ui: &mut egui::Ui, models: &[ProviderUsage]) {
    if models.is_empty() {
        ui.weak("No model usage in this period.");
        return;
    }
    let mut models: Vec<_> = models.iter().collect();
    models.sort_by_key(|model| std::cmp::Reverse(model.prompt_tokens + model.completion_tokens));
    let width = ui.available_width();
    if width < 620.0 {
        for model in models {
            activity_panel(ui, |ui| {
                ui.label(egui::RichText::new(&model.model).strong())
                    .on_hover_text(&model.model);
                ui.weak(&model.provider);
                ui.add_space(6.0);
                ui.columns(3, |columns| {
                    columns[0].weak("Tokens");
                    columns[0].label(
                        egui::RichText::new(grouped_number(
                            model.prompt_tokens + model.completion_tokens,
                        ))
                        .strong(),
                    );
                    columns[1].weak("Requests");
                    columns[1]
                        .label(egui::RichText::new(grouped_number(model.request_count)).strong());
                    columns[2].weak("Cache hit");
                    columns[2].label(egui::RichText::new(model_cache(model)).strong());
                });
            });
            ui.add_space(6.0);
        }
        return;
    }
    let numeric_width = ((width - 34.0) * 0.18).clamp(100.0, 170.0);
    let model_width = width - 34.0 - numeric_width * 3.0;
    let row = |ui: &mut egui::Ui, model: Option<&ProviderUsage>, shaded: bool| {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 12))
            .fill(if shaded {
                ui.visuals().faint_bg_color
            } else {
                egui::Color32::TRANSPARENT
            })
            .show(ui, |ui| {
                ui.set_width(width - 34.0);
                ui.spacing_mut().item_spacing.x = 0.0;
                let height = if model.is_some() { 42.0 } else { 20.0 };
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(model_width, height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(model_width);
                            if let Some(model) = model {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(&model.model).strong())
                                        .truncate(),
                                )
                                .on_hover_text(&model.model);
                                ui.add(
                                    egui::Label::new(egui::RichText::new(&model.provider).weak())
                                        .truncate(),
                                )
                                .on_hover_text(&model.provider);
                            } else {
                                ui.strong("Model / provider");
                            }
                        },
                    );
                    let values = match model {
                        Some(model) => [
                            grouped_number(model.prompt_tokens + model.completion_tokens),
                            grouped_number(model.request_count),
                            model_cache(model),
                        ],
                        None => ["Tokens".into(), "Requests".into(), "Cache hit".into()],
                    };
                    for value in values {
                        ui.allocate_ui_with_layout(
                            egui::vec2(numeric_width, height),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.set_width(numeric_width);
                                ui.label(egui::RichText::new(value).size(14.0).strong());
                            },
                        );
                    }
                });
            });
    };
    egui::Frame::new()
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_width(width - 2.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            row(ui, None, true);
            for (index, model) in models.into_iter().enumerate() {
                let y = ui.next_widget_position().y;
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    y,
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );
                row(ui, Some(model), index % 2 == 1);
            }
        });
    ui.add_space(6.0);
    ui.weak("Sorted by tokens used · Cache hit is the share of input tokens served from cache.");
}

fn activity_panel(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width();
    egui::Frame::new()
        .inner_margin(16)
        .corner_radius(8)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .show(ui, |ui| {
            ui.set_width((width - 34.0).max(80.0));
            content(ui);
        });
}

/// Hourly volume is encoded by height, with a labeled scale and exact hover values.
fn render_hourly(ui: &mut egui::Ui, hourly: &[HourUsage], accent: egui::Color32) {
    ui.strong("Usage by hour");
    ui.weak("Total tokens at each hour of the day · selected period");
    let mut values = [0i64; 24];
    let mut requests = [0i64; 24];
    for entry in hourly {
        if (0..24).contains(&entry.hour) {
            values[entry.hour as usize] += entry.prompt_tokens + entry.completion_tokens;
            requests[entry.hour as usize] += entry.request_count;
        }
    }
    let max = values.iter().copied().max().unwrap_or(0);
    if max <= 0 {
        ui.add_space(12.0);
        ui.weak("No hourly usage in this period.");
        return;
    }
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 190.0),
        egui::Sense::hover(),
    );
    let plot = egui::Rect::from_min_max(
        rect.min + egui::vec2(52.0, 12.0),
        rect.max - egui::vec2(5.0, 32.0),
    );
    let label_color = ui.visuals().text_color();
    let font = egui::FontId::proportional(13.0);
    for fraction in [0.0, 0.5, 1.0] {
        let y = plot.bottom() - plot.height() * fraction;
        ui.painter().hline(
            plot.x_range(),
            y,
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        ui.painter().text(
            egui::pos2(plot.left() - 10.0, y),
            egui::Align2::RIGHT_CENTER,
            axis_number((max as f32 * fraction).round() as i64),
            font.clone(),
            label_color,
        );
    }
    let stride = plot.width() / 24.0;
    for (hour, tokens) in values.iter().enumerate() {
        let x = plot.left() + hour as f32 * stride;
        let height = plot.height() * (*tokens as f32 / max as f32);
        let bar = egui::Rect::from_min_max(
            egui::pos2(x + 1.0, plot.bottom() - height),
            egui::pos2(x + stride - 2.0, plot.bottom()),
        );
        let hit = egui::Rect::from_min_max(
            egui::pos2(x, plot.top()),
            egui::pos2(x + stride, plot.bottom()),
        );
        let response = ui
            .interact(hit, ui.id().with(("hour", hour)), egui::Sense::hover())
            .on_hover_text(format!(
                "{hour:02}:00–{hour:02}:59\n{} tokens · {} requests",
                grouped_number(*tokens),
                grouped_number(requests[hour])
            ));
        if *tokens > 0 {
            ui.painter().rect_filled(
                bar,
                2.0,
                if response.hovered() {
                    accent.gamma_multiply(1.15)
                } else {
                    accent
                },
            );
        }
        let tick = if plot.width() < 400.0 {
            hour % 6 == 0
        } else {
            hour % 3 == 0
        };
        if tick {
            ui.painter().text(
                egui::pos2(x + stride / 2.0, plot.bottom() + 10.0),
                egui::Align2::CENTER_TOP,
                format!("{hour:02}:00"),
                font.clone(),
                label_color,
            );
        }
    }
    if let Some((hour, tokens)) = values.iter().enumerate().max_by_key(|(_, tokens)| *tokens) {
        ui.label(format!(
            "Busiest hour: {hour:02}:00 · {} tokens",
            grouped_number(*tokens)
        ));
    }
}

#[derive(Clone, Copy)]
struct CalendarWindow {
    first: i64,
    last: i64,
    monday: i64,
    weeks: usize,
}

fn calendar_window(activity: &[UsageBucket], limit_weeks: Option<usize>) -> Option<CalendarWindow> {
    let dates: Vec<_> = activity
        .iter()
        .filter_map(|bucket| date_number(&bucket.label))
        .collect();
    let last = *dates.iter().max()?;
    let first = *dates.iter().min()?;
    let first = limit_weeks.map_or(first, |weeks| first.max(last - (weeks as i64 * 7) + 1));
    let monday = first - weekday_index(&calendar_date(first))? as i64;
    Some(CalendarWindow {
        first,
        last,
        monday,
        weeks: ((last - monday) / 7 + 1) as usize,
    })
}

fn calendar_date(number: i64) -> String {
    // Gregorian civil date from the same day number used by date_number.
    let z = number + 305;
    let era = z / 146097;
    let day_of_era = z - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn month_label(date: &str) -> String {
    let month = date[5..7].parse::<usize>().unwrap_or(1);
    let name = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][month - 1];
    format!("{name} {}", &date[..4])
}

fn render_calendar(ui: &mut egui::Ui, activity: &[UsageBucket], heat: &HeatScale) {
    ui.strong("Daily activity");
    let id = ui.id().with("calendar-weeks");
    let mut weeks = ui.data_mut(|data| *data.get_temp_mut_or(id, Some(12usize)));
    ui.horizontal_wrapped(|ui| {
        for (value, label) in [
            (Some(12), "12 weeks"),
            (Some(26), "26 weeks"),
            (None, "Available history"),
        ] {
            ui.selectable_value(&mut weeks, value, label);
        }
    });
    ui.data_mut(|data| data.insert_temp(id, weeks));
    let Some(window) = calendar_window(activity, weeks) else {
        ui.weak("No daily activity yet.");
        return;
    };
    ui.label(format!(
        "{} – {}",
        calendar_date(window.first),
        calendar_date(window.last)
    ));
    ui.weak("Each square is one day. Hover for the date, tokens, and requests.");
    let entries: std::collections::HashMap<_, _> = activity
        .iter()
        .filter_map(|bucket| Some((date_number(&bucket.label)?, bucket)))
        .collect();
    let max = entries
        .iter()
        .filter(|(day, _)| **day >= window.first && **day <= window.last)
        .map(|(_, bucket)| bucket_tokens(bucket))
        .max()
        .unwrap_or(0)
        .max(0);
    let stride = ((ui.available_width() - 52.0) / window.weeks as f32).clamp(22.0, 30.0);
    let label_color = ui.visuals().text_color();
    let font = egui::FontId::proportional(13.0);
    egui::ScrollArea::horizontal()
        .id_salt(("daily-activity-scroll", weeks))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(window.weeks as f32 * stride + 52.0, 32.0 + 7.0 * stride),
                egui::Sense::hover(),
            );
            for row in 0..7 {
                ui.painter().text(
                    rect.min + egui::vec2(0.0, 32.0 + row as f32 * stride + (stride - 4.0) / 2.0),
                    egui::Align2::LEFT_CENTER,
                    weekday_label(row).trim(),
                    font.clone(),
                    label_color,
                );
            }
            let mut last_month = String::new();
            let mut label_end = f32::NEG_INFINITY;
            for column in 0..window.weeks {
                let day = (window.monday + column as i64 * 7).max(window.first);
                let date = calendar_date(day);
                let month = &date[..7];
                let x = rect.left() + 52.0 + column as f32 * stride;
                if month != last_month {
                    // Short month labels stay attached to their actual week;
                    // add the year at January when browsing across years.
                    let full = month_label(&date);
                    let label = if &date[5..7] == "01" && window.weeks > 3 {
                        full.clone()
                    } else {
                        full.split_whitespace().next().unwrap().to_string()
                    };
                    let galley = ui
                        .painter()
                        .layout_no_wrap(label, font.clone(), label_color);
                    let label_x = x.min(rect.right() - galley.size().x);
                    if label_x >= label_end {
                        label_end = label_x + galley.size().x + 6.0;
                        ui.painter()
                            .galley(egui::pos2(label_x, rect.top()), galley, label_color);
                    }
                    last_month = month.into();
                }
                for row in 0..7 {
                    let day = window.monday + column as i64 * 7 + row;
                    if day < window.first || day > window.last {
                        continue;
                    }
                    let bucket = entries.get(&day);
                    let tokens = bucket.map_or(0, |b| bucket_tokens(b));
                    let requests = bucket.map_or(0, |b| b.request_count);
                    let cell = egui::Rect::from_min_size(
                        egui::pos2(x, rect.top() + 32.0 + row as f32 * stride),
                        egui::vec2(stride - 4.0, stride - 4.0),
                    );
                    ui.painter().rect_filled(cell, 3.0, heat.color(tokens, max));
                    let response = ui
                        .interact(cell, ui.id().with(day), egui::Sense::hover())
                        .on_hover_text(format!(
                            "{}\n{} tokens · {} requests",
                            calendar_date(day),
                            grouped_number(tokens),
                            grouped_number(requests)
                        ));
                    ui.painter().rect_stroke(
                        cell,
                        3.0,
                        if response.hovered() {
                            egui::Stroke::new(1.5, label_color)
                        } else {
                            ui.visuals().widgets.noninteractive.bg_stroke
                        },
                        egui::StrokeKind::Inside,
                    );
                }
            }
        });
    if max == 0 {
        ui.weak("No tokens recorded in these dates.");
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Tokens per day");
        for (value, label) in [
            (0, "0".into()),
            (max / 2, axis_number(max / 2)),
            (max, axis_number(max)),
        ] {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 3.0, heat.color(value, max));
            ui.label(label);
        }
    });
}

/// The same content function is used by the page and the headless visual checks.
pub(super) fn render_activity(
    ui: &mut egui::Ui,
    hourly: &[HourUsage],
    activity: &[UsageBucket],
    heat: &HeatScale,
    accent: egui::Color32,
) {
    activity_panel(ui, |ui| render_hourly(ui, hourly, accent));
    ui.add_space(12.0);
    activity_panel(ui, |ui| render_calendar(ui, activity, heat));
}

/// Validate real calendar dates before sending a range request.
fn date_number(date: &str) -> Option<i64> {
    if date.len() != 10 || date.as_bytes()[4] != b'-' || date.as_bytes()[7] != b'-' {
        return None;
    }
    if !date
        .bytes()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    let year = date.get(0..4)?.parse::<i64>().ok()?;
    let month = date.get(5..7)?.parse::<usize>().ok()?;
    let day = date.get(8..10)?.parse::<i64>().ok()?;
    if year < 1 || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day < 1 || day > days[month - 1] {
        return None;
    }
    let before = year - 1;
    Some(
        before * 365 + before / 4 - before / 100
            + before / 400
            + days[..month - 1].iter().sum::<i64>()
            + day,
    )
}

pub fn custom_range(start: &str, end: &str) -> Result<DateRange, String> {
    let (start, end) = (start.trim(), end.trim());
    for (label, value) in [("Start", start), ("End", end)] {
        if !value.is_empty() && date_number(value).is_none() {
            return Err(format!("{label}: enter a valid date as YYYY-MM-DD."));
        }
    }
    if !start.is_empty() && !end.is_empty() && start > end {
        return Err("End date must be on or after the start date.".into());
    }
    Ok(DateRange {
        start: if start.is_empty() {
            "0000-01-01".into()
        } else {
            start.into()
        },
        end: if end.is_empty() {
            "9999-12-31".into()
        } else {
            end.into()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_window_is_recent_and_dates_survive_leap_days() {
        for date in [
            "0001-01-01",
            "1970-01-01",
            "2000-02-29",
            "2024-02-29",
            "2026-09-10",
            "9999-12-31",
        ] {
            assert_eq!(calendar_date(date_number(date).unwrap()), date);
        }
        let mut activity = Vec::new();
        for date in ["2024-01-01", "2026-09-10", "2026-08-10"] {
            activity.push(UsageBucket {
                label: date.into(),
                prompt_tokens: 10,
                completion_tokens: 0,
                cached_tokens: 0,
                cost: 0.0,
                request_count: 1,
            });
        }
        let recent = calendar_window(&activity, Some(12)).unwrap();
        assert_eq!(recent.last - recent.first + 1, 84);
        assert_eq!(weekday_index(&calendar_date(recent.monday)), Some(0));
        let all = calendar_window(&activity, None).unwrap();
        assert_eq!(calendar_date(all.first), "2024-01-01");
        assert_eq!(calendar_date(all.last), "2026-09-10");
    }

    #[test]
    fn custom_range_rejects_invalid_dates_and_reversed_ranges() {
        for date in [
            "2026-02-29",
            "2026-04-31",
            "2026-13-01",
            "2026-01-00",
            "+026-01-01",
            "2026-1-01",
            "bad",
        ] {
            assert!(custom_range(date, "").is_err(), "accepted {date}");
        }
        assert!(custom_range("2024-02-29", "2024-03-01").is_ok());
        assert!(custom_range("2026-09-10", "2026-09-01").is_err());
        let range = custom_range("", "").unwrap();
        assert_eq!(range.start, "0000-01-01");
        assert_eq!(range.end, "9999-12-31");
        assert_eq!(
            date_number("2024-03-01").unwrap() - date_number("2024-02-28").unwrap(),
            2
        );
    }
    use bone_protocol::{ProviderUsage, UsageBucket};

    fn empty_snapshot() -> UsageStatsSnapshot {
        UsageStatsSnapshot {
            started_at: None,
            ended_at: None,
            total: UsageSummary::default(),
            by_model_today: Vec::new(),
            by_model_7d: Vec::new(),
            by_model_4w: Vec::new(),
            by_model_all: Vec::new(),
            daily: Vec::new(),
            weekly: Vec::new(),
            monthly: Vec::new(),
            all_time: Vec::new(),
            yearly: Vec::new(),
            hourly_today: Vec::new(),
            hourly_7d: Vec::new(),
            hourly_4w: Vec::new(),
            hourly_all: Vec::new(),
            daily_activity: Vec::new(),
        }
    }

    #[test]
    fn compact_number_matches_tui_buckets() {
        assert_eq!(compact_number(0), "0");
        assert_eq!(compact_number(999), "999");
        assert_eq!(compact_number(99_999), "99999");
        assert_eq!(compact_number(100_000), "100k");
        assert_eq!(compact_number(1_500_000), "1.5m");
        assert_eq!(compact_number(2_000_000_000), "2b");
    }

    #[test]
    fn cache_percent_handles_zero_prompt_tokens() {
        assert_eq!(cache_percent(&UsageSummary::default()), 0);
        let summary = UsageSummary {
            prompt_tokens: 100,
            cached_tokens: 50,
            ..UsageSummary::default()
        };
        assert_eq!(cache_percent(&summary), 50);
    }

    #[test]
    fn bucket_tokens_sums_prompt_and_completion() {
        let bucket = UsageBucket {
            label: "2026-01-01".into(),
            prompt_tokens: 30,
            completion_tokens: 12,
            cached_tokens: 0,
            cost: 0.0,
            request_count: 1,
        };
        assert_eq!(bucket_tokens(&bucket), 42);
    }

    #[test]
    fn render_without_clicks_returns_none() {
        let ctx = egui::Context::default();
        let mut snapshot = empty_snapshot();
        snapshot.weekly = vec![UsageBucket {
            label: "2026-W01".into(),
            prompt_tokens: 120,
            completion_tokens: 40,
            cached_tokens: 10,
            cost: 0.1,
            request_count: 3,
        }];
        snapshot.by_model_7d = vec![ProviderUsage {
            provider: "local".into(),
            model: "m".into(),
            prompt_tokens: 120,
            completion_tokens: 40,
            cached_tokens: 10,
            cost: 0.1,
            request_count: 3,
        }];
        let palette = Palette::default();
        let heat = HeatScale::new(
            egui::Color32::from_gray(80),
            egui::Color32::from_rgb(35, 209, 139),
            egui::Color32::from_gray(90),
        );
        let view = StatsView {
            mode: 1,
            custom: None,
            refreshed_secs: Some(3),
            heat: &heat,
        };
        let mut action = None;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            action = render(ui, &snapshot, &view, &palette);
        })
        .textures_delta
        .clear();
        assert_eq!(action, None);
    }

    #[test]
    fn render_with_custom_range_and_heatmaps_returns_none() {
        // A range reply populates total/daily/hourly_today/by_model_today and
        // daily_activity; rendering it must exercise the heat strip and calendar.
        let ctx = egui::Context::default();
        let mut snapshot = empty_snapshot();
        snapshot.total = UsageSummary {
            prompt_tokens: 200,
            completion_tokens: 80,
            cached_tokens: 50,
            cost: 0.2,
            request_count: 4,
        };
        snapshot.daily = vec![UsageBucket {
            label: "2026-01-01".into(),
            prompt_tokens: 200,
            completion_tokens: 80,
            cached_tokens: 50,
            cost: 0.2,
            request_count: 4,
        }];
        snapshot.hourly_today = vec![HourUsage {
            hour: 9,
            prompt_tokens: 200,
            completion_tokens: 80,
            cached_tokens: 50,
            request_count: 4,
        }];
        snapshot.by_model_today = vec![ProviderUsage {
            provider: "local".into(),
            model: "m".into(),
            prompt_tokens: 200,
            completion_tokens: 80,
            cached_tokens: 50,
            cost: 0.2,
            request_count: 4,
        }];
        snapshot.daily_activity = snapshot.daily.clone();
        let palette = Palette::default();
        let heat = HeatScale::new(
            egui::Color32::from_gray(80),
            egui::Color32::from_rgb(35, 209, 139),
            egui::Color32::from_gray(90),
        );
        let range = DateRange {
            start: "2026-01-01".into(),
            end: "2026-01-31".into(),
        };
        let view = StatsView {
            mode: 1,
            custom: Some(&range),
            refreshed_secs: None,
            heat: &heat,
        };
        let mut action = None;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            action = render(ui, &snapshot, &view, &palette);
        })
        .textures_delta
        .clear();
        assert_eq!(action, None);
    }

    #[test]
    fn heat_scale_maps_zero_to_empty_and_max_to_high() {
        let low = egui::Color32::from_gray(40);
        let high = egui::Color32::from_rgb(200, 40, 40);
        let empty = egui::Color32::from_gray(90);
        let heat = HeatScale::new(low, high, empty);
        assert_eq!(heat.color(0, 100), empty);
        // The top of the gradient reaches the high endpoint.
        assert_eq!(heat.color(100, 100), high);
        // Mid-range lands strictly between the endpoints.
        let mid = heat.color(50, 100);
        assert!(mid.r() > low.r() && mid.r() < high.r());
    }

    #[test]
    fn range_label_reports_empty_single_and_span() {
        let mut snapshot = empty_snapshot();
        assert_eq!(range_label(&snapshot, 1), "no usage events yet");
        snapshot.weekly = vec![UsageBucket {
            label: "2026-W01".into(),
            ..UsageBucket {
                label: String::new(),
                prompt_tokens: 0,
                completion_tokens: 0,
                cached_tokens: 0,
                cost: 0.0,
                request_count: 0,
            }
        }];
        assert_eq!(range_label(&snapshot, 1), "2026-W01");
        snapshot.weekly.push(UsageBucket {
            label: "2026-W02".into(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cost: 0.0,
            request_count: 0,
        });
        assert_eq!(range_label(&snapshot, 1), "2026-W01 → 2026-W02");
    }

    #[test]
    fn weekday_index_is_monday_based() {
        // 2026-01-01 is a Thursday (Mon=0 … Sun=6).
        assert_eq!(weekday_index("2026-01-01"), Some(3));
        // 2026-01-04 is a Sunday.
        assert_eq!(weekday_index("2026-01-04"), Some(6));
        // 2026-01-05 is a Monday.
        assert_eq!(weekday_index("2026-01-05"), Some(0));
        assert_eq!(weekday_index("bogus"), None);
        assert_eq!(weekday_index("2026-13-01"), None);
    }
}
