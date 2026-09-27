//! The user's external editor command (`$VISUAL`, then `$EDITOR`).

/// The editor command line, split into program and arguments.
pub fn editor_command() -> Vec<String> {
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

pub fn default_editor() -> &'static str {
    if cfg!(windows) { "notepad" } else { "nano" }
}

pub fn split_editor_command(command: &str) -> Vec<String> {
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
    fn split_editor_command_cases() {
        for (command, expected) in [
            ("code -w", vec!["code", "-w"]),
            (
                "\"/opt/Editor With Spaces/editor\" --wait",
                vec!["/opt/Editor With Spaces/editor", "--wait"],
            ),
        ] {
            assert_eq!(
                split_editor_command(command),
                expected,
                "command: {command}"
            );
        }
    }
}
