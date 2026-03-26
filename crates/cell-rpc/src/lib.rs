use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use cell_ai_core::{Message, UserContent, UserContentBlock, UserMessage};
use cell_core::{AgentControl, AgentSession, rpc_event_from_agent_event};
use cell_jsonline_transport::{
    InboundFrame, JsonLineService, ServiceControl, TransportEmitter, run_stdio_service,
};
use cell_protocol::{RpcCommand, RpcEvent, RpcInbound, RpcResponse};
use serde::Serialize;
use serde_json::json;

pub fn run_rpc_with_io(
    reader: impl Read + Send + 'static,
    writer: impl Write,
    session: AgentSession,
) -> Result<i32, String> {
    run_stdio_service(reader, writer, RpcService::new(session)?)
}

struct RpcService {
    shared_session: Arc<Mutex<AgentSession>>,
    control: AgentControl,
    prompt_handle: Option<thread::JoinHandle<()>>,
    reader_closed: bool,
    deferred_commands: VecDeque<RpcCommand>,
}

impl RpcService {
    fn new(session: AgentSession) -> Result<Self, String> {
        let shared_session = Arc::new(Mutex::new(session));
        let control = shared_session
            .lock()
            .map_err(|_| "Failed to lock RPC session".to_string())?
            .control();
        Ok(Self {
            shared_session,
            control,
            prompt_handle: None,
            reader_closed: false,
            deferred_commands: VecDeque::new(),
        })
    }

    fn handle_inbound_command(
        &mut self,
        command: RpcCommand,
        emitter: &TransportEmitter<RpcResponse, RpcEvent>,
    ) -> Result<(), String> {
        self.join_finished_prompt();

        if self.prompt_active() {
            if is_midstream_rpc_command(&command) {
                handle_streaming_rpc_command(&self.control, emitter, command)?;
            } else {
                self.deferred_commands.push_back(command);
            }
            return Ok(());
        }

        if !self.deferred_commands.is_empty() {
            self.deferred_commands.push_back(command);
            return Ok(());
        }

        self.handle_idle_command(command, emitter)
    }

    fn handle_idle_command(
        &mut self,
        command: RpcCommand,
        emitter: &TransportEmitter<RpcResponse, RpcEvent>,
    ) -> Result<(), String> {
        match command {
            RpcCommand::Prompt {
                id,
                message,
                images,
                streaming_behavior: _,
            } => {
                let prompt_message = match user_rpc_message(message, images) {
                    Ok(message) => message,
                    Err(error) => {
                        emit_response(emitter, RpcResponse::error(id, "prompt", error))?;
                        return Ok(());
                    }
                };
                self.start_prompt(id, prompt_message, emitter)
            }
            RpcCommand::Steer {
                id,
                message,
                images,
            } => match user_rpc_message(message, images) {
                Ok(message) => {
                    self.control.steer(message);
                    emit_response(emitter, RpcResponse::success(id, "steer", None))
                }
                Err(error) => emit_response(emitter, RpcResponse::error(id, "steer", error)),
            },
            RpcCommand::FollowUp {
                id,
                message,
                images,
            } => match user_rpc_message(message, images) {
                Ok(message) => {
                    self.control.follow_up(message);
                    emit_response(emitter, RpcResponse::success(id, "follow_up", None))
                }
                Err(error) => emit_response(emitter, RpcResponse::error(id, "follow_up", error)),
            },
            RpcCommand::Abort { id } => {
                self.control.abort();
                emit_response(emitter, RpcResponse::success(id, "abort", None))
            }
            other => {
                let mut session = self
                    .shared_session
                    .lock()
                    .map_err(|_| "Failed to lock RPC session".to_string())?;
                handle_rpc_command(&mut session, emitter, other)
            }
        }
    }

