//! External-editor integration (`InputAction::OpenEditor`): drop to the user's
//! `$VISUAL`/`$EDITOR`, then read the edited text back into the input buffer.

use std::io;
use std::path::Path;

use super::super::render::Renderer;
use super::App;
use bone_render::editor::editor_command;

impl App {
    pub(super) async fn open_editor(&mut self, term: &mut super::BoneTerminal) -> io::Result<()> {
        let tmp = editor_temp_path()?;
        let editor = editor_command();

        self.reset_terminal_background();
        Renderer::prepare_exit(term)?;
        Renderer::shutdown_terminal()?;

        let editor_result = run_editor(&editor, tmp.as_ref()).await;
        let text_result = if editor_result.as_ref().is_ok_and(|status| status.success()) {
            Some(std::fs::read_to_string(&tmp))
        } else {
            None
        };

        let physical_size = crossterm::terminal::size()?;
        let initial_height = crate::ui::render::initial_viewport_height(physical_size.1);
        *term = Renderer::init_terminal(initial_height)?;
        self.apply_terminal_background();
        self.renderer.viewport_height = initial_height;
        self.renderer.last_size = Some(physical_size);
        self.flush_new_messages_to_scrollback(term)?;

        match editor_result {
            Ok(status) if status.success() => {}
            Ok(status) => {
                return self.show_reply(format!("Editor exited with status: {status}"), term);
            }
            Err(err) => {
                return self.show_reply(
                    format!("Editor failed: {err}. Set VISUAL or EDITOR to an installed editor."),
                    term,
                );
            }
        }

        let text = match text_result {
            Some(Ok(text)) => text,
            Some(Err(err)) => {
                return self.show_reply(format!("Could not read editor input: {err}"), term);
            }
            None => String::new(),
        };
        let text = text.trim_end_matches(['\r', '\n']).to_string();
        if !text.trim().is_empty() {
            self.input.buffer = text;
            self.input.cursor_pos = self.input.buffer.chars().count();
        }

        self.force_redraw(term)
    }
}

fn editor_temp_path() -> io::Result<tempfile::TempPath> {
    tempfile::Builder::new()
        .prefix("bone-edit-")
        .suffix(".txt")
        .tempfile()
        .map(tempfile::NamedTempFile::into_temp_path)
}

async fn run_editor(editor: &[String], path: &Path) -> io::Result<std::process::ExitStatus> {
    let Some(program) = editor.first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "editor command is empty",
        ));
    };

    tokio::process::Command::new(program)
        .args(&editor[1..])
        .arg(path)
        .spawn()
        .map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("could not launch `{}`: {err}", editor.join(" ")),
            )
        })?
        .wait()
        .await
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod editor_tests;
