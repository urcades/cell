use std::collections::BTreeMap;

pub use pi_rust_plugin_protocol::{
    HostIdentity, HostMessage, PLUGIN_PROTOCOL_VERSION_V1 as HOST_PROTOCOL_VERSION_V1, PluginMessage,
};
use pi_rust_plugins::{
    CommandRegistrationV1, FlagRegistrationV1, LifecycleHookRegistrationV1, ModelRegistrationV1,
    PluginManifestV1, ProviderRegistrationV1, ToolRegistrationV1,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityCounts {
    pub commands: usize,
    pub tools: usize,
    pub flags: usize,
    pub hooks: usize,
    pub providers: usize,
    pub models: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CapabilityIndex {
    pub commands: BTreeMap<String, CommandRegistrationV1>,
    pub tools: BTreeMap<String, ToolRegistrationV1>,
    pub flags: BTreeMap<String, FlagRegistrationV1>,
    pub hooks: BTreeMap<String, LifecycleHookRegistrationV1>,
    pub providers: BTreeMap<String, ProviderRegistrationV1>,
    pub models: BTreeMap<String, ModelRegistrationV1>,
}

impl CapabilityIndex {
    pub fn from_manifest(
        plugin_id: &str,
        manifest: &PluginManifestV1,
    ) -> Result<Self, crate::HostError> {
        let commands = collect_by_name(plugin_id, "command", &manifest.commands)?;
        let tools = collect_by_name(plugin_id, "tool", &manifest.tools)?;
        let flags = collect_by_name(plugin_id, "flag", &manifest.flags)?;
        let hooks = collect_by_hook_name(plugin_id, &manifest.hooks)?;
        let providers = collect_by_provider_id(plugin_id, &manifest.providers)?;
        let models = collect_by_model_id(plugin_id, &manifest.models)?;

        Ok(Self {
            commands,
            tools,
            flags,
            hooks,
            providers,
            models,
        })
    }

    pub fn counts(&self) -> CapabilityCounts {
        CapabilityCounts {
            commands: self.commands.len(),
            tools: self.tools.len(),
            flags: self.flags.len(),
            hooks: self.hooks.len(),
            providers: self.providers.len(),
            models: self.models.len(),
        }
    }

    pub fn command_names(&self) -> Vec<String> {
        self.commands.keys().cloned().collect()
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn provider_ids(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    pub fn model_ids(&self) -> Vec<String> {
        self.models.keys().cloned().collect()
    }
}

fn collect_by_name<T>(
    plugin_id: &str,
    kind: &'static str,
    values: &[T],
) -> Result<BTreeMap<String, T>, crate::HostError>
where
    T: Clone + NamedCapability,
{
    let mut map = BTreeMap::new();
    for value in values {
        let name = value.capability_name().to_string();
        if map.insert(name.clone(), value.clone()).is_some() {
            return Err(crate::HostError::DuplicateCapability {
                plugin_id: plugin_id.to_string(),
                kind,
                name,
            });
        }
    }
    Ok(map)
}

fn collect_by_hook_name(
    plugin_id: &str,
    values: &[LifecycleHookRegistrationV1],
) -> Result<BTreeMap<String, LifecycleHookRegistrationV1>, crate::HostError> {
    collect_by_name(plugin_id, "hook", values)
}

fn collect_by_provider_id(
    plugin_id: &str,
    values: &[ProviderRegistrationV1],
) -> Result<BTreeMap<String, ProviderRegistrationV1>, crate::HostError> {
    let mut map = BTreeMap::new();
    for value in values {
        if map
            .insert(value.provider_id.clone(), value.clone())
            .is_some()
        {
            return Err(crate::HostError::DuplicateCapability {
                plugin_id: plugin_id.to_string(),
                kind: "provider",
                name: value.provider_id.clone(),
            });
        }
    }
    Ok(map)
}

fn collect_by_model_id(
    plugin_id: &str,
    values: &[ModelRegistrationV1],
) -> Result<BTreeMap<String, ModelRegistrationV1>, crate::HostError> {
    let mut map = BTreeMap::new();
    for value in values {
        if map.insert(value.model_id.clone(), value.clone()).is_some() {
            return Err(crate::HostError::DuplicateCapability {
                plugin_id: plugin_id.to_string(),
                kind: "model",
                name: value.model_id.clone(),
            });
        }
    }
    Ok(map)
}

trait NamedCapability {
    fn capability_name(&self) -> &str;
}

impl NamedCapability for CommandRegistrationV1 {
    fn capability_name(&self) -> &str {
        &self.name
    }
}

impl NamedCapability for ToolRegistrationV1 {
    fn capability_name(&self) -> &str {
        &self.name
    }
}

impl NamedCapability for FlagRegistrationV1 {
    fn capability_name(&self) -> &str {
        &self.name
    }
}

impl NamedCapability for LifecycleHookRegistrationV1 {
    fn capability_name(&self) -> &str {
        &self.name
    }
}
