use super::*;
use crate::theme::Theme;

#[test]
fn distinct_theme_produces_different_heat_gradient() {
    let mut theme = Theme::default();
    theme.heat_low = Color::Rgb(0, 0, 50);
    theme.heat_high = Color::Rgb(0, 100, 255);

    let gradient = build_heat_gradient(&theme);

    assert_eq!(gradient.len(), 15);
    assert_eq!(gradient[0], theme.heat_low);
    assert_eq!(gradient[14], theme.heat_high);
    assert_ne!(gradient, build_heat_gradient(&Theme::default()));
}

#[test]
fn chart_line_uses_chart_and_chart_empty_roles() {
    let mut theme = Theme::default();
    theme.chart = Color::Rgb(1, 2, 3);
    theme.chart_empty = Color::Rgb(4, 5, 6);
    let bucket = UsageBucket {
        label: "today".into(),
        prompt_tokens: 25,
        completion_tokens: 25,
        cached_tokens: 0,
        cost: 0.0,
        request_count: 1,
    };

    let line = usage_chart_line(&bucket, 100, 10, &theme);

    assert_eq!(line.spans[1].style.fg, Some(theme.chart));
    assert_eq!(line.spans[2].style.fg, Some(theme.chart_empty));
    assert_eq!(line.spans[1].content, "█████");
    assert_eq!(line.spans[2].content, "░░░░░");
}

#[test]
fn non_rgb_heat_endpoints_repeat_the_semantic_fallback() {
    let mut theme = Theme::default();
    theme.heat_low = Color::Indexed(7);
    assert_eq!(
        build_heat_gradient(&theme),
        [Color::Indexed(7); HEAT_LEVELS]
    );

    theme.heat_low = Color::Rgb(1, 2, 3);
    theme.heat_high = Color::Reset;
    assert_eq!(build_heat_gradient(&theme), [Color::Reset; HEAT_LEVELS]);
}

#[test]
fn heat_style_uses_low_subtle_and_high_colors() {
    let mut theme = Theme::default();
    theme.palette.subtle = Color::Rgb(13, 14, 15);
    theme.heat_low = Color::Rgb(7, 8, 9);
    theme.heat_high = Color::Rgb(10, 11, 12);
    let heat_scale = HeatScale::new(&theme);

    assert_eq!(heat_scale.style(0, 100).fg, Some(theme.palette.subtle));
    assert_eq!(heat_scale.style(1, 100).fg, Some(theme.heat_low));
    assert_eq!(heat_scale.style(100, 100).fg, Some(theme.heat_high));
}

fn bucket(label: &str) -> bone_protocol::UsageBucket {
    bone_protocol::UsageBucket {
        label: label.into(),
        prompt_tokens: 1,
        completion_tokens: 2,
        cached_tokens: 0,
        cost: 0.0,
        request_count: 1,
    }
}

fn snapshot() -> UsageStatsSnapshot {
    UsageStatsSnapshot {
        started_at: None,
        ended_at: None,
        total: bone_protocol::UsageSummary::default(),
        by_model_today: Vec::new(),
        by_model_7d: Vec::new(),
        by_model_4w: Vec::new(),
        by_model_all: Vec::new(),
        daily: vec![bucket("today")],
        weekly: vec![bucket("week")],
        monthly: vec![bucket("month")],
        all_time: vec![bucket("all")],
        yearly: vec![bucket("year")],
        hourly_today: Vec::new(),
        hourly_7d: Vec::new(),
        hourly_4w: Vec::new(),
        hourly_all: Vec::new(),
        daily_activity: vec![bucket("2026-01-01"), bucket("2026-01-02")],
    }
}

fn ready_screen() -> StatsScreen {
    let (mut screen, _) = StatsScreen::new(&Theme::default());
    screen.loaded(Ok(snapshot()));
    screen
}

#[test]
fn touch_stats_tabs_and_footer_use_existing_keys() {
    let screen = ready_screen();
    assert_eq!(
        screen.touch_key(2, 2, 80, 20),
        Some(TouchAction::Keys(vec![Key::plain(KeyCode::Char('d'))]))
    );
    // The panel title row (row 1) is not a tab row.
    assert_eq!(screen.touch_key(1, 2, 80, 20), None);
    assert_eq!(
        screen.touch_key(19, 0, 80, 20),
        Some(TouchAction::Keys(vec![Key::plain(KeyCode::Esc)]))
    );

    let close = " q/Esc quit  ".chars().count() as u16;
    assert_eq!(
        screen.touch_key(19, close + 2, 80, 20),
        Some(TouchAction::Keys(vec![Key::plain(KeyCode::Right)]))
    );
    // Swipe scrolls, so the scroll hint is not a tap target.
    assert_eq!(screen.touch_key(19, 65, 80, 20), None);
}

#[test]
fn touch_stats_date_picker_selects_fields_and_actions() {
    let mut screen = ready_screen();
    assert!(matches!(
        screen.handle_key(Key::plain(KeyCode::Char('t'))),
        StatsAction::None
    ));
    let popup = popup_rect(80, 20);
    assert_eq!(popup.y, 5);

    assert_eq!(
        screen.touch_key(popup.y + 5, popup.x + 4, 80, 20),
        Some(TouchAction::Keys(vec![Key::plain(KeyCode::Tab)]))
    );
    assert_eq!(
        screen.touch_key(popup.y + 7, popup.x + 2 + 15, 80, 20),
        Some(TouchAction::Keys(vec![Key::plain(KeyCode::Enter)]))
    );
    assert_eq!(
        screen.touch_key(popup.y + 7, popup.x + 2 + 29, 80, 20),
        Some(TouchAction::Keys(vec![Key::plain(KeyCode::Esc)]))
    );
}
