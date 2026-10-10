//! Reloading Lua when its files change: a failed TUI reload keeps the old
//! configuration, ctrl+c survives a broken one, and the watcher tells the
//! TUI's files from the core's.

use std::path::Path;

use super::*;
use crate::{Changed, LuaSources, Watched, lua_snapshot, settle, side_of};

fn eval(h: &mut Harness, code: &str) -> String {
    h.app.with_api(|lua| lua.load(code).eval()).unwrap()
}

fn last_log(h: &Harness) -> String {
    h.app.log.last().cloned().unwrap_or_default()
}

#[tokio::test]
async fn a_syntax_error_keeps_the_previous_config() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(
        &tui,
        r#"MARK = "old"
           bone.keymap.set("ctrl+g", "quit")"#,
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();

    std::fs::write(&tui, "MARK = = 'half-typed'").unwrap();
    h.app.reload_user_config();
    let log = last_log(&h);
    assert!(
        log.starts_with("Lua reload failed, kept the previous configuration."),
        "{log}"
    );
    assert_eq!(eval(&mut h, "return MARK"), "old");
    let ctrl_g = vec![keys::parse("ctrl+g").unwrap()];
    assert!(
        h.app
            .keymaps
            .lookup(&Context::Main, &ctrl_g)
            .action
            .is_some()
    );
    assert!(h.app.lua_errors.iter().any(|e| e.starts_with("reload: ")));

    // Fixing the file is picked up by the next reload.
    std::fs::write(&tui, r#"MARK = "new""#).unwrap();
    h.app.reload_user_config();
    assert_eq!(last_log(&h), "Lua configuration reloaded");
    assert_eq!(eval(&mut h, "return MARK"), "new");
    assert!(
        h.app
            .keymaps
            .lookup(&Context::Main, &ctrl_g)
            .action
            .is_none()
    );
}

#[tokio::test]
async fn an_error_partway_through_keeps_the_previous_config() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(&tui, r#"MARK = "old""#).unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();

    std::fs::write(
        &tui,
        r#"MARK = "half"
           error("boom")"#,
    )
    .unwrap();
    h.app.reload_user_config();
    assert_eq!(eval(&mut h, "return MARK"), "old");
}

#[tokio::test]
async fn an_error_the_old_config_had_does_not_block_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(
        &tui,
        r#"MARK = "a"
           error("still broken")"#,
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();

    std::fs::write(
        &tui,
        r#"MARK = "b"
           error("still broken")"#,
    )
    .unwrap();
    h.app.reload_user_config();
    assert_eq!(eval(&mut h, "return MARK"), "b");
}

#[tokio::test]
async fn shutdown_hooks_run_only_when_the_reload_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    let out = dir.path().join("shutdowns.txt");
    let config = format!(
        r#"bone.plugin.on_shutdown(function()
             local f = io.open({:?}, "a") f:write("x") f:close()
           end)"#,
        out.display().to_string()
    );
    std::fs::write(&tui, &config).unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();

    std::fs::write(&tui, "error('broken')").unwrap();
    h.app.reload_user_config();
    assert!(
        !out.exists(),
        "a failed reload must not shut the old config down"
    );

    std::fs::write(&tui, &config).unwrap();
    h.app.reload_user_config();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "x");
}

#[tokio::test]
async fn ready_after_a_reload_names_the_panels_that_were_open() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(&tui, r#"bone.ui.panel({ id = "todo", lines = { "x" } })"#).unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    assert_eq!(h.app.panels.len(), 1);

    std::fs::write(
        &tui,
        r#"bone.on("ready", function(d)
             READY = tostring(d.reload) .. ":" .. table.concat(d.panels, ",")
           end)"#,
    )
    .unwrap();
    h.app.reload_user_config();
    assert_eq!(eval(&mut h, "return READY"), "true:todo");
    assert!(h.app.panels.is_empty());
}

