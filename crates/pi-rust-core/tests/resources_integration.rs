use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use pi_rust_ai_core::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message, Model,
    StopReason, StreamOptions, Usage, UsageCost, UserContent, UserMessage,
};
use pi_rust_ai_providers::{ApiProvider, ProviderRegistry};
use pi_rust_config::{ENV_AGENT_DIR, PROJECT_CONFIG_DIR_NAME};
use pi_rust_core::{NonInteractiveRequest, create_agent_session};
use pi_rust_models::ModelRegistry;
use pi_rust_oauth::AuthStorage;
use pi_rust_protocol::OutputMode;
use tempfile::tempdir;

struct ContextEchoProvider;

impl ApiProvider for ContextEchoProvider {
    fn api(&self) -> &'static str {
        "openai-responses"
    }

    fn stream(
        &self,
        model: &Model,
        context: &Context,
        _options: Option<StreamOptions>,
    ) -> pi_rust_ai_core::AssistantMessageEventStream {
        let (mut sender, stream) = pi_rust_ai_core::AssistantMessageEventStream::new();
        let response_text = match context.messages.last() {
            Some(Message::User(UserMessage {
                content: UserContent::Text(text),
                ..
            })) if text == "__system__" => context.system_prompt.clone().unwrap_or_default(),
            Some(Message::User(UserMessage {
                content: UserContent::Text(text),
                ..
            })) => text.clone(),
            _ => String::new(),
        };
        let assistant = AssistantMessage {
            content: vec![AssistantContentBlock::Text {
                text: response_text,
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
        };
        sender.send(AssistantMessageEvent::Done {
            reason: assistant.stop_reason,
            message: assistant,
        });
        stream
    }
}

fn env_guard() -> &'static Mutex<()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(()))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, content).expect("write file");
}

#[tokio::test]
async fn create_agent_session_loads_resources_and_system_prompt() {
    let _guard = env_guard().lock().expect("env guard");
    let tempdir = tempdir().expect("tempdir");
    let workspace = tempdir.path().join("workspace");
    let cwd = workspace.join("project").join("app");
    let agent_dir = tempdir.path().join("agent");

    write_file(&agent_dir.join("SYSTEM.md"), "global system");
    write_file(&workspace.join("AGENTS.md"), "root instructions");
    write_file(&cwd.join("AGENTS.md"), "cwd instructions");
    write_file(
        &cwd.join(PROJECT_CONFIG_DIR_NAME).join("APPEND_SYSTEM.md"),
        "project append",
    );
    write_file(
        &cwd.join(PROJECT_CONFIG_DIR_NAME)
            .join("prompts")
            .join("review.md"),
        "---\ndescription: Review a target\n---\nReview $1 with $2",
    );
    write_file(
        &cwd.join(PROJECT_CONFIG_DIR_NAME)
            .join("skills")
            .join("checks")
            .join("SKILL.md"),
        "---\nname: checks\ndescription: Run verification checks\n---\nUse the checks skill.",
    );

    let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
    unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

    let mut providers = ProviderRegistry::new();
    providers.register(Arc::new(ContextEchoProvider));
    let mut auth = AuthStorage::in_memory(Default::default());
    auth.set_runtime_api_key("openai", "runtime-key");
    let mut models = ModelRegistry::new(auth, None);

    let mut session = create_agent_session(
        &NonInteractiveRequest {
            cwd: cwd.clone(),
            mode: OutputMode::Text,
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
    .expect("create agent session");

    match original_agent_dir {
        Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
        None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
    }

    let commands = session.get_commands();
    assert!(
        commands.iter().any(|command| command.name == "review"),
        "commands: {commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|command| command.name == "skill:checks"),
        "commands: {commands:?}"
    );

    let prompt_run = session
        .prompt_text("/review src/lib.rs strict".to_string())
        .await
        .expect("prompt run");
    assert_eq!(
        prompt_run.assistant_message.content,
        vec![AssistantContentBlock::Text {
            text: "Review src/lib.rs with strict".to_string(),
            text_signature: None,
        }]
    );

    let system_run = session
        .prompt_text("__system__".to_string())
        .await
        .expect("system run");
    let system_prompt = match &system_run.assistant_message.content[0] {
        AssistantContentBlock::Text { text, .. } => text,
        other => panic!("unexpected content: {other:?}"),
    };
    assert!(system_prompt.contains("global system"));
    assert!(system_prompt.contains("project append"));
    assert!(system_prompt.contains("root instructions"));
    assert!(system_prompt.contains("cwd instructions"));
    assert!(system_prompt.contains("<available_skills>"));
    assert!(system_prompt.contains(cwd.to_string_lossy().as_ref()));
}
