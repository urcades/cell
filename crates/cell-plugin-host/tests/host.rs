use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use cell_plugin_host::{
    DISCOVERY_FILE_NAMES, HostIdentity, PluginContentBlock, PluginHost, PluginHostConfig,
    PluginLaunchDescriptor, discover_plugins,
};
use cell_plugins::{
    CommandRegistrationV1, LifecycleEventV1, LifecycleHookContextV1, ModelInputKindV1,
    ModelRegistrationV1, PluginIdentityV1, PluginManifestV1, ProviderAuthV1,
    ProviderRegistrationV1, ToolRegistrationV1, ValueKindV1,
};
use tempfile::TempDir;

fn write_executable_script(path: &std::path::Path, contents: &str) {
    fs::write(path, contents).expect("write script");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
    }
}

fn manifest_json() -> String {
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

    serde_json::to_string(&cell_plugin_host::PluginMessage::Registration {
        protocol_version: cell_plugin_host::HOST_PROTOCOL_VERSION_V1,
        manifest,
    })
    .expect("serialize registration")
}

fn plugin_manifest(
    id: &str,
    name: &str,
    commands: &[&str],
    tools: &[&str],
    providers: &[&str],
    models: &[&str],
    hooks: &[(LifecycleEventV1, &str, i16)],
) -> PluginManifestV1 {
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
        manifest
            .hooks
            .push(cell_plugins::LifecycleHookRegistrationV1 {
                event: event.clone(),
                name: (*hook_name).to_string(),
                description: Some(format!("Hook {hook_name}")),
                priority: *priority,
            });
    }

    manifest
}

