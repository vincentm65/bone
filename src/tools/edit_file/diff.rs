use similar::TextDiff;

pub(crate) fn summarize_change(old: &str, new: &str) -> String {
    let (_, insertions, deletions) = build_numbered_diff_lines(old, new, 3);
    format!("+{insertions}, -{deletions}")
}

pub(crate) fn build_unified_diff(tool_name: &str, path: &str, old: &str, new: &str) -> String {
    let (lines, insertions, deletions) = build_numbered_diff_lines(old, new, 3);
    let header = format!("    {tool_name} {path} (-{deletions} | +{insertions})");

    if lines.is_empty() {
        return format!("\n{header}\n(no changes)");
    }

    let mut output = format!("\n{header}\n");
    output.push_str(&lines.join("\n"));
    output
}

pub(super) fn build_numbered_diff_lines(
    old: &str,
    new: &str,
    context_radius: usize,
) -> (Vec<String>, usize, usize) {
    if old == new {
        return (Vec::new(), 0, 0);
    }

    let diff = TextDiff::from_lines(old, new);
    let unified = diff
        .unified_diff()
        .context_radius(context_radius)
        .to_string();
    let mut old_line: Option<usize> = None;
    let mut new_line: Option<usize> = None;
    let mut insertions = 0;
    let mut deletions = 0;
    let mut lines = Vec::new();

    for raw_line in unified.lines() {
        if raw_line.starts_with("--- ") || raw_line.starts_with("+++ ") {
            continue;
        }

        if raw_line.starts_with("@@ ") {
            if let Some((old_start, new_start)) = parse_hunk_header(raw_line) {
                old_line = Some(old_start);
                new_line = Some(new_start);
            }
            continue;
        }

        let (Some(current_old), Some(current_new)) = (old_line, new_line) else {
            continue;
        };

        let Some(sign) = raw_line.chars().next() else {
            continue;
        };
        let text = raw_line.get(1..).unwrap_or("");

        match sign {
            ' ' => {
                lines.push(format!("{current_old:>5}   {text}"));
                old_line = Some(current_old + 1);
                new_line = Some(current_new + 1);
            }
            '-' => {
                lines.push(format!("{current_old:>5} - {text}"));
                deletions += 1;
                old_line = Some(current_old + 1);
            }
            '+' => {
                lines.push(format!("{current_new:>5} + {text}"));
                insertions += 1;
                new_line = Some(current_new + 1);
            }
            _ => {}
        }
    }

    (lines, insertions, deletions)
}

pub(super) fn parse_hunk_header(header: &str) -> Option<(usize, usize)> {
    let mut parts = header.split_whitespace();
    parts.next()?; // @@
    let old_part = parts.next()?;
    let new_part = parts.next()?;
    Some((parse_hunk_start(old_part)?, parse_hunk_start(new_part)?))
}

pub(super) fn parse_hunk_start(part: &str) -> Option<usize> {
    let range = part.strip_prefix(['-', '+'])?;
    let start = range.split(',').next()?;
    start.parse().ok()
}
