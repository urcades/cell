use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageResourceKind {
    Extensions,
    Skills,
    Prompts,
    Themes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageResourceState {
    pub path: PathBuf,
    pub enabled: bool,
}

impl PackageConfigEntry {
    pub fn source(&self) -> &str {
        match self {
            Self::Source(source) => source,
            Self::Object { source, .. } => source,
        }
    }

    pub fn resource_filters(&self, kind: PackageResourceKind) -> Option<&[String]> {
        match self {
            Self::Source(_) => None,
            Self::Object {
                extensions,
                skills,
                prompts,
                themes,
                ..
            } => match kind {
                PackageResourceKind::Extensions => extensions.as_deref(),
                PackageResourceKind::Skills => skills.as_deref(),
                PackageResourceKind::Prompts => prompts.as_deref(),
                PackageResourceKind::Themes => themes.as_deref(),
            },
        }
    }

    pub fn set_resource_filters(
        &mut self,
        kind: PackageResourceKind,
        filters: Option<Vec<String>>,
    ) {
        match self {
            Self::Source(source) => {
                if filters.is_none() {
                    return;
                }
                let source = source.clone();
                let mut entry = Self::Object {
                    source,
                    extensions: None,
                    skills: None,
                    prompts: None,
                    themes: None,
                };
                entry.set_resource_filters(kind, filters);
                *self = entry;
            }
            Self::Object {
                source,
                extensions,
                skills,
                prompts,
                themes,
            } => {
                match kind {
                    PackageResourceKind::Extensions => *extensions = filters,
                    PackageResourceKind::Skills => *skills = filters,
                    PackageResourceKind::Prompts => *prompts = filters,
                    PackageResourceKind::Themes => *themes = filters,
                }

                if extensions.is_none() && skills.is_none() && prompts.is_none() && themes.is_none()
                {
                    *self = Self::Source(source.clone());
                }
            }
        }
    }

    pub fn toggle_resource_entry(&mut self, kind: PackageResourceKind, path: &str, enabled: bool) {
        let next_filters = self
            .resource_filters(kind)
            .map(|filters| toggle_exact_entries(filters, path, enabled))
            .unwrap_or_else(|| {
                vec![if enabled {
                    format!("+{}", normalize_exact_entry(path))
                } else {
                    format!("-{}", normalize_exact_entry(path))
                }]
            });
        self.set_resource_filters(kind, Some(next_filters));
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
    #[error("failed to acquire package state lock {path}: {message}")]
    Lock { path: PathBuf, message: String },
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
        let _lock = PackageStateFileLock::acquire(&self.path)?;
        self.load_unlocked()
    }

    pub fn save(&self, state: &PackageState) -> Result<(), PackageStateError> {
        let _lock = PackageStateFileLock::acquire(&self.path)?;
        self.save_unlocked(state)
    }

    pub fn transact<R>(
        &self,
        mutate: impl FnOnce(&mut PackageState) -> R,
    ) -> Result<R, PackageStateError> {
        let _lock = PackageStateFileLock::acquire(&self.path)?;
        let mut state = self.load_unlocked()?;
        let result = mutate(&mut state);
        self.save_unlocked(&state)?;
        Ok(result)
    }

    fn load_unlocked(&self) -> Result<PackageState, PackageStateError> {
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

    fn save_unlocked(&self, state: &PackageState) -> Result<(), PackageStateError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| PackageStateError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let payload = serde_json::to_string_pretty(state)?;
        let temp_path = package_state_temp_path(&self.path);
        fs::write(&temp_path, payload).map_err(|source| PackageStateError::Io {
            path: temp_path.clone(),
            source,
        })?;
        fs::rename(&temp_path, &self.path).map_err(|source| PackageStateError::Io {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub fn upsert(&self, record: PackageInstallRecord) -> Result<(), PackageStateError> {
        self.transact(|state| {
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
        })?;
        Ok(())
    }

    pub fn remove(
        &self,
        identity: &str,
        scope: Option<PackageInstallScope>,
    ) -> Result<bool, PackageStateError> {
        self.transact(|state| {
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
            }
            changed
        })
    }
}

fn package_state_lock_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.lock", path.display()))
}

fn package_state_temp_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.display()))
}

