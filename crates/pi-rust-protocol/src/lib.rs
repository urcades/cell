use pi_rust_ai_core::{AssistantMessageEvent, Message, Model};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputMode {
    Text,
    Json,
    Rpc,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    All,
    OneAtATime,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcCommandSource {
    Extension,
    Prompt,
    Skill,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcCommandLocation {
    User,
    Project,
    Path,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcNotifyType {
    Info,
    Warning,
    Error,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RpcWidgetPlacement {
    AboveEditor,
    BelowEditor,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSlashCommand {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: RpcCommandSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<RpcCommandLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    pub thinking_level: String,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub auto_compaction_enabled: bool,
    pub message_count: usize,
    pub pending_message_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_calls: usize,
    pub tool_results: usize,
    pub total_messages: usize,
    pub tokens: RpcTokenStats,
    pub cost: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcTokenStats {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcForkMessage {
    pub entry_id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcBashResult {
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RpcCommand {
    Prompt {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<Value>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<String>,
    },
    Steer {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<Value>>,
    },
    FollowUp {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        images: Option<Vec<Value>>,
    },
    Abort {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    NewSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
    },
    GetState {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SetModel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    CycleModel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetAvailableModels {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SetThinkingLevel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        level: String,
    },
    CycleThinkingLevel {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SetSteeringMode {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        mode: QueueMode,
    },
    SetFollowUpMode {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        mode: QueueMode,
    },
    Compact {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    SetAutoCompaction {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        enabled: bool,
    },
    SetAutoRetry {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        enabled: bool,
    },
    AbortRetry {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Bash {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        command: String,
    },
    AbortBash {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetSessionStats {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    ExportHtml {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_path: Option<String>,
    },
    SwitchSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        session_path: String,
    },
    Fork {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        entry_id: String,
    },
    GetForkMessages {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetLastAssistantText {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    SetSessionName {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
    },
    GetMessages {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    GetCommands {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

impl RpcCommand {
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Prompt { id, .. }
            | Self::Steer { id, .. }
            | Self::FollowUp { id, .. }
            | Self::Abort { id }
            | Self::NewSession { id, .. }
            | Self::GetState { id }
            | Self::SetModel { id, .. }
            | Self::CycleModel { id }
            | Self::GetAvailableModels { id }
            | Self::SetThinkingLevel { id, .. }
            | Self::CycleThinkingLevel { id }
            | Self::SetSteeringMode { id, .. }
            | Self::SetFollowUpMode { id, .. }
            | Self::Compact { id, .. }
            | Self::SetAutoCompaction { id, .. }
            | Self::SetAutoRetry { id, .. }
            | Self::AbortRetry { id }
            | Self::Bash { id, .. }
            | Self::AbortBash { id }
            | Self::GetSessionStats { id }
            | Self::ExportHtml { id, .. }
            | Self::SwitchSession { id, .. }
            | Self::Fork { id, .. }
            | Self::GetForkMessages { id }
            | Self::GetLastAssistantText { id }
            | Self::SetSessionName { id, .. }
            | Self::GetMessages { id }
            | Self::GetCommands { id } => id.as_deref(),
        }
    }

    pub fn command_name(&self) -> &'static str {
        match self {
            Self::Prompt { .. } => "prompt",
            Self::Steer { .. } => "steer",
            Self::FollowUp { .. } => "follow_up",
            Self::Abort { .. } => "abort",
            Self::NewSession { .. } => "new_session",
            Self::GetState { .. } => "get_state",
            Self::SetModel { .. } => "set_model",
            Self::CycleModel { .. } => "cycle_model",
            Self::GetAvailableModels { .. } => "get_available_models",
            Self::SetThinkingLevel { .. } => "set_thinking_level",
            Self::CycleThinkingLevel { .. } => "cycle_thinking_level",
            Self::SetSteeringMode { .. } => "set_steering_mode",
            Self::SetFollowUpMode { .. } => "set_follow_up_mode",
            Self::Compact { .. } => "compact",
            Self::SetAutoCompaction { .. } => "set_auto_compaction",
            Self::SetAutoRetry { .. } => "set_auto_retry",
            Self::AbortRetry { .. } => "abort_retry",
            Self::Bash { .. } => "bash",
            Self::AbortBash { .. } => "abort_bash",
            Self::GetSessionStats { .. } => "get_session_stats",
            Self::ExportHtml { .. } => "export_html",
            Self::SwitchSession { .. } => "switch_session",
            Self::Fork { .. } => "fork",
            Self::GetForkMessages { .. } => "get_fork_messages",
            Self::GetLastAssistantText { .. } => "get_last_assistant_text",
            Self::SetSessionName { .. } => "set_session_name",
            Self::GetMessages { .. } => "get_messages",
            Self::GetCommands { .. } => "get_commands",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcExtensionUiRequest {
    Select {
        id: String,
        title: String,
        options: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Confirm {
        id: String,
        title: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Input {
        id: String,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Editor {
        id: String,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prefill: Option<String>,
    },
    Notify {
        id: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        notify_type: Option<RpcNotifyType>,
    },
    SetStatus {
        id: String,
        status_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        status_text: Option<String>,
    },
    SetWidget {
        id: String,
        widget_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        widget_lines: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        widget_placement: Option<RpcWidgetPlacement>,
    },
    SetTitle {
        id: String,
        title: String,
    },
    SetEditorText {
        id: String,
        text: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcExtensionUiResponse {
    ExtensionUiResponse {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        confirmed: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cancelled: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcInbound {
    Command(RpcCommand),
    ExtensionUiResponse(RpcExtensionUiResponse),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub response_type: &'static str,
    pub command: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RpcResponse {
    pub fn success(id: Option<String>, command: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            id,
            response_type: "response",
            command: command.into(),
            success: true,
            data,
            error: None,
        }
    }

    pub fn error(id: Option<String>, command: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            id,
            response_type: "response",
            command: command.into(),
            success: false,
            data: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RpcEvent {
    AgentStart,
    AgentEnd {
        messages: Vec<Message>,
    },
    TurnStart,
    TurnEnd {
        message: Message,
        tool_results: Vec<Message>,
    },
    MessageStart {
        message: Message,
    },
    MessageUpdate {
        message: Message,
        assistant_message_event: AssistantMessageEvent,
    },
    MessageEnd {
        message: Message,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: Value,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: Message,
        is_error: bool,
    },
    AutoCompactionStart {
        reason: String,
    },
    AutoCompactionEnd {
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        aborted: bool,
        will_retry: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    AutoRetryStart {
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        error_message: String,
    },
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        final_error: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{QueueMode, RpcCommand, RpcInbound, RpcResponse};

    #[test]
    fn parses_set_model_command_with_model_id() {
        let command: RpcCommand = serde_json::from_value(json!({
            "type": "set_model",
            "provider": "openai",
            "modelId": "gpt-5.1-codex"
        }))
        .expect("parse command");

        assert_eq!(command.command_name(), "set_model");
        assert_eq!(command.id(), None);
    }

    #[test]
    fn parses_extension_ui_response_as_inbound() {
        let inbound: RpcInbound = serde_json::from_value(json!({
            "type": "extension_ui_response",
            "id": "req-1",
            "value": "hello"
        }))
        .expect("parse inbound");

        assert!(matches!(inbound, RpcInbound::ExtensionUiResponse(_)));
    }

    #[test]
    fn serializes_queue_mode_like_typescript() {
        assert_eq!(
            serde_json::to_value(QueueMode::OneAtATime).expect("serialize"),
            json!("one-at-a-time")
        );
    }

    #[test]
    fn response_helper_sets_response_type() {
        let response = RpcResponse::success(Some("1".to_string()), "get_state", None);
        assert_eq!(response.response_type, "response");
        assert!(response.success);
    }

    #[test]
    fn parses_camel_case_rpc_command_fields() {
        let command: RpcCommand = serde_json::from_value(json!({
            "type": "export_html",
            "outputPath": "/tmp/out.html"
        }))
        .expect("parse export command");
        assert!(
            matches!(command, RpcCommand::ExportHtml { output_path, .. } if output_path.as_deref() == Some("/tmp/out.html"))
        );

        let command: RpcCommand = serde_json::from_value(json!({
            "type": "new_session",
            "parentSession": "/tmp/session.jsonl"
        }))
        .expect("parse new session");
        assert!(
            matches!(command, RpcCommand::NewSession { parent_session, .. } if parent_session.as_deref() == Some("/tmp/session.jsonl"))
        );
    }
}
