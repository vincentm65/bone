//! Pure string-classification tests for the destructive-command guard.
//!
//! These tests never spawn a process, never execute a command, and never call
//! [`GuardRoots::detect`] — the roots are fixed fakes, so nothing here touches
//! the real home directory or the config directory.

use super::*;
use std::path::PathBuf;

fn roots() -> GuardRoots {
    GuardRoots {
        home: Some(PathBuf::from("/home/example")),
        workspace: Some(PathBuf::from("/home/example/projects/bone")),
    }
}

fn assert_denied(command: &str) {
    assert!(
        hard_deny(command, &roots()).is_some(),
        "expected guard to deny: {command}"
    );
}

fn assert_allowed(command: &str) {
    assert!(
        hard_deny(command, &roots()).is_none(),
        "expected guard to allow: {command}"
    );
}

#[test]
fn denies_home_and_root_deletions() {
    for command in [
        "rm -rf ~",
        "rm -rf $HOME",
        "rm -rf ${HOME}",
        "rm -rf /",
        "rm -rf /*",
        "rm -rf /home/example",
        "rm -rf ~/",
        "rm -rf ..",
        "rm -rf ../..",
        "rm -rf .",
        "rm -rf *",
        "rm -rf target/*",
        "rm -rf",
    ] {
        assert_denied(command);
    }
}

#[test]
fn denies_shell_wrappers_and_privilege_escalation() {
    for command in [
        "bash -c 'rm -rf ~'",
        "sh -c \"rm -rf $HOME\"",
        "sudo rm -rf /home/example",
        "env rm -rf ~",
        "cd ~ && rm -rf .",
    ] {
        assert_denied(command);
    }
}

#[test]
fn denies_wrappers_after_connectors_and_nested_wrappers() {
    for command in [
        "true && bash -c 'dd if=/dev/zero of=/dev/sda'",
        "sudo bash -c 'rm -rf /home/example'",
    ] {
        assert_denied(command);
    }
}

#[test]
fn handles_command_substitutions_recursively() {
    for command in [
        r#"echo "lines=$(wc -l < results.jsonl)""#,
        r#"a && echo "$(true && printf ok)" || b"#,
        r#"sleep 600; cd /home/example/bench; echo "lines=$(wc -l < .bone-bench/results.jsonl)"; tail -4 .bone-bench/results.jsonl | grep -o '"run_id":"[^"]*"' | paste -sd' ' -; ls .bone-bench/worktrees/"#,
        r#"echo '$(rm -rf /)'"#,
        "echo \"$(printf '%s' ')')\"",
        "echo $( (true && printf ok); printf done )",
        "echo $(true # comment )\nprintf ok)",
        // A `#` inside a substitution is part of that command, not a comment on the
        // outer line: the newline it runs up to must stay visible to the splitter.
        "echo $(true # counting\n)",
        "echo $(true\n)",
    ] {
        assert_allowed(command);
    }
    for command in [
        "echo $(rm -rf /)",
        "echo $(true && rm -rf /home/example)",
        "echo $(echo $(rm -rf /))",
        // The `;` that follows the substitution is a real top-level separator, so
        // the trailing `rm` is a second segment and must still be refused.
        "echo $(true # note\n); rm -rf /home/example",
        // Here the newline is *inside* the substitution, so the `rm` is part of
        // the substitution body and must be refused there.
        "echo $(true # note\nrm -rf /home/example)",
    ] {
        assert_denied(command);
    }
}

#[test]
fn substitutions_are_inspected_with_the_segment_cwd() {
    // The substitution runs after the `cd`, so `worktrees` resolves under the
    // directory the segment changed into, not under the workspace; deletion
    // outside the workspace is allowed.
    assert_allowed("cd /home/example/bench; echo \"$(rm -rf worktrees)\"");
    // The reverse must also hold: a `cd` *inside* the substitution runs in a
    // subshell, so it must not leak out and make the later relative `sub`
    // resolve under `/home/example`.
    assert_allowed("echo $(cd /home/example); rm -rf sub");
    // Quotes and ordinary grouping inside a substitution must not make its
    // closing delimiter look like an earlier command boundary.
    assert_denied("echo $(printf \" ) && ; \"; rm -rf /home/example)");
    assert_denied("echo $( (cd /home/example); rm -rf . )");
}

