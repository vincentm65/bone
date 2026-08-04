use super::*;

#[test]
fn verbose_identity_contains_every_build_dimension() {
    let value = verbose();
    assert!(value.starts_with(&format!("bone {VERSION}\n")));
    for label in ["commit:", "target:", "profile:", "channel:"] {
        assert!(value.lines().any(|line| line.starts_with(label)), "{value}");
    }
    assert!(!GIT_SHA.is_empty());
    assert!(!TARGET.is_empty());
}