struct PackageStateFileLock {
    path: PathBuf,
}

impl PackageStateFileLock {
    fn acquire(path: &Path) -> Result<Self, PackageStateError> {
        const LOCK_WAIT: Duration = Duration::from_millis(500);
        const RETRY_DELAY: Duration = Duration::from_millis(10);

        let lock_path = package_state_lock_path(path);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|source| PackageStateError::Io {
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
                        return Err(PackageStateError::Lock {
                            path: lock_path,
                            message: "timed out waiting for package state lock".to_string(),
                        });
                    }
                    thread::sleep(RETRY_DELAY);
                }
                Err(error) => {
                    return Err(PackageStateError::Lock {
                        path: lock_path,
                        message: error.to_string(),
                    });
                }
            }
        }
    }
}

impl Drop for PackageStateFileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
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
            let parsed = parse_package_source_with_base(&source, Some(&resolve_base));
            let identity = package_identity_from_input(&source, Some(&resolve_base));
            if !seen.insert(identity.clone()) {
                continue;
            }

            let installed = state
                .packages
                .iter()
                .find(|record| record.scope == scope && record.identity == identity)
                .and_then(|record| {
                    let install_path = PathBuf::from(&record.install_path);
                    if matches!(parsed, PackageSource::Local { .. }) && !install_path.exists() {
                        None
                    } else {
                        Some(InstalledPackage {
                            source: source.clone(),
                            identity: record.identity.clone(),
                            scope: record.scope,
                            install_path,
                            pinned: record.pinned,
                        })
                    }
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
        self.add_package_source(source, scope)?;
        Ok(installed_package_from_record(&record))
    }

    pub fn remove(
        &mut self,
        source: &str,
        scope: PackageInstallScope,
    ) -> Result<bool, PackageManagerError> {
        let parsed = parse_package_source_with_base(source, Some(self.base_dir_for_scope(scope)));
        let identity = package_identity(&parsed, Some(self.base_dir_for_scope(scope)));
        let removed = self.remove_package_source(source, scope)?;
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

    pub fn add_package_source(
        &mut self,
        source: &str,
        scope: PackageInstallScope,
    ) -> Result<bool, PackageManagerError> {
        let settings_scope = settings_scope_for_install_scope(scope);
        let identity = package_identity_from_input(source, Some(self.base_dir_for_scope(scope)));
        let normalized_source = self.normalize_source_for_settings(source, scope);
        let package_base_dir = self.settings_base_dir_for_scope(scope);
        let install_base_dir = self.base_dir_for_scope(scope).to_path_buf();

        self.settings_manager
            .transact_scoped_settings(settings_scope, |settings| {
                let mut packages = package_entries_from_settings(settings);
                if packages.iter().any(|existing| {
                    let existing_identity = package_entry_identity(
                        existing,
                        scope,
                        &install_base_dir,
                        &package_base_dir,
                    );
                    existing_identity == identity
                }) {
                    return Ok(false);
                }

                packages.push(PackageConfigEntry::Source(normalized_source.clone()));
                write_package_entries(settings, packages);
                Ok(true)
            })
            .map_err(Into::into)
    }

    pub fn remove_package_source(
        &mut self,
        source: &str,
        scope: PackageInstallScope,
    ) -> Result<bool, PackageManagerError> {
        let settings_scope = settings_scope_for_install_scope(scope);
        let target_identity =
            package_identity_from_input(source, Some(self.base_dir_for_scope(scope)));
        let package_base_dir = self.settings_base_dir_for_scope(scope);
        let install_base_dir = self.base_dir_for_scope(scope).to_path_buf();

        self.settings_manager
            .transact_scoped_settings(settings_scope, |settings| {
                let mut packages = package_entries_from_settings(settings);
                let before = packages.len();
                packages.retain(|existing| {
                    let existing_identity = package_entry_identity(
                        existing,
                        scope,
                        &install_base_dir,
                        &package_base_dir,
                    );
                    existing_identity != target_identity
                });
                let changed = before != packages.len();
                if changed {
                    write_package_entries(settings, packages);
                }
                Ok(changed)
            })
            .map_err(Into::into)
    }

    pub fn set_package_filters(
        &mut self,
        identity: &str,
        scope: PackageInstallScope,
        kind: PackageResourceKind,
        filters: Option<&[String]>,
    ) -> Result<bool, PackageManagerError> {
        self.update_package_entry(identity, scope, |entry| {
            entry.set_resource_filters(kind, filters.map(|values| values.to_vec()));
            true
        })
    }

    pub fn toggle_package_resource(
        &mut self,
        identity: &str,
        scope: PackageInstallScope,
        kind: PackageResourceKind,
        install_root: &Path,
        path: impl AsRef<Path>,
        enabled: bool,
    ) -> Result<bool, PackageManagerError> {
        let path = normalize_package_resource_path(install_root, path.as_ref());
        self.update_package_entry(identity, scope, |entry| {
            entry.toggle_resource_entry(kind, &path, enabled);
            true
        })
    }

    pub fn set_package_resource_enabled(
        &mut self,
        identity: &str,
        scope: PackageInstallScope,
        kind: PackageResourceKind,
        install_root: &Path,
        path: impl AsRef<Path>,
        enabled: bool,
    ) -> Result<bool, PackageManagerError> {
        let path = normalize_package_resource_path(install_root, path.as_ref());
        self.update_package_entry(identity, scope, |entry| {
            let mut states = normalized_exact_resource_states(entry.resource_filters(kind));
            if let Some(existing) = states.iter_mut().find(|state| state.path == path) {
                existing.enabled = enabled;
            } else {
                states.push(NormalizedPackageResourceState {
                    path: path.clone(),
                    enabled,
                });
            }
            states.sort_by(|left, right| left.path.cmp(&right.path));
            states.dedup_by(|left, right| left.path == right.path);
            let next_filters = build_package_resource_filters_from_exact_states(
                entry.resource_filters(kind),
                &states,
            );
            entry.set_resource_filters(kind, next_filters);
            true
        })
    }

    pub fn sync_package_resource_states(
        &mut self,
        identity: &str,
        scope: PackageInstallScope,
        kind: PackageResourceKind,
        install_root: &Path,
        states: &[PackageResourceState],
    ) -> Result<bool, PackageManagerError> {
        let normalized_states = normalize_package_resource_states(states, install_root);
        self.update_package_entry(identity, scope, |entry| {
            let next_filters =
                build_package_resource_filters(entry.resource_filters(kind), &normalized_states);
            entry.set_resource_filters(kind, next_filters);
            true
        })
    }

    pub fn package_resource_enabled(
        &self,
        identity: &str,
        scope: PackageInstallScope,
        kind: PackageResourceKind,
        install_root: &Path,
        path: &Path,
    ) -> bool {
        let settings_scope = settings_scope_for_install_scope(scope);
        let package_base_dir = self.settings_base_dir_for_scope(scope);
        let install_base_dir = self.base_dir_for_scope(scope).to_path_buf();
        let normalized_path = normalize_package_resource_path(install_root, path);

        for entry in self.package_entries_for_scope(settings_scope) {
            let existing_identity =
                package_entry_identity(&entry, scope, &install_base_dir, &package_base_dir);
            if existing_identity != identity {
                continue;
            }

            let Some(filters) = entry.resource_filters(kind) else {
                return true;
            };

            if filters.is_empty() {
                return false;
            }

            return path_enabled_by_filters(&normalized_path, filters);
        }

        true
    }

    fn update_package_entry(
        &mut self,
        identity: &str,
        scope: PackageInstallScope,
        mutate: impl FnOnce(&mut PackageConfigEntry) -> bool,
    ) -> Result<bool, PackageManagerError> {
        let settings_scope = settings_scope_for_install_scope(scope);
        let package_base_dir = self.settings_base_dir_for_scope(scope);
        let install_base_dir = self.base_dir_for_scope(scope).to_path_buf();

        self.settings_manager
            .transact_scoped_settings(settings_scope, |settings| {
                let mut packages = package_entries_from_settings(settings);
                let mut changed = false;
                for entry in packages.iter_mut() {
                    let before = entry.clone();
                    let existing_identity =
                        package_entry_identity(entry, scope, &install_base_dir, &package_base_dir);
                    if existing_identity == identity {
                        changed = mutate(entry) && *entry != before;
                        break;
                    }
                }
                if changed {
                    write_package_entries(settings, packages);
                }
                Ok(changed)
            })
            .map_err(Into::into)
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
    normalize_macos_private_var_path(normalized)
}

fn normalize_macos_private_var_path(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(stripped) = path.strip_prefix("/private") {
            let candidate = Path::new("/").join(stripped);
            if candidate.exists() || candidate.parent().is_some_and(std::path::Path::exists) {
                return candidate;
            }
        }
    }
    path
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

fn package_entries_from_settings(settings: &Value) -> Vec<PackageConfigEntry> {
    settings
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| serde_json::from_value::<PackageConfigEntry>(entry.clone()).ok())
        .collect()
}

fn write_package_entries(settings: &mut Value, packages: Vec<PackageConfigEntry>) {
    let object = settings
        .as_object_mut()
        .expect("load_settings guarantees root object");
    if packages.is_empty() {
        object.remove("packages");
        return;
    }

    object.insert(
        "packages".to_string(),
        Value::Array(
            packages
                .into_iter()
                .map(|entry| serde_json::to_value(entry).expect("package entries must serialize"))
                .collect(),
        ),
    );
}

fn package_entry_identity(
    entry: &PackageConfigEntry,
    scope: PackageInstallScope,
    install_base_dir: &Path,
    settings_base_dir: &Path,
) -> String {
    let source = entry.source();
    let identity_base_dir =
        identity_base_dir_for_settings_source(source, scope, install_base_dir, settings_base_dir);
    package_identity_from_input(source, Some(&identity_base_dir))
}

fn identity_base_dir_for_settings_source(
    source: &str,
    scope: PackageInstallScope,
    install_base_dir: &Path,
    settings_base_dir: &Path,
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
            return install_base_dir.to_path_buf();
        }
    }

    match scope {
        PackageInstallScope::User | PackageInstallScope::Temporary => {
            settings_base_dir.to_path_buf()
        }
        PackageInstallScope::Project => settings_base_dir.to_path_buf(),
    }
}