fn registration_json(manifest: PluginManifestV1) -> String {
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

fn plugin_script_with_stderr(manifest_json: &str, stderr_lines: &[&str]) -> String {
    let stderr = stderr_lines
        .iter()
        .map(|line| format!("echo '{}' >&2\n", line))
        .collect::<String>();
    format!(
        r#"#!/bin/sh
set -eu
read request
case "$request" in
  *'"type":"handshake_request"'* ) ;;
  * ) echo "unexpected handshake" >&2; exit 42 ;;
esac
{stderr}cat <<'JSON'
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

fn write_plugin_descriptor(root: &Path, id: &str, name: &str) {
    write_plugin_descriptor_with_env(root, id, name, Default::default());
}

fn write_plugin_descriptor_with_env(
    root: &Path,
    id: &str,
    name: &str,
    env: BTreeMap<String, String>,
) {
    fs::create_dir_all(root).expect("create plugin dir");
    let descriptor = PluginLaunchDescriptor {
        id: id.to_string(),
        name: name.to_string(),
        executable: PathBuf::from("plugin.sh"),
        args: Vec::new(),
        working_directory: None,
        env,
        description: Some(format!("{name} plugin")),
    };
    fs::write(
        root.join(DISCOVERY_FILE_NAMES[0]),
        serde_json::to_string_pretty(&descriptor).expect("serialize descriptor"),
    )
    .expect("write descriptor");
}

fn plugin_hook_runtime_script(manifest_json: &str) -> String {
    r#"#!/bin/sh
set -eu
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
cat >"$tmp" <<'PY'
import json
import os
import sys
import time

handshake = json.loads(sys.stdin.readline())
if handshake.get("type") != "handshake_request":
    sys.stderr.write("unexpected handshake\n")
    sys.exit(42)

print(r'''{manifest_json}''')
sys.stdout.flush()

behavior = os.environ.get("HOOK_BEHAVIOR", "continue")
log_path = os.environ.get("HOOK_LOG_FILE")

while True:
    raw = sys.stdin.readline()
    if not raw:
        break
    request = json.loads(raw)
    if request.get("type") != "hook_request":
        sys.stderr.write("unexpected hook request\n")
        sys.exit(43)

    if log_path:
        with open(log_path, "a", encoding="utf-8") as log_file:
            log_file.write(request["hookName"] + "\n")

    if behavior == "continue":
        print(json.dumps({
            "type": "hook_response",
            "requestId": request["requestId"],
            "outcome": "continue",
        }), flush=True)
    elif behavior == "stop":
        print(json.dumps({
            "type": "hook_response",
            "requestId": request["requestId"],
            "outcome": "stopPropagation",
        }), flush=True)
    elif behavior == "timeout":
        time.sleep(10)
    elif behavior == "malformed":
        print("{not json}", flush=True)
        sys.exit(13)
    elif behavior == "exit":
        sys.exit(13)
    else:
        sys.stderr.write("unknown behavior\n")
        sys.exit(44)
PY
python3 "$tmp"
"#
    .replace("{manifest_json}", manifest_json)
}

fn plugin_host(tempdir: &Path, timeout: Duration) -> PluginHost {
    PluginHost::new(PluginHostConfig {
        discovery_roots: vec![tempdir.to_path_buf()],
        workspace_root: Some(tempdir.to_path_buf()),
        handshake_timeout: timeout,
        host_identity: HostIdentity::new("cell-plugin-host", "0.52.12"),
    })
}

fn plugin_host_with_roots(
    roots: Vec<PathBuf>,
    workspace_root: &Path,
    timeout: Duration,
) -> PluginHost {
    PluginHost::new(PluginHostConfig {
        discovery_roots: roots,
        workspace_root: Some(workspace_root.to_path_buf()),
        handshake_timeout: timeout,
        host_identity: HostIdentity::new("cell-plugin-host", "0.52.12"),
    })
}

fn lifecycle_hook_context(root: &Path, event: LifecycleEventV1) -> LifecycleHookContextV1 {
    LifecycleHookContextV1 {
        event,
        plugin_id: "subject-plugin".to_string(),
        workspace_root: Some(root.to_path_buf()),
        session_id: Some("session-1".to_string()),
        provider_id: Some("provider-1".to_string()),
        model_id: Some("model-1".to_string()),
        data: BTreeMap::new(),
    }
}

fn prepare_hook_plugin(
    tempdir: &TempDir,
    folder: &str,
    id: &str,
    name: &str,
    hook_name: &str,
    priority: i16,
    behavior: &str,
    log_file: &Path,
) -> PathBuf {
    let root = tempdir.path().join(folder);
    fs::create_dir_all(&root).expect("create plugin dir");

    let mut env = BTreeMap::new();
    env.insert("HOOK_BEHAVIOR".to_string(), behavior.to_string());
    env.insert(
        "HOOK_LOG_FILE".to_string(),
        log_file.to_string_lossy().into_owned(),
    );

    write_executable_script(
        &root.join("plugin.sh"),
        &plugin_hook_runtime_script(&registration_json(plugin_manifest(
            id,
            name,
            &[],
            &[],
            &[],
            &[],
            &[(LifecycleEventV1::HostStartup, hook_name, priority)],
        ))),
    );
    write_plugin_descriptor_with_env(&root, id, name, env);

    root
}

fn prepare_plugin(
    tempdir: &TempDir,
    folder: &str,
    id: &str,
    name: &str,
    script: Option<&str>,
) -> PathBuf {
    let root = tempdir.path().join(folder);
    fs::create_dir_all(&root).expect("create plugin dir");
    if let Some(script) = script {
        write_executable_script(&root.join("plugin.sh"), script);
    }
    write_plugin_descriptor(&root, id, name);
    root
}

#[test]
fn discovery_is_recursively_file_based() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/example");
    fs::create_dir_all(&root).expect("mkdir");

    let descriptor_path = root.join(DISCOVERY_FILE_NAMES[0]);
    let executable_path = root.join("plugin.sh");
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
fn launch_registers_capabilities_over_typed_stdio() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/example");
    fs::create_dir_all(&root).expect("mkdir");

    let executable_path = root.join("plugin.sh");
    write_executable_script(&executable_path, &plugin_script(&manifest_json()));

    let descriptor_path = root.join(DISCOVERY_FILE_NAMES[0]);
    let descriptor = PluginLaunchDescriptor {
        id: "example".to_string(),
        name: "Example Plugin".to_string(),
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
    let host = PluginHost::new(PluginHostConfig {
        discovery_roots: vec![tempdir.path().to_path_buf()],
        workspace_root: Some(tempdir.path().to_path_buf()),
        handshake_timeout: Duration::from_secs(5),
        host_identity: HostIdentity::new("cell-plugin-host", "0.52.12"),
    });

    let registered = host
        .launch_and_register(discovered.into_iter().next().expect("plugin"))
        .expect("register");

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
fn active_registry_invokes_plugin_command() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/command");
    fs::create_dir_all(&root).expect("mkdir");

    let executable_path = root.join("plugin.sh");
    write_executable_script(
        &executable_path,
        &plugin_runtime_script(
            &registration_json(plugin_manifest(
                "command-plugin",
                "Command Plugin",
                &["rewrite"],
                &[],
                &[],
                &[],
                &[],
            )),
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
    write_plugin_descriptor(&root, "command-plugin", "Command Plugin");

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let runtime = host.discover_and_load_runtime_plugins();
    assert!(
        runtime.summary.warnings.is_empty(),
        "warnings: {:#?}",
        runtime.summary.warnings
    );
    let mut registry = runtime.registry.expect("runtime registry");

    let replacement = registry
        .invoke_command(
            "rewrite",
            &["alpha".to_string(), "beta".to_string()],
            tempdir.path(),
            Some("session-1"),
            Some("/rewrite alpha beta"),
        )
        .expect("command replacement");

    assert_eq!(replacement, "rewritten:alpha beta");
}

#[test]
fn active_registry_invokes_plugin_tool() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/tool");
    fs::create_dir_all(&root).expect("mkdir");

    let executable_path = root.join("plugin.sh");
    write_executable_script(
        &executable_path,
        &plugin_runtime_script(
            &registration_json(plugin_manifest(
                "tool-plugin",
                "Tool Plugin",
                &[],
                &["plugin-tool"],
                &[],
                &[],
                &[],
            )),
            r#"
request = json.loads(sys.stdin.readline())
assert request["type"] == "tool_request"
print(json.dumps({
    "type": "tool_response",
    "requestId": request["requestId"],
    "content": [{"type": "text", "text": f"tool:{request['arguments']['value']}"}],
    "details": {"echo": request["arguments"]},
    "isError": False,
}), flush=True)
"#,
        ),
    );
    write_plugin_descriptor(&root, "tool-plugin", "Tool Plugin");

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let runtime = host.discover_and_load_runtime_plugins();
    assert!(
        runtime.summary.warnings.is_empty(),
        "warnings: {:#?}",
        runtime.summary.warnings
    );
    let mut registry = runtime.registry.expect("runtime registry");

    let (content, details, is_error) = registry
        .invoke_tool(
            "call-1",
            "plugin-tool",
            serde_json::json!({ "value": "beta" }),
            tempdir.path(),
            Some("session-1"),
        )
        .expect("tool result");

    assert_eq!(
        content,
        vec![PluginContentBlock::Text {
            text: "tool:beta".to_string()
        }]
    );
    assert_eq!(details, Some(serde_json::json!({ "echo": { "value": "beta" } })));
    assert!(!is_error);
}

#[test]
fn handshake_timeout_is_reported_cleanly() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/silent");
    fs::create_dir_all(&root).expect("mkdir");

    let executable_path = root.join("plugin.sh");
    write_executable_script(
        &executable_path,
        "#!/bin/sh\nset -eu\nread request\nsleep 3\n",
    );

    let descriptor_path = root.join(DISCOVERY_FILE_NAMES[0]);
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
    let host = PluginHost::new(PluginHostConfig {
        discovery_roots: vec![tempdir.path().to_path_buf()],
        workspace_root: Some(tempdir.path().to_path_buf()),
        handshake_timeout: Duration::from_millis(50),
        host_identity: HostIdentity::new("cell-plugin-host", "0.52.12"),
    });

    let error = host
        .launch_and_register(discovered.into_iter().next().expect("plugin"))
        .expect_err("timeout");
    assert!(error.to_string().contains("did not respond within"));
}

#[test]
fn invalid_manifest_version_is_rejected() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/versioned");
    fs::create_dir_all(&root).expect("mkdir");

    let mut manifest = PluginManifestV1::new(PluginIdentityV1 {
        id: "versioned".to_string(),
        name: "Versioned Plugin".to_string(),
        version: "1.0.0".to_string(),
        description: None,
        authors: Vec::new(),
        homepage: None,
        repository: None,
        license: None,
    });
    manifest.commands.push(CommandRegistrationV1 {
        name: "hello".to_string(),
        description: None,
        aliases: Vec::new(),
        parameters: Vec::new(),
        hidden: false,
    });
    let manifest_json = serde_json::to_string(&cell_plugin_host::PluginMessage::Registration {
        protocol_version: cell_plugin_host::HOST_PROTOCOL_VERSION_V1,
        manifest: PluginManifestV1 {
            manifest_version: 2,
            ..manifest
        },
    })
    .expect("serialize registration");

    let executable_path = root.join("plugin.sh");
    write_executable_script(&executable_path, &plugin_script(&manifest_json));

    let descriptor_path = root.join(DISCOVERY_FILE_NAMES[0]);
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
    let host = PluginHost::new(PluginHostConfig {
        discovery_roots: vec![tempdir.path().to_path_buf()],
        workspace_root: Some(tempdir.path().to_path_buf()),
        handshake_timeout: Duration::from_secs(5),
        host_identity: HostIdentity::new("cell-plugin-host", "0.52.12"),
    });

    let error = host
        .launch_and_register(discovered.into_iter().next().expect("plugin"))
        .expect_err("version mismatch");
    assert!(error.to_string().contains("manifest version"));
}

