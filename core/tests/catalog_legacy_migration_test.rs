//! Legacy flat-layout → plugin package migration, end to end against a local
//! filesystem fixture: a fresh install lands only in `lua/plugins/<name>/`;
//! when legacy flat files coexist, `install()` migrates missing package files
//! from them byte-for-byte (preserving user edits), updates the rest from the
//! catalog, and sweeps every legacy path; and `remove()` cleans both layouts
//! and prunes the package directory.
//!
//! Drives `BONE_CATALOG_URL` (the fixture) and both config-root overrides
//! (`BONE_DIR`, `XDG_CONFIG_HOME`) so the client writes into a temp `bone-rust`
//! dir whatever the caller's environment sets. Kept to a single test to avoid
//! env-var races across threads.

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

/// Publish the plugin package (primary `plugins/demo/init.lua` plus the
/// bundled `plugins/demo/lib/helper.lua`) and a matching index.
fn publish(fixture: &Path, primary: &str, helper: &str) -> CatalogEntry {
    fs::create_dir_all(fixture.join("plugins/demo/lib")).unwrap();
    fs::write(fixture.join("plugins/demo/init.lua"), primary).unwrap();
    fs::write(fixture.join("plugins/demo/lib/helper.lua"), helper).unwrap();
    let primary_sha = sha256_hex(primary.as_bytes());
    let helper_sha = sha256_hex(helper.as_bytes());
    let json = format!(
        r#"[{{ "name": "demo", "kind": "plugin", "description": "demo plugin",
              "sha256": "{primary_sha}", "files": [
                {{ "path": "plugins/demo/lib/helper.lua", "sha256": "{helper_sha}" }}
              ] }}]"#
    );
    fs::write(fixture.join("catalog.json"), json).unwrap();
    catalog::fetch_index().into_iter().next().unwrap()
}

#[test]
fn legacy_flat_install_migrates_to_the_plugin_package() {
    let fixture = common::temp_dir("catalog-legacy-fixture");
    let cfg = common::temp_dir("catalog-legacy-cfg");

    // SAFETY: single-test file; no other threads read these vars concurrently.
    // `BONE_DIR` outranks `XDG_CONFIG_HOME` in `config::try_bone_dir`, so set it
    // too: an inherited `BONE_DIR` would otherwise leave this test writing into
    // — and `remove()`-ing from — the real config dir.
    unsafe {
        std::env::set_var("BONE_CATALOG_URL", &fixture);
        std::env::set_var("BONE_DIR", cfg.join("bone-rust"));
        std::env::set_var("XDG_CONFIG_HOME", &cfg);
    }
    let lua = cfg.join("bone-rust").join("lua");
    let package_dir = lua.join("plugins/demo");
    let package_init = package_dir.join("init.lua");
    let package_helper = package_dir.join("lib/helper.lua");
    let legacy_tool = lua.join("tools/demo.lua");
    let legacy_command = lua.join("commands/demo.lua");
    let legacy_helper = lua.join("lib/helper.lua");

    // (A) A fresh install lands only in the package layout.
    let entry = publish(&fixture, "-- demo v1\n", "-- helper v1\n");
    assert!(!catalog::is_installed(&entry));
    catalog::install(&entry).unwrap();
    assert_eq!(fs::read_to_string(&package_init).unwrap(), "-- demo v1\n");
    assert_eq!(
        fs::read_to_string(&package_helper).unwrap(),
        "-- helper v1\n"
    );
    for legacy in [&legacy_tool, &legacy_command, &legacy_helper] {
        assert!(
            !legacy.exists(),
            "fresh install must not create {}",
            legacy.display()
        );
    }
    assert!(catalog::is_installed(&entry));
    assert!(!catalog::needs_update(&entry));
    assert_eq!(catalog::updates_available(), 0);

    // (B) Mixed layout: package files present AND a legacy flat file left over.
    // The package files are updated from the catalog (sha-verified); the
    // legacy file is swept even though its bytes were superseded.
    let entry = publish(&fixture, "-- demo v2\n", "-- helper v2\n");
    fs::create_dir_all(legacy_tool.parent().unwrap()).unwrap();
    fs::write(&legacy_tool, "-- demo v1, edited locally\n").unwrap();
    assert!(
        catalog::is_installed(&entry),
        "a legacy primary still counts as installed"
    );
    assert!(
        catalog::needs_update(&entry),
        "any leftover legacy path flags the item for migration"
    );
    catalog::install(&entry).unwrap();
    assert_eq!(
        fs::read_to_string(&package_init).unwrap(),
        "-- demo v2\n",
        "existing package file is updated from the catalog"
    );
    assert_eq!(
        fs::read_to_string(&package_helper).unwrap(),
        "-- helper v2\n"
    );
    assert!(!legacy_tool.exists(), "legacy file swept after migration");
    assert!(!catalog::needs_update(&entry));

    // (C) Legacy-only layout: the package files were lost (e.g. an old Bone
    // install), the flat files remain. Their bytes are moved verbatim — a
    // user-edited primary is preserved, not re-downloaded.
    fs::remove_dir_all(&package_dir).unwrap();
    fs::create_dir_all(legacy_helper.parent().unwrap()).unwrap();
    fs::write(&legacy_tool, "-- demo v1, edited locally\n").unwrap();
    fs::write(&legacy_helper, "-- helper v2\n").unwrap();
    assert!(
        catalog::is_installed(&entry),
        "legacy-only still counts as installed"
    );
    assert!(catalog::needs_update(&entry));
    catalog::install(&entry).unwrap();
    assert_eq!(
        fs::read_to_string(&package_init).unwrap(),
        "-- demo v1, edited locally\n",
        "a user-edited legacy primary migrates byte-for-byte without a download"
    );
    assert_eq!(
        fs::read_to_string(&package_helper).unwrap(),
        "-- helper v2\n",
        "a legacy bundled file migrates byte-for-byte without a download"
    );
    assert!(!legacy_tool.exists());
    assert!(!legacy_helper.exists());
    assert!(
        catalog::needs_update(&entry),
        "a migrated-but-edited primary still differs from the catalog"
    );

    // Applying the update again adopts the upstream content and clears the flag.
    catalog::install(&entry).unwrap();
    assert_eq!(fs::read_to_string(&package_init).unwrap(), "-- demo v2\n");
    assert!(!catalog::needs_update(&entry));

    // (D) remove() cleans the package layout, any leftover legacy files, and
    // prunes the now-empty package directory.
    fs::create_dir_all(legacy_tool.parent().unwrap()).unwrap();
    fs::create_dir_all(legacy_command.parent().unwrap()).unwrap();
    fs::write(&legacy_tool, "-- leftover\n").unwrap();
    fs::write(&legacy_command, "-- leftover command\n").unwrap();
    catalog::remove(&entry).unwrap();
    assert!(!package_init.exists());
    assert!(!package_helper.exists());
    assert!(
        !package_dir.exists(),
        "empty package directory must be pruned"
    );
    for legacy in [&legacy_tool, &legacy_command, &legacy_helper] {
        assert!(!legacy.exists(), "{} must be removed", legacy.display());
    }
    assert!(!catalog::is_installed(&entry));

    fs::remove_dir_all(&fixture).ok();
    fs::remove_dir_all(&cfg).ok();
}
