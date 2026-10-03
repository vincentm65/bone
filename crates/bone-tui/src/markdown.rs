//! Markdown as data. `parse` splits text into blocks (headings, paragraphs,
//! list items, quotes, code, rules, blank lines) with inline spans marked
//! bold/italic/code/link. Lua decides how they look (`bone.markdown.parse`).

use serde_json::{Value, json};

/// Parse text into blocks. Runs of blank lines collapse to one; blank lines
/// at the start and end are dropped. An unclosed fence (mid-stream) runs to
/// the end.
pub fn parse(text: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut code: Option<(Option<String>, Vec<String>)> = None;
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            match code.take() {
                Some((lang, lines)) => {
                    out.push(json!({ "kind": "code", "lang": lang, "lines": lines }))
                }
                None => {
                    let lang = trimmed[3..].trim();
                    code = Some(((!lang.is_empty()).then(|| lang.to_owned()), Vec::new()));
                }
            }
            continue;
        }
        if let Some((_, lines)) = &mut code {
            lines.push(raw.to_owned());
            continue;
        }
        if trimmed.is_empty() {
            if out.last().is_some_and(|b| b["kind"] != "blank") {
                out.push(json!({ "kind": "blank" }));
            }
            continue;
        }
        let indent = raw.len() - trimmed.len();

        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            out.push(json!({ "kind": "heading", "level": hashes, "spans": inline(trimmed[hashes + 1..].trim()) }));
            continue;
        }
        let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.len() >= 3
            && ['-', '*', '_']
                .iter()
                .any(|&r| compact.chars().all(|c| c == r))
        {
            out.push(json!({ "kind": "rule" }));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('>') {
            out.push(json!({ "kind": "quote", "spans": inline(rest.trim_start()) }));
            continue;
        }
        if let Some((marker, body)) = list_item(trimmed) {
            out.push(json!({
                "kind": "item",
                "indent": indent,
                "marker": marker,
                "ordered": marker.ends_with(['.', ')']),
                "spans": inline(body),
            }));
            continue;
        }
        out.push(json!({ "kind": "paragraph", "indent": indent, "spans": inline(trimmed) }));
    }
    if let Some((lang, lines)) = code {
        out.push(json!({ "kind": "code", "lang": lang, "lines": lines, "open": true }));
    }
    while out.last().is_some_and(|b| b["kind"] == "blank") {
        out.pop();
    }
    out
}

fn list_item(s: &str) -> Option<(&str, &str)> {
    if let Some(rest) = s.strip_prefix(['-', '*', '+']) {
        return rest.strip_prefix(' ').map(|body| (&s[..1], body));
    }
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && digits < 4 {
        let after = &s[digits..];
        if let Some(body) = after
            .strip_prefix(". ")
            .or_else(|| after.strip_prefix(") "))
        {
            return Some((&s[..digits + 1], body));
        }
    }
    None
}