#[tokio::test]
async fn ctrl_c_quits_when_lua_maps_no_keys() {
    let dir = tempfile::tempdir().unwrap();
    // An empty copy of the defaults drops every default keymap.
    let defaults = dir.path().join("runtime/tui/defaults.lua");
    std::fs::create_dir_all(defaults.parent().unwrap()).unwrap();
    std::fs::write(&defaults, "").unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    let ctrl_c = vec![keys::Key::ctrl_c()];
    assert!(
        h.app
            .keymaps
            .lookup(&Context::Main, &ctrl_c)
            .action
            .is_none()
    );

    h.input("hello").await;
    h.input("{ctrl+c}").await;
    assert!(h.app.prompt.is_empty());
    h.input("{ctrl+c}{ctrl+c}").await;
    assert!(h.app.quit.is_some());
}

#[tokio::test]
async fn ctrl_c_mapped_only_elsewhere_still_interrupts_here() {
    let dir = tempfile::tempdir().unwrap();
    let defaults = dir.path().join("runtime/tui/defaults.lua");
    std::fs::create_dir_all(defaults.parent().unwrap()).unwrap();
    std::fs::write(
        &defaults,
        r#"bone.keymap.set("ctrl+c", "dismiss", { context = "popup" })"#,
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    h.input("hello{ctrl+c}").await;
    assert!(h.app.prompt.is_empty());
}

#[tokio::test]
async fn replies_to_a_rejected_load_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(&tui, "").unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    h.settle().await;

    std::fs::write(
        &tui,
        r#"bone.request("session/list", {}, function() end)
           bone.defer(1, function() end)
           error("broken")"#,
    )
    .unwrap();
    h.app.reload_user_config();
    h.settle().await;
    let log = last_log(&h);
    assert!(log.starts_with("Lua reload failed"), "{log}");
    assert!(
        !h.app.log.iter().any(|l| l.contains("callback")),
        "{:?}",
        h.app.log
    );
}

#[tokio::test]
async fn a_rejected_load_leaves_built_in_options_alone() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(&tui, "").unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();
    assert!(h.app.options.mouse);

    std::fs::write(
        &tui,
        r#"bone.o.mouse = false
           error("broken")"#,
    )
    .unwrap();
    h.app.reload_user_config();
    assert!(h.app.options.mouse);
}

#[tokio::test]
async fn a_new_error_in_a_file_that_already_failed_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(
        &tui,
        r#"MARK = "old"
           error("late")"#,
    )
    .unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();

    // A syntax error is not the error the file already had.
    std::fs::write(&tui, "MARK = = 1\nerror('late')").unwrap();
    h.app.reload_user_config();
    assert_eq!(eval(&mut h, "return MARK"), "old");

    // The old error, moved down a line by an edit above it, is.
    std::fs::write(&tui, "MARK = 'new'\n\nerror('late')").unwrap();
    h.app.reload_user_config();
    assert_eq!(eval(&mut h, "return MARK"), "new");
}

