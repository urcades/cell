use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use pi_rust_config::get_auth_path;
use reqwest::Url;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

type FallbackResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub refresh: String,
    pub access: String,
    pub expires: i64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthCredential {
    #[serde(rename = "api_key")]
    ApiKey { key: String },
    #[serde(rename = "oauth")]
    OAuth(OAuthCredentials),
}

pub type AuthStorageData = BTreeMap<String, AuthCredential>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthSource {
    RuntimeOverride,
    StoredApiKey,
    StoredOAuth,
    Environment,
    Fallback,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthStatus {
    pub provider: String,
    pub authenticated: bool,
    pub source: AuthSource,
    pub has_stored_auth: bool,
    pub has_env_auth: bool,
    pub has_runtime_override: bool,
}

pub trait OAuthProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn get_api_key(&self, credentials: &OAuthCredentials) -> Option<String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthAuthInfo {
    pub url: String,
    pub instructions: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthPrompt {
    pub message: String,
    pub placeholder: Option<String>,
}

pub trait OAuthLoginBridge: Send + Sync {
    fn show_auth(&self, info: OAuthAuthInfo) -> Result<(), String>;
    fn prompt(&self, prompt: OAuthPrompt) -> Result<String, String>;

    fn manual_code_input(&self, prompt: OAuthPrompt) -> Result<String, String> {
        self.prompt(prompt)
    }

    fn progress(&self, _message: &str) -> Result<(), String> {
        Ok(())
    }

    fn cancel_pending_input(&self) {}

    fn is_cancelled(&self) -> bool {
        false
    }
}

const OPENAI_CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_CODEX_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const OPENAI_CODEX_SCOPE: &str = "openid profile email offline_access";
const OPENAI_CODEX_JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const OPENAI_CODEX_SUCCESS_HTML: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\" /><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" /><title>Authentication successful</title></head><body><p>Authentication successful. Return to your terminal to continue.</p></body></html>";

const ANTHROPIC_CLIENT_ID_B64: &str = "OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl";
const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const ANTHROPIC_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const ANTHROPIC_SCOPE: &str = "org:create_api_key user:profile user:inference";

pub fn oauth_provider_name(provider: &str) -> &'static str {
    match provider {
        "openai-codex" => "OpenAI Codex",
        "anthropic" => "Anthropic",
        _ => "OAuth Provider",
    }
}

pub fn oauth_provider_uses_callback_server(provider: &str) -> bool {
    matches!(provider, "openai-codex")
}

pub fn login_oauth_provider(
    provider: &str,
    bridge: Arc<dyn OAuthLoginBridge>,
) -> Result<OAuthCredentials, String> {
    match provider {
        "openai-codex" => login_openai_codex(bridge),
        "anthropic" => login_anthropic(bridge),
        _ => Err(format!("Unsupported OAuth provider: {provider}")),
    }
}

pub fn refresh_oauth_credentials(
    provider: &str,
    credentials: &OAuthCredentials,
) -> Result<OAuthCredentials, String> {
    match provider {
        "openai-codex" => refresh_openai_codex_token(credentials),
        "anthropic" => refresh_anthropic_token(credentials),
        _ => Err(format!("Unsupported OAuth provider: {provider}")),
    }
}

#[derive(Debug, Error)]
pub enum AuthStorageError {
    #[error("failed to parse auth storage: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("failed to persist auth storage: {0}")]
    Persist(std::io::Error),
}

#[derive(Default)]
struct OAuthProviderRegistry {
    providers: HashMap<String, Arc<dyn OAuthProvider>>,
}

impl OAuthProviderRegistry {
    fn register(&mut self, provider: Arc<dyn OAuthProvider>) {
        self.providers.insert(provider.id().to_string(), provider);
    }

    fn get(&self, provider: &str) -> Option<Arc<dyn OAuthProvider>> {
        self.providers.get(provider).cloned()
    }

    fn list(&self) -> Vec<String> {
        let mut providers = self.providers.keys().cloned().collect::<Vec<_>>();
        providers.sort();
        providers
    }
}

fn oauth_provider_registry() -> &'static Mutex<OAuthProviderRegistry> {
    static REGISTRY: OnceLock<Mutex<OAuthProviderRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(OAuthProviderRegistry::default()))
}

