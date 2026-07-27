use std::fs;

mod common;

#[test]
fn named_remove_cleans_up_partial_installation() {
    let fixture = common::temp_dir("catalog-named-fixture");
    let bone_dir = common::temp_dir("catalog-named-config");
    fs::create_dir_all(&fixture).unwrap();
    fs::write(
        fixture.join("catalog.json"),
        r#"[{"name":"demo.lua","kind":"tool","files":[{"path":"themes/demo.lua"},{"path":"assets/demo/README.md"}]}]"#,
    )
    .unwrap();

    let _guard = common::isolate_bone_dir(&bone_dir);
    // SAFETY: this integration test is the only test in its process.
    unsafe { std::env::set_var("BONE_CATALOG_URL", &fixture) };

    let primary = bone_dir.join("lua/tools/demo.lua");
    let bundled = bone_dir.join("lua/themes/demo.lua");
    let missing = bone_dir.join("lua/assets/demo/README.md");
    fs::create_dir_all(primary.parent().unwrap()).unwrap();
    fs::create_dir_all(bundled.parent().unwrap()).unwrap();
    fs::write(&primary, "return {}\n").unwrap();
    fs::write(&bundled, "return {}\n").unwrap();
    assert!(!missing.exists());

    let outcome = bone::ui::catalog::apply_named("remove", "demo");

    assert!(outcome.changed, "{}", outcome.message);
    assert_eq!(outcome.message, "Catalog item removed: demo");
    assert!(!primary.exists());
    assert!(!bundled.exists());

    fs::remove_dir_all(&fixture).ok();
    fs::remove_dir_all(&bone_dir).ok();
}
