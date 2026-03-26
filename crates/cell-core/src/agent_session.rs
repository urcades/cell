use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use cell_ai_core::{
    AiThinkingLevel, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Message,
    Model, StopReason, StreamOptions, ToolResultMessage, Usage, UserContent, UserContentBlock,
    UserMessage,
};
use cell_ai_providers::ProviderRegistry;
use cell_config::{SettingsLoadError, SettingsManager, SettingsScope};
use cell_models::{
    ModelRegistry, ScopedModel, models_are_equal, resolve_model_scope, supports_xhigh,
};
use cell_plugin_host::{ActivePluginRegistry, PluginHostWarning, PluginStartupSummary, RegisteredPluginSummary};
use cell_plugins::{LifecycleEventV1, LifecycleHookContextV1};
use cell_protocol::{
    QueueMode, RpcBashResult, RpcCommandLocation, RpcCommandSource, RpcEvent, RpcForkMessage,
    RpcPluginRuntimeDiagnostics, RpcPluginRuntimePluginSummary, RpcPluginRuntimeWarning,
    RpcSessionState, RpcSessionStats, RpcSlashCommand, RpcTokenStats,
};
use cell_resources::{
    LoadedTextResource, PromptTemplate, SkillDefinition, expand_prompt_template,
};
use cell_session::{
    SessionCustomMessageEntry, SessionEntry, SessionManager, SessionTreeNode, parse_entry_base,
};
use cell_tools::ToolSet;
use serde::Serialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::RuntimeError;
use crate::agent_core::{
    Agent, AgentControl, AgentEvent, THINKING_LEVELS, is_valid_thinking_level,
};
use crate::export_html::export_session_to_html;
use crate::runtime_resources::{
    SessionRuntimeConfig, load_session_runtime_resources_with_settings,
};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelCycleResult {
    pub model: Model,
    pub thinking_level: String,
    pub is_scoped: bool,
}

