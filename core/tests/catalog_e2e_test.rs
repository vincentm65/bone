//! End-to-end catalog flow against a local filesystem fixture: fetch the
//! index, install an item, detect an update when its content changes, refresh
//! the cached index without auto-installing, fall back to the cache when
//! offline, reject a bad checksum, and remove the item.
//!
//! Drives `BONE_CATALOG_URL` (the fixture) and `XDG_CONFIG_HOME` (so the client
//! writes into a temp `bone-rust` dir). Kept to a single test to avoid env-var
//! races across threads.

use std::fs;
use std::path::Path;

use bone_core::ext::catalog::{self, CatalogEntry};
use sha2::{Digest, Sha256};

mod common;

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Write the fixture's `demo.lua` plus an index whose sha256 matches it, and
/// return the matching `CatalogEntry`.
fn publish(fixture: &Path, body: &str) -> CatalogEntry {
    fs::write(fixture.join("tools").join("demo.lua"), body).unwrap();
    let sha = sha256_hex(body.as_bytes());
    let json = format!(
        r#"[{{ "name": "demo.lua", "kind": "tool", "description": "demo",
              "sha256": "{sha}" }}]"#
    );
    fs::write(fixture.join("catalog.json"), json).unwrap();
    catalog::fetch_index().into_iter().next().unwrap()
}

#[test]
fn catalog_fetch_install_update_remove() {
    let fixture = common::temp_dir("catalog-fixture");
    let cfg = common::temp_dir("catalog-cfg");
    fs::create_dir_all(fixture.join("tools")).unwrap();

    // SAFETY: single-test file; no other threads read these vars concurrently.
    unsafe {
        std::env::set_var("BONE_CATALOG_URL", &fixture);
        std::env::set_var("XDG_CONFIG_HOME", &cfg);
    }
    let installed_path = cfg.join("bone-rust").join("lua/tools/demo.lua");

    // Publish v1 and install it.
    let entry = publish(&fixture, "-- demo v1\n");
    assert!(!catalog::is_installed(&entry));
    catalog::install(&entry).unwrap();
    assert_eq!(fs::read_to_string(&installed_path).unwrap(), "-- demo v1\n");
    assert!(catalog::is_installed(&entry));
    // Fresh install matches the published content: no update pending.
    assert!(!catalog::needs_update(&entry));
    assert_eq!(catalog::updates_available(), 0);

    // Publish new content (a "git" change). The on-disk copy now differs.
    let entry = publish(&fixture, "-- demo v2\n");
    assert!(
        catalog::needs_update(&entry),
        "changed content flags update"
    );
    assert_eq!(catalog::updates_available(), 1);

    // refresh_now only refreshes the cached index — it must NOT auto-install.
    catalog::refresh_now();
    assert_eq!(
        fs::read_to_string(&installed_path).unwrap(),
        "-- demo v1\n",
        "refresh must not pull updates without user action"
    );
    assert_eq!(catalog::updates_available(), 1);

    // The user applies the update via install(); the flag clears.
    catalog::install(&entry).unwrap();
    assert_eq!(fs::read_to_string(&installed_path).unwrap(), "-- demo v2\n");
    assert!(!catalog::needs_update(&entry));
    assert_eq!(catalog::updates_available(), 0);

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
        sha256: "deadbeef".to_string(),
        ..CatalogEntry::default()
    };
    assert!(catalog::install(&bad).is_err(), "bad sha256 should fail");

    // Remove it.
    catalog::remove(&entry).unwrap();
    assert!(!installed_path.exists());

    fs::remove_dir_all(&fixture).ok();
    fs::remove_dir_all(&cfg).ok();
}
