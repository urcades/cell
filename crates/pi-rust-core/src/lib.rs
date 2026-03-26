mod agent_core;
mod agent_session;
mod export_html;
mod runtime_resources;
mod system_prompt;

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use pi_rust_ai_core::{AssistantContentBlock, Model, StopReason};
use pi_rust_ai_providers::{ProviderRegistry, ProviderRegistryError};
use pi_rust_config::{SettingsManager, SettingsManagerError};
use pi_rust_models::{
    ModelRegistry, ScopedModel, default_model_for_provider, resolve_cli_model,
    resolve_known_cli_model,
};
use pi_rust_oauth::AuthSource;
use pi_rust_protocol::OutputMode;
use pi_rust_session::{SessionManager, SessionManagerError};
use pi_rust_tools::ToolSet;
use thiserror::Error;

pub use agent_core::{
    Agent, AgentControl, AgentEvent, AgentState, THINKING_LEVELS, is_valid_thinking_level,
};
pub use agent_session::{
    AgentSession, ForkableUserMessage, ModelCycleResult, PromptRun, StartupResourceNotice,
    StartupResourceNoticeSection, StartupResourceSummary, build_scoped_models,
    rpc_event_from_agent_event,
};
pub use pi_rust_session::SessionTreeNode;
use runtime_resources::{
    load_session_runtime_resources_with_settings, runtime_config_from_request,
};

