use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use pi_rust_ai_core::UserContentBlock;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::path_utils::resolve_to_cwd;
use crate::truncate::{
    DEFAULT_MAX_BYTES, TruncationOptions, TruncationResult, format_size, truncate_head,
};
use crate::{ToolError, ToolOutput};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolInput {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub fn definition() -> Value {
    json!({
        "name": "read",
        "description": format!(
            "Read the contents of a file. Text reads may be truncated at {}; continuation metadata is returned in details. Use offset/limit for large files.",
            format_size(DEFAULT_MAX_BYTES)
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "number"},
                "limit": {"type": "number"}
            },
            "required": ["path"]
        }
    })
}

pub fn execute(cwd: &Path, input: Value) -> Result<ToolOutput, ToolError> {
    let input: ReadToolInput = serde_json::from_value(input)?;
    let path = resolve_to_cwd(&input.path, cwd);
    let bytes = fs::read(&path)?;

    if let Some(mime_type) = detect_image_mime_type(&path) {
        let base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        return Ok(ToolOutput {
            content: vec![UserContentBlock::Image {
                data: base64,
                mime_type: mime_type.to_string(),
            }],
            details: Some(json!({
                "toolKind": "read",
                "renderKind": "image",
                "path": input.path,
                "mimeType": mime_type,
                "kind": "image",
                "status": "complete",
            })),
            is_error: false,
        });
    }

    let text = String::from_utf8_lossy(&bytes).to_string();
    let all_lines = text.split('\n').collect::<Vec<_>>();
    let total_lines = all_lines.len();
    let start_line = input.offset.unwrap_or(1).saturating_sub(1);
    if start_line >= total_lines {
        return Err(ToolError::Message(format!(
            "Offset {} is beyond end of file ({} lines total)",
            input.offset.unwrap_or(1),
            total_lines
        )));
    }

    let selected_content = if let Some(limit) = input.limit {
        let end_line = std::cmp::min(start_line + limit, total_lines);
        all_lines[start_line..end_line].join("\n")
    } else {
        all_lines[start_line..].join("\n")
    };

    let truncation = truncate_head(&selected_content, TruncationOptions::default());
    let output_text = truncation.content.clone();
    let details = if truncation.truncated {
        let start_line_display = start_line + 1;
        let end_line_display = start_line_display + truncation.output_lines.saturating_sub(1);
        let continuation = if truncation.first_line_exceeds_limit {
            json!({
                "kind": "first-line-too-large",
                "lineNumber": start_line_display,
                "lineSize": format_size(all_lines[start_line].len()),
                "limitBytes": DEFAULT_MAX_BYTES,
                "limitLabel": format_size(DEFAULT_MAX_BYTES),
                "totalLines": total_lines,
                "remainingLines": total_lines.saturating_sub(start_line),
                "nextOffset": start_line_display,
            })
        } else {
            json!({
                "kind": match truncation.truncated_by.as_deref() {
                    Some("lines") => "line-limit",
                    Some("bytes") => "byte-limit",
                    _ => "partial",
                },
                "startLine": start_line_display,
                "endLine": end_line_display,
                "totalLines": total_lines,
                "remainingLines": total_lines.saturating_sub(end_line_display),
                "nextOffset": end_line_display + 1,
                "limitBytes": DEFAULT_MAX_BYTES,
                "limitLabel": format_size(DEFAULT_MAX_BYTES),
            })
        };
        let mut details = json!({
            "toolKind": "read",
            "renderKind": "text",
            "path": input.path,
            "offset": input.offset.unwrap_or(1),
            "limit": input.limit,
            "startLine": start_line_display,
            "endLine": end_line_display,
            "totalLines": total_lines,
            "returnedLines": truncation.output_lines,
            "remainingLines": total_lines.saturating_sub(end_line_display),
            "nextOffset": end_line_display + 1,
            "truncation": truncation,
            "kind": "text",
            "status": "partial",
        });
        if let Some(object) = details.as_object_mut() {
            object.insert("continuation".to_string(), continuation);
        }
        details
    } else if let Some(limit) = input.limit {
        let end_line = std::cmp::min(start_line + limit, total_lines);
        let mut details = json!({
            "toolKind": "read",
            "renderKind": "text",
            "path": input.path,
            "offset": input.offset.unwrap_or(1),
            "limit": input.limit,
            "startLine": start_line + 1,
            "endLine": end_line,
            "totalLines": total_lines,
            "returnedLines": end_line.saturating_sub(start_line),
            "remainingLines": total_lines.saturating_sub(end_line),
            "nextOffset": end_line + 1,
            "kind": "text",
            "status": if end_line < total_lines { "partial" } else { "complete" },
        });
        if end_line < total_lines {
            let continuation = json!({
                "kind": "requested-limit",
                "startLine": start_line + 1,
                "endLine": end_line,
                "totalLines": total_lines,
                "remainingLines": total_lines - end_line,
                "nextOffset": end_line + 1,
                "limitLines": limit,
            });
            if let Some(object) = details.as_object_mut() {
                object.insert("continuation".to_string(), continuation);
            }
        }
        details
    } else {
        json!({
            "toolKind": "read",
            "renderKind": "text",
            "path": input.path,
            "offset": input.offset.unwrap_or(1),
            "limit": input.limit,
            "startLine": start_line + 1,
            "endLine": total_lines,
            "totalLines": total_lines,
            "returnedLines": total_lines.saturating_sub(start_line),
            "remainingLines": 0usize,
            "kind": "text",
            "status": "complete",
        })
    };

    Ok(ToolOutput {
        content: vec![UserContentBlock::Text {
            text: output_text,
            text_signature: None,
        }],
        details: Some(details),
        is_error: false,
    })
}

