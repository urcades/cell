use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HostError {
    #[error("failed to read plugin descriptor {path}: {source}")]
    DescriptorRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse plugin descriptor {path}: {source}")]
    DescriptorParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid plugin descriptor {path}: {message}")]
    InvalidDescriptor { path: PathBuf, message: String },
    #[error("failed to discover plugins under {path}: {source}")]
    Discovery {
        path: PathBuf,
        source: walkdir::Error,
    },
    #[error("plugin executable not found: {path}")]
    MissingExecutable { path: PathBuf },
    #[error("failed to spawn plugin {plugin_id}: {source}")]
    Spawn {
        plugin_id: String,
        source: std::io::Error,
    },
    #[error("plugin {plugin_id} did not respond within {timeout:?}")]
    HandshakeTimeout {
        plugin_id: String,
        timeout: Duration,
    },
    #[error("plugin {plugin_id} did not respond to command `{command_name}` within {timeout:?}")]
    CommandTimeout {
        plugin_id: String,
        command_name: String,
        timeout: Duration,
    },
    #[error("plugin {plugin_id} did not respond to tool `{tool_name}` within {timeout:?}")]
    ToolTimeout {
        plugin_id: String,
        tool_name: String,
        timeout: Duration,
    },
    #[error("plugin {plugin_id} did not respond to hook `{hook_name}` within {timeout:?}")]
    HookTimeout {
        plugin_id: String,
        hook_name: String,
        timeout: Duration,
    },
    #[error("plugin {plugin_id} exited before registration")]
    EarlyExit { plugin_id: String },
    #[error("plugin {plugin_id} sent malformed data: {message}")]
    Protocol { plugin_id: String, message: String },
    #[error("plugin {plugin_id} command `{command_name}` failed: {message}")]
    CommandFailed {
        plugin_id: String,
        command_name: String,
        message: String,
    },
    #[error("plugin {plugin_id} tool `{tool_name}` failed: {message}")]
    ToolFailed {
        plugin_id: String,
        tool_name: String,
        message: String,
    },
    #[error("plugin {plugin_id} hook `{hook_name}` failed: {message}")]
    HookFailed {
        plugin_id: String,
        hook_name: String,
        message: String,
    },
    #[error("duplicate capability registration for {kind} `{name}` in plugin {plugin_id}")]
    DuplicateCapability {
        plugin_id: String,
        kind: &'static str,
        name: String,
    },
    #[error(
        "duplicate capability registration for {kind} `{name}` across plugins `{first_plugin_id}` and `{second_plugin_id}`"
    )]
    DuplicateMergedCapability {
        kind: &'static str,
        name: String,
        first_plugin_id: String,
        second_plugin_id: String,
    },
    #[error("plugin {plugin_id} reported unsupported manifest version {manifest_version}")]
    UnsupportedManifestVersion {
        plugin_id: String,
        manifest_version: u16,
    },
    #[error(
        "plugin {plugin_id} reported a mismatched identity: expected `{expected}` but got `{actual}`"
    )]
    PluginIdentityMismatch {
        plugin_id: String,
        expected: String,
        actual: String,
    },
}
