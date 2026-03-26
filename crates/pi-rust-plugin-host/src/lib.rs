//! Headless Rust plugin host skeleton.
//!
//! This crate is intentionally narrow. It knows how to:
//! - discover plugin launch descriptors from disk
//! - launch a plugin process out of process
//! - exchange a typed JSON-line handshake over stdio
//! - build a capability index from the plugin manifest returned at handshake
//!
//! It can also keep a live runtime registry for commands, tools, and lifecycle
//! hooks after the startup handshake, while provider/model execution remains
//! future work.

mod descriptor;
mod error;
mod host;
mod process;
mod protocol;
mod registry;

pub use descriptor::{
    DISCOVERY_FILE_NAMES, DiscoveredPlugin, PluginLaunchDescriptor, discover_plugins,
    load_descriptor,
};
pub use error::HostError;
pub use host::{
    ActivePluginRegistry, HookDispatchReport, LoadedPluginRuntime, PluginHost, PluginHostConfig,
    PluginHostWarning, PluginStartupSummary, RegisteredPlugin, RegisteredPluginSummary,
};
pub use process::{PluginSession, PluginSessionConfig};
pub use pi_rust_plugin_protocol::{
    HostIdentity, HostMessage, LogLevel, PLUGIN_PROTOCOL_VERSION_V1 as HOST_PROTOCOL_VERSION_V1,
    PluginContentBlock, PluginMessage,
};
pub use protocol::{CapabilityCounts, CapabilityIndex};
pub use registry::{
    MergedCommandRegistration, MergedFlagRegistration, MergedHookRegistration,
    MergedModelRegistration, MergedPluginRecord, MergedPluginRegistry, MergedProviderRegistration,
    MergedToolRegistration, PluginSource,
};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use pi_rust_plugins::{
        CommandRegistrationV1, ModelInputKindV1, ModelRegistrationV1, PluginIdentityV1,
        PluginManifestV1, ProviderAuthV1, ProviderRegistrationV1, ToolRegistrationV1, ValueKindV1,
    };
    use tempfile::TempDir;

    use super::*;

    fn write_executable_script(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write plugin script");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).expect("script metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("mark script executable");
        }
    }

    fn example_manifest() -> PluginManifestV1 {
        let mut manifest = PluginManifestV1::new(PluginIdentityV1 {
            id: "example".to_string(),
            name: "Example Plugin".to_string(),
            version: "1.0.0".to_string(),
            description: Some("Example plugin".to_string()),
            authors: vec!["Acme".to_string()],
            homepage: None,
            repository: None,
            license: Some("MIT".to_string()),
        });
        manifest.commands.push(CommandRegistrationV1 {
            name: "hello".to_string(),
            description: Some("Say hello".to_string()),
            aliases: vec!["hi".to_string()],
            parameters: Vec::new(),
            hidden: false,
        });
        manifest.tools.push(ToolRegistrationV1 {
            name: "echo".to_string(),
            description: Some("Echo text".to_string()),
            aliases: Vec::new(),
            parameters: Vec::new(),
            output: Some(ValueKindV1::String),
            hidden: false,
        });
        manifest.providers.push(ProviderRegistrationV1 {
            provider_id: "example".to_string(),
            name: "Example".to_string(),
            api: "example-chat".to_string(),
            description: Some("Example provider".to_string()),
            base_url: Some("https://example.invalid".to_string()),
            headers: Default::default(),
            auth: ProviderAuthV1::None,
        });
        manifest.models.push(ModelRegistrationV1 {
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
        manifest
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

    #[test]
    fn discovers_launch_descriptors_recursively() {
        let tempdir = TempDir::new().expect("tempdir");
        let nested = tempdir.path().join("plugins/example");
        fs::create_dir_all(&nested).expect("create nested dir");

        let descriptor_path = nested.join("pi-plugin-host.json");
        let executable_path = nested.join("plugin.sh");
        write_executable_script(&executable_path, "#!/bin/sh\nexit 0\n");

        let descriptor = PluginLaunchDescriptor {
            id: "example".to_string(),
            name: "Example Plugin".to_string(),
            executable: PathBuf::from("plugin.sh"),
            args: vec!["--serve".to_string()],
            working_directory: None,
            env: Default::default(),
            description: Some("Example plugin".to_string()),
        };
        fs::write(
            &descriptor_path,
            serde_json::to_string_pretty(&descriptor).expect("serialize descriptor"),
        )
        .expect("write descriptor");

        let discovered = discover_plugins(&[tempdir.path().to_path_buf()]).expect("discover");
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].descriptor.id, "example");
        assert_eq!(discovered[0].descriptor_path, descriptor_path);
    }

    #[test]
    fn handshakes_and_registers_capabilities_over_stdio() {
        let tempdir = TempDir::new().expect("tempdir");
        let nested = tempdir.path().join("plugins/example");
        fs::create_dir_all(&nested).expect("create nested dir");

        let manifest = example_manifest();
        let manifest_json = serde_json::to_string(&PluginMessage::Registration {
            protocol_version: HOST_PROTOCOL_VERSION_V1,
            manifest: manifest.clone(),
        })
        .expect("serialize manifest");

        let executable_path = nested.join("plugin.sh");
        write_executable_script(&executable_path, &plugin_script(&manifest_json));

        let descriptor_path = nested.join("pi-plugin-host.json");
        let descriptor = PluginLaunchDescriptor {
            id: "example".to_string(),
            name: "Example Plugin".to_string(),
            executable: PathBuf::from("plugin.sh"),
            args: Vec::new(),
            working_directory: None,
            env: Default::default(),
            description: Some("Example plugin".to_string()),
        };
        fs::write(
            &descriptor_path,
            serde_json::to_string_pretty(&descriptor).expect("serialize descriptor"),
        )
        .expect("write descriptor");

        let discovered = discover_plugins(&[tempdir.path().to_path_buf()]).expect("discover");
        let plugin_host = PluginHost::new(PluginHostConfig {
            discovery_roots: vec![tempdir.path().to_path_buf()],
            workspace_root: Some(tempdir.path().to_path_buf()),
            handshake_timeout: Duration::from_secs(5),
            host_identity: HostIdentity::new("pi-rust-plugin-host", "0.52.12"),
        });

        let registered = plugin_host
            .launch_and_register(discovered.into_iter().next().expect("plugin"))
            .expect("register plugin");

        assert_eq!(registered.manifest.plugin.id, "example");
        assert_eq!(
            registered.capabilities.command_names(),
            vec!["hello".to_string()]
        );
        assert_eq!(
            registered.capabilities.tool_names(),
            vec!["echo".to_string()]
        );
        assert_eq!(
            registered.capabilities.provider_ids(),
            vec!["example".to_string()]
        );
        assert_eq!(
            registered.capabilities.model_ids(),
            vec!["example-1".to_string()]
        );
        assert_eq!(registered.capabilities.counts().commands, 1);
    }

    #[test]
    fn handshake_times_out_when_plugin_is_silent() {
        let tempdir = TempDir::new().expect("tempdir");
        let nested = tempdir.path().join("plugins/silent");
        fs::create_dir_all(&nested).expect("create nested dir");

        let executable_path = nested.join("plugin.sh");
        write_executable_script(
            &executable_path,
            "#!/bin/sh\nset -eu\nread request\nsleep 2\n",
        );

        let descriptor_path = nested.join("pi-plugin-host.json");
        let descriptor = PluginLaunchDescriptor {
            id: "silent".to_string(),
            name: "Silent Plugin".to_string(),
            executable: PathBuf::from("plugin.sh"),
            args: Vec::new(),
            working_directory: None,
            env: Default::default(),
            description: None,
        };
        fs::write(
            &descriptor_path,
            serde_json::to_string_pretty(&descriptor).expect("serialize descriptor"),
        )
        .expect("write descriptor");

        let discovered = discover_plugins(&[tempdir.path().to_path_buf()]).expect("discover");
        let plugin_host = PluginHost::new(PluginHostConfig {
            discovery_roots: vec![tempdir.path().to_path_buf()],
            workspace_root: Some(tempdir.path().to_path_buf()),
            handshake_timeout: Duration::from_millis(50),
            host_identity: HostIdentity::new("pi-rust-plugin-host", "0.52.12"),
        });

        let error = plugin_host
            .launch_and_register(discovered.into_iter().next().expect("plugin"))
            .expect_err("timeout");
        assert!(error.to_string().contains("did not respond within"));
    }

    #[test]
    fn registration_requires_expected_manifest_version() {
        let tempdir = TempDir::new().expect("tempdir");
        let nested = tempdir.path().join("plugins/versioned");
        fs::create_dir_all(&nested).expect("create nested dir");

        let manifest = PluginMessage::Registration {
            protocol_version: HOST_PROTOCOL_VERSION_V1,
            manifest: PluginManifestV1 {
                manifest_version: 2,
                ..example_manifest()
            },
        };
        let executable_path = nested.join("plugin.sh");
        write_executable_script(
            &executable_path,
            &plugin_script(&serde_json::to_string(&manifest).expect("serialize")),
        );

        let descriptor_path = nested.join("pi-plugin-host.json");
        let descriptor = PluginLaunchDescriptor {
            id: "versioned".to_string(),
            name: "Versioned Plugin".to_string(),
            executable: PathBuf::from("plugin.sh"),
            args: Vec::new(),
            working_directory: None,
            env: Default::default(),
            description: None,
        };
        fs::write(
            &descriptor_path,
            serde_json::to_string_pretty(&descriptor).expect("serialize descriptor"),
        )
        .expect("write descriptor");

        let discovered = discover_plugins(&[tempdir.path().to_path_buf()]).expect("discover");
        let plugin_host = PluginHost::new(PluginHostConfig {
            discovery_roots: vec![tempdir.path().to_path_buf()],
            workspace_root: Some(tempdir.path().to_path_buf()),
            handshake_timeout: Duration::from_secs(5),
            host_identity: HostIdentity::new("pi-rust-plugin-host", "0.52.12"),
        });

        let error = plugin_host
            .launch_and_register(discovered.into_iter().next().expect("plugin"))
            .expect_err("invalid manifest");
        assert!(error.to_string().contains("manifest version"));
    }
}
