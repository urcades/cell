mod help;
mod interactive;
mod keybindings;

use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use pi_rust_ai_core::{Message, UserContent, UserContentBlock, UserMessage};
use pi_rust_ai_providers::{ProviderRegistry, register_builtin_providers};
use pi_rust_config::get_agent_dir;
use pi_rust_core::{
    AgentControl, AgentSession, NonInteractiveRequest, create_agent_session,
    export_session_file_to_html, list_models, rpc_event_from_agent_event, run_non_interactive,
};
use pi_rust_models::ModelRegistry;
use pi_rust_oauth::AuthStorage;
use pi_rust_packages::{PackageInstallScope, PackageManager};
use pi_rust_protocol::{OutputMode, RpcCommand, RpcInbound, RpcResponse};
use serde::Serialize;
use serde_json::json;

pub use help::{
    render_help_text, render_package_command_help, render_package_command_usage,
    render_version_text,
};

const VALID_THINKING_LEVELS: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionFlagType {
    Boolean,
    String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionFlagValue {
    Boolean(bool),
    String(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageCommand {
    Install,
    Remove,
    Update,
    List,
}

impl PackageCommand {
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "install" => Some(Self::Install),
            "remove" => Some(Self::Remove),
            "update" => Some(Self::Update),
            "list" => Some(Self::List),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Remove => "remove",
            Self::Update => "update",
            Self::List => "list",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PackageCommandOptions {
    command: PackageCommand,
    source: Option<String>,
    local: bool,
    help: bool,
    invalid_option: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub thinking: Option<String>,
    pub continue_session: bool,
    pub resume: bool,
    pub help: bool,
    pub version: bool,
    pub mode: Option<OutputMode>,
    pub no_session: bool,
    pub session: Option<String>,
    pub session_dir: Option<String>,
    pub models: Option<Vec<String>>,
    pub tools: Option<Vec<String>>,
    pub no_tools: bool,
    pub extensions: Vec<String>,
    pub no_extensions: bool,
    pub print: bool,
    pub export: Option<String>,
    pub no_skills: bool,
    pub skills: Vec<String>,
    pub prompt_templates: Vec<String>,
    pub no_prompt_templates: bool,
    pub themes: Vec<String>,
    pub no_themes: bool,
    pub list_models: Option<Option<String>>,
    pub verbose: bool,
    pub messages: Vec<String>,
    pub file_args: Vec<String>,
    pub unknown_flags: BTreeMap<String, ExtensionFlagValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunResult {
    Completed {
        exit_code: i32,
        stdout: Option<String>,
        stderr: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliError {
    UnsupportedExtensions,
    PackageCommandNotImplemented(PackageCommand),
    TuiRequiresTerminal(&'static str),
    InteractiveFailed(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::UnsupportedExtensions => {
                write!(
                    f,
                    "pi-rust v1 does not execute JS/TS extensions. Remove --extension/--no-extensions and any extension-only package resources."
                )
            }
            CliError::PackageCommandNotImplemented(command) => {
                write!(
                    f,
                    "pi-rust recognizes the \"{}\" package command, but package execution is not implemented yet.",
                    command.as_str()
                )
            }
            CliError::TuiRequiresTerminal(command) => {
                write!(f, "{command} requires an interactive terminal (TTY).")
            }
            CliError::InteractiveFailed(message) => write!(f, "{message}"),
        }
    }
}

pub fn parse_args(args: &[String]) -> Args {
    parse_args_with_extension_flags(args, None)
}

pub fn parse_args_with_extension_flags(
    args: &[String],
    extension_flags: Option<&BTreeMap<String, ExtensionFlagType>>,
) -> Args {
    let mut result = Args::default();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--help" | "-h" => result.help = true,
            "--version" | "-v" => result.version = true,
            "--mode" if index + 1 < args.len() => {
                index += 1;
                result.mode = match args[index].as_str() {
                    "text" => Some(OutputMode::Text),
                    "json" => Some(OutputMode::Json),
                    "rpc" => Some(OutputMode::Rpc),
                    _ => result.mode,
                };
            }
            "--continue" | "-c" => result.continue_session = true,
            "--resume" | "-r" => result.resume = true,
            "--provider" if index + 1 < args.len() => {
                index += 1;
                result.provider = Some(args[index].clone());
            }
            "--model" if index + 1 < args.len() => {
                index += 1;
                result.model = Some(args[index].clone());
            }
            "--api-key" if index + 1 < args.len() => {
                index += 1;
                result.api_key = Some(args[index].clone());
            }
            "--system-prompt" if index + 1 < args.len() => {
                index += 1;
                result.system_prompt = Some(args[index].clone());
            }
            "--append-system-prompt" if index + 1 < args.len() => {
                index += 1;
                result.append_system_prompt = Some(args[index].clone());
            }
            "--no-session" => result.no_session = true,
            "--session" if index + 1 < args.len() => {
                index += 1;
                result.session = Some(args[index].clone());
            }
            "--session-dir" if index + 1 < args.len() => {
                index += 1;
                result.session_dir = Some(args[index].clone());
            }
            "--models" if index + 1 < args.len() => {
                index += 1;
                result.models = Some(split_csv_preserving_empties(&args[index]));
            }
            "--no-tools" => result.no_tools = true,
            "--tools" if index + 1 < args.len() => {
                index += 1;
                result.tools = Some(parse_tool_names(&args[index]));
            }
            "--thinking" if index + 1 < args.len() => {
                index += 1;
                if is_valid_thinking_level(&args[index]) {
                    result.thinking = Some(args[index].clone());
                }
            }
            "--print" | "-p" => result.print = true,
            "--export" if index + 1 < args.len() => {
                index += 1;
                result.export = Some(args[index].clone());
            }
            "--extension" | "-e" if index + 1 < args.len() => {
                index += 1;
                result.extensions.push(args[index].clone());
            }
            "--no-extensions" | "-ne" => result.no_extensions = true,
            "--skill" if index + 1 < args.len() => {
                index += 1;
                result.skills.push(args[index].clone());
            }
            "--prompt-template" if index + 1 < args.len() => {
                index += 1;
                result.prompt_templates.push(args[index].clone());
            }
            "--theme" if index + 1 < args.len() => {
                index += 1;
                result.themes.push(args[index].clone());
            }
            "--no-skills" | "-ns" => result.no_skills = true,
            "--no-prompt-templates" | "-np" => result.no_prompt_templates = true,
            "--no-themes" => result.no_themes = true,
            "--list-models" => {
                if index + 1 < args.len()
                    && !args[index + 1].starts_with('-')
                    && !args[index + 1].starts_with('@')
                {
                    index += 1;
                    result.list_models = Some(Some(args[index].clone()));
                } else {
                    result.list_models = Some(None);
                }
            }
            "--verbose" => result.verbose = true,
            value if value.starts_with('@') => result
                .file_args
                .push(value.trim_start_matches('@').to_string()),
            value if value.starts_with("--") => {
                if let Some(extension_flags) = extension_flags {
                    let flag_name = value.trim_start_matches("--");
                    if let Some(flag_type) = extension_flags.get(flag_name) {
                        match flag_type {
                            ExtensionFlagType::Boolean => {
                                result.unknown_flags.insert(
                                    flag_name.to_string(),
                                    ExtensionFlagValue::Boolean(true),
                                );
                            }
                            ExtensionFlagType::String if index + 1 < args.len() => {
                                index += 1;
                                result.unknown_flags.insert(
                                    flag_name.to_string(),
                                    ExtensionFlagValue::String(args[index].clone()),
                                );
                            }
                            ExtensionFlagType::String => {}
                        }
                    }
                }
            }
            value if !value.starts_with('-') => result.messages.push(value.to_string()),
            _ => {}
        }
        index += 1;
    }

    result
}

pub fn run(args: &[String]) -> Result<RunResult, CliError> {
    let mut providers = ProviderRegistry::new();
    register_builtin_providers(&mut providers);
    let mut models = ModelRegistry::new(AuthStorage::create(None), None);
    run_with_services(args, &mut providers, &mut models)
}

pub fn run_rpc_stdio(args: &[String]) -> Result<i32, String> {
    let mut providers = ProviderRegistry::new();
    register_builtin_providers(&mut providers);
    let mut models = ModelRegistry::new(AuthStorage::create(None), None);
    run_rpc_stdio_with_services(args, &mut providers, &mut models)
}

pub fn run_with_services(
    args: &[String],
    providers: &mut ProviderRegistry,
    models: &mut ModelRegistry,
) -> Result<RunResult, CliError> {
    if let Some(package_command) = parse_package_command(args) {
        return run_package_command(package_command);
    }

    let parsed = parse_args(args);
    if !parsed.extensions.is_empty() || parsed.no_extensions {
        return Err(CliError::UnsupportedExtensions);
    }
    if parsed.version {
        return Ok(RunResult::Completed {
            exit_code: 0,
            stdout: Some(render_version_text().to_string()),
            stderr: None,
        });
    }
    if parsed.help {
        return Ok(RunResult::Completed {
            exit_code: 0,
            stdout: Some(render_help_text().to_string()),
            stderr: None,
        });
    }
    if parsed.list_models.is_some() {
        let search = parsed
            .list_models
            .as_ref()
            .and_then(|value| value.as_deref());
        return Ok(RunResult::Completed {
            exit_code: 0,
            stdout: Some(list_models(models, search)),
            stderr: None,
        });
    }
    if let Some(export_target) = &parsed.export {
        return run_export_command(export_target, &parsed.messages);
    }

    let mode = parsed.mode.unwrap_or(OutputMode::Text);
    let is_interactive = !parsed.print && parsed.mode.is_none();
    if matches!(args.first().map(String::as_str), Some("config")) {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err(CliError::TuiRequiresTerminal("config"));
        }
        interactive::run_config_tui().map_err(CliError::InteractiveFailed)?;
        return Ok(RunResult::Completed {
            exit_code: 0,
            stdout: None,
            stderr: None,
        });
    }
    if is_interactive {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err(CliError::TuiRequiresTerminal("Interactive mode"));
        }

        interactive::run_interactive(
            NonInteractiveRequest {
                cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                mode: OutputMode::Text,
                provider: parsed.provider.clone(),
                model: parsed.model.clone(),
                api_key: parsed.api_key.clone(),
                system_prompt: parsed.system_prompt.clone(),
                append_system_prompt: parsed.append_system_prompt.clone(),
                initial_message: None,
                messages: parsed.messages.clone(),
                continue_session: parsed.continue_session || parsed.resume,
                no_session: parsed.no_session,
                session: parsed.session.as_ref().map(PathBuf::from),
                session_dir: parsed.session_dir.as_ref().map(PathBuf::from),
                models: parsed.models.clone(),
                no_tools: parsed.no_tools,
                tools: parsed.tools.clone(),
                thinking: parsed.thinking.clone(),
                no_skills: parsed.no_skills,
                skills: parsed.skills.iter().map(PathBuf::from).collect(),
                prompt_templates: parsed.prompt_templates.iter().map(PathBuf::from).collect(),
                no_prompt_templates: parsed.no_prompt_templates,
                themes: parsed.themes.iter().map(PathBuf::from).collect(),
                no_themes: parsed.no_themes,
            },
            parsed.resume,
            providers,
            models,
        )
        .map_err(CliError::InteractiveFailed)?;

        return Ok(RunResult::Completed {
            exit_code: 0,
            stdout: None,
            stderr: None,
        });
    }
    if mode == OutputMode::Rpc {
        return Err(CliError::InteractiveFailed(
            "Use run_rpc_stdio for RPC execution.".to_string(),
        ));
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(run_non_interactive(
        NonInteractiveRequest {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            mode,
            provider: parsed.provider.clone(),
            model: parsed.model.clone(),
            api_key: parsed.api_key.clone(),
            system_prompt: parsed.system_prompt.clone(),
            append_system_prompt: parsed.append_system_prompt.clone(),
            initial_message: None,
            messages: parsed.messages.clone(),
            continue_session: parsed.continue_session,
            no_session: parsed.no_session,
            session: parsed.session.as_ref().map(PathBuf::from),
            session_dir: parsed.session_dir.as_ref().map(PathBuf::from),
            models: parsed.models.clone(),
            no_tools: parsed.no_tools,
            tools: parsed.tools.clone(),
            thinking: parsed.thinking.clone(),
            no_skills: parsed.no_skills,
            skills: parsed.skills.iter().map(PathBuf::from).collect(),
            prompt_templates: parsed.prompt_templates.iter().map(PathBuf::from).collect(),
            no_prompt_templates: parsed.no_prompt_templates,
            themes: parsed.themes.iter().map(PathBuf::from).collect(),
            no_themes: parsed.no_themes,
        },
        providers,
        models,
    ));

    match result {
        Ok(result) => Ok(RunResult::Completed {
            exit_code: result.exit_code,
            stdout: if result.stdout.is_empty() {
                None
            } else {
                Some(result.stdout.join("\n"))
            },
            stderr: if result.stderr.is_empty() {
                None
            } else {
                Some(result.stderr.join("\n"))
            },
        }),
        Err(error) => Ok(RunResult::Completed {
            exit_code: 1,
            stdout: None,
            stderr: Some(error.to_string()),
        }),
    }
}

pub fn run_rpc_stdio_with_services(
    args: &[String],
    providers: &mut ProviderRegistry,
    models: &mut ModelRegistry,
) -> Result<i32, String> {
    let parsed = parse_args(args);
    if !parsed.extensions.is_empty() || parsed.no_extensions {
        return Err(CliError::UnsupportedExtensions.to_string());
    }

    let request = NonInteractiveRequest {
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        mode: OutputMode::Rpc,
        provider: parsed.provider.clone(),
        model: parsed.model.clone(),
        api_key: parsed.api_key.clone(),
        system_prompt: parsed.system_prompt.clone(),
        append_system_prompt: parsed.append_system_prompt.clone(),
        initial_message: None,
        messages: Vec::new(),
        continue_session: parsed.continue_session,
        no_session: parsed.no_session,
        session: parsed.session.as_ref().map(PathBuf::from),
        session_dir: parsed.session_dir.as_ref().map(PathBuf::from),
        models: parsed.models.clone(),
        no_tools: parsed.no_tools,
        tools: parsed.tools.clone(),
        thinking: parsed.thinking.clone(),
        no_skills: parsed.no_skills,
        skills: parsed.skills.iter().map(PathBuf::from).collect(),
        prompt_templates: parsed.prompt_templates.iter().map(PathBuf::from).collect(),
        no_prompt_templates: parsed.no_prompt_templates,
        themes: parsed.themes.iter().map(PathBuf::from).collect(),
        no_themes: parsed.no_themes,
    };

    let session =
        create_agent_session(&request, providers, models).map_err(|error| error.to_string())?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_rpc_with_io(stdin, stdout.lock(), session)
}

fn parse_package_command(args: &[String]) -> Option<PackageCommandOptions> {
    let (command, rest) = args.split_first()?;
    let command = PackageCommand::from_str(command)?;

    let mut options = PackageCommandOptions {
        command,
        source: None,
        local: false,
        help: false,
        invalid_option: None,
    };

    for arg in rest {
        match arg.as_str() {
            "-h" | "--help" => options.help = true,
            "-l" | "--local" => {
                if matches!(command, PackageCommand::Install | PackageCommand::Remove) {
                    options.local = true;
                } else if options.invalid_option.is_none() {
                    options.invalid_option = Some(arg.clone());
                }
            }
            value if value.starts_with('-') => {
                if options.invalid_option.is_none() {
                    options.invalid_option = Some(value.to_string());
                }
            }
            value => {
                if options.source.is_none() {
                    options.source = Some(value.to_string());
                }
            }
        }
    }

    Some(options)
}

fn handle_rpc_command(
    session: &mut AgentSession,
    writer: &mut impl Write,
    command: RpcCommand,
) -> Result<(), String> {
    let command_name = command.command_name().to_string();

    match command {
        RpcCommand::Prompt { .. }
        | RpcCommand::Steer { .. }
        | RpcCommand::FollowUp { .. }
        | RpcCommand::Abort { .. } => {
            return Err("Streaming commands must be handled by the RPC runtime.".to_string());
        }
        RpcCommand::NewSession { id, parent_session } => {
            let cancelled = session
                .new_session(parent_session.as_deref())
                .map_err(|error| error.to_string())?;
            write_json_line(
                writer,
                &RpcResponse::success(id, "new_session", Some(json!({ "cancelled": cancelled }))),
            )?;
        }
        RpcCommand::GetState { id } => {
            write_success(writer, id, "get_state", &session.get_state())?;
        }
        RpcCommand::SetModel {
            id,
            provider,
            model_id,
        } => {
            let model = session
                .set_model(&provider, &model_id)
                .map_err(|error| error.to_string())?;
            write_success(writer, id, "set_model", &model)?;
        }
        RpcCommand::CycleModel { id } => {
            let result = session.cycle_model().map_err(|error| error.to_string())?;
            write_success(writer, id, "cycle_model", &result)?;
        }
        RpcCommand::GetAvailableModels { id } => {
            write_success(
                writer,
                id,
                "get_available_models",
                &json!({ "models": session.get_available_models() }),
            )?;
        }
        RpcCommand::SetThinkingLevel { id, level } => {
            session
                .set_thinking_level(&level)
                .map_err(|error| error.to_string())?;
            write_json_line(
                writer,
                &RpcResponse::success(id, "set_thinking_level", None),
            )?;
        }
        RpcCommand::CycleThinkingLevel { id } => {
            let result = session
                .cycle_thinking_level()
                .map_err(|error| error.to_string())?;
            write_success(
                writer,
                id,
                "cycle_thinking_level",
                &result.map(|level| json!({ "level": level })),
            )?;
        }
        RpcCommand::SetSteeringMode { id, mode } => {
            session.set_steering_mode(mode);
            write_json_line(writer, &RpcResponse::success(id, "set_steering_mode", None))?;
        }
        RpcCommand::SetFollowUpMode { id, mode } => {
            session.set_follow_up_mode(mode);
            write_json_line(
                writer,
                &RpcResponse::success(id, "set_follow_up_mode", None),
            )?;
        }
        RpcCommand::Compact {
            id,
            custom_instructions,
        } => {
            let result = session
                .compact(custom_instructions.as_deref())
                .map_err(|error| error.to_string())?;
            write_success(writer, id, "compact", &result)?;
        }
        RpcCommand::SetAutoCompaction { id, enabled } => {
            session.set_auto_compaction(enabled);
            write_json_line(
                writer,
                &RpcResponse::success(id, "set_auto_compaction", None),
            )?;
        }
        RpcCommand::SetAutoRetry { id, enabled } => {
            session.set_auto_retry(enabled);
            write_json_line(writer, &RpcResponse::success(id, "set_auto_retry", None))?;
        }
        RpcCommand::AbortRetry { id } => match session.abort_retry() {
            Ok(()) => write_json_line(writer, &RpcResponse::success(id, "abort_retry", None))?,
            Err(error) => {
                write_json_line(
                    writer,
                    &RpcResponse::error(id, "abort_retry", error.to_string()),
                )?;
            }
        },
        RpcCommand::Bash { id, command } => {
            let result = session.bash(&command).map_err(|error| error.to_string())?;
            write_success(writer, id, "bash", &result)?;
        }
        RpcCommand::AbortBash { id } => match session.abort_bash() {
            Ok(()) => write_json_line(writer, &RpcResponse::success(id, "abort_bash", None))?,
            Err(error) => {
                write_json_line(
                    writer,
                    &RpcResponse::error(id, "abort_bash", error.to_string()),
                )?;
            }
        },
        RpcCommand::GetSessionStats { id } => {
            write_success(
                writer,
                id,
                "get_session_stats",
                &session.get_session_stats(),
            )?;
        }
        RpcCommand::ExportHtml { id, output_path } => {
            let path = session
                .export_html(output_path.as_deref().map(PathBuf::from).as_deref())
                .map_err(|error| error.to_string())?;
            write_success(
                writer,
                id,
                "export_html",
                &json!({ "path": path.to_string_lossy() }),
            )?;
        }
        RpcCommand::SwitchSession { id, session_path } => {
            let cancelled = session
                .switch_session(&session_path)
                .map_err(|error| error.to_string())?;
            write_success(
                writer,
                id,
                "switch_session",
                &json!({ "cancelled": cancelled }),
            )?;
        }
        RpcCommand::Fork { id, entry_id } => {
            let (text, cancelled) = session.fork(&entry_id).map_err(|error| error.to_string())?;
            write_success(
                writer,
                id,
                "fork",
                &json!({ "text": text, "cancelled": cancelled }),
            )?;
        }
        RpcCommand::GetForkMessages { id } => {
            write_success(
                writer,
                id,
                "get_fork_messages",
                &json!({ "messages": session.get_fork_messages() }),
            )?;
        }
        RpcCommand::GetLastAssistantText { id } => {
            write_success(
                writer,
                id,
                "get_last_assistant_text",
                &json!({ "text": session.get_last_assistant_text() }),
            )?;
        }
        RpcCommand::SetSessionName { id, name } => {
            session
                .set_session_name(&name)
                .map_err(|error| error.to_string())?;
            write_json_line(writer, &RpcResponse::success(id, "set_session_name", None))?;
        }
        RpcCommand::GetMessages { id } => {
            write_success(
                writer,
                id,
                "get_messages",
                &json!({ "messages": session.get_messages() }),
            )?;
        }
        RpcCommand::GetCommands { id } => {
            write_success(
                writer,
                id,
                "get_commands",
                &json!({ "commands": session.get_commands() }),
            )?;
        }
    }

    if command_name.is_empty() {
        return Err("Unknown command".to_string());
    }
    Ok(())
}

fn run_rpc_with_io(
    reader: impl Read + Send + 'static,
    mut writer: impl Write,
    session: AgentSession,
) -> Result<i32, String> {
    let shared_session = Arc::new(Mutex::new(session));
    let control = shared_session
        .lock()
        .map_err(|_| "Failed to lock RPC session".to_string())?
        .control();
    let (inbound_tx, inbound_rx) = mpsc::channel::<Result<RpcInbound, String>>();
    let (outbound_tx, outbound_rx) = mpsc::channel::<String>();

    let reader_thread = thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if line.trim().is_empty() => continue,
                Ok(_) => {
                    let parsed = serde_json::from_str::<RpcInbound>(&line)
                        .map_err(|error| format!("Failed to parse command: {error}"));
                    let _ = inbound_tx.send(parsed);
                }
                Err(error) => {
                    let _ = inbound_tx.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });

    let mut prompt_handle: Option<thread::JoinHandle<()>> = None;
    let mut reader_closed = false;

    loop {
        drain_outbound_lines(&mut writer, &outbound_rx)?;

        if prompt_handle
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
        {
            if let Some(handle) = prompt_handle.take() {
                let _ = handle.join();
            }
        }

        if reader_closed && prompt_handle.is_none() {
            break;
        }

        match inbound_rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(RpcInbound::ExtensionUiResponse(_))) => {}
            Ok(Ok(RpcInbound::Command(command))) => {
                let prompt_active = prompt_handle
                    .as_ref()
                    .is_some_and(|handle| !handle.is_finished());
                if prompt_active {
                    if is_midstream_rpc_command(&command) {
                        handle_streaming_rpc_command(&control, &mut writer, command)?;
                    } else {
                        wait_for_prompt_completion(&mut prompt_handle, &outbound_rx, &mut writer)?;
                        let mut session = shared_session
                            .lock()
                            .map_err(|_| "Failed to lock RPC session".to_string())?;
                        handle_rpc_command(&mut session, &mut writer, command)?;
                    }
                    continue;
                }

                match command {
                    RpcCommand::Prompt {
                        id,
                        message,
                        images,
                        streaming_behavior: _,
                    } => {
                        let prompt_message = match user_rpc_message(message, images) {
                            Ok(message) => message,
                            Err(error) => {
                                write_json_line(
                                    &mut writer,
                                    &RpcResponse::error(id, "prompt", error),
                                )?;
                                continue;
                            }
                        };
                        write_json_line(
                            &mut writer,
                            &RpcResponse::success(id.clone(), "prompt", None),
                        )?;
                        let session = Arc::clone(&shared_session);
                        let outbound_tx = outbound_tx.clone();
                        let (started_tx, started_rx) = mpsc::channel();
                        prompt_handle = Some(thread::spawn(move || {
                            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
                            let outcome = {
                                let mut session = session.lock().expect("prompt session lock");
                                session.prepare_prompt();
                                let _ = started_tx.send(());
                                runtime.block_on(session.prompt_message_prepared(prompt_message))
                            };
                            match outcome {
                                Ok(run) => {
                                    for event in run.events {
                                        let _ = outbound_tx.send(
                                            serialize_json_line(&rpc_event_from_agent_event(event))
                                                .expect("serialize rpc event"),
                                        );
                                    }
                                }
                                Err(error) => {
                                    let _ = outbound_tx.send(
                                        serialize_json_line(&RpcResponse::error(
                                            id,
                                            "prompt",
                                            error.to_string(),
                                        ))
                                        .expect("serialize rpc error"),
                                    );
                                }
                            }
                        }));
                        started_rx
                            .recv()
                            .map_err(|_| "Failed to start prompt worker".to_string())?;
                    }
                    RpcCommand::Steer {
                        id,
                        message,
                        images,
                    } => match user_rpc_message(message, images) {
                        Ok(message) => {
                            control.steer(message);
                            write_json_line(&mut writer, &RpcResponse::success(id, "steer", None))?;
                        }
                        Err(error) => {
                            write_json_line(&mut writer, &RpcResponse::error(id, "steer", error))?;
                        }
                    },
                    RpcCommand::FollowUp {
                        id,
                        message,
                        images,
                    } => match user_rpc_message(message, images) {
                        Ok(message) => {
                            control.follow_up(message);
                            write_json_line(
                                &mut writer,
                                &RpcResponse::success(id, "follow_up", None),
                            )?;
                        }
                        Err(error) => {
                            write_json_line(
                                &mut writer,
                                &RpcResponse::error(id, "follow_up", error),
                            )?;
                        }
                    },
                    RpcCommand::Abort { id } => {
                        control.abort();
                        write_json_line(&mut writer, &RpcResponse::success(id, "abort", None))?;
                    }
                    other => {
                        let mut session = shared_session
                            .lock()
                            .map_err(|_| "Failed to lock RPC session".to_string())?;
                        handle_rpc_command(&mut session, &mut writer, other)?;
                    }
                }
            }
            Ok(Err(error)) => {
                write_json_line(&mut writer, &RpcResponse::error(None, "parse", error))?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                reader_closed = true;
            }
        }
    }

    if let Some(handle) = prompt_handle {
        let _ = handle.join();
    }
    let _ = reader_thread.join();
    Ok(0)
}

fn handle_streaming_rpc_command(
    control: &AgentControl,
    writer: &mut impl Write,
    command: RpcCommand,
) -> Result<(), String> {
    match command {
        RpcCommand::Prompt {
            id,
            message,
            images,
            streaming_behavior,
        } => match streaming_behavior.as_deref() {
            Some("steer") => match user_rpc_message(message, images) {
                Ok(message) => {
                    control.steer(message);
                    write_json_line(writer, &RpcResponse::success(id, "prompt", None))
                }
                Err(error) => write_json_line(writer, &RpcResponse::error(id, "prompt", error)),
            },
            Some("followUp") => match user_rpc_message(message, images) {
                Ok(message) => {
                    control.follow_up(message);
                    write_json_line(writer, &RpcResponse::success(id, "prompt", None))
                }
                Err(error) => write_json_line(writer, &RpcResponse::error(id, "prompt", error)),
            },
            _ => write_json_line(
                writer,
                &RpcResponse::error(
                    id,
                    "prompt",
                    "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.",
                ),
            ),
        },
        RpcCommand::Steer {
            id,
            message,
            images,
        } => match user_rpc_message(message, images) {
            Ok(message) => {
                control.steer(message);
                write_json_line(writer, &RpcResponse::success(id, "steer", None))
            }
            Err(error) => write_json_line(writer, &RpcResponse::error(id, "steer", error)),
        },
        RpcCommand::FollowUp {
            id,
            message,
            images,
        } => match user_rpc_message(message, images) {
            Ok(message) => {
                control.follow_up(message);
                write_json_line(writer, &RpcResponse::success(id, "follow_up", None))
            }
            Err(error) => write_json_line(writer, &RpcResponse::error(id, "follow_up", error)),
        },
        RpcCommand::Abort { id } => {
            control.abort();
            write_json_line(writer, &RpcResponse::success(id, "abort", None))
        }
        other => write_json_line(
            writer,
            &RpcResponse::error(
                other.id().map(ToOwned::to_owned),
                other.command_name(),
                "Command is unavailable while a prompt is streaming.",
            ),
        ),
    }
}

fn is_midstream_rpc_command(command: &RpcCommand) -> bool {
    matches!(
        command,
        RpcCommand::Prompt { .. }
            | RpcCommand::Steer { .. }
            | RpcCommand::FollowUp { .. }
            | RpcCommand::Abort { .. }
    )
}

fn wait_for_prompt_completion(
    prompt_handle: &mut Option<thread::JoinHandle<()>>,
    outbound_rx: &mpsc::Receiver<String>,
    writer: &mut impl Write,
) -> Result<(), String> {
    while prompt_handle
        .as_ref()
        .is_some_and(|handle| !handle.is_finished())
    {
        drain_outbound_lines(writer, outbound_rx)?;
        thread::sleep(Duration::from_millis(10));
    }
    if let Some(handle) = prompt_handle.take() {
        let _ = handle.join();
    }
    drain_outbound_lines(writer, outbound_rx)
}

fn write_success<T: Serialize>(
    writer: &mut impl Write,
    id: Option<String>,
    command: &str,
    data: &T,
) -> Result<(), String> {
    let data = serde_json::to_value(data).map_err(|error| error.to_string())?;
    write_json_line(writer, &RpcResponse::success(id, command, Some(data)))
}

fn write_json_line(writer: &mut impl Write, value: &impl Serialize) -> Result<(), String> {
    let line = serialize_json_line(value)?;
    writer
        .write_all(line.as_bytes())
        .map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())
}