#[tokio::test]
async fn an_error_in_a_ready_handler_rejects_the_reload() {
    let dir = tempfile::tempdir().unwrap();
    let tui = dir.path().join("tui.lua");
    std::fs::write(&tui, r#"MARK = "old""#).unwrap();
    let mut h = Harness::build(Some(dir.path().to_owned())).await;
    h.app.load_user_config();

    std::fs::write(
        &tui,
        r#"MARK = "new"
           bone.on("ready", function() error("in ready") end)"#,
    )
    .unwrap();
    h.app.reload_user_config();
    assert_eq!(eval(&mut h, "return MARK"), "old");
}

#[tokio::test]
async fn a_mapped_ctrl_c_does_what_lua_says() {
    // A handler that runs keeps the prompt; one that errors or returns
    // false passes the key on, so it still interrupts.
    for (handler, interrupts) in [
        ("function() PRESSED = 'yes' end", false),
        ("function() error('oops') end", true),
        ("function() return false end", true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tui.lua"),
            format!(r#"bone.keymap.set("ctrl+c", {handler})"#),
        )
        .unwrap();
        let mut h = Harness::build(Some(dir.path().to_owned())).await;
        h.app.load_user_config();
        h.input("hello{ctrl+c}").await;
        assert_eq!(h.app.prompt.is_empty(), interrupts, "{handler}");
        if !interrupts {
            assert_eq!(eval(&mut h, "return PRESSED"), "yes");
        }
    }
}

#[tokio::test]
async fn core_reload_requests_and_reports_failures() {
    let mut h = Harness::build(None).await;
    *h.fail.lock().unwrap() = Some("core/reload".into());
    h.app.reload_core_config();
    h.settle().await;
    assert_eq!(h.requests("core/reload").len(), 1);
    let log = last_log(&h);
    assert!(
        log.starts_with("core reload failed, kept the previous configuration"),
        "{log}"
    );
}

#[test]
fn files_belong_to_the_side_that_loads_them() {
    let side = |p: &str| side_of(Path::new(p));
    assert_eq!(side("tui.lua"), Watched::Tui);
    assert_eq!(side("colors/black.lua"), Watched::Tui);
    assert_eq!(side("runtime/tui/defaults.lua"), Watched::Tui);
    assert_eq!(side("runtime/colors/black.lua"), Watched::Tui);
    assert_eq!(side("plugins/git/tui.lua"), Watched::Tui);
    assert_eq!(side("plugins/git/colors/x.lua"), Watched::Tui);
    assert_eq!(side("core.lua"), Watched::Core);
    assert_eq!(side("runtime/core/api.lua"), Watched::Core);
    assert_eq!(side("plugins/git/core.lua"), Watched::Core);
    assert_eq!(side("lua/mine.lua"), Watched::Both);
    assert_eq!(side("lua/tui/x.lua"), Watched::Both);
    assert_eq!(side("runtime/lua/bone/util.lua"), Watched::Both);
    assert_eq!(side("plugins/git/lua/git.lua"), Watched::Both);
}

#[test]
fn the_snapshot_changes_only_for_the_side_that_loads_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let write = |rel: &str, text: &str| {
        let p = d.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write("tui.lua", "");
    write("core.lua", "");
    let base = lua_snapshot(Some(d), None);

    write("plugins/p/core.lua", "-- core");
    let after_core = lua_snapshot(Some(d), None);
    assert_ne!(after_core.core, base.core);
    assert_eq!(after_core.tui, base.tui);

    write("plugins/p/tui.lua", "-- tui");
    let after_tui = lua_snapshot(Some(d), None);
    assert_ne!(after_tui.tui, after_core.tui);
    assert_eq!(after_tui.core, after_core.core);

    write("lua/shared.lua", "-- both");
    let after_both = lua_snapshot(Some(d), None);
    assert_ne!(after_both.tui, after_tui.tui);
    assert_ne!(after_both.core, after_tui.core);

    // Hidden folders (a cloned plugin's .git) are not walked.
    write("plugins/p/.git/hooks/x.lua", "");
    assert_eq!(lua_snapshot(Some(d), None), after_both);

    // A trusted project's .bone/ is the TUI's.
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("tui.lua"), "").unwrap();
    let with_project = lua_snapshot(Some(d), Some(project.path()));
    assert_ne!(with_project.tui, after_both.tui);
    assert_eq!(with_project.core, after_both.core);
}

#[test]
fn a_change_reloads_only_once_it_has_settled() {
    let at = |tui, core| LuaSources { tui, core };
    let mut state = at(1, 1);
    let mut pending = None;
    // Nothing changed.
    assert_eq!(settle(&mut state, &mut pending, at(1, 1)), None);
    // Seen once: maybe still being written.
    assert_eq!(settle(&mut state, &mut pending, at(2, 1)), None);
    // Still changing: wait again.
    assert_eq!(settle(&mut state, &mut pending, at(3, 1)), None);
    // The same twice: reload what changed.
    assert_eq!(
        settle(&mut state, &mut pending, at(3, 1)),
        Some(Changed {
            tui: true,
            core: false
        })
    );
    assert_eq!(state, at(3, 1));
    // An edit undone before it settled reloads nothing.
    assert_eq!(settle(&mut state, &mut pending, at(3, 2)), None);
    assert_eq!(settle(&mut state, &mut pending, at(3, 1)), None);
    assert_eq!(pending, None);
}
