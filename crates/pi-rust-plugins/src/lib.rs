//! Pure-Rust plugin v1 contract.
//!
//! This crate is intentionally declarative. It defines the manifest and the
//! registration interfaces for Rust-native plugins without implementing any
//! runtime loader, dynamic dispatch layer, or foreign-language bridge.
//!
//! v1 keeps the contract narrow:
//! - a plugin describes itself with a manifest
//! - a plugin can register commands, tools, flags, lifecycle hooks, providers,
//!   and models
//! - lifecycle hooks are synchronous, host-driven callbacks
//!
//! The host can use these types to validate and inspect plugin capability
//! metadata today, while loading and execution remain a later concern.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const PLUGIN_MANIFEST_VERSION_V1: u16 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifestV1 {
    pub manifest_version: u16,
    pub plugin: PluginIdentityV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandRegistrationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolRegistrationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<FlagRegistrationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<LifecycleHookRegistrationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderRegistrationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelRegistrationV1>,
}

impl PluginManifestV1 {
    pub fn new(plugin: PluginIdentityV1) -> Self {
        Self {
            manifest_version: PLUGIN_MANIFEST_VERSION_V1,
            plugin,
            commands: Vec::new(),
            tools: Vec::new(),
            flags: Vec::new(),
            hooks: Vec::new(),
            providers: Vec::new(),
            models: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginIdentityV1 {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRegistrationV1 {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<ParameterRegistrationV1>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRegistrationV1 {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<ParameterRegistrationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ValueKindV1>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagRegistrationV1 {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: FlagKindV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterRegistrationV1 {
    pub name: String,
    pub kind: ValueKindV1,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueKindV1 {
    String,
    Boolean,
    Integer,
    Number,
    Path,
    Json,
    StringList,
    StringMap,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FlagKindV1 {
    Boolean,
    Value { kind: ValueKindV1 },
    Choice { choices: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleHookRegistrationV1 {
    pub event: LifecycleEventV1,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: i16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LifecycleEventV1 {
    PluginLoaded,
    PluginEnabled,
    PluginDisabled,
    HostStartup,
    HostShutdown,
    SessionStarted,
    SessionEnded,
    PromptStarted,
    PromptFinished,
    CommandStarted,
    CommandFinished,
    ToolStarted,
    ToolFinished,
    ProviderRegistered,
    ModelRegistered,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRegistrationV1 {
    pub provider_id: String,
    pub name: String,
    pub api: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub auth: ProviderAuthV1,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ProviderAuthV1 {
    #[default]
    None,
    ApiKeyHeader {
        header: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
    },
    BearerToken,
    OAuth,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRegistrationV1 {
    pub provider_id: String,
    pub model_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modalities: Vec<ModelInputKindV1>,
    #[serde(default)]
    pub reasoning: bool,
    pub context_window: u32,
    pub max_output_tokens: u32,
    #[serde(default)]
    pub default: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelInputKindV1 {
    Text,
    Image,
    Audio,
    File,
    ToolResult,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleHookContextV1 {
    pub event: LifecycleEventV1,
    pub plugin_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LifecycleHookOutcomeV1 {
    Continue,
    StopPropagation,
}

pub trait PluginDefinitionV1 {
    fn manifest(&self) -> &PluginManifestV1;

    fn register(&self, registrar: &mut dyn PluginRegistrarV1);
}

pub trait PluginRegistrarV1 {
    fn register_command(&mut self, command: CommandRegistrationV1);

    fn register_tool(&mut self, tool: ToolRegistrationV1);

    fn register_flag(&mut self, flag: FlagRegistrationV1);

    fn register_lifecycle_hook(&mut self, hook: LifecycleHookRegistrationV1);

    fn register_provider(&mut self, provider: ProviderRegistrationV1);

    fn register_model(&mut self, model: ModelRegistrationV1);
}

pub trait LifecycleHookHandlerV1 {
    fn handle(&self, context: &LifecycleHookContextV1) -> LifecycleHookOutcomeV1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Collector {
        commands: Vec<CommandRegistrationV1>,
        tools: Vec<ToolRegistrationV1>,
        flags: Vec<FlagRegistrationV1>,
        hooks: Vec<LifecycleHookRegistrationV1>,
        providers: Vec<ProviderRegistrationV1>,
        models: Vec<ModelRegistrationV1>,
    }

    impl PluginRegistrarV1 for Collector {
        fn register_command(&mut self, command: CommandRegistrationV1) {
            self.commands.push(command);
        }

        fn register_tool(&mut self, tool: ToolRegistrationV1) {
            self.tools.push(tool);
        }

        fn register_flag(&mut self, flag: FlagRegistrationV1) {
            self.flags.push(flag);
        }

        fn register_lifecycle_hook(&mut self, hook: LifecycleHookRegistrationV1) {
            self.hooks.push(hook);
        }

        fn register_provider(&mut self, provider: ProviderRegistrationV1) {
            self.providers.push(provider);
        }

        fn register_model(&mut self, model: ModelRegistrationV1) {
            self.models.push(model);
        }
    }

    struct ExamplePlugin {
        manifest: PluginManifestV1,
    }

    impl PluginDefinitionV1 for ExamplePlugin {
        fn manifest(&self) -> &PluginManifestV1 {
            &self.manifest
        }

        fn register(&self, registrar: &mut dyn PluginRegistrarV1) {
            registrar.register_command(CommandRegistrationV1 {
                name: "hello".to_string(),
                description: Some("Say hello".to_string()),
                aliases: vec!["hi".to_string()],
                parameters: vec![ParameterRegistrationV1 {
                    name: "name".to_string(),
                    kind: ValueKindV1::String,
                    required: false,
                    description: Some("Who to greet".to_string()),
                    default_value: None,
                }],
                hidden: false,
            });
            registrar.register_tool(ToolRegistrationV1 {
                name: "echo".to_string(),
                description: Some("Echo text".to_string()),
                aliases: Vec::new(),
                parameters: vec![ParameterRegistrationV1 {
                    name: "text".to_string(),
                    kind: ValueKindV1::String,
                    required: true,
                    description: None,
                    default_value: None,
                }],
                output: Some(ValueKindV1::String),
                hidden: false,
            });
            registrar.register_flag(FlagRegistrationV1 {
                name: "verbose".to_string(),
                description: Some("Enable verbose output".to_string()),
                kind: FlagKindV1::Boolean,
                aliases: vec!["v".to_string()],
                default_value: Some(serde_json::Value::Bool(false)),
                hidden: false,
            });
            registrar.register_lifecycle_hook(LifecycleHookRegistrationV1 {
                event: LifecycleEventV1::SessionStarted,
                name: "on-session-start".to_string(),
                description: Some("Record session start".to_string()),
                priority: 0,
            });
            registrar.register_provider(ProviderRegistrationV1 {
                provider_id: "example".to_string(),
                name: "Example".to_string(),
                api: "example-chat".to_string(),
                description: Some("Example provider".to_string()),
                base_url: Some("https://example.invalid".to_string()),
                headers: BTreeMap::new(),
                auth: ProviderAuthV1::None,
            });
            registrar.register_model(ModelRegistrationV1 {
                provider_id: "example".to_string(),
                model_id: "example-1".to_string(),
                name: "Example 1".to_string(),
                description: None,
                input_modalities: vec![ModelInputKindV1::Text],
                reasoning: false,
                context_window: 4096,
                max_output_tokens: 1024,
                default: true,
            });
        }
    }

    #[test]
    fn manifest_round_trips() {
        let mut manifest = PluginManifestV1::new(PluginIdentityV1 {
            id: "example".to_string(),
            name: "Example Plugin".to_string(),
            version: "1.0.0".to_string(),
            description: Some("Example plugin".to_string()),
            authors: vec!["Acme".to_string()],
            homepage: Some("https://example.invalid".to_string()),
            repository: None,
            license: Some("MIT".to_string()),
        });
        manifest.commands.push(CommandRegistrationV1 {
            name: "hello".to_string(),
            description: Some("Say hello".to_string()),
            aliases: Vec::new(),
            parameters: Vec::new(),
            hidden: false,
        });
        manifest.providers.push(ProviderRegistrationV1 {
            provider_id: "example".to_string(),
            name: "Example".to_string(),
            api: "example-chat".to_string(),
            description: None,
            base_url: None,
            headers: BTreeMap::new(),
            auth: ProviderAuthV1::None,
        });

        let json = serde_json::to_string(&manifest).expect("serialize");
        let decoded: PluginManifestV1 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, manifest);
        assert_eq!(decoded.manifest_version, PLUGIN_MANIFEST_VERSION_V1);
    }

    #[test]
    fn plugin_definition_can_register_capabilities() {
        let plugin = ExamplePlugin {
            manifest: PluginManifestV1::new(PluginIdentityV1 {
                id: "example".to_string(),
                name: "Example Plugin".to_string(),
                version: "1.0.0".to_string(),
                description: None,
                authors: Vec::new(),
                homepage: None,
                repository: None,
                license: None,
            }),
        };

        let mut collector = Collector::default();
        plugin.register(&mut collector);

        assert!(plugin.manifest().plugin.id == "example");
        assert_eq!(collector.commands.len(), 1);
        assert_eq!(collector.tools.len(), 1);
        assert_eq!(collector.flags.len(), 1);
        assert_eq!(collector.hooks.len(), 1);
        assert_eq!(collector.providers.len(), 1);
        assert_eq!(collector.models.len(), 1);
    }

    #[test]
    fn lifecycle_hook_context_is_serializable() {
        let context = LifecycleHookContextV1 {
            event: LifecycleEventV1::PromptStarted,
            plugin_id: "example".to_string(),
            workspace_root: Some(PathBuf::from("/tmp/workspace")),
            session_id: Some("session-1".to_string()),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-5.1-codex".to_string()),
            data: BTreeMap::from([(
                "prompt".to_string(),
                serde_json::Value::String("hello".to_string()),
            )]),
        };

        let json = serde_json::to_string(&context).expect("serialize");
        let decoded: LifecycleHookContextV1 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, context);
    }
}
