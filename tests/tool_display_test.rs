use bone::ui::tool_display::format_bash_label;

#[test]
fn bash_label_splits_top_level_shell_chains() {
    assert_eq!(
        format_bash_label("cd repo && cargo test"),
        "bash cd repo &&\n cargo test"
    );
}

#[test]
fn bash_label_keeps_quoted_operators_intact() {
    assert_eq!(
        format_bash_label("printf \"a && b\" && echo done"),
        "bash printf \"a && b\" &&\n echo done"
    );
}

#[test]
fn bash_label_expands_unquoted_heredoc_delimiter() {
    assert_eq!(
        format_bash_label("cat > /tmp/file << EOFfn main() {}EOF"),
        "bash cat > /tmp/file << EOF\n  fn main()\n  {\n  }\n EOF"
    );
}

#[test]
fn bash_label_expands_quoted_heredoc_delimiter() {
    assert_eq!(
        format_bash_label("cat > /tmp/file << 'EOF'let x = 1;EOF"),
        "bash cat > /tmp/file << 'EOF'\n  let x = 1;\n EOF"
    );
}

#[test]
fn bash_label_handles_collapsed_heredoc_followed_by_command() {
    assert_eq!(
        format_bash_label("cat << 'EOF'let x = 1;EOFBONE_TEST_DIR=/tmp cargo test"),
        "bash cat << 'EOF'\n  let x = 1;\n EOF\n BONE_TEST_DIR=/tmp cargo test"
    );
}

#[test]
fn bash_label_reflows_basic_code_payload() {
    assert_eq!(
        format_bash_label("cat << EOF// hello fn main(){let x = 1;}EOF"),
        "bash cat << EOF\n  // hello fn main()\n  {\n    let x = 1;\n  }\n EOF"
    );
}
