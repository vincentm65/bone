//! Exact text replacements, validated together against the original file before writing.
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolResult, ToolSpec, typed_args};

pub struct EditFile(ToolSpec);

impl EditFile {
    pub fn new() -> Self {
        EditFile(ToolSpec {
            name: "edit_file".into(),
            description:
                "Edit one file using exact text replacements. Batch separate changes in one \
                edits array. Each old_string must match the original file exactly (including \
                whitespace) and be unique unless replace_all is true. Edits must not overlap; \
                merge nearby changes into one edit. All edits are validated before writing; \
                on any failure nothing is written. Read the file first."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path, absolute or relative to the working directory."},
                    "edits": {
                        "type": "array", "minItems": 1,
                        "description": "Independent replacements matched against the original file, not after earlier edits. Batch separate changes here.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": {"type": "string", "description": "Exact original text to replace; include enough context to be unique."},
                                "new_string": {"type": "string", "description": "Replacement text."},
                                "replace_all": {"type": "boolean", "description": "Replace every occurrence of this old_string. Default false."}
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    edits: Option<Vec<Replacement>>,
    // Accept the historical single-edit shape, but advertise the batch shape.
    old_string: Option<String>,
    new_string: Option<String>,
    replace_all: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

impl Args {
    fn replacements(self) -> Result<(String, Vec<Replacement>, bool), String> {
        if let Some(edits) = self.edits {
            if self.old_string.is_some() || self.new_string.is_some() || self.replace_all.is_some()
            {
                return Err("use edits or the legacy single replacement, not both".into());
            }
            if edits.is_empty() {
                return Err("edits must contain at least one replacement".into());
            }
            return Ok((self.path, edits, false));
        }
        match (self.old_string, self.new_string) {
            (Some(old_string), Some(new_string)) => Ok((
                self.path,
                vec![Replacement {
                    old_string,
                    new_string,
                    replace_all: self.replace_all.unwrap_or(false),
                }],
                true,
            )),
            _ => Err(
                "edit_file expects {path, edits: [{old_string, new_string, replace_all?}]}".into(),
            ),
        }
    }
}

/// Resolve all matches against the original bytes; reject cross-edit overlaps.
/// Returning a new string before the sole file write makes validation atomic.
fn apply(text: &str, edits: &[Replacement], display: &str) -> Result<(String, usize), String> {
    if edits.is_empty() {
        return Err("edits must contain at least one replacement".into());
    }
    let mut spans = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        if edit.old_string.is_empty() {
            return Err(format!(
                "edit {}: old_string is empty; use write_file to create files",
                index + 1
            ));
        }
        if edit.old_string == edit.new_string {
            return Err(format!(
                "edit {}: old_string and new_string are identical",
                index + 1
            ));
        }
        let matches: Vec<_> = text
            .char_indices()
            .filter(|&(start, _)| text[start..].starts_with(&edit.old_string))
            .map(|(start, _)| start)
            .collect();
        match matches.len() {
            0 => {
                return Err(format!(
                    "edit {}: old_string not found in {display}. Re-read the file and copy the text exactly.",
                    index + 1
                ));
            }
            n if n > 1 && !edit.replace_all => {
                return Err(format!(
                    "edit {}: old_string matches {n} places in {display}. Include more surrounding context to make it unique, or set replace_all.",
                    index + 1
                ));
            }
            _ => {}
        }
        for start in matches {
            spans.push((start, start + edit.old_string.len(), index));
        }
    }
    spans.sort_unstable();
    for pair in spans.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err(format!(
                "edits {} and {} overlap in {display}; merge them into one edit. No changes written.",
                pair[0].2 + 1,
                pair[1].2 + 1
            ));
        }
    }
    let mut updated = String::new();
    let mut cursor = 0;
    for &(start, end, index) in &spans {
        updated.push_str(&text[cursor..start]);
        updated.push_str(&edits[index].new_string);
        cursor = end;
    }
    updated.push_str(&text[cursor..]);
    Ok((updated, spans.len()))
}

impl Tool for EditFile {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: Args = typed_args(args)?;
            let (path_arg, edits, legacy) = args.replacements()?;
            let path = ctx.resolve(&path_arg);
            let text = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let (updated, count) = apply(&text, &edits, &path.display().to_string())?;
            tokio::fs::write(&path, updated)
                .await
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            let s = if count == 1 { "" } else { "s" };
            if legacy {
                Ok(format!(
                    "Replaced {count} occurrence{s} in {}",
                    path.display()
                ))
            } else {
                Ok(format!(
                    "Replaced {count} occurrence{s} in {} ({} edits)",
                    path.display(),
                    edits.len()
                ))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn edit(old: &str, new: &str, all: bool) -> Replacement {
        Replacement {
            old_string: old.into(),
            new_string: new.into(),
            replace_all: all,
        }
    }
    #[test]
    fn disjoint_original_matching_and_order() {
        let edits = [edit("cc", "a", false), edit("aa", "cc", false)];
        assert_eq!(
            apply("aa bb cc\n", &edits, "x").unwrap(),
            ("cc bb a\n".into(), 2)
        );
    }
    #[test]
    fn adjacent_unicode_crlf_and_deletion() {
        assert_eq!(
            apply(
                "é🙂\r\nlast",
                &[edit("é", "", false), edit("🙂", "λ", false)],
                "x"
            )
            .unwrap()
            .0,
            "λ\r\nlast"
        );
    }
    #[test]
    fn repeated_matches_need_replace_all() {
        assert!(
            apply("a a b", &[edit("a", "x", false)], "x")
                .unwrap_err()
                .contains("matches 2 places")
        );
        assert_eq!(
            apply(
                "a a b",
                &[edit("a", "long", true), edit("b", "", false)],
                "x"
            )
            .unwrap(),
            ("long long ".into(), 3)
        );
        assert!(
            apply("aaa", &[edit("aa", "x", false)], "x")
                .unwrap_err()
                .contains("matches 2 places")
        );
        assert!(
            apply("aaa", &[edit("aa", "x", true)], "x")
                .unwrap_err()
                .contains("overlap")
        );
    }
    #[test]
    fn reject_overlap_nested_and_duplicate() {
        for edits in [
            vec![edit("abc", "X", false), edit("bcd", "Y", false)],
            vec![edit("abcd", "X", false), edit("bc", "Y", false)],
            vec![edit("abc", "X", false), edit("abc", "Y", false)],
        ] {
            assert!(apply("abcd", &edits, "x").unwrap_err().contains("overlap"));
        }
        assert!(
            apply("a a", &[edit("a", "x", true), edit("a a", "y", false)], "x")
                .unwrap_err()
                .contains("overlap")
        );
    }
    #[test]
    fn no_sequential_matching() {
        assert!(
            apply("a", &[edit("a", "b", false), edit("b", "c", false)], "x")
                .unwrap_err()
                .contains("not found")
        );
    }
    #[test]
    fn reject_empty_missing_and_noop() {
        assert!(apply("a", &[], "x").is_err());
        for edits in [
            vec![edit("", "b", false)],
            vec![edit("a", "a", false)],
            vec![edit("a", "b", false), edit("missing", "c", false)],
        ] {
            assert!(apply("a", &edits, "x").is_err());
        }
    }
    #[test]
    fn argument_shapes() {
        for value in [
            json!({"path":"x", "edits": []}),
            json!({"path":"x", "edits":[{"old_string":"a", "new_string":"b"}], "old_string":"a", "new_string":"b"}),
            json!({"path":"x", "old_string":"a"}),
        ] {
            assert!(
                serde_json::from_value::<Args>(value)
                    .unwrap()
                    .replacements()
                    .is_err()
            );
        }
        assert!(
            serde_json::from_value::<Args>(json!({"path":"x", "edits":[{"old_string":"a"}]}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<Args>(
                json!({"path":"x", "edits":[{"old_string":"a", "new_string":"b", "typo":true}]})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<Args>(json!({"path":"x", "old_string":"a", "new_string":"b"}))
                .unwrap()
                .replacements()
                .unwrap()
                .2
        );
    }
}
