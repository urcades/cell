use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::descriptor::DiscoveredPlugin;
use crate::error::HostError;
use pi_rust_plugin_protocol::PluginContentBlock;
use pi_rust_jsonline_transport::{
    ClientReceiveError, InboundFrame, JsonLineClientSession,
};
use crate::protocol::{
    CapabilityIndex, HOST_PROTOCOL_VERSION_V1, HostIdentity, HostMessage, PluginMessage,
};
use pi_rust_plugins::{LifecycleHookContextV1, LifecycleHookOutcomeV1};

#[derive(Clone, Debug)]
pub struct PluginSessionConfig {
    pub host_identity: HostIdentity,
    pub workspace_root: Option<PathBuf>,
    pub handshake_timeout: Duration,
}

impl Default for PluginSessionConfig {
    fn default() -> Self {
        Self {
            host_identity: HostIdentity::new("pi-rust-plugin-host", "0.52.12"),
            workspace_root: None,
            handshake_timeout: Duration::from_secs(5),
        }
    }
}

pub struct PluginSession {
    descriptor: DiscoveredPlugin,
    child: Child,
    client: JsonLineClientSession<ChildStdin, HostMessage, PluginMessage>,
    config: PluginSessionConfig,
}

pub type PluginFrame = InboundFrame<PluginMessage>;

impl PluginSession {
    pub fn launch(
        discovered: DiscoveredPlugin,
        config: PluginSessionConfig,
    ) -> Result<Self, HostError> {
        let executable = resolve_executable(&discovered)?;
        let mut command = Command::new(executable);
        command.args(&discovered.descriptor.args);
        command.current_dir(resolve_working_directory(&discovered));
        command.envs(&discovered.descriptor.env);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|source| HostError::Spawn {
            plugin_id: discovered.descriptor.id.clone(),
            source,
        })?;

