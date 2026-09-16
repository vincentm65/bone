//! Unit tests for catalog helpers that don't touch the real config dir.
//! Filesystem install/remove flow is covered by `tests/catalog_e2e_test.rs`.

use super::*;

#[test]
fn parses_index_with_defaults_and_metadata() {
    let json = br#"[
        { "name": "weather.lua", "kind": "tool", "description": "show the weather",
          "version": 3, "updated_date": "2026-03-10", "author": "Bone Team",
          "repo_url": "https://example.com/repo", "docs_url": "https://example.com/docs",
          "min_bone_version": ">=2.4", "dependencies": ["helper.lua"],
          "permissions": ["network"], "long_description": "More detail.", "sha256": "abc" },
        { "name": "goal.lua", "kind": "command", "files": [
          { "path": "themes/nord.lua", "sha256": "def" }
        ] }
    ]"#;
    let entries = parse_index(json).expect("valid index");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "weather.lua");
    assert_eq!(entries[0].sha256, "abc");
    assert_eq!(entries[0].version.as_deref(), Some("3"));
    assert_eq!(entries[0].updated_at.as_deref(), Some("2026-03-10"));
    assert_eq!(entries[0].author.as_deref(), Some("Bone Team"));
    assert_eq!(
        entries[0].repository.as_deref(),
        Some("https://example.com/repo")
    );
    assert_eq!(
        entries[0].documentation.as_deref(),
        Some("https://example.com/docs")
    );
    assert_eq!(entries[0].min_bone_version.as_deref(), Some(">=2.4"));
    assert_eq!(entries[0].dependencies, ["helper.lua"]);
    assert_eq!(entries[0].permissions, ["network"]);
    assert_eq!(entries[0].long_description.as_deref(), Some("More detail."));
    assert!(entries[0].files.is_empty());
    assert_eq!(entries[0].dir_segment(), "tools");
    // Missing optional metadata and sha256 use empty defaults.
    assert!(entries[1].sha256.is_empty());
    assert!(entries[1].version.is_none());
    assert!(entries[1].dependencies.is_empty());
    assert_eq!(entries[1].dir_segment(), "commands");
    assert!(entries[1].is_command());
    assert_eq!(entries[1].files.len(), 1);
    assert_eq!(entries[1].files[0].path, "themes/nord.lua");
    assert_eq!(entries[1].files[0].sha256, "def");
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
fn bundled_paths_are_validated_and_unique() {
    let valid = CatalogEntry {
        name: "themes.lua".into(),
        kind: "command".into(),
        files: vec![
            CatalogFile {
                path: "themes/nord.lua".into(),
                sha256: "abc".into(),
            },
            CatalogFile {
                path: "tools/helper.lua".into(),
                sha256: String::new(),
            },
            CatalogFile {
                path: "packages/helper/src/main.rs".into(),
                sha256: String::new(),
            },
        ],
        ..CatalogEntry::default()
    };
    assert!(valid.validate().is_ok());

    for paths in [
        vec!["nord.lua"],
        vec!["/themes/nord.lua"],
        vec!["themes/../nord.lua"],
        vec!["themes/./nord.lua"],
        vec!["themes\\nord.lua"],
        vec!["commands/themes.lua"],
        vec!["themes/nord.lua", "themes/nord.lua"],
    ] {
        let mut entry = valid.clone();
        entry.files = paths
            .into_iter()
            .map(|path| CatalogFile {
                path: path.into(),
                sha256: String::new(),
            })
            .collect();
        assert!(entry.validate().is_err(), "accepted {:?}", entry.files);
    }
}

#[test]
fn catalog_operations_reject_malformed_entries() {
    let invalid = CatalogEntry {
        name: "../escape.lua".into(),
        kind: "tool".into(),
        ..CatalogEntry::default()
    };

    assert!(!is_installed(&invalid));
    assert!(!needs_update(&invalid));
    assert!(install(&invalid).is_err());
    assert!(remove(&invalid).is_err());
}

fn plugin_entry(name: &str) -> CatalogEntry {
    CatalogEntry {
        name: name.into(),
        kind: "plugin".into(),
        ..CatalogEntry::default()
    }
}

