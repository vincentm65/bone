//! External-editor integration (`InputAction::OpenEditor`): drop to the user's
//! `$VISUAL`/`$EDITOR`, then read the edited text back into the input buffer.

use std::io;
use std::path::Path;

use super::super::render::{MIN_ROWS, Renderer};
use super::App;

impl App {
    pub(super) async fn open_editor(&mut self, term: &mut super::BoneTerminal) -> io::Result<()> {
        let tmp = std::env::temp_dir().join("bone-edit.txt");
        std::fs::write(&tmp, "")?;
        let editor = editor_command();

        Renderer::prepare_exit(term)?;
        Renderer::shutdown_terminal()?;

        let editor_result = run_editor(&editor, &tmp).await;
        let text_result = if editor_result.as_ref().is_ok_and(|status| status.success()) {
            Some(std::fs::read_to_string(&tmp))
        } else {
            None
        };
        std::fs::remove_file(&tmp).ok();

        *term = Renderer::init_terminal(MIN_ROWS)?;
        self.renderer.viewport_height = MIN_ROWS;
        self.renderer
            .flush_new_to_scrollback(&self.messages, term)?;

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

fn editor_command() -> Vec<String> {
    std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .map(|value| split_editor_command(&value))
        .filter(|parts| !parts.is_empty())
        .unwrap_or_else(|| vec![default_editor().to_string()])
}

fn default_editor() -> &'static str {
    if cfg!(windows) { "notepad" } else { "nano" }
}

fn split_editor_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        if !cfg!(windows) && ch == '\\' {
            escaped = true;
            continue;
        }

        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }

        match ch {
            '"' | '\'' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_editor_command_keeps_args() {
        assert_eq!(split_editor_command("code -w"), vec!["code", "-w"]);
    }

    #[test]
    fn split_editor_command_respects_quotes() {
        assert_eq!(
            split_editor_command("\"/opt/Editor With Spaces/editor\" --wait"),
            vec!["/opt/Editor With Spaces/editor", "--wait"]
        );
    }

    #[test]
    fn default_editor_is_platform_specific() {
        if cfg!(windows) {
            assert_eq!(default_editor(), "notepad");
        } else {
            assert_eq!(default_editor(), "nano");
        }
    }
}
