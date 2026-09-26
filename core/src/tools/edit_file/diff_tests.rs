use super::*;

fn numbered(count: usize) -> String {
    (1..=count).map(|n| format!("line {n}\n")).collect()
}

#[test]
fn separates_hunks_with_a_marker_row() {
    let old = numbered(40);
    let new = old
        .replace("line 5\n", "changed 5\n")
        .replace("line 30\n", "changed 30\n");

    let (lines, insertions, deletions) = build_numbered_diff_lines(&old, &new, 3);

    assert_eq!((insertions, deletions), (2, 2));
    let separators: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.as_str() == HUNK_SEPARATOR)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(separators.len(), 1, "one separator between two hunks");
    let at = separators[0];
    assert_eq!(lines[at - 1], "    8   line 8", "first hunk ends before it");
    assert_eq!(
        lines[at + 1],
        "   27   line 27",
        "second hunk starts after it"
    );
}

#[test]
fn single_hunk_has_no_separator() {
    let old = numbered(10);
    let new = old.replace("line 5\n", "changed 5\n");

    let (lines, _, _) = build_numbered_diff_lines(&old, &new, 3);

    assert!(!lines.iter().any(|line| line == HUNK_SEPARATOR));
    assert_eq!(lines.first().map(String::as_str), Some("    2   line 2"));
}
