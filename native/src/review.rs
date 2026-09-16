//! Bounded, read-only, file-by-file workspace review.
use bone_protocol::HostResponse;
use eframe::egui;
use std::time::{Duration, Instant};

#[derive(Default)]
struct FileChange {
    path: String,
    status: String,
    staged: String,
    unstaged: String,
    untracked: bool,
}

struct Snapshot {
    workspace: String,
    files: Vec<FileChange>,
    truncated: bool,
}

#[derive(Default)]
pub struct Review {
    pub open: bool,
    pub window: crate::workspace::Id,
    workspace: String,
    pub pending: Option<(u64, u64, Instant)>,
    pub notice: String,
    snapshot: Option<Snapshot>,
    selected: String,
    staged: bool,
}

impl Review {
    pub fn apply(&mut self, response: HostResponse) {
        self.pending = None;
        match response {
            HostResponse::WorkspaceReview {
                workspace,
                status,
                staged,
                unstaged,
                truncated,
            } => {
                let files = parse_files(&status, &staged, &unstaged);
                if !files.iter().any(|file| file.path == self.selected) {
                    self.selected = files
                        .first()
                        .map(|file| file.path.clone())
                        .unwrap_or_default();
                }
                self.snapshot = Some(Snapshot {
                    workspace,
                    files,
                    truncated,
                });
                self.notice.clear();
            }
            HostResponse::Error { message, .. } => self.notice = message,
            _ => self.notice = "Unexpected review response from server".into(),
        }
    }

    pub fn check_pending(&mut self, connected: bool) {
        if let Some((_, _, at)) = self.pending
            && (!connected || at.elapsed() > Duration::from_secs(20))
        {
            self.pending = None;
            self.notice =
                "Review unavailable: connection lost or request timed out. Refresh to retry."
                    .into();
        }
    }

    pub fn set_workspace(&mut self, workspace: &str) -> bool {
        if self.workspace == workspace {
            return false;
        }
        self.workspace = workspace.to_owned();
        self.snapshot = None;
        self.pending = None;
        self.notice.clear();
        self.selected.clear();
        true
    }

    pub fn file_count(&self, workspace: &str) -> Option<usize> {
        self.snapshot
            .as_ref()
            .filter(|snapshot| snapshot.workspace == workspace)
            .map(|snapshot| snapshot.files.len())
    }

    /// A docked inspector; it never takes keyboard ownership from the conversation.
    pub fn show(&mut self, ui: &mut egui::Ui, colors: &crate::theme::ThemeColors) -> bool {
        let mut refresh = false;
        ui.horizontal(|ui| {
            ui.strong("Workspace changes");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::icons::button(ui, crate::icons::Icon::Close, "Close changes").clicked() {
                    self.open = false;
                }
                refresh = ui
                    .add_enabled(
                        self.pending.is_none(),
                        egui::Button::new("Refresh").frame(false),
                    )
                    .clicked();
            });
        });
        ui.label(egui::RichText::new("Entire workspace").small().weak())
            .on_hover_text("Includes changes from every task and manual edits in this workspace.");
        if !self.notice.is_empty() {
            ui.colored_label(ui.visuals().warn_fg_color, &self.notice);
        }
        let Some(snapshot) = &self.snapshot else {
            crate::surface::empty(
                ui,
                if self.pending.is_some() {
                    "Loading changes…"
                } else {
                    "No snapshot yet"
                },
                "Changes appear here when the workspace is refreshed.",
            );
            return refresh;
        };
        ui.add(
            egui::Label::new(egui::RichText::new(&snapshot.workspace).small().weak()).truncate(),
        )
        .on_hover_text(&snapshot.workspace);
        if self.pending.is_some() || !self.notice.is_empty() {
            ui.weak("Showing the previous snapshot");
        }
        if snapshot.truncated {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Large output was truncated; some changes may be incomplete.",
            );
        }
        ui.separator();
        if snapshot.files.is_empty() {
            crate::surface::empty(
                ui,
                if snapshot.truncated {
                    "No files in this partial snapshot"
                } else {
                    "Working tree clean"
                },
                "Refresh after making changes to inspect them here.",
            );
            return refresh;
        }
        let height = ui.available_height();
        if ui.available_width() >= 760.0 {
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(220.0, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(220.0);
                        render_files(
                            ui,
                            &snapshot.files,
                            &mut self.selected,
                            (height - 30.0).max(32.0),
                        );
                    },
                );
                crate::surface::divider(ui, height);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    render_diff(
                        ui,
                        &snapshot.files,
                        &self.selected,
                        &mut self.staged,
                        (height - 130.0).max(32.0),
                        colors,
                    );
                });
            });
        } else {
            let list_height = (height * 0.25).clamp(36.0, 140.0);
            render_files(ui, &snapshot.files, &mut self.selected, list_height);
            ui.separator();
            let diff_height = (ui.available_height() - 130.0).max(32.0);
            render_diff(
                ui,
                &snapshot.files,
                &self.selected,
                &mut self.staged,
                diff_height,
                colors,
            );
        }
        refresh
    }
}

