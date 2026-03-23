use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use crate::path_utils::resolve_to_cwd;
use crate::{ToolError, ToolOutput};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolInput {
    path: String,
    old_text: String,
    new_text: String,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum DiffRowKind {
    Context,
    Remove,
    Add,
    Ellipsis,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiffRow {
    kind: DiffRowKind,
    old_line_number: Option<usize>,
    new_line_number: Option<usize>,
    content: String,
}

pub fn definition() -> Value {
    json!({
        "name": "edit",
        "description": "Edit a file by replacing exact text. The oldText must match exactly. Use this for precise, surgical edits.",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "oldText": {"type": "string"},
                "newText": {"type": "string"}
            },
            "required": ["path", "oldText", "newText"]
        }
    })
}

pub fn execute(cwd: &Path, input: Value) -> Result<ToolOutput, ToolError> {
    let input: EditToolInput = serde_json::from_value(input)?;
    let path = resolve_to_cwd(&input.path, cwd);
    let original = fs::read_to_string(&path)
        .map_err(|_| ToolError::Message(format!("File not found: {}", input.path)))?;

    let occurrences = original.matches(&input.old_text).count();
    if occurrences == 0 {
        return Err(ToolError::Message(format!(
            "Could not find the exact text in {}. The old text must match exactly including all whitespace and newlines.",
            input.path
        )));
    }
    if occurrences > 1 {
        return Err(ToolError::Message(format!(
            "Found {occurrences} occurrences of the text in {}. The text must be unique. Please provide more context to make it unique.",
            input.path
        )));
    }

    let updated = original.replacen(&input.old_text, &input.new_text, 1);
    if updated == original {
        return Err(ToolError::Message(format!(
            "No changes would be made to {}. The replacement produces identical content.",
            input.path
        )));
    }

    fs::write(&path, updated.as_bytes())?;
    let (diff, first_changed_line, diff_rows) = generate_diff_string(&original, &updated, 4);
    let mut details = serde_json::Map::new();
    details.insert("path".to_string(), json!(input.path));
    details.insert("diff".to_string(), json!(diff));
    details.insert("diffFormat".to_string(), json!("compact-numbered"));
    details.insert("diffContextLines".to_string(), json!(4));
    details.insert("diffLineCount".to_string(), json!(diff_rows.len()));
    details.insert("diffRows".to_string(), json!(diff_rows));
    details.insert("oldTextBytes".to_string(), json!(input.old_text.len()));
    details.insert("newTextBytes".to_string(), json!(input.new_text.len()));
    details.insert("status".to_string(), json!("edited"));
    if let Some(first_changed_line) = first_changed_line {
        details.insert("firstChangedLine".to_string(), json!(first_changed_line));
    }

    Ok(ToolOutput {
        content: vec![pi_rust_ai_core::UserContentBlock::Text {
            text: String::new(),
            text_signature: None,
        }],
        details: Some(Value::Object(details)),
        is_error: false,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffSegmentTag {
    Equal,
    Delete,
    Insert,
}

struct DiffSegment {
    tag: DiffSegmentTag,
    lines: Vec<String>,
}

fn generate_diff_string(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> (String, Option<usize>, Vec<DiffRow>) {
    let diff = TextDiff::from_lines(old_content, new_content);
    let mut segments = Vec::<DiffSegment>::new();

    for change in diff.iter_all_changes() {
        let tag = match change.tag() {
            ChangeTag::Equal => DiffSegmentTag::Equal,
            ChangeTag::Delete => DiffSegmentTag::Delete,
            ChangeTag::Insert => DiffSegmentTag::Insert,
        };
        let line = change
            .to_string_lossy()
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string();

        match segments.last_mut() {
            Some(last) if last.tag == tag => last.lines.push(line),
            _ => segments.push(DiffSegment {
                tag,
                lines: vec![line],
            }),
        }
    }

    let old_lines = old_content.split('\n').collect::<Vec<_>>();
    let new_lines = new_content.split('\n').collect::<Vec<_>>();
    let line_num_width = old_lines.len().max(new_lines.len()).to_string().len();

    let mut output = Vec::new();
    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line = None;
    let mut rows = Vec::new();

    for index in 0..segments.len() {
        let segment = &segments[index];
        let next_is_change =
            index + 1 < segments.len() && segments[index + 1].tag != DiffSegmentTag::Equal;

        match segment.tag {
            DiffSegmentTag::Delete => {
                if first_changed_line.is_none() {
                    first_changed_line = Some(new_line_num);
                }
                for line in &segment.lines {
                    let line_num = format!("{:>width$}", old_line_num, width = line_num_width);
                    output.push(format!("-{line_num} {line}"));
                    rows.push(DiffRow {
                        kind: DiffRowKind::Remove,
                        old_line_number: Some(old_line_num),
                        new_line_number: None,
                        content: line.clone(),
                    });
                    old_line_num += 1;
                }
                last_was_change = true;
            }
            DiffSegmentTag::Insert => {
                if first_changed_line.is_none() {
                    first_changed_line = Some(new_line_num);
                }
                for line in &segment.lines {
                    let line_num = format!("{:>width$}", new_line_num, width = line_num_width);
                    output.push(format!("+{line_num} {line}"));
                    rows.push(DiffRow {
                        kind: DiffRowKind::Add,
                        old_line_number: None,
                        new_line_number: Some(new_line_num),
                        content: line.clone(),
                    });
                    new_line_num += 1;
                }
                last_was_change = true;
            }
            DiffSegmentTag::Equal => {
                if last_was_change || next_is_change {
                    let mut lines_to_show = segment.lines.as_slice();
                    let mut skip_start = 0usize;
                    let mut skip_end = 0usize;

                    if !last_was_change {
                        skip_start = segment.lines.len().saturating_sub(context_lines);
                        lines_to_show = &lines_to_show[skip_start..];
                    }

                    if !next_is_change && lines_to_show.len() > context_lines {
                        skip_end = lines_to_show.len() - context_lines;
                        lines_to_show = &lines_to_show[..context_lines];
                    }

                    if skip_start > 0 {
                        output.push(format!(" {} ...", " ".repeat(line_num_width)));
                        rows.push(DiffRow {
                            kind: DiffRowKind::Ellipsis,
                            old_line_number: None,
                            new_line_number: None,
                            content: "...".to_string(),
                        });
                        old_line_num += skip_start;
                        new_line_num += skip_start;
                    }

                    for line in lines_to_show {
                        let line_num = format!("{:>width$}", old_line_num, width = line_num_width);
                        output.push(format!(" {line_num} {line}"));
                        rows.push(DiffRow {
                            kind: DiffRowKind::Context,
                            old_line_number: Some(old_line_num),
                            new_line_number: Some(new_line_num),
                            content: line.to_string(),
                        });
                        old_line_num += 1;
                        new_line_num += 1;
                    }

                    if skip_end > 0 {
                        output.push(format!(" {} ...", " ".repeat(line_num_width)));
                        rows.push(DiffRow {
                            kind: DiffRowKind::Ellipsis,
                            old_line_number: None,
                            new_line_number: None,
                            content: "...".to_string(),
                        });
                        old_line_num += skip_end;
                        new_line_num += skip_end;
                    }
                } else {
                    old_line_num += segment.lines.len();
                    new_line_num += segment.lines.len();
                }
                last_was_change = false;
            }
        }
    }

    (output.join("\n"), first_changed_line, rows)
}

#[cfg(test)]
mod tests {
    use super::{DiffRowKind, generate_diff_string};
    use std::fs;
    use tempfile::tempdir;

    use super::execute;

    #[test]
    fn generate_diff_string_uses_compact_numbered_surface() {
        let (diff, first_changed_line, rows) =
            generate_diff_string("alpha\nbeta\ngamma", "alpha\nbravo\ngamma", 4);

        assert_eq!(first_changed_line, Some(2));
        assert_eq!(diff, " 1 alpha\n-2 beta\n+2 bravo\n 3 gamma");
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0].kind, DiffRowKind::Context));
        assert!(matches!(rows[1].kind, DiffRowKind::Remove));
        assert!(matches!(rows[2].kind, DiffRowKind::Add));
        assert!(matches!(rows[3].kind, DiffRowKind::Context));
    }

    #[test]
    fn edit_tool_returns_compact_structured_payload() {
        let tempdir = tempdir().expect("tempdir");
        let file_path = tempdir.path().join("sample.txt");
        fs::write(&file_path, "hello world").expect("write file");

        let output = execute(
            tempdir.path(),
            serde_json::json!({
                "path": "sample.txt",
                "oldText": "world",
                "newText": "pi"
            }),
        )
        .expect("edit");

        assert_eq!(output.content.len(), 1);
        assert!(matches!(
            &output.content[0],
            pi_rust_ai_core::UserContentBlock::Text { text, .. } if text.is_empty()
        ));
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("path"))
                .and_then(|value| value.as_str()),
            Some("sample.txt")
        );
        assert!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("diff"))
                .and_then(|value| value.as_str())
                .is_some()
        );
        assert!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("diffRows"))
                .and_then(|value| value.as_array())
                .is_some()
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("firstChangedLine"))
                .and_then(|value| value.as_u64()),
            Some(1)
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("status"))
                .and_then(|value| value.as_str()),
            Some("edited")
        );
    }
}
