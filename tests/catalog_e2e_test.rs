//! End-to-end catalogue flow against a local filesystem fixture: fetch the
//! index, install an item, refresh it after a version bump, fall back to the
//! cache when offline, reject a bad checksum, and remove the item.
//!
//! Drives `BONE_CATALOG_URL` (the fixture) and `XDG_CONFIG_HOME` (so the client
//! writes into a temp `bone-rust` dir). Kept to a single test to avoid env-var
//! races across threads.

use std::fs;
use std::path::Path;

use bone::ext::catalog::{self, CatalogEntry};

mod common;

fn write_index(fixture: &Path, version: u32, sha256: &str) {
    let json = format!(
        r#"[{{ "name": "demo.lua", "kind": "tool", "description": "demo",
              "version": {version}, "sha256": "{sha256}" }}]"#
    );
    fs::write(fixture.join("catalog.json"), json).unwrap();
}

#[test]
fn catalog_fetch_install_refresh_remove() {
    let fixture = common::temp_dir("catalog-fixture");
    let cfg = common::temp_dir("catalog-cfg");
    fs::create_dir_all(fixture.join("tools")).unwrap();
    fs::write(fixture.join("tools").join("demo.lua"), "-- demo v1\n").unwrap();
    write_index(&fixture, 1, "");

    // SAFETY: single-test file; no other threads read these vars concurrently.
    unsafe {
        std::env::set_var("BONE_CATALOG_URL", &fixture);
        std::env::set_var("XDG_CONFIG_HOME", &cfg);
    }
    let installed_path = cfg.join("bone-rust").join("lua/tools/demo.lua");

    // Fetch the index.
    let entries = catalog::fetch_index();
    assert_eq!(entries.len(), 1, "fixture index has one entry");
    let entry = entries[0].clone();
    assert!(!catalog::is_installed(&entry));

    // Install it.
    catalog::install(&entry).unwrap();
    assert_eq!(fs::read_to_string(&installed_path).unwrap(), "-- demo v1\n");
    assert!(catalog::is_installed(&entry));
    assert_eq!(catalog::installed_version("demo.lua"), Some(1));

    // Bump the version and refresh: the installed copy is updated.
    fs::write(fixture.join("tools").join("demo.lua"), "-- demo v2\n").unwrap();
    write_index(&fixture, 2, "");
    catalog::refresh_now();
    assert_eq!(fs::read_to_string(&installed_path).unwrap(), "-- demo v2\n");
    assert_eq!(catalog::installed_version("demo.lua"), Some(2));

    // Offline: an unreachable base falls back to the cached index.
    unsafe {
        std::env::set_var("BONE_CATALOG_URL", fixture.join("does-not-exist"));
    }
    assert_eq!(catalog::fetch_index().len(), 1, "offline uses cached index");

    // A checksum mismatch aborts the install.
    unsafe {
        std::env::set_var("BONE_CATALOG_URL", &fixture);
    }
    let bad = CatalogEntry {
        name: "demo.lua".to_string(),
        kind: "tool".to_string(),
        description: String::new(),
        version: 9,
        sha256: "deadbeef".to_string(),
    };
    assert!(catalog::install(&bad).is_err(), "bad sha256 should fail");

    // Remove it.
    catalog::remove(&entry).unwrap();
    assert!(!installed_path.exists());
    assert_eq!(catalog::installed_version("demo.lua"), None);

    fs::remove_dir_all(&fixture).ok();
    fs::remove_dir_all(&cfg).ok();
}