fn command_result_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_oauth_provider(provider: Arc<dyn OAuthProvider>) {
    oauth_provider_registry()
        .lock()
        .expect("oauth registry lock")
        .register(provider);
}

pub fn get_oauth_provider(provider: &str) -> Option<Arc<dyn OAuthProvider>> {
    oauth_provider_registry()
        .lock()
        .expect("oauth registry lock")
        .get(provider)
}

pub fn get_oauth_providers() -> Vec<String> {
    oauth_provider_registry()
        .lock()
        .expect("oauth registry lock")
        .list()
}

pub fn get_env_api_key(provider: &str) -> Option<String> {
    match provider {
        "anthropic" => env::var("ANTHROPIC_OAUTH_TOKEN")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                env::var("ANTHROPIC_API_KEY")
                    .ok()
                    .filter(|value| !value.is_empty())
            }),
        "openai" | "openai-codex" => env::var("OPENAI_API_KEY")
            .ok()
            .filter(|value| !value.is_empty()),
        "openrouter" => env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|value| !value.is_empty()),
        _ => None,
    }
}

pub fn resolve_config_value(config: &str) -> Option<String> {
    if config.starts_with('!') {
        return execute_command(config);
    }
    env::var(config)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| Some(config.to_string()))
}

pub fn resolve_headers(
    headers: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    let headers = headers?;
    let mut resolved = BTreeMap::new();
    for (key, value) in headers {
        if let Some(value) = resolve_config_value(value) {
            resolved.insert(key.clone(), value);
        }
    }
    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

pub fn clear_config_value_cache() {
    command_result_cache()
        .lock()
        .expect("command cache lock")
        .clear();
}

fn execute_command(command_config: &str) -> Option<String> {
    if let Some(value) = command_result_cache()
        .lock()
        .expect("command cache lock")
        .get(command_config)
        .cloned()
    {
        return value;
    }

    let command = command_config.trim_start_matches('!');
    let output = Command::new("/bin/sh")
        .arg("-lc")
        .arg(command)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                }
            } else {
                None
            }
        });

    command_result_cache()
        .lock()
        .expect("command cache lock")
        .insert(command_config.to_string(), output.clone());
    output
}

#[derive(Clone)]
pub struct AuthStorage {
    auth_path: Option<PathBuf>,
    data: AuthStorageData,
    runtime_overrides: BTreeMap<String, String>,
    fallback_resolver: Option<FallbackResolver>,
    errors: Vec<String>,
}

impl AuthStorage {
    pub fn create(auth_path: Option<PathBuf>) -> Self {
        let auth_path = auth_path.or_else(|| Some(get_auth_path()));
        let data = auth_path
            .as_deref()
            .and_then(|path| load_auth_data(path).ok())
            .unwrap_or_default();

        Self {
            auth_path,
            data,
            runtime_overrides: BTreeMap::new(),
            fallback_resolver: None,
            errors: Vec::new(),
        }
    }

    pub fn in_memory(data: AuthStorageData) -> Self {
        Self {
            auth_path: None,
            data,
            runtime_overrides: BTreeMap::new(),
            fallback_resolver: None,
            errors: Vec::new(),
        }
    }

    pub fn reload(&mut self) {
        if let Some(path) = &self.auth_path {
            match load_auth_data(path) {
                Ok(data) => self.data = data,
                Err(error) => self.errors.push(error.to_string()),
            }
        }
    }

    pub fn get(&self, provider: &str) -> Option<&AuthCredential> {
        self.data.get(provider)
    }

    pub fn set(
        &mut self,
        provider: impl Into<String>,
        credential: AuthCredential,
    ) -> Result<(), AuthStorageError> {
        self.data.insert(provider.into(), credential);
        self.persist()
    }

    pub fn remove(&mut self, provider: &str) -> Result<(), AuthStorageError> {
        self.data.remove(provider);
        self.persist()
    }

    pub fn list(&self) -> Vec<String> {
        self.current_data_snapshot().keys().cloned().collect()
    }

    pub fn has(&self, provider: &str) -> bool {
        self.current_data_snapshot().contains_key(provider)
    }

    pub fn has_auth(&self, provider: &str) -> bool {
        if self.runtime_overrides.contains_key(provider) {
            return true;
        }
        let data = self.current_data_snapshot();
        if data.contains_key(provider) {
            return true;
        }
        if get_env_api_key(provider).is_some() {
            return true;
        }
        self.fallback_resolver
            .as_ref()
            .and_then(|resolver| resolver(provider))
            .is_some()
    }

