use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::{Map, Value, json};
use tempfile::tempdir;

struct ProviderCase {
    provider: &'static str,
    default_model: &'static str,
    model_env: &'static str,
}

const PROVIDER_CASES: [ProviderCase; 4] = [
    ProviderCase {
        provider: "openai",
        default_model: "gpt-4.1",
        model_env: "PI_RUST_SMOKE_MODEL_OPENAI",
    },
    ProviderCase {
        provider: "openai-codex",
        default_model: "gpt-5.3-codex",
        model_env: "PI_RUST_SMOKE_MODEL_OPENAI_CODEX",
    },
    ProviderCase {
        provider: "openrouter",
        default_model: "openai/gpt-5.1-codex",
        model_env: "PI_RUST_SMOKE_MODEL_OPENROUTER",
    },
    ProviderCase {
        provider: "anthropic",
        default_model: "claude-haiku-4-5",
        model_env: "PI_RUST_SMOKE_MODEL_ANTHROPIC",
    },
];

#[test]
#[ignore = "requires live provider credentials and network access"]
fn live_provider_smoke_matrix() {
    let source_agent_dir = detect_source_agent_dir();
    let tempdir = tempdir().expect("tempdir");
    let agent_dir = tempdir.path().join("agent");
    fs::create_dir_all(&agent_dir).expect("create isolated agent dir");
    copy_agent_state(source_agent_dir.as_deref(), &agent_dir);

    let mut attempted = 0usize;
    let mut skipped = Vec::new();

    for case in PROVIDER_CASES {
        if !provider_has_auth(case.provider, source_agent_dir.as_deref()) {
            skipped.push(case.provider);
            continue;
        }

        attempted += 1;
        let model = env::var(case.model_env).unwrap_or_else(|_| case.default_model.to_string());
        run_text_smoke(case.provider, &model, &agent_dir);
        run_json_smoke(case.provider, &model, &agent_dir);
    }

    assert!(
        attempted > 0,
        "No live provider credentials detected. Checked env vars and auth stores under {:?}. Skipped providers: {}",
        candidate_source_agent_dirs(),
        skipped.join(", ")
    );
}

fn run_text_smoke(provider: &str, model: &str, agent_dir: &Path) {
    let token = smoke_token(provider);
    let output = run_pi_rust(
        agent_dir,
        &[
            "--print",
            "--provider",
            provider,
            "--model",
            model,
            "--no-session",
            "--no-tools",
            "--thinking",
            "off",
            "--system-prompt",
            &format!(
                "You are running a provider smoke test. Reply with exactly {token} and nothing else."
            ),
            &format!("Return exactly {token}."),
        ],
    );

    assert!(
        output.status.success(),
        "text smoke failed for {provider}/{model}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(
        stdout.contains(&token.to_lowercase()),
        "text smoke response for {provider}/{model} did not contain token {token}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout),
    );
}

fn run_json_smoke(provider: &str, model: &str, agent_dir: &Path) {
    let token = smoke_token(provider);
    let output = run_pi_rust(
        agent_dir,
        &[
            "--mode",
            "json",
            "--provider",
            provider,
            "--model",
            model,
            "--no-session",
            "--no-tools",
            "--thinking",
            "off",
            "--system-prompt",
            &format!(
                "You are running a provider smoke test. Reply with exactly {token} and nothing else."
            ),
            &format!("Return exactly {token}."),
        ],
    );

    assert!(
        output.status.success(),
        "json smoke failed for {provider}/{model}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let lines = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("json output line"))
        .collect::<Vec<_>>();

    assert!(
        lines
            .iter()
            .any(|line| line.get("type").and_then(Value::as_str) == Some("session")),
        "json smoke for {provider}/{model} did not emit a session header"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.get("type").and_then(Value::as_str) == Some("agent_start")),
        "json smoke for {provider}/{model} did not emit an agent_start event"
    );

    let turn_end = lines
        .iter()
        .find(|line| line.get("type").and_then(Value::as_str) == Some("turn_end"))
        .expect("turn_end event");
    let done_text = turn_end
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    assert!(
        done_text.to_lowercase().contains(&token.to_lowercase()),
        "json smoke turn_end event for {provider}/{model} did not contain token {token}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout),
    );
}

fn run_pi_rust(agent_dir: &Path, args: &[&str]) -> std::process::Output {
    let mut command = cargo_bin_cmd!("pi-rust");
    command.env("PI_RUST_CODING_AGENT_DIR", agent_dir);
    command.args(args);
    command.output().expect("run pi-rust")
}

