use super::*;

fn make_config_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (rel, content) in files {
        let full = dir.path().join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, *content).unwrap();
    }
    dir
}

#[test]
fn init_lua_changes_affect_hash() {
    let dir = make_config_dir(&[("init.lua", "-- v1")]);
    let h1 = stamp(dir.path()).unwrap();

    // Change content of init.lua.
    let dir2 = make_config_dir(&[("init.lua", "-- v2")]);
    let h2 = stamp(dir2.path()).unwrap();
    assert_ne!(h1, h2);
}

#[test]
fn adding_lua_file_changes_hash() {
    let dir = make_config_dir(&[("init.lua", "hello")]);
    let h1 = stamp(dir.path()).unwrap();

    let dir2 = make_config_dir(&[("init.lua", "hello"), ("lua/mod.lua", "return 1")]);
    let h2 = stamp(dir2.path()).unwrap();
    assert_ne!(h1, h2);
}

#[test]
fn deleting_lua_file_changes_hash() {
    let dir = make_config_dir(&[("init.lua", "hello"), ("lua/mod.lua", "return 1")]);
    let h1 = stamp(dir.path()).unwrap();

    let dir2 = make_config_dir(&[("init.lua", "hello")]);
    let h2 = stamp(dir2.path()).unwrap();
    assert_ne!(h1, h2);
}

#[test]
fn renaming_lua_file_changes_hash() {
    let dir = make_config_dir(&[("lua/a.lua", "content_a")]);
    let h1 = stamp(dir.path()).unwrap();

    let dir2 = make_config_dir(&[("lua/b.lua", "content_a")]);
    let h2 = stamp(dir2.path()).unwrap();
    assert_ne!(h1, h2);
}

#[test]
fn non_lua_files_ignored() {
    let dir = make_config_dir(&[("init.lua", "hello"), ("lua/readme.txt", "ignore me")]);
    let h1 = stamp(dir.path()).unwrap();

    let dir2 = make_config_dir(&[("init.lua", "hello"), ("lua/readme.txt", "changed")]);
    let h2 = stamp(dir2.path()).unwrap();
    assert_eq!(h1, h2);
}

#[test]
fn recursive_lua_files_included() {
    let dir = make_config_dir(&[
        ("init.lua", "root"),
        ("lua/a.lua", "a"),
        ("lua/sub/b.lua", "b"),
    ]);
    let h = stamp(dir.path()).unwrap();

    // Changing a nested file should change the hash.
    let dir2 = make_config_dir(&[
        ("init.lua", "root"),
        ("lua/a.lua", "a"),
        ("lua/sub/b.lua", "changed"),
    ]);
    let h2 = stamp(dir2.path()).unwrap();
    assert_ne!(h, h2);
}

#[test]
fn hash_is_deterministic_across_calls() {
    let dir = make_config_dir(&[
        ("init.lua", "init content"),
        ("lua/lib/utils.lua", "return {}"),
        ("lua/plugins/theme.lua", "return { dark = true }"),
    ]);
    let h1 = stamp(dir.path()).unwrap();
    let h2 = stamp(dir.path()).unwrap();
    let h3 = stamp(dir.path()).unwrap();
    assert_eq!(h1, h2);
    assert_eq!(h2, h3);
}

#[test]
fn display_produces_64_hex_chars() {
    let dir = make_config_dir(&[("init.lua", "test")]);
    let h = stamp(dir.path()).unwrap();
    let s = h.to_string();
    assert_eq!(s.len(), 64);
    assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn path_and_content_framing_prevents_concatenation_collisions() {
    let one_file = make_config_dir(&[("lua/a.lua", "Xlua/b.luaY")]);
    let two_files = make_config_dir(&[("lua/a.lua", "X"), ("lua/b.lua", "Y")]);
    assert_ne!(
        stamp(one_file.path()).unwrap(),
        stamp(two_files.path()).unwrap()
    );
}

#[test]
fn no_init_lua_but_lua_files_still_hashed() {
    let dir = make_config_dir(&[("lua/mod.lua", "only lua dir")]);
    let h = stamp(dir.path()).unwrap();
    // Should not error even without init.lua.
    let dir2 = make_config_dir(&[]);
    let h2 = stamp(dir2.path()).unwrap();
    assert_ne!(h, h2);
}
