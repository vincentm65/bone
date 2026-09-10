//! Explicit local-editor handoff; daemon paths are never silently treated as local.
use eframe::egui;
use std::path::{Component, Path, PathBuf};

pub fn parse(text: &str) -> Option<(PathBuf, Option<u32>)> {
    if text.is_empty()
        || text.len() > 2048
        || text.chars().any(|c| c.is_control())
        || text.contains("://")
    {
        return None;
    }
    let (path, line) = if let Some((path, line)) = text.rsplit_once(":")
        && let Some(line) = line.parse::<u32>().ok().filter(|n| *n > 0)
    {
        // Only a numeric suffix is a line reference. This preserves a plain
        // Windows drive path such as `C:\\src\\main.rs`.
        (path, Some(line))
    } else if let Some((path, line)) = text.rsplit_once("#L") {
        (path, Some(line.parse::<u32>().ok().filter(|n| *n > 0)?))
    } else {
        (text, None)
    };
    let drive_prefix =
        path.len() >= 2 && path.as_bytes()[0].is_ascii_alphabetic() && path.as_bytes()[1] == b':';
    if path
        .char_indices()
        .any(|(index, character)| character == ':' && !(drive_prefix && index == 1))
        || path.contains(['#', '?'])
    {
        return None;
    }
    let path = PathBuf::from(path);
    if path.extension().is_none() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some((path, line))
}

pub fn set_workspace(ctx: &egui::Context, workspace: &str) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("file-ref-workspace"), workspace.to_string()));
}

pub fn request(ctx: &egui::Context, reference: &str) {
    let Some((path, line)) = parse(reference) else {
        return;
    };
    let workspace = ctx
        .data(|d| d.get_temp::<String>(egui::Id::new("file-ref-workspace")))
        .unwrap_or_default();
    let root = Path::new(&workspace);
    if !root.is_absolute() {
        ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new("file-ref-error"),
                "File unavailable: the daemon has not supplied an absolute workspace path."
                    .to_string(),
            )
        });
        return;
    }
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    if !is_within_workspace(root, &path) {
        ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new("file-ref-error"),
                "File unavailable: this reference is outside the daemon workspace.".to_string(),
            )
        });
        return;
    }
    ctx.data_mut(|d| {
        d.insert_temp(
            egui::Id::new("file-ref-pending"),
            (path.display().to_string(), line),
        )
    });
}

/// Check both the lexical path and the resolved filesystem path. The daemon
/// may describe a remote workspace that is not mounted locally, so a missing
/// path falls back to the lexical check. When any part exists locally,
/// canonicalizing the nearest existing ancestor catches symlink escapes while
/// still allowing a not-yet-created source file to be opened in an editor.
fn is_within_workspace(root: &Path, path: &Path) -> bool {
    if !path.starts_with(root) {
        return false;
    }

    let Some(canonical_root) = std::fs::canonicalize(root).ok() else {
        return true;
    };

    let mut existing = path;
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return true;
        };
        if parent == existing {
            return true;
        }
        existing = parent;
    }
    let Ok(canonical_existing) = std::fs::canonicalize(existing) else {
        return true;
    };
    canonical_existing.starts_with(canonical_root)
}

fn editor_url(editor: &str, path: &str, line: Option<u32>) -> String {
    let encoded: String = path
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!(
        "{editor}://file{encoded}{}",
        line.map(|n| format!(":{n}")).unwrap_or_default()
    )
}

pub fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| {
        d.get_temp::<(String, Option<u32>)>(egui::Id::new("file-ref-pending"))
            .is_some()
            || d.get_temp::<String>(egui::Id::new("file-ref-error"))
                .is_some()
    })
}

pub fn dialog(ctx: &egui::Context) {
    let error_id = egui::Id::new("file-ref-error");
    if let Some(error) = ctx.data(|d| d.get_temp::<String>(error_id)) {
        let response = crate::surface::modal(ctx, error_id).show(ctx, |ui| {
            ui.label(error);
            ui.button("Close").clicked()
        });
        if response.inner || response.should_close() {
            ctx.data_mut(|d| d.remove::<String>(error_id));
        }
        return;
    }
    let id = egui::Id::new("file-ref-pending");
    let Some((path, line)) = ctx.data(|d| d.get_temp::<(String, Option<u32>)>(id)) else {
        return;
    };
    let mut done = false;
    let response = crate::surface::modal(ctx, egui::Id::new("open-file-editor")).show(ctx, |ui| {
        ui.set_max_width(500.0);
        ui.heading("Open file in a local editor?");
        ui.label(&path);
        if let Some(line) = line { ui.label(format!("Line {line}")); }
        ui.label("This path comes from the daemon. For remote daemons, continue only if the same workspace is mounted at this path on this computer.");
        ui.horizontal(|ui| {
            for (name, scheme) in [("Zed", "zed"), ("VS Code", "vscode")] {
                if ui.button(name).clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(editor_url(scheme, &path, line)));
                    done = true;
                }
            }
            if ui.button("Cancel").clicked() { done = true; }
        });
        ui.weak("Requires the editor's URL handler. No file is edited by Bone.");
    });
    if done || response.should_close() {
        ctx.data_mut(|d| d.remove::<(String, Option<u32>)>(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_are_bounded_and_editor_urls_cannot_inject_queries() {
        assert_eq!(
            parse("src/main.rs:42"),
            Some((PathBuf::from("src/main.rs"), Some(42)))
        );
        assert!(parse("../secret.txt").is_none());
        assert!(parse("https://example.com/a.rs").is_none());
        assert!(parse("command:run").is_none());
        assert_eq!(
            parse(r"C:\work\src\main.rs:42"),
            Some((PathBuf::from(r"C:\work\src\main.rs"), Some(42)))
        );
        assert_eq!(
            parse(r"C:\work\src\main.rs"),
            Some((PathBuf::from(r"C:\work\src\main.rs"), None))
        );
        assert_eq!(
            editor_url("vscode", "/work/a b.rs", Some(9)),
            "vscode://file/work/a%20b.rs:9"
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_symlink_cannot_escape_workspace() {
        let root = std::env::temp_dir().join(format!(
            "bone-file-ref-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let outside = root.with_extension("outside");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(outside.join("src")).unwrap();
        std::fs::write(outside.join("src/secret.rs"), b"secret").unwrap();

        std::os::unix::fs::symlink(outside.join("src"), root.join("src/link")).unwrap();

        assert!(!is_within_workspace(
            &root,
            &root.join("src/link/secret.rs")
        ));
        assert!(is_within_workspace(&root, &root.join("src/new.rs")));

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
