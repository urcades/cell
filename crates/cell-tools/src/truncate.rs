use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
pub const GREP_MAX_LINE_LENGTH: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<String>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

#[derive(Clone, Debug, Default)]
pub struct TruncationOptions {
    pub max_lines: Option<usize>,
    pub max_bytes: Option<usize>,
}

pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let total_bytes = content.len();
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let first_line_bytes = lines.first().map(|line| line.len()).unwrap_or(0);
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some("bytes".to_string()),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines = Vec::new();
    let mut output_bytes = 0;
    let mut truncated_by = "lines".to_string();

    for (index, line) in lines.iter().enumerate() {
        if index >= max_lines {
            truncated_by = "lines".to_string();
            break;
        }
        let line_bytes = line.len() + usize::from(index > 0);
        if output_bytes + line_bytes > max_bytes {
            truncated_by = "bytes".to_string();
            break;
        }
        output_lines.push(*line);
        output_bytes += line_bytes;
    }

    let content = output_lines.join("\n");
    TruncationResult {
        output_lines: output_lines.len(),
        output_bytes: content.len(),
        content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let total_bytes = content.len();
    let lines = content.split('\n').collect::<Vec<_>>();
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        };
    }

    let mut output_lines = Vec::new();
    let mut output_bytes = 0;
    let mut truncated_by = "lines".to_string();
    let mut last_line_partial = false;

    for (index, line) in lines.iter().rev().enumerate() {
        if index >= max_lines {
            truncated_by = "lines".to_string();
            break;
        }
        let line_bytes = line.len() + usize::from(index > 0);
        if output_bytes + line_bytes > max_bytes {
            truncated_by = "bytes".to_string();
            if output_lines.is_empty() {
                let take_from = line.len().saturating_sub(max_bytes);
                output_lines.insert(0, line[take_from..].to_string());
                last_line_partial = true;
            }
            break;
        }
        output_lines.insert(0, (*line).to_string());
        output_bytes += line_bytes;
    }

    let content = output_lines.join("\n");
    TruncationResult {
        output_lines: output_lines.len(),
        output_bytes: content.len(),
        content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    if line.chars().count() <= max_chars {
        (line.to_string(), false)
    } else {
        (
            format!(
                "{}... [truncated]",
                line.chars().take(max_chars).collect::<String>()
            ),
            true,
        )
    }
}