fn serialize_json_line(value: &impl Serialize) -> Result<String, String> {
    let mut buffer = Vec::new();
    serde_json::to_writer(&mut buffer, value).map_err(|error| error.to_string())?;
    buffer.push(b'\n');
    String::from_utf8(buffer).map_err(|error| error.to_string())
}

fn user_rpc_message(
    text: String,
    images: Option<Vec<serde_json::Value>>,
) -> Result<Message, String> {
    let mut content = Vec::new();
    if !text.is_empty() || images.as_ref().is_none_or(Vec::is_empty) {
        content.push(UserContentBlock::Text {
            text,
            text_signature: None,
        });
    }

    for image in images.unwrap_or_default() {
        content.push(parse_rpc_image(image)?);
    }

    Ok(Message::User(UserMessage {
        content: UserContent::Blocks(content),
        timestamp: 0,
    }))
}

fn parse_rpc_image(image: serde_json::Value) -> Result<UserContentBlock, String> {
    let object = image
        .as_object()
        .ok_or_else(|| "RPC image payload must be an object.".to_string())?;
    if let Some(image_type) = object.get("type").and_then(serde_json::Value::as_str) {
        if image_type != "image" {
            return Err(format!("Unsupported RPC image payload type: {image_type}"));
        }
    }

    let data = object
        .get("data")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "RPC image payload is missing data.".to_string())?;
    let mime_type = object
        .get("mimeType")
        .or_else(|| object.get("mime_type"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "RPC image payload is missing mimeType.".to_string())?;

    Ok(UserContentBlock::Image {
        data: data.to_string(),
        mime_type: mime_type.to_string(),
    })
}

