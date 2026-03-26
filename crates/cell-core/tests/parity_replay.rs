use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use cell_ai_core::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message, StopReason,
    StreamOptions, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
};
use cell_ai_providers::{ApiProvider, ProviderRegistry};
use cell_config::ENV_AGENT_DIR;
use cell_core::{NonInteractiveRequest, run_non_interactive};
use cell_models::ModelRegistry;
use cell_oauth::AuthStorage;
use cell_protocol::OutputMode;
use serde_json::{Value, json};
use tempfile::tempdir;

struct EchoProvider;

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
        let prompt = match context.messages.last() {
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
        };

        let response_text = if prompt == "__system__" {
            context.system_prompt.clone().unwrap_or_default()
        } else {
            format!("echo:{prompt}")
        };

        let assistant = AssistantMessage {
            content: vec![AssistantContentBlock::Text {
                text: response_text,
                text_signature: None,
            }],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            usage: usage(),
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

fn request(
    cwd: &Path,
    mode: OutputMode,
    no_session: bool,
    session_dir: Option<PathBuf>,
) -> NonInteractiveRequest {
    NonInteractiveRequest {
        cwd: cwd.to_path_buf(),
        mode,
        provider: Some("openai".to_string()),
        model: Some("gpt-5.1-codex".to_string()),
        api_key: None,
        system_prompt: None,
        append_system_prompt: None,
        initial_message: None,
        messages: vec!["hello".to_string()],
        continue_session: false,
        no_session,
        session: None,
        session_dir,
        models: None,
        no_tools: true,
        tools: None,
        thinking: Some("off".to_string()),
        no_skills: true,
        skills: Vec::new(),
        prompt_templates: Vec::new(),
        no_prompt_templates: true,
        themes: Vec::new(),
        no_themes: true,
    }
}

fn read_fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/parity")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("fixture json")
}

fn env_guard() -> &'static Mutex<()> {
    static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    GUARD.get_or_init(|| Mutex::new(()))
}

