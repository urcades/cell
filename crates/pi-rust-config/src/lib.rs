use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use thiserror::Error;

pub const APP_NAME: &str = "pi-rust";
pub const CONFIG_DIR_NAME: &str = ".pi-rust";
pub const PROJECT_CONFIG_DIR_NAME: &str = ".pi";
pub const ENV_AGENT_DIR: &str = "PI_RUST_CODING_AGENT_DIR";
pub const DEFAULT_SHARE_VIEWER_URL: &str = "https://pi.dev/session/";

pub fn expand_tilde(input: &str) -> PathBuf {
    if input == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(input));
    }
    if let Some(suffix) = input.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(suffix);
        }
    }
    PathBuf::from(input)
}

pub fn get_agent_dir() -> PathBuf {
    if let Ok(value) = env::var(ENV_AGENT_DIR) {
        return expand_tilde(&value);
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(CONFIG_DIR_NAME)
        .join("agent")
}

pub fn get_settings_path() -> PathBuf {
    get_agent_dir().join("settings.json")
}

pub fn get_project_config_dir(cwd: impl AsRef<Path>) -> PathBuf {
    cwd.as_ref().join(PROJECT_CONFIG_DIR_NAME)
}

pub fn get_project_settings_path(cwd: impl AsRef<Path>) -> PathBuf {
    get_project_config_dir(cwd).join("settings.json")
}

pub fn get_models_path() -> PathBuf {
    get_agent_dir().join("models.json")
}

pub fn get_auth_path() -> PathBuf {
    get_agent_dir().join("auth.json")
}

pub fn get_prompts_dir() -> PathBuf {
    get_agent_dir().join("prompts")
}

pub fn get_sessions_dir() -> PathBuf {
    get_agent_dir().join("sessions")
}

pub fn get_share_viewer_url(gist_id: &str) -> String {
    let base =
        env::var("PI_SHARE_VIEWER_URL").unwrap_or_else(|_| DEFAULT_SHARE_VIEWER_URL.to_string());
    format!("{base}#{gist_id}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsScope {
    Global,
    Project,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsLayers {
    pub global: Value,
    pub project: Value,
    pub runtime_overrides: Value,
}

impl Default for SettingsLayers {
    fn default() -> Self {
        Self {
            global: empty_settings(),
            project: empty_settings(),
            runtime_overrides: empty_settings(),
        }
    }
}

impl SettingsLayers {
    pub fn merged(&self) -> Value {
        let merged = deep_merge_settings(&self.global, &self.project);
        deep_merge_settings(&merged, &self.runtime_overrides)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsLoadError {
    pub scope: SettingsScope,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum SettingsManagerError {
    #[error("failed to read settings file {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("failed to parse settings file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("settings value must be a JSON object at {path}")]
    InvalidRoot { path: PathBuf },
    #[error("failed to serialize settings: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Clone, Debug)]
pub struct SettingsManager {
    global_settings_path: PathBuf,
    project_settings_path: PathBuf,
    layers: SettingsLayers,
    errors: Vec<SettingsLoadError>,
}

impl SettingsManager {
    pub fn create(cwd: impl AsRef<Path>, agent_dir: Option<PathBuf>) -> Self {
        let global_settings_path = agent_dir
            .unwrap_or_else(get_agent_dir)
            .join("settings.json");
        let project_settings_path = get_project_settings_path(cwd);
        Self::from_paths(global_settings_path, project_settings_path)
    }

    pub fn from_paths(global_settings_path: PathBuf, project_settings_path: PathBuf) -> Self {
        let mut manager = Self {
            global_settings_path,
            project_settings_path,
            layers: SettingsLayers::default(),
            errors: Vec::new(),
        };
        manager.reload();
        manager
    }

    pub fn global_settings_path(&self) -> &Path {
        &self.global_settings_path
    }

    pub fn project_settings_path(&self) -> &Path {
        &self.project_settings_path
    }

    pub fn layers(&self) -> &SettingsLayers {
        &self.layers
    }

    pub fn scoped_settings(&self, scope: SettingsScope) -> &Value {
        match scope {
            SettingsScope::Global => &self.layers.global,
            SettingsScope::Project => &self.layers.project,
        }
    }

    pub fn merged_settings(&self) -> Value {
        self.layers.merged()
    }

    pub fn reload(&mut self) {
        self.errors.clear();

        let (global, global_error) =
            load_settings_with_fallback(&self.global_settings_path, SettingsScope::Global);
        if let Some(error) = global_error {
            self.errors.push(error);
        }
        self.layers.global = global;

        let (project, project_error) =
            load_settings_with_fallback(&self.project_settings_path, SettingsScope::Project);
        if let Some(error) = project_error {
            self.errors.push(error);
        }
        self.layers.project = project;
    }

    pub fn drain_errors(&mut self) -> Vec<SettingsLoadError> {
        std::mem::take(&mut self.errors)
    }

    pub fn apply_runtime_overrides(
        &mut self,
        overrides: Value,
    ) -> Result<(), SettingsManagerError> {
        ensure_object(&overrides, Path::new("<runtime-overrides>"))?;
        self.layers.runtime_overrides =
            deep_merge_settings(&self.layers.runtime_overrides, &overrides);
        Ok(())
    }

    pub fn clear_runtime_overrides(&mut self) {
        self.layers.runtime_overrides = empty_settings();
    }

    pub fn update_global_settings(&mut self, patch: Value) -> Result<(), SettingsManagerError> {
        self.update_scoped_settings(SettingsScope::Global, patch)
    }

    pub fn update_project_settings(&mut self, patch: Value) -> Result<(), SettingsManagerError> {
        self.update_scoped_settings(SettingsScope::Project, patch)
    }

    pub fn replace_global_settings(&mut self, settings: Value) -> Result<(), SettingsManagerError> {
        self.replace_scoped_settings(SettingsScope::Global, settings)
    }

    pub fn replace_project_settings(
        &mut self,
        settings: Value,
    ) -> Result<(), SettingsManagerError> {
        self.replace_scoped_settings(SettingsScope::Project, settings)
    }

    pub fn get_string_list(&self, key: &str, scope: Option<SettingsScope>) -> Vec<String> {
        let value = match scope {
            Some(scope) => self.scoped_settings(scope),
            None => &self.layers.merged(),
        };
        extract_string_list(value.get(key))
    }

    pub fn get_string(&self, key: &str, scope: Option<SettingsScope>) -> Option<String> {
        let merged;
        let value = match scope {
            Some(scope) => self.scoped_settings(scope),
            None => {
                merged = self.layers.merged();
                &merged
            }
        };
        value
            .get(key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    pub fn get_bool(&self, key: &str, scope: Option<SettingsScope>) -> Option<bool> {
        let merged;
        let value = match scope {
            Some(scope) => self.scoped_settings(scope),
            None => {
                merged = self.layers.merged();
                &merged
            }
        };
        value.get(key).and_then(Value::as_bool)
    }

    pub fn get_optional_string_list(
        &self,
        key: &str,
        scope: Option<SettingsScope>,
    ) -> Option<Vec<String>> {
        let merged;
        let value = match scope {
            Some(scope) => self.scoped_settings(scope),
            None => {
                merged = self.layers.merged();
                &merged
            }
        };
        value.get(key).map(|entry| extract_string_list(Some(entry)))
    }

    pub fn set_string_list(
        &mut self,
        scope: SettingsScope,
        key: &str,
        values: &[String],
    ) -> Result<(), SettingsManagerError> {
        let mut next = self.scoped_settings(scope).clone();
        let object = next
            .as_object_mut()
            .expect("settings root must remain an object");
        object.insert(
            key.to_string(),
            Value::Array(values.iter().cloned().map(Value::String).collect()),
        );
        self.replace_scoped_settings(scope, next)
    }

    pub fn set_optional_string_list(
        &mut self,
        scope: SettingsScope,
        key: &str,
        values: Option<&[String]>,
    ) -> Result<(), SettingsManagerError> {
        let mut next = self.scoped_settings(scope).clone();
        let object = next
            .as_object_mut()
            .expect("settings root must remain an object");
        match values {
            Some(values) => {
                object.insert(
                    key.to_string(),
                    Value::Array(values.iter().cloned().map(Value::String).collect()),
                );
            }
            None => {
                object.remove(key);
            }
        }
        self.replace_scoped_settings(scope, next)
    }

    pub fn get_default_provider(&self) -> Option<String> {
        self.get_string("defaultProvider", None)
    }

    pub fn get_default_model(&self) -> Option<String> {
        self.get_string("defaultModel", None)
    }

    pub fn set_default_model_and_provider(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<(), SettingsManagerError> {
        let mut next = self.scoped_settings(SettingsScope::Global).clone();
        let object = next
            .as_object_mut()
            .expect("settings root must remain an object");
        object.insert(
            "defaultProvider".to_string(),
            Value::String(provider.to_string()),
        );
        object.insert(
            "defaultModel".to_string(),
            Value::String(model_id.to_string()),
        );
        self.replace_scoped_settings(SettingsScope::Global, next)
    }

    pub fn get_enabled_models(&self, scope: Option<SettingsScope>) -> Option<Vec<String>> {
        self.get_optional_string_list("enabledModels", scope)
    }

    pub fn set_enabled_models(
        &mut self,
        scope: SettingsScope,
        patterns: Option<&[String]>,
    ) -> Result<(), SettingsManagerError> {
        self.set_optional_string_list(scope, "enabledModels", patterns)
    }

    pub fn get_enable_skill_commands(&self) -> bool {
        self.get_bool("enableSkillCommands", None).unwrap_or(true)
    }

    fn update_scoped_settings(
        &mut self,
        scope: SettingsScope,
        patch: Value,
    ) -> Result<(), SettingsManagerError> {
        ensure_object(&patch, Path::new("<settings-patch>"))?;
        match scope {
            SettingsScope::Global => {
                self.layers.global = deep_merge_settings(&self.layers.global, &patch);
                persist_settings(&self.global_settings_path, &self.layers.global)
            }
            SettingsScope::Project => {
                self.layers.project = deep_merge_settings(&self.layers.project, &patch);
                persist_settings(&self.project_settings_path, &self.layers.project)
            }
        }
    }

    fn replace_scoped_settings(
        &mut self,
        scope: SettingsScope,
        settings: Value,
    ) -> Result<(), SettingsManagerError> {
        ensure_object(&settings, Path::new("<settings>"))?;
        match scope {
            SettingsScope::Global => {
                self.layers.global = settings;
                persist_settings(&self.global_settings_path, &self.layers.global)
            }
            SettingsScope::Project => {
                self.layers.project = settings;
                persist_settings(&self.project_settings_path, &self.layers.project)
            }
        }
    }
}

pub fn deep_merge_settings(base: &Value, overrides: &Value) -> Value {
    if !base.is_object() || !overrides.is_object() {
        return overrides.clone();
    }

    let mut merged = base.clone();
    merge_in_place(&mut merged, overrides);
    merged
}

pub fn migrate_settings(value: &mut Value) -> Result<(), SettingsManagerError> {
    ensure_object(value, Path::new("<settings>"))?;
    let object = value
        .as_object_mut()
        .expect("ensure_object guarantees root object");

    if !object.contains_key("steeringMode") {
        if let Some(queue_mode) = object.remove("queueMode") {
            object.insert("steeringMode".to_string(), queue_mode);
        }
    }

    if !object.contains_key("transport") {
        if let Some(websockets) = object.get("websockets").and_then(Value::as_bool) {
            object.remove("websockets");
            object.insert(
                "transport".to_string(),
                Value::String(if websockets { "websocket" } else { "sse" }.to_string()),
            );
        }
    }

    if let Some(skills_settings) = object.get("skills").and_then(Value::as_object).cloned() {
        if !object.contains_key("enableSkillCommands") {
            if let Some(value) = skills_settings
                .get("enableSkillCommands")
                .and_then(Value::as_bool)
            {
                object.insert("enableSkillCommands".to_string(), Value::Bool(value));
            }
        }

        let custom_directories = skills_settings
            .get("customDirectories")
            .and_then(Value::as_array)
            .cloned();
        match custom_directories {
            Some(entries) if !entries.is_empty() => {
                object.insert("skills".to_string(), Value::Array(entries));
            }
            _ => {
                object.remove("skills");
            }
        }
    }

    Ok(())
}

fn load_settings_with_fallback(
    path: &Path,
    scope: SettingsScope,
) -> (Value, Option<SettingsLoadError>) {
    match load_settings(path) {
        Ok(value) => (value, None),
        Err(error) => (
            empty_settings(),
            Some(SettingsLoadError {
                scope,
                message: error.to_string(),
            }),
        ),
    }
}

fn load_settings(path: &Path) -> Result<Value, SettingsManagerError> {
    if !path.exists() {
        return Ok(empty_settings());
    }

    let content = fs::read_to_string(path).map_err(|source| SettingsManagerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if content.trim().is_empty() {
        return Ok(empty_settings());
    }

    let mut value: Value =
        serde_json::from_str(&content).map_err(|source| SettingsManagerError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    ensure_object(&value, path)?;
    migrate_settings(&mut value)?;
    Ok(value)
}

fn persist_settings(path: &Path, value: &Value) -> Result<(), SettingsManagerError> {
    ensure_object(value, path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SettingsManagerError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let payload = serde_json::to_string_pretty(value)?;
    fs::write(path, payload).map_err(|source| SettingsManagerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn ensure_object(value: &Value, path: &Path) -> Result<(), SettingsManagerError> {
    if value.is_object() {
        Ok(())
    } else {
        Err(SettingsManagerError::InvalidRoot {
            path: path.to_path_buf(),
        })
    }
}

fn merge_in_place(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, source_value) in source_map {
                match target_map.get_mut(key) {
                    Some(target_value) => merge_in_place(target_value, source_value),
                    None => {
                        target_map.insert(key.clone(), source_value.clone());
                    }
                }
            }
        }
        (target_slot, source_value) => {
            *target_slot = source_value.clone();
        }
    }
}

fn empty_settings() -> Value {
    Value::Object(Map::new())
}

fn extract_string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn env_guard() -> &'static Mutex<()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(()))
    }

    fn read_json_file(path: &Path) -> Value {
        let content = fs::read_to_string(path).expect("read file");
        serde_json::from_str(&content).expect("parse json")
    }

    #[test]
    fn expands_tilde_paths() {
        let _guard = env_guard().lock().expect("lock");
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        assert_eq!(expand_tilde("~"), PathBuf::from(home.clone()));
        assert_eq!(expand_tilde("~/agent"), PathBuf::from(home).join("agent"));
    }

    #[test]
    fn honors_agent_dir_env_override() {
        let _guard = env_guard().lock().expect("lock");
        let original = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, "~/custom-agent") };
        let resolved = get_agent_dir();
        let expected = expand_tilde("~/custom-agent");
        assert_eq!(resolved, expected);
        match original {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }
    }

    #[test]
    fn project_settings_path_uses_pi_directory() {
        let cwd = PathBuf::from("/tmp/workspace");
        assert_eq!(get_project_config_dir(&cwd), cwd.join(".pi"));
        assert_eq!(
            get_project_settings_path(&cwd),
            cwd.join(".pi/settings.json")
        );
    }

    #[test]
    fn deep_merge_keeps_nested_object_fields_and_overrides_arrays() {
        let base = json!({
            "terminal": {
                "showImages": true,
                "clearOnShrink": false
            },
            "skills": ["a"],
            "theme": "light"
        });
        let overrides = json!({
            "terminal": {
                "clearOnShrink": true
            },
            "skills": ["b"],
            "theme": "dark"
        });

        let merged = deep_merge_settings(&base, &overrides);
        assert_eq!(merged["terminal"]["showImages"], Value::Bool(true));
        assert_eq!(merged["terminal"]["clearOnShrink"], Value::Bool(true));
        assert_eq!(merged["skills"], json!(["b"]));
        assert_eq!(merged["theme"], Value::String("dark".to_string()));
    }

    #[test]
    fn migrates_legacy_settings_shape() {
        let mut settings = json!({
            "queueMode": "all",
            "websockets": true,
            "skills": {
                "enableSkillCommands": false,
                "customDirectories": ["./skills"]
            }
        });

        migrate_settings(&mut settings).expect("migrate settings");

        assert_eq!(settings["steeringMode"], Value::String("all".to_string()));
        assert_eq!(
            settings["transport"],
            Value::String("websocket".to_string())
        );
        assert_eq!(settings["enableSkillCommands"], Value::Bool(false));
        assert_eq!(settings["skills"], json!(["./skills"]));
        assert!(settings.get("queueMode").is_none());
        assert!(settings.get("websockets").is_none());
    }

    #[test]
    fn layered_manager_merges_global_project_and_runtime_overrides() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        fs::write(
            &global_path,
            r#"{"theme":"global","terminal":{"showImages":false}}"#,
        )
        .expect("write global");
        fs::write(
            &project_path,
            r#"{"theme":"project","terminal":{"clearOnShrink":true}}"#,
        )
        .expect("write project");

        let mut manager = SettingsManager::from_paths(global_path, project_path);
        manager
            .apply_runtime_overrides(json!({"theme":"runtime","terminal":{"showImages":true}}))
            .expect("apply overrides");

        let merged = manager.merged_settings();
        assert_eq!(merged["theme"], Value::String("runtime".to_string()));
        assert_eq!(merged["terminal"]["showImages"], Value::Bool(true));
        assert_eq!(merged["terminal"]["clearOnShrink"], Value::Bool(true));
    }

    #[test]
    fn scoped_update_persists_and_preserves_existing_nested_fields() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        fs::write(
            &global_path,
            r#"{"theme":"light","terminal":{"showImages":true},"other":{"x":1}}"#,
        )
        .expect("write global");

        let mut manager = SettingsManager::from_paths(global_path.clone(), project_path);
        manager
            .update_global_settings(json!({"terminal":{"clearOnShrink":true}}))
            .expect("update global");

        let persisted = read_json_file(&global_path);
        assert_eq!(persisted["theme"], Value::String("light".to_string()));
        assert_eq!(persisted["terminal"]["showImages"], Value::Bool(true));
        assert_eq!(persisted["terminal"]["clearOnShrink"], Value::Bool(true));
        assert_eq!(persisted["other"]["x"], json!(1));
    }

    #[test]
    fn reload_collects_parse_errors_and_uses_empty_fallback() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        fs::write(&global_path, "{not-json").expect("write broken global");
        fs::write(&project_path, r#"{"theme":"project"}"#).expect("write project");

        let mut manager = SettingsManager::from_paths(global_path, project_path);
        let errors = manager.drain_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].scope, SettingsScope::Global);
        assert_eq!(
            manager.merged_settings()["theme"],
            Value::String("project".to_string())
        );
    }

    #[test]
    fn gets_and_sets_string_lists_by_scope() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        fs::write(
            &global_path,
            r#"{"skills":["global-skill"],"packages":["npm:chalk"]}"#,
        )
        .expect("write global");
        fs::write(&project_path, r#"{"skills":["project-skill"]}"#).expect("write project");

        let mut manager = SettingsManager::from_paths(global_path.clone(), project_path.clone());
        assert_eq!(
            manager.get_string_list("skills", Some(SettingsScope::Global)),
            vec!["global-skill".to_string()]
        );
        assert_eq!(
            manager.get_string_list("skills", Some(SettingsScope::Project)),
            vec!["project-skill".to_string()]
        );
        assert_eq!(
            manager.get_string_list("skills", None),
            vec!["project-skill".to_string()]
        );

        manager
            .set_string_list(
                SettingsScope::Project,
                "packages",
                &["git:github.com/acme/tools".to_string()],
            )
            .expect("set project packages");

        let persisted = read_json_file(&project_path);
        assert_eq!(persisted["packages"], json!(["git:github.com/acme/tools"]));
    }

    #[test]
    fn preserves_optional_enabled_models_presence() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        let mut manager = SettingsManager::from_paths(global_path.clone(), project_path);

        assert_eq!(manager.get_enabled_models(None), None);

        let patterns = [
            "openai/gpt-5.1-codex".to_string(),
            "anthropic/claude-opus-4-6:high".to_string(),
        ];
        manager
            .set_enabled_models(SettingsScope::Global, Some(&patterns))
            .expect("set enabled models");
        assert_eq!(manager.get_enabled_models(None), Some(patterns.to_vec()));

        manager
            .set_enabled_models(SettingsScope::Global, Some(&[]))
            .expect("set empty enabled models");
        assert_eq!(manager.get_enabled_models(None), Some(Vec::new()));
        let persisted = read_json_file(&global_path);
        assert_eq!(persisted["enabledModels"], json!([]));

        manager
            .set_enabled_models(SettingsScope::Global, None)
            .expect("clear enabled models");
        assert_eq!(manager.get_enabled_models(None), None);
        let persisted = read_json_file(&global_path);
        assert!(persisted.get("enabledModels").is_none());
    }

    #[test]
    fn persists_default_model_and_provider() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        let mut manager = SettingsManager::from_paths(global_path.clone(), project_path);

        manager
            .set_default_model_and_provider("openai", "gpt-5.1-codex")
            .expect("set default model");

        assert_eq!(manager.get_default_provider().as_deref(), Some("openai"));
        assert_eq!(
            manager.get_default_model().as_deref(),
            Some("gpt-5.1-codex")
        );

        let persisted = read_json_file(&global_path);
        assert_eq!(persisted["defaultProvider"], json!("openai"));
        assert_eq!(persisted["defaultModel"], json!("gpt-5.1-codex"));
    }
}