fn toggle_exact_entries(entries: &[String], target: &str, enabled: bool) -> Vec<String> {
    let target = normalize_exact_entry(target);
    let mut next = Vec::new();
    let mut replaced = false;

    for entry in entries {
        if normalize_exact_entry(entry) == target {
            if !replaced {
                next.push(if enabled {
                    format!("+{target}")
                } else {
                    format!("-{target}")
                });
                replaced = true;
            }
            continue;
        }
        next.push(entry.clone());
    }

    if !replaced {
        next.push(if enabled {
            format!("+{target}")
        } else {
            format!("-{target}")
        });
    }

    next
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedPackageResourceState {
    path: String,
    enabled: bool,
}

fn normalize_package_resource_states(
    states: &[PackageResourceState],
    install_root: &Path,
) -> Vec<NormalizedPackageResourceState> {
    let mut normalized = states
        .iter()
        .map(|state| NormalizedPackageResourceState {
            path: normalize_package_resource_path(install_root, &state.path),
            enabled: state.enabled,
        })
        .collect::<Vec<_>>();
    normalized.sort_by(|left, right| left.path.cmp(&right.path));
    normalized.dedup_by(|left, right| left.path == right.path);
    normalized
}

fn normalized_exact_resource_states(
    current: Option<&[String]>,
) -> Vec<NormalizedPackageResourceState> {
    let mut states = current
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            if let Some(path) = entry.strip_prefix('+') {
                Some(NormalizedPackageResourceState {
                    path: normalize_exact_entry(path),
                    enabled: true,
                })
            } else {
                entry
                    .strip_prefix('-')
                    .map(|path| NormalizedPackageResourceState {
                        path: normalize_exact_entry(path),
                        enabled: false,
                    })
            }
        })
        .collect::<Vec<_>>();
    states.sort_by(|left, right| left.path.cmp(&right.path));
    states.dedup_by(|left, right| left.path == right.path);
    states
}