#[derive(Clone, Debug)]
pub struct PromptRun {
    pub assistant_message: AssistantMessage,
    pub raw_events: Vec<AssistantMessageEvent>,
    pub events: Vec<AgentEvent>,
    pub tool_results: Vec<ToolResultMessage>,
    pub new_messages: Vec<Message>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum StartupResourceNoticeSection {
    Context,
    Skill,
    Prompt,
    Theme,
    Extension,
    Resource,
}

impl StartupResourceNoticeSection {
    pub fn heading(self) -> &'static str {
        match self {
            Self::Context => "Context issues",
            Self::Skill => "Skill conflicts",
            Self::Prompt => "Prompt conflicts",
            Self::Theme => "Theme conflicts",
            Self::Extension => "Extension warnings",
            Self::Resource => "Resource issues",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StartupResourceNotice {
    pub section: StartupResourceNoticeSection,
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StartupResourceSummary {
    pub context_paths: Vec<PathBuf>,
    pub skills: Vec<PathBuf>,
    pub prompts: Vec<PathBuf>,
    pub extensions: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_summaries: Vec<String>,
    pub themes: Vec<PathBuf>,
    pub conflicts: Vec<StartupResourceNotice>,
    pub notices: Vec<StartupResourceNotice>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ForkableUserMessage {
    pub entry_id: String,
    pub parent_id: Option<String>,
    pub timestamp: String,
    pub index: usize,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TreeNavigationResult {
    pub editor_text: Option<String>,
    pub summary_created: bool,
}

#[derive(Clone)]
pub struct AgentSession {
    agent: Agent,
    provider_registry: ProviderRegistry,
    model_registry: ModelRegistry,
    session: SessionManager,
    settings_manager: Option<SettingsManager>,
    runtime_resource_config: Option<SessionRuntimeConfig>,
    runtime_resource_tool_names: Vec<String>,
    tool_set: ToolSet,
    plugin_runtime: Option<Arc<Mutex<ActivePluginRegistry>>>,
    plugin_runtime_summaries: Vec<RpcPluginRuntimePluginSummary>,
    plugin_runtime_warnings: VecDeque<RpcPluginRuntimeWarning>,
    session_started_notified: bool,
    session_ended_notified: bool,
    scoped_models: Vec<ScopedModel>,
    prompt_templates: Vec<PromptTemplate>,
    skills: Vec<SkillDefinition>,
    themes: Vec<LoadedTextResource>,
    startup_resource_summary: StartupResourceSummary,
}

const COMPACTION_SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";
const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";
const PLUGIN_RUNTIME_WARNING_BUFFER_LIMIT: usize = 64;
const BUILTIN_SLASH_COMMANDS: &[&str] = &[
    "settings",
    "model",
    "scoped-models",
    "export",
    "share",
    "copy",
    "name",
    "session",
    "changelog",
    "hotkeys",
    "fork",
    "tree",
    "login",
    "logout",
    "new",
    "compact",
    "resume",
    "reload",
    "quit",
    "exit",
];

impl AgentSession {
    pub fn new(
        provider_registry: ProviderRegistry,
        model_registry: ModelRegistry,
        session: SessionManager,
        tool_set: ToolSet,
        model: Model,
        thinking_level: impl Into<String>,
        system_prompt: Option<String>,
        scoped_models: Vec<ScopedModel>,
        prompt_templates: Vec<PromptTemplate>,
        skills: Vec<SkillDefinition>,
    ) -> Self {
        let mut session_instance = Self {
            agent: Agent::new(model, thinking_level, system_prompt),
            provider_registry,
            model_registry,
            session,
            settings_manager: None,
            runtime_resource_config: None,
            runtime_resource_tool_names: Vec::new(),
            tool_set,
            plugin_runtime: None,
            plugin_runtime_summaries: Vec::new(),
            plugin_runtime_warnings: VecDeque::new(),
            session_started_notified: false,
            session_ended_notified: false,
            scoped_models,
            prompt_templates,
            skills,
            themes: Vec::new(),
            startup_resource_summary: StartupResourceSummary::default(),
        };
        session_instance.restore_from_session_context();
        session_instance
    }

    pub(crate) fn attach_settings_manager(&mut self, settings_manager: SettingsManager) {
        self.settings_manager = Some(settings_manager);
    }

    pub(crate) fn attach_runtime_resources(
        &mut self,
        config: SessionRuntimeConfig,
        enabled_tool_names: Vec<String>,
        themes: Vec<LoadedTextResource>,
        plugin_runtime: Option<Arc<Mutex<ActivePluginRegistry>>>,
        plugin_startup_summary: PluginStartupSummary,
        startup_resource_summary: StartupResourceSummary,
    ) {
        self.runtime_resource_config = Some(config);
        self.runtime_resource_tool_names = enabled_tool_names;
        self.themes = themes;
        self.plugin_runtime = plugin_runtime.clone();
        if let Some(plugin_runtime) = plugin_runtime {
            self.tool_set.attach_plugin_runtime(plugin_runtime);
        }
        self.seed_plugin_runtime_diagnostics(&plugin_startup_summary);
        self.dispatch_lifecycle_hook_event(LifecycleEventV1::HostStartup, None);
        for plugin_summary in self.plugin_runtime_summaries.clone() {
            self.dispatch_lifecycle_hook_event(
                LifecycleEventV1::PluginLoaded,
                Some(format!("plugin_id={}", plugin_summary.plugin_id)),
            );
        }
        self.record_session_start();
        self.startup_resource_summary = startup_resource_summary;
    }

    pub fn model_registry(&self) -> &ModelRegistry {
        &self.model_registry
    }

    pub fn model_registry_mut(&mut self) -> &mut ModelRegistry {
        &mut self.model_registry
    }

    pub fn control(&self) -> AgentControl {
        self.agent.control()
    }

    pub fn session(&self) -> &SessionManager {
        &self.session
    }

    pub fn current_model(&self) -> &Model {
        &self.agent.state().model
    }

    pub fn current_thinking_level(&self) -> &str {
        &self.agent.state().thinking_level
    }

    pub fn get_state(&self) -> RpcSessionState {
        RpcSessionState {
            model: Some(self.agent.state().model.clone()),
            thinking_level: self.agent.state().thinking_level.clone(),
            is_streaming: self.agent.state().is_streaming,
            is_compacting: self.agent.state().is_compacting,
            steering_mode: self.agent.state().steering_mode,
            follow_up_mode: self.agent.state().follow_up_mode,
            session_file: self
                .session
                .get_session_file()
                .map(|path| path.to_string_lossy().to_string()),
            session_id: self.session.get_session_id().to_string(),
            session_name: self.session.get_session_name(),
            auto_compaction_enabled: self.agent.state().auto_compaction_enabled,
            message_count: self.build_context_messages().len(),
            pending_message_count: self.agent.pending_message_count(),
        }
    }

    pub fn get_available_models(&self) -> Vec<Model> {
        self.model_registry.get_available()
    }

    pub fn get_scoped_models(&self) -> Vec<Model> {
        self.scoped_models
            .iter()
            .map(|scoped| scoped.model.clone())
            .collect()
    }

    pub fn get_scoped_model_entries(&self) -> Vec<ScopedModel> {
        self.scoped_models.clone()
    }

    pub fn set_scoped_models(&mut self, scoped_models: Vec<ScopedModel>) {
        self.scoped_models = scoped_models;
    }

    pub fn set_scoped_models_from_patterns(&mut self, patterns: &[String]) -> Vec<ScopedModel> {
        let scoped_models = build_scoped_models(patterns, &self.model_registry);
        self.scoped_models = scoped_models.clone();
        scoped_models
    }

    pub fn current_scoped_model_patterns(&self) -> Vec<String> {
        self.scoped_models
            .iter()
            .map(scoped_model_to_pattern)
            .collect()
    }

    pub fn get_persisted_enabled_model_patterns(&self) -> Option<Vec<String>> {
        self.settings_manager
            .as_ref()
            .and_then(|settings_manager| settings_manager.get_enabled_models(None))
    }

    pub fn save_current_scoped_models(&mut self) -> Result<Vec<String>, RuntimeError> {
        let patterns = self.current_scoped_model_patterns();
        let settings_manager = self.settings_manager_mut()?;
        settings_manager.set_enabled_models(SettingsScope::Global, Some(&patterns))?;
        Ok(patterns)
    }

    pub fn load_persisted_enabled_models(&mut self) -> Result<Vec<ScopedModel>, RuntimeError> {
        let persisted = self
            .settings_manager
            .as_ref()
            .and_then(|settings_manager| settings_manager.get_enabled_models(None));
        let scoped_models = persisted
            .as_deref()
            .map(|patterns| build_scoped_models(patterns, &self.model_registry))
            .unwrap_or_default();
        self.scoped_models = scoped_models.clone();
        Ok(scoped_models)
    }

    pub fn clear_persisted_enabled_models(&mut self) -> Result<(), RuntimeError> {
        let settings_manager = self.settings_manager_mut()?;
        settings_manager.set_enabled_models(SettingsScope::Global, None)?;
        Ok(())
    }

    pub fn drain_settings_errors(&mut self) -> Vec<SettingsLoadError> {
        self.settings_manager
            .as_mut()
            .map(SettingsManager::drain_errors)
            .unwrap_or_default()
    }

    pub fn get_themes(&self) -> Vec<LoadedTextResource> {
        self.themes.clone()
    }

    pub fn startup_resource_summary(&self) -> &StartupResourceSummary {
        &self.startup_resource_summary
    }

    pub fn startup_context_paths(&self) -> &[PathBuf] {
        &self.startup_resource_summary.context_paths
    }

    pub fn startup_resource_notices(&self) -> &[StartupResourceNotice] {
        &self.startup_resource_summary.notices
    }

    pub fn get_plugin_runtime_diagnostics(&self) -> RpcPluginRuntimeDiagnostics {
        RpcPluginRuntimeDiagnostics {
            plugins: self.plugin_runtime_summaries.clone(),
            warnings: self.plugin_runtime_warnings.iter().cloned().collect(),
        }
    }

    pub fn reload_runtime_resources(&mut self) -> Result<(), RuntimeError> {
        let config = self.runtime_resource_config.clone().ok_or_else(|| {
            RuntimeError::Message("Runtime resources are not configured.".to_string())
        })?;
        let enabled_tool_names = self.runtime_resource_tool_names.clone();
        let settings_manager = self.settings_manager_mut()?;
        settings_manager.reload();
        let resources = load_session_runtime_resources_with_settings(
            &config,
            settings_manager.clone(),
            &enabled_tool_names,
        );
        self.agent.set_system_prompt(Some(resources.system_prompt));
        self.prompt_templates = resources.prompt_templates;
        self.skills = resources.skills;
        self.themes = resources.themes;
        self.plugin_runtime = resources.plugin_runtime.clone();
        if let Some(plugin_runtime) = resources.plugin_runtime {
            self.tool_set.attach_plugin_runtime(plugin_runtime);
        }
        self.seed_plugin_runtime_diagnostics(&resources.plugin_startup_summary);
        for plugin_summary in self.plugin_runtime_summaries.clone() {
            self.dispatch_lifecycle_hook_event(
                LifecycleEventV1::PluginLoaded,
                Some(format!("plugin_id={}", plugin_summary.plugin_id)),
            );
        }
        self.startup_resource_summary = resources.startup_summary;
        Ok(())
    }

    pub fn set_model(&mut self, provider: &str, model_id: &str) -> Result<Model, RuntimeError> {
        let model = self
            .model_registry
            .find(provider, model_id)
            .ok_or_else(|| {
                RuntimeError::Message(format!("Model not found: {provider}/{model_id}"))
            })?;
        self.agent.set_model(model.clone());
        self.session.append_model_change(provider, model_id)?;
        self.persist_default_model(&model)?;
        Ok(model)
    }

    pub fn find_model_for_selection(&self, query: &str) -> Option<Model> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }

        let candidates = self.selection_model_candidates();
        if let Some((provider, model_id)) = query.split_once('/') {
            return candidates
                .into_iter()
                .find(|model| model.provider.0 == provider && model.id == model_id);
        }

        let mut matches = candidates
            .into_iter()
            .filter(|model| model.id == query)
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            Some(matches.remove(0))
        } else {
            None
        }
    }

    pub fn cycle_model(&mut self) -> Result<Option<ModelCycleResult>, RuntimeError> {
        self.cycle_model_with_direction(1)
    }

    pub fn cycle_model_backward(&mut self) -> Result<Option<ModelCycleResult>, RuntimeError> {
        self.cycle_model_with_direction(-1)
    }

    fn cycle_model_with_direction(
        &mut self,
        direction: isize,
    ) -> Result<Option<ModelCycleResult>, RuntimeError> {
        let scoped = !self.scoped_models.is_empty();
        let mut models = if scoped {
            self.scoped_models.clone()
        } else {
            self.model_registry
                .get_available()
                .into_iter()
                .map(|model| ScopedModel {
                    model,
                    thinking_level: None,
                })
                .collect::<Vec<_>>()
        };
        if models.is_empty() {
            return Ok(None);
        }

        if !scoped {
            models.sort_by(|left, right| {
                format!("{}/{}", left.model.provider.0, left.model.id)
                    .cmp(&format!("{}/{}", right.model.provider.0, right.model.id))
            });
        }

        let current_index = models
            .iter()
            .position(|candidate| {
                models_are_equal(Some(&candidate.model), Some(self.current_model()))
            })
            .unwrap_or_else(|| {
                if direction.is_negative() {
                    0
                } else {
                    models.len().saturating_sub(1)
                }
            });
        let next_index =
            (current_index as isize + direction).rem_euclid(models.len() as isize) as usize;
        let next = models[next_index].clone();
        let next_thinking = next
            .thinking_level
            .clone()
            .unwrap_or_else(|| self.agent.state().thinking_level.clone());

        self.agent.set_model(next.model.clone());
        self.session
            .append_model_change(&next.model.provider.0, &next.model.id)?;
        self.persist_default_model(&next.model)?;
        if next_thinking != self.agent.state().thinking_level {
            self.agent.set_thinking_level(next_thinking.clone());
            self.session.append_thinking_level_change(&next_thinking)?;
        }

        Ok(Some(ModelCycleResult {
            model: next.model,
            thinking_level: next_thinking,
            is_scoped: scoped,
        }))
    }

    pub fn set_thinking_level(&mut self, level: &str) -> Result<(), RuntimeError> {
        if !is_valid_thinking_level(level) {
            return Err(RuntimeError::Message(format!(
                "Invalid thinking level: {level}"
            )));
        }
        if level == "xhigh" && !supports_xhigh(self.current_model()) {
            return Err(RuntimeError::Message(format!(
                "Model {} does not support xhigh thinking.",
                self.current_model().id
            )));
        }
        self.agent.set_thinking_level(level.to_string());
        self.session.append_thinking_level_change(level)?;
        Ok(())
    }

    pub fn cycle_thinking_level(&mut self) -> Result<Option<String>, RuntimeError> {
        let levels = if supports_xhigh(self.current_model()) {
            THINKING_LEVELS.to_vec()
        } else {
            THINKING_LEVELS[..THINKING_LEVELS.len() - 1].to_vec()
        };
        let current_index = levels
            .iter()
            .position(|level| *level == self.current_thinking_level())
            .unwrap_or(0);
        let next = levels[(current_index + 1) % levels.len()].to_string();
        self.set_thinking_level(&next)?;
        Ok(Some(next))
    }

    pub fn set_steering_mode(&mut self, mode: QueueMode) {
        self.agent.set_steering_mode(mode);
    }

    pub fn set_follow_up_mode(&mut self, mode: QueueMode) {
        self.agent.set_follow_up_mode(mode);
    }

    pub fn set_auto_compaction(&mut self, enabled: bool) {
        self.agent.set_auto_compaction_enabled(enabled);
    }

    pub fn set_auto_retry(&mut self, enabled: bool) {
        self.agent.set_auto_retry_enabled(enabled);
    }

    pub fn steer_text(&mut self, message: String) {
        self.agent.steer(Message::User(UserMessage {
            content: UserContent::Text(message),
            timestamp: 0,
        }));
    }

    pub fn follow_up_text(&mut self, message: String) {
        self.agent.follow_up(Message::User(UserMessage {
            content: UserContent::Text(message),
            timestamp: 0,
        }));
    }

    pub fn abort_retry(&mut self) -> Result<(), RuntimeError> {
        self.abort();
        Ok(())
    }

    pub fn abort_bash(&mut self) -> Result<(), RuntimeError> {
        self.abort();
        Ok(())
    }

    pub fn abort(&mut self) {
        self.agent.abort();
        self.agent.state_mut().error = Some("aborted".to_string());
    }

    pub fn new_session(&mut self, parent_session: Option<&str>) -> Result<bool, RuntimeError> {
        let new_session = match parent_session {
            Some(path) if !path.trim().is_empty() => SessionManager::fork_from(
                path,
                self.session.get_cwd(),
                Some(self.session.get_session_dir().to_path_buf()),
            )?,
            _ => SessionManager::create(
                self.session.get_cwd(),
                Some(self.session.get_session_dir().to_path_buf()),
            )?,
        };
        self.transition_session(new_session);
        Ok(false)
    }

    pub fn switch_session(&mut self, session_path: &str) -> Result<bool, RuntimeError> {
        self.transition_session(SessionManager::open(session_path)?);
        Ok(false)
    }

    pub fn fork(&mut self, entry_id: &str) -> Result<(String, bool), RuntimeError> {
        let selected_text = self
            .session
            .get_entry(entry_id)
            .and_then(extract_entry_text)
            .ok_or_else(|| RuntimeError::Message("Invalid entry ID for fork.".to_string()))?;
        self.record_session_end();
        self.session.create_branched_session(entry_id)?;
        self.restore_from_session_context();
        self.record_session_start();
        Ok((selected_text, false))
    }

    fn transition_session(&mut self, new_session: SessionManager) {
        self.record_session_end();
        self.session = new_session;
        self.restore_from_session_context();
        self.record_session_start();
    }

    fn record_session_start(&mut self) {
        if self.session_started_notified && !self.session_ended_notified {
            return;
        }
        self.dispatch_lifecycle_hook_event(
            LifecycleEventV1::SessionStarted,
            Some(format!("session_id={}", self.session.get_session_id())),
        );
        self.session_started_notified = true;
        self.session_ended_notified = false;
    }

    fn record_session_end(&mut self) {
        if !self.session_started_notified || self.session_ended_notified {
            return;
        }
        self.dispatch_lifecycle_hook_event(
            LifecycleEventV1::SessionEnded,
            Some(format!("session_id={}", self.session.get_session_id())),
        );
        self.session_ended_notified = true;
    }

    fn seed_plugin_runtime_diagnostics(&mut self, startup_summary: &PluginStartupSummary) {
        self.plugin_runtime_summaries = startup_summary
            .summaries
            .iter()
            .map(plugin_runtime_summary_from_registered)
            .collect();
        self.plugin_runtime_warnings.clear();
        for warning in &startup_summary.warnings {
            self.push_plugin_runtime_warning(plugin_runtime_warning_from_host(warning));
        }
    }

    fn dispatch_lifecycle_hook_event(
        &mut self,
        event: LifecycleEventV1,
        details: Option<String>,
    ) {
        let Some(plugin_runtime) = self.plugin_runtime.as_ref().map(Arc::clone) else {
            return;
        };
        let context = build_lifecycle_hook_context(
            &event,
            details.as_deref(),
            self.session.get_cwd(),
            Some(self.session.get_session_id()),
            Some(&self.current_model().provider.0),
            Some(&self.current_model().id),
        );
        let warnings = match plugin_runtime.lock() {
            Ok(mut plugin_runtime) => plugin_runtime
                .dispatch_hooks(context)
                .warnings
                .into_iter()
                .map(|warning| {
                    plugin_runtime_warning_from_host_event(&warning, &event, details.as_deref())
                })
                .collect(),
            Err(_) => vec![RpcPluginRuntimeWarning {
                path: None,
                plugin_id: None,
                plugin_name: None,
                event: Some(lifecycle_event_name(&event).to_string()),
                details,
                message: format!(
                    "Failed to lock plugin runtime while dispatching `{}` hooks.",
                    lifecycle_event_name(&event)
                ),
            }],
        };
        for warning in warnings {
            self.push_plugin_runtime_warning(warning);
        }
    }

    fn push_plugin_runtime_warning(&mut self, warning: RpcPluginRuntimeWarning) {
        while self.plugin_runtime_warnings.len() >= PLUGIN_RUNTIME_WARNING_BUFFER_LIMIT {
            self.plugin_runtime_warnings.pop_front();
        }
        self.plugin_runtime_warnings.push_back(warning);
    }

    pub fn forkable_user_messages(&self) -> Vec<ForkableUserMessage> {
        let mut fork_index = 0usize;
        self.session
            .get_entries()
            .iter()
            .filter_map(|entry| {
                let Ok(SessionEntry::Message(message_entry)) =
                    serde_json::from_value::<SessionEntry>(entry.clone())
                else {
                    return None;
                };
                let Message::User(user_message) = message_entry.message else {
                    return None;
                };
                let base = parse_entry_base(entry).ok().flatten()?;
                Some(ForkableUserMessage {
                    entry_id: base.id,
                    parent_id: base.parent_id,
                    timestamp: base.timestamp,
                    index: {
                        let current = fork_index;
                        fork_index += 1;
                        current
                    },
                    text: extract_message_text(&Message::User(user_message)),
                })
            })
            .collect()
    }

    pub fn get_fork_messages(&self) -> Vec<RpcForkMessage> {
        self.forkable_user_messages()
            .into_iter()
            .map(|message| RpcForkMessage {
                entry_id: message.entry_id,
                text: message.text,
            })
            .collect()
    }

    pub fn get_tree(&self) -> Vec<SessionTreeNode> {
        self.session.get_tree()
    }

    pub fn get_leaf_id(&self) -> Option<String> {
        self.session.get_leaf_id().map(ToOwned::to_owned)
    }

    pub fn branch_to(&mut self, entry_id: &str) -> Result<(), RuntimeError> {
        self.session.branch(entry_id)?;
        self.restore_from_session_context();
        Ok(())
    }

    pub fn navigate_tree(
        &mut self,
        target_id: &str,
        summarize: bool,
        custom_instructions: Option<&str>,
    ) -> Result<TreeNavigationResult, RuntimeError> {
        let old_leaf_id = self.session.get_leaf_id().map(ToOwned::to_owned);
        if old_leaf_id.as_deref() == Some(target_id) {
            return Ok(TreeNavigationResult {
                editor_text: None,
                summary_created: false,
            });
        }

        let target_value = self
            .session
            .get_entry(target_id)
            .cloned()
            .ok_or_else(|| RuntimeError::Message(format!("Entry {target_id} not found")))?;
        let target_entry =
            serde_json::from_value::<SessionEntry>(target_value.clone()).map_err(|error| {
                RuntimeError::Message(format!("Failed to parse tree target: {error}"))
            })?;
        let target_parent_id = parse_entry_base(&target_value)
            .ok()
            .flatten()
            .and_then(|base| base.parent_id);

        let summary_text = if summarize {
            let entries = collect_entries_for_branch_summary(
                &self.session,
                old_leaf_id.as_deref(),
                target_id,
            );
            build_branch_summary_text(&entries, custom_instructions)
        } else {
            None
        };

        let (new_leaf_id, editor_text) = match &target_entry {
            SessionEntry::Message(message_entry) => match &message_entry.message {
                Message::User(_) => (
                    target_parent_id,
                    Some(extract_message_text(&message_entry.message)),
                ),
                _ => (Some(target_id.to_string()), None),
            },
            SessionEntry::CustomMessage(_) => (target_parent_id, extract_entry_text(&target_value)),
            _ => (Some(target_id.to_string()), None),
        };

        if let Some(summary) = summary_text.as_ref() {
            self.session.branch_with_summary(
                new_leaf_id.as_deref(),
                summary.clone(),
                None,
                None,
            )?;
        } else if let Some(new_leaf_id) = new_leaf_id.as_deref() {
            self.session.branch(new_leaf_id)?;
        } else {
            self.session.reset_leaf();
        }

        self.restore_from_session_context();
        Ok(TreeNavigationResult {
            editor_text,
            summary_created: summary_text.is_some(),
        })
    }

    pub fn set_entry_label(
        &mut self,
        entry_id: &str,
        label: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.session.append_label_change(entry_id, label)?;
        Ok(())
    }

    pub fn get_last_assistant_text(&self) -> Option<String> {
        self.build_context_messages()
            .into_iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(assistant) => Some(
                    assistant
                        .content
                        .iter()
                        .filter_map(|block| match block {
                            AssistantContentBlock::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
    }

    pub fn set_session_name(&mut self, name: &str) -> Result<(), RuntimeError> {
        if name.trim().is_empty() {
            return Err(RuntimeError::Message(
                "Session name cannot be empty".to_string(),
            ));
        }
        self.session.append_session_info(name)?;
        Ok(())
    }

    pub fn get_messages(&self) -> Vec<Message> {
        self.build_context_messages()
    }

    pub fn get_commands(&self) -> Vec<RpcSlashCommand> {
        let mut commands = Vec::new();
        let mut seen = BUILTIN_SLASH_COMMANDS
            .iter()
            .map(|name| (*name).to_string())
            .collect::<HashSet<_>>();
        for template in &self.prompt_templates {
            if seen.insert(template.name.clone()) {
                commands.push(RpcSlashCommand {
                    name: template.name.clone(),
                    description: Some(template.description.clone()),
                    source: RpcCommandSource::Prompt,
                    location: Some(location_for_scope(template.scope)),
                    path: Some(template.path.to_string_lossy().to_string()),
                });
            }
        }
        let skill_commands_enabled = self
            .settings_manager
            .as_ref()
            .is_none_or(SettingsManager::get_enable_skill_commands);
        if skill_commands_enabled {
            for skill in &self.skills {
                let name = format!("skill:{}", skill.name);
                if seen.insert(name.clone()) {
                    commands.push(RpcSlashCommand {
                        name,
                        description: Some(skill.description.clone()),
                        source: RpcCommandSource::Skill,
                        location: Some(location_for_scope(skill.scope)),
                        path: Some(skill.path.to_string_lossy().to_string()),
                    });
                }
            }
        }
        if let Some(plugin_runtime) = &self.plugin_runtime {
            if let Ok(plugin_runtime) = plugin_runtime.lock() {
                for command in plugin_runtime.merged_registry().commands.values() {
                    let name = command.registration.name.clone();
                    if command.registration.hidden || !seen.insert(name.clone()) {
                        continue;
                    }
                    commands.push(RpcSlashCommand {
                        name,
                        description: command.registration.description.clone(),
                        source: RpcCommandSource::Extension,
                        location: Some(RpcCommandLocation::Path),
                        path: Some(command.source.descriptor_path.to_string_lossy().to_string()),
                    });
                }
            }
        }
        commands
    }

    pub fn get_session_stats(&self) -> RpcSessionStats {
        let mut user_messages = 0usize;
        let mut assistant_messages = 0usize;
        let mut tool_calls = 0usize;
        let mut tool_results = 0usize;
        let mut usage = Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 0,
            cost: cell_ai_core::UsageCost {
                input: "0".to_string(),
                output: "0".to_string(),
                cache_read: "0".to_string(),
                cache_write: "0".to_string(),
                total: "0".to_string(),
            },
        };
        let mut cost = 0.0;

        for entry in self.session.get_entries() {
            let Ok(session_entry) = serde_json::from_value::<SessionEntry>(entry.clone()) else {
                continue;
            };
            match session_entry {
                SessionEntry::Message(message_entry) => match message_entry.message {
                    Message::User(_) => user_messages += 1,
                    Message::Assistant(assistant) => {
                        assistant_messages += 1;
                        tool_calls += assistant
                            .content
                            .iter()
                            .filter(|block| matches!(block, AssistantContentBlock::ToolCall { .. }))
                            .count();
                        usage.input += assistant.usage.input;
                        usage.output += assistant.usage.output;
                        usage.cache_read += assistant.usage.cache_read;
                        usage.cache_write += assistant.usage.cache_write;
                        usage.total_tokens += assistant.usage.total_tokens;
                        cost += assistant.usage.cost.total.parse::<f64>().unwrap_or(0.0);
                    }
                    Message::ToolResult(_) => tool_results += 1,
                },
                SessionEntry::CustomMessage(_) => {}
                _ => {}
            }
        }

        RpcSessionStats {
            session_file: self
                .session
                .get_session_file()
                .map(|path| path.to_string_lossy().to_string()),
            session_id: self.session.get_session_id().to_string(),
            user_messages,
            assistant_messages,
            tool_calls,
            tool_results,
            total_messages: user_messages + assistant_messages + tool_results,
            tokens: RpcTokenStats {
                input: usage.input,
                output: usage.output,
                cache_read: usage.cache_read,
                cache_write: usage.cache_write,
                total: usage.total_tokens,
            },
            cost,
        }
    }

    pub fn export_html(&self, output_path: Option<&Path>) -> Result<PathBuf, RuntimeError> {
        Ok(export_session_to_html(
            &self.session,
            output_path,
            self.agent.state().system_prompt.as_deref(),
        )?)
    }

    pub fn bash(&mut self, command: &str) -> Result<RpcBashResult, RuntimeError> {
        self.manual_bash(command, false)
    }

    pub fn manual_bash(
        &mut self,
        command: &str,
        exclude_from_context: bool,
    ) -> Result<RpcBashResult, RuntimeError> {
        let result = self
            .tool_set
            .execute_bash_direct(command)
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        self.session.append_custom_message_entry(
            "bash_execution",
            UserContent::Blocks(vec![UserContentBlock::Text {
                text: format_bash_execution_text(command, &result),
                text_signature: None,
            }]),
            true,
            Some(json!({
                "command": command,
                "prefix": if exclude_from_context { "!!" } else { "!" },
                "excludeFromContext": exclude_from_context,
                "output": result.output,
                "exitCode": result.exit_code,
                "cancelled": result.cancelled,
                "truncated": result.truncated,
                "fullOutputPath": result.full_output_path,
            })),
        )?;
        self.refresh_messages_from_session();
        Ok(RpcBashResult {
            output: result.output,
            exit_code: result.exit_code,
            cancelled: result.cancelled,
            truncated: result.truncated,
            full_output_path: result.full_output_path,
        })
    }

    pub fn compact(&mut self, custom_instructions: Option<&str>) -> Result<Value, RuntimeError> {
        let branch = self.session.get_branch(None);
        if branch.is_empty() {
            return Err(RuntimeError::Message(
                "Cannot compact an empty session.".to_string(),
            ));
        }

        let first_kept_entry_id = parse_entry_base(branch.last().expect("last branch entry"))
            .ok()
            .flatten()
            .map(|base| base.id)
            .ok_or_else(|| {
                RuntimeError::Message("Cannot determine compaction target.".to_string())
            })?;
        let summary_lines = branch
            .iter()
            .take(branch.len().saturating_sub(1))
            .filter_map(extract_context_entry_text)
            .collect::<Vec<_>>();
        let mut summary = summary_lines.join("\n");
        if let Some(custom_instructions) =
            custom_instructions.filter(|value| !value.trim().is_empty())
        {
            summary = format!("{custom_instructions}\n\n{summary}");
        }
        if summary.trim().is_empty() {
            summary = "Compacted prior session context.".to_string();
        }

        let tokens_before = self.get_session_stats().tokens.total;
        let entry_id = self.session.append_compaction(
            summary.clone(),
            &first_kept_entry_id,
            tokens_before,
            None,
            None,
        )?;
        Ok(json!({
            "entryId": entry_id,
            "summary": summary,
            "firstKeptEntryId": first_kept_entry_id,
            "tokensBefore": tokens_before,
        }))
    }

    pub async fn prompt_text(&mut self, prompt: String) -> Result<PromptRun, RuntimeError> {
        self.prepare_prompt();
        self.prompt_text_prepared(prompt).await
    }

    pub fn prepare_prompt(&mut self) {
        self.agent.reset_abort();
    }

    pub async fn prompt_text_prepared(
        &mut self,
        prompt: String,
    ) -> Result<PromptRun, RuntimeError> {
        let prompt = expand_prompt_template(&prompt, &self.prompt_templates);
        let prompt = self.resolve_runtime_command_prompt(prompt)?;
        self.prompt_message_prepared(Message::User(UserMessage {
            content: UserContent::Text(prompt),
            timestamp: 0,
        }))
        .await
    }

    pub async fn prompt_text_prepared_with_events(
        &mut self,
        prompt: String,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Result<PromptRun, RuntimeError> {
        let prompt = expand_prompt_template(&prompt, &self.prompt_templates);
        let prompt = self.resolve_runtime_command_prompt(prompt)?;
        self.prompt_message_prepared_internal(
            Message::User(UserMessage {
                content: UserContent::Text(prompt),
                timestamp: 0,
            }),
            Some(event_tx),
        )
        .await
    }

    pub async fn prompt_text_as_blocks(
        &mut self,
        prompt: String,
    ) -> Result<PromptRun, RuntimeError> {
        let prompt = expand_prompt_template(&prompt, &self.prompt_templates);
        let prompt = self.resolve_runtime_command_prompt(prompt)?;
        self.prompt_message_prepared(Message::User(UserMessage {
            content: UserContent::Blocks(vec![UserContentBlock::Text {
                text: prompt,
                text_signature: None,
            }]),
            timestamp: 0,
        }))
        .await
    }

    fn resolve_runtime_command_prompt(&mut self, prompt: String) -> Result<String, RuntimeError> {
        let Some(plugin_runtime) = self.plugin_runtime.as_ref().map(Arc::clone) else {
            return Ok(prompt);
        };
        let Some(command_text) = prompt.strip_prefix('/') else {
            return Ok(prompt);
        };
        let (command_name, args_text) = if let Some(index) = command_text.find(' ') {
            (&command_text[..index], command_text[index + 1..].trim())
        } else {
            (command_text, "")
        };
        let plugin_runtime_guard = plugin_runtime
            .lock()
            .map_err(|_| RuntimeError::Message("Failed to lock plugin runtime.".to_string()))?;
        if !plugin_runtime_guard
            .merged_registry()
            .commands
            .contains_key(command_name)
        {
            return Ok(prompt);
        }
        let args = parse_runtime_command_args(args_text);
        drop(plugin_runtime_guard);
        self.dispatch_lifecycle_hook_event(
            LifecycleEventV1::CommandStarted,
            Some(format!("command_name={command_name}")),
        );
        let result = plugin_runtime
            .lock()
            .map_err(|_| RuntimeError::Message("Failed to lock plugin runtime.".to_string()))?
            .invoke_command(
                command_name,
                &args,
                self.tool_set.cwd(),
                Some(&self.session.get_session_id().to_string()),
                Some(&prompt),
            );
        self.dispatch_lifecycle_hook_event(
            LifecycleEventV1::CommandFinished,
            Some(format!("command_name={command_name}")),
        );
        result.map_err(|error| RuntimeError::Message(error.to_string()))
    }

    pub async fn prompt_message(&mut self, message: Message) -> Result<PromptRun, RuntimeError> {
        self.prepare_prompt();
        self.prompt_message_prepared(message).await
    }

    pub async fn prompt_message_prepared(
        &mut self,
        message: Message,
    ) -> Result<PromptRun, RuntimeError> {
        self.prompt_message_prepared_internal(message, None).await
    }

    async fn prompt_message_prepared_internal(
        &mut self,
        message: Message,
        event_tx: Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<PromptRun, RuntimeError> {
        self.dispatch_lifecycle_hook_event(
            LifecycleEventV1::PromptStarted,
            Some(format!("session_id={}", self.session.get_session_id())),
        );
        let mut prompt_run = PromptRun {
            assistant_message: empty_assistant_message(self.current_model()),
            raw_events: Vec::new(),
            events: vec![AgentEvent::AgentStart],
            tool_results: Vec::new(),
            new_messages: Vec::new(),
        };
        emit_event(&event_tx, &AgentEvent::AgentStart);
        let mut pending_messages = vec![message];
        let mut last_assistant_message = None;

        loop {
            let mut has_more_tool_calls = true;
            while has_more_tool_calls || !pending_messages.is_empty() {
                push_prompt_event(&mut prompt_run, &event_tx, AgentEvent::TurnStart);

                if !pending_messages.is_empty() {
                    let incoming = std::mem::take(&mut pending_messages);
                    for message in incoming {
                        self.session.append_message(message.clone())?;
                        prompt_run.new_messages.push(message.clone());
                        push_prompt_event(
                            &mut prompt_run,
                            &event_tx,
                            AgentEvent::MessageStart {
                                message: message.clone(),
                            },
                        );
                        push_prompt_event(
                            &mut prompt_run,
                            &event_tx,
                            AgentEvent::MessageEnd { message },
                        );
                    }
                    self.refresh_messages_from_session();
                }

                let assistant_message = self.stream_assistant(&mut prompt_run, &event_tx).await?;
                last_assistant_message = Some(assistant_message.clone());

                if matches!(
                    assistant_message.stop_reason,
                    StopReason::Error | StopReason::Aborted
                ) {
                    push_prompt_event(
                        &mut prompt_run,
                        &event_tx,
                        AgentEvent::TurnEnd {
                            message: Message::Assistant(assistant_message.clone()),
                            tool_results: Vec::new(),
                        },
                    );
                    let messages = prompt_run.new_messages.clone();
                    push_prompt_event(
                        &mut prompt_run,
                        &event_tx,
                        AgentEvent::AgentEnd { messages },
                    );
                    self.dispatch_lifecycle_hook_event(
                        LifecycleEventV1::PromptFinished,
                        Some(format!(
                            "session_id={}; stop_reason={:?}",
                            self.session.get_session_id(),
                            assistant_message.stop_reason
                        )),
                    );
                    prompt_run.assistant_message = assistant_message;
                    return Ok(prompt_run);
                }

                let tool_calls = assistant_message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } => Some((id.clone(), name.clone(), arguments.clone())),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                has_more_tool_calls = !tool_calls.is_empty();
                let mut turn_tool_results = Vec::new();

                if has_more_tool_calls {
                    for (index, (tool_call_id, tool_name, arguments)) in
                        tool_calls.iter().enumerate()
                    {
                        push_prompt_event(
                            &mut prompt_run,
                            &event_tx,
                            AgentEvent::ToolExecutionStart {
                                tool_call_id: tool_call_id.clone(),
                                tool_name: tool_name.clone(),
                                args: arguments.clone(),
                            },
                        );
                        if self.agent.is_abort_requested() {
                            has_more_tool_calls = false;
                            break;
                        }
                        self.dispatch_lifecycle_hook_event(
                            LifecycleEventV1::ToolStarted,
                            Some(format!("tool_call_id={tool_call_id}; tool_name={tool_name}")),
                        );
                        let tool_result =
                            self.tool_set
                                .execute(tool_call_id, tool_name, arguments.clone());
                        let tool_result_message = Message::ToolResult(tool_result.clone());
                        self.session.append_message(tool_result_message.clone())?;
                        prompt_run.tool_results.push(tool_result.clone());
                        prompt_run.new_messages.push(tool_result_message.clone());
                        turn_tool_results.push(tool_result_message.clone());
                        push_prompt_event(
                            &mut prompt_run,
                            &event_tx,
                            AgentEvent::ToolExecutionEnd {
                                tool_call_id: tool_call_id.clone(),
                                tool_name: tool_name.clone(),
                                result: tool_result_message.clone(),
                                is_error: tool_result.is_error,
                            },
                        );
                        push_prompt_event(
                            &mut prompt_run,
                            &event_tx,
                            AgentEvent::MessageStart {
                                message: tool_result_message.clone(),
                            },
                        );
                        push_prompt_event(
                            &mut prompt_run,
                            &event_tx,
                            AgentEvent::MessageEnd {
                                message: tool_result_message,
                            },
                        );
                        self.dispatch_lifecycle_hook_event(
                            LifecycleEventV1::ToolFinished,
                            Some(format!(
                                "tool_call_id={tool_call_id}; tool_name={tool_name}; is_error={}",
                                tool_result.is_error
                            )),
                        );
                        self.refresh_messages_from_session();

                        let steering_messages = self.agent.take_steering_messages();
                        if !steering_messages.is_empty() {
                            for skipped in tool_calls.iter().skip(index + 1) {
                                let skipped_result = skipped_tool_result(&skipped.0, &skipped.1);
                                let skipped_message = Message::ToolResult(skipped_result.clone());
                                self.session.append_message(skipped_message.clone())?;
                                prompt_run.tool_results.push(skipped_result.clone());
                                prompt_run.new_messages.push(skipped_message.clone());
                                turn_tool_results.push(skipped_message.clone());
                                push_prompt_event(
                                    &mut prompt_run,
                                    &event_tx,
                                    AgentEvent::ToolExecutionStart {
                                        tool_call_id: skipped.0.clone(),
                                        tool_name: skipped.1.clone(),
                                        args: skipped.2.clone(),
                                    },
                                );
                                push_prompt_event(
                                    &mut prompt_run,
                                    &event_tx,
                                    AgentEvent::ToolExecutionEnd {
                                        tool_call_id: skipped.0.clone(),
                                        tool_name: skipped.1.clone(),
                                        result: skipped_message.clone(),
                                        is_error: true,
                                    },
                                );
                                push_prompt_event(
                                    &mut prompt_run,
                                    &event_tx,
                                    AgentEvent::MessageStart {
                                        message: skipped_message.clone(),
                                    },
                                );
                                push_prompt_event(
                                    &mut prompt_run,
                                    &event_tx,
                                    AgentEvent::MessageEnd {
                                        message: skipped_message,
                                    },
                                );
                            }
                            pending_messages = steering_messages;
                            has_more_tool_calls = false;
                            break;
                        }
                    }
                }

                push_prompt_event(
                    &mut prompt_run,
                    &event_tx,
                    AgentEvent::TurnEnd {
                        message: Message::Assistant(assistant_message),
                        tool_results: turn_tool_results,
                    },
                );

                if !pending_messages.is_empty() {
                    continue;
                }
                if !has_more_tool_calls {
                    let steering_messages = self.agent.take_steering_messages();
                    if !steering_messages.is_empty() {
                        pending_messages = steering_messages;
                        continue;
                    }
                    let follow_up_messages = self.agent.take_follow_up_messages();
                    if !follow_up_messages.is_empty() {
                        pending_messages = follow_up_messages;
                    }
                }
            }

            if pending_messages.is_empty() {
                break;
            }
        }

        let assistant_message = last_assistant_message
            .ok_or_else(|| RuntimeError::Message("No assistant response generated.".to_string()))?;
        let messages = prompt_run.new_messages.clone();
        push_prompt_event(
            &mut prompt_run,
            &event_tx,
            AgentEvent::AgentEnd { messages },
        );
        self.dispatch_lifecycle_hook_event(
            LifecycleEventV1::PromptFinished,
            Some(format!(
                "session_id={}; stop_reason={:?}",
                self.session.get_session_id(),
                assistant_message.stop_reason
            )),
        );
        prompt_run.assistant_message = assistant_message;
        Ok(prompt_run)
    }

    async fn stream_assistant(
        &mut self,
        prompt_run: &mut PromptRun,
        event_tx: &Option<mpsc::Sender<AgentEvent>>,
    ) -> Result<AssistantMessage, RuntimeError> {
        let stream_options = StreamOptions {
            api_key: self.model_registry.get_api_key(self.current_model()),
            reasoning: map_reasoning_level(self.current_thinking_level()),
            session_id: Some(self.session.get_session_id().to_string()),
            ..StreamOptions::default()
        };
        self.agent.set_streaming(true);
        let context_messages = self.build_context_messages();
        let mut stream = self.provider_registry.stream(
            self.current_model(),
            &cell_ai_core::Context {
                system_prompt: self.agent.state().system_prompt.clone(),
                messages: context_messages,
                tools: if self.tool_set.definitions().is_empty() {
                    None
                } else {
                    Some(self.tool_set.definitions())
                },
            },
            Some(stream_options),
        )?;

        let mut partial_message = None;
        let mut added_partial = false;
        let control = self.agent.control();
        let abort_wait = control.wait_for_abort();
        tokio::pin!(abort_wait);
        loop {
            tokio::select! {
                _ = &mut abort_wait => {
                    let final_message = aborted_assistant_message(self.current_model());
                    self.session.append_message(Message::Assistant(final_message.clone()))?;
                    prompt_run.new_messages.push(Message::Assistant(final_message.clone()));
                    if !added_partial {
                        push_prompt_event(
                            prompt_run,
                            event_tx,
                            AgentEvent::MessageStart {
                                message: Message::Assistant(final_message.clone()),
                            },
                        );
                    }
                    push_prompt_event(
                        prompt_run,
                        event_tx,
                        AgentEvent::MessageEnd {
                            message: Message::Assistant(final_message.clone()),
                        },
                    );
                    self.refresh_messages_from_session();
                    self.agent.set_streaming(false);
                    return Ok(final_message);
                }
                maybe_event = stream.next() => {
                    let Some(event) = maybe_event else {
                        break;
                    };
                    prompt_run.raw_events.push(event.clone());
                    match &event {
                        AssistantMessageEvent::Start { partial } => {
                            let message = Message::Assistant(partial.clone());
                            partial_message = Some(partial.clone());
                            added_partial = true;
                            push_prompt_event(prompt_run, event_tx, AgentEvent::MessageStart { message });
                        }
                        AssistantMessageEvent::TextStart { partial, .. }
                        | AssistantMessageEvent::TextDelta { partial, .. }
                        | AssistantMessageEvent::TextEnd { partial, .. }
                        | AssistantMessageEvent::ThinkingStart { partial, .. }
                        | AssistantMessageEvent::ThinkingDelta { partial, .. }
                        | AssistantMessageEvent::ThinkingEnd { partial, .. }
                        | AssistantMessageEvent::ToolcallStart { partial, .. }
                        | AssistantMessageEvent::ToolcallDelta { partial, .. }
                        | AssistantMessageEvent::ToolcallEnd { partial, .. } => {
                            partial_message = Some(partial.clone());
                            push_prompt_event(
                                prompt_run,
                                event_tx,
                                AgentEvent::MessageUpdate {
                                    message: Message::Assistant(partial.clone()),
                                    assistant_message_event: event.clone(),
                                },
                            );
                        }
                        AssistantMessageEvent::Done { message, .. } | AssistantMessageEvent::Error { error: message, .. } => {
                            let final_message = message.clone();
                            self.session.append_message(Message::Assistant(final_message.clone()))?;
                            prompt_run.new_messages.push(Message::Assistant(final_message.clone()));
                            if !added_partial {
                                push_prompt_event(
                                    prompt_run,
                                    event_tx,
                                    AgentEvent::MessageStart {
                                        message: Message::Assistant(final_message.clone()),
                                    },
                                );
                            }
                            push_prompt_event(
                                prompt_run,
                                event_tx,
                                AgentEvent::MessageEnd {
                                    message: Message::Assistant(final_message.clone()),
                                },
                            );
                            self.refresh_messages_from_session();
                            self.agent.set_streaming(false);
                            return Ok(final_message);
                        }
                    }
                }
            }
        }

        let final_message = stream
            .result()
            .await
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        self.session
            .append_message(Message::Assistant(final_message.clone()))?;
        prompt_run
            .new_messages
            .push(Message::Assistant(final_message.clone()));
        if !added_partial && partial_message.is_none() {
            push_prompt_event(
                prompt_run,
                event_tx,
                AgentEvent::MessageStart {
                    message: Message::Assistant(final_message.clone()),
                },
            );
        }
        push_prompt_event(
            prompt_run,
            event_tx,
            AgentEvent::MessageEnd {
                message: Message::Assistant(final_message.clone()),
            },
        );
        self.refresh_messages_from_session();
        self.agent.set_streaming(false);
        Ok(final_message)
    }

    fn refresh_messages_from_session(&mut self) {
        self.agent.replace_messages(self.build_context_messages());
    }

    fn restore_from_session_context(&mut self) {
        let context = self.session.build_session_context();
        if let Some((provider, model_id)) = context.model {
            if let Some(model) = self.model_registry.find(&provider, &model_id) {
                self.agent.set_model(model);
            }
        }
        if let Some(thinking_level) = context
            .thinking_level
            .filter(|thinking_level| is_valid_thinking_level(thinking_level))
        {
            self.agent.set_thinking_level(thinking_level);
        }
        self.agent.replace_messages(self.build_context_messages());
    }

    fn settings_manager_mut(&mut self) -> Result<&mut SettingsManager, RuntimeError> {
        self.settings_manager
            .as_mut()
            .ok_or_else(|| RuntimeError::Message("Settings manager is not configured.".to_string()))
    }

    fn persist_default_model(&mut self, model: &Model) -> Result<(), RuntimeError> {
        if let Some(settings_manager) = self.settings_manager.as_mut() {
            settings_manager.set_default_model_and_provider(&model.provider.0, &model.id)?;
        }
        Ok(())
    }

    fn selection_model_candidates(&self) -> Vec<Model> {
        if self.scoped_models.is_empty() {
            self.model_registry.get_available()
        } else {
            self.scoped_models
                .iter()
                .map(|scoped| scoped.model.clone())
                .collect()
        }
    }

    fn build_context_messages(&self) -> Vec<Message> {
        let path = self.session.get_branch(None);
        let mut compaction_entry_id = None;
        let mut first_kept_entry_id = None;
        let mut compaction_summary = None;
        let mut compaction_timestamp = None;
        for entry in &path {
            if let Ok(SessionEntry::Compaction(entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                compaction_entry_id = Some(entry.id);
                first_kept_entry_id = Some(entry.first_kept_entry_id);
                compaction_summary = Some(entry.summary);
                compaction_timestamp = Some(entry.timestamp);
            }
        }

        let append_context_message = |entry: &Value, messages: &mut Vec<Message>| {
            if let Ok(SessionEntry::Message(message_entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                messages.push(message_entry.message);
                return;
            }
            if let Ok(SessionEntry::CustomMessage(entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                if bash_entry_excluded_from_context(&entry) {
                    return;
                }
                messages.push(Message::User(UserMessage {
                    content: entry.content,
                    timestamp: timestamp_to_millis(&entry.timestamp),
                }));
                return;
            }
            if let Ok(SessionEntry::BranchSummary(entry)) =
                serde_json::from_value::<SessionEntry>(entry.clone())
            {
                if entry.summary.is_empty() {
                    return;
                }
                messages.push(summary_message(
                    format!(
                        "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                        entry.summary
                    ),
                    &entry.timestamp,
                ));
            }
        };

        let mut contextual_messages = Vec::new();
        if let (
            Some(compaction_entry_id),
            Some(first_kept_entry_id),
            Some(summary),
            Some(timestamp),
        ) = (
            compaction_entry_id,
            first_kept_entry_id,
            compaction_summary,
            compaction_timestamp,
        ) {
            contextual_messages.push(summary_message(
                format!("{COMPACTION_SUMMARY_PREFIX}{summary}{COMPACTION_SUMMARY_SUFFIX}"),
                &timestamp,
            ));
            let compaction_index = path.iter().position(|entry| {
                parse_entry_base(entry)
                    .ok()
                    .flatten()
                    .is_some_and(|base| base.id == compaction_entry_id)
            });
            if let Some(compaction_index) = compaction_index {
                let mut found_first_kept = false;
                for entry in path.iter().take(compaction_index) {
                    if parse_entry_base(entry)
                        .ok()
                        .flatten()
                        .is_some_and(|base| base.id == first_kept_entry_id)
                    {
                        found_first_kept = true;
                    }
                    if found_first_kept {
                        append_context_message(entry, &mut contextual_messages);
                    }
                }
                for entry in path.iter().skip(compaction_index + 1) {
                    append_context_message(entry, &mut contextual_messages);
                }
            }
        } else {
            for entry in &path {
                append_context_message(entry, &mut contextual_messages);
            }
        }

        contextual_messages
    }
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        self.record_session_end();
    }
}

fn map_reasoning_level(value: &str) -> Option<AiThinkingLevel> {
    match value {
        "minimal" => Some(AiThinkingLevel::Minimal),
        "low" => Some(AiThinkingLevel::Low),
        "medium" => Some(AiThinkingLevel::Medium),
        "high" => Some(AiThinkingLevel::High),
        "xhigh" => Some(AiThinkingLevel::Xhigh),
        _ => None,
    }
}

fn empty_assistant_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 0,
            cost: cell_ai_core::UsageCost {
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
    }
}

fn aborted_assistant_message(model: &Model) -> AssistantMessage {
    let mut message = empty_assistant_message(model);
    message.stop_reason = StopReason::Aborted;
    message.error_message = Some("Request aborted".to_string());
    message
}

fn skipped_tool_result(tool_call_id: &str, tool_name: &str) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: tool_call_id.to_string(),
        tool_name: tool_name.to_string(),
        content: vec![UserContentBlock::Text {
            text: "Skipped due to queued user message.".to_string(),
            text_signature: None,
        }],
        details: Some(json!({})),
        is_error: true,
        timestamp: 0,
    }
}

fn format_bash_execution_text(
    command: &str,
    result: &cell_tools::BashExecutionResult,
) -> String {
    let mut lines = vec![format!("$ {command}")];
    if !result.output.is_empty() {
        lines.push(result.output.clone());
    }
    if result.cancelled {
        lines.push("Command cancelled".to_string());
    } else if let Some(exit_code) = result.exit_code {
        lines.push(format!("Exit code: {exit_code}"));
    }
    if result.truncated {
        if let Some(path) = &result.full_output_path {
            lines.push(format!("Full output: {path}"));
        } else {
            lines.push("Output truncated".to_string());
        }
    }
    lines.join("\n")
}

fn scoped_model_to_pattern(scoped_model: &ScopedModel) -> String {
    let base = format!(
        "{}/{}",
        scoped_model.model.provider.0, scoped_model.model.id
    );
    scoped_model
        .thinking_level
        .as_ref()
        .map(|thinking_level| format!("{base}:{thinking_level}"))
        .unwrap_or(base)
}

fn parse_runtime_command_args(args_text: &str) -> Vec<String> {
    if args_text.is_empty() {
        return Vec::new();
    }

    shlex::split(args_text).unwrap_or_else(|| {
        args_text
            .split_whitespace()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    })
}

fn extract_context_entry_text(entry: &Value) -> Option<String> {
    if let Ok(SessionEntry::CustomMessage(custom_message)) =
        serde_json::from_value::<SessionEntry>(entry.clone())
    {
        if bash_entry_excluded_from_context(&custom_message) {
            return None;
        }
    }
    extract_entry_text(entry)
}

fn extract_entry_text(entry: &Value) -> Option<String> {
    let session_entry = serde_json::from_value::<SessionEntry>(entry.clone()).ok()?;
    match session_entry {
        SessionEntry::Message(message_entry) => Some(extract_message_text(&message_entry.message)),
        SessionEntry::CustomMessage(custom_message) => Some(match custom_message.content {
            UserContent::Text(text) => text,
            UserContent::Blocks(blocks) => blocks
                .into_iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text { text, .. } => Some(text),
                    UserContentBlock::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }),
        _ => None,
    }
}

fn collect_entries_for_branch_summary(
    session: &SessionManager,
    old_leaf_id: Option<&str>,
    target_id: &str,
) -> Vec<Value> {
    let Some(old_leaf_id) = old_leaf_id else {
        return Vec::new();
    };

    let old_path = session
        .get_branch(Some(old_leaf_id))
        .into_iter()
        .filter_map(|entry| parse_entry_base(&entry).ok().flatten().map(|base| base.id))
        .collect::<HashSet<_>>();
    let target_path = session.get_branch(Some(target_id));
    let common_ancestor_id = target_path.iter().rev().find_map(|entry| {
        let base = parse_entry_base(entry).ok().flatten()?;
        old_path.contains(&base.id).then_some(base.id)
    });

    let mut entries = Vec::new();
    let mut current_id = Some(old_leaf_id.to_string());
    while let Some(current_id_value) = current_id {
        if common_ancestor_id.as_deref() == Some(current_id_value.as_str()) {
            break;
        }
        let Some(entry) = session.get_entry(&current_id_value).cloned() else {
            break;
        };
        current_id = parse_entry_base(&entry)
            .ok()
            .flatten()
            .and_then(|base| base.parent_id);
        entries.push(entry);
    }
    entries.reverse();
    entries
}

fn build_branch_summary_text(
    entries: &[Value],
    custom_instructions: Option<&str>,
) -> Option<String> {
    let summary_lines = entries
        .iter()
        .filter_map(extract_context_entry_text)
        .collect::<Vec<_>>();
    let mut summary = summary_lines.join("\n");
    if let Some(custom_instructions) = custom_instructions.filter(|value| !value.trim().is_empty())
    {
        summary = format!("{custom_instructions}\n\n{summary}");
    }
    if summary.trim().is_empty() {
        None
    } else {
        Some(summary)
    }
}

fn extract_message_text(message: &Message) -> String {
    match message {
        Message::User(user) => match &user.content {
            UserContent::Text(text) => text.clone(),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    UserContentBlock::Text { text, .. } => Some(text.clone()),
                    UserContentBlock::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        },
        Message::Assistant(assistant) => assistant
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::Text { text, .. } => Some(text.clone()),
                AssistantContentBlock::Thinking { thinking, .. } => Some(thinking.clone()),
                AssistantContentBlock::ToolCall {
                    name, arguments, ..
                } => Some(format!("tool:{} {}", name, arguments)),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Message::ToolResult(tool_result) => tool_result
            .content
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text { text, .. } => Some(text.clone()),
                UserContentBlock::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn bash_entry_excluded_from_context(entry: &SessionCustomMessageEntry) -> bool {
    entry.custom_type == "bash_execution"
        && entry
            .details
            .as_ref()
            .and_then(|details| details.get("excludeFromContext"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn summary_message(text: String, timestamp: &str) -> Message {
    Message::User(UserMessage {
        content: UserContent::Blocks(vec![UserContentBlock::Text {
            text,
            text_signature: None,
        }]),
        timestamp: timestamp_to_millis(timestamp),
    })
}

fn timestamp_to_millis(timestamp: &str) -> i64 {
    OffsetDateTime::parse(timestamp, &Rfc3339)
        .map(|value| i64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or(0))
        .unwrap_or(0)
}

fn emit_event(event_tx: &Option<mpsc::Sender<AgentEvent>>, event: &AgentEvent) {
    if let Some(event_tx) = event_tx {
        let _ = event_tx.send(event.clone());
    }
}

fn push_prompt_event(
    prompt_run: &mut PromptRun,
    event_tx: &Option<mpsc::Sender<AgentEvent>>,
    event: AgentEvent,
) {
    emit_event(event_tx, &event);
    prompt_run.events.push(event);
}

fn location_for_scope(scope: cell_resources::ResourceScope) -> RpcCommandLocation {
    match scope {
        cell_resources::ResourceScope::Global => RpcCommandLocation::User,
        cell_resources::ResourceScope::Project => RpcCommandLocation::Project,
    }
}

fn plugin_runtime_summary_from_registered(
    summary: &RegisteredPluginSummary,
) -> RpcPluginRuntimePluginSummary {
    RpcPluginRuntimePluginSummary {
        descriptor_path: summary.descriptor_path.to_string_lossy().to_string(),
        plugin_id: summary.plugin_id.clone(),
        plugin_name: summary.plugin_name.clone(),
        manifest_version: summary.manifest_version,
        command_count: summary.commands.len(),
        tool_count: summary.tools.len(),
        flag_count: summary.flags.len(),
        hook_count: summary.hooks.len(),
        provider_count: summary.providers.len(),
        model_count: summary.models.len(),
    }
}

fn plugin_runtime_warning_from_host(warning: &PluginHostWarning) -> RpcPluginRuntimeWarning {
    RpcPluginRuntimeWarning {
        path: Some(warning.path.to_string_lossy().to_string()),
        plugin_id: warning.plugin_id.clone(),
        plugin_name: warning.plugin_name.clone(),
        event: None,
        details: None,
        message: warning.message.clone(),
    }
}

fn plugin_runtime_warning_from_host_event(
    warning: &PluginHostWarning,
    event: &LifecycleEventV1,
    details: Option<&str>,
) -> RpcPluginRuntimeWarning {
    RpcPluginRuntimeWarning {
        path: Some(warning.path.to_string_lossy().to_string()),
        plugin_id: warning.plugin_id.clone(),
        plugin_name: warning.plugin_name.clone(),
        event: Some(lifecycle_event_name(event).to_string()),
        details: details.map(ToOwned::to_owned),
        message: warning.message.clone(),
    }
}

fn build_lifecycle_hook_context(
    event: &LifecycleEventV1,
    details: Option<&str>,
    cwd: &Path,
    session_id: Option<&str>,
    provider_id: Option<&str>,
    model_id: Option<&str>,
) -> LifecycleHookContextV1 {
    let mut data = BTreeMap::new();
    if let Some(details) = details {
        data.insert("details".to_string(), Value::String(details.to_string()));
        for segment in details.split(';') {
            let Some((key, value)) = segment.trim().split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                continue;
            }
            data.insert(key.to_string(), Value::String(value.to_string()));
        }
    }

    LifecycleHookContextV1 {
        event: event.clone(),
        plugin_id: lifecycle_subject_plugin_id(event, details),
        workspace_root: Some(cwd.to_path_buf()),
        session_id: session_id.map(ToOwned::to_owned),
        provider_id: provider_id.map(ToOwned::to_owned),
        model_id: model_id.map(ToOwned::to_owned),
        data,
    }
}

fn lifecycle_subject_plugin_id(event: &LifecycleEventV1, details: Option<&str>) -> String {
    if matches!(event, LifecycleEventV1::PluginLoaded) {
        if let Some(plugin_id) = details.and_then(|text| {
            text.split(';').find_map(|segment| {
                let (key, value) = segment.trim().split_once('=')?;
                (key.trim() == "plugin_id").then(|| value.trim().to_string())
            })
        }) {
            return plugin_id;
        }
    }
    "cell".to_string()
}

fn lifecycle_event_name(event: &LifecycleEventV1) -> &'static str {
    match event {
        LifecycleEventV1::PluginLoaded => "plugin_loaded",
        LifecycleEventV1::PluginEnabled => "plugin_enabled",
        LifecycleEventV1::PluginDisabled => "plugin_disabled",
        LifecycleEventV1::HostStartup => "host_startup",
        LifecycleEventV1::HostShutdown => "host_shutdown",
        LifecycleEventV1::SessionStarted => "session_started",
        LifecycleEventV1::SessionEnded => "session_ended",
        LifecycleEventV1::PromptStarted => "prompt_started",
        LifecycleEventV1::PromptFinished => "prompt_finished",
        LifecycleEventV1::CommandStarted => "command_started",
        LifecycleEventV1::CommandFinished => "command_finished",
        LifecycleEventV1::ToolStarted => "tool_started",
        LifecycleEventV1::ToolFinished => "tool_finished",
        LifecycleEventV1::ProviderRegistered => "provider_registered",
        LifecycleEventV1::ModelRegistered => "model_registered",
    }
}

pub fn rpc_event_from_agent_event(event: AgentEvent) -> RpcEvent {
    match event {
        AgentEvent::AgentStart => RpcEvent::AgentStart,
        AgentEvent::AgentEnd { messages } => RpcEvent::AgentEnd { messages },
        AgentEvent::TurnStart => RpcEvent::TurnStart,
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => RpcEvent::TurnEnd {
            message,
            tool_results,
        },
        AgentEvent::MessageStart { message } => RpcEvent::MessageStart { message },
        AgentEvent::MessageUpdate {
            message,
            assistant_message_event,
        } => RpcEvent::MessageUpdate {
            message,
            assistant_message_event,
        },
        AgentEvent::MessageEnd { message } => RpcEvent::MessageEnd { message },
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => RpcEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        },
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
        } => RpcEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
        },
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
        } => RpcEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
        },
        AgentEvent::AutoCompactionStart { reason } => RpcEvent::AutoCompactionStart { reason },
        AgentEvent::AutoCompactionEnd {
            result,
            aborted,
            will_retry,
            error_message,
        } => RpcEvent::AutoCompactionEnd {
            result,
            aborted,
            will_retry,
            error_message,
        },
        AgentEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        } => RpcEvent::AutoRetryStart {
            attempt,
            max_attempts,
            delay_ms,
            error_message,
        },
        AgentEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        } => RpcEvent::AutoRetryEnd {
            success,
            attempt,
            final_error,
        },
    }
}

pub fn build_scoped_models(
    patterns: &[String],
    model_registry: &ModelRegistry,
) -> Vec<ScopedModel> {
    resolve_model_scope(patterns, model_registry)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, mpsc};

