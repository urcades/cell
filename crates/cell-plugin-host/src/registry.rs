use std::collections::BTreeMap;
use std::path::PathBuf;

use cell_plugins::{
    CommandRegistrationV1, FlagRegistrationV1, LifecycleEventV1, LifecycleHookRegistrationV1,
    ModelRegistrationV1, ProviderRegistrationV1, ToolRegistrationV1,
};
use serde::{Deserialize, Serialize};

use crate::HostError;
use crate::host::{RegisteredPlugin, RegisteredPluginSummary};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSource {
    pub plugin_id: String,
    pub plugin_name: String,
    pub descriptor_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedCapability<T> {
    pub source: PluginSource,
    pub registration: T,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedPluginRecord {
    pub source: PluginSource,
    pub summary: RegisteredPluginSummary,
}

pub type MergedCommandRegistration = OwnedCapability<CommandRegistrationV1>;
pub type MergedToolRegistration = OwnedCapability<ToolRegistrationV1>;
pub type MergedFlagRegistration = OwnedCapability<FlagRegistrationV1>;
pub type MergedHookRegistration = OwnedCapability<LifecycleHookRegistrationV1>;
pub type MergedProviderRegistration = OwnedCapability<ProviderRegistrationV1>;
pub type MergedModelRegistration = OwnedCapability<ModelRegistrationV1>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedPluginRegistry {
    pub plugins: Vec<MergedPluginRecord>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub commands: BTreeMap<String, MergedCommandRegistration>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub tools: BTreeMap<String, MergedToolRegistration>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub flags: BTreeMap<String, MergedFlagRegistration>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hooks: Vec<MergedHookRegistration>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub providers: BTreeMap<String, MergedProviderRegistration>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub models: BTreeMap<String, MergedModelRegistration>,
}

impl MergedPluginRegistry {
    pub fn from_registered_plugins(plugins: Vec<RegisteredPlugin>) -> Result<Self, HostError> {
        merge_registered_plugins(&plugins)
    }
}

pub fn merge_registered_plugins(
    plugins: &[RegisteredPlugin],
) -> Result<MergedPluginRegistry, HostError> {
    let mut plugin_records = plugins
        .iter()
        .map(|plugin| MergedPluginRecord {
            source: plugin_source(plugin),
            summary: plugin.summary(),
        })
        .collect::<Vec<_>>();
    plugin_records.sort_by(|left, right| {
        left.source
            .plugin_id
            .cmp(&right.source.plugin_id)
            .then_with(|| {
                left.source
                    .descriptor_path
                    .cmp(&right.source.descriptor_path)
            })
    });

    let mut commands = BTreeMap::new();
    let mut tools = BTreeMap::new();
    let mut flags = BTreeMap::new();
    let mut providers = BTreeMap::new();
    let mut models = BTreeMap::new();
    let mut hooks = Vec::new();

    for plugin in plugins {
        let source = plugin_source(plugin);

        for (name, command) in &plugin.capabilities.commands {
            insert_capability(
                &mut commands,
                "command",
                name,
                OwnedCapability {
                    source: source.clone(),
                    registration: command.clone(),
                },
            )?;
        }
        for (name, tool) in &plugin.capabilities.tools {
            insert_capability(
                &mut tools,
                "tool",
                name,
                OwnedCapability {
                    source: source.clone(),
                    registration: tool.clone(),
                },
            )?;
        }
        for (name, flag) in &plugin.capabilities.flags {
            insert_capability(
                &mut flags,
                "flag",
                name,
                OwnedCapability {
                    source: source.clone(),
                    registration: flag.clone(),
                },
            )?;
        }
        for hook in plugin.capabilities.hooks.values() {
            hooks.push(OwnedCapability {
                source: source.clone(),
                registration: hook.clone(),
            });
        }
        for (provider_id, provider) in &plugin.capabilities.providers {
            insert_capability(
                &mut providers,
                "provider",
                provider_id,
                OwnedCapability {
                    source: source.clone(),
                    registration: provider.clone(),
                },
            )?;
        }
        for (model_id, model) in &plugin.capabilities.models {
            insert_capability(
                &mut models,
                "model",
                model_id,
                OwnedCapability {
                    source: source.clone(),
                    registration: model.clone(),
                },
            )?;
        }
    }

    hooks.sort_by(|left, right| {
        hook_event_key(&left.registration.event)
            .cmp(hook_event_key(&right.registration.event))
            .then_with(|| right.registration.priority.cmp(&left.registration.priority))
            .then_with(|| left.source.plugin_id.cmp(&right.source.plugin_id))
            .then_with(|| left.source.plugin_name.cmp(&right.source.plugin_name))
            .then_with(|| left.registration.name.cmp(&right.registration.name))
            .then_with(|| {
                left.source
                    .descriptor_path
                    .cmp(&right.source.descriptor_path)
            })
    });

    Ok(MergedPluginRegistry {
        plugins: plugin_records,
        commands,
        tools,
        flags,
        hooks,
        providers,
        models,
    })
}

fn insert_capability<T>(
    map: &mut BTreeMap<String, OwnedCapability<T>>,
    kind: &'static str,
    name: &str,
    capability: OwnedCapability<T>,
) -> Result<(), HostError> {
    if let Some(existing) = map.get(name) {
        return Err(HostError::DuplicateMergedCapability {
            kind,
            name: name.to_string(),
            first_plugin_id: existing.source.plugin_id.clone(),
            second_plugin_id: capability.source.plugin_id,
        });
    }
    map.insert(name.to_string(), capability);
    Ok(())
}

fn plugin_source(plugin: &RegisteredPlugin) -> PluginSource {
    PluginSource {
        plugin_id: plugin.manifest.plugin.id.clone(),
        plugin_name: plugin.manifest.plugin.name.clone(),
        descriptor_path: plugin.descriptor().descriptor_path.clone(),
    }
}

fn hook_event_key(event: &LifecycleEventV1) -> &'static str {
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
