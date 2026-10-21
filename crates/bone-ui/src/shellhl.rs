//! Shell command highlighting: programs, flags, paths, strings, variables,
//! operators and comments.

const OPERATORS: &[&str] = &[
    "&&", "||", ">>", "2>&1", "2>", "&>", "|", ";", "&", ">", "<", "(", ")",
];

/// `(text, highlight group)` pieces covering all of `cmd`.
pub fn highlight(cmd: &str) -> Vec<(String, &'static str)> {
    let c: Vec<char> = cmd.chars().collect();
    let n = c.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut expect_cmd = true;
    let push = |out: &mut Vec<(String, &'static str)>, s: String, group: &'static str| {
        out.push((s, group))
    };
    while i < n {
        if c[i].is_whitespace() {
            let start = i;
            while i < n && c[i].is_whitespace() {
                i += 1;
            }
            out.push((c[start..i].iter().collect::<String>(), "Normal"));
            continue;
        }
        if c[i] == '#' {
            push(&mut out, c[i..].iter().collect(), "ShellComment");
            break;
        }
        let rest: String = c[i..].iter().take(4).collect();
        if let Some(op) = OPERATORS.iter().find(|op| rest.starts_with(**op)) {
            push(&mut out, (*op).to_owned(), "ShellOperator");
            i += op.chars().count();
            expect_cmd = !matches!(*op, ">" | ">>" | "<" | "2>" | "&>" | "2>&1" | ")");
            continue;
        }
        // A word, with quotes allowed inside it.
        let start = i;
        while i < n && !c[i].is_whitespace() && !"|;&<>()".contains(c[i]) {
            if c[i] == '\'' || c[i] == '"' {
                let q = c[i];
                i += 1;
                while i < n && c[i] != q {
                    i += if c[i] == '\\' && q == '"' { 2 } else { 1 };
                }
                i = (i + 1).min(n);
            } else {
                i += 1;
            }
        }
        let word: String = c[start..i].iter().collect();
        let group = if word.starts_with(['\'', '"']) {
            "ShellString"
        } else if word.starts_with('$') {
            "ShellVariable"
        } else if expect_cmd && word.contains('=') && !word.starts_with('=') {
            // FOO=bar before a command.
            push(&mut out, word, "ShellVariable");
            continue;
        } else if expect_cmd {
            expect_cmd = false;
            "ShellProgram"
        } else if word.starts_with('-') {
            "ShellFlag"
        } else if word.contains('/') || word.starts_with(['.', '~']) {
            "ShellPath"
        } else {
            "ToolArgs"
        };
        push(&mut out, word, group);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_tokens() {
        let got: Vec<(String, &str)> = highlight(
            r#"RUST_LOG=1 cargo test -p x ./src "a b" $HOME | grep -v foo > out.txt # done"#,
        )
        .into_iter()
        .filter(|(t, _)| !t.trim().is_empty())
        .collect();
        let want = [
            ("RUST_LOG=1", "ShellVariable"),
            ("cargo", "ShellProgram"),
            ("test", "ToolArgs"),
            ("-p", "ShellFlag"),
            ("x", "ToolArgs"),
            ("./src", "ShellPath"),
            ("\"a b\"", "ShellString"),
            ("$HOME", "ShellVariable"),
            ("|", "ShellOperator"),
            ("grep", "ShellProgram"),
            ("-v", "ShellFlag"),
            ("foo", "ToolArgs"),
            (">", "ShellOperator"),
            ("out.txt", "ToolArgs"),
            ("# done", "ShellComment"),
        ];
        assert_eq!(
            got,
            want.iter()
                .map(|(t, g)| (t.to_string(), *g))
                .collect::<Vec<_>>()
        );
        let rebuilt: String = highlight("a  'b c'&&d")
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert_eq!(rebuilt, "a  'b c'&&d");
    }
}