fn detect_image_mime_type(path: &PathBuf) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|value| value.to_lowercase())
    {
        Some(extension) if extension == "png" => Some("image/png"),
        Some(extension) if extension == "jpg" || extension == "jpeg" => Some("image/jpeg"),
        Some(extension) if extension == "gif" => Some("image/gif"),
        Some(extension) if extension == "webp" => Some("image/webp"),
        _ => None,
    }
}

#[allow(dead_code)]
fn _details_value(truncation: TruncationResult) -> Value {
    json!({ "truncation": truncation })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::execute;

    fn text_output_text(output: &crate::ToolOutput) -> String {
        match &output.content[0] {
            pi_rust_ai_core::UserContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text output"),
        }
    }

    #[test]
    fn read_tool_uses_bash_hint_when_first_line_is_too_large() {
        let tempdir = tempdir().expect("tempdir");
        let file_path = tempdir.path().join("big.txt");
        fs::write(&file_path, "x".repeat(60_000)).expect("write file");

        let output = execute(tempdir.path(), json!({ "path": "big.txt" })).expect("read");
        let text = text_output_text(&output);

        assert!(text.is_empty());
        assert!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("truncation"))
                .and_then(|truncation| truncation.get("firstLineExceedsLimit"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("continuation"))
                .and_then(|continuation| continuation.get("kind"))
                .and_then(Value::as_str),
            Some("first-line-too-large")
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("continuation"))
                .and_then(|continuation| continuation.get("nextOffset"))
                .and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("continuation"))
                .and_then(|continuation| continuation.get("limitLabel"))
                .and_then(Value::as_str),
            Some("50.0KB")
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("kind"))
                .and_then(Value::as_str),
            Some("text")
        );
    }

    #[test]
    fn read_tool_reports_byte_truncation_with_offset_hint() {
        let tempdir = tempdir().expect("tempdir");
        let file_path = tempdir.path().join("wide.txt");
        let mut content = String::new();
        for index in 0..700 {
            content.push_str(&format!("{index:04} {}\n", "x".repeat(80)));
        }
        fs::write(&file_path, content).expect("write file");

        let output = execute(tempdir.path(), json!({ "path": "wide.txt" })).expect("read");
        let text = text_output_text(&output);

        assert!(text.contains("0000 "));
        assert!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("truncation"))
                .is_some()
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("continuation"))
                .and_then(|continuation| continuation.get("kind"))
                .and_then(Value::as_str),
            Some("byte-limit")
        );
        assert!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("continuation"))
                .and_then(|continuation| continuation.get("nextOffset"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 1
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("continuation"))
                .and_then(|continuation| continuation.get("limitLabel"))
                .and_then(Value::as_str),
            Some("50.0KB")
        );
    }

    #[test]
    fn read_tool_returns_image_payload_without_text_label() {
        let tempdir = tempdir().expect("tempdir");
        let file_path = tempdir.path().join("preview.png");
        fs::write(&file_path, b"not really a png but mime is extension-based").expect("write file");

        let output = execute(tempdir.path(), json!({ "path": "preview.png" })).expect("read");

        assert!(matches!(
            &output.content[0],
            pi_rust_ai_core::UserContentBlock::Image { .. }
        ));
        assert_eq!(output.content.len(), 1);
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("mimeType"))
                .and_then(Value::as_str),
            Some("image/png")
        );
    }
}