#[test]
fn duplicate_capabilities_are_rejected() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/duplicate");
    fs::create_dir_all(&root).expect("mkdir");

    let mut manifest = PluginManifestV1::new(PluginIdentityV1 {
        id: "duplicate".to_string(),
        name: "Duplicate Plugin".to_string(),
        version: "1.0.0".to_string(),
        description: None,
        authors: Vec::new(),
        homepage: None,
        repository: None,
        license: None,
    });
    manifest.commands.push(CommandRegistrationV1 {
        name: "hello".to_string(),
        description: None,
        aliases: Vec::new(),
        parameters: Vec::new(),
        hidden: false,
    });
    manifest.commands.push(CommandRegistrationV1 {
        name: "hello".to_string(),
        description: None,
        aliases: Vec::new(),
        parameters: Vec::new(),
        hidden: false,
    });
    let manifest_json = serde_json::to_string(&cell_plugin_host::PluginMessage::Registration {
        protocol_version: cell_plugin_host::HOST_PROTOCOL_VERSION_V1,
        manifest,
    })
    .expect("serialize registration");

    let executable_path = root.join("plugin.sh");
    write_executable_script(&executable_path, &plugin_script(&manifest_json));

    let descriptor_path = root.join(DISCOVERY_FILE_NAMES[0]);
    let descriptor = PluginLaunchDescriptor {
        id: "duplicate".to_string(),
        name: "Duplicate Plugin".to_string(),
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
    let host = PluginHost::new(PluginHostConfig {
        discovery_roots: vec![tempdir.path().to_path_buf()],
        workspace_root: Some(tempdir.path().to_path_buf()),
        handshake_timeout: Duration::from_secs(5),
        host_identity: HostIdentity::new("cell-plugin-host", "0.52.12"),
    });

    let error = host
        .launch_and_register(discovered.into_iter().next().expect("plugin"))
        .expect_err("duplicate capability");
    assert!(
        error
            .to_string()
            .contains("duplicate capability registration")
    );
}