    use cell_ai_core::{
        ApiId, AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message,
        Model, ModelCost, ModelInput, ProviderId, StopReason, StreamOptions, Usage, UsageCost,
        UserContent, UserContentBlock, UserMessage,
    };
    use cell_ai_providers::{ApiProvider, ProviderRegistry};
    use cell_config::{ENV_AGENT_DIR, PROJECT_CONFIG_DIR_NAME, SettingsManager};
    use cell_models::{ModelRegistry, ScopedModel};
    use cell_oauth::AuthStorage;
    use cell_packages::{PackageInstallScope, PackageManager};
    use cell_plugins::{
        CommandRegistrationV1, LifecycleEventV1, LifecycleHookRegistrationV1, ModelInputKindV1,
        ModelRegistrationV1, PluginIdentityV1, PluginManifestV1, ProviderAuthV1,
        ProviderRegistrationV1, ToolRegistrationV1, ValueKindV1,
    };
    use cell_protocol::{OutputMode, RpcEvent, RpcPluginRuntimeDiagnostics};
    use cell_session::SessionManager;
    use cell_tools::ToolSet;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::{NonInteractiveRequest, create_agent_session};

    use super::{
        AgentEvent, AgentSession, StartupResourceNoticeSection, StartupResourceSummary,
        rpc_event_from_agent_event,
    };

