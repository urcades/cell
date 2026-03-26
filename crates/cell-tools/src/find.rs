use std::path::Path;

use globset::GlobBuilder;
use cell_ai_core::UserContentBlock;
use serde::Deserialize;
use serde_json::{Value, json};
use walkdir::WalkDir;

use crate::path_utils::resolve_to_cwd;
use crate::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, format_size, truncate_head};
use crate::{ToolError, ToolOutput};

const DEFAULT_LIMIT: usize = 1000;

#[derive(Debug, Deserialize)]
pub struct FindToolInput {
    pattern: String,
    path: Option<String>,
    limit: Option<usize>,
}

pub fn definition() -> Value {
    json!({
        "name": "find",
        "description": "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore-like exclusions for .git and node_modules.",
        "parameters": {
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string"},
                "limit": {"type": "number"}
            },
            "required": ["pattern"]
        }
    })
}

pub fn execute(cwd: &Path, input: Value) -> Result<ToolOutput, ToolError> {
    let input: FindToolInput = serde_json::from_value(input)?;
    let root = resolve_to_cwd(input.path.as_deref().unwrap_or("."), cwd);
    if !root.exists() {
        return Err(ToolError::Message(format!(
            "Path not found: {}",
            root.display()
        )));
    }
    let matcher = GlobBuilder::new(&input.pattern)
        .case_insensitive(true)
        .build()
        .map_err(|error| ToolError::Message(error.to_string()))?
        .compile_matcher();
    let effective_limit = input.limit.unwrap_or(DEFAULT_LIMIT);

    let mut matches = Vec::new();
    for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if path == root {
            continue;
        }
        if path.components().any(|component| {
            let value = component.as_os_str().to_string_lossy();
            value == ".git" || value == "node_modules"
        }) {
            continue;
        }
        let relative = path
            .strip_prefix(&root)
            .map_err(|error| ToolError::Message(error.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        if matcher.is_match(&relative) {
            matches.push(relative);
            if matches.len() >= effective_limit {
                break;
            }
        }
    }

    if matches.is_empty() {
        return Ok(ToolOutput {
            content: vec![UserContentBlock::Text {
                text: "No files found matching pattern".to_string(),
                text_signature: None,
            }],
            details: None,
            is_error: false,
        });
    }

    let raw_output = matches.join("\n");
    let truncation = truncate_head(
        &raw_output,
        TruncationOptions {
            max_lines: Some(usize::MAX),
            max_bytes: None,
        },
    );
    let mut output = truncation.content.clone();
    let mut details = json!({});
    let mut notices = Vec::new();

    if matches.len() >= effective_limit {
        notices.push(format!(
            "{effective_limit} results limit reached. Use limit={} for more, or refine pattern",
            effective_limit * 2
        ));
        details["resultLimitReached"] = json!(effective_limit);
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