fn normalize_runtime_json(value: Value) -> Value {
    match value {
        Value::Array(items) => {
            Value::Array(items.into_iter().map(normalize_runtime_json).collect())
        }
        Value::Object(map) => {
            let event_type = map
                .get("type")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let mut normalized = serde_json::Map::new();
            for (key, item) in map {
                let normalized_value = match key.as_str() {
                    "timestamp" => json!(0),
                    "cwd" => json!("<CWD>"),
                    "sessionId" => json!("<SESSION_ID>"),
                    "id" if event_type.as_deref() == Some("session") => json!("<SESSION_ID>"),
                    _ => normalize_runtime_json(item),
                };
                normalized.insert(key, normalized_value);
            }
            Value::Object(normalized)
        }
        other => other,
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

fn summarize_session_artifact(entries: Vec<Value>) -> Value {
    let summarized = entries
        .into_iter()
        .map(|entry| match entry {
            Value::Object(map) => match map.get("type").and_then(Value::as_str) {
                Some("session") => json!({
                    "type": "session",
                    "cwd": "<CWD>",
                }),
                Some("model_change") => json!({
                    "type": "model_change",
                    "provider": map.get("provider").cloned().unwrap_or(Value::Null),
                    "modelId": map.get("modelId").cloned().unwrap_or(Value::Null),
                }),
                Some("thinking_level_change") => json!({
                    "type": "thinking_level_change",
                    "thinkingLevel": map
                        .get("thinkingLevel")
                        .cloned()
                        .or_else(|| map.get("level").cloned())
                        .unwrap_or(Value::Null),
                }),
                Some("message") => {
                    summarize_message_entry(map.get("message").cloned().unwrap_or(Value::Null))
                }
                _ => normalize_runtime_json(Value::Object(map)),
            },
            other => normalize_runtime_json(other),
        })
        .collect::<Vec<_>>();
    json!({ "entries": summarized })
}

fn summarize_message_entry(message: Value) -> Value {
    let Value::Object(map) = message else {
        return normalize_runtime_json(message);
    };

    match map.get("role").and_then(Value::as_str) {
        Some("user") => json!({
            "type": "message",
            "role": "user",
            "text": extract_text(map.get("content")),
        }),
        Some("assistant") => json!({
            "type": "message",
            "role": "assistant",
            "stopReason": map.get("stopReason").cloned().unwrap_or(Value::Null),
            "text": extract_text(map.get("content")),
        }),
        Some("toolResult") => json!({
            "type": "message",
            "role": "toolResult",
            "toolName": map.get("toolName").cloned().unwrap_or(Value::Null),
            "isError": map.get("isError").cloned().unwrap_or(Value::Bool(false)),
            "text": extract_text(map.get("content")),
        }),
        _ => normalize_runtime_json(Value::Object(map)),
    }
}

fn extract_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                Value::Object(map) if map.get("type").and_then(Value::as_str) == Some("text") => {
                    map.get("text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn normalize_path_text(value: &str, cwd: &Path, temp_root: &Path) -> String {
    value
        .replace(&format!("/private{}", cwd.to_string_lossy()), "<CWD>")
        .replace(&format!("/private{}", temp_root.to_string_lossy()), "<TMP>")
        .replace(&cwd.to_string_lossy().to_string(), "<CWD>")
        .replace(&temp_root.to_string_lossy().to_string(), "<TMP>")
}

fn normalize_system_prompt_text(value: &str, cwd: &Path, temp_root: &Path) -> String {
    normalize_path_text(value, cwd, temp_root)
        .lines()
        .map(|line| {
            if line.starts_with("Current date and time: ") {
                "Current date and time: <NOW>".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn print_text_matches_upstream_fixture() {
    let tempdir = tempdir().expect("tempdir");
    let (providers, mut models) = registry_with_provider();
    let result = run_non_interactive(
        request(tempdir.path(), OutputMode::Text, true, None),
        &providers,
        &mut models,
    )
    .await
    .expect("runtime result");
    let fixture = read_fixture("print-text.json");

    assert_eq!(
        json!({
            "stdoutLines": result.stdout,
            "stderrLines": result.stderr,
        }),
        fixture
    );
}

#[tokio::test]
async fn print_json_matches_upstream_fixture() {
    let tempdir = tempdir().expect("tempdir");
    let (providers, mut models) = registry_with_provider();
    let result = run_non_interactive(
        request(tempdir.path(), OutputMode::Json, true, None),
        &providers,
        &mut models,
    )
    .await
    .expect("runtime result");
    let fixture = read_fixture("print-json.json");

    let lines = result
        .stdout
        .into_iter()
        .map(|line| serde_json::from_str::<Value>(&line).expect("json line"))
        .map(normalize_runtime_json)
        .collect::<Vec<_>>();

    assert_eq!(
        json!({
            "lines": lines,
            "stderrLines": result.stderr,
        }),
        fixture
    );
}

#[tokio::test]
async fn session_artifact_matches_upstream_fixture() {
    let tempdir = tempdir().expect("tempdir");
    let session_dir = tempdir.path().join("sessions");
    let (providers, mut models) = registry_with_provider();
    run_non_interactive(
        request(
            tempdir.path(),
            OutputMode::Text,
            false,
            Some(session_dir.clone()),
        ),
        &providers,
        &mut models,
    )
    .await
    .expect("runtime result");

    let session_file = find_session_file(&session_dir).expect("session file");
    let entries = std::fs::read_to_string(session_file)
        .expect("session jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("session entry"))
        .collect::<Vec<_>>();

    assert_eq!(
        summarize_session_artifact(entries),
        read_fixture("session-artifact.json")
    );
}

#[tokio::test]
async fn resource_precedence_matches_upstream_fixture() {
    let _guard = env_guard().lock().expect("env guard");
    let tempdir = tempdir().expect("tempdir");
    let temp_root = tempdir.path();
    let cwd = temp_root.join("workspace").join("app");
    let agent_dir = temp_root.join("agent");
    let fake_home = temp_root.join("home");
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::create_dir_all(&fake_home).expect("home");
    std::fs::write(agent_dir.join("SYSTEM.md"), "global system").expect("global system");
    std::fs::write(temp_root.join("AGENTS.md"), "root instructions").expect("root agents");
    std::fs::write(cwd.join("AGENTS.md"), "cwd instructions").expect("cwd agents");
    std::fs::create_dir_all(cwd.join(".pi").join("prompts")).expect("prompt dir");
    std::fs::create_dir_all(cwd.join(".pi").join("skills").join("checks")).expect("skills dir");
    std::fs::write(cwd.join(".pi").join("APPEND_SYSTEM.md"), "project append")
        .expect("append system");
    std::fs::write(
        cwd.join(".pi").join("prompts").join("review.md"),
        "---\ndescription: Review a target\n---\nReview $1 with $2",
    )
    .expect("prompt");
    std::fs::write(
        cwd.join(".pi")
            .join("skills")
            .join("checks")
            .join("SKILL.md"),
        "---\nname: checks\ndescription: Run verification checks\n---\nUse the checks skill.",
    )
    .expect("skill");

    let original_home = std::env::var_os("HOME");
    let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
    unsafe { std::env::set_var("HOME", &fake_home) };
    unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

    let (providers, mut models) = registry_with_provider();
    let mut session = cell_core::create_agent_session(
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
            no_tools: true,
            tools: None,
            thinking: Some("off".to_string()),
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

    let commands = session
        .get_commands()
        .into_iter()
        .map(|command| {
            json!({
                "name": command.name,
                "description": command.description,
                "source": command.source,
                "location": command.location,
                "path": command.path.map(|path| normalize_path_text(&path, &cwd, temp_root)),
            })
        })
        .collect::<Vec<_>>();

    let prompt_run = session
        .prompt_text("__system__".to_string())
        .await
        .expect("prompt system");
    let system_prompt = prompt_run
        .assistant_message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    match original_home {
        Some(value) => unsafe { std::env::set_var("HOME", value) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    match original_agent_dir {
        Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
        None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
    }

    assert_eq!(
        json!({
            "commands": commands,
            "systemPrompt": normalize_system_prompt_text(&system_prompt, &cwd, temp_root),
        }),
        normalize_runtime_json(read_fixture("resource-precedence.json"))
    );
}
