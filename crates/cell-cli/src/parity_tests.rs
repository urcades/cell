use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cell_ai_core::{
    AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message, StopReason,
    StreamOptions, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
};
use cell_ai_providers::{ApiProvider, ProviderRegistry};
use cell_config::ENV_AGENT_DIR;
use cell_core::{
    AgentSession, NonInteractiveRequest, create_agent_session, run_non_interactive,
};
use cell_models::ModelRegistry;
use cell_oauth::AuthStorage;
use cell_protocol::OutputMode;
use serde_json::{Value, json};
use tempfile::tempdir;

use super::{RunResult, run, run_rpc_with_io};

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

        let assistant = AssistantMessage {
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

fn rpc_session(tempdir: &Path) -> AgentSession {
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
            no_tools: true,
            tools: None,
            thinking: Some("off".to_string()),
            no_skills: true,
            skills: Vec::new(),
            prompt_templates: Vec::new(),
            no_prompt_templates: true,
            themes: Vec::new(),
            no_themes: true,
        },
        &providers,
        &mut models,
    )
    .expect("create agent session")
}

fn read_fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/parity")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("fixture json")
}

fn normalize_path_value(value: &str, cwd: &Path, temp_root: &Path) -> String {
    value
        .replace(&format!("/private{}", cwd.to_string_lossy()), "<CWD>")
        .replace(&format!("/private{}", temp_root.to_string_lossy()), "<TMP>")
        .replace(&cwd.to_string_lossy().to_string(), "<CWD>")
        .replace(&temp_root.to_string_lossy().to_string(), "<TMP>")
}

fn normalize_run_result(result: RunResult, cwd: &Path, temp_root: &Path) -> Value {
    match result {
        RunResult::Completed {
            exit_code,
            stdout,
            stderr,
        } => json!({
            "exitCode": exit_code,
            "stdoutLines": stdout
                .unwrap_or_default()
                .split('\n')
                .filter(|line| !line.is_empty())
                .map(|line| normalize_path_value(line, cwd, temp_root))
                .collect::<Vec<_>>(),
            "stderrLines": stderr
                .unwrap_or_default()
                .split('\n')
                .filter(|line| !line.is_empty())
                .map(|line| normalize_path_value(line, cwd, temp_root))
                .collect::<Vec<_>>(),
        }),
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

fn normalize_rpc_value(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(normalize_rpc_value).collect()),
        Value::Object(map) => {
            let mut normalized = serde_json::Map::new();
            for (key, item) in map {
                let normalized_value = match key.as_str() {
                    "sessionId" => json!("<SESSION_ID>"),
                    _ => normalize_rpc_value(item),
                };
                normalized.insert(key, normalized_value);
            }
            Value::Object(normalized)
        }
        other => other,
    }
}

#[test]
fn rpc_transcript_matches_upstream_fixture() {
    let tempdir = tempdir().expect("tempdir");
    let session = rpc_session(tempdir.path());
    let input = Cursor::new(
        "{\"type\":\"get_state\",\"id\":\"1\"}\n\
         {\"type\":\"prompt\",\"id\":\"2\",\"message\":\"hello\"}\n\
         {\"type\":\"get_last_assistant_text\",\"id\":\"3\"}\n",
    );
    let mut output = Vec::new();

    let exit_code = run_rpc_with_io(input, &mut output, session).expect("run rpc");
    assert_eq!(exit_code, 0);

    let lines = String::from_utf8(output)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json line"))
        .map(normalize_rpc_value)
        .collect::<Vec<_>>();

    assert_eq!(
        json!({
            "lines": lines,
            "stderrLines": [],
        }),
        read_fixture("rpc.json")
    );
}

#[test]
fn rpc_image_transcript_matches_upstream_fixture() {
    let tempdir = tempdir().expect("tempdir");
    let session = rpc_session(tempdir.path());
    let input = Cursor::new(
        "{\"type\":\"prompt\",\"id\":\"1\",\"message\":\"see image\",\"images\":[{\"type\":\"image\",\"data\":\"ZmFrZQ==\",\"mimeType\":\"image/png\"}]}\n\
         {\"type\":\"get_messages\",\"id\":\"2\"}\n",
    );
    let mut output = Vec::new();

    let exit_code = run_rpc_with_io(input, &mut output, session).expect("run rpc");
    assert_eq!(exit_code, 0);

    let lines = String::from_utf8(output)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json line"))
        .map(normalize_rpc_value)
        .collect::<Vec<_>>();

    assert_eq!(
        json!({
            "lines": lines,
            "stderrLines": [],
        }),
        read_fixture("rpc-images.json")
    );
}

