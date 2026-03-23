mod bash;
mod edit;
mod find;
mod grep;
mod ls;
mod path_utils;
mod read;
mod truncate;
mod write;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pi_rust_ai_core::{ToolDefinition, ToolResultMessage, UserContentBlock};
use serde_json::Value;
use thiserror::Error;

pub use bash::BashExecutionResult;
pub use truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, GREP_MAX_LINE_LENGTH, TruncationOptions,
    TruncationResult, format_size, truncate_head, truncate_line, truncate_tail,
};

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{0}")]
    Message(String),
    #[error("{0}")]
    Detailed(String, Option<Value>),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
pub struct ToolOutput {
    pub content: Vec<UserContentBlock>,
    pub details: Option<Value>,
    pub is_error: bool,
}

pub const BUILTIN_TOOLS: &[&str] = &["read", "bash", "edit", "write", "grep", "find", "ls"];

#[derive(Clone, Debug)]
pub struct ToolSet {
    cwd: PathBuf,
    enabled: BTreeMap<String, bool>,
}

impl ToolSet {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        let enabled = BUILTIN_TOOLS
            .iter()
            .map(|name| (name.to_string(), true))
            .collect::<BTreeMap<_, _>>();
        Self {
            cwd: cwd.into(),
            enabled,
        }
    }

    pub fn with_enabled_names(cwd: impl Into<PathBuf>, enabled_names: &[String]) -> Self {
        let mut enabled = BTreeMap::new();
        for name in BUILTIN_TOOLS {
            enabled.insert(
                (*name).to_string(),
                enabled_names.iter().any(|enabled| enabled == name),
            );
        }
        Self {
            cwd: cwd.into(),
            enabled,
        }
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        BUILTIN_TOOLS
            .iter()
            .filter(|name| self.enabled.get(**name).copied().unwrap_or(false))
            .map(|name| {
                let raw = definition_for(name).expect("tool definition");
                ToolDefinition {
                    name: raw["name"].as_str().unwrap_or_default().to_string(),
                    description: raw["description"].as_str().unwrap_or_default().to_string(),
                    parameters: raw["parameters"].clone(),
                }
            })
            .collect()
    }

    pub fn execute(&self, tool_call_id: &str, tool_name: &str, input: Value) -> ToolResultMessage {
        match self.execute_raw(tool_name, input) {
            Ok(output) => ToolResultMessage {
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                content: output.content,
                details: output.details,
                is_error: output.is_error,
                timestamp: 0,
            },
            Err(ToolError::Detailed(error, details)) => ToolResultMessage {
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                content: vec![UserContentBlock::Text {
                    text: error,
                    text_signature: None,
                }],
                details,
                is_error: true,
                timestamp: 0,
            },
            Err(error) => ToolResultMessage {
                tool_call_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                content: vec![UserContentBlock::Text {
                    text: error.to_string(),
                    text_signature: None,
                }],
                details: None,
                is_error: true,
                timestamp: 0,
            },
        }
    }

    pub fn execute_raw(&self, tool_name: &str, input: Value) -> Result<ToolOutput, ToolError> {
        if !self.enabled.get(tool_name).copied().unwrap_or(false) {
            return Err(ToolError::Message(format!(
                "Tool \"{tool_name}\" is not enabled."
            )));
        }

        match tool_name {
            "read" => read::execute(&self.cwd, input),
            "write" => write::execute(&self.cwd, input),
            "edit" => edit::execute(&self.cwd, input),
            "bash" => bash::execute(&self.cwd, input),
            "grep" => grep::execute(&self.cwd, input),
            "find" => find::execute(&self.cwd, input),
            "ls" => ls::execute(&self.cwd, input),
            _ => Err(ToolError::Message(format!("Unknown tool: {tool_name}"))),
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn execute_bash_direct(&self, command: &str) -> Result<BashExecutionResult, ToolError> {
        bash::execute_direct(&self.cwd, command)
    }
}

pub fn definition_for(name: &str) -> Option<Value> {
    match name {
        "read" => Some(read::definition()),
        "write" => Some(write::definition()),
        "edit" => Some(edit::definition()),
        "bash" => Some(bash::definition()),
        "grep" => Some(grep::definition()),
        "find" => Some(find::definition()),
        "ls" => Some(ls::definition()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::ToolSet;

    #[test]
    fn read_tool_honors_offset_and_limit_notices() {
        let tempdir = tempdir().expect("tempdir");
        let file_path = tempdir.path().join("example.txt");
        fs::write(&file_path, "a\nb\nc\nd").expect("write file");

        let tools = ToolSet::new(tempdir.path());
        let output = tools
            .execute_raw(
                "read",
                json!({ "path": "example.txt", "offset": 2, "limit": 2 }),
            )
            .expect("read");
        let text = match &output.content[0] {
            pi_rust_ai_core::UserContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text output"),
        };
        assert!(text.contains("b\nc"));
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("continuation"))
                .and_then(|continuation| continuation.get("kind"))
                .and_then(Value::as_str),
            Some("requested-limit")
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("continuation"))
                .and_then(|continuation| continuation.get("nextOffset"))
                .and_then(Value::as_u64),
            Some(4)
        );
    }

    #[test]
    fn write_and_edit_tools_modify_files() {
        let tempdir = tempdir().expect("tempdir");
        let tools = ToolSet::new(tempdir.path());
        tools
            .execute_raw(
                "write",
                json!({ "path": "file.txt", "content": "before text" }),
            )
            .expect("write");
        tools
            .execute_raw(
                "edit",
                json!({ "path": "file.txt", "oldText": "before", "newText": "after" }),
            )
            .expect("edit");
        let content = fs::read_to_string(tempdir.path().join("file.txt")).expect("read file");
        assert_eq!(content, "after text");
    }

    #[test]
    fn find_and_grep_tools_return_matches() {
        let tempdir = tempdir().expect("tempdir");
        fs::create_dir_all(tempdir.path().join("src")).expect("mkdir");
        fs::write(
            tempdir.path().join("src/app.ts"),
            "const value = 1;\nconsole.log(value);\n",
        )
        .expect("write app");
        fs::write(tempdir.path().join("src/lib.rs"), "fn main() {}\n").expect("write lib");
        let tools = ToolSet::new(tempdir.path());

        let find_output = tools
            .execute_raw("find", json!({ "pattern": "**/*.ts" }))
            .expect("find");
        let find_text = match &find_output.content[0] {
            pi_rust_ai_core::UserContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text output"),
        };
        assert!(find_text.contains("src/app.ts"));

        let grep_output = tools
            .execute_raw("grep", json!({ "pattern": "console", "path": "src" }))
            .expect("grep");
        let grep_text = match &grep_output.content[0] {
            pi_rust_ai_core::UserContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text output"),
        };
        assert!(grep_text.contains("app.ts:2"));
    }

    #[test]
    fn ls_and_bash_tools_return_output() {
        let tempdir = tempdir().expect("tempdir");
        fs::write(tempdir.path().join("alpha.txt"), "alpha").expect("write file");
        fs::create_dir_all(tempdir.path().join("beta")).expect("mkdir");

        let tools = ToolSet::new(tempdir.path());
        let ls_output = tools.execute_raw("ls", json!({})).expect("ls");
        let ls_text = match &ls_output.content[0] {
            pi_rust_ai_core::UserContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text output"),
        };
        assert!(ls_text.contains("alpha.txt"));
        assert!(ls_text.contains("beta/"));

        let bash_output = tools
            .execute_raw("bash", json!({ "command": "printf 'hello'" }))
            .expect("bash");
        let bash_text = match &bash_output.content[0] {
            pi_rust_ai_core::UserContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text output"),
        };
        assert!(bash_text.contains("hello"));
    }
}
