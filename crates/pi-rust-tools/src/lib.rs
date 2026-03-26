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
use std::sync::{Arc, Mutex};

use pi_rust_ai_core::{ToolDefinition, ToolResultMessage, UserContentBlock};
use pi_rust_plugin_host::{ActivePluginRegistry, PluginContentBlock};
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

#[derive(Clone)]
pub struct ToolSet {
    cwd: PathBuf,
    enabled: BTreeMap<String, bool>,
    plugin_runtime: Option<Arc<Mutex<ActivePluginRegistry>>>,
    plugin_tools_enabled_by_default: bool,
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
            plugin_runtime: None,
            plugin_tools_enabled_by_default: false,
        }
    }

    pub fn with_enabled_names(cwd: impl Into<PathBuf>, enabled_names: &[String]) -> Self {
        Self::with_enabled_names_and_plugins(cwd, enabled_names, false)
    }

    pub fn with_enabled_names_and_plugins(
        cwd: impl Into<PathBuf>,
        enabled_names: &[String],
        plugin_tools_enabled_by_default: bool,
    ) -> Self {
        let mut enabled = BTreeMap::new();
        for name in BUILTIN_TOOLS {
            enabled.insert(
                (*name).to_string(),
                enabled_names.iter().any(|enabled| enabled == name),
            );
        }
        for name in enabled_names {
            enabled.entry(name.clone()).or_insert(true);
        }
        Self {
            cwd: cwd.into(),
            enabled,
            plugin_runtime: None,
            plugin_tools_enabled_by_default,
        }
    }

    pub fn attach_plugin_runtime(&mut self, plugin_runtime: Arc<Mutex<ActivePluginRegistry>>) {
        self.plugin_runtime = Some(plugin_runtime);
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = BUILTIN_TOOLS
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
            .collect::<Vec<_>>();

        if let Some(plugin_runtime) = &self.plugin_runtime {
            if let Ok(plugin_runtime) = plugin_runtime.lock() {
                for (name, registration) in &plugin_runtime.merged_registry().tools {
                    if BUILTIN_TOOLS.contains(&name.as_str()) {
                        continue;
                    }
                    if registration.registration.hidden {
                        continue;
                    }
                    if !self.plugin_tool_enabled(name) {
                        continue;
                    }
                    definitions.push(ToolDefinition {
                        name: registration.registration.name.clone(),
                        description: registration
                            .registration
                            .description
                            .clone()
                            .unwrap_or_default(),
                        parameters: plugin_tool_parameters(&registration.registration.parameters),
                    });
                }
            }
        }

        definitions
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
        if !self.plugin_tool_enabled(tool_name) {
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
            _ => self.execute_plugin_tool(tool_name, input),
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn execute_bash_direct(&self, command: &str) -> Result<BashExecutionResult, ToolError> {
        bash::execute_direct(&self.cwd, command)
    }

    fn plugin_tool_enabled(&self, tool_name: &str) -> bool {
        self.enabled
            .get(tool_name)
            .copied()
            .unwrap_or(self.plugin_tools_enabled_by_default)
    }

    fn execute_plugin_tool(&self, tool_name: &str, input: Value) -> Result<ToolOutput, ToolError> {
        let Some(plugin_runtime) = &self.plugin_runtime else {
            return Err(ToolError::Message(format!("Unknown tool: {tool_name}")));
        };
        let mut plugin_runtime = plugin_runtime
            .lock()
            .map_err(|_| ToolError::Message("Failed to lock plugin runtime".to_string()))?;
        let (content, details, is_error) = plugin_runtime
            .invoke_tool("plugin-tool", tool_name, input, &self.cwd, None)
            .map_err(|error| ToolError::Detailed(error.to_string(), None))?;
        Ok(ToolOutput {
            content: content
                .into_iter()
                .map(plugin_content_block_to_user_block)
                .collect(),
            details,
            is_error,
        })
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

fn plugin_tool_parameters(parameters: &[pi_rust_plugins::ParameterRegistrationV1]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for parameter in parameters {
        if parameter.required {
            required.push(parameter.name.clone());
        }
        properties.insert(
            parameter.name.clone(),
            serde_json::json!({
                "type": value_kind_to_json_type(&parameter.kind),
                "description": parameter.description.clone().unwrap_or_default(),
            }),
        );
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": true
    })
}

fn value_kind_to_json_type(kind: &pi_rust_plugins::ValueKindV1) -> &'static str {
    match kind {
        pi_rust_plugins::ValueKindV1::String | pi_rust_plugins::ValueKindV1::Path => "string",
        pi_rust_plugins::ValueKindV1::Boolean => "boolean",
        pi_rust_plugins::ValueKindV1::Integer => "integer",
        pi_rust_plugins::ValueKindV1::Number => "number",
        pi_rust_plugins::ValueKindV1::Json
        | pi_rust_plugins::ValueKindV1::StringList
        | pi_rust_plugins::ValueKindV1::StringMap => "object",
    }
}

fn plugin_content_block_to_user_block(block: PluginContentBlock) -> UserContentBlock {
    match block {
        PluginContentBlock::Text { text } => UserContentBlock::Text {
            text,
            text_signature: None,
        },
        PluginContentBlock::Image { data, mime_type } => UserContentBlock::Image { data, mime_type },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use pi_rust_plugin_host::{DISCOVERY_FILE_NAMES, HostIdentity, PluginHost, PluginHostConfig};
    use pi_rust_plugins::{
        ModelInputKindV1, PluginIdentityV1, PluginManifestV1, ProviderAuthV1,
        ProviderRegistrationV1, ToolRegistrationV1, ValueKindV1,
    };
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::ToolSet;

    fn write_executable_script(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write script");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("chmod");
        }
    }

    fn plugin_manifest_json(id: &str, name: &str, tool_name: &str) -> String {
        let mut manifest = PluginManifestV1::new(PluginIdentityV1 {
            id: id.to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: Some(format!("{name} plugin")),
            authors: vec!["Acme".to_string()],
            homepage: None,
            repository: None,
            license: Some("MIT".to_string()),
        });
        manifest.tools.push(ToolRegistrationV1 {
            name: tool_name.to_string(),
            description: Some(format!("Tool {tool_name}")),
            aliases: Vec::new(),
            parameters: Vec::new(),
            output: Some(ValueKindV1::String),
            hidden: false,
        });
        manifest.providers.push(ProviderRegistrationV1 {
            provider_id: id.to_string(),
            name: format!("{id} provider"),
            api: format!("{id}-chat"),
            description: Some(format!("Provider {id}")),
            base_url: Some("https://example.invalid".to_string()),
            headers: Default::default(),
            auth: ProviderAuthV1::None,
        });
        manifest.models.push(pi_rust_plugins::ModelRegistrationV1 {
            provider_id: id.to_string(),
            model_id: format!("{id}-model"),
            name: format!("{name} Model"),
            description: None,
            input_modalities: vec![ModelInputKindV1::Text],
            reasoning: false,
            context_window: 4096,
            max_output_tokens: 1024,
            default: false,
        });

        serde_json::to_string(&pi_rust_plugin_host::PluginMessage::Registration {
            protocol_version: pi_rust_plugin_host::HOST_PROTOCOL_VERSION_V1,
            manifest,
        })
        .expect("serialize registration")
    }

    fn plugin_tool_runtime_script(manifest_json: &str) -> String {
        format!(
            r#"#!/bin/sh
set -eu
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
cat >"$tmp" <<'PY'
import json, sys
handshake = json.loads(sys.stdin.readline())
if handshake.get("type") != "handshake_request":
    sys.stderr.write("unexpected handshake\n")
    sys.exit(42)
print(r'''{manifest_json}''')
sys.stdout.flush()
request = json.loads(sys.stdin.readline())
assert request["type"] == "tool_request"
print(json.dumps({{
    "type": "tool_response",
    "requestId": request["requestId"],
    "content": [{{"type": "text", "text": f"plugin:{{request['arguments']['value']}}"}}],
    "details": {{"echo": request["arguments"]}},
    "isError": False,
}}), flush=True)
PY
python3 "$tmp"
"#
        )
    }

    fn write_plugin_descriptor(root: &Path, id: &str, name: &str) {
        fs::create_dir_all(root).expect("create plugin dir");
        let descriptor = pi_rust_plugin_host::PluginLaunchDescriptor {
            id: id.to_string(),
            name: name.to_string(),
            executable: PathBuf::from("plugin.sh"),
            args: Vec::new(),
            working_directory: None,
            env: Default::default(),
            description: Some(format!("{name} plugin")),
        };
        fs::write(
            root.join(DISCOVERY_FILE_NAMES[0]),
            serde_json::to_string_pretty(&descriptor).expect("serialize descriptor"),
        )
        .expect("write descriptor");
    }

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

    #[test]
    fn plugin_tools_are_exposed_and_execute_through_tool_set() {
        let tempdir = tempdir().expect("tempdir");
        let plugin_root = tempdir.path().join("plugins/tool");
        write_executable_script(
            &plugin_root.join("plugin.sh"),
            &plugin_tool_runtime_script(&plugin_manifest_json(
                "tool-plugin",
                "Tool Plugin",
                "plugin-tool",
            )),
        );
        write_plugin_descriptor(&plugin_root, "tool-plugin", "Tool Plugin");

        let host = PluginHost::new(PluginHostConfig {
            discovery_roots: vec![plugin_root.clone()],
            workspace_root: Some(tempdir.path().to_path_buf()),
            handshake_timeout: Duration::from_millis(500),
            host_identity: HostIdentity::new("pi-rust-plugin-host", "0.52.12"),
        });
        let runtime = host.discover_and_load_runtime_plugins();
        assert!(runtime.summary.warnings.is_empty(), "{:#?}", runtime.summary.warnings);
        let registry = Arc::new(Mutex::new(runtime.registry.expect("runtime registry")));

        let enabled = Vec::<String>::new();
        let mut tools = ToolSet::with_enabled_names_and_plugins(tempdir.path(), &enabled, true);
        tools.attach_plugin_runtime(registry);

        let definitions = tools.definitions();
        assert!(definitions.iter().any(|definition| definition.name == "plugin-tool"));

        let output = tools
            .execute_raw("plugin-tool", json!({ "value": "beta" }))
            .expect("plugin tool");
        let text = match &output.content[0] {
            pi_rust_ai_core::UserContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text output"),
        };
        assert_eq!(text, "plugin:beta");
        assert_eq!(output.details, Some(json!({ "echo": { "value": "beta" } })));
        assert!(!output.is_error);
    }
}