#[test]
fn launch_registers_capabilities_even_with_noisy_stderr() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/noisy");
    fs::create_dir_all(&root).expect("mkdir");

    let manifest = plugin_manifest(
        "noisy",
        "Noisy Plugin",
        &["hello"],
        &["echo"],
        &["noisy"],
        &["noisy-1"],
        &[],
    );
    let manifest_json = registration_json(manifest);
    write_executable_script(
        &root.join("plugin.sh"),
        &plugin_script_with_stderr(&manifest_json, &["booting", "still here"]),
    );
    write_plugin_descriptor(&root, "noisy", "Noisy Plugin");

    let discovered = discover_plugins(&[tempdir.path().to_path_buf()]).expect("discover");
    let host = plugin_host(tempdir.path(), Duration::from_secs(5));

    let registered = host
        .launch_and_register(discovered.into_iter().next().expect("plugin"))
        .expect("register noisy plugin");

    assert_eq!(registered.manifest.plugin.id, "noisy");
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
        vec!["noisy".to_string()]
    );
    assert_eq!(
        registered.capabilities.model_ids(),
        vec!["noisy-1".to_string()]
    );
}

#[test]
fn launch_reports_malformed_json_on_stdout() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/malformed");
    fs::create_dir_all(&root).expect("mkdir");

    write_executable_script(
        &root.join("plugin.sh"),
        r#"#!/bin/sh
set -eu
read request
case "$request" in
  *'"type":"handshake_request"'* ) ;;
  * ) echo "unexpected handshake" >&2; exit 42 ;;
esac
printf '{not json}\n'
"#,
    );
    write_plugin_descriptor(&root, "malformed", "Malformed Plugin");

    let discovered = discover_plugins(&[tempdir.path().to_path_buf()]).expect("discover");
    let host = plugin_host(tempdir.path(), Duration::from_secs(5));

    let error = host
        .launch_and_register(discovered.into_iter().next().expect("plugin"))
        .expect_err("malformed registration");
    assert!(error.to_string().contains("malformed data"));
}