    pub fn get_all(&self) -> AuthStorageData {
        self.current_data_snapshot()
    }

    pub fn get_status(&self, provider: &str) -> AuthStatus {
        let has_runtime_override = self.runtime_overrides.contains_key(provider);
        let data = self.current_data_snapshot();
        let stored = data.get(provider);
        let has_stored_auth = stored.is_some();
        let has_env_auth = get_env_api_key(provider).is_some();
        let fallback_auth = self
            .fallback_resolver
            .as_ref()
            .and_then(|resolver| resolver(provider))
            .is_some();

        let source = if has_runtime_override {
            AuthSource::RuntimeOverride
        } else {
            match stored {
                Some(AuthCredential::ApiKey { .. }) => AuthSource::StoredApiKey,
                Some(AuthCredential::OAuth(_)) => AuthSource::StoredOAuth,
                None if has_env_auth => AuthSource::Environment,
                None if fallback_auth => AuthSource::Fallback,
                None => AuthSource::Missing,
            }
        };

        AuthStatus {
            provider: provider.to_string(),
            authenticated: !matches!(source, AuthSource::Missing),
            source,
            has_stored_auth,
            has_env_auth,
            has_runtime_override,
        }
    }

    pub fn get_statuses(&self, providers: &[String]) -> Vec<AuthStatus> {
        providers
            .iter()
            .map(|provider| self.get_status(provider))
            .collect()
    }

    pub fn set_runtime_api_key(&mut self, provider: impl Into<String>, api_key: impl Into<String>) {
        self.runtime_overrides
            .insert(provider.into(), api_key.into());
    }

    pub fn remove_runtime_api_key(&mut self, provider: &str) {
        self.runtime_overrides.remove(provider);
    }

    pub fn set_fallback_resolver(&mut self, resolver: FallbackResolver) {
        self.fallback_resolver = Some(resolver);
    }

    pub fn drain_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    pub fn get_api_key(&self, provider: &str) -> Option<String> {
        if let Some(runtime_key) = self.runtime_overrides.get(provider) {
            if !runtime_key.is_empty() {
                return Some(runtime_key.clone());
            }
        }

        let mut data = self.current_data_snapshot();
        match data.get(provider) {
            Some(AuthCredential::ApiKey { key }) => return resolve_config_value(key),
            Some(AuthCredential::OAuth(credentials)) => {
                let mut effective_credentials = credentials.clone();
                if oauth_credentials_need_refresh(&effective_credentials) {
                    match refresh_oauth_credentials(provider, &effective_credentials) {
                        Ok(refreshed) => {
                            effective_credentials = refreshed.clone();
                            data.insert(provider.to_string(), AuthCredential::OAuth(refreshed));
                            let _ = self.persist_snapshot(&data);
                        }
                        Err(_) => return None,
                    }
                }
                if let Some(oauth_provider) = get_oauth_provider(provider) {
                    return oauth_provider.get_api_key(&effective_credentials);
                }
                return None;
            }
            None => {}
        }

        get_env_api_key(provider).or_else(|| {
            self.fallback_resolver
                .as_ref()
                .and_then(|resolver| resolver(provider))
        })
    }

    pub fn refresh_provider(&mut self, provider: &str) -> Option<String> {
        self.reload();
        let credential = self.data.get(provider).cloned();
        match credential {
            Some(AuthCredential::OAuth(credentials))
                if oauth_credentials_need_refresh(&credentials) =>
            {
                let refreshed = refresh_oauth_credentials(provider, &credentials).ok()?;
                self.data.insert(
                    provider.to_string(),
                    AuthCredential::OAuth(refreshed.clone()),
                );
                let _ = self.persist();
                get_oauth_provider(provider)?.get_api_key(&refreshed)
            }
            _ => self.get_api_key(provider),
        }
    }

    pub fn logout(&mut self, provider: &str) -> Result<bool, AuthStorageError> {
        let removed_runtime = self.runtime_overrides.remove(provider).is_some();
        let removed_stored = self.data.remove(provider).is_some();
        if removed_stored {
            self.persist()?;
        }
        Ok(removed_runtime || removed_stored)
    }