fn render_files(ui: &mut egui::Ui, files: &[FileChange], selected: &mut String, height: f32) {
    ui.strong(format!("Changed files ({})", files.len()));
    egui::ScrollArea::vertical()
        .id_salt("review-files")
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for file in files {
                let label = file.path.replace('\n', "↵").replace('\t', "⇥");
                let response = ui
                    .add(egui::Button::selectable(*selected == file.path, &label).truncate())
                    .on_hover_text(&file.path);
                if response.clicked() {
                    *selected = file.path.clone();
                }
                ui.weak(&file.status);
            }
        });
}

fn render_diff(
    ui: &mut egui::Ui,
    files: &[FileChange],
    selected: &str,
    staged: &mut bool,
    height: f32,
    colors: &crate::theme::ThemeColors,
) {
    let Some(file) = files.iter().find(|file| file.path == selected) else {
        return;
    };
    ui.add(egui::Label::new(egui::RichText::new(&file.path).strong()).truncate());
    if file.untracked && file.staged.is_empty() && file.unstaged.is_empty() {
        ui.label("Untracked file");
        ui.weak("This snapshot lists new files but does not include their contents.");
        return;
    }
    // Select a side with content when changing files; preserve the choice when both exist.
    if file.staged.is_empty() {
        *staged = false;
    } else if file.unstaged.is_empty() {
        *staged = true;
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !file.unstaged.is_empty(),
                egui::Button::selectable(!*staged, "Unstaged"),
            )
            .clicked()
        {
            *staged = false;
        }
        if ui
            .add_enabled(
                !file.staged.is_empty(),
                egui::Button::selectable(*staged, "Staged"),
            )
            .clicked()
        {
            *staged = true;
        }
    });
    let diff = if *staged {
        &file.staged
    } else {
        &file.unstaged
    };
    if diff.is_empty() {
        ui.weak("No diff was received for this file. Refresh if the workspace changed.");
        return;
    }
    if ui.small_button("Copy file diff").clicked() {
        ui.ctx().copy_text(diff.clone());
    }
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    for line in diff.split_inclusive('\n').take(1000) {
        let (color, background) = if line.starts_with('+') && !line.starts_with("+++") {
            (
                colors
                    .diff_added_text
                    .unwrap_or_else(|| ui.visuals().text_color()),
                colors.diff_added,
            )
        } else if line.starts_with('-') && !line.starts_with("---") {
            (
                colors
                    .diff_removed_text
                    .unwrap_or_else(|| ui.visuals().text_color()),
                colors.diff_removed,
            )
        } else {
            (ui.visuals().text_color(), egui::Color32::TRANSPARENT)
        };
        job.append(
            line,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::monospace(13.0),
                color,
                background,
                ..Default::default()
            },
        );
    }
    egui::ScrollArea::both()
        .id_salt(("review-diff", selected, *staged))
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(job)
                    .selectable(true)
                    .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
    if diff.lines().count() > 1000 {
        ui.weak("Showing 1,000 lines. Copy file diff includes all received lines for this file.");
    }
}

fn status_label(code: &str) -> String {
    if code == "??" {
        return "Untracked".into();
    }
    let name = if code.contains('U') || code == "AA" || code == "DD" {
        "Conflict"
    } else if code.contains('R') {
        "Renamed"
    } else if code.contains('D') {
        "Deleted"
    } else if code.contains('A') {
        "Added"
    } else {
        "Modified"
    };
    let bytes = code.as_bytes();
    let staged = bytes.first().is_some_and(|b| *b != b' ' && *b != b'?');
    let unstaged = bytes.get(1).is_some_and(|b| *b != b' ' && *b != b'?');
    format!(
        "{name} · {}",
        match (staged, unstaged) {
            (true, true) => "staged and unstaged",
            (true, false) => "staged",
            _ => "unstaged",
        }
    )
}