#[test]
fn launch_rejects_mismatched_plugin_identity() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/wrong-id");
    fs::create_dir_all(&root).expect("mkdir");

    let manifest = plugin_manifest(
        "wrong-id",
        "Wrong ID Plugin",
        &["hello"],
        &[],
        &[],
        &[],
        &[],
    );
    let manifest_json = registration_json(manifest);
    write_executable_script(&root.join("plugin.sh"), &plugin_script(&manifest_json));
    write_plugin_descriptor(&root, "expected-id", "Wrong ID Plugin");

    let discovered = discover_plugins(&[tempdir.path().to_path_buf()]).expect("discover");
    let host = plugin_host(tempdir.path(), Duration::from_secs(5));

    let error = host
        .launch_and_register(discovered.into_iter().next().expect("plugin"))
        .expect_err("wrong plugin identity");
    assert!(error.to_string().contains("mismatched identity"));
}

#[test]
fn launch_reports_missing_executable() {
    let tempdir = TempDir::new().expect("tempdir");
    let root = tempdir.path().join("plugins/missing");
    fs::create_dir_all(&root).expect("mkdir");
    write_plugin_descriptor(&root, "missing", "Missing Plugin");

    let discovered = discover_plugins(&[tempdir.path().to_path_buf()]).expect("discover");
    let host = plugin_host(tempdir.path(), Duration::from_secs(5));

    let error = host
        .launch_and_register(discovered.into_iter().next().expect("plugin"))
        .expect_err("missing executable");
    assert!(error.to_string().contains("plugin executable not found"));
}

#[test]
fn discover_and_register_startup_plugins_only_uses_explicit_roots() {
    let tempdir = TempDir::new().expect("tempdir");

    let good_root = tempdir.path().join("packages/good");
    fs::create_dir_all(&good_root).expect("mkdir good");
    write_executable_script(
        &good_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "good",
            "Good Plugin",
            &["good-command"],
            &["good-tool"],
            &["good-provider"],
            &["good-model"],
            &[],
        ))),
    );
    write_plugin_descriptor(&good_root, "good", "Good Plugin");

    let stray_root = tempdir.path().join("packages/stray");
    fs::create_dir_all(&stray_root).expect("mkdir stray");
    write_executable_script(
        &stray_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "stray",
            "Stray Plugin",
            &["stray-command"],
            &[],
            &[],
            &[],
            &[],
        ))),
    );
    write_plugin_descriptor(&stray_root, "stray", "Stray Plugin");

    let host = plugin_host_with_roots(
        vec![good_root.clone()],
        tempdir.path(),
        Duration::from_secs(5),
    );
    let startup = host.discover_and_register_startup_plugins();

    assert_eq!(startup.summaries.len(), 1);
    assert_eq!(startup.summaries[0].plugin_id, "good");
    assert!(startup.warnings.is_empty());
}

#[test]
fn discover_and_register_startup_plugins_reports_warnings_without_blocking() {
    let tempdir = TempDir::new().expect("tempdir");

    let good_root = tempdir.path().join("packages/good");
    fs::create_dir_all(&good_root).expect("mkdir good");
    write_executable_script(
        &good_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "good",
            "Good Plugin",
            &["good-command"],
            &["good-tool"],
            &["good-provider"],
            &["good-model"],
            &[],
        ))),
    );
    write_plugin_descriptor(&good_root, "good", "Good Plugin");

    let malformed_root = tempdir.path().join("packages/malformed");
    fs::create_dir_all(&malformed_root).expect("mkdir malformed");
    fs::write(
        malformed_root.join(DISCOVERY_FILE_NAMES[0]),
        "{ not json }\n",
    )
    .expect("write malformed descriptor");

    let timeout_root = tempdir.path().join("packages/timeout");
    fs::create_dir_all(&timeout_root).expect("mkdir timeout");
    write_executable_script(
        &timeout_root.join("plugin.sh"),
        "#!/bin/sh\nset -eu\nread request\nsleep 10\n",
    );
    write_plugin_descriptor(&timeout_root, "timeout", "Timeout Plugin");

    let duplicate_root = tempdir.path().join("packages/duplicate");
    fs::create_dir_all(&duplicate_root).expect("mkdir duplicate");
    write_executable_script(
        &duplicate_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "duplicate",
            "Duplicate Plugin",
            &["dup", "dup"],
            &[],
            &[],
            &[],
            &[],
        ))),
    );
    write_plugin_descriptor(&duplicate_root, "duplicate", "Duplicate Plugin");

    let host = plugin_host_with_roots(
        vec![
            good_root.clone(),
            malformed_root.clone(),
            timeout_root.clone(),
            duplicate_root.clone(),
        ],
        tempdir.path(),
            Duration::from_secs(5),
    );
    let startup = host.discover_and_register_startup_plugins();

    assert_eq!(startup.summaries.len(), 1);
    assert_eq!(startup.summaries[0].plugin_id, "good");
    assert_eq!(startup.warnings.len(), 3);
    assert!(
        startup
            .warnings
            .iter()
            .any(|warning| warning.path == malformed_root.join(DISCOVERY_FILE_NAMES[0]))
    );
    assert!(startup.warnings.iter().any(|warning| warning.path
        == timeout_root.join(DISCOVERY_FILE_NAMES[0])
        && warning.message.contains("did not respond")));
    assert!(
        startup.warnings.iter().any(|warning| warning.path
            == duplicate_root.join(DISCOVERY_FILE_NAMES[0])
            && warning.message.contains("duplicate capability")),
        "warnings: {:#?}",
        startup.warnings
    );
}

