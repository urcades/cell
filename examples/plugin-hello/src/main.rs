use std::error::Error;
use std::io::{self, BufRead, Write};

use pi_rust_plugin_protocol::{
    HostMessage, PluginContentBlock, PluginMessage, PLUGIN_PROTOCOL_VERSION_V1,
};
use pi_rust_plugins::{
    CommandRegistrationV1, LifecycleEventV1, LifecycleHookOutcomeV1,
    LifecycleHookRegistrationV1, ParameterRegistrationV1, PluginIdentityV1, PluginManifestV1,
    ToolRegistrationV1, ValueKindV1,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    let handshake = read_message(&mut stdin)?;
    match handshake {
        HostMessage::HandshakeRequest { .. } => {
            write_message(
                &mut stdout,
                &PluginMessage::Registration {
                    protocol_version: PLUGIN_PROTOCOL_VERSION_V1,
                    manifest: manifest(),
                },
            )?;
        }
        other => {
            return Err(format!("expected handshake request, got {other:?}").into());
        }
    }

    loop {
        let Some(message) = read_optional_message(&mut stdin)? else {
            break;
        };

        match message {
            HostMessage::CommandRequest {
                request_id, args, ..
            } => {
                let replacement = format!("hello:{}", args.join("|"));
                write_message(
                    &mut stdout,
                    &PluginMessage::CommandResponse {
                        request_id,
                        replacement,
                    },
                )?;
            }
            HostMessage::ToolRequest {
                request_id,
                arguments,
                ..
            } => {
                let text = arguments
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("world");
                write_message(
                    &mut stdout,
                    &PluginMessage::ToolResponse {
                        request_id,
                        content: vec![PluginContentBlock::Text {
                            text: format!("tool:{text}"),
                        }],
                        details: Some(json!({ "echo": text })),
                        is_error: false,
                    },
                )?;
            }
            HostMessage::HookRequest { request_id, .. } => {
                write_message(
                    &mut stdout,
                    &PluginMessage::HookResponse {
                        request_id,
                        outcome: LifecycleHookOutcomeV1::Continue,
                    },
                )?;
            }
            HostMessage::ShutdownRequest { .. } => {
                write_message(&mut stdout, &PluginMessage::ShutdownAck { ok: true })?;
                break;
            }
            HostMessage::HandshakeRequest { .. } => {}
        }
    }

    stdout.flush()?;
    Ok(())
}

fn manifest() -> PluginManifestV1 {
    let mut manifest = PluginManifestV1::new(PluginIdentityV1 {
        id: "hello-plugin".to_string(),
        name: "Hello Plugin".to_string(),
        version: "0.1.0".to_string(),
        description: Some("Standalone Rust plugin example".to_string()),
        authors: vec!["pi-rust".to_string()],
        homepage: None,
        repository: None,
        license: Some("MIT".to_string()),
    });

    manifest.commands.push(CommandRegistrationV1 {
        name: "hello".to_string(),
        description: Some("Rewrite input as a hello response".to_string()),
        aliases: vec![],
        parameters: vec![ParameterRegistrationV1 {
            name: "name".to_string(),
            kind: ValueKindV1::String,
            required: false,
            description: Some("Name to greet".to_string()),
            default_value: None,
        }],
        hidden: false,
    });

    manifest.tools.push(ToolRegistrationV1 {
        name: "echo".to_string(),
        description: Some("Echo a text field".to_string()),
        aliases: vec![],
        parameters: vec![ParameterRegistrationV1 {
            name: "text".to_string(),
            kind: ValueKindV1::String,
            required: true,
            description: Some("Text to echo".to_string()),
            default_value: None,
        }],
        output: Some(ValueKindV1::String),
        hidden: false,
    });

    manifest.hooks.push(LifecycleHookRegistrationV1 {
        event: LifecycleEventV1::SessionStarted,
        name: "session-started".to_string(),
        description: Some("Observe the first session event".to_string()),
        priority: 0,
    });

    manifest
}

fn read_message(stdin: &mut impl BufRead) -> Result<HostMessage, Box<dyn Error>> {
    let mut line = String::new();
    let bytes = stdin.read_line(&mut line)?;
    if bytes == 0 {
        return Err("unexpected EOF during handshake".into());
    }
    Ok(serde_json::from_str(line.trim_end())?)
}

fn read_optional_message(stdin: &mut impl BufRead) -> Result<Option<HostMessage>, Box<dyn Error>> {
    loop {
        let mut line = String::new();
        let bytes = stdin.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            continue;
        }
        return Ok(Some(serde_json::from_str(line.trim_end())?));
    }
}

fn write_message(
    stdout: &mut impl Write,
    message: &PluginMessage,
) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *stdout, message)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