fn smoke_token(provider: &str) -> String {
    format!(
        "PI_RUST_SMOKE_{}_OK",
        provider.replace('-', "_").to_ascii_uppercase()
    )
}

fn provider_has_auth(provider: &str, source_agent_dir: Option<&Path>) -> bool {
    env_has_auth(provider)
        || source_agent_dir
            .is_some_and(|dir| auth_file_has_provider(provider, &dir.join("auth.json")))
        || matches!(provider, "openai-codex") && load_codex_oauth_credential().is_some()
}

fn env_has_auth(provider: &str) -> bool {
    match provider {
        "openai" | "openai-codex" => env_has_value("OPENAI_API_KEY"),
        "openrouter" => env_has_value("OPENROUTER_API_KEY"),
        "anthropic" => env_has_value("ANTHROPIC_API_KEY") || env_has_value("ANTHROPIC_OAUTH_TOKEN"),
        _ => false,
    }
}

fn env_has_value(name: &str) -> bool {
    env::var(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn auth_file_has_provider(provider: &str, auth_path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(auth_path) else {
        return false;
    };
    if content.trim().is_empty() {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(&content) else {
        return false;
    };
    let Some(credentials) = value.get(provider).and_then(Value::as_object) else {
        return false;
    };
    match credentials.get("type").and_then(Value::as_str) {
        Some("api_key") => credentials
            .get("key")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        Some("oauth") => credentials
            .get("access")
            .and_then(Value::as_str)
            .or_else(|| credentials.get("refresh").and_then(Value::as_str))
            .is_some_and(|value| !value.is_empty()),
        _ => false,
    }
}

fn detect_source_agent_dir() -> Option<PathBuf> {
    candidate_source_agent_dirs().into_iter().find(|dir| {
        auth_file_has_any_provider(&dir.join("auth.json"))
            || dir.join("models.json").exists()
            || dir.join("settings.json").exists()
    })
}

fn auth_file_has_any_provider(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    if content.trim().is_empty() {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(&content) else {
        return false;
    };
    value
        .as_object()
        .is_some_and(|object| object.keys().any(|key| !key.trim().is_empty()))
}

fn candidate_source_agent_dirs() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(value) = env::var("PI_RUST_SMOKE_SOURCE_AGENT_DIR") {
        let path = PathBuf::from(value);
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        for candidate in [home.join(".pi-rust/agent"), home.join(".pi/agent")] {
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }

    candidates
}

fn copy_agent_state(source_agent_dir: Option<&Path>, target_agent_dir: &Path) {
    let Some(source_agent_dir) = source_agent_dir else {
        merge_codex_auth(target_agent_dir);
        return;
    };

    for file_name in ["auth.json", "models.json", "settings.json"] {
        let source = source_agent_dir.join(file_name);
        if !source.exists() {
            continue;
        }
        let target = target_agent_dir.join(file_name);
        let _ = fs::copy(source, target);
    }

    merge_codex_auth(target_agent_dir);
}

fn merge_codex_auth(target_agent_dir: &Path) {
    let Some(codex_credential) = load_codex_oauth_credential() else {
        return;
    };

    let auth_path = target_agent_dir.join("auth.json");
    let mut auth_object = match fs::read_to_string(&auth_path)
        .ok()
        .filter(|content| !content.trim().is_empty())
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
    {
        Some(Value::Object(object)) => object,
        _ => Map::new(),
    };

    if auth_object.contains_key("openai-codex") {
        return;
    }

    auth_object.insert("openai-codex".to_string(), codex_credential);
    let payload = Value::Object(auth_object);
    let _ = fs::write(
        auth_path,
        serde_json::to_string_pretty(&payload).expect("serialize merged auth"),
    );
}

fn load_codex_oauth_credential() -> Option<Value> {
    let home = env::var_os("HOME").map(PathBuf::from)?;
    let auth_path = home.join(".codex").join("auth.json");
    let content = fs::read_to_string(auth_path).ok()?;
    if content.trim().is_empty() {
        return None;
    }

    let value = serde_json::from_str::<Value>(&content).ok()?;
    let tokens = value.get("tokens")?.as_object()?;
    let access = tokens.get("access_token")?.as_str()?.trim();
    if access.is_empty() {
        return None;
    }

    let refresh = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let account_id = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .unwrap_or_default();

    Some(json!({
        "type": "oauth",
        "refresh": refresh,
        "access": access,
        "expires": 4102444800i64,
        "account_id": account_id,
    }))
}