#[test]
fn discover_and_merge_sorts_hooks_and_preserves_sources() {
    let tempdir = TempDir::new().expect("tempdir");

    let alpha_root = prepare_plugin(
        &tempdir,
        "plugins/alpha",
        "alpha",
        "Alpha Plugin",
        Some(&plugin_script(&registration_json(plugin_manifest(
            "alpha",
            "Alpha Plugin",
            &["alpha-command"],
            &["alpha-tool"],
            &["alpha-provider"],
            &["alpha-model"],
            &[(LifecycleEventV1::HostStartup, "alpha-start", 0)],
        )))),
    );
    let beta_root = prepare_plugin(
        &tempdir,
        "plugins/beta",
        "beta",
        "Beta Plugin",
        Some(&plugin_script(&registration_json(plugin_manifest(
            "beta",
            "Beta Plugin",
            &["beta-command"],
            &["beta-tool"],
            &["beta-provider"],
            &["beta-model"],
            &[(LifecycleEventV1::HostStartup, "beta-start", 0)],
        )))),
    );
    let gamma_root = prepare_plugin(
        &tempdir,
        "plugins/gamma",
        "gamma",
        "Gamma Plugin",
        Some(&plugin_script(&registration_json(plugin_manifest(
            "gamma",
            "Gamma Plugin",
            &["gamma-command"],
            &["gamma-tool"],
            &["gamma-provider"],
            &["gamma-model"],
            &[(LifecycleEventV1::SessionStarted, "gamma-session", 10)],
        )))),
    );

    let _ = (alpha_root, beta_root, gamma_root);

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let merged = host.discover_and_merge().expect("merge registry");

    assert_eq!(
        merged
            .plugins
            .iter()
            .map(|plugin| plugin.source.plugin_id.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "beta", "gamma"]
    );
    assert_eq!(
        merged
            .commands
            .get("alpha-command")
            .expect("alpha command")
            .source
            .plugin_id,
        "alpha"
    );
    assert_eq!(
        merged
            .hooks
            .iter()
            .map(|hook| (
                hook.source.plugin_id.as_str(),
                hook.registration.name.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("alpha", "alpha-start"),
            ("beta", "beta-start"),
            ("gamma", "gamma-session"),
            ]
    );
}

#[test]
fn active_registry_dispatches_hooks_in_merged_order_and_stops_on_request() {
    let tempdir = TempDir::new().expect("tempdir");
    let log_file = tempdir.path().join("hooks.log");

    let _alpha_root = prepare_hook_plugin(
        &tempdir,
        "plugins/alpha",
        "alpha",
        "Alpha Plugin",
        "alpha-start",
        30,
        "continue",
        &log_file,
    );
    let _beta_root = prepare_hook_plugin(
        &tempdir,
        "plugins/beta",
        "beta",
        "Beta Plugin",
        "beta-stop",
        20,
        "stop",
        &log_file,
    );
    let _gamma_root = prepare_hook_plugin(
        &tempdir,
        "plugins/gamma",
        "gamma",
        "Gamma Plugin",
        "gamma-late",
        10,
        "continue",
        &log_file,
    );

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let mut runtime = host.discover_and_load_runtime_plugins();
    assert!(
        runtime.summary.warnings.is_empty(),
        "warnings: {:#?}",
        runtime.summary.warnings
    );

    let report = runtime
        .registry
        .as_mut()
        .expect("runtime registry")
        .dispatch_hooks(lifecycle_hook_context(
            tempdir.path(),
            LifecycleEventV1::HostStartup,
        ));

    assert!(report.stopped);
    assert!(report.warnings.is_empty(), "warnings: {report:#?}");
    let lines = fs::read_to_string(&log_file).expect("hook log");
    assert_eq!(
        lines.lines().collect::<Vec<_>>(),
        vec!["alpha-start", "beta-stop"]
    );
}

#[test]
fn active_registry_dispatches_hooks_best_effort_through_timeout() {
    let tempdir = TempDir::new().expect("tempdir");
    let log_file = tempdir.path().join("hooks.log");

    let _alpha_root = prepare_hook_plugin(
        &tempdir,
        "plugins/alpha",
        "alpha",
        "Alpha Plugin",
        "alpha-start",
        30,
        "continue",
        &log_file,
    );
    let _beta_root = prepare_hook_plugin(
        &tempdir,
        "plugins/beta",
        "beta",
        "Beta Plugin",
        "beta-timeout",
        20,
        "timeout",
        &log_file,
    );
    let _gamma_root = prepare_hook_plugin(
        &tempdir,
        "plugins/gamma",
        "gamma",
        "Gamma Plugin",
        "gamma-late",
        10,
        "continue",
        &log_file,
    );

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let mut runtime = host.discover_and_load_runtime_plugins();
    assert!(
        runtime.summary.warnings.is_empty(),
        "warnings: {:#?}",
        runtime.summary.warnings
    );

    let report = runtime
        .registry
        .as_mut()
        .expect("runtime registry")
        .dispatch_hooks(lifecycle_hook_context(
            tempdir.path(),
            LifecycleEventV1::HostStartup,
        ));

    assert!(!report.stopped);
    assert_eq!(report.warnings.len(), 1, "warnings: {report:#?}");
    assert_eq!(
        report.warnings[0].plugin_id.as_deref(),
        Some("beta")
    );
    assert!(
        report.warnings[0]
            .message
            .contains("did not respond to hook"),
        "warnings: {report:#?}"
    );
    let lines = fs::read_to_string(&log_file).expect("hook log");
    assert_eq!(
        lines.lines().collect::<Vec<_>>(),
        vec!["alpha-start", "beta-timeout", "gamma-late"]
    );
}

#[test]
fn active_registry_dispatches_hooks_best_effort_through_malformed_and_crash() {
    let tempdir = TempDir::new().expect("tempdir");
    let log_file = tempdir.path().join("hooks.log");

    let _alpha_root = prepare_hook_plugin(
        &tempdir,
        "plugins/alpha",
        "alpha",
        "Alpha Plugin",
        "alpha-start",
        40,
        "continue",
        &log_file,
    );
    let _beta_root = prepare_hook_plugin(
        &tempdir,
        "plugins/beta",
        "beta",
        "Beta Plugin",
        "beta-malformed",
        30,
        "malformed",
        &log_file,
    );
    let _gamma_root = prepare_hook_plugin(
        &tempdir,
        "plugins/gamma",
        "gamma",
        "Gamma Plugin",
        "gamma-exit",
        20,
        "exit",
        &log_file,
    );
    let _delta_root = prepare_hook_plugin(
        &tempdir,
        "plugins/delta",
        "delta",
        "Delta Plugin",
        "delta-late",
        10,
        "continue",
        &log_file,
    );

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let mut runtime = host.discover_and_load_runtime_plugins();
    assert!(
        runtime.summary.warnings.is_empty(),
        "warnings: {:#?}",
        runtime.summary.warnings
    );

    let report = runtime
        .registry
        .as_mut()
        .expect("runtime registry")
        .dispatch_hooks(lifecycle_hook_context(
            tempdir.path(),
            LifecycleEventV1::HostStartup,
        ));

    assert!(!report.stopped);
    assert_eq!(report.warnings.len(), 2, "warnings: {report:#?}");
    assert_eq!(report.warnings[0].plugin_id.as_deref(), Some("beta"));
    assert!(report.warnings[0].message.contains("malformed"));
    assert_eq!(report.warnings[1].plugin_id.as_deref(), Some("gamma"));
    assert!(
        report.warnings[1].message.contains("exited before"),
        "warnings: {report:#?}"
    );
    let lines = fs::read_to_string(&log_file).expect("hook log");
    assert_eq!(
        lines.lines().collect::<Vec<_>>(),
        vec!["alpha-start", "beta-malformed", "gamma-exit", "delta-late"]
    );
}

#[test]
fn discover_and_merge_rejects_duplicate_commands_across_plugins() {
    let tempdir = TempDir::new().expect("tempdir");
    let alpha_root = tempdir.path().join("plugins/alpha");
    fs::create_dir_all(&alpha_root).expect("mkdir");
    write_executable_script(
        &alpha_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "alpha",
            "Alpha Plugin",
            &["shared-command"],
            &[],
            &[],
            &[],
            &[],
        ))),
    );
    write_plugin_descriptor(&alpha_root, "alpha", "Alpha Plugin");

    let beta_root = tempdir.path().join("plugins/beta");
    fs::create_dir_all(&beta_root).expect("mkdir");
    write_executable_script(
        &beta_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "beta",
            "Beta Plugin",
            &["shared-command"],
            &[],
            &[],
            &[],
            &[],
        ))),
    );
    write_plugin_descriptor(&beta_root, "beta", "Beta Plugin");

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let error = host.discover_and_merge().expect_err("duplicate command");
    assert!(
        error
            .to_string()
            .contains("duplicate capability registration")
    );
    assert!(error.to_string().contains("command"));
}