        let stdin = child.stdin.take().ok_or_else(|| HostError::Protocol {
            plugin_id: discovered.descriptor.id.clone(),
            message: "plugin stdin was not piped".to_string(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| HostError::Protocol {
            plugin_id: discovered.descriptor.id.clone(),
            message: "plugin stdout was not piped".to_string(),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| HostError::Protocol {
            plugin_id: discovered.descriptor.id.clone(),
            message: "plugin stderr was not piped".to_string(),
        })?;

        let client = JsonLineClientSession::new(stdout, stdin);
        spawn_stderr_reader(stderr, discovered.descriptor.id.clone());

        Ok(Self {
            descriptor: discovered,
            child,
            client,
            config,
        })
    }

    pub fn descriptor(&self) -> &DiscoveredPlugin {
        &self.descriptor
    }

    pub fn send_shutdown(&mut self, reason: Option<String>) -> Result<(), HostError> {
        self.send_host_message(HostMessage::ShutdownRequest { reason })
    }

    pub fn handshake(mut self) -> Result<crate::host::RegisteredPlugin, HostError> {
        self.send_host_message(HostMessage::HandshakeRequest {
            protocol_version: HOST_PROTOCOL_VERSION_V1,
            host: self.config.host_identity.clone(),
            workspace_root: self.config.workspace_root.clone(),
        })?;

        let registration = loop {
            match self.recv_frame(Some(self.config.handshake_timeout))? {
                PluginFrame::Message(PluginMessage::Registration {
                    protocol_version,
                    manifest,
                }) => {
                    if protocol_version != HOST_PROTOCOL_VERSION_V1 {
                        return Err(HostError::UnsupportedManifestVersion {
                            plugin_id: self.descriptor.descriptor.id.clone(),
                            manifest_version: protocol_version,
                        });
                    }
                    if manifest.manifest_version != HOST_PROTOCOL_VERSION_V1 {
                        return Err(HostError::UnsupportedManifestVersion {
                            plugin_id: self.descriptor.descriptor.id.clone(),
                            manifest_version: manifest.manifest_version,
                        });
                    }
                    if manifest.plugin.id != self.descriptor.descriptor.id {
                        return Err(HostError::PluginIdentityMismatch {
                            plugin_id: self.descriptor.descriptor.id.clone(),
                            expected: self.descriptor.descriptor.id.clone(),
                            actual: manifest.plugin.id.clone(),
                        });
                    }
                    break manifest;
                }
                PluginFrame::Message(PluginMessage::Log { .. }) => continue,
                PluginFrame::Message(PluginMessage::ShutdownAck { .. }) => {
                    return Err(HostError::EarlyExit {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                    });
                }
                PluginFrame::Message(PluginMessage::CommandResponse { .. })
                | PluginFrame::Message(PluginMessage::CommandError { .. })
                | PluginFrame::Message(PluginMessage::HookResponse { .. })
                | PluginFrame::Message(PluginMessage::HookError { .. })
                | PluginFrame::Message(PluginMessage::ToolResponse { .. })
                | PluginFrame::Message(PluginMessage::ToolError { .. }) => {
                    return Err(HostError::EarlyExit {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                    });
                }
                PluginFrame::ProtocolError { raw, error } => {
                    return Err(HostError::Protocol {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        message: format!("received malformed frame `{raw}`: {error}"),
                    });
                }
                PluginFrame::StreamClosed => {
                    return Err(HostError::EarlyExit {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                    });
                }
            }
        };

        let capabilities =
            CapabilityIndex::from_manifest(&self.descriptor.descriptor.id, &registration)?;
        Ok(crate::host::RegisteredPlugin {
            session: self,
            manifest: registration,
            capabilities,
        })
    }

    pub fn next_message(&mut self, timeout: Option<Duration>) -> Result<PluginFrame, HostError> {
        self.recv_frame(timeout)
    }

    pub fn invoke_command(
        &mut self,
        request_id: String,
        command_name: String,
        args: Vec<String>,
        cwd: PathBuf,
        session_id: Option<String>,
        raw_input: Option<String>,
        timeout: Duration,
    ) -> Result<String, HostError> {
        self.send_host_message(HostMessage::CommandRequest {
            request_id: request_id.clone(),
            command_name: command_name.clone(),
            args,
            cwd,
            session_id,
            raw_input,
        })?;

        loop {
            match self.recv_frame(Some(timeout)).map_err(|error| match error {
                HostError::HandshakeTimeout { plugin_id, timeout } => HostError::CommandTimeout {
                    plugin_id,
                    command_name: command_name.clone(),
                    timeout,
                },
                other => other,
            })? {
                PluginFrame::Message(PluginMessage::Log { .. }) => continue,
                PluginFrame::Message(PluginMessage::CommandResponse {
                    request_id: response_id,
                    replacement,
                }) if response_id == request_id => return Ok(replacement),
                PluginFrame::Message(PluginMessage::CommandError {
                    request_id: response_id,
                    message,
                    ..
                }) if response_id == request_id => {
                    return Err(HostError::CommandFailed {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        command_name,
                        message,
                    });
                }
                PluginFrame::Message(message) => {
                    return Err(HostError::Protocol {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        message: format!("unexpected plugin response during command invocation: {message:?}"),
                    });
                }
                PluginFrame::ProtocolError { raw, error } => {
                    return Err(HostError::Protocol {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        message: format!("received malformed frame `{raw}`: {error}"),
                    });
                }
                PluginFrame::StreamClosed => {
                    return Err(HostError::EarlyExit {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                    });
                }
            }
        }
    }

    pub fn invoke_hook(
        &mut self,
        request_id: String,
        hook_name: String,
        context: LifecycleHookContextV1,
        timeout: Duration,
    ) -> Result<LifecycleHookOutcomeV1, HostError> {
        self.send_host_message(HostMessage::HookRequest {
            request_id: request_id.clone(),
            hook_name: hook_name.clone(),
            context,
        })?;

        loop {
            match self.recv_frame(Some(timeout)).map_err(|error| match error {
                HostError::HandshakeTimeout { plugin_id, timeout } => HostError::HookTimeout {
                    plugin_id,
                    hook_name: hook_name.clone(),
                    timeout,
                },
                other => other,
            })? {
                PluginFrame::Message(PluginMessage::Log { .. }) => continue,
                PluginFrame::Message(PluginMessage::HookResponse {
                    request_id: response_id,
                    outcome,
                }) if response_id == request_id => return Ok(outcome),
                PluginFrame::Message(PluginMessage::HookError {
                    request_id: response_id,
                    message,
                    ..
                }) if response_id == request_id => {
                    return Err(HostError::HookFailed {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        hook_name,
                        message,
                    });
                }
                PluginFrame::Message(message) => {
                    return Err(HostError::Protocol {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        message: format!("unexpected plugin response during hook invocation: {message:?}"),
                    });
                }
                PluginFrame::ProtocolError { raw, error } => {
                    return Err(HostError::Protocol {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        message: format!("received malformed frame `{raw}`: {error}"),
                    });
                }
                PluginFrame::StreamClosed => {
                    return Err(HostError::EarlyExit {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                    });
                }
            }
        }
    }

    pub fn invoke_tool(
        &mut self,
        request_id: String,
        tool_call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
        cwd: PathBuf,
        session_id: Option<String>,
        timeout: Duration,
    ) -> Result<(Vec<PluginContentBlock>, Option<serde_json::Value>, bool), HostError> {
        self.send_host_message(HostMessage::ToolRequest {
            request_id: request_id.clone(),
            tool_call_id,
            tool_name: tool_name.clone(),
            arguments,
            cwd,
            session_id,
        })?;

        loop {
            match self.recv_frame(Some(timeout)).map_err(|error| match error {
                HostError::HandshakeTimeout { plugin_id, timeout } => HostError::ToolTimeout {
                    plugin_id,
                    tool_name: tool_name.clone(),
                    timeout,
                },
                other => other,
            })? {
                PluginFrame::Message(PluginMessage::Log { .. }) => continue,
                PluginFrame::Message(PluginMessage::ToolResponse {
                    request_id: response_id,
                    content,
                    details,
                    is_error,
                }) if response_id == request_id => return Ok((content, details, is_error)),
                PluginFrame::Message(PluginMessage::ToolError {
                    request_id: response_id,
                    message,
                    ..
                }) if response_id == request_id => {
                    return Err(HostError::ToolFailed {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        tool_name,
                        message,
                    });
                }
                PluginFrame::Message(message) => {
                    return Err(HostError::Protocol {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        message: format!("unexpected plugin response during tool invocation: {message:?}"),
                    });
                }
                PluginFrame::ProtocolError { raw, error } => {
                    return Err(HostError::Protocol {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                        message: format!("received malformed frame `{raw}`: {error}"),
                    });
                }
                PluginFrame::StreamClosed => {
                    return Err(HostError::EarlyExit {
                        plugin_id: self.descriptor.descriptor.id.clone(),
                    });
                }
            }
        }
    }

    fn send_host_message(&mut self, message: HostMessage) -> Result<(), HostError> {
        self.client.send(&message).map_err(|source| HostError::Protocol {
            plugin_id: self.descriptor.descriptor.id.clone(),
            message: source,
        })
    }

    fn recv_frame(&mut self, timeout: Option<Duration>) -> Result<PluginFrame, HostError> {
        self.client.recv_frame(timeout).map_err(|error| match error {
            ClientReceiveError::Timeout => HostError::HandshakeTimeout {
                plugin_id: self.descriptor.descriptor.id.clone(),
                timeout: timeout.unwrap_or_default(),
            },
            ClientReceiveError::Disconnected => HostError::EarlyExit {
                plugin_id: self.descriptor.descriptor.id.clone(),
            },
        })
    }

    fn kill_child(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for PluginSession {
    fn drop(&mut self) {
        self.kill_child();
    }
}

fn resolve_executable(discovered: &DiscoveredPlugin) -> Result<PathBuf, HostError> {
    let executable = &discovered.descriptor.executable;
    let candidate = if executable.is_absolute() {
        executable.clone()
    } else {
        discovered.base_dir().join(executable)
    };
    if !candidate.exists() {
        return Err(HostError::MissingExecutable { path: candidate });
    }
    Ok(candidate)
}

fn resolve_working_directory(discovered: &DiscoveredPlugin) -> PathBuf {
    if let Some(working_directory) = &discovered.descriptor.working_directory {
        if working_directory.is_absolute() {
            return working_directory.clone();
        }
        return discovered.base_dir().join(working_directory);
    }
    discovered.base_dir().to_path_buf()
}

fn spawn_stderr_reader(stderr: std::process::ChildStderr, _plugin_id: String) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buffer = String::new();
        let _ = reader.read_to_string(&mut buffer);
    });
}
