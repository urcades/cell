use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::path_utils::resolve_to_cwd;
use crate::{ToolError, ToolOutput};

#[derive(Debug, Deserialize)]
pub struct WriteToolInput {
    path: String,
    content: String,
}

pub fn definition() -> Value {
    json!({
        "name": "write",
        "description": "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"]
        }
    })
}

pub fn execute(cwd: &Path, input: Value) -> Result<ToolOutput, ToolError> {
    let input: WriteToolInput = serde_json::from_value(input)?;
    let path = resolve_to_cwd(&input.path, cwd);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &input.content)?;
    Ok(ToolOutput {
        content: vec![cell_ai_core::UserContentBlock::Text {
            text: String::new(),
            text_signature: None,
        }],
        details: Some(json!({
            "path": input.path,
            "status": "written",
            "bytesWritten": input.content.len(),
            "lineCount": input.content.lines().count(),
        })),
        is_error: false,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::execute;

    #[test]
    fn write_tool_returns_compact_structured_payload() {
        let tempdir = tempdir().expect("tempdir");
        let output = execute(
            tempdir.path(),
            serde_json::json!({
                "path": "dir/file.txt",
                "content": "hello"
            }),
        )
        .expect("write");

        assert_eq!(output.content.len(), 1);
        assert!(matches!(
            &output.content[0],
            cell_ai_core::UserContentBlock::Text { text, .. } if text.is_empty()
        ));
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("path"))
                .and_then(|value| value.as_str()),
            Some("dir/file.txt")
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("bytesWritten"))
                .and_then(|value| value.as_u64()),
            Some(5)
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("status"))
                .and_then(|value| value.as_str()),
            Some("written")
        );
    }
}