#[test]
fn discover_and_merge_rejects_duplicate_tools_across_plugins() {
    let tempdir = TempDir::new().expect("tempdir");
    let alpha_root = tempdir.path().join("plugins/alpha");
    fs::create_dir_all(&alpha_root).expect("mkdir");
    write_executable_script(
        &alpha_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "alpha",
            "Alpha Plugin",
            &[],
            &["shared-tool"],
            &[],
            &[],
            &[],
        ))),
    );
    write_plugin_descriptor(&alpha_root, "alpha", "Alpha Plugin");

    let beta_root = tempdir.path().join("plugins/beta");
    fs::create_dir_all(&beta_root).expect("mkdir");
    write_executable_script(
        &beta_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "beta",
            "Beta Plugin",
            &[],
            &["shared-tool"],
            &[],
            &[],
            &[],
        ))),
    );
    write_plugin_descriptor(&beta_root, "beta", "Beta Plugin");

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let error = host.discover_and_merge().expect_err("duplicate tool");
    assert!(
        error
            .to_string()
            .contains("duplicate capability registration")
    );
    assert!(error.to_string().contains("tool"));
}

#[test]
fn discover_and_merge_rejects_duplicate_providers_across_plugins() {
    let tempdir = TempDir::new().expect("tempdir");
    let alpha_root = tempdir.path().join("plugins/alpha");
    fs::create_dir_all(&alpha_root).expect("mkdir");
    write_executable_script(
        &alpha_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "alpha",
            "Alpha Plugin",
            &[],
            &[],
            &["shared-provider"],
            &[],
            &[],
        ))),
    );
    write_plugin_descriptor(&alpha_root, "alpha", "Alpha Plugin");

    let beta_root = tempdir.path().join("plugins/beta");
    fs::create_dir_all(&beta_root).expect("mkdir");
    write_executable_script(
        &beta_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "beta",
            "Beta Plugin",
            &[],
            &[],
            &["shared-provider"],
            &[],
            &[],
        ))),
    );
    write_plugin_descriptor(&beta_root, "beta", "Beta Plugin");

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let error = host.discover_and_merge().expect_err("duplicate provider");
    assert!(
        error
            .to_string()
            .contains("duplicate capability registration")
    );
    assert!(error.to_string().contains("provider"));
}