    struct EchoProvider;

    struct SlowEchoProvider;

    struct StreamingEchoProvider;

    struct ContextEchoProvider;

    impl ApiProvider for EchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> cell_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
            let prompt = match context.messages.last() {
                Some(Message::User(UserMessage {
                    content: UserContent::Text(text),
                    ..
                })) => text.clone(),
                Some(Message::ToolResult(tool_result)) => match &tool_result.content[0] {
                    UserContentBlock::Text { text, .. } => format!("tool:{text}"),
                    _ => "tool".to_string(),
                },
                _ => String::new(),
            };
            let assistant = if prompt == "call-tool" {
                AssistantMessage {
                    content: vec![AssistantContentBlock::ToolCall {
                        id: "tool-1".to_string(),
                        name: "write".to_string(),
                        arguments: json!({"path":"result.txt","content":"written"}),
                        thought_signature: None,
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
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: 0,
                }
            } else {
                AssistantMessage {
                    content: vec![AssistantContentBlock::Text {
                        text: prompt,
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
                }
            };
            sender.send(AssistantMessageEvent::Done {
                reason: assistant.stop_reason,
                message: assistant,
            });
            stream
        }
    }

    impl ApiProvider for SlowEchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> cell_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
            let prompt = match context.messages.last() {
                Some(Message::User(UserMessage {
                    content: UserContent::Text(text),
                    ..
                })) => text.clone(),
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
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(120));
                sender.send(AssistantMessageEvent::Done {
                    reason: assistant.stop_reason,
                    message: assistant,
                });
            });
            stream
        }
    }

    impl ApiProvider for StreamingEchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> cell_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
            let prompt = match context.messages.last() {
                Some(Message::User(UserMessage {
                    content: UserContent::Text(text),
                    ..
                })) => text.clone(),
                _ => String::new(),
            };
            let partial = AssistantMessage {
                content: vec![AssistantContentBlock::Text {
                    text: "echo:".to_string(),
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
            let final_message = AssistantMessage {
                content: vec![AssistantContentBlock::Text {
                    text: format!("echo:{prompt}"),
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
            std::thread::spawn(move || {
                sender.send(AssistantMessageEvent::Start {
                    partial: partial.clone(),
                });
                std::thread::sleep(std::time::Duration::from_millis(20));
                sender.send(AssistantMessageEvent::TextDelta {
                    content_index: 0,
                    delta: prompt.clone(),
                    partial: final_message.clone(),
                });
                std::thread::sleep(std::time::Duration::from_millis(20));
                sender.send(AssistantMessageEvent::Done {
                    reason: final_message.stop_reason,
                    message: final_message,
                });
            });
            stream
        }
    }

    impl ApiProvider for ContextEchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> cell_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
            let response_text = match context.messages.last() {
                Some(Message::User(UserMessage {
                    content: UserContent::Text(text),
                    ..
                })) if text == "__system__" => context.system_prompt.clone().unwrap_or_default(),
                Some(Message::User(UserMessage {
                    content: UserContent::Text(text),
                    ..
                })) => text.clone(),
                Some(Message::User(UserMessage {
                    content: UserContent::Blocks(blocks),
                    ..
                })) if matches!(
                    blocks.first(),
                    Some(UserContentBlock::Text { text, .. }) if text == "__system__"
                ) =>
                {
                    context.system_prompt.clone().unwrap_or_default()
                }
                Some(Message::User(UserMessage {
                    content: UserContent::Blocks(blocks),
                    ..
                })) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text { text, .. } => Some(text.clone()),
                        UserContentBlock::Image { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
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

    struct HookEchoProvider;

    impl ApiProvider for HookEchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> cell_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = cell_ai_core::AssistantMessageEventStream::new();
            let prompt = match context.messages.last() {
                Some(Message::User(UserMessage {
                    content: UserContent::Text(text),
                    ..
                })) => text.clone(),
                _ => String::new(),
            };
            let assistant = if prompt == "call-plugin-tool" {
                AssistantMessage {
                    content: vec![AssistantContentBlock::ToolCall {
                        id: "tool-1".to_string(),
                        name: "plugin-write".to_string(),
                        arguments: json!({"value":"tool"}),
                        thought_signature: None,
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
                    stop_reason: StopReason::ToolUse,
                    error_message: None,
                    timestamp: 0,
                }
            } else {
                AssistantMessage {
                    content: vec![AssistantContentBlock::Text {
                        text: prompt,
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
                }
            };
            sender.send(AssistantMessageEvent::Done {
                reason: assistant.stop_reason,
                message: assistant,
            });
            stream
        }
    }

    fn env_guard() -> &'static Mutex<()> {
        crate::test_env_guard()
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write file");
    }

    fn write_executable_script(path: &Path, content: &str) {
        write_file(path, content);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).expect("script metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("mark script executable");
        }
    }

    fn plugin_registration_json(
        id: &str,
        name: &str,
        commands: &[&str],
        tools: &[&str],
        providers: &[&str],
        models: &[&str],
    ) -> String {
        let mut manifest = PluginManifestV1::new(PluginIdentityV1 {
            id: id.to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: Some(format!("{name} plugin")),
            authors: vec!["Acme".to_string()],
            homepage: None,
            repository: None,
            license: Some("MIT".to_string()),
        });

        for command_name in commands {
            manifest.commands.push(CommandRegistrationV1 {
                name: (*command_name).to_string(),
                description: Some(format!("Command {command_name}")),
                aliases: Vec::new(),
                parameters: Vec::new(),
                hidden: false,
            });
        }

        for tool_name in tools {
            manifest.tools.push(ToolRegistrationV1 {
                name: (*tool_name).to_string(),
                description: Some(format!("Tool {tool_name}")),
                aliases: Vec::new(),
                parameters: Vec::new(),
                output: Some(ValueKindV1::String),
                hidden: false,
            });
        }

        for provider_id in providers {
            manifest.providers.push(ProviderRegistrationV1 {
                provider_id: (*provider_id).to_string(),
                name: format!("{provider_id} provider"),
                api: format!("{provider_id}-chat"),
                description: Some(format!("Provider {provider_id}")),
                base_url: Some("https://example.invalid".to_string()),
                headers: Default::default(),
                auth: ProviderAuthV1::None,
            });
        }

        for model_id in models {
            manifest.models.push(ModelRegistrationV1 {
                provider_id: providers.first().copied().unwrap_or(id).to_string(),
                model_id: (*model_id).to_string(),
                name: format!("{model_id} model"),
                description: None,
                input_modalities: vec![ModelInputKindV1::Text],
                reasoning: false,
                context_window: 4096,
                max_output_tokens: 1024,
                default: false,
            });
        }

        serde_json::to_string(&cell_plugin_host::PluginMessage::Registration {
            protocol_version: cell_plugin_host::HOST_PROTOCOL_VERSION_V1,
            manifest,
        })
        .expect("serialize registration")
    }

    fn plugin_registration_json_with_hooks(
        id: &str,
        name: &str,
        commands: &[&str],
        tools: &[&str],
        providers: &[&str],
        models: &[&str],
        hooks: &[(LifecycleEventV1, &str, i16)],
    ) -> String {
        let mut manifest = PluginManifestV1::new(PluginIdentityV1 {
            id: id.to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: Some(format!("{name} plugin")),
            authors: vec!["Acme".to_string()],
            homepage: None,
            repository: None,
            license: Some("MIT".to_string()),
        });

        for command_name in commands {
            manifest.commands.push(CommandRegistrationV1 {
                name: (*command_name).to_string(),
                description: Some(format!("Command {command_name}")),
                aliases: Vec::new(),
                parameters: Vec::new(),
                hidden: false,
            });
        }

        for tool_name in tools {
            manifest.tools.push(ToolRegistrationV1 {
                name: (*tool_name).to_string(),
                description: Some(format!("Tool {tool_name}")),
                aliases: Vec::new(),
                parameters: Vec::new(),
                output: Some(ValueKindV1::String),
                hidden: false,
            });
        }

        for provider_id in providers {
            manifest.providers.push(ProviderRegistrationV1 {
                provider_id: (*provider_id).to_string(),
                name: format!("{provider_id} provider"),
                api: format!("{provider_id}-chat"),
                description: Some(format!("Provider {provider_id}")),
                base_url: Some("https://example.invalid".to_string()),
                headers: Default::default(),
                auth: ProviderAuthV1::None,
            });
        }

        for model_id in models {
            manifest.models.push(ModelRegistrationV1 {
                provider_id: providers.first().copied().unwrap_or(id).to_string(),
                model_id: (*model_id).to_string(),
                name: format!("{model_id} model"),
                description: None,
                input_modalities: vec![ModelInputKindV1::Text],
                reasoning: false,
                context_window: 4096,
                max_output_tokens: 1024,
                default: false,
            });
        }

        for (event, hook_name, priority) in hooks {
            manifest.hooks.push(LifecycleHookRegistrationV1 {
                event: event.clone(),
                name: (*hook_name).to_string(),
                description: Some(format!("Hook {hook_name}")),
                priority: *priority,
            });
        }

        serde_json::to_string(&cell_plugin_host::PluginMessage::Registration {
            protocol_version: cell_plugin_host::HOST_PROTOCOL_VERSION_V1,
            manifest,
        })
        .expect("serialize registration")
    }

    fn plugin_script(manifest_json: &str) -> String {
        format!(
            r#"#!/bin/sh
set -eu
read request
case "$request" in
  *'"type":"handshake_request"'* ) ;;
  * ) echo "unexpected handshake" >&2; exit 42 ;;
esac
cat <<'JSON'
{manifest_json}
JSON
"#
        )
    }

    fn plugin_runtime_script(manifest_json: &str, handler_python: &str) -> String {
        format!(
            r#"#!/bin/sh
set -eu
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
cat >"$tmp" <<'PY'
import json, sys
handshake = json.loads(sys.stdin.readline())
if handshake.get("type") != "handshake_request":
    sys.stderr.write("unexpected handshake\n")
    sys.exit(42)
print(r'''{manifest_json}''')
sys.stdout.flush()
{handler_python}
PY
python3 "$tmp"
"#
        )
    }

    fn plugin_descriptor_json(id: &str, name: &str) -> String {
        serde_json::to_string_pretty(&cell_plugin_host::PluginLaunchDescriptor {
            id: id.to_string(),
            name: name.to_string(),
            executable: PathBuf::from("plugin.sh"),
            args: Vec::new(),
            working_directory: None,
            env: Default::default(),
            description: Some(format!("{name} plugin")),
        })
        .expect("serialize descriptor")
    }

    fn install_plugin_package(
        package_manager: &mut PackageManager,
        package_root: &Path,
    ) -> Result<(), cell_packages::PackageManagerError> {
        package_manager.install(
            &package_root.to_string_lossy(),
            PackageInstallScope::Project,
        )?;
        Ok(())
    }

    fn model() -> Model {
        Model {
            id: "gpt-5.1-codex".to_string(),
            name: "GPT".to_string(),
            api: ApiId::new("openai-responses"),
            provider: ProviderId::new("openai"),
            base_url: "https://api.openai.com/v1".to_string(),
            reasoning: true,
            input: vec![ModelInput::Text],
            cost: ModelCost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            context_window: 10,
            max_tokens: 10,
            headers: None,
            compat: None,
        }
    }

    fn assistant_message(timestamp: i64) -> AssistantMessage {
        AssistantMessage {
            content: vec![AssistantContentBlock::Text {
                text: "assistant".to_string(),
                text_signature: None,
            }],
            api: model().api,
            provider: model().provider,
            model: model().id,
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
            timestamp,
        }
    }

    #[tokio::test]
    async fn prompt_text_runs_and_collects_events() {
        let tempdir = tempdir().expect("tempdir");
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::with_enabled_names(tempdir.path(), &["write".to_string()]);
        let mut agent_session = AgentSession::new(
            providers,
            models,
            session,
            tools,
            model(),
            "off",
            Some("system".to_string()),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        let run = agent_session
            .prompt_text("hello".to_string())
            .await
            .expect("prompt");
        assert_eq!(
            run.assistant_message.content,
            vec![AssistantContentBlock::Text {
                text: "hello".to_string(),
                text_signature: None,
            }]
        );
        assert!(
            run.events
                .iter()
                .any(|event| matches!(event, AgentEvent::AgentStart))
        );
    }

    #[tokio::test]
    async fn tool_calls_append_tool_results() {
        let tempdir = tempdir().expect("tempdir");
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::with_enabled_names(tempdir.path(), &["write".to_string()]);
        let mut agent_session = AgentSession::new(
            providers,
            models,
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        let run = agent_session
            .prompt_text("call-tool".to_string())
            .await
            .expect("prompt");
        assert!(!run.tool_results.is_empty());
        assert!(tempdir.path().join("result.txt").exists());
    }

    #[tokio::test]
    async fn steering_messages_take_precedence_after_non_tool_turns() {
        let tempdir = tempdir().expect("tempdir");
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(SlowEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::with_enabled_names(tempdir.path(), &["read".to_string()]);
        let mut agent_session = AgentSession::new(
            providers,
            models,
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let control = agent_session.control();

        let prompt =
            tokio::spawn(async move { agent_session.prompt_text("first".to_string()).await });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        control.steer(Message::User(UserMessage {
            content: UserContent::Text("second".to_string()),
            timestamp: 0,
        }));

        let run = prompt.await.expect("join").expect("prompt");
        assert_eq!(
            run.assistant_message.content,
            vec![AssistantContentBlock::Text {
                text: "echo:second".to_string(),
                text_signature: None,
            }]
        );
    }

    #[tokio::test]
    async fn prompt_text_with_events_emits_live_updates_before_completion() {
        let tempdir = tempdir().expect("tempdir");
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(StreamingEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::with_enabled_names(tempdir.path(), &["read".to_string()]);
        let mut agent_session = AgentSession::new(
            providers,
            models,
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let (event_tx, event_rx) = mpsc::channel();

        let prompt = tokio::spawn(async move {
            agent_session
                .prompt_text_prepared_with_events("hello".to_string(), event_tx)
                .await
        });
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let mut saw_start = false;
        let mut saw_delta = false;

        while tokio::time::Instant::now() < deadline && !(saw_start && saw_delta) {
            match event_rx.try_recv() {
                Ok(AgentEvent::MessageStart {
                    message: Message::Assistant(_),
                }) => saw_start = true,
                Ok(AgentEvent::MessageUpdate {
                    assistant_message_event: AssistantMessageEvent::TextDelta { .. },
                    ..
                }) => saw_delta = true,
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }

        let run = prompt.await.expect("join").expect("prompt");
        assert!(saw_start, "expected a live assistant start event");
        assert!(saw_delta, "expected a live text delta event");
        assert_eq!(
            run.assistant_message.content,
            vec![AssistantContentBlock::Text {
                text: "echo:hello".to_string(),
                text_signature: None,
            }]
        );
    }

    #[test]
    fn scoped_model_cycle_preserves_configured_order_and_persists_default() {
        let tempdir = tempdir().expect("tempdir");
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        auth.set_runtime_api_key("anthropic", "runtime-key");
        auth.set_runtime_api_key("openrouter", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::with_enabled_names(tempdir.path(), &["read".to_string()]);
        let mut agent_session = AgentSession::new(
            providers,
            models.clone(),
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        agent_session.attach_settings_manager(SettingsManager::from_paths(
            tempdir.path().join("global.json"),
            tempdir.path().join("project.json"),
        ));
        agent_session.set_scoped_models(vec![
            ScopedModel {
                model: models
                    .find("anthropic", "claude-opus-4-6")
                    .expect("anthropic model"),
                thinking_level: Some("high".to_string()),
            },
            ScopedModel {
                model: models
                    .find("openrouter", "openai/gpt-5.1-codex")
                    .expect("openrouter model"),
                thinking_level: None,
            },
            ScopedModel {
                model: models
                    .find("openai", "gpt-5.1-codex")
                    .expect("openai model"),
                thinking_level: None,
            },
        ]);

        let cycle = agent_session
            .cycle_model()
            .expect("cycle result")
            .expect("next model");
        assert_eq!(cycle.model.provider.0, "anthropic");
        assert_eq!(cycle.model.id, "claude-opus-4-6");
        assert_eq!(cycle.thinking_level, "high");

        let persisted = fs::read_to_string(tempdir.path().join("global.json")).expect("settings");
        let persisted: serde_json::Value =
            serde_json::from_str(&persisted).expect("parse settings");
        assert_eq!(persisted["defaultProvider"], json!("anthropic"));
        assert_eq!(persisted["defaultModel"], json!("claude-opus-4-6"));
    }

    #[test]
    fn scoped_models_can_be_persisted_and_restored() {
        let tempdir = tempdir().expect("tempdir");
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        auth.set_runtime_api_key("anthropic", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::with_enabled_names(tempdir.path(), &["read".to_string()]);
        let mut agent_session = AgentSession::new(
            providers,
            models.clone(),
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        agent_session.attach_settings_manager(SettingsManager::from_paths(
            tempdir.path().join("global.json"),
            tempdir.path().join("project.json"),
        ));
        agent_session.set_scoped_models(vec![
            ScopedModel {
                model: models
                    .find("openai", "gpt-5.1-codex")
                    .expect("openai model"),
                thinking_level: Some("minimal".to_string()),
            },
            ScopedModel {
                model: models
                    .find("anthropic", "claude-opus-4-6")
                    .expect("anthropic model"),
                thinking_level: None,
            },
        ]);

        let patterns = agent_session
            .save_current_scoped_models()
            .expect("persist scoped models");
        assert_eq!(
            patterns,
            vec![
                "openai/gpt-5.1-codex:minimal".to_string(),
                "anthropic/claude-opus-4-6".to_string(),
            ]
        );
        assert_eq!(
            agent_session.get_persisted_enabled_model_patterns(),
            Some(patterns.clone())
        );

        agent_session.set_scoped_models(Vec::new());
        let restored = agent_session
            .load_persisted_enabled_models()
            .expect("restore scoped models");
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].model.provider.0, "openai");
        assert_eq!(restored[0].thinking_level.as_deref(), Some("minimal"));
        assert_eq!(restored[1].model.provider.0, "anthropic");

        agent_session
            .clear_persisted_enabled_models()
            .expect("clear enabled models");
        assert_eq!(agent_session.get_persisted_enabled_model_patterns(), None);
    }

    #[test]
    fn model_selection_prefers_exact_matches_and_respects_scoped_models() {
        let tempdir = tempdir().expect("tempdir");
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        auth.set_runtime_api_key("anthropic", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::with_enabled_names(tempdir.path(), &["read".to_string()]);
        let mut agent_session = AgentSession::new(
            providers,
            models.clone(),
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        let bare_exact = agent_session
            .find_model_for_selection("gpt-5.1-codex")
            .expect("bare exact match");
        assert_eq!(bare_exact.provider.0, "openai");
        assert_eq!(bare_exact.id, "gpt-5.1-codex");
        assert_eq!(
            agent_session
                .find_model_for_selection("openai/gpt-5.1-codex")
                .expect("provider/model exact match")
                .provider
                .0,
            "openai"
        );

        agent_session.set_scoped_models(vec![ScopedModel {
            model: models
                .find("anthropic", "claude-opus-4-6")
                .expect("anthropic model"),
            thinking_level: None,
        }]);

        assert!(
            agent_session
                .find_model_for_selection("gpt-5.1-codex")
                .is_none(),
            "bare exact match should respect scoped-model constraints"
        );
        let scoped_exact = agent_session
            .find_model_for_selection("claude-opus-4-6")
            .expect("scoped exact match");
        assert_eq!(scoped_exact.provider.0, "anthropic");
    }

    #[test]
    fn manual_bash_metadata_distinguishes_excluded_context() {
        let tempdir = tempdir().expect("tempdir");
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::with_enabled_names(tempdir.path(), &["read".to_string()]);
        let mut agent_session = AgentSession::new(
            providers,
            models,
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        agent_session
            .manual_bash("printf hidden", true)
            .expect("manual bash");

        assert!(agent_session.get_messages().is_empty());
        let entry = agent_session
            .session()
            .get_entries()
            .last()
            .cloned()
            .expect("bash entry");
        assert_eq!(entry["type"], json!("custom_message"));
        assert_eq!(entry["customType"], json!("bash_execution"));
        assert_eq!(entry["details"]["prefix"], json!("!!"));
        assert_eq!(entry["details"]["excludeFromContext"], json!(true));
    }

    #[test]
    fn forkable_user_messages_only_include_user_entries_in_order() {
        let tempdir = tempdir().expect("tempdir");
        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let mut session = SessionManager::in_memory(tempdir.path());
        let first_id = session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("first".to_string()),
                timestamp: 1,
            }))
            .expect("first user");
        let assistant_id = session
            .append_message(Message::Assistant(assistant_message(2)))
            .expect("assistant");
        let second_id = session
            .append_message(Message::User(UserMessage {
                content: UserContent::Text("second".to_string()),
                timestamp: 3,
            }))
            .expect("second user");
        let tools = ToolSet::with_enabled_names(tempdir.path(), &["read".to_string()]);
        let agent_session = AgentSession::new(
            providers,
            models,
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );

        let forkable = agent_session.forkable_user_messages();
        assert_eq!(forkable.len(), 2);
        assert_eq!(forkable[0].entry_id, first_id);
        assert_eq!(forkable[0].index, 0);
        assert_eq!(forkable[0].text, "first");
        assert_eq!(forkable[1].entry_id, second_id);
        assert_eq!(forkable[1].index, 1);
        assert_eq!(forkable[1].text, "second");
        assert_eq!(
            forkable[1].parent_id.as_deref(),
            Some(assistant_id.as_str())
        );
    }

    #[test]
    fn create_agent_session_records_startup_resource_summary() {
        let _guard = env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let broken_context = cwd.join("AGENTS.md");
        let project_skill = cwd
            .join(PROJECT_CONFIG_DIR_NAME)
            .join("skills")
            .join("dup")
            .join("SKILL.md");
        let project_prompt = cwd
            .join(PROJECT_CONFIG_DIR_NAME)
            .join("prompts")
            .join("review.md");
        let missing_theme = cwd.join("missing-theme.json");

        write_file(&agent_dir.join("AGENTS.md"), "global context");
        fs::create_dir_all(&broken_context).expect("create broken context dir");
        write_file(
            &agent_dir.join("skills").join("dup").join("SKILL.md"),
            "---\nname: dup\ndescription: Global dup\n---\nGlobal dup",
        );
        write_file(
            &project_skill,
            "---\nname: dup\ndescription: Project dup\n---\nProject dup",
        );
        write_file(
            &agent_dir.join("prompts").join("review.md"),
            "---\ndescription: Global review\n---\nGlobal review",
        );
        write_file(
            &project_prompt,
            "---\ndescription: Project review\n---\nProject review",
        );

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(ContextEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);

        let agent_session = create_agent_session(
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
                themes: vec![missing_theme.clone()],
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

        let summary = agent_session.startup_resource_summary();
        assert_eq!(summary.context_paths, vec![agent_dir.join("AGENTS.md")]);
        assert_eq!(
            summary.skills,
            vec![
                agent_dir.join("skills").join("dup").join("SKILL.md"),
                project_skill.clone()
            ]
        );
        assert_eq!(
            summary.prompts,
            vec![
                agent_dir.join("prompts").join("review.md"),
                project_prompt.clone()
            ]
        );
        assert_eq!(summary.extensions, Vec::<PathBuf>::new());
        assert!(summary.extension_summaries.is_empty());
        assert_eq!(summary.themes, vec![missing_theme.clone()]);
        assert_eq!(
            summary.conflicts.len(),
            summary.notices.len().saturating_sub(1)
        );
        assert!(summary.notices.iter().any(|notice| {
            notice.section == StartupResourceNoticeSection::Context && notice.path == broken_context
        }));
        assert!(summary.notices.iter().any(|notice| {
            notice.section == StartupResourceNoticeSection::Skill && notice.path == project_skill
        }));
        assert!(summary.notices.iter().any(|notice| {
            notice.section == StartupResourceNoticeSection::Prompt && notice.path == project_prompt
        }));
        assert!(summary.notices.iter().any(|notice| {
            notice.section == StartupResourceNoticeSection::Theme && notice.path == missing_theme
        }));
    }

    #[tokio::test]
    async fn reload_runtime_resources_refreshes_startup_resource_summary() {
        let _guard = env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let project_context = cwd.join("AGENTS.md");
        write_file(&agent_dir.join("SYSTEM.md"), "system");
        write_file(&project_context, "project context");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(ContextEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);

        let mut agent_session = create_agent_session(
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

        assert_eq!(
            agent_session.startup_resource_summary().context_paths,
            vec![project_context.clone()]
        );
        assert!(agent_session.startup_resource_summary().notices.is_empty());
        assert!(
            agent_session
                .startup_resource_summary()
                .extension_summaries
                .is_empty()
        );

        fs::remove_file(&project_context).expect("remove context file");
        fs::create_dir_all(&project_context).expect("create broken context dir");

        agent_session
            .reload_runtime_resources()
            .expect("reload runtime resources");

        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        let summary = agent_session.startup_resource_summary();
        assert!(summary.context_paths.is_empty());
        let notice = summary
            .notices
            .iter()
            .find(|notice| {
                notice.section == StartupResourceNoticeSection::Context
                    && notice.path == project_context
            })
            .expect("context diagnostic");
        assert!(notice.message.contains("failed to read resource file"));
    }

    #[tokio::test]
    async fn create_agent_session_records_startup_plugin_summaries_and_warnings() {
        let _guard = env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let good_root = tempdir.path().join("packages/good");
        let malformed_root = tempdir.path().join("packages/malformed");
        let good_descriptor = good_root.join(cell_plugin_host::DISCOVERY_FILE_NAMES[0]);
        let malformed_descriptor =
            malformed_root.join(cell_plugin_host::DISCOVERY_FILE_NAMES[0]);

        write_executable_script(
            &good_root.join("plugin.sh"),
            &plugin_script(&plugin_registration_json(
                "good",
                "Good Plugin",
                &["good-command"],
                &["good-tool"],
                &["good-provider"],
                &["good-model"],
            )),
        );
        write_file(
            &good_descriptor,
            &plugin_descriptor_json("good", "Good Plugin"),
        );
        write_file(&malformed_descriptor, "{ not json }\n");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

        let mut package_manager = PackageManager::create(&cwd, Some(agent_dir.clone()));
        install_plugin_package(&mut package_manager, &good_root).expect("install good plugin");
        install_plugin_package(&mut package_manager, &malformed_root)
            .expect("install malformed plugin");

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(ContextEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);

        let agent_session = create_agent_session(
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

        let summary = agent_session.startup_resource_summary();
        assert_eq!(summary.extension_summaries.len(), 1);
        assert!(
            summary
                .extension_summaries
                .iter()
                .any(|line| line.contains("Good Plugin [good]"))
        );
        assert_eq!(summary.extensions, vec![good_descriptor.clone()]);
        assert!(summary.notices.iter().any(|notice| {
            notice.section == StartupResourceNoticeSection::Extension
                && notice.path == malformed_descriptor
                && notice.message.contains("failed to parse plugin descriptor")
        }));

        let diagnostics: RpcPluginRuntimeDiagnostics =
            agent_session.get_plugin_runtime_diagnostics();
        assert_eq!(diagnostics.plugins.len(), 1);
        assert_eq!(diagnostics.plugins[0].plugin_id, "good");
        assert_eq!(diagnostics.warnings.len(), 1);
        assert!(
            diagnostics
                .warnings
                .iter()
                .any(|warning| warning.message.contains("failed to parse plugin descriptor"))
        );
    }

    #[tokio::test]
    async fn reload_runtime_resources_refreshes_commands_prompts_and_themes() {
        let _guard = env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        write_file(&agent_dir.join("SYSTEM.md"), "initial system");
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("prompts")
                .join("review.md"),
            "---\ndescription: Review a target\n---\nReview $1",
        );
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("skills")
                .join("checks")
                .join("SKILL.md"),
            "---\nname: checks\ndescription: Run checks\n---\nRun checks.",
        );
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("themes")
                .join("light.json"),
            "{}",
        );

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(ContextEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);

        let mut agent_session = create_agent_session(
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

        write_file(&agent_dir.join("SYSTEM.md"), "updated system");
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("prompts")
                .join("plan.md"),
            "---\ndescription: Plan work\n---\nPlan $1",
        );
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("skills")
                .join("lint")
                .join("SKILL.md"),
            "---\nname: lint\ndescription: Run lint\n---\nRun lint.",
        );
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("themes")
                .join("dark.json"),
            "{}",
        );

        agent_session
            .reload_runtime_resources()
            .expect("reload runtime resources");

        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        let commands = agent_session.get_commands();
        assert!(commands.iter().any(|command| command.name == "review"));
        assert!(commands.iter().any(|command| command.name == "plan"));
        assert!(
            commands
                .iter()
                .any(|command| command.name == "skill:checks")
        );
        assert!(commands.iter().any(|command| command.name == "skill:lint"));
        assert_eq!(agent_session.get_themes().len(), 2);

        let prompt_run = agent_session
            .prompt_text("/review src/lib.rs".to_string())
            .await
            .expect("prompt run");
        assert_eq!(
            prompt_run.assistant_message.content,
            vec![AssistantContentBlock::Text {
                text: "Review src/lib.rs".to_string(),
                text_signature: None,
            }]
        );
    }

    #[tokio::test]
    async fn reload_runtime_resources_refreshes_startup_plugin_summaries_and_warnings() {
        let _guard = env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let good_root = tempdir.path().join("packages/good");
        let fixed_root = tempdir.path().join("packages/fixed");
        let good_descriptor = good_root.join(cell_plugin_host::DISCOVERY_FILE_NAMES[0]);
        let fixed_descriptor = fixed_root.join(cell_plugin_host::DISCOVERY_FILE_NAMES[0]);

        write_executable_script(
            &good_root.join("plugin.sh"),
            &plugin_script(&plugin_registration_json(
                "good",
                "Good Plugin",
                &["good-command"],
                &["good-tool"],
                &["good-provider"],
                &["good-model"],
            )),
        );
        write_file(
            &good_descriptor,
            &plugin_descriptor_json("good", "Good Plugin"),
        );
        write_file(&fixed_descriptor, "{ not json }\n");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

        let mut package_manager = PackageManager::create(&cwd, Some(agent_dir.clone()));
        install_plugin_package(&mut package_manager, &good_root).expect("install good plugin");
        install_plugin_package(&mut package_manager, &fixed_root).expect("install fixed plugin");

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(ContextEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);

        let mut agent_session = create_agent_session(
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

        assert_eq!(
            agent_session
                .startup_resource_summary()
                .extension_summaries
                .len(),
            1
        );
        assert!(agent_session.startup_resource_summary().notices.iter().any(
            |notice| notice.section == StartupResourceNoticeSection::Extension
                && notice.path == fixed_descriptor
        ));

        write_executable_script(
            &fixed_root.join("plugin.sh"),
            &plugin_script(&plugin_registration_json(
                "fixed",
                "Fixed Plugin",
                &["fixed-command"],
                &["fixed-tool"],
                &["fixed-provider"],
                &["fixed-model"],
            )),
        );
        write_file(
            &fixed_descriptor,
            &plugin_descriptor_json("fixed", "Fixed Plugin"),
        );

        agent_session
            .reload_runtime_resources()
            .expect("reload runtime resources");

        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        let summary = agent_session.startup_resource_summary();
        assert_eq!(summary.extension_summaries.len(), 2);
        assert!(
            summary
                .extension_summaries
                .iter()
                .any(|line| line.contains("Good Plugin [good]"))
        );
        assert!(
            summary
                .extension_summaries
                .iter()
                .any(|line| line.contains("Fixed Plugin [fixed]"))
        );
        assert!(summary.notices.iter().all(|notice| {
            !(notice.section == StartupResourceNoticeSection::Extension
                && notice.path == fixed_descriptor
                && notice.message.contains("failed to parse plugin descriptor"))
        }));

        let diagnostics: RpcPluginRuntimeDiagnostics =
            agent_session.get_plugin_runtime_diagnostics();
        assert_eq!(diagnostics.plugins.len(), 2);
        assert!(diagnostics.warnings.is_empty());
    }

    #[tokio::test]
    async fn plugin_runtime_commands_appear_in_command_list_and_rewrite_prompt_text() {
        let tempdir = tempdir().expect("tempdir");
        let plugin_root = tempdir.path().join("packages/rewrite");
        let descriptor_path = plugin_root.join("cell-plugin-host.json");
        write_executable_script(
            &plugin_root.join("plugin.sh"),
            &plugin_runtime_script(
                &plugin_registration_json("rewrite", "Rewrite Plugin", &["rewrite"], &[], &[], &[]),
                r#"
request = json.loads(sys.stdin.readline())
assert request["type"] == "command_request"
print(json.dumps({
    "type": "command_response",
    "requestId": request["requestId"],
    "replacement": f"rewritten:{' '.join(request['args'])}",
}), flush=True)
"#,
            ),
        );
        write_file(
            &descriptor_path,
            &plugin_descriptor_json("rewrite", "Rewrite Plugin"),
        );

        let host = cell_plugin_host::PluginHost::new(cell_plugin_host::PluginHostConfig {
            discovery_roots: vec![plugin_root.clone()],
            workspace_root: Some(tempdir.path().to_path_buf()),
            handshake_timeout: std::time::Duration::from_millis(500),
            host_identity: cell_plugin_host::HostIdentity::new("cell-plugin-host", "0.52.12"),
        });
        let runtime = host.discover_and_load_runtime_plugins();
        assert!(
            runtime.summary.warnings.is_empty(),
            "warnings: {:#?}",
            runtime.summary.warnings
        );
        let startup_summary = runtime.summary.clone();
        let registry = runtime.registry.expect("runtime registry");

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(ContextEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::new(tempdir.path());
        let mut agent_session = AgentSession::new(
            providers,
            models,
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        agent_session.attach_runtime_resources(
            crate::runtime_resources::SessionRuntimeConfig {
                cwd: tempdir.path().to_path_buf(),
                system_prompt: None,
                append_system_prompt: None,
                explicit_skill_paths: Vec::new(),
                explicit_prompt_paths: Vec::new(),
                explicit_theme_paths: Vec::new(),
                no_skills: false,
                no_prompt_templates: false,
                no_themes: false,
            },
            Vec::new(),
            Vec::new(),
            Some(Arc::new(Mutex::new(registry))),
            startup_summary,
            StartupResourceSummary::default(),
        );

        let commands = agent_session.get_commands();
        assert!(commands.iter().any(|command| {
            command.name == "rewrite"
                && command.source == cell_protocol::RpcCommandSource::Extension
                && command.path.as_deref() == Some(descriptor_path.to_string_lossy().as_ref())
        }));

        let run = agent_session
            .prompt_text("/rewrite alpha beta".to_string())
            .await
            .expect("prompt");
        assert_eq!(
            run.assistant_message.content,
            vec![AssistantContentBlock::Text {
                text: "rewritten:alpha beta".to_string(),
                text_signature: None,
            }]
        );
    }

    #[tokio::test]
    async fn plugin_runtime_commands_preserve_quoted_arguments() {
        let tempdir = tempdir().expect("tempdir");
        let plugin_root = tempdir.path().join("packages/rewrite");
        let descriptor_path = plugin_root.join("cell-plugin-host.json");
        write_executable_script(
            &plugin_root.join("plugin.sh"),
            &plugin_runtime_script(
                &plugin_registration_json("rewrite", "Rewrite Plugin", &["rewrite"], &[], &[], &[]),
                r#"
request = json.loads(sys.stdin.readline())
assert request["type"] == "command_request"
print(json.dumps({
    "type": "command_response",
    "requestId": request["requestId"],
    "replacement": f"rewritten:{'|'.join(request['args'])}",
}), flush=True)
"#,
            ),
        );
        write_file(
            &descriptor_path,
            &plugin_descriptor_json("rewrite", "Rewrite Plugin"),
        );

        let host = cell_plugin_host::PluginHost::new(cell_plugin_host::PluginHostConfig {
            discovery_roots: vec![plugin_root.clone()],
            workspace_root: Some(tempdir.path().to_path_buf()),
            handshake_timeout: std::time::Duration::from_millis(500),
            host_identity: cell_plugin_host::HostIdentity::new("cell-plugin-host", "0.52.12"),
        });
        let runtime = host.discover_and_load_runtime_plugins();
        assert!(
            runtime.summary.warnings.is_empty(),
            "warnings: {:#?}",
            runtime.summary.warnings
        );
        let startup_summary = runtime.summary.clone();
        let registry = runtime.registry.expect("runtime registry");

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(ContextEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let models = ModelRegistry::new(auth, None);
        let session = SessionManager::in_memory(tempdir.path());
        let tools = ToolSet::new(tempdir.path());
        let mut agent_session = AgentSession::new(
            providers,
            models,
            session,
            tools,
            model(),
            "off",
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        agent_session.attach_runtime_resources(
            crate::runtime_resources::SessionRuntimeConfig {
                cwd: tempdir.path().to_path_buf(),
                system_prompt: None,
                append_system_prompt: None,
                explicit_skill_paths: Vec::new(),
                explicit_prompt_paths: Vec::new(),
                explicit_theme_paths: Vec::new(),
                no_skills: false,
                no_prompt_templates: false,
                no_themes: false,
            },
            Vec::new(),
            Vec::new(),
            Some(Arc::new(Mutex::new(registry))),
            startup_summary,
            StartupResourceSummary::default(),
        );

        let run = agent_session
            .prompt_text(r#"/rewrite "alpha beta" gamma"#.to_string())
            .await
            .expect("prompt");
        assert_eq!(
            run.assistant_message.content,
            vec![AssistantContentBlock::Text {
                text: "rewritten:alpha beta|gamma".to_string(),
                text_signature: None,
            }]
        );
    }

    #[tokio::test]
    async fn plugin_runtime_hook_warnings_cover_session_prompt_command_and_tool_events() {
        let _guard = env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().to_path_buf();
        let plugin_root = cwd.join("packages/hooks");
        let descriptor_path = plugin_root.join("cell-plugin-host.json");
        let hook_log = cwd.join("hook-events.log");
        let hook_handler = format!(
            r#"
hook_log = r'''{}'''
while True:
    line = sys.stdin.readline()
    if not line:
        break
    request = json.loads(line)
    if request["type"] == "hook_request":
        with open(hook_log, "a", encoding="utf-8") as handle:
            handle.write(request["context"]["event"] + "\n")
        print(json.dumps({{
            "type": "hook_response",
            "requestId": request["requestId"],
            "outcome": "continue",
        }}), flush=True)
    elif request["type"] == "command_request":
        print(json.dumps({{
            "type": "command_response",
            "requestId": request["requestId"],
            "replacement": f"rewritten:{{' '.join(request['args'])}}",
        }}), flush=True)
    elif request["type"] == "tool_request":
        print(json.dumps({{
            "type": "tool_response",
            "requestId": request["requestId"],
            "content": [{{"type": "text", "text": f"tool:{{request['arguments']['value']}}"}}],
            "details": {{"echo": request["arguments"]}},
            "isError": False,
        }}), flush=True)
    else:
        sys.stderr.write(f"unexpected request type: {{request['type']}}\n")
        sys.exit(42)
"#,
            hook_log.display()
        );
        write_executable_script(
            &plugin_root.join("plugin.sh"),
            &plugin_runtime_script(
                &plugin_registration_json_with_hooks(
                    "hooks",
                    "Hooks Plugin",
                    &["rewrite"],
                    &["plugin-write"],
                    &[],
                    &[],
                    &[
                        (LifecycleEventV1::HostStartup, "host-startup", 0),
                        (LifecycleEventV1::PluginLoaded, "plugin-loaded", 0),
                        (LifecycleEventV1::SessionStarted, "session-started", 0),
                        (LifecycleEventV1::PromptStarted, "prompt-started", 0),
                        (LifecycleEventV1::PromptFinished, "prompt-finished", 0),
                        (LifecycleEventV1::CommandStarted, "command-started", 0),
                        (LifecycleEventV1::CommandFinished, "command-finished", 0),
                        (LifecycleEventV1::ToolStarted, "tool-started", 0),
                        (LifecycleEventV1::ToolFinished, "tool-finished", 0),
                        (LifecycleEventV1::SessionEnded, "session-ended", 0),
                    ],
                ),
                &hook_handler,
            ),
        );
        write_file(
            &descriptor_path,
            &plugin_descriptor_json("hooks", "Hooks Plugin"),
        );

        let agent_dir = tempdir.path().join("agent");
        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };
        let mut package_manager = PackageManager::create(&cwd, Some(agent_dir.clone()));
        install_plugin_package(&mut package_manager, &plugin_root).expect("install hooks plugin");

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(HookEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);

        let mut agent_session = create_agent_session(
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
                tools: None,
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

        let diagnostics: RpcPluginRuntimeDiagnostics =
            agent_session.get_plugin_runtime_diagnostics();
        assert_eq!(diagnostics.plugins.len(), 1);
        assert!(diagnostics.warnings.is_empty());

        let rewrite_run = agent_session
            .prompt_text("/rewrite alpha beta".to_string())
            .await
            .expect("rewrite prompt");
        assert_eq!(
            rewrite_run.assistant_message.content,
            vec![AssistantContentBlock::Text {
                text: "rewritten:alpha beta".to_string(),
                text_signature: None,
            }]
        );

        let diagnostics: RpcPluginRuntimeDiagnostics =
            agent_session.get_plugin_runtime_diagnostics();
        assert!(diagnostics.warnings.is_empty());

        let tool_run = agent_session
            .prompt_text("call-plugin-tool".to_string())
            .await
            .expect("tool prompt");
        assert!(!tool_run.tool_results.is_empty());

        let diagnostics: RpcPluginRuntimeDiagnostics =
            agent_session.get_plugin_runtime_diagnostics();
        assert!(diagnostics.warnings.is_empty());

        drop(agent_session);
        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }
        let hook_events = std::fs::read_to_string(&hook_log)
            .expect("read hook log")
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            hook_events,
            vec![
                "hostStartup",
                "pluginLoaded",
                "sessionStarted",
                "commandStarted",
                "commandFinished",
                "promptStarted",
                "promptFinished",
                "promptStarted",
                "toolStarted",
                "toolFinished",
                "promptFinished",
                "sessionEnded",
            ]
        );
    }

    #[test]
    fn converts_agent_events_to_rpc_events() {
        let event = AgentEvent::AgentStart;
        assert!(matches!(
            rpc_event_from_agent_event(event),
            RpcEvent::AgentStart
        ));
    }
}