#[test]
fn denies_dynamic_command_names() {
    // The program name is only known at run time, so the literal-name verb checks
    // cannot see it; the guard refuses instead of guessing.
    for command in [
        "$(echo git) clean -f",
        "$(echo rm) -rf /home/example/projects",
        "${CMD} clean -f",
        "sudo $(echo rm) -rf /home/example",
    ] {
        assert_denied(command);
    }
    // A substitution in an *argument* leaves the program name literal and known.
    assert_allowed("git commit -m \"$(date)\"");
}

#[test]
fn still_denies_unsupported_nested_syntax() {
    assert_denied("echo `rm -rf /`");
    assert_denied("cat <(rm -rf /)");
}

#[test]
fn denies_shell_escapes_that_reconstruct_dangerous_tokens() {
    for command in [
        r"r\m -rf /home/example",
        r"r''m -rf /home/example",
        r"rm -rf ./\../bone",
    ] {
        assert_denied(command);
    }
}

#[test]
fn denies_a_command_hidden_behind_an_escaped_separator() {
    // A backslash-quoted separator is literal data, so a `#` right after it is
    // still mid-word and does not open a comment. Bash runs the tail as its own
    // command, so the guard must inspect it.
    for command in [
        r"echo a\;# x; rm -rf /home/example",
        r"echo a\)# z; rm -rf /home/example",
        r"echo a\|# q; rm -rf /home/example",
        r"echo a\&# r; rm -rf /home/example",
        r"echo a\(# s; rm -rf /home/example",
        r"echo a\ # y; rm -rf /home/example",
        r"echo a\;# x; rm -rf ~",
        r"sh -c 'echo a\;# x; rm -rf /home/example'",
    ] {
        assert_denied(command);
    }
    // A real comment still hides the tail, because bash never runs it.
    assert_allowed(r"echo a # x; rm -rf /home/example");
}

#[test]
fn command_path_flag_does_not_consume_the_command() {
    assert_denied("command -p dd if=/dev/zero of=/dev/sda");
}

#[test]
fn denies_disk_system_and_nested_operations() {
    for command in [
        "mv ~ /tmp/x",
        "find ~ -delete",
        "find / -delete",
        "dd if=/dev/zero of=~/disk.img",
        "dd if=/dev/zero of=/dev/sda",
        "mkfs.ext4 /dev/sda1",
        "truncate -s 0 ~/.bashrc",
        "echo x > ~/.bashrc",
        "> /etc/passwd",
        "find ~ -print0 | xargs -0 rm -rf",
        "git clean -fdx",
        "chmod -R 777 ~",
        ":(){ :|:& };:",
    ] {
        assert_denied(command);
    }
}

#[test]
fn allows_workspace_scoped_commands() {
    for command in [
        "rm -rf target",
        "rm -rf target/",
        "rm -f src/main.rs",
        "rm -rf /tmp/bone-guard-canary",
        "ls -la ~",
        "cat ~/.bashrc",
        "grep -rn \"rm\" /etc/hosts",
        "git commit -m \"fix rm -rf bug\"",
        "node -e 'const f = x => Object.keys(x); console.log(f({a: 1}))'",
        "cargo build",
        "touch notes.md",
        "mv target/a target/b",
        "chmod +x script.sh",
        "cmd 2>/dev/null",
        "find target -name '*.tmp' -delete",
    ] {
        assert_allowed(command);
    }
}

#[test]
fn denies_guard_self_test_canary() {
    for command in [
        "bone-guard-selftest",
        "sudo bone-guard-selftest",
        "sh -c 'bone-guard-selftest'",
        "true && bone-guard-selftest",
        "bone-guard-selftest --now",
    ] {
        assert_denied(command);
    }
}

#[test]
fn canary_matches_whole_token_only() {
    // A word that merely contains the canary name is a different token, so it is
    // not refused: the canary check is exact, not a substring match.
    assert_allowed("echo bone-guard-selftest-backup");
}