fn drain_outbound_lines(
    writer: &mut impl Write,
    outbound_rx: &mpsc::Receiver<String>,
) -> Result<(), String> {
    while let Ok(line) = outbound_rx.try_recv() {
        writer
            .write_all(line.as_bytes())
            .map_err(|error| error.to_string())?;
        writer.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn run_package_command(options: PackageCommandOptions) -> Result<RunResult, CliError> {
    if options.help {
        return Ok(RunResult::Completed {
            exit_code: 0,
            stdout: Some(
                render_package_command_help(options.command)
                    .trim_end()
                    .to_string(),
            ),
            stderr: None,
        });
    }

    if let Some(invalid_option) = options.invalid_option {
        return Ok(RunResult::Completed {
            exit_code: 1,
            stdout: None,
            stderr: Some(format!(
                "Unknown option {invalid_option} for \"{}\".\nUse \"pi-rust --help\" or \"{}\".",
                options.command.as_str(),
                render_package_command_usage(options.command)
            )),
        });
    }

    if matches!(
        options.command,
        PackageCommand::Install | PackageCommand::Remove
    ) && options.source.is_none()
    {
        return Ok(RunResult::Completed {
            exit_code: 1,
            stdout: None,
            stderr: Some(format!(
                "Missing {} source.\nUsage: {}",
                options.command.as_str(),
                render_package_command_usage(options.command)
            )),
        });
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let agent_dir = Some(get_agent_dir());
    let mut package_manager = PackageManager::create(&cwd, agent_dir);
    let scope = if options.local {
        PackageInstallScope::Project
    } else {
        PackageInstallScope::User
    };

    match options.command {
        PackageCommand::Install => match package_manager.install(
            options.source.as_deref().expect("validated install source"),
            scope,
        ) {
            Ok(installed) => Ok(RunResult::Completed {
                exit_code: 0,
                stdout: Some(format!(
                    "Installing {}...\nInstalled {}",
                    installed.source, installed.source
                )),
                stderr: None,
            }),
            Err(error) => Ok(RunResult::Completed {
                exit_code: 1,
                stdout: None,
                stderr: Some(format!("Error: {error}")),
            }),
        },
        PackageCommand::Remove => match package_manager.remove(
            options.source.as_deref().expect("validated remove source"),
            scope,
        ) {
            Ok(true) => Ok(RunResult::Completed {
                exit_code: 0,
                stdout: Some(format!(
                    "Removing {}...\nRemoved {}",
                    options.source.as_deref().expect("validated remove source"),
                    options.source.as_deref().expect("validated remove source")
                )),
                stderr: None,
            }),
            Ok(false) => Ok(RunResult::Completed {
                exit_code: 1,
                stdout: None,
                stderr: Some(format!(
                    "No matching package found for {}",
                    options.source.as_deref().expect("validated remove source")
                )),
            }),
            Err(error) => Ok(RunResult::Completed {
                exit_code: 1,
                stdout: None,
                stderr: Some(format!("Error: {error}")),
            }),
        },
        PackageCommand::Update => match package_manager.update(options.source.as_deref()) {
            Ok(_) => Ok(RunResult::Completed {
                exit_code: 0,
                stdout: Some(if let Some(source) = options.source {
                    format!("Updating {source}...\nUpdated {source}")
                } else {
                    "Updating packages...\nUpdated packages".to_string()
                }),
                stderr: None,
            }),
            Err(error) => Ok(RunResult::Completed {
                exit_code: 1,
                stdout: None,
                stderr: Some(format!("Error: {error}")),
            }),
        },
        PackageCommand::List => {
            let global_packages = package_manager.list_by_scope(PackageInstallScope::User);
            let project_packages = package_manager.list_by_scope(PackageInstallScope::Project);
            if global_packages.is_empty() && project_packages.is_empty() {
                return Ok(RunResult::Completed {
                    exit_code: 0,
                    stdout: Some("No packages installed.".to_string()),
                    stderr: None,
                });
            }

            let mut lines = Vec::new();
            if !global_packages.is_empty() {
                lines.push("User packages:".to_string());
                for package in global_packages {
                    lines.push(format!("  {}", package.source));
                    lines.push(format!("    {}", package.install_path.to_string_lossy()));
                }
            }
            if !project_packages.is_empty() {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push("Project packages:".to_string());
                for package in project_packages {
                    lines.push(format!("  {}", package.source));
                    lines.push(format!("    {}", package.install_path.to_string_lossy()));
                }
            }
            Ok(RunResult::Completed {
                exit_code: 0,
                stdout: Some(lines.join("\n")),
                stderr: None,
            })
        }
    }
}

fn run_export_command(export_target: &str, extra_args: &[String]) -> Result<RunResult, CliError> {
    let output_path = extra_args.first().map(PathBuf::from);
    match export_session_file_to_html(export_target, output_path.as_deref()) {
        Ok(path) => Ok(RunResult::Completed {
            exit_code: 0,
            stdout: Some(format!("Exported to: {}", path.to_string_lossy())),
            stderr: None,
        }),
        Err(error) => Ok(RunResult::Completed {
            exit_code: 1,
            stdout: None,
            stderr: Some(error.to_string()),
        }),
    }
}

fn split_csv_preserving_empties(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_tool_names(value: &str) -> Vec<String> {
    split_csv_preserving_empties(value)
        .into_iter()
        .filter(|name| is_known_tool(name))
        .collect()
}

fn is_known_tool(value: &str) -> bool {
    matches!(
        value,
        "read" | "bash" | "edit" | "write" | "grep" | "find" | "ls"
    )
}

fn is_valid_thinking_level(value: &str) -> bool {
    VALID_THINKING_LEVELS.contains(&value)
}

#[cfg(test)]
fn test_env_guard() -> &'static std::sync::Mutex<()> {
    static GUARD: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    GUARD.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod parity_tests;

#[cfg(test)]
mod tests {
    use pi_rust_ai_core::{
        AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message,
        StopReason, StreamOptions, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
    };
    use pi_rust_ai_providers::{ApiProvider, ProviderRegistry, register_builtin_providers};
    use pi_rust_config::ENV_AGENT_DIR;
    use pi_rust_core::{AgentSession, NonInteractiveRequest, create_agent_session};
    use pi_rust_models::ModelRegistry;
    use pi_rust_oauth::AuthStorage;
    use pi_rust_protocol::OutputMode;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Cursor;
    use tempfile::tempdir;

    use super::{
        CliError, ExtensionFlagType, ExtensionFlagValue, PackageCommand, RunResult, parse_args,
        parse_args_with_extension_flags, run,
    };

    struct EchoProvider;

    struct SlowEchoProvider;

    impl ApiProvider for EchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &pi_rust_ai_core::Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> pi_rust_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = pi_rust_ai_core::AssistantMessageEventStream::new();
            let prompt = match context.messages.last() {
                Some(Message::User(UserMessage {
                    content: UserContent::Text(text),
                    ..
                })) => text.clone(),
                Some(Message::User(UserMessage {
                    content: UserContent::Blocks(blocks),
                    ..
                })) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            let assistant = AssistantMessage {
                content: vec![AssistantContentBlock::Text {
                    text: format!("echo:{prompt}"),
                    text_signature: None,
                }],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                usage: Usage {
                    input: 1,
                    output: 1,
                    cache_read: 0,
                    cache_write: 0,
                    total_tokens: 2,
                    cost: UsageCost {
                        input: "0".to_string(),
                        output: "0".to_string(),
                        cache_read: "0".to_string(),
                        cache_write: "0".to_string(),
                        total: "0".to_string(),
                    },
                },
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
            };
            sender.send(AssistantMessageEvent::Done {
                reason: assistant.stop_reason,
                message: assistant,
            });
            stream
        }
    }

    impl ApiProvider for SlowEchoProvider {
        fn api(&self) -> &'static str {
            "openai-responses"
        }

        fn stream(
            &self,
            model: &pi_rust_ai_core::Model,
            context: &Context,
            _options: Option<StreamOptions>,
        ) -> pi_rust_ai_core::AssistantMessageEventStream {
            let (mut sender, stream) = pi_rust_ai_core::AssistantMessageEventStream::new();
            let prompt = match context.messages.last() {
                Some(Message::User(UserMessage {
                    content: UserContent::Text(text),
                    ..
                })) => text.clone(),
                Some(Message::User(UserMessage {
                    content: UserContent::Blocks(blocks),
                    ..
                })) => blocks
                    .iter()
                    .filter_map(|block| match block {
                        UserContentBlock::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };
            let assistant = AssistantMessage {
                content: vec![AssistantContentBlock::Text {
                    text: format!("echo:{prompt}"),
                    text_signature: None,
                }],
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                usage: Usage {
                    input: 1,
                    output: 1,
                    cache_read: 0,
                    cache_write: 0,
                    total_tokens: 2,
                    cost: UsageCost {
                        input: "0".to_string(),
                        output: "0".to_string(),
                        cache_read: "0".to_string(),
                        cache_write: "0".to_string(),
                        total: "0".to_string(),
                    },
                },
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: 0,
            };
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(120));
                sender.send(AssistantMessageEvent::Done {
                    reason: assistant.stop_reason,
                    message: assistant,
                });
            });
            stream
        }
    }

    fn rpc_session(tempdir: &std::path::Path) -> AgentSession {
        let mut providers = ProviderRegistry::new();
        providers.register(std::sync::Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);
        create_agent_session(
            &NonInteractiveRequest {
                cwd: tempdir.to_path_buf(),
                mode: OutputMode::Rpc,
                provider: Some("openai".to_string()),
                model: Some("gpt-5.1-codex".to_string()),
                api_key: None,
                system_prompt: None,
                append_system_prompt: None,
                initial_message: None,
                messages: Vec::new(),
                continue_session: false,
                no_session: true,
                session: None,
                session_dir: None,
                models: None,
                no_tools: false,
                tools: None,
                thinking: None,
                no_skills: false,
                skills: Vec::new(),
                prompt_templates: Vec::new(),
                no_prompt_templates: false,
                themes: Vec::new(),
                no_themes: false,
            },
            &providers,
            &mut models,
        )
        .expect("create agent session")
    }

    fn slow_rpc_session(tempdir: &std::path::Path) -> AgentSession {
        let mut providers = ProviderRegistry::new();
        providers.register(std::sync::Arc::new(SlowEchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);
        create_agent_session(
            &NonInteractiveRequest {
                cwd: tempdir.to_path_buf(),
                mode: OutputMode::Rpc,
                provider: Some("openai".to_string()),
                model: Some("gpt-5.1-codex".to_string()),
                api_key: None,
                system_prompt: None,
                append_system_prompt: None,
                initial_message: None,
                messages: Vec::new(),
                continue_session: false,
                no_session: true,
                session: None,
                session_dir: None,
                models: None,
                no_tools: false,
                tools: None,
                thinking: None,
                no_skills: false,
                skills: Vec::new(),
                prompt_templates: Vec::new(),
                no_prompt_templates: false,
                themes: Vec::new(),
                no_themes: false,
            },
            &providers,
            &mut models,
        )
        .expect("create slow agent session")
    }

    #[test]
    fn parses_core_cli_flags() {
        let args = vec![
            "--provider".to_string(),
            "openai".to_string(),
            "--model".to_string(),
            "gpt-5.1-codex".to_string(),
            "--models".to_string(),
            "sonnet:high,haiku:low".to_string(),
            "--tools".to_string(),
            "read,grep".to_string(),
            "@README.md".to_string(),
            "Review this".to_string(),
        ];
        let parsed = parse_args(&args);
        assert_eq!(parsed.provider.as_deref(), Some("openai"));
        assert_eq!(parsed.model.as_deref(), Some("gpt-5.1-codex"));
        assert_eq!(
            parsed.models,
            Some(vec!["sonnet:high".to_string(), "haiku:low".to_string()])
        );
        assert_eq!(
            parsed.tools,
            Some(vec!["read".to_string(), "grep".to_string()])
        );
        assert_eq!(parsed.file_args, vec!["README.md".to_string()]);
        assert_eq!(parsed.messages, vec!["Review this".to_string()]);
    }

    #[test]
    fn help_returns_fixture_output() {
        let args = vec!["--help".to_string()];
        let result = run(&args).expect("help result");
        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some(super::render_help_text().to_string()),
                stderr: None,
            }
        );
    }

    #[test]
    fn extension_flags_fail_fast() {
        let args = vec!["--extension".to_string(), "./ext.ts".to_string()];
        let error = run(&args).expect_err("extension failure");
        assert_eq!(error, CliError::UnsupportedExtensions);
    }

    #[test]
    fn version_takes_precedence_over_help() {
        let result = run(&["--help".to_string(), "--version".to_string()]).expect("version result");
        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some(super::render_version_text().to_string()),
                stderr: None,
            }
        );
    }

    #[test]
    fn list_models_only_consumes_search_when_next_arg_is_not_flag_or_file() {
        let parsed = parse_args(&["--list-models".to_string(), "sonnet".to_string()]);
        assert_eq!(parsed.list_models, Some(Some("sonnet".to_string())));

        let parsed = parse_args(&["--list-models".to_string(), "--verbose".to_string()]);
        assert_eq!(parsed.list_models, Some(None));
        assert!(parsed.verbose);

        let parsed = parse_args(&["--list-models".to_string(), "@prompt.md".to_string()]);
        assert_eq!(parsed.list_models, Some(None));
        assert_eq!(parsed.file_args, vec!["prompt.md".to_string()]);
    }

    #[test]
    fn models_preserve_empty_patterns_like_typescript() {
        let parsed = parse_args(&["--models".to_string(), ",sonnet,,haiku, ".to_string()]);
        assert_eq!(
            parsed.models,
            Some(vec![
                "".to_string(),
                "sonnet".to_string(),
                "".to_string(),
                "haiku".to_string(),
                "".to_string()
            ])
        );
    }

    #[test]
    fn tools_drop_unknown_values() {
        let parsed = parse_args(&["--tools".to_string(), "read,,bogus,grep".to_string()]);
        assert_eq!(
            parsed.tools,
            Some(vec!["read".to_string(), "grep".to_string()])
        );
    }

    #[test]
    fn invalid_thinking_levels_are_ignored() {
        let parsed = parse_args(&["--thinking".to_string(), "veryhigh".to_string()]);
        assert_eq!(parsed.thinking, None);

        let parsed = parse_args(&["--thinking".to_string(), "high".to_string()]);
        assert_eq!(parsed.thinking.as_deref(), Some("high"));
    }

    #[test]
    fn extension_registered_flags_are_captured_on_second_pass() {
        let mut extension_flags = BTreeMap::new();
        extension_flags.insert("plan".to_string(), ExtensionFlagType::Boolean);
        extension_flags.insert("profile".to_string(), ExtensionFlagType::String);

        let parsed = parse_args_with_extension_flags(
            &[
                "--plan".to_string(),
                "--profile".to_string(),
                "coder".to_string(),
                "hello".to_string(),
            ],
            Some(&extension_flags),
        );

        assert_eq!(
            parsed.unknown_flags.get("plan"),
            Some(&ExtensionFlagValue::Boolean(true))
        );
        assert_eq!(
            parsed.unknown_flags.get("profile"),
            Some(&ExtensionFlagValue::String("coder".to_string()))
        );
        assert_eq!(parsed.messages, vec!["hello".to_string()]);
    }

    #[test]
    fn package_help_is_handled_before_global_args() {
        let result = run(&["install".to_string(), "--help".to_string()]).expect("package help");
        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some(
                    super::render_package_command_help(PackageCommand::Install)
                        .trim_end()
                        .to_string()
                ),
                stderr: None,
            }
        );
    }

    #[test]
    fn package_validation_matches_typescript_shape() {
        let result = run(&["install".to_string(), "--bogus".to_string()]).expect("invalid option");
        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 1,
                stdout: None,
                stderr: Some(
                    "Unknown option --bogus for \"install\".\nUse \"pi-rust --help\" or \"pi-rust install <source> [-l]\"."
                        .to_string()
                ),
            }
        );

        let result = run(&["remove".to_string()]).expect("missing source");
        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 1,
                stdout: None,
                stderr: Some(
                    "Missing remove source.\nUsage: pi-rust remove <source> [-l]".to_string()
                ),
            }
        );
    }

    #[test]
    fn tui_commands_require_a_terminal_in_tests() {
        let error = run(&["config".to_string()]).expect_err("config tty check");
        assert_eq!(error, CliError::TuiRequiresTerminal("config"));

        let error = run(&[]).expect_err("interactive tty check");
        assert_eq!(error, CliError::TuiRequiresTerminal("Interactive mode"));
    }

    #[test]
    fn list_models_marks_openai_codex_oauth_auth_after_builtin_registration() {
        let _guard = super::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let agent_dir = tempdir.path().join("agent");
        fs::create_dir_all(&agent_dir).expect("agent dir");
        fs::write(
            agent_dir.join("auth.json"),
            r#"{
  "openai-codex": {
    "type": "oauth",
    "refresh": "refresh-token",
    "access": "access-token",
    "expires": 4102444800
  }
}"#,
        )
        .expect("write auth");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };

        let mut providers = ProviderRegistry::new();
        register_builtin_providers(&mut providers);
        let auth = AuthStorage::create(Some(agent_dir.join("auth.json")));
        assert_eq!(
            auth.get_api_key("openai-codex").as_deref(),
            Some("access-token")
        );

        let result =
            run(&["--list-models".to_string(), "openai-codex".to_string()]).expect("list models");

        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some("openai-codex/gpt-5.3-codex [auth]".to_string()),
                stderr: None,
            }
        );
    }

    #[test]
    fn package_commands_mutate_temp_settings() {
        let _guard = super::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let local_pkg = cwd.join("pkg");
        fs::create_dir_all(&local_pkg).expect("local pkg");
        let agent_dir = tempdir.path().join("agent");
        fs::create_dir_all(&agent_dir).expect("agent dir");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        let original_cwd = std::env::current_dir().expect("cwd");
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };
        std::env::set_current_dir(&cwd).expect("set cwd");

        let install = run(&[
            "install".to_string(),
            "./pkg".to_string(),
            "--local".to_string(),
        ])
        .expect("install");
        let list = run(&["list".to_string()]).expect("list");
        let remove = run(&[
            "remove".to_string(),
            "./pkg".to_string(),
            "--local".to_string(),
        ])
        .expect("remove");

        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(
            install,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some("Installing ./pkg...\nInstalled ./pkg".to_string()),
                stderr: None,
            }
        );
        match list {
            RunResult::Completed {
                exit_code,
                stdout,
                stderr,
            } => {
                assert_eq!(exit_code, 0);
                assert!(stderr.is_none());
                let stdout = stdout.expect("list stdout");
                assert!(stdout.contains("Project packages:"));
                assert!(stdout.contains("../pkg"));
            }
        }
        assert_eq!(
            remove,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some("Removing ./pkg...\nRemoved ./pkg".to_string()),
                stderr: None,
            }
        );
    }

    #[test]
    fn rpc_mode_processes_commands_and_emits_events() {
        let tempdir = tempdir().expect("tempdir");
        let session = rpc_session(tempdir.path());
        let input = Cursor::new(
            "{\"type\":\"get_state\",\"id\":\"1\"}\n{\"type\":\"prompt\",\"id\":\"2\",\"message\":\"hello\"}\n{\"type\":\"get_last_assistant_text\",\"id\":\"3\"}\n",
        );
        let mut output = Vec::new();

        let exit_code = super::run_rpc_with_io(input, &mut output, session).expect("run rpc");
        assert_eq!(exit_code, 0);

        let lines = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
            .collect::<Vec<_>>();

        assert_eq!(lines[0]["type"], "response");
        assert_eq!(lines[0]["command"], "get_state");
        assert_eq!(lines[0]["success"], true);

        assert_eq!(lines[1]["type"], "response");
        assert_eq!(lines[1]["command"], "prompt");
        assert_eq!(lines[1]["success"], true);

        assert!(lines.iter().any(|line| line["type"] == "agent_start"));
        assert!(lines.iter().any(|line| line["type"] == "agent_end"));

        let last = lines.last().expect("last rpc line");
        assert_eq!(last["type"], "response");
        assert_eq!(last["command"], "get_last_assistant_text");
        assert_eq!(last["data"]["text"], "echo:hello");
    }

    #[test]
    fn rpc_mode_accepts_midstream_steer_and_resolves_following_query_after_completion() {
        let tempdir = tempdir().expect("tempdir");
        let session = slow_rpc_session(tempdir.path());
        let input = Cursor::new(
            "{\"type\":\"prompt\",\"id\":\"1\",\"message\":\"hello\"}\n\
             {\"type\":\"steer\",\"id\":\"2\",\"message\":\"redirect\"}\n\
             {\"type\":\"get_last_assistant_text\",\"id\":\"3\"}\n",
        );
        let mut output = Vec::new();

        let exit_code = super::run_rpc_with_io(input, &mut output, session).expect("run rpc");
        assert_eq!(exit_code, 0);

        let lines = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
            .collect::<Vec<_>>();
        assert_eq!(lines[0]["type"], "response");
        assert_eq!(lines[0]["command"], "prompt");
        assert_eq!(lines[1]["type"], "response");
        assert_eq!(lines[1]["command"], "steer");
        assert!(lines.iter().any(|line| line["type"] == "agent_end"));

        let last = lines.last().expect("last rpc line");
        assert_eq!(last["type"], "response");
        assert_eq!(last["command"], "get_last_assistant_text");
        assert_eq!(last["data"]["text"], "echo:redirect");
    }

    #[test]
    fn rpc_mode_abort_stops_active_prompt_and_persists_aborted_assistant() {
        let tempdir = tempdir().expect("tempdir");
        let session = slow_rpc_session(tempdir.path());
        let input = Cursor::new(
            "{\"type\":\"prompt\",\"id\":\"1\",\"message\":\"hello\"}\n\
             {\"type\":\"abort\",\"id\":\"2\"}\n\
             {\"type\":\"get_messages\",\"id\":\"3\"}\n",
        );
        let mut output = Vec::new();

        let exit_code = super::run_rpc_with_io(input, &mut output, session).expect("run rpc");
        assert_eq!(exit_code, 0);

        let lines = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json line"))
            .collect::<Vec<_>>();

        assert_eq!(lines[0]["type"], "response");
        assert_eq!(lines[0]["command"], "prompt");
        assert_eq!(lines[1]["type"], "response");
        assert_eq!(lines[1]["command"], "abort");

        let last = lines.last().expect("last rpc line");
        assert_eq!(last["type"], "response");
        assert_eq!(last["command"], "get_messages");
        let messages = last["data"]["messages"].as_array().expect("messages array");
        let assistant = messages
            .iter()
            .find(|message| message["role"] == "assistant")
            .expect("assistant message");
        assert_eq!(assistant["stopReason"], "aborted");
        assert_eq!(assistant["errorMessage"], "Request aborted");
    }
}
