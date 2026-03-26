use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cell_plugin_protocol::PluginContentBlock;
use cell_plugins::{
    CommandRegistrationV1, FlagRegistrationV1, LifecycleHookContextV1,
    LifecycleHookOutcomeV1, LifecycleHookRegistrationV1, ModelRegistrationV1, PluginManifestV1,
    ProviderRegistrationV1, ToolRegistrationV1,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::descriptor::discover_plugins_with_warnings;
use crate::descriptor::{DiscoveredPlugin, discover_plugins};
use crate::error::HostError;
use crate::process::{PluginSession, PluginSessionConfig};
use crate::protocol::{CapabilityCounts, CapabilityIndex, HostIdentity};
use crate::registry::{MergedPluginRegistry, merge_registered_plugins};

#[derive(Clone, Debug)]
pub struct PluginHostConfig {
    pub discovery_roots: Vec<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    pub handshake_timeout: Duration,
    pub host_identity: HostIdentity,
}

impl Default for PluginHostConfig {
    fn default() -> Self {
        Self {
            discovery_roots: vec![PathBuf::from(".")],
            workspace_root: None,
            handshake_timeout: Duration::from_secs(5),
            host_identity: HostIdentity::new("cell-plugin-host", "0.52.12"),
        }
    }
}

pub struct PluginHost {
    config: PluginHostConfig,
}

pub struct ActivePluginRegistry {
    merged: MergedPluginRegistry,
    plugins: BTreeMap<String, RegisteredPlugin>,
    request_timeout: Duration,
    next_request_id: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookDispatchReport {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<PluginHostWarning>,
    pub stopped: bool,
}

pub struct LoadedPluginRuntime {
    pub registry: Option<ActivePluginRegistry>,
    pub summary: PluginStartupSummary,
}

impl PluginHost {
    pub fn new(config: PluginHostConfig) -> Self {
        Self { config }
    }

    pub fn discover(&self) -> Result<Vec<DiscoveredPlugin>, HostError> {
        discover_plugins(&self.config.discovery_roots)
    }

    pub fn launch(&self, discovered: DiscoveredPlugin) -> Result<PluginSession, HostError> {
        PluginSession::launch(
            discovered,
            PluginSessionConfig {
                host_identity: self.config.host_identity.clone(),
                workspace_root: self.config.workspace_root.clone(),
                handshake_timeout: self.config.handshake_timeout,
            },
        )
    }

    pub fn launch_and_register(
        &self,
        discovered: DiscoveredPlugin,
    ) -> Result<RegisteredPlugin, HostError> {
        let session = self.launch(discovered)?;
        session.handshake()
    }

    pub fn discover_and_register(&self) -> Result<Vec<RegisteredPlugin>, HostError> {
        self.discover()?
            .into_iter()
            .map(|plugin| self.launch_and_register(plugin))
            .collect()
    }

    pub fn discover_and_register_startup_plugins(&self) -> PluginStartupSummary {
        self.discover_and_load_runtime_plugins().summary
    }

    pub fn discover_and_load_runtime_plugins(&self) -> LoadedPluginRuntime {
        let (discovered, mut warnings) =
            discover_plugins_with_warnings(&self.config.discovery_roots);
        let mut accepted = Vec::new();
        let mut command_names = BTreeMap::<String, String>::new();
        let mut tool_names = BTreeMap::<String, String>::new();
        let mut provider_ids = BTreeMap::<String, String>::new();
        let mut model_ids = BTreeMap::<String, String>::new();
        let mut plugin_ids = BTreeSet::<String>::new();

        for plugin in discovered {
            let plugin_id = plugin.descriptor.id.clone();
            let plugin_name = plugin.descriptor.name.clone();
            let descriptor_path = plugin.descriptor_path.clone();

            match self.launch_and_register(plugin) {
                Ok(registered) => {
                    if !plugin_ids.insert(registered.manifest.plugin.id.clone()) {
                        warnings.push(PluginHostWarning {
                            path: descriptor_path,
                            plugin_id: Some(plugin_id),
                            plugin_name: Some(plugin_name),
                            message: format!(
                                "duplicate plugin id `{}` across startup plugins",
                                registered.manifest.plugin.id
                            ),
                        });
                        continue;
                    }

                    if let Some(message) = duplicate_runtime_registration_message(
                        &registered,
                        &command_names,
                        &tool_names,
                        &provider_ids,
                        &model_ids,
                    ) {
                        warnings.push(PluginHostWarning {
                            path: descriptor_path,
                            plugin_id: Some(plugin_id),
                            plugin_name: Some(plugin_name),
                            message,
                        });
                        continue;
                    }

                    remember_runtime_registrations(
                        &registered,
                        &mut command_names,
                        &mut tool_names,
                        &mut provider_ids,
                        &mut model_ids,
                    );
                    accepted.push(registered);
                }
                Err(error) => warnings.push(PluginHostWarning {
                    path: descriptor_path,
                    plugin_id: Some(plugin_id),
                    plugin_name: Some(plugin_name),
                    message: error.to_string(),
                }),
            }
        }

        let summaries = accepted.iter().map(RegisteredPlugin::summary).collect::<Vec<_>>();
        let registry = if accepted.is_empty() {
            None
        } else {
            Some(
                ActivePluginRegistry::from_registered_plugins(
                    accepted,
                    self.config.handshake_timeout,
                )
                .expect("deduped runtime plugins"),
            )
        };

        LoadedPluginRuntime {
            registry,
            summary: PluginStartupSummary { summaries, warnings },
        }
    }

    pub fn discover_and_merge(&self) -> Result<MergedPluginRegistry, HostError> {
        MergedPluginRegistry::from_registered_plugins(self.discover_and_register()?)
    }

    pub fn merge_registered_plugins(
        &self,
        plugins: Vec<RegisteredPlugin>,
    ) -> Result<MergedPluginRegistry, HostError> {
        MergedPluginRegistry::from_registered_plugins(plugins)
    }
}

impl ActivePluginRegistry {
    pub fn from_registered_plugins(
        plugins: Vec<RegisteredPlugin>,
        request_timeout: Duration,
    ) -> Result<Self, HostError> {
        let merged = merge_registered_plugins(&plugins)?;
        let plugins = plugins
            .into_iter()
            .map(|plugin| (plugin.manifest.plugin.id.clone(), plugin))
            .collect();
        Ok(Self {
            merged,
            plugins,
            request_timeout,
            next_request_id: 0,
        })
    }

    pub fn merged_registry(&self) -> &MergedPluginRegistry {
        &self.merged
    }

    pub fn invoke_command(
        &mut self,
        name: &str,
        args: &[String],
        cwd: &Path,
        session_id: Option<&str>,
        raw_input: Option<&str>,
    ) -> Result<String, HostError> {
        let owner = self
            .merged
            .commands
            .get(name)
            .ok_or_else(|| HostError::Protocol {
                plugin_id: String::new(),
                message: format!("unknown plugin command `{name}`"),
            })?
            .source
            .plugin_id
            .clone();
        let request_id = self.next_request_id("command");
        let plugin = self.plugins.get_mut(&owner).ok_or_else(|| HostError::Protocol {
            plugin_id: owner.clone(),
            message: format!("registered plugin `{owner}` is no longer available"),
        })?;
        plugin.session.invoke_command(
            request_id,
            name.to_string(),
            args.to_vec(),
            cwd.to_path_buf(),
            session_id.map(ToOwned::to_owned),
            raw_input.map(ToOwned::to_owned),
            self.request_timeout,
        )
    }

    pub fn invoke_tool(
        &mut self,
        tool_call_id: &str,
        name: &str,
        arguments: Value,
        cwd: &Path,
        session_id: Option<&str>,
    ) -> Result<(Vec<PluginContentBlock>, Option<Value>, bool), HostError> {
        let owner = self
            .merged
            .tools
            .get(name)
            .ok_or_else(|| HostError::Protocol {
                plugin_id: String::new(),
                message: format!("unknown plugin tool `{name}`"),
            })?
            .source
            .plugin_id
            .clone();
        let request_id = self.next_request_id("tool");
        let plugin = self.plugins.get_mut(&owner).ok_or_else(|| HostError::Protocol {
            plugin_id: owner.clone(),
            message: format!("registered plugin `{owner}` is no longer available"),
        })?;
        plugin.session.invoke_tool(
            request_id,
            tool_call_id.to_string(),
            name.to_string(),
            arguments,
            cwd.to_path_buf(),
            session_id.map(ToOwned::to_owned),
            self.request_timeout,
        )
    }

    pub fn dispatch_hooks(&mut self, context: LifecycleHookContextV1) -> HookDispatchReport {
        let mut warnings = Vec::new();
        let event = context.event.clone();
        let hooks = self
            .merged
            .hooks
            .iter()
            .filter(|hook| hook.registration.event == event)
            .cloned()
            .collect::<Vec<_>>();

        for hook in hooks {
            let plugin_id = hook.source.plugin_id.clone();
            let plugin_name = hook.source.plugin_name.clone();
            let descriptor_path = hook.source.descriptor_path.clone();
            let hook_name = hook.registration.name.clone();
            let request_id = self.next_request_id("hook");

            let Some(plugin) = self.plugins.get_mut(&plugin_id) else {
                warnings.push(hook_warning(
                    descriptor_path,
                    Some(plugin_id.clone()),
                    Some(plugin_name),
                    format!("registered plugin `{plugin_id}` is no longer available"),
                ));
                continue;
            };

            match plugin.session.invoke_hook(
                request_id,
                hook_name.clone(),
                context.clone(),
                self.request_timeout,
            ) {
                Ok(LifecycleHookOutcomeV1::Continue) => continue,
                Ok(LifecycleHookOutcomeV1::StopPropagation) => {
                    return HookDispatchReport {
                        warnings,
                        stopped: true,
                    };
                }
                Err(error) => warnings.push(hook_warning(
                    descriptor_path,
                    Some(plugin_id),
                    Some(plugin_name),
                    format!("hook `{hook_name}` failed: {error}"),
                )),
            }
        }

        HookDispatchReport {
            warnings,
            stopped: false,
        }
    }

    fn next_request_id(&mut self, prefix: &str) -> String {
        self.next_request_id += 1;
        format!("{prefix}-{}", self.next_request_id)
    }
}

fn duplicate_runtime_registration_message(
    plugin: &RegisteredPlugin,
    command_names: &BTreeMap<String, String>,
    tool_names: &BTreeMap<String, String>,
    provider_ids: &BTreeMap<String, String>,
    model_ids: &BTreeMap<String, String>,
) -> Option<String> {
    for name in plugin.capabilities.commands.keys() {
        if let Some(existing) = command_names.get(name) {
            return Some(format!(
                "duplicate capability registration for command `{name}` across plugins `{existing}` and `{}`",
                plugin.manifest.plugin.id
            ));
        }
    }
    for name in plugin.capabilities.tools.keys() {
        if let Some(existing) = tool_names.get(name) {
            return Some(format!(
                "duplicate capability registration for tool `{name}` across plugins `{existing}` and `{}`",
                plugin.manifest.plugin.id
            ));
        }
    }
    for provider_id in plugin.capabilities.providers.keys() {
        if let Some(existing) = provider_ids.get(provider_id) {
            return Some(format!(
                "duplicate capability registration for provider `{provider_id}` across plugins `{existing}` and `{}`",
                plugin.manifest.plugin.id
            ));
        }
    }
    for model_id in plugin.capabilities.models.keys() {
        if let Some(existing) = model_ids.get(model_id) {
            return Some(format!(
                "duplicate capability registration for model `{model_id}` across plugins `{existing}` and `{}`",
                plugin.manifest.plugin.id
            ));
        }
    }
    None
}

fn remember_runtime_registrations(
    plugin: &RegisteredPlugin,
    command_names: &mut BTreeMap<String, String>,
    tool_names: &mut BTreeMap<String, String>,
    provider_ids: &mut BTreeMap<String, String>,
    model_ids: &mut BTreeMap<String, String>,
) {
    for name in plugin.capabilities.commands.keys() {
        command_names.insert(name.clone(), plugin.manifest.plugin.id.clone());
    }
    for name in plugin.capabilities.tools.keys() {
        tool_names.insert(name.clone(), plugin.manifest.plugin.id.clone());
    }
    for provider_id in plugin.capabilities.providers.keys() {
        provider_ids.insert(provider_id.clone(), plugin.manifest.plugin.id.clone());
    }
    for model_id in plugin.capabilities.models.keys() {
        model_ids.insert(model_id.clone(), plugin.manifest.plugin.id.clone());
    }
}

fn hook_warning(
    path: PathBuf,
    plugin_id: Option<String>,
    plugin_name: Option<String>,
    message: String,
) -> PluginHostWarning {
    PluginHostWarning {
        path,
        plugin_id,
        plugin_name,
        message,
    }
}

pub struct RegisteredPlugin {
    pub(crate) session: PluginSession,
    pub manifest: PluginManifestV1,
    pub capabilities: CapabilityIndex,
}

impl std::fmt::Debug for RegisteredPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredPlugin")
            .field("descriptor", &self.descriptor())
            .field("manifest", &self.manifest)
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

impl RegisteredPlugin {
    pub fn descriptor(&self) -> &DiscoveredPlugin {
        self.session.descriptor()
    }

    pub fn summary(&self) -> RegisteredPluginSummary {
        RegisteredPluginSummary {
            descriptor_path: self.descriptor().descriptor_path.clone(),
            plugin_id: self.manifest.plugin.id.clone(),
            plugin_name: self.manifest.plugin.name.clone(),
            manifest_version: self.manifest.manifest_version,
            capabilities: self.capabilities.counts(),
            commands: self
                .capabilities
                .commands
                .values()
                .cloned()
                .collect::<Vec<CommandRegistrationV1>>(),
            tools: self
                .capabilities
                .tools
                .values()
                .cloned()
                .collect::<Vec<ToolRegistrationV1>>(),
            flags: self
                .capabilities
                .flags
                .values()
                .cloned()
                .collect::<Vec<FlagRegistrationV1>>(),
            hooks: self
                .capabilities
                .hooks
                .values()
                .cloned()
                .collect::<Vec<LifecycleHookRegistrationV1>>(),
            providers: self
                .capabilities
                .providers
                .values()
                .cloned()
                .collect::<Vec<ProviderRegistrationV1>>(),
            models: self
                .capabilities
                .models
                .values()
                .cloned()
                .collect::<Vec<ModelRegistrationV1>>(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredPluginSummary {
    pub descriptor_path: PathBuf,
    pub plugin_id: String,
    pub plugin_name: String,
    pub manifest_version: u16,
    pub capabilities: CapabilityCounts,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub commands: Vec<CommandRegistrationV1>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<ToolRegistrationV1>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub flags: Vec<FlagRegistrationV1>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hooks: Vec<LifecycleHookRegistrationV1>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub providers: Vec<ProviderRegistrationV1>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub models: Vec<ModelRegistrationV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginHostWarning {
    pub path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub plugin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub plugin_name: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginStartupSummary {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub summaries: Vec<RegisteredPluginSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<PluginHostWarning>,
}