    pub fn clear_all(&mut self) -> Result<(), AuthStorageError> {
        self.runtime_overrides.clear();
        self.data.clear();
        self.persist()
    }

    fn persist(&self) -> Result<(), AuthStorageError> {
        self.persist_snapshot(&self.data)
    }

    fn persist_snapshot(&self, data: &AuthStorageData) -> Result<(), AuthStorageError> {
        let Some(path) = &self.auth_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(AuthStorageError::Persist)?;
        }
        let payload = serde_json::to_string_pretty(data)?;
        let temp_path = path.with_extension(format!(
            "{}.tmp",
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("json")
        ));
        fs::write(&temp_path, payload).map_err(AuthStorageError::Persist)?;
        fs::rename(&temp_path, path).map_err(AuthStorageError::Persist)
    }

    fn current_data_snapshot(&self) -> AuthStorageData {
        self.auth_path
            .as_deref()
            .and_then(|path| load_auth_data(path).ok())
            .unwrap_or_else(|| self.data.clone())
    }
}

fn load_auth_data(path: &Path) -> Result<AuthStorageData, AuthStorageError> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let content = fs::read_to_string(path).map_err(AuthStorageError::Persist)?;
    if content.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    Ok(serde_json::from_str(&content)?)
}

fn oauth_credentials_need_refresh(credentials: &OAuthCredentials) -> bool {
    !credentials.refresh.trim().is_empty()
        && current_epoch_ms() >= normalized_expires_ms(credentials.expires)
}

fn normalized_expires_ms(expires: i64) -> i64 {
    if expires > 0 && expires < 10_000_000_000 {
        expires.saturating_mul(1000)
    } else {
        expires
    }
}

fn current_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn create_pkce_pair() -> (String, String) {
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn anthropic_client_id() -> Result<String, String> {
    let bytes = STANDARD
        .decode(ANTHROPIC_CLIENT_ID_B64)
        .map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

fn open_browser(url: &str) {
    let mut command = match env::consts::OS {
        "macos" => {
            let mut command = Command::new("open");
            command.arg(url);
            command
        }
        "windows" => {
            let mut command = Command::new("cmd");
            command.args(["/C", "start", "", url]);
            command
        }
        _ => {
            let mut command = Command::new("xdg-open");
            command.arg(url);
            command
        }
    };
    let _ = command.spawn();
}

fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }

    if let Ok(url) = Url::parse(value) {
        return (
            url.query_pairs()
                .find(|(key, _)| key == "code")
                .map(|(_, value)| value.into_owned()),
            url.query_pairs()
                .find(|(key, _)| key == "state")
                .map(|(_, value)| value.into_owned()),
        );
    }

    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_string()), Some(state.to_string()));
    }

    if value.contains("code=") {
        let query_url = format!("http://localhost/?{value}");
        if let Ok(url) = Url::parse(&query_url) {
            return (
                url.query_pairs()
                    .find(|(key, _)| key == "code")
                    .map(|(_, value)| value.into_owned()),
                url.query_pairs()
                    .find(|(key, _)| key == "state")
                    .map(|(_, value)| value.into_owned()),
            );
        }
    }

    (Some(value.to_string()), None)
}

fn decode_jwt_payload(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| {
            let mut padded = payload.to_string();
            while padded.len() % 4 != 0 {
                padded.push('=');
            }
            URL_SAFE.decode(padded)
        })
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn openai_codex_account_id(access_token: &str) -> Option<String> {
    decode_jwt_payload(access_token)?
        .get(OPENAI_CODEX_JWT_CLAIM_PATH)?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

fn blocking_client() -> Result<Client, String> {
    Client::builder().build().map_err(|error| error.to_string())
}

fn exchange_openai_codex_code(code: &str, verifier: &str) -> Result<OAuthCredentials, String> {
    let response = blocking_client()?
        .post(OPENAI_CODEX_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", OPENAI_CODEX_CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", OPENAI_CODEX_REDIRECT_URI),
        ])
        .send()
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(format!("Token exchange failed ({status}): {body}"));
    }

    let payload: TokenResponse = response.json().map_err(|error| error.to_string())?;
    let access = payload
        .access_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Token response was missing access_token.".to_string())?;
    let refresh = payload
        .refresh_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Token response was missing refresh_token.".to_string())?;
    let expires_in = payload
        .expires_in
        .ok_or_else(|| "Token response was missing expires_in.".to_string())?;
    let account_id = openai_codex_account_id(&access)
        .ok_or_else(|| "Failed to extract accountId from token.".to_string())?;
    let mut extra = BTreeMap::new();
    extra.insert("account_id".to_string(), Value::String(account_id));
    Ok(OAuthCredentials {
        access,
        refresh,
        expires: current_epoch_ms() + expires_in.saturating_mul(1000),
        extra,
    })
}