#[derive(Clone, Debug)]
pub struct NonInteractiveRequest {
    pub cwd: PathBuf,
    pub mode: OutputMode,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub initial_message: Option<String>,
    pub messages: Vec<String>,
    pub continue_session: bool,
    pub no_session: bool,
    pub session: Option<PathBuf>,
    pub session_dir: Option<PathBuf>,
    pub models: Option<Vec<String>>,
    pub no_tools: bool,
    pub tools: Option<Vec<String>>,
    pub thinking: Option<String>,
    pub no_skills: bool,
    pub skills: Vec<PathBuf>,
    pub prompt_templates: Vec<PathBuf>,
    pub no_prompt_templates: bool,
    pub themes: Vec<PathBuf>,
    pub no_themes: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeOutput {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub exit_code: i32,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Provider(#[from] ProviderRegistryError),
    #[error(transparent)]
    Session(#[from] SessionManagerError),
    #[error(transparent)]
    Settings(#[from] SettingsManagerError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Export(#[from] export_html::ExportError),
}

#[cfg(test)]
pub(crate) fn test_env_guard() -> &'static Mutex<()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(()))
}

pub async fn run_non_interactive(
    request: NonInteractiveRequest,
    provider_registry: &ProviderRegistry,
    model_registry: &mut ModelRegistry,
) -> Result<RuntimeOutput, RuntimeError> {
    let mut output = RuntimeOutput::default();
    let mut pending_messages = Vec::new();
    if let Some(initial_message) = request.initial_message.clone() {
        pending_messages.push(initial_message);
    }
    pending_messages.extend(request.messages.clone());

    if pending_messages.is_empty() {
        output.stderr.push("No prompt provided.".to_string());
        output.exit_code = 1;
        return Ok(output);
    }

    let mut agent_session = match create_agent_session(&request, provider_registry, model_registry)
    {
        Ok(agent_session) => agent_session,
        Err(RuntimeError::Message(message)) => {
            output.stderr.push(message);
            output.exit_code = 1;
            return Ok(output);
        }
        Err(error) => return Err(error),
    };

    if request.mode == OutputMode::Json {
        output
            .stdout
            .push(serde_json::to_string(agent_session.session().get_header())?);
    }

    let mut last_assistant_message = None;
    for prompt in pending_messages {
        let run = agent_session.prompt_text_as_blocks(prompt).await?;
        if request.mode == OutputMode::Json {
            for event in run.events {
                output
                    .stdout
                    .push(serde_json::to_string(&rpc_event_from_agent_event(event))?);
            }
        }
        last_assistant_message = Some(run.assistant_message);
    }

    if request.mode == OutputMode::Text {
        if let Some(message) = last_assistant_message {
            render_text_mode_response(&message, &mut output);
        }
    }

    Ok(output)
}

pub fn create_agent_session(
    request: &NonInteractiveRequest,
    provider_registry: &ProviderRegistry,
    model_registry: &mut ModelRegistry,
) -> Result<AgentSession, RuntimeError> {
    let settings_manager = SettingsManager::create(&request.cwd, None);

    if let Some(api_key) = &request.api_key {
        if request.model.is_none() {
            return Err(RuntimeError::Message(
                "--api-key requires a model to be specified via --model, --provider/--model, or --models"
                    .to_string(),
            ));
        }

        let provider = resolve_known_cli_model(
            request.provider.as_deref(),
            request.model.as_deref(),
            model_registry,
        )
        .model
        .map(|model| model.provider.0)
        .ok_or_else(|| {
            RuntimeError::Message("Failed to resolve model for --api-key override".to_string())
        })?;
        model_registry
            .auth_storage_mut()
            .set_runtime_api_key(provider, api_key.clone());
    }

    let mut session = create_session_manager(request)?;
    let session_is_empty = session.get_entries().is_empty();
    let scoped_model_patterns = request
        .models
        .clone()
        .or_else(|| settings_manager.get_enabled_models(None));
    let scoped_models = scoped_model_patterns
        .as_ref()
        .map(|patterns| build_scoped_models(patterns, model_registry))
        .unwrap_or_default();
    let model = resolve_execution_model(
        request,
        model_registry,
        Some(&settings_manager),
        &scoped_models,
        session_is_empty,
    )?;
    let enabled_tool_names = resolve_enabled_tools(request);
    let mut tool_set = ToolSet::with_enabled_names_and_plugins(
        &request.cwd,
        &enabled_tool_names,
        request.tools.is_none() && !request.no_tools,
    );
    let runtime_config = runtime_config_from_request(request);
    let resources = load_session_runtime_resources_with_settings(
        &runtime_config,
        settings_manager.clone(),
        &enabled_tool_names,
    );
    if let Some(plugin_runtime) = &resources.plugin_runtime {
        tool_set.attach_plugin_runtime(plugin_runtime.clone());
    }
    let thinking_level = request
        .thinking
        .clone()
        .or_else(|| {
            if session_is_empty {
                scoped_models
                    .first()
                    .and_then(|scoped| scoped.thinking_level.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "off".to_string());

    if session_is_empty {
        session.append_model_change(&model.provider.0, &model.id)?;
        session.append_thinking_level_change(&thinking_level)?;
    }

    let mut agent_session = AgentSession::new(
        provider_registry.clone(),
        model_registry.clone(),
        session,
        tool_set,
        model,
        thinking_level,
        Some(resources.system_prompt),
        scoped_models,
        resources.prompt_templates,
        resources.skills,
    );
    agent_session.attach_settings_manager(settings_manager);
    agent_session.attach_runtime_resources(
        runtime_config,
        enabled_tool_names,
        resources.themes,
        resources.plugin_runtime,
        resources.plugin_startup_summary,
        resources.startup_summary,
    );
    Ok(agent_session)
}

pub fn session_manager_for_request(
    request: &NonInteractiveRequest,
) -> Result<SessionManager, SessionManagerError> {
    create_session_manager(request)
}

pub fn export_session_file_to_html(
    input_path: impl AsRef<Path>,
    output_path: Option<&Path>,
) -> Result<PathBuf, RuntimeError> {
    let session = SessionManager::open(input_path.as_ref())?;
    Ok(export_html::export_session_to_html(
        &session,
        output_path,
        None,
    )?)
}

pub fn list_models(model_registry: &ModelRegistry, search: Option<&str>) -> String {
    let search = search.map(|value| value.to_lowercase());
    let mut models = model_registry.get_available();
    models.sort_by(|left, right| {
        format!("{}/{}", left.provider.0, left.id)
            .cmp(&format!("{}/{}", right.provider.0, right.id))
    });

    models
        .into_iter()
        .filter(|model| {
            search.as_ref().is_none_or(|search| {
                let provider_id = format!("{}/{}", model.provider.0, model.id).to_lowercase();
                provider_id.contains(search) || model.name.to_lowercase().contains(search)
            })
        })
        .map(|model| format!("{}/{}", model.provider.0, model.id))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn list_known_models(model_registry: &ModelRegistry, search: Option<&str>) -> String {
    let search = search.map(|value| value.to_lowercase());
    let mut models = model_registry.known_models_with_auth();
    models.sort_by(|left, right| {
        format!("{}/{}", left.model.provider.0, left.model.id)
            .cmp(&format!("{}/{}", right.model.provider.0, right.model.id))
    });

    models
        .into_iter()
        .filter(|status| {
            search.as_ref().is_none_or(|search| {
                let provider_id =
                    format!("{}/{}", status.model.provider.0, status.model.id).to_lowercase();
                provider_id.contains(search) || status.model.name.to_lowercase().contains(search)
            })
        })
        .map(|status| {
            format!(
                "{}/{} [{}]",
                status.model.provider.0,
                status.model.id,
                auth_source_marker(&status.auth_source)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn auth_source_marker(source: &AuthSource) -> &'static str {
    match source {
        AuthSource::RuntimeOverride => "runtime",
        AuthSource::StoredApiKey => "stored-api-key",
        AuthSource::StoredOAuth => "stored-oauth",
        AuthSource::Environment => "env",
        AuthSource::Fallback => "fallback",
        AuthSource::Missing => "missing",
    }
}

fn render_text_mode_response(
    assistant_message: &pi_rust_ai_core::AssistantMessage,
    output: &mut RuntimeOutput,
) {
    if matches!(
        assistant_message.stop_reason,
        StopReason::Error | StopReason::Aborted
    ) {
        output.stderr.push(
            assistant_message
                .error_message
                .clone()
                .unwrap_or_else(|| format!("Request {:?}", assistant_message.stop_reason)),
        );
        output.exit_code = 1;
        return;
    }

    let text = assistant_message
        .content
        .iter()
        .filter_map(|content| match content {
            AssistantContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    output.stdout.push(text);
}

fn resolve_enabled_tools(request: &NonInteractiveRequest) -> Vec<String> {
    if request.no_tools {
        Vec::new()
    } else if let Some(names) = &request.tools {
        names.clone()
    } else {
        vec![
            "read".to_string(),
            "bash".to_string(),
            "edit".to_string(),
            "write".to_string(),
        ]
    }
}

fn create_session_manager(
    request: &NonInteractiveRequest,
) -> Result<SessionManager, SessionManagerError> {
    if request.no_session {
        return Ok(SessionManager::in_memory(&request.cwd));
    }
    if let Some(path) = &request.session {
        return SessionManager::open(path);
    }
    if request.continue_session {
        return SessionManager::continue_recent(&request.cwd, request.session_dir.clone());
    }
    SessionManager::create(&request.cwd, request.session_dir.clone())
}

fn resolve_execution_model(
    request: &NonInteractiveRequest,
    model_registry: &ModelRegistry,
    settings_manager: Option<&SettingsManager>,
    scoped_models: &[ScopedModel],
    prefer_scoped_model: bool,
) -> Result<Model, RuntimeError> {
    let resolved = resolve_cli_model(
        request.provider.as_deref(),
        request.model.as_deref(),
        model_registry,
    );
    if let Some(error) = resolved.error {
        return Err(RuntimeError::Message(error));
    }
    if let Some(result) = resolved.model {
        return Ok(result);
    }

    if prefer_scoped_model {
        if let Some(scoped_model) = scoped_models.first() {
            return Ok(scoped_model.model.clone());
        }
    }

    if let Some(settings_manager) = settings_manager {
        if let (Some(provider), Some(model_id)) = (
            settings_manager.get_default_provider(),
            settings_manager.get_default_model(),
        ) {
            if let Some(model) = model_registry
                .get_available()
                .into_iter()
                .find(|model| model.provider.0 == provider && model.id == model_id)
            {
                return Ok(model);
            }
        }
    }

    let available_models = model_registry.get_available();
    if available_models.is_empty() {
        return Err(RuntimeError::Message(
            "No models available.\nSet an API key environment variable or create models.json."
                .to_string(),
        ));
    }

    for provider in ["anthropic", "openai", "openai-codex", "openrouter"] {
        if let Some(default_model_id) = default_model_for_provider(provider) {
            if let Some(model) = available_models
                .iter()
                .find(|model| model.provider.0 == provider && model.id == default_model_id)
            {
                return Ok(model.clone());
            }
        }
    }

    Ok(available_models[0].clone())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use pi_rust_ai_core::{
        ApiId, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, ModelCost,
        ModelInput, ProviderId, StopReason, StreamOptions, Usage, UsageCost, UserContent,
        UserMessage,
    };
    use pi_rust_ai_providers::{ApiProvider, ProviderRegistry};
    use pi_rust_config::ENV_AGENT_DIR;
    use pi_rust_models::ModelRegistry;
    use pi_rust_oauth::AuthStorage;
    use pi_rust_protocol::OutputMode;
    use tempfile::tempdir;

    use super::{
        NonInteractiveRequest, create_agent_session, export_session_file_to_html,
        list_known_models, list_models, run_non_interactive,
    };

    struct EchoProvider;

    impl ApiProvider for EchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &pi_rust_ai_core::Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> pi_rust_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = pi_rust_ai_core::AssistantMessageEventStream::new();
            let last_message = context.messages.last().cloned();
            let assistant = if let Some(pi_rust_ai_core::Message::ToolResult(tool_result)) =
                last_message
            {
                AssistantMessage {
                    content: vec![AssistantContentBlock::Text {
                        text: format!(
                            "tool:{}:{}",
                            tool_result.tool_name,
                            match &tool_result.content[0] {
                                pi_rust_ai_core::UserContentBlock::Text { text, .. } =>
                                    text.clone(),
                                _ => "binary".to_string(),
                            }
                        ),
                        text_signature: None,
                    }],
                    api: model.api.clone(),
                    provider: model.provider.clone(),
                    model: model.id.clone(),
                    usage: usage(),
                    stop_reason: StopReason::Stop,
                    error_message: None,
                    timestamp: 0,
                }
            } else {
                let user_message = context.messages.last().expect("user message");
                let prompt = match user_message {
                    pi_rust_ai_core::Message::User(UserMessage {
                        content: UserContent::Text(text),
                        ..
                    }) => text.clone(),
                    pi_rust_ai_core::Message::User(UserMessage {
                        content: UserContent::Blocks(blocks),
                        ..
                    }) => blocks
                        .iter()
                        .filter_map(|block| match block {
                            pi_rust_ai_core::UserContentBlock::Text { text, .. } => {
                                Some(text.clone())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(""),
                    _ => String::new(),
                };
                if prompt.contains("call-tool") {
                    AssistantMessage {
                        content: vec![AssistantContentBlock::ToolCall {
                            id: "tool-1".to_string(),
                            name: "write".to_string(),
                            arguments: serde_json::json!({"path":"result.txt","content":"written"}),
                            thought_signature: None,
                        }],
                        api: model.api.clone(),
                        provider: model.provider.clone(),
                        model: model.id.clone(),
                        usage: usage(),
                        stop_reason: StopReason::ToolUse,
                        error_message: None,
                        timestamp: 0,
                    }
                } else {
                    AssistantMessage {
                        content: vec![AssistantContentBlock::Text {
                            text: format!("echo:{prompt}"),
                            text_signature: None,
                        }],
                        api: model.api.clone(),
                        provider: model.provider.clone(),
                        model: model.id.clone(),
                        usage: usage(),
                        stop_reason: StopReason::Stop,
                        error_message: None,
                        timestamp: 0,
                    }
                }
            };
            sender.send(AssistantMessageEvent::Done {
                reason: assistant.stop_reason,
                message: assistant,
            });
            stream
        }
    }

    fn usage() -> Usage {
        Usage {
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
        }
    }

    fn registry_with_provider() -> (ProviderRegistry, ModelRegistry) {
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));

        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        (providers, models)
    }

    fn session_file_request(cwd: &Path) -> NonInteractiveRequest {
        NonInteractiveRequest {
            cwd: cwd.to_path_buf(),
            mode: OutputMode::Text,
            provider: Some("openai".to_string()),
            model: Some("gpt-5.1-codex".to_string()),
            api_key: None,
            system_prompt: None,
            append_system_prompt: None,
            initial_message: None,
            messages: vec!["hello".to_string()],
            continue_session: false,
            no_session: false,
            session: None,
            session_dir: Some(cwd.join("sessions")),
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
        }
    }

    fn find_session_file(dir: &Path) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_session_file(&path) {
                    return Some(found);
                }
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                return Some(path);
            }
        }
        None
    }

    #[tokio::test]
    async fn runs_text_mode_prompt() {
        let tempdir = tempdir().expect("tempdir");
        let (providers, mut models) = registry_with_provider();

        let result = run_non_interactive(
            session_file_request(tempdir.path()),
            &providers,
            &mut models,
        )
        .await
        .expect("runtime result");

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, vec!["echo:hello".to_string()]);
    }

    #[tokio::test]
    async fn runs_tool_loop_and_emits_json_events() {
        let tempdir = tempdir().expect("tempdir");
        let (providers, mut models) = registry_with_provider();

        let result = run_non_interactive(
            NonInteractiveRequest {
                cwd: tempdir.path().to_path_buf(),
                mode: OutputMode::Json,
                provider: Some("openai".to_string()),
                model: Some("gpt-5.1-codex".to_string()),
                api_key: None,
                system_prompt: None,
                append_system_prompt: None,
                initial_message: None,
                messages: vec!["call-tool".to_string()],
                continue_session: false,
                no_session: false,
                session: None,
                session_dir: Some(tempdir.path().join("sessions")),
                models: None,
                no_tools: false,
                tools: Some(vec!["write".to_string()]),
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
        .await
        .expect("runtime result");

        assert!(
            result
                .stdout
                .iter()
                .any(|line| line.contains("\"type\":\"session\""))
        );
        assert!(
            result
                .stdout
                .iter()
                .any(|line| line.contains("\"tool_execution_end\""))
        );
        let written =
            std::fs::read_to_string(tempdir.path().join("result.txt")).expect("written file");
        assert_eq!(written, "written");
    }

    #[tokio::test]
    async fn exports_persisted_session_to_html() {
        let tempdir = tempdir().expect("tempdir");
        let (providers, mut models) = registry_with_provider();
        run_non_interactive(
            session_file_request(tempdir.path()),
            &providers,
            &mut models,
        )
        .await
        .expect("runtime result");

        let session_dir = tempdir.path().join("sessions");
        let session_file = find_session_file(&session_dir).expect("session file");

        let output_path = export_session_file_to_html(&session_file, None).expect("export");
        let html = std::fs::read_to_string(output_path).expect("html");
        assert!(html.contains("echo:hello"));
    }

    #[test]
    fn lists_available_models_only() {
        let (_providers, models) = registry_with_provider();
        let output = list_models(&models, Some("openai"));
        assert!(output.contains("openai/gpt-5.1-codex"));
        assert!(!output.contains("openai-codex/gpt-5.3-codex"));
        assert!(!output.contains("["));
    }

    #[test]
    fn lists_known_models_with_auth_markers() {
        let (_providers, models) = registry_with_provider();
        let output = list_known_models(&models, Some("openai"));
        assert!(output.contains("openai/gpt-5.1-codex [runtime]"));
        assert!(output.contains("openai-codex/gpt-5.3-codex [missing]"));
    }

    #[test]
    fn model_fixture_matches_registry_shape() {
        let (_providers, models) = registry_with_provider();
        let model = models.find("openai", "gpt-5.1-codex").expect("model");
        assert_eq!(model.id, "gpt-5.1-codex");
        assert_eq!(model.name, "GPT-5.1 Codex");
        assert_eq!(model.api, ApiId::new("openai-responses"));
        assert_eq!(model.provider, ProviderId::new("openai"));
        assert_eq!(model.base_url, "https://api.openai.com/v1");
        assert!(model.reasoning);
        assert_eq!(model.input, vec![ModelInput::Text, ModelInput::Image]);
        assert_eq!(
            model.cost,
            ModelCost {
                input: 1.25,
                output: 10.0,
                cache_read: 0.125,
                cache_write: 0.0,
            }
        );
        assert_eq!(model.context_window, 400_000);
        assert_eq!(model.max_tokens, 128_000);
        assert!(model.headers.is_none());
        assert!(model.compat.is_none());
    }

    #[test]
    fn create_agent_session_uses_saved_default_model_when_cli_model_is_omitted() {
        let _guard = crate::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        fs::create_dir_all(&agent_dir).expect("create agent dir");
        fs::write(
            agent_dir.join("settings.json"),
            r#"{"defaultProvider":"anthropic","defaultModel":"claude-opus-4-6"}"#,
        )
        .expect("write settings");
        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        auth.set_runtime_api_key("anthropic", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);

        let session = create_agent_session(
            &NonInteractiveRequest {
                cwd,
                mode: OutputMode::Text,
                provider: None,
                model: None,
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
                tools: Some(vec!["read".to_string()]),
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
        .expect("create session");

        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(session.current_model().provider.0, "anthropic");
        assert_eq!(session.current_model().id, "claude-opus-4-6");
    }

    #[test]
    fn create_agent_session_uses_saved_enabled_models_for_initial_scope() {
        let _guard = crate::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        fs::create_dir_all(&agent_dir).expect("create agent dir");
        fs::write(
            agent_dir.join("settings.json"),
            r#"{"enabledModels":["openrouter/openai/gpt-5.1-codex:high"]}"#,
        )
        .expect("write settings");
        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        auth.set_runtime_api_key("openrouter", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);

        let session = create_agent_session(
            &NonInteractiveRequest {
                cwd,
                mode: OutputMode::Text,
                provider: None,
                model: None,
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
                tools: Some(vec!["read".to_string()]),
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
        .expect("create session");

        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(session.current_model().provider.0, "openrouter");
        assert_eq!(session.current_model().id, "openai/gpt-5.1-codex");
        assert_eq!(session.current_thinking_level(), "high");
    }
}