fn normalize_package_resource_path(install_root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(install_root).unwrap_or(path);
    normalize_exact_entry(relative.to_string_lossy().as_ref())
}

fn build_package_resource_filters(
    current: Option<&[String]>,
    states: &[NormalizedPackageResourceState],
) -> Option<Vec<String>> {
    if states.is_empty() {
        return current.map(|filters| filters.to_vec());
    }
    if states.iter().all(|state| !state.enabled) {
        return Some(Vec::new());
    }

    let current_filters = current.unwrap_or(&[]);
    let fully_disabled = current.is_some() && current_filters.is_empty();
    let base_filters = current_filters
        .iter()
        .filter(|entry| !is_exact_filter_entry(entry))
        .cloned()
        .collect::<Vec<_>>();

    if fully_disabled {
        return Some(
            states
                .iter()
                .filter(|state| state.enabled)
                .map(|state| format!("+{}", state.path))
                .collect(),
        );
    }

    let exact_overrides = states
        .iter()
        .filter_map(|state| {
            let base_enabled = path_enabled_by_filters(&state.path, &base_filters);
            if state.enabled == base_enabled {
                None
            } else if state.enabled {
                Some(format!("+{}", state.path))
            } else {
                Some(format!("-{}", state.path))
            }
        })
        .collect::<Vec<_>>();

    if base_filters.is_empty() && exact_overrides.is_empty() {
        None
    } else {
        let mut next = base_filters;
        next.extend(exact_overrides);
        Some(next)
    }
}

