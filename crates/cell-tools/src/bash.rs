use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use cell_ai_core::UserContentBlock;
use serde::Deserialize;
use serde_json::{Value, json};
use wait_timeout::ChildExt;

use crate::truncate::{TruncationOptions, truncate_tail};
use crate::{ToolError, ToolOutput};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashExecutionResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BashToolInput {
    command: String,
    timeout: Option<u64>,
}

static TEMP_OUTPUT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn definition() -> Value {
    json!({
        "name": "bash",
        "description": "Execute a shell command in the current working directory. Returns combined stdout/stderr plus structured execution metadata.",
        "parameters": {
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "timeout": {"type": "number"}
            },
            "required": ["command"]
        }
    })
}

pub fn execute(cwd: &Path, input: Value) -> Result<ToolOutput, ToolError> {
    let input: BashToolInput = serde_json::from_value(input)?;
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut child = Command::new(shell)
        .arg("-lc")
        .arg(&input.command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(timeout_seconds) = input.timeout {
        let timeout = Duration::from_secs(timeout_seconds);
        if child.wait_timeout(timeout)?.is_none() {
            child.kill()?;
            let output = child.wait_with_output()?;
            let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
            let details = json!({
                "toolKind": "bash",
                "renderKind": "shell-output",
                "command": input.command,
                "timedOut": true,
                "timeoutSeconds": timeout_seconds,
                "exitCode": output.status.code(),
                "cancelled": output.status.code().is_none(),
                "status": "timedOut",
            });
            let message = if combined.is_empty() {
                "Command timed out".to_string()
            } else {
                combined
            };
            return Err(ToolError::Detailed(message, Some(details)));
        }
    }

    let output = child.wait_with_output()?;
    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let truncation = truncate_tail(&combined, TruncationOptions::default());
    let exit_code = output.status.code();
    let cancelled = exit_code.is_none();
    let successful = output.status.success();
    let mut text = truncation.content.clone();
    let mut details = json!({
        "toolKind": "bash",
        "renderKind": "shell-output",
        "command": input.command,
        "exitCode": exit_code,
        "cancelled": cancelled,
        "status": if successful { "completed" } else { "failed" },
        "outputBytes": combined.len(),
        "outputLines": combined.lines().count(),
        "truncated": truncation.truncated,
    });

    if truncation.truncated {
        let temp_path = unique_temp_output_path();
        fs::write(&temp_path, combined.as_bytes())?;
        if let Some(object) = details.as_object_mut() {
            object.insert("truncation".to_string(), json!(truncation));
            object.insert(
                "fullOutputPath".to_string(),
                json!(temp_path.to_string_lossy().to_string()),
            );
        }
    }

    if !successful {
        if text.is_empty() {
            text = if cancelled {
                "Command cancelled".to_string()
            } else {
                "Command failed".to_string()
            };
        }
        return Err(ToolError::Detailed(text, Some(details)));
    }

    Ok(ToolOutput {
        content: vec![UserContentBlock::Text {
            text,
            text_signature: None,
        }],
        details: Some(details),
        is_error: false,
    })
}

pub fn execute_direct(cwd: &Path, command: &str) -> Result<BashExecutionResult, ToolError> {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let output = Command::new(shell)
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .output()?;

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let truncation = truncate_tail(&combined, TruncationOptions::default());
    let mut full_output_path = None;

    if truncation.truncated {
        let temp_path = unique_temp_output_path();
        fs::write(&temp_path, combined.as_bytes())?;
        full_output_path = Some(temp_path.to_string_lossy().to_string());
    }

    Ok(BashExecutionResult {
        output: if truncation.truncated {
            truncation.content
        } else {
            combined
        },
        exit_code: output.status.code(),
        cancelled: output.status.code().is_none(),
        truncated: truncation.truncated,
        full_output_path,
    })
}

fn unique_temp_output_path() -> std::path::PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = TEMP_OUTPUT_COUNTER.fetch_add(1, Ordering::Relaxed);
    env::temp_dir().join(format!(
        "cell-bash-{timestamp}-{}-{counter}.log",
        std::process::id()
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::{execute, execute_direct};

    fn text_output_text(output: &crate::ToolOutput) -> String {
        match &output.content[0] {
            cell_ai_core::UserContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text output"),
        }
    }

    #[test]
    fn bash_tool_reports_line_truncation_surface_and_full_output_path() {
        let tempdir = tempdir().expect("tempdir");
        let output = execute(
            tempdir.path(),
            json!({
                "command": "i=1; while [ $i -le 2101 ]; do if [ $i -eq 2101 ]; then printf 'line'; else printf 'line\\n'; fi; i=$((i+1)); done"
            }),
        )
        .expect("bash");

        let text = text_output_text(&output);
        assert!(!text.contains("Full output: "));
        assert!(!text.contains("Output truncated"));
        assert!(!text.contains("50.0KB limit"));
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("toolKind"))
                .and_then(Value::as_str),
            Some("bash")
        );
        assert_eq!(
            output
                .details
                .as_ref()
                .and_then(|details| details.get("renderKind"))
                .and_then(Value::as_str),
            Some("shell-output")
        );

        let path = output
            .details
            .as_ref()
            .and_then(|details| details.get("fullOutputPath"))
            .and_then(Value::as_str)
            .expect("full output path");
        assert!(Path::new(path).exists());
        let full_output = fs::read_to_string(path).expect("read full output");
        assert!(full_output.contains("line"));
    }

    #[test]
    fn bash_tool_reports_failure_without_exit_code_prose() {
        let tempdir = tempdir().expect("tempdir");
        let error = execute(
            tempdir.path(),
            json!({
                "command": "printf oops; exit 3"
            }),
        )
        .expect_err("bash failure");

        let super::ToolError::Detailed(message, details) = error else {
            panic!("expected detailed error");
        };

        assert_eq!(message, "oops");
        let details = details.expect("details");
        assert_eq!(
            details.get("toolKind").and_then(Value::as_str),
            Some("bash")
        );
        assert_eq!(
            details.get("renderKind").and_then(Value::as_str),
            Some("shell-output")
        );
        assert_eq!(
            details.get("status").and_then(Value::as_str),
            Some("failed")
        );
        assert_eq!(details.get("exitCode").and_then(Value::as_i64), Some(3));
    }

    #[test]
    fn bash_direct_execution_preserves_success_exit_code_in_data() {
        let tempdir = tempdir().expect("tempdir");
        let result = execute_direct(tempdir.path(), "printf hi").expect("bash direct");

        assert_eq!(result.output, "hi");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.cancelled);
        assert!(!result.truncated);
    }
}
