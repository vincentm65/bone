use bone::chat::ToolDisplay;
use bone::ui::render::messages::{msg_to_lines, render_tool};
use bone::ui::theme::Theme;
use bone::ui::tool_display::shell_row;

fn nasty_strings() -> Vec<String> {
    let mut v: Vec<String> = [
        // multibyte adjacent to every lexer token class
        "é&&ü",
        "echo \"é",
        "echo 'π",
        "echo $é",
        "2>é",
        "1>π",
        "é|ü;ç",
        "echo é#ü",
        "echo #é comment",
        "VAR=é echo ${ü}",
        "é\\ü",
        // emoji / ZWJ / 4-byte / combining / CJK / RTL
        "echo 👩‍👩‍👧‍👦 🇺🇸 𝕏",
        "echo e\u{301}e\u{301}e\u{301}",
        "ls 日本語のファイル.txt",
        "echo مرحبا שלום",
        "grep '─│╰⋮' src/",
        // unterminated quotes with multibyte tails
        "echo \"日本語",
        "echo '👍",
        // heredoc with unicode delim
        "cat <<'ÉOF'\né body ü\nÉOF",
        // operators glued to multibyte
        "π>>out ü<in ((é))",
        "café&&café||café 2>&1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // every 1..4-byte char class glued to every ASCII punct the lexer dispatches on
    for c in ['é', 'π', '一', '𝕏', '👍'] {
        for p in [
            '\'', '"', '$', '#', '|', ';', '&', '>', '<', '(', ')', '{', '}', '2', '=',
        ] {
            v.push(format!("{c}{p}{c}{p}"));
            v.push(format!("{p}{c}{p}{c}"));
        }
    }
    v
}

#[test]
fn all_render_paths_survive_arbitrary_unicode() {
    let theme = Theme::default();
    for s in nasty_strings() {
        for width in [1usize, 2, 3, 7, 16, 80] {
            // shell label + shell output (collapsed and expanded)
            for expanded in [false, true] {
                let row = shell_row(
                    &s,
                    format!("exit code: 0\nstdout:\n{s}\n{s}\nstderr:\n{s}"),
                    false,
                );
                msg_to_lines(&[row], &theme, None, width as u16, expanded);
            }
            // non-shell tool label + content (link styling path)
            let tool = ToolDisplay {
                label: format!("read_file {s} (lines 1-2, 2 read)"),
                is_error: false,
                is_shell: false,
            };
            let mut lines = Vec::new();
            render_tool(
                &tool,
                &format!("/tmp/{s} http://ex.com/{s} ./{s}"),
                0,
                &theme,
                &mut lines,
                width,
                false,
            );
        }
    }
}
