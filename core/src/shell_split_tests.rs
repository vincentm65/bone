use super::*;

#[test]
fn policy_style() {
    let opts = ShellSplitOptions {
        keep_separators: false,
        split_newlines: true,
        strip_comments: true,
    };
    let segs = shell_split("echo hello && echo world # comment\nls -la", &opts);
    assert_eq!(segs, vec!["echo hello", "echo world", "ls -la"]);
}

#[test]
fn display_style() {
    let opts = ShellSplitOptions {
        keep_separators: true,
        split_newlines: false,
        strip_comments: false,
    };
    let segs = shell_split("echo hello && echo world", &opts);
    assert_eq!(segs, vec!["echo hello &&", "echo world"]);
}

#[test]
fn quoted_separators_ignored() {
    let opts = ShellSplitOptions {
        keep_separators: false,
        split_newlines: true,
        strip_comments: false,
    };
    let segs = shell_split("echo 'hello;world' && ls", &opts);
    assert_eq!(segs, vec!["echo 'hello;world'", "ls"]);
}
