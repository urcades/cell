use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use pi_rust_plugin_host::{PluginContentBlock, PluginHost, PluginHostConfig};
use pi_rust_plugins::{LifecycleEventV1, LifecycleHookContextV1};
use serde_json::json;

#[test]
fn example_plugin_supports_command_tool_and_hook_dispatch() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let host = PluginHost::new(PluginHostConfig {
        discovery_roots: vec![root.clone()],
        workspace_root: Some(root.clone()),
        handshake_timeout: Duration::from_secs(20),
        ..PluginHostConfig::default()
    });

    let mut runtime = host.discover_and_load_runtime_plugins();
    assert!(
        runtime.summary.warnings.is_empty(),
        "warnings: {:#?}",
        runtime.summary.warnings
    );
    assert_eq!(runtime.summary.summaries.len(), 1);
    assert_eq!(runtime.summary.summaries[0].plugin_id, "hello-plugin");
    assert_eq!(runtime.summary.summaries[0].capabilities.commands, 1);
    assert_eq!(runtime.summary.summaries[0].capabilities.tools, 1);
    assert_eq!(runtime.summary.summaries[0].capabilities.hooks, 1);

    let registry = runtime.registry.as_mut().expect("runtime registry");

    let replacement = registry
        .invoke_command(
            "hello",
            &["Ada".to_string(), "Lovelace".to_string()],
            &root,
            Some("session-1"),
            Some("hello Ada Lovelace"),
        )
        .expect("command replacement");
    assert_eq!(replacement, "hello:Ada|Lovelace");

    let (content, details, is_error) = registry
        .invoke_tool(
            "tool-call-1",
            "echo",
            json!({ "text": "Ada" }),
            &root,
            Some("session-1"),
        )
        .expect("tool result");
    assert_eq!(
        content,
        vec![PluginContentBlock::Text {
            text: "tool:Ada".to_string()
        }]
    );
    assert_eq!(details, Some(json!({ "echo": "Ada" })));
    assert!(!is_error);

    let report = registry.dispatch_hooks(LifecycleHookContextV1 {
        event: LifecycleEventV1::SessionStarted,
        plugin_id: "subject-plugin".to_string(),
        workspace_root: Some(root.clone()),
        session_id: Some("session-1".to_string()),
        provider_id: None,
        model_id: None,
        data: BTreeMap::new(),
    });
    assert!(!report.stopped);
    assert!(report.warnings.is_empty(), "warnings: {:#?}", report.warnings);
}