#[test]
fn discover_and_merge_rejects_duplicate_models_across_plugins() {
    let tempdir = TempDir::new().expect("tempdir");
    let alpha_root = tempdir.path().join("plugins/alpha");
    fs::create_dir_all(&alpha_root).expect("mkdir");
    write_executable_script(
        &alpha_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "alpha",
            "Alpha Plugin",
            &[],
            &[],
            &["alpha-provider"],
            &["shared-model"],
            &[],
        ))),
    );
    write_plugin_descriptor(&alpha_root, "alpha", "Alpha Plugin");

    let beta_root = tempdir.path().join("plugins/beta");
    fs::create_dir_all(&beta_root).expect("mkdir");
    write_executable_script(
        &beta_root.join("plugin.sh"),
        &plugin_script(&registration_json(plugin_manifest(
            "beta",
            "Beta Plugin",
            &[],
            &[],
            &["beta-provider"],
            &["shared-model"],
            &[],
        ))),
    );
    write_plugin_descriptor(&beta_root, "beta", "Beta Plugin");

    let host = plugin_host(tempdir.path(), Duration::from_secs(5));
    let error = host.discover_and_merge().expect_err("duplicate model");
    assert!(
        error
            .to_string()
            .contains("duplicate capability registration")
    );
    assert!(error.to_string().contains("model"));
}
