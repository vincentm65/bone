use super::*;

fn page(source: &str) -> PanePage {
    PanePage {
        source: source.to_string(),
        title: source.to_string(),
        content: Vec::new(),
        visible_rows: 1,
        scroll: 0,
    }
}

fn pages(sources: &[&str]) -> Vec<PanePage> {
    sources.iter().map(|s| page(s)).collect()
}

#[test]
fn remove_clamps_active_when_active_page_was_last() {
    // Active page points at the page being removed (the last one); after
    // removal active_page must fall back to the new last index, not dangle
    // past the end (the out-of-bounds panic this guards against).
    let mut p = pages(&["interact", "subagent"]);
    let active = PanePage::remove(&mut p, "subagent", 1);
    assert_eq!(p.len(), 1);
    assert_eq!(active, 0);
}

#[test]
fn remove_shifts_active_when_lower_page_removed() {
    // Removing a page below the active one shifts active_page down by one
    // so it keeps pointing at the same logical page.
    let mut p = pages(&["subagent", "interact"]);
    let active = PanePage::remove(&mut p, "subagent", 1);
    assert_eq!(p.len(), 1);
    assert_eq!(active, 0);
}

#[test]
fn remove_leaves_active_when_higher_page_removed() {
    let mut p = pages(&["interact", "subagent"]);
    let active = PanePage::remove(&mut p, "subagent", 0);
    assert_eq!(active, 0);
}

#[test]
fn remove_resets_active_when_emptied() {
    let mut p = pages(&["interact"]);
    let active = PanePage::remove(&mut p, "interact", 0);
    assert!(p.is_empty());
    assert_eq!(active, 0);
}

#[test]
fn remove_missing_source_is_noop() {
    let mut p = pages(&["interact", "subagent"]);
    let active = PanePage::remove(&mut p, "nope", 1);
    assert_eq!(p.len(), 2);
    assert_eq!(active, 1);
}