/// Inline markup: `code`, **bold**, *italic* / _italic_, [text](url).
/// Each span is `{ text, bold?, italic?, code?, link? }`.
pub fn inline(text: &str) -> Vec<Value> {
    let c: Vec<char> = text.chars().collect();
    let n = c.len();
    let mut out = Vec::new();
    let mut buf = String::new();
    let (mut bold, mut italic) = (false, false);
    let flush = |buf: &mut String, out: &mut Vec<Value>, bold: bool, italic: bool| {
        if !buf.is_empty() {
            let mut s = json!({ "text": std::mem::take(buf) });
            if bold {
                s["bold"] = json!(true);
            }
            if italic {
                s["italic"] = json!(true);
            }
            out.push(s);
        }
    };
    let find = |from: usize, pat: &[char]| {
        (from..n.saturating_sub(pat.len() - 1)).find(|&j| c[j..j + pat.len()] == *pat)
    };
    let alnum = |i: usize| i < n && c[i].is_alphanumeric();
    let mut i = 0;
    while i < n {
        let ch = c[i];
        if ch == '`'
            && let Some(j) = find(i + 1, &['`'])
        {
            flush(&mut buf, &mut out, bold, italic);
            out.push(json!({ "text": c[i + 1..j].iter().collect::<String>(), "code": true }));
            i = j + 1;
            continue;
        }
        if (ch == '*' || ch == '_') && i + 1 < n && c[i + 1] == ch {
            let word_ok = ch == '*' || (i == 0 || !alnum(i - 1));
            if bold || (word_ok && find(i + 2, &[ch, ch]).is_some()) {
                flush(&mut buf, &mut out, bold, italic);
                bold = !bold;
                i += 2;
                continue;
            }
        }
        if ch == '*' || ch == '_' {
            let closes =
                italic && i > 0 && !c[i - 1].is_whitespace() && (ch == '*' || !alnum(i + 1));
            if closes {
                flush(&mut buf, &mut out, bold, italic);
                italic = false;
                i += 1;
                continue;
            }
            let opens = !italic
                && i + 1 < n
                && !c[i + 1].is_whitespace()
                && (ch == '*' || i == 0 || !alnum(i - 1))
                && (i + 2..n).any(|j| {
                    c[j] == ch && !c[j - 1].is_whitespace() && (ch == '*' || !alnum(j + 1))
                });
            if opens {
                flush(&mut buf, &mut out, bold, italic);
                italic = true;
                i += 1;
                continue;
            }
        }
        if ch == '['
            && let Some(close) = find(i + 1, &[']', '('])
            && let Some(end) = find(close + 2, &[')'])
        {
            flush(&mut buf, &mut out, bold, italic);
            let label: String = c[i + 1..close].iter().collect();
            let url: String = c[close + 2..end].iter().collect();
            out.push(json!({ "text": label, "link": url }));
            i = end + 1;
            continue;
        }
        buf.push(ch);
        i += 1;
    }
    flush(&mut buf, &mut out, bold, italic);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(md: &str) -> Vec<String> {
        parse(md)
            .iter()
            .map(|b| b["kind"].as_str().unwrap().to_owned())
            .collect()
    }

    fn plain(s: &str) -> String {
        inline(s)
            .iter()
            .map(|s| s["text"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn blocks() {
        let md = "\n# Title\n\n\n- one\n  - nested\n1. first\n> quoted\n\n```rust\nfn x() {}\n```\n---\ntext\n\n";
        assert_eq!(
            kinds(md),
            [
                "heading",
                "blank",
                "item",
                "item",
                "item",
                "quote",
                "blank",
                "code",
                "rule",
                "paragraph"
            ]
        );
        let b = parse(md);
        assert_eq!(b[0]["level"], 1);
        assert_eq!(
            (b[3]["indent"].as_u64(), b[3]["marker"].as_str()),
            (Some(2), Some("-"))
        );
        assert_eq!(
            (b[4]["ordered"].as_bool(), b[4]["marker"].as_str()),
            (Some(true), Some("1."))
        );
        assert_eq!(b[7]["lang"], "rust");
        assert_eq!(b[7]["lines"], json!(["fn x() {}"]));
        // An unclosed fence while streaming is still code.
        assert_eq!(parse("```\nlet a")[0]["open"], true);
    }

    #[test]
    fn inline_markup() {
        assert_eq!(
            plain("use `a*b` and **bold** and *it* or _it_"),
            "use a*b and bold and it or it"
        );
        assert_eq!(plain("snake_case_name stays"), "snake_case_name stays");
        assert_eq!(plain("2 * 3 * 4"), "2 * 3 * 4");
        let s = inline("a **b** `c` [d](http://x)");
        assert_eq!(s[1], json!({ "text": "b", "bold": true }));
        assert_eq!(s[3], json!({ "text": "c", "code": true }));
        assert_eq!(s[5], json!({ "text": "d", "link": "http://x" }));
    }
}
