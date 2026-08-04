use super::*;
use crate::ui::theme::Theme;

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
