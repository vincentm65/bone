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

#[test]
fn substitution_quotes_do_not_close_the_substitution() {
    let opts = ShellSplitOptions {
        keep_separators: false,
        split_newlines: true,
        strip_comments: false,
    };
    for command in [
        "echo $(printf \"%s\" \" ) && ; \" && true); rm -rf /home/example",
        "echo $(printf '%s' ' ) && ; ' && true); rm -rf /home/example",
    ] {
        let segs = shell_split(command, &opts);
        assert_eq!(
            segs,
            vec![
                format!("{})", command.split_once("); rm").unwrap().0),
                "rm -rf /home/example".to_string(),
            ],
            "command: {command}"
        );
    }
}

#[test]
fn nested_substitutions_and_grouping_keep_inner_connectors() {
    let opts = ShellSplitOptions {
        keep_separators: false,
        split_newlines: true,
        strip_comments: false,
    };
    let command =
        "echo $( (true && printf ok); printf \"$(false || true)\" ) ; rm -rf /home/example";
    assert_eq!(
        shell_split(command, &opts),
        vec![
            "echo $( (true && printf ok); printf \"$(false || true)\" )",
            "rm -rf /home/example",
        ]
    );
}

#[test]
fn comments_end_at_newline_inside_and_outside_substitutions() {
    let opts = ShellSplitOptions {
        keep_separators: false,
        split_newlines: true,
        strip_comments: true,
    };
    assert_eq!(
        shell_split(
            "echo $(true # inner ) still comment\nprintf ok); rm -rf /home/example",
            &opts
        ),
        vec![
            "echo $(true # inner ) still comment\nprintf ok)".to_string(),
            "rm -rf /home/example".to_string(),
        ]
    );
    assert_eq!(
        shell_split("echo ok # outer\nrm -rf /home/example", &opts),
        vec!["echo ok", "rm -rf /home/example"]
    );
}

#[test]
fn escaped_separators_stay_in_the_word_and_do_not_start_comments() {
    let opts = ShellSplitOptions {
        keep_separators: false,
        split_newlines: true,
        strip_comments: true,
    };
    // A backslash-quoted character is literal data, so it neither ends the word
    // nor makes the following `#` a comment: bash runs the tail for real.
    for (command, head) in [
        (r"echo a\;# x; rm -rf /home/example", r"echo a\;# x"),
        (r"echo a\)# z; rm -rf /home/example", r"echo a\)# z"),
        (r"echo a\|# q; rm -rf /home/example", r"echo a\|# q"),
        (r"echo a\&# r; rm -rf /home/example", r"echo a\&# r"),
        (r"echo a\(# s; rm -rf /home/example", r"echo a\(# s"),
        (r"echo a\ # y; rm -rf /home/example", r"echo a\ # y"),
        (r"echo a\\# w; rm -rf /home/example", r"echo a\\# w"),
        (r"echo a\;; rm -rf /home/example", r"echo a\;"),
    ] {
        assert_eq!(
            shell_split(command, &opts),
            vec![head.to_string(), "rm -rf /home/example".to_string()],
            "command: {command}"
        );
    }
    // An unescaped separator still ends the word, so a following `#` is a comment
    // and the tail is genuinely one:
    assert_eq!(
        shell_split(r"echo a; # note; rm -rf /home/example", &opts),
        vec!["echo a"]
    );
}
