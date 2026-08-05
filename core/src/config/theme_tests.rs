use super::*;

#[test]
fn parser_accepts_named_and_rgb_forms_only() {
    assert_eq!(
        parse_color(" dark_gray "),
        Ok(ColorValue::Named(NamedColor::DarkGray))
    );
    assert_eq!(
        parse_color("#12aBc3"),
        Ok(ColorValue::Rgb(0x12, 0xab, 0xc3))
    );
    assert_eq!(parse_color("12aBc3"), Ok(ColorValue::Rgb(0x12, 0xab, 0xc3)));
    assert!(parse_color("#12345").is_err());
    assert!(parse_color("indexed(1)").is_err());
}

#[test]
fn registry_is_complete_unique_and_excludes_removed_role() {
    let names = role_names().collect::<Vec<_>>();
    let unique = names
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names.len(), 10 + 15 + 8 + 18 + 7 + 4);
    assert_eq!(names.len(), unique.len());
    assert!(role("markdown_heading").is_some());
    assert!(role("tab_active").is_none());
    assert!(!role("fg").unwrap().runtime);
    assert!(role("bg").unwrap().runtime);
}
