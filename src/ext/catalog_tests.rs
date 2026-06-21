//! Unit tests for catalog helpers that don't touch the real config dir.
//! Filesystem install/remove flow is covered by `tests/catalog_e2e_test.rs`.

use super::*;

#[test]
fn parses_index_with_defaults() {
    let json = br#"[
        { "name": "browser.lua", "kind": "tool", "description": "drive a browser",
          "version": 3, "sha256": "abc" },
        { "name": "goal.lua", "kind": "command" }
    ]"#;
    let entries = parse_index(json).expect("valid index");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "browser.lua");
    assert_eq!(entries[0].version, 3);
    assert_eq!(entries[0].dir_segment(), "tools");
    // Missing version/sha256 fall back to defaults.
    assert_eq!(entries[1].version, 1);
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
