use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use pi_rust_config::{
    SettingsManager, SettingsManagerError, SettingsScope, get_agent_dir, get_project_config_dir,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const SUPPORTED_PACKAGE_SOURCES: &[&str] = &["npm", "git", "local"];
pub const CURRENT_PACKAGE_STATE_VERSION: u32 = 1;
pub const ENV_PACKAGE_DIR: &str = "PI_PACKAGE_DIR";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PackageSource {
    Npm {
        spec: String,
        name: String,
        version: Option<String>,
        pinned: bool,
    },
    Git {
        repo: String,
        host: String,
        path: String,
        #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
        pinned: bool,
    },
    Local {
        path: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageInstallScope {
    User,
    Project,
    Temporary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInstallRecord {
    pub source: String,
    pub identity: String,
    pub scope: PackageInstallScope,
    pub install_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledPackage {
    pub source: String,
    pub identity: String,
    pub scope: PackageInstallScope,
    pub install_path: PathBuf,
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PackageConfigEntry {
    Source(String),
    Object {
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extensions: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skills: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompts: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        themes: Option<Vec<String>>,
    },
}

impl PackageConfigEntry {
    fn source(&self) -> &str {
        match self {
            Self::Source(source) => source,
            Self::Object { source, .. } => source,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageState {
    #[serde(default = "default_package_state_version")]
    pub version: u32,
    #[serde(default)]
    pub packages: Vec<PackageInstallRecord>,
}

impl Default for PackageState {
    fn default() -> Self {
        Self {
            version: CURRENT_PACKAGE_STATE_VERSION,
            packages: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum PackageStateError {
    #[error("failed to read package state file {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("failed to parse package state file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to serialize package state: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum PackageManagerError {
    #[error(transparent)]
    State(#[from] PackageStateError),
    #[error(transparent)]
    Settings(#[from] SettingsManagerError),
    #[error("local package path does not exist: {0}")]
    MissingLocalPath(PathBuf),
    #[error("failed to run {command}: {message}")]
    CommandFailed { command: String, message: String },
    #[error("{0}")]
    Message(String),
}

#[derive(Clone, Debug)]
pub struct PackageStateStore {
    path: PathBuf,
}

impl PackageStateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<PackageState, PackageStateError> {
        if !self.path.exists() {
            return Ok(PackageState::default());
        }

        let content = fs::read_to_string(&self.path).map_err(|source| PackageStateError::Io {
            path: self.path.clone(),
            source,
        })?;
        if content.trim().is_empty() {
            return Ok(PackageState::default());
        }

        let mut state: PackageState =
            serde_json::from_str(&content).map_err(|source| PackageStateError::Parse {
                path: self.path.clone(),
                source,
            })?;
        if state.version == 0 {
            state.version = CURRENT_PACKAGE_STATE_VERSION;
        }
        Ok(state)
    }

    pub fn save(&self, state: &PackageState) -> Result<(), PackageStateError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| PackageStateError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let payload = serde_json::to_string_pretty(state)?;
        fs::write(&self.path, payload).map_err(|source| PackageStateError::Io {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub fn upsert(&self, record: PackageInstallRecord) -> Result<(), PackageStateError> {
        let mut state = self.load()?;
        if let Some(index) = state
            .packages
            .iter()
            .position(|item| item.identity == record.identity && item.scope == record.scope)
        {
            state.packages[index] = record;
        } else {
            state.packages.push(record);
        }

        state.packages.sort_by(|left, right| {
            left.scope
                .cmp(&right.scope)
                .then_with(|| left.identity.cmp(&right.identity))
                .then_with(|| left.source.cmp(&right.source))
        });
        state.version = CURRENT_PACKAGE_STATE_VERSION;
        self.save(&state)
    }

    pub fn remove(
        &self,
        identity: &str,
        scope: Option<PackageInstallScope>,
    ) -> Result<bool, PackageStateError> {
        let mut state = self.load()?;
        let before = state.packages.len();
        state.packages.retain(|record| {
            if record.identity != identity {
                return true;
            }
            if let Some(scope_filter) = scope {
                return record.scope != scope_filter;
            }
            false
        });

        let changed = before != state.packages.len();
        if changed {
            state.version = CURRENT_PACKAGE_STATE_VERSION;
            self.save(&state)?;
        }
        Ok(changed)
    }
}

#[derive(Clone, Debug)]
pub struct PackageManager {
    cwd: PathBuf,
    agent_dir: PathBuf,
    settings_manager: SettingsManager,
    state_store: PackageStateStore,
}

impl PackageManager {
    pub fn create(cwd: impl AsRef<Path>, agent_dir: Option<PathBuf>) -> Self {
        let cwd = cwd.as_ref().to_path_buf();
        let agent_dir = agent_dir.unwrap_or_else(get_agent_dir);
        let settings_manager = SettingsManager::create(&cwd, Some(agent_dir.clone()));
        let state_store = PackageStateStore::new(agent_dir.join("packages").join("state.json"));
        Self {
            cwd,
            agent_dir,
            settings_manager,
            state_store,
        }
    }

    pub fn with_settings_manager(
        cwd: impl AsRef<Path>,
        agent_dir: Option<PathBuf>,
        settings_manager: SettingsManager,
    ) -> Self {
        let cwd = cwd.as_ref().to_path_buf();
        let agent_dir = agent_dir.unwrap_or_else(get_agent_dir);
        let state_store = PackageStateStore::new(agent_dir.join("packages").join("state.json"));
        Self {
            cwd,
            agent_dir,
            settings_manager,
            state_store,
        }
    }

    pub fn settings_manager(&self) -> &SettingsManager {
        &self.settings_manager
    }

    pub fn settings_manager_mut(&mut self) -> &mut SettingsManager {
        &mut self.settings_manager
    }

    pub fn agent_dir(&self) -> &Path {
        &self.agent_dir
    }

    pub fn list_by_scope(&self, scope: PackageInstallScope) -> Vec<InstalledPackage> {
        let state = self.state_store.load().unwrap_or_default();
        let mut seen = HashSet::new();
        let mut packages = Vec::new();

        for entry in self.package_entries_for_scope(settings_scope_for_install_scope(scope)) {
            let source = entry.source().to_string();
            let resolve_base = self.identity_base_dir_for_settings_source(&source, scope);
            let identity = package_identity_from_input(&source, Some(&resolve_base));
            if !seen.insert(identity.clone()) {
                continue;
            }

            let installed = state
                .packages
                .iter()
                .find(|record| record.scope == scope && record.identity == identity)
                .map(|record| InstalledPackage {
                    source: source.clone(),
                    identity: record.identity.clone(),
                    scope: record.scope,
                    install_path: PathBuf::from(&record.install_path),
                    pinned: record.pinned,
                })
                .unwrap_or_else(|| {
                    self.installed_package_for_source(&source, scope, Some(&resolve_base))
                });
            packages.push(installed);
        }

        packages
    }

    pub fn list_all(&self) -> Vec<InstalledPackage> {
        let mut packages = Vec::new();
        let mut index_by_identity = HashMap::new();

        for package in self.list_by_scope(PackageInstallScope::User) {
            index_by_identity.insert(package.identity.clone(), packages.len());
            packages.push(package);
        }

        for package in self.list_by_scope(PackageInstallScope::Project) {
            if let Some(index) = index_by_identity.get(&package.identity).copied() {
                packages[index] = package;
            } else {
                index_by_identity.insert(package.identity.clone(), packages.len());
                packages.push(package);
            }
        }

        packages
    }

    pub fn install(
        &mut self,
        source: &str,
        scope: PackageInstallScope,
    ) -> Result<InstalledPackage, PackageManagerError> {
        let parsed = parse_package_source_with_base(source, Some(self.base_dir_for_scope(scope)));
        let install_path = self.install_path_for_parsed_source(&parsed, scope);

        if let PackageSource::Local { path } = &parsed {
            let resolved = resolve_local_path(path, Some(self.base_dir_for_scope(scope)));
            if !resolved.exists() {
                return Err(PackageManagerError::MissingLocalPath(resolved));
            }
        } else if let PackageSource::Npm { spec, .. } = &parsed {
            let install_root = self.npm_project_root_for_source(&parsed, scope);
            self.ensure_npm_project(&install_root)?;
            self.run_command(
                "npm",
                &[
                    "install".to_string(),
                    spec.clone(),
                    "--prefix".to_string(),
                    install_root.to_string_lossy().to_string(),
                ],
                None,
            )?;
        } else if let PackageSource::Git { repo, git_ref, .. } = &parsed {
            self.install_git(repo, git_ref.as_deref(), &install_path)?;
        }

        let record = build_install_record(
            source,
            scope,
            install_path.to_string_lossy().to_string(),
            Some(self.base_dir_for_scope(scope)),
        );
        self.state_store.upsert(record.clone())?;
        self.add_source_to_settings(source, scope)?;
        Ok(installed_package_from_record(&record))
    }

    pub fn remove(
        &mut self,
        source: &str,
        scope: PackageInstallScope,
    ) -> Result<bool, PackageManagerError> {
        let parsed = parse_package_source_with_base(source, Some(self.base_dir_for_scope(scope)));
        let identity = package_identity(&parsed, Some(self.base_dir_for_scope(scope)));
        let removed = self.remove_source_from_settings(source, scope)?;
        let _ = self.state_store.remove(&identity, Some(scope))?;

        if removed {
            let install_path = self.install_path_for_parsed_source(&parsed, scope);
            if matches!(parsed, PackageSource::Npm { .. }) {
                let install_root = self.npm_project_root_for_source(&parsed, scope);
                if install_root.exists() {
                    let package_name = match &parsed {
                        PackageSource::Npm { name, .. } => name.clone(),
                        _ => String::new(),
                    };
                    if !package_name.is_empty() {
                        let _ = self.run_command(
                            "npm",
                            &[
                                "uninstall".to_string(),
                                package_name,
                                "--prefix".to_string(),
                                install_root.to_string_lossy().to_string(),
                            ],
                            None,
                        );
                    }
                    if scope != PackageInstallScope::Project
                        || env::var_os(ENV_PACKAGE_DIR).is_some()
                    {
                        let _ = fs::remove_dir_all(&install_root);
                    }
                }
            } else if install_path.exists() && !matches!(parsed, PackageSource::Local { .. }) {
                let _ = fs::remove_dir_all(&install_path);
            }
        }

        Ok(removed)
    }

    pub fn update(
        &mut self,
        source: Option<&str>,
    ) -> Result<Vec<InstalledPackage>, PackageManagerError> {
        let mut updated = Vec::new();
        let packages = self.list_all();
        for package in packages {
            if source.is_some_and(|value| value != package.source) {
                continue;
            }
            let parsed = parse_package_source_with_base(
                &package.source,
                Some(self.base_dir_for_scope(package.scope)),
            );
            match &parsed {
                PackageSource::Local { path } => {
                    let resolved =
                        resolve_local_path(path, Some(self.base_dir_for_scope(package.scope)));
                    if !resolved.exists() {
                        return Err(PackageManagerError::MissingLocalPath(resolved));
                    }
                }
                PackageSource::Npm { spec, .. } => {
                    let install_root = self.npm_project_root_for_source(&parsed, package.scope);
                    self.ensure_npm_project(&install_root)?;
                    self.run_command(
                        "npm",
                        &[
                            "install".to_string(),
                            spec.clone(),
                            "--prefix".to_string(),
                            install_root.to_string_lossy().to_string(),
                        ],
                        None,
                    )?;
                }
                PackageSource::Git { git_ref, .. } => {
                    self.update_git(&parsed, git_ref.as_deref(), package.scope)?;
                }
            }
            let record = build_install_record(
                &package.source,
                package.scope,
                self.install_path_for_parsed_source(&parsed, package.scope)
                    .to_string_lossy()
                    .to_string(),
                Some(self.base_dir_for_scope(package.scope)),
            );
            self.state_store.upsert(record.clone())?;
            updated.push(installed_package_from_record(&record));
        }
        Ok(updated)
    }

    pub fn installed_path(&self, source: &str, scope: PackageInstallScope) -> PathBuf {
        self.install_path_for_source(source, scope, Some(self.base_dir_for_scope(scope)))
    }

    pub fn resource_roots(&self) -> Vec<(PackageInstallScope, PathBuf)> {
        self.list_all()
            .into_iter()
            .map(|package| (package.scope, package.install_path))
            .collect()
    }

    fn add_source_to_settings(
        &mut self,
        source: &str,
        scope: PackageInstallScope,
    ) -> Result<(), PackageManagerError> {
        let settings_scope = settings_scope_for_install_scope(scope);
        let mut packages = self.package_entry_values_for_scope(settings_scope);
        let identity = package_identity_from_input(source, Some(self.base_dir_for_scope(scope)));
        if packages.iter().any(|existing| {
            let Some(existing_source) = package_source_from_entry(existing) else {
                return false;
            };
            let resolve_base = self.identity_base_dir_for_settings_source(&existing_source, scope);
            package_identity_from_input(&existing_source, Some(&resolve_base)) == identity
        }) {
            return Ok(());
        }
        packages.push(Value::String(
            self.normalize_source_for_settings(source, scope),
        ));
        self.set_package_entry_values(settings_scope, packages)?;
        Ok(())
    }

    fn remove_source_from_settings(
        &mut self,
        source: &str,
        scope: PackageInstallScope,
    ) -> Result<bool, PackageManagerError> {
        let settings_scope = settings_scope_for_install_scope(scope);
        let current = self.package_entry_values_for_scope(settings_scope);
        let target_identity =
            package_identity_from_input(source, Some(self.base_dir_for_scope(scope)));
        let next = current
            .iter()
            .filter(|existing| {
                let Some(existing_source) = package_source_from_entry(existing) else {
                    return true;
                };
                let resolve_base =
                    self.identity_base_dir_for_settings_source(&existing_source, scope);
                package_identity_from_input(&existing_source, Some(&resolve_base))
                    != target_identity
            })
            .cloned()
            .collect::<Vec<_>>();
        if next.len() == current.len() {
            return Ok(false);
        }
        self.set_package_entry_values(settings_scope, next)?;
        Ok(true)
    }

    fn installed_package_for_source(
        &self,
        source: &str,
        scope: PackageInstallScope,
        base_dir: Option<&Path>,
    ) -> InstalledPackage {
        let record = build_install_record(
            source,
            scope,
            self.install_path_for_source(source, scope, base_dir)
                .to_string_lossy()
                .to_string(),
            base_dir,
        );
        installed_package_from_record(&record)
    }

    fn install_path_for_source(
        &self,
        source: &str,
        scope: PackageInstallScope,
        base_dir: Option<&Path>,
    ) -> PathBuf {
        let parsed = parse_package_source_with_base(
            source,
            base_dir.or_else(|| Some(self.base_dir_for_scope(scope))),
        );
        self.install_path_for_parsed_source(&parsed, scope)
    }

    fn package_root_for_scope(&self, scope: PackageInstallScope) -> PathBuf {
        if let Some(path) = env::var_os(ENV_PACKAGE_DIR) {
            return PathBuf::from(path).join(match scope {
                PackageInstallScope::User => "user",
                PackageInstallScope::Project => "project",
                PackageInstallScope::Temporary => "temporary",
            });
        }

        match scope {
            PackageInstallScope::User => self.agent_dir.join("packages").join("user"),
            PackageInstallScope::Project => get_project_config_dir(&self.cwd),
            PackageInstallScope::Temporary => self.agent_dir.join("packages").join("tmp"),
        }
    }

    fn base_dir_for_scope(&self, scope: PackageInstallScope) -> &Path {
        match scope {
            PackageInstallScope::User | PackageInstallScope::Temporary => &self.agent_dir,
            PackageInstallScope::Project => &self.cwd,
        }
    }

    fn settings_base_dir_for_scope(&self, scope: PackageInstallScope) -> PathBuf {
        match scope {
            PackageInstallScope::User | PackageInstallScope::Temporary => self.agent_dir.clone(),
            PackageInstallScope::Project => get_project_config_dir(&self.cwd),
        }
    }

    fn package_entries_for_scope(&self, scope: SettingsScope) -> Vec<PackageConfigEntry> {
        self.settings_manager
            .scoped_settings(scope)
            .get("packages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| serde_json::from_value::<PackageConfigEntry>(entry.clone()).ok())
            .collect()
    }

    fn package_entry_values_for_scope(&self, scope: SettingsScope) -> Vec<Value> {
        self.settings_manager
            .scoped_settings(scope)
            .get("packages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn set_package_entry_values(
        &mut self,
        scope: SettingsScope,
        packages: Vec<Value>,
    ) -> Result<(), PackageManagerError> {
        let patch = serde_json::json!({ "packages": packages });
        match scope {
            SettingsScope::Global => self.settings_manager.update_global_settings(patch)?,
            SettingsScope::Project => self.settings_manager.update_project_settings(patch)?,
        }
        Ok(())
    }

    fn identity_base_dir_for_settings_source(
        &self,
        source: &str,
        scope: PackageInstallScope,
    ) -> PathBuf {
        if let Some(spec) = source.strip_prefix("npm:") {
            let spec = spec.trim();
            if spec.starts_with("file:")
                || spec.starts_with('.')
                || spec.starts_with('/')
                || spec == "~"
                || spec.starts_with("~/")
                || is_windows_absolute_path(spec)
            {
                return self.base_dir_for_scope(scope).to_path_buf();
            }
        }

        self.settings_base_dir_for_scope(scope)
    }

    fn normalize_source_for_settings(&self, source: &str, scope: PackageInstallScope) -> String {
        let parsed = parse_package_source_with_base(source, Some(self.base_dir_for_scope(scope)));
        let PackageSource::Local { path } = parsed else {
            return source.to_string();
        };

        let resolved = resolve_local_path(&path, Some(self.base_dir_for_scope(scope)));
        let settings_base = self.settings_base_dir_for_scope(scope);
        relative_path(&settings_base, &resolved)
            .unwrap_or_else(|| PathBuf::from(source))
            .to_string_lossy()
            .to_string()
    }

    fn install_path_for_parsed_source(
        &self,
        parsed: &PackageSource,
        scope: PackageInstallScope,
    ) -> PathBuf {
        match parsed {
            PackageSource::Local { path } => {
                resolve_local_path(path, Some(self.base_dir_for_scope(scope)))
            }
            PackageSource::Npm { name, .. } => self
                .npm_project_root_for_source(parsed, scope)
                .join("node_modules")
                .join(npm_package_relative_path(name)),
            PackageSource::Git { host, path, .. } => {
                if scope == PackageInstallScope::Project && env::var_os(ENV_PACKAGE_DIR).is_none() {
                    self.package_root_for_scope(scope)
                        .join("git")
                        .join(host)
                        .join(path)
                } else {
                    self.package_root_for_scope(scope)
                        .join(identity_to_dir_name(&package_identity(
                            parsed,
                            Some(self.base_dir_for_scope(scope)),
                        )))
                }
            }
        }
    }

    fn npm_project_root_for_source(
        &self,
        parsed: &PackageSource,
        scope: PackageInstallScope,
    ) -> PathBuf {
        if scope == PackageInstallScope::Project && env::var_os(ENV_PACKAGE_DIR).is_none() {
            self.package_root_for_scope(scope).join("npm")
        } else {
            self.package_root_for_scope(scope)
                .join(identity_to_dir_name(&package_identity(
                    parsed,
                    Some(self.base_dir_for_scope(scope)),
                )))
        }
    }

    fn ensure_npm_project(&self, install_root: &Path) -> Result<(), PackageManagerError> {
        fs::create_dir_all(install_root).map_err(|source| PackageStateError::Io {
            path: install_root.to_path_buf(),
            source,
        })?;
        let package_json_path = install_root.join("package.json");
        if !package_json_path.exists() {
            fs::write(
                &package_json_path,
                serde_json::to_string_pretty(&serde_json::json!({
                    "name": "pi-rust-packages",
                    "private": true
                }))
                .map_err(PackageStateError::Serialize)?,
            )
            .map_err(|source| PackageStateError::Io {
                path: package_json_path.clone(),
                source,
            })?;
        }
        let gitignore_path = install_root.join(".gitignore");
        if !gitignore_path.exists() {
            fs::write(&gitignore_path, "*\n!.gitignore\n").map_err(|source| {
                PackageStateError::Io {
                    path: gitignore_path,
                    source,
                }
            })?;
        }
        Ok(())
    }

    fn install_git(
        &self,
        repo: &str,
        git_ref: Option<&str>,
        target_dir: &Path,
    ) -> Result<(), PackageManagerError> {
        if target_dir.exists() {
            return Ok(());
        }
        if let Some(parent) = target_dir.parent() {
            fs::create_dir_all(parent).map_err(|source| PackageStateError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        self.run_command(
            "git",
            &[
                "clone".to_string(),
                repo.to_string(),
                target_dir.to_string_lossy().to_string(),
            ],
            None,
        )?;
        if let Some(git_ref) = git_ref {
            self.run_command(
                "git",
                &["checkout".to_string(), git_ref.to_string()],
                Some(target_dir),
            )?;
        }
        if target_dir.join("package.json").exists() {
            self.run_command("npm", &["install".to_string()], Some(target_dir))?;
        }
        Ok(())
    }

    fn update_git(
        &self,
        parsed: &PackageSource,
        git_ref: Option<&str>,
        scope: PackageInstallScope,
    ) -> Result<(), PackageManagerError> {
        let target_dir = self.install_path_for_parsed_source(parsed, scope);
        if !target_dir.exists() {
            if let PackageSource::Git { repo, .. } = parsed {
                return self.install_git(repo, git_ref, &target_dir);
            }
            return Ok(());
        }

        self.run_command(
            "git",
            &["fetch".to_string(), "--prune".to_string()],
            Some(&target_dir),
        )?;
        if let Some(git_ref) = git_ref {
            self.run_command(
                "git",
                &["checkout".to_string(), git_ref.to_string()],
                Some(&target_dir),
            )?;
            self.run_command(
                "git",
                &["pull".to_string(), "--ff-only".to_string()],
                Some(&target_dir),
            )?;
        } else {
            self.run_command(
                "git",
                &["pull".to_string(), "--ff-only".to_string()],
                Some(&target_dir),
            )?;
        }

        if target_dir.join("package.json").exists() {
            self.run_command("npm", &["install".to_string()], Some(&target_dir))?;
        }
        Ok(())
    }

    fn run_command(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
    ) -> Result<(), PackageManagerError> {
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd.unwrap_or(&self.cwd))
            .output()
            .map_err(|error| PackageManagerError::CommandFailed {
                command: format!("{program} {}", args.join(" ")),
                message: error.to_string(),
            })?;
        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Err(PackageManagerError::CommandFailed {
            command: format!("{program} {}", args.join(" ")),
            message: if !stderr.is_empty() { stderr } else { stdout },
        })
    }
}

pub fn parse_package_source(source: &str) -> PackageSource {
    parse_package_source_with_base(source, None)
}

fn parse_package_source_with_base(source: &str, base_dir: Option<&Path>) -> PackageSource {
    if let Some(spec) = source.strip_prefix("npm:") {
        let spec = spec.trim().to_string();
        let (name, version) = parse_npm_spec(&spec, base_dir);
        return PackageSource::Npm {
            spec,
            name,
            pinned: version.is_some(),
            version,
        };
    }

    let trimmed = source.trim();
    let is_local_path_like = trimmed.starts_with('.')
        || trimmed.starts_with('/')
        || trimmed == "~"
        || trimmed.starts_with("~/")
        || is_windows_absolute_path(trimmed);
    if is_local_path_like {
        return PackageSource::Local {
            path: source.to_string(),
        };
    }

    if let Some(parsed) = parse_git_source(source) {
        return parsed;
    }

    PackageSource::Local {
        path: source.to_string(),
    }
}

pub fn parse_git_source(source: &str) -> Option<PackageSource> {
    let trimmed = source.trim();
    let has_git_prefix = trimmed.starts_with("git:");
    let url = if has_git_prefix {
        trimmed.trim_start_matches("git:").trim()
    } else {
        trimmed
    };
    if !has_git_prefix && !has_explicit_git_protocol(url) {
        return None;
    }

    let (repo_without_ref, git_ref) = split_git_ref(url);
    let (repo, host, path) = parse_git_repo_components(&repo_without_ref)?;
    let normalized_path = normalize_repo_path(&path)?;

    Some(PackageSource::Git {
        repo,
        host,
        path: normalized_path,
        pinned: git_ref.is_some(),
        git_ref,
    })
}

pub fn package_identity(source: &PackageSource, base_dir: Option<&Path>) -> String {
    match source {
        PackageSource::Npm { name, .. } => format!("npm:{name}"),
        PackageSource::Git { host, path, .. } => {
            format!("git:{}/{}", host.to_lowercase(), path.to_lowercase())
        }
        PackageSource::Local { path } => {
            let resolved = resolve_local_path(path, base_dir);
            format!("local:{}", resolved.to_string_lossy())
        }
    }
}

pub fn package_identity_from_input(source: &str, base_dir: Option<&Path>) -> String {
    let parsed = parse_package_source_with_base(source, base_dir);
    package_identity(&parsed, base_dir)
}

pub fn build_install_record(
    input_source: &str,
    scope: PackageInstallScope,
    install_path: impl Into<String>,
    base_dir: Option<&Path>,
) -> PackageInstallRecord {
    let parsed = parse_package_source_with_base(input_source, base_dir);
    let (source_version, source_ref, pinned) = match &parsed {
        PackageSource::Npm {
            version, pinned, ..
        } => (version.clone(), None, *pinned),
        PackageSource::Git {
            git_ref, pinned, ..
        } => (None, git_ref.clone(), *pinned),
        PackageSource::Local { .. } => (None, None, false),
    };

    PackageInstallRecord {
        source: input_source.to_string(),
        identity: package_identity(&parsed, base_dir),
        scope,
        install_path: install_path.into(),
        source_version,
        source_ref,
        pinned,
    }
}

fn default_package_state_version() -> u32 {
    CURRENT_PACKAGE_STATE_VERSION
}

fn parse_npm_spec(spec: &str, base_dir: Option<&Path>) -> (String, Option<String>) {
    static NPM_SPEC_RE: OnceLock<Regex> = OnceLock::new();
    let regex = NPM_SPEC_RE.get_or_init(|| {
        Regex::new(r"^(@?[^@]+(?:/[^@]+)?)(?:@(.+))?$").expect("npm spec regex must compile")
    });

    if let Some(local_name) = resolve_npm_local_package_name(spec, base_dir) {
        return (local_name, None);
    }

    if let Some(captures) = regex.captures(spec) {
        let name = captures
            .get(1)
            .map(|value| value.as_str().to_string())
            .unwrap_or_else(|| spec.to_string());
        let version = captures.get(2).map(|value| value.as_str().to_string());
        return (name, version);
    }

    (spec.to_string(), None)
}

fn is_windows_absolute_path(path: &str) -> bool {
    static WINDOWS_ABSOLUTE_RE: OnceLock<Regex> = OnceLock::new();
    let regex = WINDOWS_ABSOLUTE_RE
        .get_or_init(|| Regex::new(r"^[A-Za-z]:[\\/]|^\\\\").expect("windows path regex"));
    regex.is_match(path)
}

fn has_explicit_git_protocol(source: &str) -> bool {
    static PROTOCOL_RE: OnceLock<Regex> = OnceLock::new();
    let regex =
        PROTOCOL_RE.get_or_init(|| Regex::new(r"^(https?|ssh|git)://").expect("protocol regex"));
    regex.is_match(source)
}

fn split_git_ref(url: &str) -> (String, Option<String>) {
    if let Some(index) = url.find('#') {
        let repo = trim_repo_url(&url[..index]).to_string();
        let git_ref = url[index + 1..].trim();
        if !repo.is_empty() && !git_ref.is_empty() {
            return (repo, Some(git_ref.to_string()));
        }
    }

    if let Some(captures) = scp_like_repo_regex().captures(url) {
        let host = captures
            .get(1)
            .map(|value| value.as_str())
            .unwrap_or_default();
        let tail = captures
            .get(2)
            .map(|value| value.as_str())
            .unwrap_or_default();
        if let Some((path, git_ref)) = tail.rsplit_once('@') {
            if !path.is_empty() && !git_ref.is_empty() {
                return (format!("git@{host}:{path}"), Some(git_ref.to_string()));
            }
        }
        return (trim_repo_url(url).to_string(), None);
    }

    if has_explicit_git_protocol(url) {
        if let Some((scheme, rest)) = url.split_once("://") {
            if let Some(slash_index) = rest.find('/') {
                let authority = &rest[..slash_index];
                let path_with_query = &rest[slash_index + 1..];
                let path = path_with_query.split('?').next().unwrap_or(path_with_query);
                if let Some((repo_path, git_ref)) = path.rsplit_once('@') {
                    if !repo_path.is_empty() && !git_ref.is_empty() {
                        let repo = format!("{scheme}://{authority}/{repo_path}");
                        return (trim_repo_url(&repo).to_string(), Some(git_ref.to_string()));
                    }
                }
            }
        }
    } else if let Some((repo, git_ref)) = url.rsplit_once('@') {
        if repo.contains('/') && !git_ref.is_empty() {
            return (trim_repo_url(repo).to_string(), Some(git_ref.to_string()));
        }
    }

    (trim_repo_url(url).to_string(), None)
}

fn parse_git_repo_components(repo: &str) -> Option<(String, String, String)> {
    if let Some(captures) = scp_like_repo_regex().captures(repo) {
        let host = captures.get(1).map(|value| value.as_str().to_string())?;
        let path = captures.get(2).map(|value| value.as_str().to_string())?;
        return Some((trim_repo_url(repo).to_string(), host, path));
    }

    if has_explicit_git_protocol(repo) {
        let (scheme, rest) = repo.split_once("://")?;
        let slash_index = rest.find('/')?;
        let authority = &rest[..slash_index];
        let host = authority
            .rsplit_once('@')
            .map(|(_, value)| value)
            .unwrap_or(authority)
            .to_string();
        let path = rest[slash_index + 1..]
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string();
        let normalized_repo = trim_repo_url(&format!("{scheme}://{authority}/{path}")).to_string();
        return Some((normalized_repo, host, path));
    }

    let (host, path) = repo.split_once('/')?;
    if !host.contains('.') && host != "localhost" {
        return None;
    }
    Some((
        format!("https://{}", trim_repo_url(repo)),
        host.to_string(),
        path.to_string(),
    ))
}

fn normalize_repo_path(path: &str) -> Option<String> {
    let path = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string();
    if path.is_empty() {
        return None;
    }
    if path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count()
        < 2
    {
        return None;
    }
    Some(path)
}

fn trim_repo_url(value: &str) -> &str {
    value.trim_end_matches('/')
}

fn scp_like_repo_regex() -> &'static Regex {
    static SCP_RE: OnceLock<Regex> = OnceLock::new();
    SCP_RE.get_or_init(|| Regex::new(r"^git@([^:]+):(.+)$").expect("scp-like regex"))
}

fn resolve_local_path(input: &str, base_dir: Option<&Path>) -> PathBuf {
    let expanded = if input == "~" {
        env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(input))
    } else if let Some(suffix) = input.strip_prefix("~/") {
        env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(suffix)
    } else {
        PathBuf::from(input)
    };

    let absolute = if expanded.is_absolute() || is_windows_absolute_path(input) {
        expanded
    } else {
        base_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .join(expanded)
    };
    normalize_path(&absolute)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let from = normalize_path(from);
    let to = normalize_path(to);

    let from_components = from.components().collect::<Vec<_>>();
    let to_components = to.components().collect::<Vec<_>>();

    let mut common_prefix_len = 0usize;
    while common_prefix_len < from_components.len()
        && common_prefix_len < to_components.len()
        && from_components[common_prefix_len] == to_components[common_prefix_len]
    {
        common_prefix_len += 1;
    }

    if common_prefix_len == 0
        && from_components
            .first()
            .map(|component| component.as_os_str())
            != to_components.first().map(|component| component.as_os_str())
    {
        return None;
    }

    let mut relative = PathBuf::new();
    for component in &from_components[common_prefix_len..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &to_components[common_prefix_len..] {
        relative.push(component.as_os_str());
    }

    if relative.as_os_str().is_empty() {
        Some(PathBuf::from("."))
    } else {
        Some(relative)
    }
}

fn npm_package_relative_path(name: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for segment in name.split('/') {
        path.push(segment);
    }
    path
}

fn resolve_npm_local_package_name(spec: &str, base_dir: Option<&Path>) -> Option<String> {
    let local_path = if let Some(path) = spec.strip_prefix("file:") {
        resolve_local_path(path, base_dir)
    } else if spec.starts_with('.')
        || spec.starts_with('/')
        || spec.starts_with('~')
        || is_windows_absolute_path(spec)
    {
        resolve_local_path(spec, base_dir)
    } else {
        return None;
    };

    let package_json_path = local_path.join("package.json");
    let content = fs::read_to_string(package_json_path).ok()?;
    let parsed = serde_json::from_str::<Value>(&content).ok()?;
    parsed.get("name")?.as_str().map(ToOwned::to_owned)
}

fn identity_to_dir_name(identity: &str) -> String {
    identity
        .chars()
        .map(|char| match char {
            '/' | '\\' | ':' | '@' | '#' | '?' | '=' => '-',
            value => value,
        })
        .collect()
}

fn settings_scope_for_install_scope(scope: PackageInstallScope) -> SettingsScope {
    match scope {
        PackageInstallScope::User | PackageInstallScope::Temporary => SettingsScope::Global,
        PackageInstallScope::Project => SettingsScope::Project,
    }
}

fn installed_package_from_record(record: &PackageInstallRecord) -> InstalledPackage {
    InstalledPackage {
        source: record.source.clone(),
        identity: record.identity.clone(),
        scope: record.scope,
        install_path: PathBuf::from(&record.install_path),
        pinned: record.pinned,
    }
}

fn package_source_from_entry(entry: &Value) -> Option<String> {
    match entry {
        Value::String(value) => Some(value.clone()),
        Value::Object(object) => object
            .get("source")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use pi_rust_config::SettingsManager;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parses_npm_source_with_optional_version() {
        let source = parse_package_source("npm:@scope/pkg@1.2.3");
        assert_eq!(
            source,
            PackageSource::Npm {
                spec: "@scope/pkg@1.2.3".to_string(),
                name: "@scope/pkg".to_string(),
                version: Some("1.2.3".to_string()),
                pinned: true,
            }
        );

        let unpinned = parse_package_source("npm:chalk");
        assert_eq!(
            unpinned,
            PackageSource::Npm {
                spec: "chalk".to_string(),
                name: "chalk".to_string(),
                version: None,
                pinned: false,
            }
        );
    }

    #[test]
    fn parses_git_sources_from_prefix_and_protocol_forms() {
        let prefixed = parse_package_source("git:github.com/acme/tools@main");
        assert_eq!(
            prefixed,
            PackageSource::Git {
                repo: "https://github.com/acme/tools".to_string(),
                host: "github.com".to_string(),
                path: "acme/tools".to_string(),
                git_ref: Some("main".to_string()),
                pinned: true,
            }
        );

        let protocol = parse_package_source("https://github.com/acme/tools.git@v1.0.0");
        assert_eq!(
            protocol,
            PackageSource::Git {
                repo: "https://github.com/acme/tools.git".to_string(),
                host: "github.com".to_string(),
                path: "acme/tools".to_string(),
                git_ref: Some("v1.0.0".to_string()),
                pinned: true,
            }
        );
    }

    #[test]
    fn falls_back_to_local_for_unqualified_non_path_sources() {
        let source = parse_package_source("github.com/acme/tools");
        assert_eq!(
            source,
            PackageSource::Local {
                path: "github.com/acme/tools".to_string()
            }
        );
    }

    #[test]
    fn package_identity_normalizes_git_and_local_sources() {
        let git_identity = package_identity_from_input("git:github.com/ACME/Tools", None);
        assert_eq!(git_identity, "git:github.com/acme/tools");

        let base_dir = PathBuf::from("/tmp/work");
        let local_identity =
            package_identity_from_input("./plugins/../plugins/pkg", Some(&base_dir));
        assert_eq!(local_identity, "local:/tmp/work/plugins/pkg");
    }

    #[test]
    fn package_state_store_round_trips_upsert_and_remove() {
        let tempdir = tempdir().expect("tempdir");
        let state_path = tempdir.path().join("packages-state.json");
        let store = PackageStateStore::new(state_path.clone());

        let initial = store.load().expect("load initial");
        assert_eq!(initial.version, CURRENT_PACKAGE_STATE_VERSION);
        assert!(initial.packages.is_empty());

        let record = build_install_record(
            "npm:@scope/pkg@1.2.3",
            PackageInstallScope::User,
            "/tmp/install/pkg",
            None,
        );
        store.upsert(record.clone()).expect("upsert record");

        let mut loaded = store.load().expect("load with record");
        assert_eq!(loaded.packages.len(), 1);
        assert_eq!(loaded.packages[0], record);

        let updated = build_install_record(
            "npm:@scope/pkg@1.2.3",
            PackageInstallScope::User,
            "/tmp/install/pkg-v2",
            None,
        );
        store.upsert(updated.clone()).expect("upsert updated");
        loaded = store.load().expect("load updated");
        assert_eq!(loaded.packages.len(), 1);
        assert_eq!(loaded.packages[0], updated);

        let removed = store
            .remove(&updated.identity, Some(PackageInstallScope::User))
            .expect("remove package");
        assert!(removed);
        loaded = store.load().expect("load after remove");
        assert!(loaded.packages.is_empty());
        assert!(state_path.exists());
    }

    #[test]
    fn package_manager_persists_sources_in_settings_by_scope() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        fs::create_dir_all(&cwd).expect("cwd");
        let agent_dir = tempdir.path().join("agent");
        let settings_manager = SettingsManager::create(&cwd, Some(agent_dir.clone()));
        let mut manager =
            PackageManager::with_settings_manager(&cwd, Some(agent_dir), settings_manager);

        let local_pkg = cwd.join("pkg");
        fs::create_dir_all(&local_pkg).expect("local package");
        manager
            .install("./pkg", PackageInstallScope::Project)
            .expect("install project package");
        manager
            .install("npm:chalk", PackageInstallScope::User)
            .expect("install user package");

        let global_packages = manager
            .settings_manager()
            .get_string_list("packages", Some(SettingsScope::Global));
        let project_packages = manager
            .settings_manager()
            .get_string_list("packages", Some(SettingsScope::Project));

        assert_eq!(global_packages, vec!["npm:chalk".to_string()]);
        assert_eq!(project_packages, vec!["../pkg".to_string()]);
        assert_eq!(manager.list_by_scope(PackageInstallScope::User).len(), 1);
        assert_eq!(manager.list_by_scope(PackageInstallScope::Project).len(), 1);
    }

    #[test]
    fn package_manager_accepts_object_entries_and_project_scope_wins() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        fs::create_dir_all(&cwd).expect("cwd");
        let agent_dir = tempdir.path().join("agent");
        let settings_manager = SettingsManager::create(&cwd, Some(agent_dir.clone()));
        let mut manager =
            PackageManager::with_settings_manager(&cwd, Some(agent_dir), settings_manager);

        manager
            .settings_manager_mut()
            .update_global_settings(serde_json::json!({
                "packages": [
                    {
                        "source": "npm:chalk",
                        "extensions": ["src/**/*.ts"],
                        "skills": [],
                        "prompts": ["!docs/**"],
                        "themes": ["themes/**"]
                    }
                ]
            }))
            .expect("seed global packages");
        manager
            .settings_manager_mut()
            .update_project_settings(serde_json::json!({
                "packages": [
                    "npm:chalk"
                ]
            }))
            .expect("seed project packages");

        let user_packages = manager.list_by_scope(PackageInstallScope::User);
        let project_packages = manager.list_by_scope(PackageInstallScope::Project);
        let all_packages = manager.list_all();

        assert_eq!(user_packages.len(), 1);
        assert_eq!(user_packages[0].source, "npm:chalk");
        assert_eq!(project_packages.len(), 1);
        assert_eq!(project_packages[0].source, "npm:chalk");
        assert_eq!(all_packages.len(), 1);
        assert_eq!(all_packages[0].scope, PackageInstallScope::Project);
        assert_eq!(all_packages[0].source, "npm:chalk");

        let parsed = serde_json::from_value::<PackageConfigEntry>(serde_json::json!({
            "source": "npm:chalk",
            "extensions": ["src/**/*.ts"],
            "skills": [],
            "prompts": ["!docs/**"],
            "themes": ["themes/**"]
        }))
        .expect("parse package entry");
        match parsed {
            PackageConfigEntry::Object {
                source,
                extensions,
                skills,
                prompts,
                themes,
            } => {
                assert_eq!(source, "npm:chalk");
                assert_eq!(extensions, Some(vec!["src/**/*.ts".to_string()]));
                assert_eq!(skills, Some(Vec::new()));
                assert_eq!(prompts, Some(vec!["!docs/**".to_string()]));
                assert_eq!(themes, Some(vec!["themes/**".to_string()]));
            }
            PackageConfigEntry::Source(_) => panic!("expected object package entry"),
        }
    }

    #[test]
    fn project_scope_local_sources_are_stored_relative_to_pi_directory() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        fs::create_dir_all(&cwd).expect("cwd");
        let agent_dir = tempdir.path().join("agent");
        let settings_manager = SettingsManager::create(&cwd, Some(agent_dir.clone()));
        let mut manager =
            PackageManager::with_settings_manager(&cwd, Some(agent_dir), settings_manager);

        fs::create_dir_all(cwd.join("pkg")).expect("pkg dir");

        manager
            .install("./pkg", PackageInstallScope::Project)
            .expect("install project package");

        let project_packages = manager
            .settings_manager()
            .get_string_list("packages", Some(SettingsScope::Project));

        assert_eq!(project_packages, vec!["../pkg".to_string()]);
    }

    #[test]
    fn package_manager_remove_matches_by_identity() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        fs::create_dir_all(&cwd).expect("cwd");
        let agent_dir = tempdir.path().join("agent");
        let settings_manager = SettingsManager::create(&cwd, Some(agent_dir.clone()));
        let mut manager =
            PackageManager::with_settings_manager(&cwd, Some(agent_dir), settings_manager);
        manager
            .settings_manager_mut()
            .set_string_list(
                SettingsScope::Global,
                "packages",
                &["git:github.com/acme/tools@main".to_string()],
            )
            .expect("seed settings");
        manager
            .state_store
            .upsert(build_install_record(
                "git:github.com/acme/tools@main",
                PackageInstallScope::User,
                manager
                    .installed_path("git:github.com/acme/tools@main", PackageInstallScope::User)
                    .to_string_lossy()
                    .to_string(),
                Some(manager.base_dir_for_scope(PackageInstallScope::User)),
            ))
            .expect("seed state");

        let removed = manager
            .remove(
                "https://github.com/acme/tools.git@v2",
                PackageInstallScope::User,
            )
            .expect("remove package");
        assert!(removed);
        assert!(
            manager
                .settings_manager()
                .get_string_list("packages", Some(SettingsScope::Global))
                .is_empty()
        );
    }

    #[test]
    fn package_manager_materializes_local_npm_source() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        fs::create_dir_all(&cwd).expect("cwd");
        let agent_dir = tempdir.path().join("agent");
        let settings_manager = SettingsManager::create(&cwd, Some(agent_dir.clone()));
        let mut manager =
            PackageManager::with_settings_manager(&cwd, Some(agent_dir), settings_manager);

        let npm_version = Command::new("npm").arg("--version").output();
        if npm_version.is_err() {
            return;
        }

        let package_dir = cwd.join("npm-pkg");
        fs::create_dir_all(&package_dir).expect("package dir");
        fs::write(
            package_dir.join("package.json"),
            r#"{"name":"fixture-pkg","version":"1.0.0"}"#,
        )
        .expect("package json");
        fs::write(package_dir.join("index.js"), "module.exports = 1;\n").expect("index");

        let installed = manager
            .install("npm:./npm-pkg", PackageInstallScope::Project)
            .expect("install npm package");

        assert!(installed.install_path.exists());
        assert_eq!(
            installed.install_path,
            cwd.join(".pi")
                .join("npm")
                .join("node_modules")
                .join("fixture-pkg")
        );
        assert!(installed.install_path.join("package.json").exists());
    }
}
