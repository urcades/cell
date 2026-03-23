use std::fs;
use std::path::{Path, PathBuf};

use globset::GlobBuilder;
use pi_rust_ai_core::UserContentBlock;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use walkdir::WalkDir;

use crate::path_utils::resolve_to_cwd;
use crate::truncate::{
    DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH, TruncationOptions, format_size, truncate_head,
    truncate_line,
};
use crate::{ToolError, ToolOutput};

const DEFAULT_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolInput {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    ignore_case: Option<bool>,
    literal: Option<bool>,
    context: Option<usize>,
    limit: Option<usize>,
}

pub fn definition() -> Value {
    json!({
        "name": "grep",
        "description": "Search file contents for a pattern. Returns matching lines with file paths and line numbers.",
        "parameters": {
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string"},
                "glob": {"type": "string"},
                "ignoreCase": {"type": "boolean"},
                "literal": {"type": "boolean"},
                "context": {"type": "number"},
                "limit": {"type": "number"}
            },
            "required": ["pattern"]
        }
    })
}

pub fn execute(cwd: &Path, input: Value) -> Result<ToolOutput, ToolError> {
    let input: GrepToolInput = serde_json::from_value(input)?;
    let root = resolve_to_cwd(input.path.as_deref().unwrap_or("."), cwd);
    let effective_limit = input.limit.unwrap_or(DEFAULT_LIMIT).max(1);
    let context_lines = input.context.unwrap_or(0);
    let regex = build_regex(
        &input.pattern,
        input.literal.unwrap_or(false),
        input.ignore_case.unwrap_or(false),
    )?;
    let glob_matcher = input
        .glob
        .as_deref()
        .map(|glob| {
            GlobBuilder::new(glob)
                .case_insensitive(true)
                .build()
                .map(|glob| glob.compile_matcher())
                .map_err(|error| ToolError::Message(error.to_string()))
        })
        .transpose()?;

    let files = collect_search_files(&root)?;
    let mut blocks = Vec::new();
    let mut matches = 0;
    let mut lines_truncated = false;

    for file in files {
        let relative = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(matcher) = &glob_matcher {
            if !matcher.is_match(&relative) {
                continue;
            }
        }

        let content = match fs::read_to_string(&file) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let lines = content.replace("\r\n", "\n").replace('\r', "\n");
        let lines = lines.split('\n').collect::<Vec<_>>();

        for (line_index, line) in lines.iter().enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            matches += 1;
            let start = line_index.saturating_sub(context_lines);
            let end = std::cmp::min(line_index + context_lines, lines.len().saturating_sub(1));

            for current in start..=end {
                let (truncated_line, was_truncated) =
                    truncate_line(lines[current], GREP_MAX_LINE_LENGTH);
                lines_truncated |= was_truncated;
                if current == line_index {
                    blocks.push(format!("{relative}:{}: {}", current + 1, truncated_line));
                } else {
                    blocks.push(format!("{relative}-{}- {}", current + 1, truncated_line));
                }
            }

            if matches >= effective_limit {
                break;
            }
        }
        if matches >= effective_limit {
            break;
        }
    }

    if blocks.is_empty() {
        return Ok(ToolOutput {
            content: vec![UserContentBlock::Text {
                text: "No matches found".to_string(),
                text_signature: None,
            }],
            details: None,
            is_error: false,
        });
    }

    let raw_output = blocks.join("\n");
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

    if matches >= effective_limit {
        notices.push(format!(
            "{effective_limit} matches limit reached. Use limit={} for more, or refine pattern",
            effective_limit * 2
        ));
        details["matchLimitReached"] = json!(effective_limit);
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        details["truncation"] = json!(truncation);
    }
    if lines_truncated {
        notices.push(format!(
            "Some lines truncated to {} chars. Use read tool to see full lines",
            GREP_MAX_LINE_LENGTH
        ));
        details["linesTruncated"] = json!(true);
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

fn build_regex(pattern: &str, literal: bool, ignore_case: bool) -> Result<Regex, ToolError> {
    let pattern = if literal {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    Regex::new(&if ignore_case {
        format!("(?i){pattern}")
    } else {
        pattern
    })
    .map_err(|error| ToolError::Message(error.to_string()))
}

fn collect_search_files(root: &Path) -> Result<Vec<PathBuf>, ToolError> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if path.components().any(|component| {
            let value = component.as_os_str().to_string_lossy();
            value == ".git" || value == "node_modules"
        }) {
            continue;
        }
        if path.is_file() {
            files.push(path.to_path_buf());
        }
    }
    Ok(files)
}