#[test]
fn allows_deletion_outside_workspace_but_not_home_or_ancestors() {
    for command in [
        "rm -rf /home/example/scratch/tree",
        "rm -rf /home/example/projects/bone-bench",
        "rm -r /home/example/scratch/tree",
        "rmdir /home/example/projects/bone-bench/.bone-bench/logs/1789920887083-rg-004-bone",
        "rmdir -p /home/example/scratch/empty",
        "rmdir ~/scratch/empty",
        "rm -d /home/example/scratch/empty",
        "rm --dir /home/example/scratch/empty",
        "bash -c 'rmdir ~/scratch/empty'",
        "cd /home/example/scratch && rm -rf tree",
        "rm -rf /tmp/bone-guard-canary",
    ] {
        assert_allowed(command);
    }
}

#[test]
fn deletion_never_reaches_home_ancestors_or_system_paths() {
    for command in [
        // The home directory itself and everything above it stay refused.
        "rm -rf /home/example",
        "rmdir /home/example/",
        "rmdir ~",
        "rmdir -p /",
        "rmdir /home",
        // An ancestor of the workspace is not scratch.
        "rmdir /home/example/projects",
        // Git metadata is protected.
        "rmdir /home/example/projects/bone/.git/refs",
        // System locations stay refused.
        "rmdir /usr/share/empty",
        "rmdir /var/tmp",
        // Mixed targets and ambiguous paths remain refused.
        "rmdir /home/example/scratch/empty /home/example/projects",
        "rmdir $TARGET",
        "rmdir /home/example/scratch/*",
    ] {
        assert_denied(command);
    }
}

#[test]
fn quoted_verb_words_are_data_not_nested_invocations() {
    // A quoted word is a search pattern, a commit subject, a log line — never a
    // nested program name, so it must not trip the nested-verb heuristic.
    for command in [
        "grep -rn \"rmdir\" --include=*.rs /home/example/projects/bone /home/example/.bone-rust",
        "git commit -m 'rmdir -p the old scratch logs'",
        "echo \"rm -rf ~\"",
    ] {
        assert_allowed(command);
    }
    // A real nested invocation is unquoted and still refused.
    for command in [
        "timeout 5 rm -rf ~",
        "find /home/example -print0 | xargs -0 rmdir",
        "find ~ -exec rm {} +",
    ] {
        assert_denied(command);
    }
}

#[test]
fn unquoted_verb_word_in_an_argument_position_still_denies() {
    // Known, accepted false positive: without command identity the guard cannot
    // tell `grep -rn rm … <home path>` from `timeout 5 rm -rf ~`, because the verb
    // word is followed by a flag and a protected target either way. Only quote
    // provenance is honoured, so this unquoted form stays refused — quote the
    // pattern (`grep -rn \"rm\" …`) and it is allowed.
    assert_denied(
        "grep -rn rm --include=*.rs /home/example/projects/bone /home/example/.bone-rust",
    );
    // The same word *not* followed by a flag is not read as an invocation at all.
    assert_allowed("grep -rn rmdir /home/example/.bone-rust");
}

#[test]
fn hard_deny_call_only_inspects_shell_commands() {
    let roots = roots();
    let shell_call = |arguments: serde_json::Value| ToolCall {
        id: "call".into(),
        name: "shell".into(),
        arguments,
    };

    assert!(hard_deny_call(&shell_call(serde_json::json!({ "command": "rm -rf ~" })), &roots).is_some());
    assert!(hard_deny_call(&shell_call(serde_json::json!({ "action": "run", "command": "rm -rf ~" })), &roots).is_some());
    assert!(hard_deny_call(&shell_call(serde_json::json!({ "action": "kill", "id": "1" })), &roots).is_none());
    assert!(hard_deny_call(&shell_call(serde_json::json!({ "action": "list" })), &roots).is_none());
    assert!(hard_deny_call(&shell_call(serde_json::json!({ "command": "rm -rf target" })), &roots).is_none());

    let edit_call = ToolCall {
        id: "call".into(),
        name: "edit_file".into(),
        arguments: serde_json::json!({ "path": "~/notes.md" }),
    };
    assert!(hard_deny_call(&edit_call, &roots).is_none());
}