    fn start_prompt(
        &mut self,
        id: Option<String>,
        prompt_message: Message,
        emitter: &TransportEmitter<RpcResponse, RpcEvent>,
    ) -> Result<(), String> {
        let session = Arc::clone(&self.shared_session);
        let prompt_emitter = emitter.clone();
        let prompt_id = id.clone();
        let (started_tx, started_rx) = mpsc::channel();
        self.prompt_handle = Some(thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
            let outcome = {
                let mut session = session.lock().expect("prompt session lock");
                session.prepare_prompt();
                let _ = started_tx.send(());
                runtime.block_on(session.prompt_message_prepared(prompt_message))
            };
            match outcome {
                Ok(run) => {
                    for event in run.events {
                        let _ = prompt_emitter.send_event(rpc_event_from_agent_event(event));
                    }
                }
                Err(error) => {
                    let _ = prompt_emitter.send_response(RpcResponse::error(
                        prompt_id,
                        "prompt",
                        error.to_string(),
                    ));
                }
            }
        }));

        started_rx
            .recv()
            .map_err(|_| "Failed to start prompt worker".to_string())?;
        emit_response(emitter, RpcResponse::success(id, "prompt", None))
    }

    fn join_finished_prompt(&mut self) {
        if self
            .prompt_handle
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
        {
            if let Some(handle) = self.prompt_handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn prompt_active(&self) -> bool {
        self.prompt_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }
}

impl JsonLineService for RpcService {
    type Request = RpcInbound;
    type Response = RpcResponse;
    type Event = RpcEvent;

    fn handle_frame(
        &mut self,
        frame: InboundFrame<Self::Request>,
        emitter: &TransportEmitter<Self::Response, Self::Event>,
    ) -> Result<ServiceControl, String> {
        match frame {
            InboundFrame::Message(RpcInbound::ExtensionUiResponse(_)) => {}
            InboundFrame::Message(RpcInbound::Command(command)) => {
                self.handle_inbound_command(command, emitter)?;
            }
            InboundFrame::ProtocolError { error, .. } => {
                emit_response(emitter, RpcResponse::error(None, "parse", error))?;
            }
            InboundFrame::StreamClosed => {
                self.reader_closed = true;
            }
        }
        Ok(ServiceControl::Continue)
    }

    fn tick(
        &mut self,
        emitter: &TransportEmitter<Self::Response, Self::Event>,
    ) -> Result<ServiceControl, String> {
        self.join_finished_prompt();

        while !self.prompt_active() {
            let Some(command) = self.deferred_commands.pop_front() else {
                break;
            };
            self.handle_idle_command(command, emitter)?;
            self.join_finished_prompt();
            if self.prompt_active() {
                break;
            }
        }

        if self.reader_closed && !self.prompt_active() && self.deferred_commands.is_empty() {
            Ok(ServiceControl::Exit(0))
        } else {
            Ok(ServiceControl::Continue)
        }
    }
}

fn handle_rpc_command(
    session: &mut AgentSession,
    emitter: &TransportEmitter<RpcResponse, RpcEvent>,
    command: RpcCommand,
) -> Result<(), String> {
    let command_name = command.command_name().to_string();

    match command {
        RpcCommand::Prompt { .. }
        | RpcCommand::Steer { .. }
        | RpcCommand::FollowUp { .. }
        | RpcCommand::Abort { .. } => {
            return Err("Streaming commands must be handled by the RPC runtime.".to_string());
        }
        RpcCommand::NewSession { id, parent_session } => {
            let cancelled = session
                .new_session(parent_session.as_deref())
                .map_err(|error| error.to_string())?;
            emit_response(
                emitter,
                RpcResponse::success(id, "new_session", Some(json!({ "cancelled": cancelled }))),
            )?;
        }
        RpcCommand::GetState { id } => {
            emit_success(emitter, id, "get_state", &session.get_state())?;
        }
        RpcCommand::SetModel {
            id,
            provider,
            model_id,
        } => {
            let model = session
                .set_model(&provider, &model_id)
                .map_err(|error| error.to_string())?;
            emit_success(emitter, id, "set_model", &model)?;
        }
        RpcCommand::CycleModel { id } => {
            let result = session.cycle_model().map_err(|error| error.to_string())?;
            emit_success(emitter, id, "cycle_model", &result)?;
        }
        RpcCommand::GetAvailableModels { id } => {
            emit_success(
                emitter,
                id,
                "get_available_models",
                &json!({ "models": session.get_available_models() }),
            )?;
        }
        RpcCommand::SetThinkingLevel { id, level } => {
            session
                .set_thinking_level(&level)
                .map_err(|error| error.to_string())?;
            emit_response(
                emitter,
                RpcResponse::success(id, "set_thinking_level", None),
            )?;
        }
        RpcCommand::CycleThinkingLevel { id } => {
            let result = session
                .cycle_thinking_level()
                .map_err(|error| error.to_string())?;
            emit_success(
                emitter,
                id,
                "cycle_thinking_level",
                &result.map(|level| json!({ "level": level })),
            )?;
        }
        RpcCommand::SetSteeringMode { id, mode } => {
            session.set_steering_mode(mode);
            emit_response(emitter, RpcResponse::success(id, "set_steering_mode", None))?;
        }
        RpcCommand::SetFollowUpMode { id, mode } => {
            session.set_follow_up_mode(mode);
            emit_response(
                emitter,
                RpcResponse::success(id, "set_follow_up_mode", None),
            )?;
        }
        RpcCommand::Compact {
            id,
            custom_instructions,
        } => {
            let result = session
                .compact(custom_instructions.as_deref())
                .map_err(|error| error.to_string())?;
            emit_success(emitter, id, "compact", &result)?;
        }
        RpcCommand::SetAutoCompaction { id, enabled } => {
            session.set_auto_compaction(enabled);
            emit_response(
                emitter,
                RpcResponse::success(id, "set_auto_compaction", None),
            )?;
        }
        RpcCommand::SetAutoRetry { id, enabled } => {
            session.set_auto_retry(enabled);
            emit_response(emitter, RpcResponse::success(id, "set_auto_retry", None))?;
        }
        RpcCommand::AbortRetry { id } => match session.abort_retry() {
            Ok(()) => emit_response(emitter, RpcResponse::success(id, "abort_retry", None))?,
            Err(error) => {
                emit_response(
                    emitter,
                    RpcResponse::error(id, "abort_retry", error.to_string()),
                )?;
            }
        },
        RpcCommand::Bash { id, command } => {
            let result = session.bash(&command).map_err(|error| error.to_string())?;
            emit_success(emitter, id, "bash", &result)?;
        }
        RpcCommand::AbortBash { id } => match session.abort_bash() {
            Ok(()) => emit_response(emitter, RpcResponse::success(id, "abort_bash", None))?,
            Err(error) => {
                emit_response(
                    emitter,
                    RpcResponse::error(id, "abort_bash", error.to_string()),
                )?;
            }
        },
        RpcCommand::GetSessionStats { id } => {
            emit_success(
                emitter,
                id,
                "get_session_stats",
                &session.get_session_stats(),
            )?;
        }
        RpcCommand::GetPluginRuntimeDiagnostics { id } => {
            let diagnostics = session.get_plugin_runtime_diagnostics();
            emit_success(
                emitter,
                id,
                "get_plugin_runtime_diagnostics",
                &diagnostics.with_capability_classes(),
            )?;
        }
        RpcCommand::ExportHtml { id, output_path } => {
            let path = session
                .export_html(output_path.as_deref().map(PathBuf::from).as_deref())
                .map_err(|error| error.to_string())?;
            emit_success(
                emitter,
                id,
                "export_html",
                &json!({ "path": path.to_string_lossy() }),
            )?;
        }
        RpcCommand::SwitchSession { id, session_path } => {
            let cancelled = session
                .switch_session(&session_path)
                .map_err(|error| error.to_string())?;
            emit_success(
                emitter,
                id,
                "switch_session",
                &json!({ "cancelled": cancelled }),
            )?;
        }
        RpcCommand::Fork { id, entry_id } => {
            let (text, cancelled) = session.fork(&entry_id).map_err(|error| error.to_string())?;
            emit_success(
                emitter,
                id,
                "fork",
                &json!({ "text": text, "cancelled": cancelled }),
            )?;
        }
        RpcCommand::GetForkMessages { id } => {
            emit_success(
                emitter,
                id,
                "get_fork_messages",
                &json!({ "messages": session.get_fork_messages() }),
            )?;
        }
        RpcCommand::GetLastAssistantText { id } => {
            emit_success(
                emitter,
                id,
                "get_last_assistant_text",
                &json!({ "text": session.get_last_assistant_text() }),
            )?;
        }
        RpcCommand::SetSessionName { id, name } => {
            session
                .set_session_name(&name)
                .map_err(|error| error.to_string())?;
            emit_response(emitter, RpcResponse::success(id, "set_session_name", None))?;
        }
        RpcCommand::GetMessages { id } => {
            emit_success(
                emitter,
                id,
                "get_messages",
                &json!({ "messages": session.get_messages() }),
            )?;
        }
        RpcCommand::GetCommands { id } => {
            emit_success(
                emitter,
                id,
                "get_commands",
                &json!({ "commands": session.get_commands() }),
            )?;
        }
    }

    if command_name.is_empty() {
        return Err("Unknown command".to_string());
    }
    Ok(())
}

fn handle_streaming_rpc_command(
    control: &AgentControl,
    emitter: &TransportEmitter<RpcResponse, RpcEvent>,
    command: RpcCommand,
) -> Result<(), String> {
    match command {
        RpcCommand::Prompt {
            id,
            message,
            images,
            streaming_behavior,
        } => match streaming_behavior.as_deref() {
            Some("steer") => match user_rpc_message(message, images) {
                Ok(message) => {
                    control.steer(message);
                    emit_response(emitter, RpcResponse::success(id, "prompt", None))
                }
                Err(error) => emit_response(emitter, RpcResponse::error(id, "prompt", error)),
            },
            Some("followUp") => match user_rpc_message(message, images) {
                Ok(message) => {
                    control.follow_up(message);
                    emit_response(emitter, RpcResponse::success(id, "prompt", None))
                }
                Err(error) => emit_response(emitter, RpcResponse::error(id, "prompt", error)),
            },
            _ => emit_response(
                emitter,
                RpcResponse::error(
                    id,
                    "prompt",
                    "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.",
                ),
            ),
        },
        RpcCommand::Steer {
            id,
            message,
            images,
        } => match user_rpc_message(message, images) {
            Ok(message) => {
                control.steer(message);
                emit_response(emitter, RpcResponse::success(id, "steer", None))
            }
            Err(error) => emit_response(emitter, RpcResponse::error(id, "steer", error)),
        },
        RpcCommand::FollowUp {
            id,
            message,
            images,
        } => match user_rpc_message(message, images) {
            Ok(message) => {
                control.follow_up(message);
                emit_response(emitter, RpcResponse::success(id, "follow_up", None))
            }
            Err(error) => emit_response(emitter, RpcResponse::error(id, "follow_up", error)),
        },
        RpcCommand::Abort { id } => {
            control.abort();
            emit_response(emitter, RpcResponse::success(id, "abort", None))
        }
        other => emit_response(
            emitter,
            RpcResponse::error(
                other.id().map(ToOwned::to_owned),
                other.command_name(),
                "Command is unavailable while a prompt is streaming.",
            ),
        ),
    }
}

fn is_midstream_rpc_command(command: &RpcCommand) -> bool {
    matches!(
        command,
        RpcCommand::Prompt { .. }
            | RpcCommand::Steer { .. }
            | RpcCommand::FollowUp { .. }
            | RpcCommand::Abort { .. }
    )
}

fn emit_success<T: Serialize>(
    emitter: &TransportEmitter<RpcResponse, RpcEvent>,
    id: Option<String>,
    command: &str,
    data: &T,
) -> Result<(), String> {
    let data = serde_json::to_value(data).map_err(|error| error.to_string())?;
    emit_response(emitter, RpcResponse::success(id, command, Some(data)))
}

fn emit_response(
    emitter: &TransportEmitter<RpcResponse, RpcEvent>,
    response: RpcResponse,
) -> Result<(), String> {
    emitter.send_response(response)
}

fn user_rpc_message(
    text: String,
    images: Option<Vec<serde_json::Value>>,
) -> Result<Message, String> {
    let mut content = Vec::new();
    if !text.is_empty() || images.as_ref().is_none_or(Vec::is_empty) {
        content.push(UserContentBlock::Text {
            text,
            text_signature: None,
        });
    }

    for image in images.unwrap_or_default() {
        content.push(parse_rpc_image(image)?);
    }

    Ok(Message::User(UserMessage {
        content: UserContent::Blocks(content),
        timestamp: 0,
    }))
}

fn parse_rpc_image(image: serde_json::Value) -> Result<UserContentBlock, String> {
    let object = image
        .as_object()
        .ok_or_else(|| "RPC image payload must be an object.".to_string())?;
    if let Some(image_type) = object.get("type").and_then(serde_json::Value::as_str) {
        if image_type != "image" {
            return Err(format!("Unsupported RPC image payload type: {image_type}"));
        }
    }

    let data = object
        .get("data")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "RPC image payload is missing data.".to_string())?;
    let mime_type = object
        .get("mimeType")
        .or_else(|| object.get("mime_type"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "RPC image payload is missing mimeType.".to_string())?;

    Ok(UserContentBlock::Image {
        data: data.to_string(),
        mime_type: mime_type.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;

    use cell_ai_core::{
        AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message,
        StopReason, StreamOptions, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
    };
    use cell_ai_providers::{ApiProvider, ProviderRegistry};
    use cell_core::{AgentSession, NonInteractiveRequest, create_agent_session};
    use cell_models::ModelRegistry;
    use cell_oauth::AuthStorage;
    use cell_protocol::OutputMode;
    use tempfile::tempdir;

    use super::run_rpc_with_io;

    struct EchoProvider;

    struct SlowEchoProvider;

    impl ApiProvider for EchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &cell_ai_core::Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> cell_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
            let prompt = prompt_text(context);
            let assistant = assistant_message(model, prompt);
            sender.send(AssistantMessageEvent::Done {
                reason: assistant.stop_reason,
                message: assistant,
            });
            stream
        }
    }

    impl ApiProvider for SlowEchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &cell_ai_core::Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> cell_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
            let prompt = prompt_text(context);
            let assistant = assistant_message(model, prompt);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(120));
                sender.send(AssistantMessageEvent::Done {
                    reason: assistant.stop_reason,
                    message: assistant,
                });
            });
            stream
        }
    }

    fn prompt_text(context: &Context) -> String {
        match context.messages.last() {
            Some(Message::User(UserMessage {
                content: UserContent::Text(text),
                ..
            })) => text.clone(),
            Some(Message::User(UserMessage {
                content: UserContent::Blocks(blocks),
                ..
            })) => blocks
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        }
    }

    fn assistant_message(model: &cell_ai_core::Model, prompt: String) -> AssistantMessage {
        AssistantMessage {
            content: vec![AssistantContentBlock::Text {
                text: format!("echo:{prompt}"),
                text_signature: None,
            }],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            usage: Usage {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 2,
                cost: UsageCost {
                    input: "0".to_string(),
                    output: "0".to_string(),
                    cache_read: "0".to_string(),
                    cache_write: "0".to_string(),
                    total: "0".to_string(),
                },
            },
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }

    fn rpc_session(tempdir: &std::path::Path) -> AgentSession {
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);
        create_agent_session(
            &NonInteractiveRequest {
                cwd: tempdir.to_path_buf(),
                mode: OutputMode::Rpc,
                provider: Some("openai".to_string()),
                model: Some("gpt-5.1-codex".to_string()),
                api_key: None,
                system_prompt: None,
                append_system_prompt: None,
                initial_message: None,
                messages: Vec::new(),
                continue_session: false,
                no_session: true,
                session: None,
                session_dir: None,
                models: None,
                no_tools: false,
                tools: None,
                thinking: None,
                no_skills: false,
                skills: Vec::new(),
                prompt_templates: Vec::new(),
                no_prompt_templates: false,
                themes: Vec::new(),
                no_themes: false,
            },
            &providers,
            &mut models,
        )
        .expect("create agent session")
    }

    fn slow_rpc_session(tempdir: &std::path::Path) -> AgentSession {
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(SlowEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);
        create_agent_session(
            &NonInteractiveRequest {
                cwd: tempdir.to_path_buf(),
                mode: OutputMode::Rpc,
                provider: Some("openai".to_string()),
                model: Some("gpt-5.1-codex".to_string()),
                api_key: None,
                system_prompt: None,
                append_system_prompt: None,
                initial_message: None,
                messages: Vec::new(),
                continue_session: false,
                no_session: true,
                session: None,
                session_dir: None,
                models: None,
                no_tools: false,
                tools: None,
                thinking: None,
                no_skills: false,
                skills: Vec::new(),
                prompt_templates: Vec::new(),
                no_prompt_templates: false,
                themes: Vec::new(),
                no_themes: false,
            },
            &providers,
            &mut models,
        )
        .expect("create slow agent session")
    }

    #[test]
    fn rpc_mode_processes_commands_and_emits_events() {
        let tempdir = tempdir().expect("tempdir");
        let session = rpc_session(tempdir.path());
        let input = Cursor::new(
            "{\"type\":\"get_state\",\"id\":\"1\"}\n{\"type\":\"prompt\",\"id\":\"2\",\"message\":\"hello\"}\n{\"type\":\"get_last_assistant_text\",\"id\":\"3\"}\n",
        );
        let mut output = Vec::new();

        let exit_code = run_rpc_with_io(input, &mut output, session).expect("run rpc");
        assert_eq!(exit_code, 0);

        let lines = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
            .collect::<Vec<_>>();

        assert_eq!(lines[0]["type"], "response");
        assert_eq!(lines[0]["command"], "get_state");
        assert_eq!(lines[0]["success"], true);

        assert_eq!(lines[1]["type"], "response");
        assert_eq!(lines[1]["command"], "prompt");
        assert_eq!(lines[1]["success"], true);

        assert!(lines.iter().any(|line| line["type"] == "agent_start"));
        assert!(lines.iter().any(|line| line["type"] == "agent_end"));

        let last = lines.last().expect("last rpc line");
        assert_eq!(last["type"], "response");
        assert_eq!(last["command"], "get_last_assistant_text");
        assert_eq!(last["data"]["text"], "echo:hello");
    }

    #[test]
    fn rpc_mode_reports_plugin_runtime_diagnostics() {
        let tempdir = tempdir().expect("tempdir");
        let session = rpc_session(tempdir.path());
        let input = Cursor::new("{\"type\":\"get_plugin_diagnostics\",\"id\":\"1\"}\n");
        let mut output = Vec::new();

        let exit_code = run_rpc_with_io(input, &mut output, session).expect("run rpc");
        assert_eq!(exit_code, 0);

        let lines = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
            .collect::<Vec<_>>();

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["type"], "response");
        assert_eq!(lines[0]["command"], "get_plugin_runtime_diagnostics");
        assert_eq!(lines[0]["success"], true);
        assert_eq!(lines[0]["data"]["plugins"], serde_json::json!([]));
        assert_eq!(lines[0]["data"]["warnings"], serde_json::json!([]));
    }

    #[test]
    fn rpc_mode_accepts_midstream_steer_and_resolves_following_query_after_completion() {
        let tempdir = tempdir().expect("tempdir");
        let session = slow_rpc_session(tempdir.path());
        let input = Cursor::new(
            "{\"type\":\"prompt\",\"id\":\"1\",\"message\":\"hello\"}\n\
             {\"type\":\"steer\",\"id\":\"2\",\"message\":\"redirect\"}\n\
             {\"type\":\"get_last_assistant_text\",\"id\":\"3\"}\n",
        );
        let mut output = Vec::new();

        let exit_code = run_rpc_with_io(input, &mut output, session).expect("run rpc");
        assert_eq!(exit_code, 0);

        let lines = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
            .collect::<Vec<_>>();
        assert_eq!(lines[0]["type"], "response");
        assert_eq!(lines[0]["command"], "prompt");
        assert_eq!(lines[1]["type"], "response");
        assert_eq!(lines[1]["command"], "steer");
        assert!(lines.iter().any(|line| line["type"] == "agent_end"));

        let last = lines.last().expect("last rpc line");
        assert_eq!(last["type"], "response");
        assert_eq!(last["command"], "get_last_assistant_text");
        assert_eq!(last["data"]["text"], "echo:redirect");
    }

    #[test]
    fn rpc_mode_abort_stops_active_prompt_and_persists_aborted_assistant() {
        let tempdir = tempdir().expect("tempdir");
        let session = slow_rpc_session(tempdir.path());
        let input = Cursor::new(
            "{\"type\":\"prompt\",\"id\":\"1\",\"message\":\"hello\"}\n\
             {\"type\":\"abort\",\"id\":\"2\"}\n\
             {\"type\":\"get_messages\",\"id\":\"3\"}\n",
        );
        let mut output = Vec::new();

        let exit_code = run_rpc_with_io(input, &mut output, session).expect("run rpc");
        assert_eq!(exit_code, 0);

        let lines = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
            .collect::<Vec<_>>();

        assert_eq!(lines[0]["type"], "response");
        assert_eq!(lines[0]["command"], "prompt");
        assert_eq!(lines[1]["type"], "response");
        assert_eq!(lines[1]["command"], "abort");

        let last = lines.last().expect("last rpc line");
        assert_eq!(last["type"], "response");
        assert_eq!(last["command"], "get_messages");
        let messages = last["data"]["messages"].as_array().expect("messages array");
        let assistant = messages
            .iter()
            .find(|message| message["role"] == "assistant")
            .expect("assistant message");
        assert_eq!(assistant["stopReason"], "aborted");
        assert_eq!(assistant["errorMessage"], "Request aborted");
    }
}