fn refresh_openai_codex_token(credentials: &OAuthCredentials) -> Result<OAuthCredentials, String> {
    let response = blocking_client()?
        .post(OPENAI_CODEX_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", credentials.refresh.as_str()),
            ("client_id", OPENAI_CODEX_CLIENT_ID),
        ])
        .send()
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(format!(
            "OpenAI Codex token refresh failed ({status}): {body}"
        ));
    }

    let payload: TokenResponse = response.json().map_err(|error| error.to_string())?;
    let access = payload
        .access_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Token refresh response was missing access_token.".to_string())?;
    let refresh = payload
        .refresh_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Token refresh response was missing refresh_token.".to_string())?;
    let expires_in = payload
        .expires_in
        .ok_or_else(|| "Token refresh response was missing expires_in.".to_string())?;
    let account_id = openai_codex_account_id(&access)
        .ok_or_else(|| "Failed to extract accountId from token.".to_string())?;
    let mut extra = BTreeMap::new();
    extra.insert("account_id".to_string(), Value::String(account_id));
    Ok(OAuthCredentials {
        access,
        refresh,
        expires: current_epoch_ms() + expires_in.saturating_mul(1000),
        extra,
    })
}

fn exchange_anthropic_code(
    code: &str,
    state: &str,
    verifier: &str,
) -> Result<OAuthCredentials, String> {
    let client_id = anthropic_client_id()?;
    let response = blocking_client()?
        .post(ANTHROPIC_TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": client_id,
            "code": code,
            "state": state,
            "redirect_uri": ANTHROPIC_REDIRECT_URI,
            "code_verifier": verifier,
        }))
        .send()
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(format!(
            "Anthropic token exchange failed ({status}): {body}"
        ));
    }

    let payload: TokenResponse = response.json().map_err(|error| error.to_string())?;
    let access = payload
        .access_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Token response was missing access_token.".to_string())?;
    let refresh = payload
        .refresh_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Token response was missing refresh_token.".to_string())?;
    let expires_in = payload
        .expires_in
        .ok_or_else(|| "Token response was missing expires_in.".to_string())?;
    Ok(OAuthCredentials {
        access,
        refresh,
        expires: current_epoch_ms() + expires_in.saturating_mul(1000) - 5 * 60 * 1000,
        extra: BTreeMap::new(),
    })
}

fn refresh_anthropic_token(credentials: &OAuthCredentials) -> Result<OAuthCredentials, String> {
    let client_id = anthropic_client_id()?;
    let response = blocking_client()?
        .post(ANTHROPIC_TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": client_id,
            "refresh_token": credentials.refresh,
        }))
        .send()
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(format!("Anthropic token refresh failed ({status}): {body}"));
    }

    let payload: TokenResponse = response.json().map_err(|error| error.to_string())?;
    let access = payload
        .access_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Token refresh response was missing access_token.".to_string())?;
    let refresh = payload
        .refresh_token
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Token refresh response was missing refresh_token.".to_string())?;
    let expires_in = payload
        .expires_in
        .ok_or_else(|| "Token refresh response was missing expires_in.".to_string())?;
    Ok(OAuthCredentials {
        access,
        refresh,
        expires: current_epoch_ms() + expires_in.saturating_mul(1000) - 5 * 60 * 1000,
        extra: BTreeMap::new(),
    })
}

struct LocalOAuthServer {
    listener: TcpListener,
    expected_state: String,
    cancelled: Arc<AtomicBool>,
}