#[test]
fn rpc_bash_transcript_matches_upstream_fixture() {
    let tempdir = tempdir().expect("tempdir");
    let session = rpc_session(tempdir.path());
    let input = Cursor::new("{\"type\":\"bash\",\"id\":\"1\",\"command\":\"printf 'hello'\"}\n");
    let mut output = Vec::new();

    let exit_code = run_rpc_with_io(input, &mut output, session).expect("run rpc");
    assert_eq!(exit_code, 0);

    let lines = String::from_utf8(output)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json line"))
        .map(normalize_rpc_value)
        .collect::<Vec<_>>();

    assert_eq!(
        json!({
            "lines": lines,
            "stderrLines": [],
        }),
        read_fixture("rpc-bash.json")
    );
}

#[test]
fn package_commands_match_upstream_fixture() {
    let _guard = super::test_env_guard().lock().expect("env guard");
    let tempdir = tempdir().expect("tempdir");
    let cwd = tempdir.path().join("workspace").join("app");
    let agent_dir = tempdir.path().join("agent");
    std::fs::create_dir_all(cwd.join("pkg")).expect("pkg dir");
    std::fs::create_dir_all(cwd.join("npm-pkg")).expect("npm pkg dir");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::write(cwd.join("pkg").join("README.md"), "local package").expect("local readme");
    std::fs::write(
        cwd.join("npm-pkg").join("package.json"),
        r#"{"name":"fixture-pkg","version":"1.0.0"}"#,
    )
    .expect("package json");
    std::fs::write(
        cwd.join("npm-pkg").join("index.js"),
        "module.exports = 1;\n",
    )
    .expect("index");

    let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
    let original_cwd = std::env::current_dir().expect("cwd");
    unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };
    std::env::set_current_dir(&cwd).expect("set cwd");

    let install_local = run(&[
        "install".to_string(),
        "./pkg".to_string(),
        "--local".to_string(),
    ])
    .expect("install local");
    let list = run(&["list".to_string()]).expect("list");
    let remove_local = run(&[
        "remove".to_string(),
        "./pkg".to_string(),
        "--local".to_string(),
    ])
    .expect("remove local");
    let install_npm = run(&[
        "install".to_string(),
        "npm:./npm-pkg".to_string(),
        "--local".to_string(),
    ])
    .expect("install npm");
    let update_npm = run(&["update".to_string(), "npm:./npm-pkg".to_string()]).expect("update npm");
    let remove_npm = run(&[
        "remove".to_string(),
        "npm:./npm-pkg".to_string(),
        "--local".to_string(),
    ])
    .expect("remove npm");

    let fixture = json!({
        "installLocal": normalize_run_result(install_local, &cwd, tempdir.path()),
        "installNpm": normalize_run_result(install_npm, &cwd, tempdir.path()),
        "list": normalize_run_result(list, &cwd, tempdir.path()),
        "updateNpm": normalize_run_result(update_npm, &cwd, tempdir.path()),
        "removeNpm": normalize_run_result(remove_npm, &cwd, tempdir.path()),
        "removeLocal": normalize_run_result(remove_local, &cwd, tempdir.path()),
    });

    std::env::set_current_dir(&original_cwd).expect("restore cwd");
    match original_agent_dir {
        Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
        None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
    }

    assert_eq!(fixture, read_fixture("package-commands.json"));
}

#[tokio::test]
async fn export_cli_matches_upstream_fixture() {
    let tempdir = tempdir().expect("tempdir");
    let session_dir = tempdir.path().join("sessions");
    let cwd = tempdir.path().join("workspace").join("app");
    let output_path = tempdir.path().join("export.html");
    let (providers, mut models) = registry_with_provider();

    run_non_interactive(
        NonInteractiveRequest {
            cwd: cwd.clone(),
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
            session_dir: Some(session_dir.clone()),
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
        },
        &providers,
        &mut models,
    )
    .await
    .expect("runtime result");

    let session_file = find_session_file(&session_dir).expect("session file");
    let result = run(&[
        "--export".to_string(),
        session_file.to_string_lossy().to_string(),
        output_path.to_string_lossy().to_string(),
    ])
    .expect("export result");
    let normalized_result = normalize_run_result(result, &cwd, tempdir.path());

    let html = std::fs::read_to_string(&output_path).expect("export html");
    assert_eq!(
        json!({
            "exitCode": normalized_result["exitCode"].clone(),
            "stdoutLines": normalized_result["stdoutLines"].clone(),
            "stderrLines": normalized_result["stderrLines"].clone(),
            "htmlChecks": {
                "containsAssistant": html.contains("assistant"),
            }
        }),
        read_fixture("export-cli.json")
    );
}
