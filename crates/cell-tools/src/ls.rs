use std::fs;
use std::path::Path;

use cell_ai_core::UserContentBlock;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::path_utils::resolve_to_cwd;
use crate::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, format_size, truncate_head};
use crate::{ToolError, ToolOutput};

const DEFAULT_LIMIT: usize = 500;

#[derive(Debug, Deserialize)]
pub struct LsToolInput {
    path: Option<String>,
    limit: Option<usize>,
}

pub fn definition() -> Value {
    json!({
        "name": "ls",
        "description": "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles.",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "limit": {"type": "number"}
            }
        }
    })
}

pub fn execute(cwd: &Path, input: Value) -> Result<ToolOutput, ToolError> {
    let input: LsToolInput = serde_json::from_value(input)?;
    let path = resolve_to_cwd(input.path.as_deref().unwrap_or("."), cwd);
    let metadata = fs::metadata(&path)?;
    if !metadata.is_dir() {
        return Err(ToolError::Message(format!(
            "Not a directory: {}",
            path.display()
        )));
    }

    let effective_limit = input.limit.unwrap_or(DEFAULT_LIMIT);
    let mut entries = fs::read_dir(&path)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.file_name().cmp(&right.file_name()));

    let mut output_entries = Vec::new();
    let mut entry_limit_reached = false;
    for entry in entries {
        if output_entries.len() >= effective_limit {
            entry_limit_reached = true;
            break;
        }
        let file_name = entry
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ToolError::Message("Invalid directory entry".to_string()))?;
        let suffix = if entry.is_dir() { "/" } else { "" };
        output_entries.push(format!("{file_name}{suffix}"));
    }

    if output_entries.is_empty() {
        return Ok(ToolOutput {
            content: vec![UserContentBlock::Text {
                text: "(empty directory)".to_string(),
                text_signature: None,
            }],
            details: None,
            is_error: false,
        });
    }

    let raw_output = output_entries.join("\n");
    let truncation = truncate_head(
        &raw_output,
        TruncationOptions {
            max_lines: Some(usize::MAX),
            max_bytes: None,
        },
    );
    let mut output = truncation.content.clone();
    let mut notices = Vec::new();
    let mut details = json!({});

    if entry_limit_reached {
        notices.push(format!(
            "{effective_limit} entries limit reached. Use limit={} for more",
            effective_limit * 2
        ));
        details["entryLimitReached"] = json!(effective_limit);
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        details["truncation"] = json!(truncation);
    }
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }

    Ok(ToolOutput {
        content: vec![UserContentBlock::Text {
            text: output,
            text_signature: None,
        }],
        details: if details == json!({}) {
            None
        } else {
            Some(details)
        },
        is_error: false,
    })
}