/// Git's quoted paths use C escapes, including octal UTF-8 bytes.
fn unquote(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.into();
    };
    let mut out = Vec::new();
    let mut bytes = inner.bytes().peekable();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let Some(escaped) = bytes.next() else {
            break;
        };
        match escaped {
            b'0'..=b'7' => {
                let mut value = (escaped - b'0') as u16;
                for _ in 0..2 {
                    if bytes.peek().is_some_and(|b| (b'0'..=b'7').contains(b)) {
                        value = value * 8 + (bytes.next().unwrap() - b'0') as u16;
                    } else {
                        break;
                    }
                }
                out.push(value as u8);
            }
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'v' => out.push(11),
            b'f' => out.push(12),
            byte => out.push(byte),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn status_path(path: &str) -> String {
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in path.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quoted {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
        }
        if !quoted && path[index..].starts_with(" -> ") {
            return unquote(&path[index + 4..]);
        }
    }
    unquote(path)
}

fn diff_path(diff: &str) -> Option<String> {
    for prefix in ["+++ ", "--- "] {
        if let Some(path) = diff.lines().find_map(|line| line.strip_prefix(prefix)) {
            let path = unquote(path.trim_end_matches('\t'));
            if path != "/dev/null" {
                return Some(
                    path.strip_prefix("b/")
                        .or_else(|| path.strip_prefix("a/"))
                        .unwrap_or(&path)
                        .into(),
                );
            }
        }
    }
    // Binary and mode-only diffs have no ---/+++ headers.
    let header = diff.lines().next()?.strip_prefix("diff --git ")?;
    if header.starts_with('"') {
        let mut escaped = false;
        for (i, ch) in header.char_indices().skip(1) {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                let path = unquote(header[i + 1..].trim_start());
                return Some(path.strip_prefix("b/").unwrap_or(&path).into());
            }
        }
    }
    header.rsplit_once(" b/").map(|(_, path)| path.into())
}

fn parse_files(status: &str, staged: &str, unstaged: &str) -> Vec<FileChange> {
    let mut files = std::collections::BTreeMap::<String, FileChange>::new();
    for line in status.lines() {
        let Some(code) = line.get(..2) else {
            continue;
        };
        let Some(path) = line.get(3..) else {
            continue;
        };
        let path = status_path(path);
        files.insert(
            path.clone(),
            FileChange {
                path,
                status: status_label(code),
                untracked: code == "??",
                ..Default::default()
            },
        );
    }
    for (text, is_staged) in [(staged, true), (unstaged, false)] {
        let mut starts: Vec<usize> = text
            .match_indices("diff --git ")
            .filter(|(i, _)| *i == 0 || text.as_bytes()[i - 1] == b'\n')
            .map(|(i, _)| i)
            .collect();
        starts.push(text.len());
        for offsets in starts.windows(2) {
            let diff = &text[offsets[0]..offsets[1]];
            let Some(path) = diff_path(diff) else {
                continue;
            };
            let file = files.entry(path.clone()).or_insert_with(|| FileChange {
                path,
                status: "Changed".into(),
                ..Default::default()
            });
            if is_staged {
                file.staged.push_str(diff);
            } else {
                file.unstaged.push_str(diff);
            }
        }
    }
    files.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_each_files_staged_and_unstaged_diffs() {
        let staged =
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+staged\n";
        let unstaged = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-staged\n+working\n";
        let files = parse_files("MM a.txt\n?? new file.txt\n", staged, unstaged);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].staged, staged);
        assert_eq!(files[0].unstaged, unstaged);
        assert!(files[1].untracked);
        assert!(files[1].staged.is_empty());
    }
    #[test]
    fn handles_quoted_renamed_deleted_and_binary_paths() {
        assert_eq!(unquote(r#""caf\303\251\tfile.txt""#), "café\tfile.txt");
        assert_eq!(status_path(r#""old -> name" -> "new name""#), "new name");
        assert_eq!(
            diff_path("diff --git a/old b/old\n--- a/old\n+++ /dev/null\n"),
            Some("old".into())
        );
        assert_eq!(
            diff_path("diff --git a/image file.png b/image file.png\nBinary files differ\n"),
            Some("image file.png".into())
        );
        assert_eq!(
            diff_path("diff --git \"a/a\\tb\" \"b/a\\tb\"\nold mode 100644\nnew mode 100755\n"),
            Some("a\tb".into())
        );
    }
    #[test]
    fn refresh_retains_selection_and_errors_keep_previous_snapshot() {
        let mut review = Review::default();
        review.apply(HostResponse::WorkspaceReview {
            workspace: "/work".into(),
            status: " M a\n M b\n".into(),
            staged: String::new(),
            unstaged: String::new(),
            truncated: false,
        });
        review.selected = "b".into();
        review.apply(HostResponse::Error {
            code: bone_protocol::HostErrorCode::Unavailable,
            message: "offline".into(),
        });
        assert_eq!(review.selected, "b");
        assert_eq!(review.snapshot.as_ref().unwrap().files.len(), 2);
        assert_eq!(review.notice, "offline");
    }
}
