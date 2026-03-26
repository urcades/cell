use std::path::PathBuf;

use pi_rust_plugins::{
    LifecycleHookContextV1, LifecycleHookOutcomeV1, PluginManifestV1,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PLUGIN_PROTOCOL_VERSION_V1: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostIdentity {
    pub name: String,
    pub version: String,
}

impl HostIdentity {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum HostMessage {
    HandshakeRequest {
        protocol_version: u16,
        host: HostIdentity,
        workspace_root: Option<PathBuf>,
    },
    ShutdownRequest {
        reason: Option<String>,
    },
    CommandRequest {
        request_id: String,
        command_name: String,
        args: Vec<String>,
        cwd: PathBuf,
        session_id: Option<String>,
        raw_input: Option<String>,
    },
    HookRequest {
        request_id: String,
        hook_name: String,
        context: LifecycleHookContextV1,
    },
    ToolRequest {
        request_id: String,
        tool_call_id: String,
        tool_name: String,
        arguments: Value,
        cwd: PathBuf,
        session_id: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PluginMessage {
    Registration {
        protocol_version: u16,
        manifest: PluginManifestV1,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    ShutdownAck {
        ok: bool,
    },
    CommandResponse {
        request_id: String,
        replacement: String,
    },
    CommandError {
        request_id: String,
        message: String,
        details: Option<Value>,
    },
    HookResponse {
        request_id: String,
        outcome: LifecycleHookOutcomeV1,
    },
    HookError {
        request_id: String,
        message: String,
        details: Option<Value>,
    },
    ToolResponse {
        request_id: String,
        content: Vec<PluginContentBlock>,
        details: Option<Value>,
        is_error: bool,
    },
    ToolError {
        request_id: String,
        message: String,
        details: Option<Value>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginContentBlock {
    Text { text: String },
    Image { data: String, mime_type: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_rust_plugins::LifecycleEventV1;
    use std::collections::BTreeMap;

    fn hook_context() -> LifecycleHookContextV1 {
        let mut data = BTreeMap::new();
        data.insert("key".to_string(), Value::String("value".to_string()));

        LifecycleHookContextV1 {
            event: LifecycleEventV1::HostStartup,
            plugin_id: "target-plugin".to_string(),
            workspace_root: Some(PathBuf::from("/workspace")),
            session_id: Some("session-1".to_string()),
            provider_id: Some("provider-1".to_string()),
            model_id: Some("model-1".to_string()),
            data,
        }
    }

    #[test]
    fn hook_request_round_trips() {
        let message = HostMessage::HookRequest {
            request_id: "hook-1".to_string(),
            hook_name: "startup".to_string(),
            context: hook_context(),
        };

        let json = serde_json::to_string(&message).expect("serialize");
        let decoded: HostMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, message);
    }

    #[test]
    fn hook_response_round_trips() {
        let message = PluginMessage::HookResponse {
            request_id: "hook-1".to_string(),
            outcome: LifecycleHookOutcomeV1::StopPropagation,
        };

        let json = serde_json::to_string(&message).expect("serialize");
        let decoded: PluginMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, message);
    }

    #[test]
    fn hook_error_round_trips() {
        let message = PluginMessage::HookError {
            request_id: "hook-1".to_string(),
            message: "hook failed".to_string(),
            details: Some(Value::String("details".to_string())),
        };

        let json = serde_json::to_string(&message).expect("serialize");
        let decoded: PluginMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, message);
    }
}