impl LocalOAuthServer {
    fn bind(expected_state: String, cancelled: Arc<AtomicBool>) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:1455").map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            listener,
            expected_state,
            cancelled,
        })
    }

    fn poll_code(&self) -> Result<Option<String>, String> {
        match self.listener.accept() {
            Ok((mut stream, _)) => self.handle_connection(&mut stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn handle_connection(
        &self,
        stream: &mut impl ReadWriteStream,
    ) -> Result<Option<String>, String> {
        let mut buffer = [0u8; 4096];
        let count = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        let request = String::from_utf8_lossy(&buffer[..count]);
        let first_line = request.lines().next().unwrap_or_default();
        let path = first_line.split_whitespace().nth(1).unwrap_or("/");
        let url =
            Url::parse(&format!("http://localhost{path}")).map_err(|error| error.to_string())?;
        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned());
        let code = url
            .query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, value)| value.into_owned());

        if state.as_deref() != Some(self.expected_state.as_str()) {
            write_http_response(stream, 400, "State mismatch")?;
            return Ok(None);
        }
        let Some(code) = code else {
            write_http_response(stream, 400, "Missing authorization code")?;
            return Ok(None);
        };
        write_http_html(stream, 200, OPENAI_CODEX_SUCCESS_HTML)?;
        Ok(Some(code))
    }

    fn wait_for_code(
        &self,
        manual_rx: &std::sync::mpsc::Receiver<Result<String, String>>,
    ) -> Result<String, String> {
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err("Login cancelled".to_string());
            }
            if let Some(code) = self.poll_code()? {
                return Ok(code);
            }
            match manual_rx.try_recv() {
                Ok(result) => return result,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err("Login input channel disconnected".to_string());
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

trait ReadWriteStream: Read + Write {}
impl<T: Read + Write> ReadWriteStream for T {}

fn write_http_response(stream: &mut impl Write, status: u16, body: &str) -> Result<(), String> {
    write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .map_err(|error| error.to_string())
}

fn write_http_html(stream: &mut impl Write, status: u16, body: &str) -> Result<(), String> {
    write!(
        stream,
        "HTTP/1.1 {status} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .map_err(|error| error.to_string())
}

fn login_openai_codex(bridge: Arc<dyn OAuthLoginBridge>) -> Result<OAuthCredentials, String> {
    let (verifier, challenge) = create_pkce_pair();
    let state = Uuid::new_v4().simple().to_string();
    let mut url = Url::parse(OPENAI_CODEX_AUTHORIZE_URL).map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", OPENAI_CODEX_CLIENT_ID)
        .append_pair("redirect_uri", OPENAI_CODEX_REDIRECT_URI)
        .append_pair("scope", OPENAI_CODEX_SCOPE)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "pi-rust");

    bridge.show_auth(OAuthAuthInfo {
        url: url.to_string(),
        instructions: Some("A browser window should open. Complete login to finish.".to_string()),
    })?;
    open_browser(url.as_str());

    let cancelled = Arc::new(AtomicBool::new(false));
    let server = match LocalOAuthServer::bind(state.clone(), Arc::clone(&cancelled)) {
        Ok(server) => Some(server),
        Err(_) => {
            bridge.progress(
                "Local callback server unavailable. Paste the authorization code manually.",
            )?;
            None
        }
    };

    let (manual_tx, manual_rx) = std::sync::mpsc::channel();
    {
        let bridge = Arc::clone(&bridge);
        let cancelled = Arc::clone(&cancelled);
        thread::spawn(move || {
            let result = bridge.manual_code_input(OAuthPrompt {
                message: "Paste the authorization code (or full redirect URL):".to_string(),
                placeholder: Some("code#state or full redirect URL".to_string()),
            });
            if result.is_err() {
                cancelled.store(true, Ordering::SeqCst);
            }
            let _ = manual_tx.send(result);
        });
    }

    let input = if let Some(server) = server {
        server.wait_for_code(&manual_rx)?
    } else {
        manual_rx
            .recv()
            .map_err(|_| "Login input channel disconnected".to_string())??
    };
    cancelled.store(true, Ordering::SeqCst);
    bridge.cancel_pending_input();
    let (code, response_state) = parse_authorization_input(&input);
    if let Some(response_state) = response_state {
        if response_state != state {
            return Err("State mismatch".to_string());
        }
    }
    let code = code.ok_or_else(|| "Missing authorization code".to_string())?;
    exchange_openai_codex_code(&code, &verifier)
}

fn login_anthropic(bridge: Arc<dyn OAuthLoginBridge>) -> Result<OAuthCredentials, String> {
    let client_id = anthropic_client_id()?;
    let (verifier, challenge) = create_pkce_pair();
    let mut url = Url::parse(ANTHROPIC_AUTHORIZE_URL).map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", &client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", ANTHROPIC_REDIRECT_URI)
        .append_pair("scope", ANTHROPIC_SCOPE)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &verifier);

    bridge.show_auth(OAuthAuthInfo {
        url: url.to_string(),
        instructions: Some(
            "Complete login in your browser, then paste the returned code.".to_string(),
        ),
    })?;
    open_browser(url.as_str());
    let input = bridge.prompt(OAuthPrompt {
        message: "Paste the authorization code:".to_string(),
        placeholder: Some("code#state".to_string()),
    })?;
    let (code, state) = parse_authorization_input(&input);
    let code = code.ok_or_else(|| "Missing authorization code".to_string())?;
    let state = state.unwrap_or_default();
    exchange_anthropic_code(&code, &state, &verifier)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::env;
    use std::sync::Arc;

    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tempfile::tempdir;

    use super::{
        AuthCredential, AuthSource, AuthStorage, AuthStorageData, OAuthCredentials, OAuthProvider,
        clear_config_value_cache, get_env_api_key, normalized_expires_ms, openai_codex_account_id,
        parse_authorization_input, register_oauth_provider, resolve_config_value, resolve_headers,
    };

    struct StaticOAuthProvider;

    impl OAuthProvider for StaticOAuthProvider {
        fn id(&self) -> &'static str {
            "anthropic"
        }

        fn get_api_key(&self, credentials: &OAuthCredentials) -> Option<String> {
            Some(credentials.access.clone())
        }
    }

    #[test]
    fn resolves_env_api_keys_for_known_providers() {
        unsafe { env::set_var("OPENAI_API_KEY", "test-openai-key") };
        assert_eq!(
            get_env_api_key("openai").as_deref(),
            Some("test-openai-key")
        );
        unsafe { env::remove_var("OPENAI_API_KEY") };
    }

    #[test]
    fn resolves_literal_and_env_backed_config_values() {
        unsafe { env::set_var("PI_RUST_TEST_KEY", "secret") };
        assert_eq!(
            resolve_config_value("PI_RUST_TEST_KEY").as_deref(),
            Some("secret")
        );
        assert_eq!(
            resolve_config_value("literal-value").as_deref(),
            Some("literal-value")
        );
        unsafe { env::remove_var("PI_RUST_TEST_KEY") };
    }

    #[test]
    fn resolves_command_config_values_and_caches_them() {
        clear_config_value_cache();
        let command = "!printf 'cached-value'";
        assert_eq!(
            resolve_config_value(command).as_deref(),
            Some("cached-value")
        );
        assert_eq!(
            resolve_config_value(command).as_deref(),
            Some("cached-value")
        );
    }

    #[test]
    fn resolves_headers_and_drops_empty_results() {
        clear_config_value_cache();
        let headers = BTreeMap::from([
            (
                "Authorization".to_string(),
                "!printf 'Bearer token'".to_string(),
            ),
            ("X-Static".to_string(), "static".to_string()),
        ]);
        let resolved = resolve_headers(Some(&headers)).expect("resolved headers");
        assert_eq!(
            resolved.get("Authorization").map(String::as_str),
            Some("Bearer token")
        );
        assert_eq!(resolved.get("X-Static").map(String::as_str), Some("static"));
    }

    #[test]
    fn auth_storage_persists_and_respects_precedence() {
        let tempdir = tempdir().expect("tempdir");
        let auth_path = tempdir.path().join("auth.json");
        let mut storage = AuthStorage::create(Some(auth_path.clone()));
        storage
            .set(
                "openai",
                AuthCredential::ApiKey {
                    key: "OPENAI_ENV_KEY".to_string(),
                },
            )
            .expect("persist auth");

        unsafe { env::set_var("OPENAI_ENV_KEY", "from-env") };
        let reloaded = AuthStorage::create(Some(auth_path));
        assert_eq!(reloaded.get_api_key("openai").as_deref(), Some("from-env"));

        let mut reloaded = reloaded;
        reloaded.set_runtime_api_key("openai", "runtime-key");
        assert_eq!(
            reloaded.get_api_key("openai").as_deref(),
            Some("runtime-key")
        );
        unsafe { env::remove_var("OPENAI_ENV_KEY") };
    }

    #[test]
    fn oauth_credentials_use_registered_provider() {
        register_oauth_provider(Arc::new(StaticOAuthProvider));
        let storage = AuthStorage::in_memory(BTreeMap::from([(
            "anthropic".to_string(),
            AuthCredential::OAuth(OAuthCredentials {
                refresh: "refresh".to_string(),
                access: "access".to_string(),
                expires: i64::MAX,
                extra: BTreeMap::new(),
            }),
        )]));

        assert_eq!(storage.get_api_key("anthropic").as_deref(), Some("access"));
    }

    #[test]
    fn parses_oauth_credentials_from_expected_auth_json_shape() {
        let value: AuthStorageData = serde_json::from_str(
            r#"{
  "openai-codex": {
    "type": "oauth",
    "refresh": "refresh-token",
    "access": "access-token",
    "expires": 4102444800
  }
}"#,
        )
        .expect("parse auth json");

        assert_eq!(
            value.get("openai-codex"),
            Some(&AuthCredential::OAuth(OAuthCredentials {
                refresh: "refresh-token".to_string(),
                access: "access-token".to_string(),
                expires: 4102444800,
                extra: BTreeMap::new(),
            }))
        );
    }

    #[test]
    fn fallback_resolver_supports_custom_provider_auth() {
        let mut storage = AuthStorage::in_memory(BTreeMap::new());
        storage.set_fallback_resolver(Arc::new(|provider| {
            if provider == "custom" {
                Some("fallback-key".to_string())
            } else {
                None
            }
        }));

        assert!(storage.has_auth("custom"));
        assert_eq!(
            storage.get_api_key("custom").as_deref(),
            Some("fallback-key")
        );
    }

    #[test]
    fn status_reports_auth_source_precedence() {
        let mut storage = AuthStorage::in_memory(BTreeMap::from([(
            "openai".to_string(),
            AuthCredential::ApiKey {
                key: "literal-key".to_string(),
            },
        )]));
        storage.set_runtime_api_key("openai", "runtime-key");
        let status = storage.get_status("openai");
        assert!(status.authenticated);
        assert_eq!(status.source, AuthSource::RuntimeOverride);
        assert!(status.has_stored_auth);
        assert!(status.has_runtime_override);
    }

    #[test]
    fn logout_and_clear_all_remove_auth_state() {
        let tempdir = tempdir().expect("tempdir");
        let auth_path = tempdir.path().join("auth.json");
        let mut storage = AuthStorage::create(Some(auth_path.clone()));
        storage
            .set(
                "openai",
                AuthCredential::ApiKey {
                    key: "literal-key".to_string(),
                },
            )
            .expect("persist auth");
        storage.set_runtime_api_key("openrouter", "runtime");

        assert!(storage.logout("openai").expect("logout"));
        assert!(!storage.has("openai"));

        storage.clear_all().expect("clear all");
        assert!(storage.get_all().is_empty());
        assert!(storage.get_status("openrouter").source == AuthSource::Missing);
    }

    #[test]
    fn normalizes_second_based_expiry_values_to_milliseconds() {
        assert_eq!(normalized_expires_ms(4_102_444_800), 4_102_444_800_000);
        assert_eq!(normalized_expires_ms(1_741_392_000_000), 1_741_392_000_000);
    }

    #[test]
    fn parses_openai_authorization_input_variants() {
        assert_eq!(
            parse_authorization_input("https://example.com/callback?code=abc&state=xyz"),
            (Some("abc".to_string()), Some("xyz".to_string()))
        );
        assert_eq!(
            parse_authorization_input("abc#xyz"),
            (Some("abc".to_string()), Some("xyz".to_string()))
        );
        assert_eq!(
            parse_authorization_input("code=abc&state=xyz"),
            (Some("abc".to_string()), Some("xyz".to_string()))
        );
        assert_eq!(
            parse_authorization_input("abc"),
            (Some("abc".to_string()), None)
        );
    }

    #[test]
    fn extracts_openai_codex_account_id_from_jwt_claim() {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_123"}}"#);
        let token = format!("{header}.{payload}.signature");
        assert_eq!(openai_codex_account_id(&token).as_deref(), Some("acct_123"));
    }
}