fn build_package_resource_filters_from_exact_states(
    current: Option<&[String]>,
    exact_states: &[NormalizedPackageResourceState],
) -> Option<Vec<String>> {
    let current_filters = current.unwrap_or(&[]);
    let fully_disabled = current.is_some() && current_filters.is_empty();
    let base_filters = current_filters
        .iter()
        .filter(|entry| !is_exact_filter_entry(entry))
        .cloned()
        .collect::<Vec<_>>();

    if fully_disabled {
        return Some(
            exact_states
                .iter()
                .filter(|state| state.enabled)
                .map(|state| format!("+{}", state.path))
                .collect(),
        );
    }

    let exact_overrides = exact_states
        .iter()
        .filter_map(|state| {
            let base_enabled = path_enabled_by_filters(&state.path, &base_filters);
            if state.enabled == base_enabled {
                None
            } else if state.enabled {
                Some(format!("+{}", state.path))
            } else {
                Some(format!("-{}", state.path))
            }
        })
        .collect::<Vec<_>>();

    if base_filters.is_empty() && exact_overrides.is_empty() {
        None
    } else {
        let mut next = base_filters;
        next.extend(exact_overrides);
        Some(next)
    }
}

fn is_exact_filter_entry(value: &str) -> bool {
    value.starts_with('+') || value.starts_with('-')
}

fn path_enabled_by_filters(path: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }

    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    let mut force_includes = Vec::new();
    let mut force_excludes = Vec::new();

    for filter in filters {
        if let Some(rest) = filter.strip_prefix('+') {
            force_includes.push(rest.to_string());
        } else if let Some(rest) = filter.strip_prefix('-') {
            force_excludes.push(rest.to_string());
        } else if let Some(rest) = filter.strip_prefix('!') {
            excludes.push(rest.to_string());
        } else {
            includes.push(filter.to_string());
        }
    }

    let mut enabled = includes.is_empty() || matches_any_package_pattern(path, &includes);
    if enabled && !excludes.is_empty() && matches_any_package_pattern(path, &excludes) {
        enabled = false;
    }
    if !enabled
        && !force_includes.is_empty()
        && matches_any_exact_package_pattern(path, &force_includes)
    {
        enabled = true;
    }
    if enabled
        && !force_excludes.is_empty()
        && matches_any_exact_package_pattern(path, &force_excludes)
    {
        enabled = false;
    }
    enabled
}

fn matches_any_package_pattern(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        let normalized = normalize_exact_entry(pattern);
        package_path_pattern_matches(&normalized, path)
            || package_path_pattern_matches(
                &normalized,
                Path::new(path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default(),
            )
            || package_path_pattern_matches(&normalized, package_parent_path(path))
    })
}

