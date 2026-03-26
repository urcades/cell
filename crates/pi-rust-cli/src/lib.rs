mod help;
mod interactive;
mod keybindings;
mod rpc;

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fmt;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use pi_rust_ai_providers::{ProviderRegistry, register_builtin_providers};
use pi_rust_config::{SettingsManager, SettingsScope, expand_tilde, get_agent_dir, get_project_config_dir};
use pi_rust_core::{
    AgentSession, NonInteractiveRequest, create_agent_session, export_session_file_to_html,
    list_known_models, list_models, run_non_interactive,
};
use pi_rust_models::ModelRegistry;
use pi_rust_oauth::AuthStorage;
use pi_rust_packages::{PackageInstallScope, PackageManager};
use pi_rust_plugin_host::{
    PluginHost, PluginHostConfig, PluginStartupSummary, RegisteredPluginSummary,
};
use pi_rust_protocol::{
    OutputMode, RpcPluginRuntimeDiagnostics, RpcPluginRuntimePluginSummary,
    RpcPluginRuntimeWarning,
};

pub use help::{
    render_help_text, render_package_command_help, render_package_command_usage,
    render_plugins_command_help, render_plugins_command_usage, render_plugins_help_text,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginsCommand {
    List,
    AddRoot,
    RemoveRoot,
}

impl PluginsCommand {
    fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::AddRoot => "add-root",
            Self::RemoveRoot => "remove-root",
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct PluginsCommandOptions {
    command: PluginsCommand,
    path: Option<String>,
    local: bool,
    help: bool,
    group_help: bool,
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
    pub list_known_models: Option<Option<String>>,
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
            "--list-known-models" => {
                if index + 1 < args.len()
                    && !args[index + 1].starts_with('-')
                    && !args[index + 1].starts_with('@')
                {
                    index += 1;
                    result.list_known_models = Some(Some(args[index].clone()));
                } else {
                    result.list_known_models = Some(None);
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
    let mode = parsed.mode.unwrap_or(OutputMode::Text);
    if let Some(result) = maybe_run_plugins_command(args, &parsed, mode)? {
        return Ok(result);
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
    if parsed.list_known_models.is_some() {
        let search = parsed
            .list_known_models
            .as_ref()
            .and_then(|value| value.as_deref());
        return Ok(RunResult::Completed {
            exit_code: 0,
            stdout: Some(list_known_models(models, search)),
            stderr: None,
        });
    }
    if let Some(export_target) = &parsed.export {
        return run_export_command(export_target, &parsed.messages);
    }
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
    if let Some(result) = maybe_run_top_level_plugin_command(&parsed, mode, providers, models)? {
        return Ok(result);
    }
    let is_interactive = !parsed.print && parsed.mode.is_none();
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

    run_non_interactive_request(
        build_non_interactive_request(&parsed, mode, parsed.messages.clone()),
        providers,
        models,
    )
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

    let request = build_non_interactive_request(&parsed, OutputMode::Rpc, Vec::new());

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

fn maybe_run_plugins_command(
    args: &[String],
    parsed: &Args,
    mode: OutputMode,
) -> Result<Option<RunResult>, CliError> {
    if matches!(mode, OutputMode::Rpc) || parsed.messages.first().map(String::as_str) != Some("plugins") {
        return Ok(None);
    }

    let Some(start_index) = args.iter().position(|arg| arg == "plugins") else {
        return Ok(None);
    };
    let Some(options) = parse_plugins_command(&args[start_index..]) else {
        return Ok(None);
    };

    match options.command {
        PluginsCommand::List => {
            if options.group_help {
                return Ok(Some(RunResult::Completed {
                    exit_code: 0,
                    stdout: Some(render_plugins_help_text().trim_end().to_string()),
                    stderr: None,
                }));
            }

            if options.help {
                return Ok(Some(RunResult::Completed {
                    exit_code: 0,
                    stdout: Some(
                        render_plugins_command_help(PluginsCommand::List)
                            .trim_end()
                            .to_string(),
                    ),
                    stderr: None,
                }));
            }

            if let Some(invalid_option) = options.invalid_option {
                return Ok(Some(RunResult::Completed {
                    exit_code: 1,
                    stdout: None,
                    stderr: Some(format!(
                        "Unknown option {invalid_option} for \"plugins list\".\nUse \"pi-rust --help\" or \"{}\".",
                        render_plugins_command_usage(PluginsCommand::List)
                    )),
                }));
            }

            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let package_manager = PackageManager::create(&cwd, None);
            let diagnostics = discover_plugin_runtime_diagnostics(&package_manager);

            let stdout = match parsed.mode {
                Some(OutputMode::Json) => serde_json::to_string_pretty(&diagnostics)
                    .map_err(|error| CliError::InteractiveFailed(error.to_string()))?,
                _ => render_plugin_runtime_diagnostics_text(&diagnostics),
            };

            Ok(Some(RunResult::Completed {
                exit_code: 0,
                stdout: Some(stdout),
                stderr: None,
            }))
        }
        PluginsCommand::AddRoot | PluginsCommand::RemoveRoot => {
            if options.group_help {
                return Ok(Some(RunResult::Completed {
                    exit_code: 0,
                    stdout: Some(render_plugins_help_text().trim_end().to_string()),
                    stderr: None,
                }));
            }

            if options.help {
                return Ok(Some(RunResult::Completed {
                    exit_code: 0,
                    stdout: Some(
                        render_plugins_command_help(options.command)
                            .trim_end()
                            .to_string(),
                    ),
                    stderr: None,
                }));
            }

            if let Some(invalid_option) = options.invalid_option {
                return Ok(Some(RunResult::Completed {
                    exit_code: 1,
                    stdout: None,
                    stderr: Some(format!(
                        "Unknown option {invalid_option} for \"plugins {}\".\nUse \"pi-rust --help\" or \"{}\".",
                        options.command.as_str(),
                        render_plugins_command_usage(options.command)
                    )),
                }));
            }

            let Some(path) = options.path else {
                return Ok(Some(RunResult::Completed {
                    exit_code: 1,
                    stdout: None,
                    stderr: Some(format!(
                        "Missing plugin root path.\nUsage: {}",
                        render_plugins_command_usage(options.command)
                    )),
                }));
            };

            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let mut settings_manager = SettingsManager::create(&cwd, None);
            let scope = if options.local {
                SettingsScope::Project
            } else {
                SettingsScope::Global
            };

            let result = match options.command {
                PluginsCommand::AddRoot => add_plugin_root(&mut settings_manager, &cwd, scope, &path),
                PluginsCommand::RemoveRoot => {
                    remove_plugin_root(&mut settings_manager, &cwd, scope, &path)
                }
                PluginsCommand::List => unreachable!(),
            };

            match result {
                Ok(message) => Ok(Some(RunResult::Completed {
                    exit_code: 0,
                    stdout: Some(message),
                    stderr: None,
                })),
                Err(message) => Ok(Some(RunResult::Completed {
                    exit_code: 1,
                    stdout: None,
                    stderr: Some(message),
                })),
            }
        }
    }
}

fn parse_plugins_command(args: &[String]) -> Option<PluginsCommandOptions> {
    let (command, rest) = args.split_first()?;
    if command != "plugins" {
        return None;
    }

    if rest.is_empty() {
        return Some(PluginsCommandOptions {
            command: PluginsCommand::List,
            path: None,
            local: false,
            help: true,
            group_help: true,
            invalid_option: None,
        });
    }

    if matches!(rest[0].as_str(), "-h" | "--help") {
        return Some(PluginsCommandOptions {
            command: PluginsCommand::List,
            path: None,
            local: false,
            help: true,
            group_help: true,
            invalid_option: None,
        });
    }

    if rest[0] == "help" {
        return Some(PluginsCommandOptions {
            command: PluginsCommand::List,
            path: None,
            local: false,
            help: true,
            group_help: true,
            invalid_option: None,
        });
    }

    let command = match rest[0].as_str() {
        "list" => PluginsCommand::List,
        "add-root" => PluginsCommand::AddRoot,
        "remove-root" => PluginsCommand::RemoveRoot,
        _ => {
            return Some(PluginsCommandOptions {
                command: PluginsCommand::List,
                path: None,
                local: false,
                help: false,
                group_help: false,
                invalid_option: Some(rest[0].clone()),
            });
        }
    };

    let mut options = PluginsCommandOptions {
        command,
        path: None,
        local: false,
        help: false,
        group_help: false,
        invalid_option: None,
    };

    let mut index = 1;
    while index < rest.len() {
        let arg = &rest[index];
        match arg.as_str() {
            "-h" | "--help" => {
                options.help = true;
            }
            "-l" | "--local" | "--project" => {
                options.local = true;
            }
            "--mode" | "--provider" | "--model" | "--api-key" | "--system-prompt"
            | "--append-system-prompt" | "--session" | "--session-dir" | "--models"
            | "--tools" | "--thinking" | "--export" | "--skill" | "--prompt-template"
            | "--theme" => {
                index += 1;
            }
            "--list-models" | "--list-known-models" => {
                if index + 1 < rest.len()
                    && !rest[index + 1].starts_with('-')
                    && !rest[index + 1].starts_with('@')
                {
                    index += 1;
                }
            }
            "--continue" | "-c" | "--resume" | "-r" | "--no-session" | "--no-tools"
            | "--print" | "-p" | "--no-extensions" | "-ne" | "--no-skills" | "-ns"
            | "--no-prompt-templates" | "-np" | "--no-themes" | "--verbose" => {}
            value if value.starts_with("--") => {
                if options.invalid_option.is_none() {
                    options.invalid_option = Some(value.to_string());
                }
            }
            value => {
                if matches!(options.command, PluginsCommand::List) {
                    if options.invalid_option.is_none() {
                        options.invalid_option = Some(value.to_string());
                    }
                } else if options.path.is_none() {
                    options.path = Some(value.to_string());
                } else if options.invalid_option.is_none() {
                    options.invalid_option = Some(value.to_string());
                }
            }
        }
        index += 1;
    }

    Some(options)
}

fn discover_plugin_runtime_diagnostics(
    package_manager: &PackageManager,
) -> RpcPluginRuntimeDiagnostics {
    let roots = build_plugin_discovery_roots(package_manager);
    let host = PluginHost::new(PluginHostConfig {
        discovery_roots: roots,
        workspace_root: Some(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        ..PluginHostConfig::default()
    });
    plugin_runtime_diagnostics_from_startup_summary(host.discover_and_load_runtime_plugins().summary)
}

fn build_plugin_discovery_roots(package_manager: &PackageManager) -> Vec<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    append_package_roots(&mut roots, &mut seen, package_manager, PackageInstallScope::Project);
    append_package_roots(&mut roots, &mut seen, package_manager, PackageInstallScope::User);
    append_plugin_roots(
        &mut roots,
        &mut seen,
        package_manager.settings_manager(),
        SettingsScope::Project,
        &get_project_config_dir(&cwd),
    );
    append_plugin_roots(
        &mut roots,
        &mut seen,
        package_manager.settings_manager(),
        SettingsScope::Global,
        &get_agent_dir(),
    );

    roots
}

fn plugin_runtime_diagnostics_from_startup_summary(
    summary: PluginStartupSummary,
) -> RpcPluginRuntimeDiagnostics {
    RpcPluginRuntimeDiagnostics {
        plugins: summary
            .summaries
            .into_iter()
            .map(plugin_runtime_plugin_summary_from_registered)
            .collect(),
        warnings: summary
            .warnings
            .into_iter()
            .map(plugin_runtime_warning_from_host)
            .collect(),
    }
}

fn plugin_runtime_plugin_summary_from_registered(
    summary: RegisteredPluginSummary,
) -> RpcPluginRuntimePluginSummary {
    RpcPluginRuntimePluginSummary {
        descriptor_path: summary.descriptor_path.to_string_lossy().to_string(),
        plugin_id: summary.plugin_id,
        plugin_name: summary.plugin_name,
        manifest_version: summary.manifest_version,
        command_count: summary.capabilities.commands,
        tool_count: summary.capabilities.tools,
        flag_count: summary.capabilities.flags,
        hook_count: summary.capabilities.hooks,
        provider_count: summary.capabilities.providers,
        model_count: summary.capabilities.models,
    }
}

fn plugin_runtime_warning_from_host(
    warning: pi_rust_plugin_host::PluginHostWarning,
) -> RpcPluginRuntimeWarning {
    RpcPluginRuntimeWarning {
        path: Some(warning.path.to_string_lossy().to_string()),
        plugin_id: warning.plugin_id,
        plugin_name: warning.plugin_name,
        event: None,
        details: None,
        message: warning.message,
    }
}

fn append_package_roots(
    roots: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    package_manager: &PackageManager,
    scope: PackageInstallScope,
) {
    for package in package_manager.list_by_scope(scope) {
        append_unique_root(roots, seen, package.install_path);
    }
}

fn append_plugin_roots(
    roots: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    settings_manager: &SettingsManager,
    scope: SettingsScope,
    base_dir: &Path,
) {
    for root in settings_manager.get_plugin_roots(Some(scope)) {
        let resolved = resolve_scoped_plugin_root(base_dir, &root);
        append_unique_root(roots, seen, resolved);
    }
}

fn append_unique_root(roots: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    let normalized = normalize_path(&path);
    if seen.insert(normalized.clone()) {
        roots.push(normalized);
    }
}

fn resolve_scoped_plugin_root(base_dir: &Path, value: &str) -> PathBuf {
    let expanded = expand_tilde(value);
    let resolved = if expanded.is_absolute() {
        expanded
    } else {
        base_dir.join(expanded)
    };
    normalize_path(&resolved)
}

fn normalize_path(path: &PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    let normalized = if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    };
    if normalized.exists() {
        std::fs::canonicalize(&normalized).unwrap_or(normalized)
    } else {
        normalized
    }
}

fn add_plugin_root(
    settings_manager: &mut SettingsManager,
    cwd: &PathBuf,
    scope: SettingsScope,
    value: &str,
) -> Result<String, String> {
    let base_dir = plugin_roots_base_dir(cwd, scope);
    let resolved = resolve_plugin_root_input(cwd, value);
    let stored = serialize_plugin_root(&base_dir, &resolved);
    let mut roots = settings_manager.get_plugin_roots(Some(scope));
    let already_present = roots.iter().any(|entry| {
        resolve_scoped_plugin_root(&base_dir, entry) == resolved
    });
    if !already_present {
        roots.push(stored.clone());
        settings_manager
            .set_plugin_roots(scope, &roots)
            .map_err(|error| error.to_string())?;
    }
    Ok(format!(
        "Added plugin root {} to {} settings.",
        value,
        plugin_scope_label(scope)
    ))
}

fn remove_plugin_root(
    settings_manager: &mut SettingsManager,
    cwd: &PathBuf,
    scope: SettingsScope,
    value: &str,
) -> Result<String, String> {
    let base_dir = plugin_roots_base_dir(cwd, scope);
    let resolved = resolve_plugin_root_input(cwd, value);
    let mut roots = settings_manager.get_plugin_roots(Some(scope));
    let before = roots.len();
    roots.retain(|entry| resolve_scoped_plugin_root(&base_dir, entry) != resolved);
    if roots.len() == before {
        return Err(format!("No matching plugin root found for {}.", value));
    }
    settings_manager
        .set_plugin_roots(scope, &roots)
        .map_err(|error| error.to_string())?;
    Ok(format!(
        "Removed plugin root {} from {} settings.",
        value,
        plugin_scope_label(scope)
    ))
}

fn plugin_roots_base_dir(cwd: &Path, scope: SettingsScope) -> PathBuf {
    match scope {
        SettingsScope::Global => get_agent_dir(),
        SettingsScope::Project => get_project_config_dir(cwd),
    }
}

fn resolve_plugin_root_input(cwd: &PathBuf, value: &str) -> PathBuf {
    let expanded = expand_tilde(value);
    if expanded.is_absolute() {
        normalize_path(&expanded)
    } else {
        normalize_path(&cwd.join(expanded))
    }
}

fn serialize_plugin_root(base_dir: &PathBuf, resolved: &PathBuf) -> String {
    if let Some(relative) = relative_path(base_dir, resolved) {
        normalize_plugin_root_text(&relative)
    } else {
        normalize_plugin_root_text(resolved)
    }
}

fn relative_path(from: &PathBuf, to: &PathBuf) -> Option<PathBuf> {
    let from = normalize_path(from);
    let to = normalize_path(to);
    let from_components = from.components().collect::<Vec<_>>();
    let to_components = to.components().collect::<Vec<_>>();

    let mut common = 0;
    while common < from_components.len()
        && common < to_components.len()
        && from_components[common] == to_components[common]
    {
        common += 1;
    }

    let mut relative = PathBuf::new();
    for component in &from_components[common..] {
        match component {
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {}
            _ => relative.push(".."),
        }
    }
    for component in &to_components[common..] {
        relative.push(component.as_os_str());
    }

    if relative.as_os_str().is_empty() {
        Some(PathBuf::from("."))
    } else {
        Some(relative)
    }
}

fn normalize_plugin_root_text(path: &PathBuf) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn plugin_scope_label(scope: SettingsScope) -> &'static str {
    match scope {
        SettingsScope::Global => "user",
        SettingsScope::Project => "project",
    }
}

fn render_plugin_runtime_diagnostics_text(diagnostics: &RpcPluginRuntimeDiagnostics) -> String {
    let mut lines = Vec::new();

    if diagnostics.plugins.is_empty() {
        lines.push("No plugins discovered.".to_string());
    } else {
        lines.push("Plugins:".to_string());
        for plugin in &diagnostics.plugins {
            lines.push(format!(
                "  {} [{}] v{} - {} - {}",
                plugin.plugin_name,
                plugin.plugin_id,
                plugin.manifest_version,
                render_plugin_capabilities(plugin),
                plugin.descriptor_path
            ));
        }
    }

    if !diagnostics.warnings.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("Warnings:".to_string());
        for warning in &diagnostics.warnings {
            lines.push(render_plugin_warning(warning));
        }
    }

    lines.join("\n")
}

fn render_plugin_capabilities(counts: &RpcPluginRuntimePluginSummary) -> String {
    let mut items = Vec::new();
    push_capability_count(&mut items, counts.command_count, "command");
    push_capability_count(&mut items, counts.tool_count, "tool");
    push_capability_count(&mut items, counts.flag_count, "flag");
    push_capability_count(&mut items, counts.hook_count, "hook");
    push_capability_count(&mut items, counts.provider_count, "provider");
    push_capability_count(&mut items, counts.model_count, "model");

    if items.is_empty() {
        "no capabilities".to_string()
    } else {
        items.join(", ")
    }
}

fn push_capability_count(target: &mut Vec<String>, count: usize, label: &str) {
    if count == 0 {
        return;
    }
    target.push(format!("{count} {label}{}", if count == 1 { "" } else { "s" }));
}

fn render_plugin_warning(warning: &RpcPluginRuntimeWarning) -> String {
    let plugin = warning
        .plugin_name
        .as_deref()
        .or(warning.plugin_id.as_deref())
        .unwrap_or("unknown plugin");
    match warning.path.as_deref() {
        Some(path) => format!("  {} - {}: {}", plugin, path, warning.message),
        None => format!("  {}: {}", plugin, warning.message),
    }
}

fn maybe_run_top_level_plugin_command(
    parsed: &Args,
    mode: OutputMode,
    providers: &ProviderRegistry,
    models: &mut ModelRegistry,
) -> Result<Option<RunResult>, CliError> {
    if matches!(mode, OutputMode::Rpc) {
        return Ok(None);
    }
    let Some((command_name, command_args)) = parsed.messages.split_first() else {
        return Ok(None);
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let package_manager = PackageManager::create(&cwd, None);
    let discovery_roots = build_plugin_discovery_roots(&package_manager);
    if discovery_roots.is_empty() {
        return Ok(None);
    }

    let host = PluginHost::new(PluginHostConfig {
        discovery_roots,
        workspace_root: Some(cwd),
        ..PluginHostConfig::default()
    });
    let runtime = host.discover_and_load_runtime_plugins();
    let Some(registry) = runtime.registry else {
        return Ok(None);
    };
    let Some(command) = registry.merged_registry().commands.get(command_name) else {
        return Ok(None);
    };
    if command.registration.hidden {
        return Ok(None);
    }

    run_non_interactive_request(
        build_non_interactive_request(
            parsed,
            mode,
            vec![format_top_level_plugin_prompt(command_name, command_args)],
        ),
        providers,
        models,
    )
    .map(Some)
}

fn run_rpc_with_io(
    reader: impl Read + Send + 'static,
    writer: impl Write,
    session: AgentSession,
) -> Result<i32, String> {
    rpc::run_rpc_with_io(reader, writer, session)
}

fn build_non_interactive_request(
    parsed: &Args,
    mode: OutputMode,
    messages: Vec<String>,
) -> NonInteractiveRequest {
    NonInteractiveRequest {
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        mode,
        provider: parsed.provider.clone(),
        model: parsed.model.clone(),
        api_key: parsed.api_key.clone(),
        system_prompt: parsed.system_prompt.clone(),
        append_system_prompt: parsed.append_system_prompt.clone(),
        initial_message: None,
        messages,
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
    }
}

fn run_non_interactive_request(
    request: NonInteractiveRequest,
    providers: &ProviderRegistry,
    models: &mut ModelRegistry,
) -> Result<RunResult, CliError> {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(run_non_interactive(request, providers, models));

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

fn format_top_level_plugin_prompt(command_name: &str, command_args: &[String]) -> String {
    if command_args.is_empty() {
        format!("/{command_name}")
    } else {
        let joined = shlex::try_join(command_args.iter().map(|arg| arg.as_str()))
            .unwrap_or_else(|_| command_args.join(" "));
        format!("/{command_name} {joined}")
    }
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use pi_rust_ai_core::{
        AssistantContentBlock, AssistantMessage, AssistantMessageEvent, Context, Message,
        StopReason, StreamOptions, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
    };
    use pi_rust_ai_providers::{ApiProvider, ProviderRegistry, register_builtin_providers};
    use pi_rust_config::{ENV_AGENT_DIR, SettingsScope};
    use pi_rust_models::ModelRegistry;
    use pi_rust_oauth::AuthStorage;
    use pi_rust_packages::{PackageInstallScope, PackageManager};
    use pi_rust_plugins::{CommandRegistrationV1, PluginIdentityV1, PluginManifestV1};
    use tempfile::tempdir;

    use super::{
        CliError, ExtensionFlagType, ExtensionFlagValue, PackageCommand, RunResult, parse_args,
        parse_args_with_extension_flags, run, run_with_services,
    };

    struct EchoProvider;

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

    fn write_executable_script(path: &Path, contents: &str) {
        fs::write(path, contents).expect("write script");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("chmod");
        }
    }

    fn plugin_registration_json(id: &str, name: &str, commands: &[&str]) -> String {
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
        serde_json::to_string(&pi_rust_plugin_host::PluginMessage::Registration {
            protocol_version: pi_rust_plugin_host::HOST_PROTOCOL_VERSION_V1,
            manifest,
        })
        .expect("serialize registration")
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

    fn plugin_descriptor_json(id: &str, name: &str) -> String {
        serde_json::to_string_pretty(&pi_rust_plugin_host::PluginLaunchDescriptor {
            id: id.to_string(),
            name: name.to_string(),
            executable: PathBuf::from("plugin.sh"),
            args: Vec::new(),
            working_directory: None,
            env: Default::default(),
            description: Some(format!("{name} plugin")),
        })
        .expect("serialize descriptor")
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
    fn plugins_help_includes_the_command_group() {
        let result = run(&["plugins".to_string(), "--help".to_string()]).expect("plugins help");
        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some(super::render_plugins_help_text().trim_end().to_string()),
                stderr: None,
            }
        );
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

        let parsed = parse_args(&["--list-known-models".to_string(), "codex".to_string()]);
        assert_eq!(parsed.list_known_models, Some(Some("codex".to_string())));

        let parsed = parse_args(&["--list-known-models".to_string(), "--verbose".to_string()]);
        assert_eq!(parsed.list_known_models, Some(None));
        assert!(parsed.verbose);
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
    fn list_models_only_shows_available_models() {
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

        let result = run(&[
            "--list-models".to_string(),
            "openai-codex/gpt-5.3-codex-spark".to_string(),
        ])
        .expect("list models");

        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some("openai-codex/gpt-5.3-codex-spark".to_string()),
                stderr: None,
            }
        );
    }

    #[test]
    fn list_known_models_reports_auth_source_markers() {
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

        let result = run(&[
            "--list-known-models".to_string(),
            "openai-codex/gpt-5.3-codex-spark".to_string(),
        ])
        .expect("list known models");

        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some("openai-codex/gpt-5.3-codex-spark [stored-oauth]".to_string()),
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
    fn plugins_add_and_remove_roots_update_project_settings() {
        let _guard = super::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let plugin_root = cwd.join("plugins");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&agent_dir).expect("agent dir");
        fs::create_dir_all(&plugin_root).expect("plugin root");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        let original_cwd = std::env::current_dir().expect("cwd");
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };
        std::env::set_current_dir(&cwd).expect("set cwd");

        let add = run(&[
            "plugins".to_string(),
            "add-root".to_string(),
            "./plugins".to_string(),
            "--project".to_string(),
        ])
        .expect("add plugin root");
        let settings: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(cwd.join(".pi").join("settings.json")).expect("settings"),
        )
        .expect("parse settings");
        let remove = run(&[
            "plugins".to_string(),
            "remove-root".to_string(),
            "./plugins".to_string(),
            "--project".to_string(),
        ])
        .expect("remove plugin root");
        let settings_after_remove: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(cwd.join(".pi").join("settings.json")).expect("settings"),
        )
        .expect("parse settings");

        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(
            add,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some("Added plugin root ./plugins to project settings.".to_string()),
                stderr: None,
            }
        );
        assert_eq!(settings["pluginRoots"], serde_json::json!(["../plugins"]));
        assert_eq!(
            remove,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some("Removed plugin root ./plugins from project settings.".to_string()),
                stderr: None,
            }
        );
        assert_eq!(settings_after_remove["pluginRoots"], serde_json::json!([]));
    }

    #[test]
    fn plugins_add_root_keeps_local_as_project_alias() {
        let _guard = super::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let plugin_root = cwd.join("plugins");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&agent_dir).expect("agent dir");
        fs::create_dir_all(&plugin_root).expect("plugin root");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        let original_cwd = std::env::current_dir().expect("cwd");
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };
        std::env::set_current_dir(&cwd).expect("set cwd");

        let add = run(&[
            "plugins".to_string(),
            "add-root".to_string(),
            "./plugins".to_string(),
            "--local".to_string(),
        ])
        .expect("add plugin root");
        let settings: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(cwd.join(".pi").join("settings.json")).expect("settings"),
        )
        .expect("parse settings");

        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(
            add,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some("Added plugin root ./plugins to project settings.".to_string()),
                stderr: None,
            }
        );
        assert_eq!(settings["pluginRoots"], serde_json::json!(["../plugins"]));
    }

    #[test]
    fn plugins_list_emits_json_diagnostics_for_discovered_plugins() {
        let _guard = super::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let plugin_root = cwd.join("plugins");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&agent_dir).expect("agent dir");
        fs::create_dir_all(&plugin_root).expect("plugin root");

        write_executable_script(
            &plugin_root.join("plugin.sh"),
            &plugin_runtime_script(
                &plugin_registration_json("list-plugin", "List Plugin", &["list"]),
                "",
            ),
        );
        fs::write(
            plugin_root.join("pi-plugin-host.json"),
            plugin_descriptor_json("list-plugin", "List Plugin"),
        )
        .expect("write descriptor");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        let original_cwd = std::env::current_dir().expect("cwd");
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };
        std::env::set_current_dir(&cwd).expect("set cwd");

        let mut settings = pi_rust_config::SettingsManager::create(&cwd, Some(agent_dir.clone()));
        settings
            .set_plugin_roots(
                pi_rust_config::SettingsScope::Project,
                &["../plugins".to_string()],
            )
            .expect("seed plugin roots");

        let result = run(&[
            "plugins".to_string(),
            "list".to_string(),
            "--mode".to_string(),
            "json".to_string(),
        ])
        .expect("plugin list");

        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        let stdout = match result {
            RunResult::Completed {
                exit_code,
                stdout,
                stderr,
            } => {
                assert_eq!(exit_code, 0);
                assert!(stderr.is_none());
                stdout.expect("json stdout")
            }
        };
        let json: serde_json::Value = serde_json::from_str(&stdout).expect("json output");
        assert_eq!(json["plugins"][0]["pluginId"], serde_json::json!("list-plugin"));
        assert_eq!(json["warnings"], serde_json::json!([]));
    }

    #[test]
    fn plugin_discovery_order_includes_project_and_user_plugin_roots_between_packages() {
        let _guard = super::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let project_package = cwd.join("project-package");
        let user_package = cwd.join("user-package");
        let project_plugin_root = cwd.join("project-plugins");
        let user_plugin_root = agent_dir.join("plugins");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&agent_dir).expect("agent dir");
        fs::create_dir_all(&project_package).expect("project package");
        fs::create_dir_all(&user_package).expect("user package");
        fs::create_dir_all(&project_plugin_root).expect("project plugin root");
        fs::create_dir_all(&user_plugin_root).expect("user plugin root");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        let original_cwd = std::env::current_dir().expect("cwd");
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };
        std::env::set_current_dir(&cwd).expect("set cwd");

        let mut package_manager = PackageManager::create(&cwd, Some(agent_dir.clone()));
        package_manager
            .install(
                project_package.to_string_lossy().as_ref(),
                PackageInstallScope::Project,
            )
            .expect("install project package");
        package_manager
            .install(user_package.to_string_lossy().as_ref(), PackageInstallScope::User)
            .expect("install user package");
        package_manager
            .settings_manager_mut()
            .set_plugin_roots(
                SettingsScope::Project,
                &["../project-plugins".to_string()],
            )
            .expect("seed project plugin roots");
        package_manager
            .settings_manager_mut()
            .set_plugin_roots(SettingsScope::Global, &["plugins".to_string()])
            .expect("seed user plugin roots");

        let discovery_roots = super::build_plugin_discovery_roots(&package_manager);

        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(
            discovery_roots,
            vec![
                super::normalize_path(
                    &package_manager.list_by_scope(PackageInstallScope::Project)[0]
                        .install_path
                        .clone(),
                ),
                super::normalize_path(
                    &package_manager.list_by_scope(PackageInstallScope::User)[0]
                        .install_path
                        .clone(),
                ),
                super::normalize_path(&project_plugin_root),
                super::normalize_path(&user_plugin_root),
            ]
        );
    }

    #[test]
    fn top_level_plugin_command_executes_through_non_interactive_path() {
        let _guard = super::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let plugin_root = tempdir.path().join("plugin-package");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&agent_dir).expect("agent dir");
        fs::create_dir_all(&plugin_root).expect("plugin root");

        write_executable_script(
            &plugin_root.join("plugin.sh"),
            &plugin_runtime_script(
                &plugin_registration_json("rewrite-plugin", "Rewrite Plugin", &["rewrite"]),
                r#"
request = json.loads(sys.stdin.readline())
assert request["type"] == "command_request"
print(json.dumps({
    "type": "command_response",
    "requestId": request["requestId"],
    "replacement": "rewritten:" + "|".join(request["args"]),
}), flush=True)
"#,
            ),
        );
        fs::write(
            plugin_root.join("pi-plugin-host.json"),
            plugin_descriptor_json("rewrite-plugin", "Rewrite Plugin"),
        )
        .expect("write descriptor");

        let mut package_manager = PackageManager::create(&cwd, Some(agent_dir.clone()));
        package_manager
            .install(
                plugin_root.to_string_lossy().as_ref(),
                PackageInstallScope::Project,
            )
            .expect("install plugin package");
        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        let original_cwd = std::env::current_dir().expect("cwd");
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };
        std::env::set_current_dir(&cwd).expect("set cwd");

        let mut providers = ProviderRegistry::new();
        providers.register(Arc::new(EchoProvider));
        let mut auth = AuthStorage::in_memory(Default::default());
        auth.set_runtime_api_key("openai", "runtime-key");
        let mut models = ModelRegistry::new(auth, None);

        let result = run_with_services(
            &[
                "rewrite".to_string(),
                "alpha beta".to_string(),
                "gamma".to_string(),
            ],
            &mut providers,
            &mut models,
        )
        .expect("run top-level plugin command");

        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

        assert_eq!(
            result,
            RunResult::Completed {
                exit_code: 0,
                stdout: Some("echo:rewritten:alpha beta|gamma".to_string()),
                stderr: None,
            }
        );
    }

    #[test]
    fn built_in_package_commands_still_beat_same_named_plugins() {
        let _guard = super::test_env_guard().lock().expect("env guard");
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let plugin_root = tempdir.path().join("plugin-package");
        fs::create_dir_all(&cwd).expect("cwd");
        fs::create_dir_all(&agent_dir).expect("agent dir");
        fs::create_dir_all(&plugin_root).expect("plugin root");

        write_executable_script(
            &plugin_root.join("plugin.sh"),
            &plugin_runtime_script(
                &plugin_registration_json("install-plugin", "Install Plugin", &["install"]),
                r#"
request = json.loads(sys.stdin.readline())
assert request["type"] == "command_request"
print(json.dumps({
    "type": "command_response",
    "requestId": request["requestId"],
    "replacement": "plugin-install-ran",
}), flush=True)
"#,
            ),
        );
        fs::write(
            plugin_root.join("pi-plugin-host.json"),
            plugin_descriptor_json("install-plugin", "Install Plugin"),
        )
        .expect("write descriptor");

        let mut package_manager = PackageManager::create(&cwd, Some(agent_dir.clone()));
        package_manager
            .install(
                plugin_root.to_string_lossy().as_ref(),
                PackageInstallScope::Project,
            )
            .expect("install plugin package");

        let original_agent_dir = std::env::var_os(ENV_AGENT_DIR);
        let original_cwd = std::env::current_dir().expect("cwd");
        unsafe { std::env::set_var(ENV_AGENT_DIR, &agent_dir) };
        std::env::set_current_dir(&cwd).expect("set cwd");

        let result = run(&["install".to_string(), "--help".to_string()]).expect("package help");

        std::env::set_current_dir(&original_cwd).expect("restore cwd");
        match original_agent_dir {
            Some(value) => unsafe { std::env::set_var(ENV_AGENT_DIR, value) },
            None => unsafe { std::env::remove_var(ENV_AGENT_DIR) },
        }

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
}