#[test]
fn plugin_entries_validate_and_resolve_package_paths() {
    let entry = plugin_entry("myplugin");
    assert!(entry.validate().is_ok());
    assert!(entry.is_plugin());
    assert_eq!(entry.dir_segment(), "plugins");
    assert_eq!(entry.primary_rel(), "plugins/myplugin/init.lua");
    assert_eq!(
        entry.plugin_dir(),
        Some(crate::config::bone_dir().join("lua/plugins/myplugin"))
    );
}

#[test]
fn plugin_bundled_files_must_stay_inside_its_package() {
    let mut entry = plugin_entry("myplugin");
    entry.files = vec![CatalogFile {
        path: "plugins/myplugin/lib/util.lua".into(),
        sha256: String::new(),
    }];
    assert!(entry.validate().is_ok());

    // A file outside the package directory is rejected.
    entry.files = vec![CatalogFile {
        path: "plugins/other/init.lua".into(),
        sha256: String::new(),
    }];
    assert!(entry.validate().is_err());

    // A file elsewhere under `lua/` is also rejected for plugins.
    entry.files = vec![CatalogFile {
        path: "themes/nord.lua".into(),
        sha256: String::new(),
    }];
    assert!(entry.validate().is_err());
}

#[test]
fn plugin_names_reject_lua_suffix_and_unsafe_segments() {
    for name in ["evil.lua", "../escape", "nested/escape", "", "back\\slash"] {
        assert!(
            plugin_entry(name).validate().is_err(),
            "accepted plugin name {name:?}"
        );
    }
}

#[test]
fn plugin_entries_survive_index_round_trip() {
    let json = br#"[
        { "name": "myplugin", "kind": "plugin", "description": "d",
          "files": [ { "path": "plugins/myplugin/lib/util.lua", "sha256": "x" } ] },
        { "name": "other.lua", "kind": "tool" }
    ]"#;
    let entries = parse_index(json).expect("valid index");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].kind, "plugin");
    assert_eq!(entries[0].dir_segment(), "plugins");
    assert_eq!(entries[0].files.len(), 1);
}

#[test]
fn min_bone_version_gate_accepts_and_rejects() {
    let make = |min: Option<&str>| {
        let mut entry = plugin_entry("gated");
        entry.min_bone_version = min.map(str::to_string);
        entry
    };

    assert!(make(None).bone_version_ok().is_ok(), "no requirement passes");
    assert!(
        make(Some("0.1.0")).bone_version_ok().is_ok(),
        "a satisfied bare version passes"
    );
    assert!(
        make(Some(">=2.4")).bone_version_ok().is_ok(),
        "a satisfied range passes"
    );
    assert!(
        make(Some("not-a-version")).bone_version_ok().is_ok(),
        "an unparseable requirement must never block an install"
    );

    let error = make(Some("999.0.0")).bone_version_ok().unwrap_err();
    assert!(
        error.starts_with("requires Bone 999.0.0 (current "),
        "unexpected error: {error}"
    );
    assert!(make(Some(">=3.0")).bone_version_ok().is_err());
}

#[test]
fn plugin_legacy_flat_paths_derive_from_the_scoped_layout() {
    let mut entry = plugin_entry("skill");
    entry.files = vec![
        CatalogFile {
            path: "plugins/skill/lib/skill.lua".into(),
            sha256: String::new(),
        },
        CatalogFile {
            path: "plugins/skill/commands/skill.lua".into(),
            sha256: String::new(),
        },
    ];
    let lua = crate::config::bone_dir().join("lua");
    assert_eq!(
        entry.legacy_primary_paths(),
        vec![lua.join("tools/skill.lua"), lua.join("commands/skill.lua")]
    );
    assert_eq!(
        entry
            .files
            .iter()
            .map(|file| entry.legacy_bundled_path(file).unwrap())
            .collect::<Vec<_>>(),
        vec![lua.join("lib/skill.lua"), lua.join("commands/skill.lua")],
        "the scoped path minus its package prefix; a bundled command may
        coincide with a legacy primary candidate"
    );

    // Non-plugins were never installed flat, so they have no legacy layout.
    let tool = CatalogEntry {
        name: "weather.lua".into(),
        kind: "tool".into(),
        files: vec![CatalogFile {
            path: "themes/nord.lua".into(),
            sha256: String::new(),
        }],
        ..CatalogEntry::default()
    };
    assert!(tool.legacy_primary_paths().is_empty());
    assert_eq!(tool.legacy_bundled_path(&tool.files[0]), None);
}
