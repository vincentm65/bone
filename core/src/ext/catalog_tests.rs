//! Unit tests for catalog helpers that don't touch the real config dir.
//! Filesystem install/remove flow is covered by `tests/catalog_e2e_test.rs`.

use super::*;

#[test]
fn parses_index_with_defaults() {
    // An unknown `version` field is tolerated (serde ignores it).
    let json = br#"[
        { "name": "browser.lua", "kind": "tool", "description": "drive a browser",
          "version": 3, "sha256": "abc" },
        { "name": "goal.lua", "kind": "command" }
    ]"#;
    let entries = parse_index(json).expect("valid index");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "browser.lua");
    assert_eq!(entries[0].sha256, "abc");
    assert_eq!(entries[0].dir_segment(), "tools");
    // Missing sha256 falls back to empty (detection disabled).
    assert!(entries[1].sha256.is_empty());
    assert_eq!(entries[1].dir_segment(), "commands");
    assert!(entries[1].is_command());
}

#[test]
fn remote_detection() {
    assert!(is_remote("https://example.com/catalog"));
    assert!(is_remote("http://example.com"));
    assert!(!is_remote("/tmp/catalog"));
    assert!(!is_remote("./catalog"));
}

#[test]
fn sha256_matches_known_vector() {
    // sha256("abc")
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn malformed_entries_are_filtered_from_the_index() {
    let json = br#"[
        { "name": "good.lua", "kind": "tool" },
        { "name": "../escape.lua", "kind": "tool" },
        { "name": "nested/escape.lua", "kind": "command" },
        { "name": "backslash\\escape.lua", "kind": "tool" },
        { "name": "nul\u0000escape.lua", "kind": "tool" },
        { "name": "", "kind": "tool" },
        { "name": "not-lua.txt", "kind": "tool" },
        { "name": "other.lua", "kind": "unknown" }
    ]"#;

    let entries = parse_index(json).expect("valid JSON index");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "good.lua");
}

#[test]
fn catalog_operations_reject_malformed_entries() {
    let invalid = CatalogEntry {
        name: "../escape.lua".into(),
        kind: "tool".into(),
        description: String::new(),
        sha256: String::new(),
    };

    assert!(!is_installed(&invalid));
    assert!(!needs_update(&invalid));
    assert!(install(&invalid).is_err());
    assert!(remove(&invalid).is_err());
}
