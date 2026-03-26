use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Map;
pub use serde_json::Value;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueModeSetting {
    OneAtATime,
    All,
}

impl QueueModeSetting {
    fn as_str(self) -> &'static str {
        match self {
            QueueModeSetting::OneAtATime => "one-at-a-time",
            QueueModeSetting::All => "all",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportSetting {
    Sse,
    Websocket,
    Auto,
}

impl TransportSetting {
    fn as_str(self) -> &'static str {
        match self {
            TransportSetting::Sse => "sse",
            TransportSetting::Websocket => "websocket",
            TransportSetting::Auto => "auto",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleEscapeActionSetting {
    Tree,
    Fork,
    None,
}

impl DoubleEscapeActionSetting {
    fn as_str(self) -> &'static str {
        match self {
            DoubleEscapeActionSetting::Tree => "tree",
            DoubleEscapeActionSetting::Fork => "fork",
            DoubleEscapeActionSetting::None => "none",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GlobalSettingChange {
    AutoCompact(bool),
    SteeringMode(QueueModeSetting),
    FollowUpMode(QueueModeSetting),
    Transport(TransportSetting),
    DefaultThinkingLevel(String),
    Theme(String),
    HideThinkingBlock(bool),
    CollapseChangelog(bool),
    QuietStartup(bool),
    TerminalShowImages(bool),
    ImagesAutoResize(bool),
    ImagesBlockImages(bool),
    EnableSkillCommands(bool),
    ShowHardwareCursor(bool),
    EditorPaddingX(i64),
    AutocompleteMaxVisible(i64),
    TerminalClearOnShrink(bool),
    DoubleEscapeAction(DoubleEscapeActionSetting),
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
    #[error("failed to acquire settings lock {path}: {message}")]
    Lock { path: PathBuf, message: String },
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

    pub fn apply_setting_change(
        &mut self,
        scope: SettingsScope,
        change: GlobalSettingChange,
    ) -> Result<(), SettingsManagerError> {
        self.transact_scoped_settings(scope, |current| {
            let object = current
                .as_object_mut()
                .expect("load_settings guarantees root object");
            match change {
                GlobalSettingChange::AutoCompact(enabled) => {
                    set_path_value(object, &["compaction", "enabled"], Value::Bool(enabled));
                }
                GlobalSettingChange::SteeringMode(mode) => {
                    set_path_value(
                        object,
                        &["steeringMode"],
                        Value::String(mode.as_str().to_string()),
                    );
                }
                GlobalSettingChange::FollowUpMode(mode) => {
                    set_path_value(
                        object,
                        &["followUpMode"],
                        Value::String(mode.as_str().to_string()),
                    );
                }
                GlobalSettingChange::Transport(transport) => {
                    set_path_value(
                        object,
                        &["transport"],
                        Value::String(transport.as_str().to_string()),
                    );
                }
                GlobalSettingChange::DefaultThinkingLevel(level) => {
                    set_path_value(object, &["defaultThinkingLevel"], Value::String(level));
                }
                GlobalSettingChange::Theme(theme) => {
                    set_path_value(object, &["theme"], Value::String(theme));
                }
                GlobalSettingChange::HideThinkingBlock(enabled) => {
                    set_path_value(object, &["hideThinkingBlock"], Value::Bool(enabled));
                }
                GlobalSettingChange::CollapseChangelog(enabled) => {
                    set_path_value(object, &["collapseChangelog"], Value::Bool(enabled));
                }
                GlobalSettingChange::QuietStartup(enabled) => {
                    set_path_value(object, &["quietStartup"], Value::Bool(enabled));
                }
                GlobalSettingChange::TerminalShowImages(enabled) => {
                    set_path_value(object, &["terminal", "showImages"], Value::Bool(enabled));
                }
                GlobalSettingChange::ImagesAutoResize(enabled) => {
                    set_path_value(object, &["images", "autoResize"], Value::Bool(enabled));
                }
                GlobalSettingChange::ImagesBlockImages(enabled) => {
                    set_path_value(object, &["images", "blockImages"], Value::Bool(enabled));
                }
                GlobalSettingChange::EnableSkillCommands(enabled) => {
                    set_path_value(object, &["enableSkillCommands"], Value::Bool(enabled));
                }
                GlobalSettingChange::ShowHardwareCursor(enabled) => {
                    set_path_value(object, &["showHardwareCursor"], Value::Bool(enabled));
                }
                GlobalSettingChange::EditorPaddingX(padding) => {
                    set_path_value(object, &["editorPaddingX"], Value::Number(padding.into()));
                }
                GlobalSettingChange::AutocompleteMaxVisible(max_visible) => {
                    set_path_value(
                        object,
                        &["autocompleteMaxVisible"],
                        Value::Number(max_visible.into()),
                    );
                }
                GlobalSettingChange::TerminalClearOnShrink(enabled) => {
                    set_path_value(object, &["terminal", "clearOnShrink"], Value::Bool(enabled));
                }
                GlobalSettingChange::DoubleEscapeAction(action) => {
                    set_path_value(
                        object,
                        &["doubleEscapeAction"],
                        Value::String(action.as_str().to_string()),
                    );
                }
            }
            Ok(())
        })
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

    pub fn get_plugin_roots(&self, scope: Option<SettingsScope>) -> Vec<String> {
        self.get_string_list("pluginRoots", scope)
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
        self.mutate_scoped_settings(scope, |object| {
            object.insert(
                key.to_string(),
                Value::Array(values.iter().cloned().map(Value::String).collect()),
            );
        })
    }

    pub fn set_plugin_roots(
        &mut self,
        scope: SettingsScope,
        roots: &[String],
    ) -> Result<(), SettingsManagerError> {
        self.set_string_list(scope, "pluginRoots", roots)
    }

    pub fn set_optional_string_list(
        &mut self,
        scope: SettingsScope,
        key: &str,
        values: Option<&[String]>,
    ) -> Result<(), SettingsManagerError> {
        self.mutate_scoped_settings(scope, |object| match values {
            Some(values) => {
                object.insert(
                    key.to_string(),
                    Value::Array(values.iter().cloned().map(Value::String).collect()),
                );
            }
            None => {
                object.remove(key);
            }
        })
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
        self.mutate_scoped_settings(SettingsScope::Global, |object| {
            object.insert(
                "defaultProvider".to_string(),
                Value::String(provider.to_string()),
            );
            object.insert(
                "defaultModel".to_string(),
                Value::String(model_id.to_string()),
            );
        })
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

    pub fn transact_scoped_settings<R>(
        &mut self,
        scope: SettingsScope,
        mutate: impl FnOnce(&mut Value) -> Result<R, SettingsManagerError>,
    ) -> Result<R, SettingsManagerError> {
        let path = self.settings_path(scope).to_path_buf();
        let _lock = SettingsFileLock::acquire(&path)?;
        let mut current = load_settings(&path)?;
        let result = mutate(&mut current)?;
        ensure_object(&current, &path)?;
        persist_settings_unlocked(&path, &current)?;
        *self.scoped_settings_mut(scope) = current;
        Ok(result)
    }

    fn update_scoped_settings(
        &mut self,
        scope: SettingsScope,
        patch: Value,
    ) -> Result<(), SettingsManagerError> {
        self.transact_scoped_settings(scope, |current| {
            ensure_object(&patch, Path::new("<settings-patch>"))?;
            *current = deep_merge_settings(current, &patch);
            Ok(())
        })
    }

    fn replace_scoped_settings(
        &mut self,
        scope: SettingsScope,
        settings: Value,
    ) -> Result<(), SettingsManagerError> {
        self.transact_scoped_settings(scope, |current| {
            ensure_object(&settings, Path::new("<settings>"))?;
            *current = settings;
            Ok(())
        })
    }

    fn mutate_scoped_settings(
        &mut self,
        scope: SettingsScope,
        mutate: impl FnOnce(&mut Map<String, Value>),
    ) -> Result<(), SettingsManagerError> {
        self.transact_scoped_settings(scope, |current| {
            let object = current
                .as_object_mut()
                .expect("load_settings guarantees root object");
            mutate(object);
            Ok(())
        })
    }

    fn settings_path(&self, scope: SettingsScope) -> &Path {
        match scope {
            SettingsScope::Global => &self.global_settings_path,
            SettingsScope::Project => &self.project_settings_path,
        }
    }

    fn scoped_settings_mut(&mut self, scope: SettingsScope) -> &mut Value {
        match scope {
            SettingsScope::Global => &mut self.layers.global,
            SettingsScope::Project => &mut self.layers.project,
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

fn persist_settings_unlocked(path: &Path, value: &Value) -> Result<(), SettingsManagerError> {
    ensure_object(value, path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SettingsManagerError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let payload = serde_json::to_string_pretty(value)?;
    let temp_path = settings_temp_path(path);
    fs::write(&temp_path, payload).map_err(|source| SettingsManagerError::Io {
        path: temp_path.clone(),
        source,
    })?;
    fs::rename(&temp_path, path).map_err(|source| SettingsManagerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn settings_lock_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.lock", path.display()))
}

fn settings_temp_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.display()))
}

struct SettingsFileLock {
    path: PathBuf,
}

impl SettingsFileLock {
    fn acquire(path: &Path) -> Result<Self, SettingsManagerError> {
        const LOCK_WAIT: Duration = Duration::from_millis(500);
        const RETRY_DELAY: Duration = Duration::from_millis(10);

        let lock_path = settings_lock_path(path);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|source| SettingsManagerError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(Self { path: lock_path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(SettingsManagerError::Lock {
                            path: lock_path,
                            message: "timed out waiting for settings lock".to_string(),
                        });
                    }
                    thread::sleep(RETRY_DELAY);
                }
                Err(error) => {
                    return Err(SettingsManagerError::Lock {
                        path: lock_path,
                        message: error.to_string(),
                    });
                }
            }
        }
    }
}

impl Drop for SettingsFileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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

fn set_path_value(object: &mut Map<String, Value>, path: &[&str], value: Value) {
    let Some((head, tail)) = path.split_first() else {
        return;
    };
    if tail.is_empty() {
        object.insert((*head).to_string(), value);
        return;
    }

    let entry = object
        .entry((*head).to_string())
        .or_insert_with(empty_settings);
    if !entry.is_object() {
        *entry = empty_settings();
    }
    let nested = entry
        .as_object_mut()
        .expect("set_path_value initializes nested object");
    set_path_value(nested, tail, value);
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
    fn apply_setting_change_persists_all_interactive_overlay_settings() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        fs::write(
            &global_path,
            r#"{
                "existing": 1,
                "compaction": {"legacy": true},
                "terminal": {"legacy": true},
                "images": {"legacy": true}
            }"#,
        )
        .expect("write global");

        let mut manager = SettingsManager::from_paths(global_path.clone(), project_path);
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::AutoCompact(true),
            )
            .expect("auto compact");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::SteeringMode(QueueModeSetting::All),
            )
            .expect("steering mode");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::FollowUpMode(QueueModeSetting::OneAtATime),
            )
            .expect("follow-up mode");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::Transport(TransportSetting::Websocket),
            )
            .expect("transport");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::DefaultThinkingLevel("high".to_string()),
            )
            .expect("thinking level");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::Theme("midnight".to_string()),
            )
            .expect("theme");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::HideThinkingBlock(true),
            )
            .expect("hide thinking");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::CollapseChangelog(true),
            )
            .expect("collapse changelog");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::QuietStartup(true),
            )
            .expect("quiet startup");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::TerminalShowImages(false),
            )
            .expect("show images");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::ImagesAutoResize(false),
            )
            .expect("auto resize images");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::ImagesBlockImages(true),
            )
            .expect("block images");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::EnableSkillCommands(false),
            )
            .expect("skill commands");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::ShowHardwareCursor(true),
            )
            .expect("hardware cursor");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::EditorPaddingX(3),
            )
            .expect("editor padding");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::AutocompleteMaxVisible(15),
            )
            .expect("autocomplete visible");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::TerminalClearOnShrink(true),
            )
            .expect("clear on shrink");
        manager
            .apply_setting_change(
                SettingsScope::Global,
                GlobalSettingChange::DoubleEscapeAction(DoubleEscapeActionSetting::Fork),
            )
            .expect("double escape");

        let persisted = read_json_file(&global_path);
        assert_eq!(persisted["existing"], json!(1));
        assert_eq!(persisted["compaction"]["legacy"], json!(true));
        assert_eq!(persisted["compaction"]["enabled"], json!(true));
        assert_eq!(persisted["steeringMode"], json!("all"));
        assert_eq!(persisted["followUpMode"], json!("one-at-a-time"));
        assert_eq!(persisted["transport"], json!("websocket"));
        assert_eq!(persisted["defaultThinkingLevel"], json!("high"));
        assert_eq!(persisted["theme"], json!("midnight"));
        assert_eq!(persisted["hideThinkingBlock"], json!(true));
        assert_eq!(persisted["collapseChangelog"], json!(true));
        assert_eq!(persisted["quietStartup"], json!(true));
        assert_eq!(persisted["terminal"]["legacy"], json!(true));
        assert_eq!(persisted["terminal"]["showImages"], json!(false));
        assert_eq!(persisted["terminal"]["clearOnShrink"], json!(true));
        assert_eq!(persisted["images"]["legacy"], json!(true));
        assert_eq!(persisted["images"]["autoResize"], json!(false));
        assert_eq!(persisted["images"]["blockImages"], json!(true));
        assert_eq!(persisted["enableSkillCommands"], json!(false));
        assert_eq!(persisted["showHardwareCursor"], json!(true));
        assert_eq!(persisted["editorPaddingX"], json!(3));
        assert_eq!(persisted["autocompleteMaxVisible"], json!(15));
        assert_eq!(persisted["doubleEscapeAction"], json!("fork"));
    }

    #[test]
    fn apply_setting_change_can_target_project_scope() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        fs::write(&project_path, r#"{"theme":"project-light"}"#).expect("write project");

        let mut manager = SettingsManager::from_paths(global_path, project_path.clone());
        manager
            .apply_setting_change(
                SettingsScope::Project,
                GlobalSettingChange::Theme("project-dark".to_string()),
            )
            .expect("update project theme");

        let persisted = read_json_file(&project_path);
        assert_eq!(persisted["theme"], json!("project-dark"));
    }

    #[test]
    fn transaction_primitive_persists_and_returns_values() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        let mut manager = SettingsManager::from_paths(global_path.clone(), project_path);

        let result = manager
            .transact_scoped_settings(SettingsScope::Global, |current| {
                let object = current
                    .as_object_mut()
                    .expect("load_settings guarantees root object");
                object.insert("theme".to_string(), json!("dark"));
                Ok("updated")
            })
            .expect("transaction");

        assert_eq!(result, "updated");
        assert_eq!(read_json_file(&global_path)["theme"], json!("dark"));
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
    fn gets_and_sets_plugin_roots_by_scope() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        fs::write(
            &global_path,
            r#"{"pluginRoots":["global-plugins"],"packages":["npm:chalk"]}"#,
        )
        .expect("write global");
        fs::write(&project_path, r#"{"pluginRoots":["project-plugins"]}"#)
            .expect("write project");

        let mut manager = SettingsManager::from_paths(global_path.clone(), project_path.clone());
        assert_eq!(
            manager.get_plugin_roots(Some(SettingsScope::Global)),
            vec!["global-plugins".to_string()]
        );
        assert_eq!(
            manager.get_plugin_roots(Some(SettingsScope::Project)),
            vec!["project-plugins".to_string()]
        );
        assert_eq!(
            manager.get_plugin_roots(None),
            vec!["project-plugins".to_string()]
        );

        manager
            .set_plugin_roots(
                SettingsScope::Project,
                &["plugins/custom".to_string(), "../plugins/extra".to_string()],
            )
            .expect("set project plugin roots");

        let persisted = read_json_file(&project_path);
        assert_eq!(
            persisted["pluginRoots"],
            json!(["plugins/custom", "../plugins/extra"])
        );
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

    #[test]
    fn concurrent_patch_updates_merge_against_latest_file_state() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        fs::write(
            &global_path,
            r#"{"theme":"light","terminal":{"showImages":true}}"#,
        )
        .expect("write global");

        let mut first = SettingsManager::from_paths(global_path.clone(), project_path.clone());
        let mut second = SettingsManager::from_paths(global_path.clone(), project_path);

        first
            .update_global_settings(json!({"terminal":{"clearOnShrink":true}}))
            .expect("first patch");
        second
            .update_global_settings(json!({"theme":"dark"}))
            .expect("second patch");

        let persisted = read_json_file(&global_path);
        assert_eq!(persisted["theme"], json!("dark"));
        assert_eq!(persisted["terminal"]["showImages"], json!(true));
        assert_eq!(persisted["terminal"]["clearOnShrink"], json!(true));
    }

    #[test]
    fn set_default_model_merges_against_latest_file_state() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        fs::write(
            &global_path,
            r#"{"theme":"light","terminal":{"showImages":true}}"#,
        )
        .expect("write global");

        let mut first = SettingsManager::from_paths(global_path.clone(), project_path.clone());
        let mut second = SettingsManager::from_paths(global_path.clone(), project_path);

        first
            .update_global_settings(json!({"terminal":{"clearOnShrink":true}}))
            .expect("first patch");
        second
            .set_default_model_and_provider("openai", "gpt-5.1-codex")
            .expect("set model");

        let persisted = read_json_file(&global_path);
        assert_eq!(persisted["theme"], json!("light"));
        assert_eq!(persisted["terminal"]["showImages"], json!(true));
        assert_eq!(persisted["terminal"]["clearOnShrink"], json!(true));
        assert_eq!(persisted["defaultProvider"], json!("openai"));
        assert_eq!(persisted["defaultModel"], json!("gpt-5.1-codex"));
    }

    #[test]
    fn update_scoped_settings_times_out_when_lock_is_held() {
        let tempdir = tempdir().expect("tempdir");
        let global_path = tempdir.path().join("global.json");
        let project_path = tempdir.path().join("project.json");
        let lock_path = settings_lock_path(&global_path);
        fs::write(&global_path, r#"{"theme":"light"}"#).expect("write global");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .expect("create lock");

        let mut manager = SettingsManager::from_paths(global_path, project_path);
        let error = manager
            .update_global_settings(json!({"theme":"dark"}))
            .expect_err("lock should block update");
        match error {
            SettingsManagerError::Lock { path, .. } => assert_eq!(path, lock_path),
            other => panic!("unexpected error: {other}"),
        }

        fs::remove_file(lock_path).expect("cleanup lock");
    }
}
