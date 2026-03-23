use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use globset::GlobBuilder;
use pi_rust_ai_core::{ApiId, Model, ModelCost, ModelInput, ProviderId};
use pi_rust_config::get_models_path;
use pi_rust_oauth::{AuthStorage, resolve_config_value, resolve_headers};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq)]
pub struct ScopedModel {
    pub model: Model,
    pub thinking_level: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<String>,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolveCliModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ProviderConfigInput {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub auth_header: bool,
    pub models: Vec<ProviderModelInput>,
}

#[derive(Clone, Debug)]
pub struct ProviderModelInput {
    pub id: String,
    pub name: String,
    pub api: Option<String>,
    pub reasoning: bool,
    pub input: Vec<ModelInput>,
    pub cost: ModelCost,
    pub context_window: u32,
    pub max_tokens: u32,
    pub headers: Option<BTreeMap<String, String>>,
    pub compat: Option<Value>,
}

#[derive(Debug, Error)]
pub enum ModelRegistryError {
    #[error("failed to parse models config: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("invalid models config: {0}")]
    Invalid(String),
    #[error("failed to read models config: {0}")]
    Io(std::io::Error),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelsConfig {
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderConfig {
    base_url: Option<String>,
    api_key: Option<String>,
    api: Option<String>,
    headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    auth_header: bool,
    #[serde(default)]
    models: Vec<ModelDefinition>,
    #[serde(default)]
    model_overrides: BTreeMap<String, ModelOverride>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelDefinition {
    id: String,
    name: Option<String>,
    api: Option<String>,
    reasoning: Option<bool>,
    input: Option<Vec<ModelInput>>,
    cost: Option<ModelCost>,
    context_window: Option<u32>,
    max_tokens: Option<u32>,
    headers: Option<BTreeMap<String, String>>,
    compat: Option<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelOverride {
    name: Option<String>,
    reasoning: Option<bool>,
    input: Option<Vec<ModelInput>>,
    cost: Option<PartialModelCost>,
    context_window: Option<u32>,
    max_tokens: Option<u32>,
    headers: Option<BTreeMap<String, String>>,
    compat: Option<Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartialModelCost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

#[derive(Clone, Debug)]
struct ProviderOverride {
    base_url: Option<String>,
    headers: Option<BTreeMap<String, String>>,
    api_key: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct CustomModelsResult {
    models: Vec<Model>,
    overrides: BTreeMap<String, ProviderOverride>,
    model_overrides: BTreeMap<String, BTreeMap<String, ModelOverride>>,
    error: Option<String>,
}

#[derive(Clone)]
pub struct ModelRegistry {
    auth_storage: AuthStorage,
    models_json_path: Option<PathBuf>,
    models: Vec<Model>,
    custom_provider_api_keys: BTreeMap<String, String>,
    registered_providers: BTreeMap<String, ProviderConfigInput>,
    load_error: Option<String>,
}

impl ModelRegistry {
    pub fn new(auth_storage: AuthStorage, models_json_path: Option<PathBuf>) -> Self {
        let mut registry = Self {
            auth_storage,
            models_json_path: models_json_path.or_else(|| Some(get_models_path())),
            models: Vec::new(),
            custom_provider_api_keys: BTreeMap::new(),
            registered_providers: BTreeMap::new(),
            load_error: None,
        };
        registry.refresh();
        registry
    }

    pub fn auth_storage(&self) -> &AuthStorage {
        &self.auth_storage
    }

    pub fn auth_storage_mut(&mut self) -> &mut AuthStorage {
        &mut self.auth_storage
    }

    pub fn refresh(&mut self) {
        self.custom_provider_api_keys.clear();
        self.load_error = None;
        self.install_fallback_resolver();

        let custom_models = self
            .models_json_path
            .as_deref()
            .map(load_custom_models)
            .transpose()
            .unwrap_or_else(|error| {
                Some(CustomModelsResult {
                    error: Some(error.to_string()),
                    ..CustomModelsResult::default()
                })
            })
            .unwrap_or_default();

        if let Some(error) = &custom_models.error {
            self.load_error = Some(error.clone());
        }

        for (provider, override_config) in &custom_models.overrides {
            if let Some(api_key) = &override_config.api_key {
                self.custom_provider_api_keys
                    .insert(provider.clone(), api_key.clone());
            }
        }
        self.install_fallback_resolver();

        let built_in_models =
            load_built_in_models(&custom_models.overrides, &custom_models.model_overrides);
        self.models = merge_custom_models(built_in_models, custom_models.models);

        let registrations = self.registered_providers.clone();
        for (provider, config) in registrations {
            self.apply_provider_config(&provider, config);
        }
    }

    pub fn get_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn get_all(&self) -> Vec<Model> {
        self.models.clone()
    }

    pub fn get_available(&self) -> Vec<Model> {
        self.models
            .iter()
            .filter(|model| self.auth_storage.has_auth(&model.provider.0))
            .cloned()
            .collect()
    }

    pub fn find(&self, provider: &str, model_id: &str) -> Option<Model> {
        self.models
            .iter()
            .find(|model| model.provider.0 == provider && model.id == model_id)
            .cloned()
    }

    pub fn get_api_key(&self, model: &Model) -> Option<String> {
        self.auth_storage.get_api_key(&model.provider.0)
    }

    pub fn get_api_key_for_provider(&self, provider: &str) -> Option<String> {
        self.auth_storage.get_api_key(provider)
    }

    pub fn register_provider(
        &mut self,
        provider_name: impl Into<String>,
        config: ProviderConfigInput,
    ) {
        let provider_name = provider_name.into();
        self.registered_providers
            .insert(provider_name.clone(), config.clone());
        self.apply_provider_config(&provider_name, config);
    }

    fn apply_provider_config(&mut self, provider_name: &str, config: ProviderConfigInput) {
        if let Some(api_key) = &config.api_key {
            self.custom_provider_api_keys
                .insert(provider_name.to_string(), api_key.clone());
            self.install_fallback_resolver();
        }

        if !config.models.is_empty() {
            self.models
                .retain(|model| model.provider.0 != provider_name);
            for model in config.models {
                let mut headers = to_hash_map(resolve_headers(config.headers.as_ref()).as_ref());
                if let Some(model_headers) = resolve_headers(model.headers.as_ref()) {
                    headers.extend(model_headers);
                }
                if config.auth_header {
                    if let Some(api_key) = config.api_key.as_deref().and_then(resolve_config_value)
                    {
                        headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
                    }
                }

                self.models.push(Model {
                    id: model.id,
                    name: model.name,
                    api: ApiId::new(
                        model
                            .api
                            .unwrap_or_else(|| config.api.clone().unwrap_or_default()),
                    ),
                    provider: ProviderId::new(provider_name.to_string()),
                    base_url: config.base_url.clone().unwrap_or_default(),
                    reasoning: model.reasoning,
                    input: model.input,
                    cost: model.cost,
                    context_window: model.context_window,
                    max_tokens: model.max_tokens,
                    headers: if headers.is_empty() {
                        None
                    } else {
                        Some(headers)
                    },
                    compat: model.compat,
                });
            }
        } else if let Some(base_url) = &config.base_url {
            let resolved_headers = to_hash_map(resolve_headers(config.headers.as_ref()).as_ref());
            self.models = self
                .models
                .iter()
                .map(|model| {
                    if model.provider.0 != provider_name {
                        return model.clone();
                    }
                    let mut updated = model.clone();
                    updated.base_url = base_url.clone();
                    if !resolved_headers.is_empty() {
                        let mut headers = updated.headers.unwrap_or_default();
                        headers.extend(resolved_headers.clone());
                        updated.headers = Some(headers);
                    }
                    updated
                })
                .collect();
        }
    }

    fn install_fallback_resolver(&mut self) {
        let api_keys = self.custom_provider_api_keys.clone();
        self.auth_storage
            .set_fallback_resolver(Arc::new(move |provider| {
                api_keys
                    .get(provider)
                    .and_then(|value| resolve_config_value(value))
            }));
    }
}

pub fn default_model_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("claude-opus-4-6"),
        "openai" => Some("gpt-5.1-codex"),
        "openai-codex" => Some("gpt-5.3-codex"),
        "openrouter" => Some("openai/gpt-5.1-codex"),
        _ => None,
    }
}

pub fn supports_xhigh(model: &Model) -> bool {
    if model.id.contains("gpt-5.2") || model.id.contains("gpt-5.3") {
        return true;
    }

    model.api.0 == "anthropic-messages"
        && (model.id.contains("opus-4-6") || model.id.contains("opus-4.6"))
}

pub fn models_are_equal(a: Option<&Model>, b: Option<&Model>) -> bool {
    match (a, b) {
        (Some(left), Some(right)) => left.id == right.id && left.provider == right.provider,
        _ => false,
    }
}

pub fn parse_model_pattern(
    pattern: &str,
    available_models: &[Model],
    allow_invalid_thinking_level_fallback: bool,
) -> ParsedModelResult {
    if let Some(exact_match) = try_match_model(pattern, available_models) {
        return ParsedModelResult {
            model: Some(exact_match),
            thinking_level: None,
            warning: None,
        };
    }

    let Some(last_colon_index) = pattern.rfind(':') else {
        return ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        };
    };

    let prefix = &pattern[..last_colon_index];
    let suffix = &pattern[last_colon_index + 1..];

    if is_valid_thinking_level(suffix) {
        let result = parse_model_pattern(
            prefix,
            available_models,
            allow_invalid_thinking_level_fallback,
        );
        if result.model.is_some() && result.warning.is_none() {
            return ParsedModelResult {
                model: result.model,
                thinking_level: Some(suffix.to_string()),
                warning: None,
            };
        }
        return result;
    }

    if !allow_invalid_thinking_level_fallback {
        return ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        };
    }

    let result = parse_model_pattern(
        prefix,
        available_models,
        allow_invalid_thinking_level_fallback,
    );
    if result.model.is_some() {
        return ParsedModelResult {
            model: result.model,
            thinking_level: None,
            warning: Some(format!(
                "Invalid thinking level \"{suffix}\" in pattern \"{pattern}\". Using default instead."
            )),
        };
    }

    result
}

pub fn resolve_model_scope(
    patterns: &[String],
    model_registry: &ModelRegistry,
) -> Vec<ScopedModel> {
    let available_models = model_registry.get_available();
    let mut scoped_models = Vec::new();

    for pattern in patterns {
        if contains_glob(pattern) {
            let (glob_pattern, thinking_level) = split_optional_thinking_suffix(pattern);
            let matcher = GlobBuilder::new(&glob_pattern)
                .case_insensitive(true)
                .build()
                .expect("valid glob")
                .compile_matcher();

            for model in &available_models {
                let full_id = format!("{}/{}", model.provider.0, model.id);
                if matcher.is_match(&full_id) || matcher.is_match(&model.id) {
                    if !scoped_models.iter().any(|existing: &ScopedModel| {
                        models_are_equal(Some(&existing.model), Some(model))
                    }) {
                        scoped_models.push(ScopedModel {
                            model: model.clone(),
                            thinking_level: thinking_level.clone(),
                        });
                    }
                }
            }
            continue;
        }

        let parsed = parse_model_pattern(pattern, &available_models, true);
        if let Some(model) = parsed.model {
            if !scoped_models
                .iter()
                .any(|existing| models_are_equal(Some(&existing.model), Some(&model)))
            {
                scoped_models.push(ScopedModel {
                    model,
                    thinking_level: parsed.thinking_level,
                });
            }
        }
    }

    scoped_models
}

pub fn resolve_cli_model(
    cli_provider: Option<&str>,
    cli_model: Option<&str>,
    model_registry: &ModelRegistry,
) -> ResolveCliModelResult {
    let Some(cli_model) = cli_model else {
        return ResolveCliModelResult {
            model: None,
            thinking_level: None,
            warning: None,
            error: None,
        };
    };

    let available_models = model_registry.get_all();
    if available_models.is_empty() {
        return ResolveCliModelResult {
            model: None,
            thinking_level: None,
            warning: None,
            error: Some(
                "No models available. Check your installation or add models to models.json."
                    .to_string(),
            ),
        };
    }

    let mut provider_map = BTreeMap::new();
    for model in &available_models {
        provider_map.insert(model.provider.0.to_lowercase(), model.provider.0.clone());
    }

    let provider = match cli_provider {
        Some(provider) => match provider_map.get(&provider.to_lowercase()) {
            Some(provider) => Some(provider.clone()),
            None => {
                return ResolveCliModelResult {
                    model: None,
                    thinking_level: None,
                    warning: None,
                    error: Some(format!(
                        "Unknown provider \"{provider}\". Use --list-models to see available providers/models."
                    )),
                };
            }
        },
        None => None,
    };

    if provider.is_none() {
        let lower = cli_model.to_lowercase();
        if let Some(exact) = available_models.iter().find(|model| {
            model.id.to_lowercase() == lower
                || format!("{}/{}", model.provider.0, model.id).to_lowercase() == lower
        }) {
            return ResolveCliModelResult {
                model: Some(exact.clone()),
                thinking_level: None,
                warning: None,
                error: None,
            };
        }
    }

    let mut provider = provider;
    let mut pattern = cli_model.to_string();
    if provider.is_none() {
        if let Some((candidate_provider, suffix)) = cli_model.split_once('/') {
            if let Some(canonical) = provider_map.get(&candidate_provider.to_lowercase()) {
                provider = Some(canonical.clone());
                pattern = suffix.to_string();
            }
        }
    } else if let Some(selected_provider) = &provider {
        let prefix = format!("{selected_provider}/");
        if cli_model.to_lowercase().starts_with(&prefix.to_lowercase()) {
            pattern = cli_model[prefix.len()..].to_string();
        }
    }

    let candidates = provider
        .as_ref()
        .map(|provider| {
            available_models
                .iter()
                .filter(|model| model.provider.0 == *provider)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| available_models.clone());

    let parsed = parse_model_pattern(&pattern, &candidates, false);
    if let Some(model) = parsed.model {
        return ResolveCliModelResult {
            model: Some(model),
            thinking_level: parsed.thinking_level,
            warning: parsed.warning,
            error: None,
        };
    }

    let display = provider
        .map(|provider| format!("{provider}/{pattern}"))
        .unwrap_or_else(|| cli_model.to_string());
    ResolveCliModelResult {
        model: None,
        thinking_level: None,
        warning: parsed.warning,
        error: Some(format!(
            "Model \"{display}\" not found. Use --list-models to see available models."
        )),
    }
}

fn load_custom_models(path: &Path) -> Result<CustomModelsResult, ModelRegistryError> {
    if !path.exists() {
        return Ok(CustomModelsResult::default());
    }

    let content = fs::read_to_string(path).map_err(ModelRegistryError::Io)?;
    let config: ModelsConfig = serde_json::from_str(&content)?;
    validate_config(&config)?;

    let mut result = CustomModelsResult::default();
    for (provider_name, provider_config) in &config.providers {
        if provider_config.base_url.is_some()
            || provider_config.headers.is_some()
            || provider_config.api_key.is_some()
        {
            result.overrides.insert(
                provider_name.clone(),
                ProviderOverride {
                    base_url: provider_config.base_url.clone(),
                    headers: provider_config.headers.clone(),
                    api_key: provider_config.api_key.clone(),
                },
            );
        }
        if !provider_config.model_overrides.is_empty() {
            result.model_overrides.insert(
                provider_name.clone(),
                provider_config.model_overrides.clone(),
            );
        }
        if provider_config.models.is_empty() {
            continue;
        }

        let provider_headers = resolve_headers(provider_config.headers.as_ref());
        for definition in &provider_config.models {
            let api = definition
                .api
                .clone()
                .or_else(|| provider_config.api.clone())
                .ok_or_else(|| {
                    ModelRegistryError::Invalid(format!(
                        "Provider {provider_name}, model {}: no \"api\" specified.",
                        definition.id
                    ))
                })?;
            let mut headers = to_hash_map(provider_headers.as_ref());
            if let Some(model_headers) = resolve_headers(definition.headers.as_ref()) {
                headers.extend(model_headers);
            }
            if provider_config.auth_header {
                if let Some(api_key) = provider_config
                    .api_key
                    .as_deref()
                    .and_then(resolve_config_value)
                {
                    headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
                }
            }

            result.models.push(Model {
                id: definition.id.clone(),
                name: definition
                    .name
                    .clone()
                    .unwrap_or_else(|| definition.id.clone()),
                api: ApiId::new(api),
                provider: ProviderId::new(provider_name.clone()),
                base_url: provider_config.base_url.clone().unwrap_or_default(),
                reasoning: definition.reasoning.unwrap_or(false),
                input: definition
                    .input
                    .clone()
                    .unwrap_or_else(|| vec![ModelInput::Text]),
                cost: definition.cost.clone().unwrap_or(ModelCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                }),
                context_window: definition.context_window.unwrap_or(128_000),
                max_tokens: definition.max_tokens.unwrap_or(16_384),
                headers: if headers.is_empty() {
                    None
                } else {
                    Some(headers)
                },
                compat: definition.compat.clone(),
            });
        }
    }

    Ok(result)
}

fn validate_config(config: &ModelsConfig) -> Result<(), ModelRegistryError> {
    for (provider_name, provider_config) in &config.providers {
        if provider_config.models.is_empty() {
            if provider_config.base_url.is_none() && provider_config.model_overrides.is_empty() {
                return Err(ModelRegistryError::Invalid(format!(
                    "Provider {provider_name}: must specify \"baseUrl\", \"modelOverrides\", or \"models\"."
                )));
            }
        } else {
            if provider_config.base_url.is_none() {
                return Err(ModelRegistryError::Invalid(format!(
                    "Provider {provider_name}: \"baseUrl\" is required when defining custom models."
                )));
            }
            if provider_config.api_key.is_none() {
                return Err(ModelRegistryError::Invalid(format!(
                    "Provider {provider_name}: \"apiKey\" is required when defining custom models."
                )));
            }
        }

        for model in &provider_config.models {
            if model.id.trim().is_empty() {
                return Err(ModelRegistryError::Invalid(format!(
                    "Provider {provider_name}: model missing \"id\"."
                )));
            }
            if provider_config.api.is_none() && model.api.is_none() {
                return Err(ModelRegistryError::Invalid(format!(
                    "Provider {provider_name}, model {}: no \"api\" specified. Set at provider or model level.",
                    model.id
                )));
            }
            if model.context_window == Some(0) {
                return Err(ModelRegistryError::Invalid(format!(
                    "Provider {provider_name}, model {}: invalid contextWindow",
                    model.id
                )));
            }
            if model.max_tokens == Some(0) {
                return Err(ModelRegistryError::Invalid(format!(
                    "Provider {provider_name}, model {}: invalid maxTokens",
                    model.id
                )));
            }
        }
    }
    Ok(())
}

fn load_built_in_models(
    overrides: &BTreeMap<String, ProviderOverride>,
    model_overrides: &BTreeMap<String, BTreeMap<String, ModelOverride>>,
) -> Vec<Model> {
    built_in_models()
        .into_iter()
        .map(|model| {
            let mut model = model;
            if let Some(provider_override) = overrides.get(&model.provider.0) {
                if let Some(base_url) = &provider_override.base_url {
                    model.base_url = base_url.clone();
                }
                let resolved_headers =
                    to_hash_map(resolve_headers(provider_override.headers.as_ref()).as_ref());
                if !resolved_headers.is_empty() {
                    let mut headers = model.headers.unwrap_or_default();
                    headers.extend(resolved_headers);
                    model.headers = Some(headers);
                }
            }
            if let Some(overrides_for_provider) = model_overrides.get(&model.provider.0) {
                if let Some(override_model) = overrides_for_provider.get(&model.id) {
                    model = apply_model_override(model, override_model);
                }
            }
            model
        })
        .collect()
}

fn apply_model_override(mut model: Model, override_model: &ModelOverride) -> Model {
    if let Some(name) = &override_model.name {
        model.name = name.clone();
    }
    if let Some(reasoning) = override_model.reasoning {
        model.reasoning = reasoning;
    }
    if let Some(input) = &override_model.input {
        model.input = input.clone();
    }
    if let Some(context_window) = override_model.context_window {
        model.context_window = context_window;
    }
    if let Some(max_tokens) = override_model.max_tokens {
        model.max_tokens = max_tokens;
    }
    if let Some(cost) = &override_model.cost {
        model.cost = ModelCost {
            input: cost.input.unwrap_or(model.cost.input),
            output: cost.output.unwrap_or(model.cost.output),
            cache_read: cost.cache_read.unwrap_or(model.cost.cache_read),
            cache_write: cost.cache_write.unwrap_or(model.cost.cache_write),
        };
    }
    if let Some(headers) = resolve_headers(override_model.headers.as_ref()) {
        let mut merged_headers = model.headers.unwrap_or_default();
        merged_headers.extend(headers);
        model.headers = Some(merged_headers);
    }
    model.compat = merge_compat(model.compat, override_model.compat.clone());
    model
}

fn merge_custom_models(built_in_models: Vec<Model>, custom_models: Vec<Model>) -> Vec<Model> {
    let mut merged = built_in_models;
    for custom_model in custom_models {
        if let Some(index) = merged.iter().position(|model| {
            model.provider == custom_model.provider && model.id == custom_model.id
        }) {
            merged[index] = custom_model;
        } else {
            merged.push(custom_model);
        }
    }
    merged
}

fn merge_compat(base: Option<Value>, override_value: Option<Value>) -> Option<Value> {
    match (base, override_value) {
        (None, None) => None,
        (Some(base), None) => Some(base),
        (None, Some(override_value)) => Some(override_value),
        (Some(Value::Object(mut base)), Some(Value::Object(override_value))) => {
            deep_merge_object(&mut base, &override_value);
            Some(Value::Object(base))
        }
        (_, Some(override_value)) => Some(override_value),
    }
}

fn deep_merge_object(base: &mut Map<String, Value>, override_value: &Map<String, Value>) {
    for (key, value) in override_value {
        match (base.get_mut(key), value) {
            (Some(Value::Object(base_child)), Value::Object(override_child)) => {
                deep_merge_object(base_child, override_child);
            }
            _ => {
                base.insert(key.clone(), value.clone());
            }
        }
    }
}

fn built_in_models() -> Vec<Model> {
    vec![
        build_model(
            "anthropic",
            "anthropic-messages",
            "claude-haiku-4-5",
            "Claude Haiku 4.5",
            true,
            200_000,
            8_192,
        ),
        build_model(
            "anthropic",
            "anthropic-messages",
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            true,
            200_000,
            8_192,
        ),
        build_model(
            "anthropic",
            "anthropic-messages",
            "claude-opus-4-6",
            "Claude Opus 4.6",
            true,
            200_000,
            8_192,
        ),
        build_model(
            "openai",
            "openai-responses",
            "gpt-4.1",
            "GPT-4.1",
            true,
            128_000,
            16_384,
        ),
        build_model_with_spec(
            "openai",
            "openai-responses",
            "gpt-5.1-codex",
            "GPT-5.1 Codex",
            true,
            vec![ModelInput::Text, ModelInput::Image],
            ModelCost {
                input: 1.25,
                output: 10.0,
                cache_read: 0.125,
                cache_write: 0.0,
            },
            400_000,
            128_000,
        ),
        build_model(
            "openai-codex",
            "openai-codex-responses",
            "gpt-5.3-codex",
            "GPT-5.3 Codex",
            true,
            200_000,
            32_768,
        ),
        build_model_with_spec(
            "openrouter",
            "openai-completions",
            "openai/gpt-5.1-codex",
            "OpenRouter GPT-5.1 Codex",
            true,
            vec![ModelInput::Text, ModelInput::Image],
            ModelCost {
                input: 1.25,
                output: 10.0,
                cache_read: 0.125,
                cache_write: 0.0,
            },
            400_000,
            128_000,
        ),
    ]
}

fn build_model(
    provider: &str,
    api: &str,
    id: &str,
    name: &str,
    reasoning: bool,
    context_window: u32,
    max_tokens: u32,
) -> Model {
    build_model_with_spec(
        provider,
        api,
        id,
        name,
        reasoning,
        vec![ModelInput::Text],
        zero_model_cost(),
        context_window,
        max_tokens,
    )
}

fn build_model_with_spec(
    provider: &str,
    api: &str,
    id: &str,
    name: &str,
    reasoning: bool,
    input: Vec<ModelInput>,
    cost: ModelCost,
    context_window: u32,
    max_tokens: u32,
) -> Model {
    Model {
        id: id.to_string(),
        name: name.to_string(),
        api: ApiId::new(api),
        provider: ProviderId::new(provider),
        base_url: default_base_url(provider).to_string(),
        reasoning,
        input,
        cost,
        context_window,
        max_tokens,
        headers: None,
        compat: None,
    }
}

fn zero_model_cost() -> ModelCost {
    ModelCost {
        input: 0.0,
        output: 0.0,
        cache_read: 0.0,
        cache_write: 0.0,
    }
}

fn default_base_url(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "https://api.anthropic.com",
        "openai" => "https://api.openai.com/v1",
        "openai-codex" => "https://chatgpt.com/backend-api",
        "openrouter" => "https://openrouter.ai/api/v1",
        _ => "https://example.com",
    }
}

fn try_match_model(model_pattern: &str, available_models: &[Model]) -> Option<Model> {
    if let Some((provider, model_id)) = model_pattern.split_once('/') {
        if let Some(provider_match) = available_models.iter().find(|model| {
            model.provider.0.eq_ignore_ascii_case(provider)
                && model.id.eq_ignore_ascii_case(model_id)
        }) {
            return Some(provider_match.clone());
        }
    }

    if let Some(exact_match) = available_models
        .iter()
        .find(|model| model.id.eq_ignore_ascii_case(model_pattern))
    {
        return Some(exact_match.clone());
    }

    let mut matches = available_models
        .iter()
        .filter(|model| {
            contains_case_insensitive(&model.id, model_pattern)
                || contains_case_insensitive(&model.name, model_pattern)
        })
        .cloned()
        .collect::<Vec<_>>();

    if matches.is_empty() {
        return None;
    }

    matches.sort_by(|left, right| right.id.cmp(&left.id));
    matches
        .iter()
        .find(|model| is_alias(&model.id))
        .cloned()
        .or_else(|| matches.first().cloned())
}

fn split_optional_thinking_suffix(pattern: &str) -> (String, Option<String>) {
    let Some(last_colon_index) = pattern.rfind(':') else {
        return (pattern.to_string(), None);
    };
    let suffix = &pattern[last_colon_index + 1..];
    if is_valid_thinking_level(suffix) {
        (
            pattern[..last_colon_index].to_string(),
            Some(suffix.to_string()),
        )
    } else {
        (pattern.to_string(), None)
    }
}

fn contains_glob(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

fn contains_case_insensitive(value: &str, needle: &str) -> bool {
    value.to_lowercase().contains(&needle.to_lowercase())
}

fn is_alias(id: &str) -> bool {
    if id.ends_with("-latest") {
        return true;
    }

    !matches!(
        id.rsplit_once('-'),
        Some((_, suffix)) if suffix.len() == 8 && suffix.chars().all(|char| char.is_ascii_digit())
    )
}

fn is_valid_thinking_level(value: &str) -> bool {
    matches!(
        value,
        "off" | "minimal" | "low" | "medium" | "high" | "xhigh"
    )
}

fn to_hash_map(headers: Option<&BTreeMap<String, String>>) -> HashMap<String, String> {
    headers
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect::<HashMap<_, _>>()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pi_rust_oauth::AuthStorage;
    use tempfile::tempdir;

    use super::{
        ModelRegistry, default_model_for_provider, models_are_equal, parse_model_pattern,
        resolve_cli_model, resolve_model_scope, supports_xhigh,
    };

    #[test]
    fn returns_expected_provider_defaults() {
        assert_eq!(default_model_for_provider("openai"), Some("gpt-5.1-codex"));
        assert_eq!(default_model_for_provider("missing"), None);
    }

    #[test]
    fn built_in_models_use_typescript_base_urls_and_apis() {
        let registry = ModelRegistry::new(AuthStorage::in_memory(Default::default()), None);

        let openai = registry
            .find("openai", "gpt-5.1-codex")
            .expect("openai model");
        assert_eq!(openai.base_url, "https://api.openai.com/v1");
        assert_eq!(openai.api.0, "openai-responses");

        let codex = registry
            .find("openai-codex", "gpt-5.3-codex")
            .expect("codex model");
        assert_eq!(codex.base_url, "https://chatgpt.com/backend-api");
        assert_eq!(codex.api.0, "openai-codex-responses");

        let openrouter = registry
            .find("openrouter", "openai/gpt-5.1-codex")
            .expect("openrouter model");
        assert_eq!(openrouter.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(openrouter.api.0, "openai-completions");

        let anthropic = registry
            .find("anthropic", "claude-opus-4-6")
            .expect("anthropic model");
        assert_eq!(anthropic.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn detects_xhigh_support() {
        let model_registry = ModelRegistry::new(AuthStorage::in_memory(Default::default()), None);
        let openai_model = model_registry
            .find("openai", "gpt-5.1-codex")
            .expect("openai model");
        let anthropic_model = model_registry
            .find("anthropic", "claude-opus-4-6")
            .expect("anthropic model");
        assert!(supports_xhigh(&anthropic_model));
        assert!(!supports_xhigh(&openai_model));
    }

    #[test]
    fn compares_model_identity_by_provider_and_id() {
        let model_registry = ModelRegistry::new(AuthStorage::in_memory(Default::default()), None);
        let left = model_registry
            .find("openai", "gpt-5.1-codex")
            .expect("left");
        let right = model_registry
            .find("openai", "gpt-5.1-codex")
            .expect("right");
        let different_provider = model_registry
            .find("openrouter", "openai/gpt-5.1-codex")
            .expect("different provider");
        assert!(models_are_equal(Some(&left), Some(&right)));
        assert!(!models_are_equal(Some(&left), Some(&different_provider)));
        assert!(!models_are_equal(Some(&left), None));
    }

    #[test]
    fn loads_custom_models_and_provider_overrides() {
        let tempdir = tempdir().expect("tempdir");
        let models_json_path = tempdir.path().join("models.json");
        fs::write(
            &models_json_path,
            r#"{
              "providers": {
                "openai": {
                  "baseUrl": "https://example.com/openai",
                  "modelOverrides": {
                    "gpt-5.1-codex": { "name": "Renamed GPT", "maxTokens": 9999 }
                  }
                },
                "custom": {
                  "baseUrl": "https://example.com/custom",
                  "apiKey": "CUSTOM_KEY",
                  "api": "openai-responses",
                  "models": [
                    { "id": "local-1" }
                  ]
                }
              }
            }"#,
        )
        .expect("write models.json");

        unsafe { std::env::set_var("CUSTOM_KEY", "custom-secret") };
        let registry = ModelRegistry::new(
            AuthStorage::in_memory(Default::default()),
            Some(models_json_path),
        );

        let overridden = registry
            .find("openai", "gpt-5.1-codex")
            .expect("overridden model");
        assert_eq!(overridden.name, "Renamed GPT");
        assert_eq!(overridden.base_url, "https://example.com/openai");
        assert_eq!(overridden.max_tokens, 9999);

        let custom = registry.find("custom", "local-1").expect("custom model");
        assert_eq!(custom.base_url, "https://example.com/custom");
        assert_eq!(
            registry.get_api_key_for_provider("custom").as_deref(),
            Some("custom-secret")
        );
        unsafe { std::env::remove_var("CUSTOM_KEY") };
    }

    #[test]
    fn resolve_cli_model_handles_provider_prefix_and_invalid_thinking_suffix() {
        let registry = ModelRegistry::new(AuthStorage::in_memory(Default::default()), None);

        let resolved = resolve_cli_model(None, Some("openai/gpt-5.1-codex"), &registry);
        assert_eq!(resolved.model.expect("resolved model").provider.0, "openai");

        let invalid = resolve_cli_model(None, Some("gpt-5.1-codex:not-a-level"), &registry);
        assert!(invalid.model.is_none());
        assert!(invalid.error.is_some());

        let valid = resolve_cli_model(None, Some("gpt-5.1-codex:high"), &registry);
        assert_eq!(valid.thinking_level.as_deref(), Some("high"));
    }

    #[test]
    fn resolve_model_scope_uses_available_models_only() {
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let registry = ModelRegistry::new(auth, None);

        let scoped = resolve_model_scope(
            &[
                "openai/*".to_string(),
                "claude-opus-4-6".to_string(),
                "gpt-5.1-codex:high".to_string(),
            ],
            &registry,
        );

        assert!(
            scoped
                .iter()
                .any(|model| model.model.provider.0 == "openai")
        );
        assert!(
            !scoped
                .iter()
                .any(|model| model.model.provider.0 == "anthropic")
        );
        assert_eq!(
            scoped
                .iter()
                .find(|model| model.model.id == "gpt-5.1-codex")
                .and_then(|model| model.thinking_level.as_deref()),
            None
        );
    }

    #[test]
    fn parse_model_pattern_prefers_aliases() {
        let registry = ModelRegistry::new(AuthStorage::in_memory(Default::default()), None);
        let mut models = registry.get_all();
        let alias = models
            .iter()
            .find(|model| model.id == "gpt-5.1-codex")
            .expect("alias model")
            .clone();
        let mut dated = alias.clone();
        dated.id = "gpt-5.1-codex-20250929".to_string();
        models.push(dated);

        let resolved = parse_model_pattern("gpt-5.1-codex", &models, false);
        assert_eq!(resolved.model.expect("resolved").id, "gpt-5.1-codex");
    }
}
