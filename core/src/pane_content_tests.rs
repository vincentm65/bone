use super::*;
use serde_json::json;

/// A number element in `lines` is SKIPPED; the two string siblings parse.
#[test]
fn skip_numeric_line() {
    let val = json!({
        "source": "s",
        "title": "t",
        "lines": ["ok", 42, "also-ok"]
    });
    let pc = PaneContent::from_json(&val).unwrap();
    assert_eq!(pc.lines.len(), 2);
    assert!(matches!(&pc.lines[0], PaneLineSpec::Plain(s) if s == "ok"));
    assert!(matches!(&pc.lines[1], PaneLineSpec::Plain(s) if s == "also-ok"));
}

/// `{"spans":"not-an-array"}` as a line element is skipped without failing.
#[test]
fn skip_bad_spans_type() {
    let val = json!({
        "source": "s",
        "title": "t",
        "lines": [
            "first",
            {"spans": "not-an-array"},
            "last"
        ]
    });
    let pc = PaneContent::from_json(&val).unwrap();
    assert_eq!(pc.lines.len(), 2);
    assert!(matches!(&pc.lines[0], PaneLineSpec::Plain(s) if s == "first"));
    assert!(matches!(&pc.lines[1], PaneLineSpec::Plain(s) if s == "last"));
}

/// Happy path: mix of plain-string lines and `{"spans":[...]}` lines.
#[test]
fn happy_path_mixed() {
    let val = json!({
        "source": "s",
        "title": "t",
        "lines": [
            "plain",
            {"spans": [
                {"text": "bold", "modifiers": ["bold"]},
                {"text": "plain"}
            ]},
            "another plain"
        ]
    });
    let pc = PaneContent::from_json(&val).unwrap();
    assert_eq!(pc.lines.len(), 3);
    assert!(matches!(&pc.lines[0], PaneLineSpec::Plain(s) if s == "plain"));
    assert!(matches!(&pc.lines[1], PaneLineSpec::Spans { .. }));
    assert!(matches!(&pc.lines[2], PaneLineSpec::Plain(s) if s == "another plain"));
}

/// `"lines": {}` still parses to 0 lines (empty-map case).
#[test]
fn empty_object_yields_zero_lines() {
    let val = json!({
        "source": "s",
        "title": "t",
        "lines": {}
    });
    let pc = PaneContent::from_json(&val).unwrap();
    assert!(pc.lines.is_empty());
}

/// `"lines": null` parses to 0 lines.
#[test]
fn null_yields_zero_lines() {
    let val = json!({
        "source": "s",
        "title": "t",
        "lines": null
    });
    let pc = PaneContent::from_json(&val).unwrap();
    assert!(pc.lines.is_empty());
}