fn matches_any_exact_package_pattern(path: &str, patterns: &[String]) -> bool {
    let normalized_path = normalize_exact_entry(path);
    let file_name = Path::new(&normalized_path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string();
    let parent = package_parent_path(&normalized_path).to_string();

    patterns.iter().any(|pattern| {
        let normalized = normalize_exact_entry(pattern);
        normalized == normalized_path || normalized == file_name || normalized == parent
    })
}

fn package_parent_path(path: &str) -> &str {
    Path::new(path)
        .parent()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
}

fn package_path_pattern_matches(pattern: &str, candidate: &str) -> bool {
    if pattern.is_empty() {
        return candidate.is_empty();
    }
    if pattern.ends_with('/') {
        let prefix = pattern.trim_end_matches('/');
        return candidate == prefix || candidate.starts_with(&format!("{prefix}/"));
    }
    if !pattern.contains('/') {
        return package_glob_matches(pattern, candidate.rsplit('/').next().unwrap_or(candidate));
    }
    package_glob_matches(pattern, candidate)
}

fn package_glob_matches(pattern: &str, value: &str) -> bool {
    fn inner(
        pattern: &[char],
        value: &[char],
        p_index: usize,
        v_index: usize,
        memo: &mut HashMap<(usize, usize), bool>,
    ) -> bool {
        if let Some(result) = memo.get(&(p_index, v_index)) {
            return *result;
        }

        let result = if p_index == pattern.len() {
            v_index == value.len()
        } else {
            match pattern[p_index] {
                '*' => {
                    if p_index + 1 < pattern.len() && pattern[p_index + 1] == '*' {
                        inner(pattern, value, p_index + 2, v_index, memo)
                            || (v_index < value.len()
                                && inner(pattern, value, p_index, v_index + 1, memo))
                    } else {
                        inner(pattern, value, p_index + 1, v_index, memo)
                            || (v_index < value.len()
                                && value[v_index] != '/'
                                && inner(pattern, value, p_index, v_index + 1, memo))
                    }
                }
                '?' => {
                    v_index < value.len()
                        && value[v_index] != '/'
                        && inner(pattern, value, p_index + 1, v_index + 1, memo)
                }
                value_char => {
                    v_index < value.len()
                        && value_char == value[v_index]
                        && inner(pattern, value, p_index + 1, v_index + 1, memo)
                }
            }
        };

        memo.insert((p_index, v_index), result);
        result
    }

    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    inner(&pattern, &value, 0, 0, &mut HashMap::new())
}

fn normalize_exact_entry(entry: &str) -> String {
    let mut normalized = entry.trim().replace('\\', "/");
    if let Some(rest) = normalized
        .strip_prefix('+')
        .or_else(|| normalized.strip_prefix('-'))
    {
        normalized = rest.to_string();
    }
    while normalized.starts_with("./") {
        normalized = normalized[2..].to_string();
    }
    if normalized.starts_with('/') {
        normalized = normalized[1..].to_string();
    }
    normalized
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

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
    fn package_config_entry_resource_filters_round_trip_and_collapse() {
        let mut entry = PackageConfigEntry::Source("npm:chalk".to_string());
        entry.set_resource_filters(
            PackageResourceKind::Skills,
            Some(vec!["skills/**/*.md".to_string()]),
        );

        match &entry {
            PackageConfigEntry::Object {
                source,
                skills,
                prompts,
                themes,
                ..
            } => {
                assert_eq!(source, "npm:chalk");
                assert_eq!(skills, &Some(vec!["skills/**/*.md".to_string()]));
                assert_eq!(prompts, &None);
                assert_eq!(themes, &None);
            }
            PackageConfigEntry::Source(_) => panic!("expected object package entry"),
        }

        entry.set_resource_filters(PackageResourceKind::Skills, None);
        assert_eq!(entry, PackageConfigEntry::Source("npm:chalk".to_string()));
    }

    #[test]
    fn package_manager_toggles_package_resource_entries_with_exact_overrides() {
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
                "packages": ["npm:chalk"]
            }))
            .expect("seed package");

        let identity = "npm:chalk";
        manager
            .set_package_filters(
                identity,
                PackageInstallScope::User,
                PackageResourceKind::Skills,
                Some(&["skills/**/*.md".to_string()]),
            )
            .expect("set package filters");
        manager
            .toggle_package_resource(
                identity,
                PackageInstallScope::User,
                PackageResourceKind::Skills,
                Path::new("/tmp/install/chalk"),
                "skills/keep.md",
                true,
            )
            .expect("toggle package resource on");

        let global = manager
            .settings_manager()
            .scoped_settings(SettingsScope::Global)
            .clone();
        assert_eq!(
            global["packages"][0]["skills"],
            serde_json::json!(["skills/**/*.md", "+skills/keep.md"])
        );

        manager
            .toggle_package_resource(
                identity,
                PackageInstallScope::User,
                PackageResourceKind::Skills,
                Path::new("/tmp/install/chalk"),
                "skills/keep.md",
                false,
            )
            .expect("toggle package resource off");

        let global = manager
            .settings_manager()
            .scoped_settings(SettingsScope::Global)
            .clone();
        assert_eq!(
            global["packages"][0]["skills"],
            serde_json::json!(["skills/**/*.md", "-skills/keep.md"])
        );

        manager
            .set_package_filters(
                identity,
                PackageInstallScope::User,
                PackageResourceKind::Skills,
                None,
            )
            .expect("clear package filters");

        let global = manager
            .settings_manager()
            .scoped_settings(SettingsScope::Global)
            .clone();
        assert_eq!(global["packages"], serde_json::json!(["npm:chalk"]));
    }

    #[test]
    fn package_manager_set_package_resource_enabled_respects_base_filters() {
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
                "packages": [{
                    "source": "npm:chalk",
                    "skills": ["skills/*.md", "+extras/keep.md", "-skills/off.md"]
                }]
            }))
            .expect("seed package");

        manager
            .set_package_resource_enabled(
                "npm:chalk",
                PackageInstallScope::User,
                PackageResourceKind::Skills,
                Path::new("/tmp/install/chalk"),
                "/tmp/install/chalk/skills/off.md",
                true,
            )
            .expect("enable base-matched resource");

        let global = manager
            .settings_manager()
            .scoped_settings(SettingsScope::Global)
            .clone();
        assert_eq!(
            global["packages"][0]["skills"],
            serde_json::json!(["skills/*.md", "+extras/keep.md"])
        );

        manager
            .set_package_resource_enabled(
                "npm:chalk",
                PackageInstallScope::User,
                PackageResourceKind::Skills,
                Path::new("/tmp/install/chalk"),
                "/tmp/install/chalk/extras/keep.md",
                false,
            )
            .expect("disable exact-override resource");

        let global = manager
            .settings_manager()
            .scoped_settings(SettingsScope::Global)
            .clone();
        assert_eq!(
            global["packages"][0]["skills"],
            serde_json::json!(["skills/*.md"])
        );

        manager
            .set_package_resource_enabled(
                "npm:chalk",
                PackageInstallScope::User,
                PackageResourceKind::Skills,
                Path::new("/tmp/install/chalk"),
                "/tmp/install/chalk/extras/keep.md",
                true,
            )
            .expect("re-enable exact-override resource");

        let global = manager
            .settings_manager()
            .scoped_settings(SettingsScope::Global)
            .clone();
        assert_eq!(
            global["packages"][0]["skills"],
            serde_json::json!(["skills/*.md", "+extras/keep.md"])
        );
    }

    #[test]
    fn package_manager_syncs_visible_resource_states_with_mixed_filters() {
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
                "packages": [{
                    "source": "npm:chalk",
                    "skills": ["skills/*.md"]
                }]
            }))
            .expect("seed package");

        manager
            .sync_package_resource_states(
                "npm:chalk",
                PackageInstallScope::User,
                PackageResourceKind::Skills,
                Path::new("/tmp/install/chalk"),
                &[
                    PackageResourceState {
                        path: PathBuf::from("/tmp/install/chalk/skills/core.md"),
                        enabled: true,
                    },
                    PackageResourceState {
                        path: PathBuf::from("/tmp/install/chalk/skills/off.md"),
                        enabled: false,
                    },
                    PackageResourceState {
                        path: PathBuf::from("/tmp/install/chalk/extras/keep.md"),
                        enabled: true,
                    },
                ],
            )
            .expect("sync states");

        let global = manager
            .settings_manager()
            .scoped_settings(SettingsScope::Global)
            .clone();
        assert_eq!(
            global["packages"][0]["skills"],
            serde_json::json!(["skills/*.md", "+extras/keep.md", "-skills/off.md"])
        );
    }

    #[test]
    fn package_manager_syncs_visible_resource_states_to_empty_disable_list() {
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
                "packages": [{
                    "source": "npm:chalk",
                    "skills": ["skills/*.md"]
                }]
            }))
            .expect("seed package");

        manager
            .sync_package_resource_states(
                "npm:chalk",
                PackageInstallScope::User,
                PackageResourceKind::Skills,
                Path::new("/tmp/install/chalk"),
                &[
                    PackageResourceState {
                        path: PathBuf::from("/tmp/install/chalk/skills/core.md"),
                        enabled: false,
                    },
                    PackageResourceState {
                        path: PathBuf::from("/tmp/install/chalk/skills/off.md"),
                        enabled: false,
                    },
                ],
            )
            .expect("sync states");

        let global = manager
            .settings_manager()
            .scoped_settings(SettingsScope::Global)
            .clone();
        assert_eq!(global["packages"][0]["skills"], serde_json::json!([]));
    }

    #[test]
    fn package_state_store_concurrent_upserts_preserve_latest_state() {
        let tempdir = tempdir().expect("tempdir");
        let state_path = tempdir.path().join("packages-state.json");
        let store = PackageStateStore::new(state_path.clone());
        let other_store = store.clone();
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = Arc::clone(&barrier);
        let second_barrier = Arc::clone(&barrier);

        let first = thread::spawn(move || {
            first_barrier.wait();
            store
                .upsert(build_install_record(
                    "npm:chalk",
                    PackageInstallScope::User,
                    "/tmp/install/chalk",
                    None,
                ))
                .expect("first upsert");
        });

        let second = thread::spawn(move || {
            second_barrier.wait();
            other_store
                .upsert(build_install_record(
                    "npm:prettier",
                    PackageInstallScope::User,
                    "/tmp/install/prettier",
                    None,
                ))
                .expect("second upsert");
        });

        barrier.wait();
        first.join().expect("join first");
        second.join().expect("join second");

        let state = PackageStateStore::new(state_path)
            .load()
            .expect("load state");
        assert_eq!(state.packages.len(), 2);
        assert!(
            state
                .packages
                .iter()
                .any(|record| record.identity == "npm:chalk")
        );
        assert!(
            state
                .packages
                .iter()
                .any(|record| record.identity == "npm:prettier")
        );
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
    fn project_scope_local_sources_outside_workspace_rehydrate_existing_paths() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        fs::create_dir_all(&cwd).expect("cwd");
        let package_dir = tempdir.path().join("plugin-package");
        fs::create_dir_all(&package_dir).expect("package dir");
        let agent_dir = tempdir.path().join("agent");

        let settings_manager = SettingsManager::create(&cwd, Some(agent_dir.clone()));
        let mut manager =
            PackageManager::with_settings_manager(&cwd, Some(agent_dir.clone()), settings_manager);
        manager
            .install(
                package_dir.to_string_lossy().as_ref(),
                PackageInstallScope::Project,
            )
            .expect("install project package");

        let reloaded = PackageManager::create(&cwd, Some(agent_dir));
        let installed = reloaded
            .list_by_scope(PackageInstallScope::Project)
            .into_iter()
            .find(|package| package.source.contains("plugin-package"))
            .expect("installed project package");

        assert_eq!(installed.install_path, package_dir);
        assert!(installed.install_path.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn relative_path_normalizes_private_prefix_for_missing_children() {
        let tempdir = tempdir().expect("tempdir");
        let workspace = tempdir.path().join("workspace");
        let project_config_dir = workspace.join(".pi");
        let package_dir = workspace.join("pkg");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&package_dir).expect("package dir");

        let private_project_config_dir = Path::new("/private").join(
            project_config_dir
                .strip_prefix("/")
                .expect("absolute project config dir"),
        );
        let relative =
            relative_path(&private_project_config_dir, &package_dir).expect("relative path");
        assert_eq!(relative, PathBuf::from("../pkg"));
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
