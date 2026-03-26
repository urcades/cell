use std::sync::{Arc, Mutex};

use cell_config::SettingsManager;
use cell_packages::{PackageInstallScope, PackageManager};
use cell_plugin_host::{
    ActivePluginRegistry, LoadedPluginRuntime, PluginHost, PluginHostConfig, PluginHostWarning,
    PluginStartupSummary,
};
use cell_resources::{
    ContextDocument, DiscoveredResources, LoadedTextResource, ParsedResources, PromptTemplate,
    ResourceDiagnostic, ResourceDiscoveryOptions, ResourceScope, ScopedPath, SkillDefinition,
    discover_resources_with_options, load_agents_context_files, load_append_system_prompt,
    load_discovered_resources, load_system_prompt, parse_loaded_resources,
};

use crate::NonInteractiveRequest;
use crate::agent_session::{
    StartupResourceNotice, StartupResourceNoticeSection, StartupResourceSummary,
};
use crate::system_prompt::{BuildSystemPromptOptions, build_system_prompt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionRuntimeConfig {
    pub cwd: std::path::PathBuf,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub explicit_skill_paths: Vec<std::path::PathBuf>,
    pub explicit_prompt_paths: Vec<std::path::PathBuf>,
    pub explicit_theme_paths: Vec<std::path::PathBuf>,
    pub no_skills: bool,
    pub no_prompt_templates: bool,
    pub no_themes: bool,
}

#[derive(Clone, Default)]
pub struct SessionRuntimeResources {
    pub system_prompt: String,
    pub prompt_templates: Vec<PromptTemplate>,
    pub skills: Vec<SkillDefinition>,
    pub themes: Vec<LoadedTextResource>,
    pub plugin_runtime: Option<Arc<Mutex<ActivePluginRegistry>>>,
    pub plugin_startup_summary: PluginStartupSummary,
    pub startup_summary: StartupResourceSummary,
}

pub(crate) fn runtime_config_from_request(request: &NonInteractiveRequest) -> SessionRuntimeConfig {
    SessionRuntimeConfig {
        cwd: request.cwd.clone(),
        system_prompt: request.system_prompt.clone(),
        append_system_prompt: request.append_system_prompt.clone(),
        explicit_skill_paths: request.skills.clone(),
        explicit_prompt_paths: request.prompt_templates.clone(),
        explicit_theme_paths: request.themes.clone(),
        no_skills: request.no_skills,
        no_prompt_templates: request.no_prompt_templates,
        no_themes: request.no_themes,
    }
}

#[allow(dead_code)]
pub fn load_session_runtime_resources(
    request: &NonInteractiveRequest,
    enabled_tool_names: &[String],
) -> SessionRuntimeResources {
    let settings_manager = SettingsManager::create(&request.cwd, None);
    load_session_runtime_resources_with_settings(
        &runtime_config_from_request(request),
        settings_manager,
        enabled_tool_names,
    )
}

pub(crate) fn load_session_runtime_resources_with_settings(
    config: &SessionRuntimeConfig,
    settings_manager: SettingsManager,
    enabled_tool_names: &[String],
) -> SessionRuntimeResources {
    let package_manager =
        PackageManager::with_settings_manager(&config.cwd, None, settings_manager.clone());
    let discovered = discover_resources_with_options(&ResourceDiscoveryOptions {
        cwd: config.cwd.clone(),
        agent_dir: Some(package_manager.agent_dir().to_path_buf()),
        settings_manager: Some(settings_manager),
        package_roots: package_manager
            .resource_roots()
            .into_iter()
            .map(|(scope, path)| ScopedPath {
                scope: resource_scope_for_package(scope),
                path,
            })
            .collect(),
        explicit_skill_paths: config.explicit_skill_paths.clone(),
        explicit_prompt_paths: config.explicit_prompt_paths.clone(),
        explicit_theme_paths: config.explicit_theme_paths.clone(),
        no_skills: config.no_skills,
        no_prompt_templates: config.no_prompt_templates,
        no_themes: config.no_themes,
    });
    let loaded = load_discovered_resources(&discovered);
    let parsed = parse_loaded_resources(&loaded);

    let (context_files, context_diagnostics) =
        load_agents_context_files(&config.cwd, Some(package_manager.agent_dir().to_path_buf()));

    let system_prompt_source = config.system_prompt.clone().or_else(|| {
        load_system_prompt(&config.cwd, Some(package_manager.agent_dir().to_path_buf()))
            .ok()
            .flatten()
            .map(|document| document.content)
    });
    let append_system_prompt = config.append_system_prompt.clone().or_else(|| {
        load_append_system_prompt(&config.cwd, Some(package_manager.agent_dir().to_path_buf()))
            .ok()
            .flatten()
            .map(|document| document.content)
    });

    let mut startup_summary =
        build_startup_resource_summary(&discovered, &parsed, &context_files, &context_diagnostics);
    let plugin_runtime = load_runtime_plugin_registry(&package_manager, &config.cwd);
    let plugin_startup_summary = plugin_runtime.summary.clone();
    startup_summary.extensions = plugin_startup_summary
        .summaries
        .iter()
        .map(|summary| summary.descriptor_path.clone())
        .collect();
    startup_summary.extension_summaries = plugin_startup_summary
        .summaries
        .iter()
        .map(format_startup_plugin_summary)
        .collect();
    startup_summary.notices.extend(
        plugin_startup_summary
            .warnings
            .iter()
            .cloned()
            .map(plugin_warning_to_notice),
    );

    SessionRuntimeResources {
        system_prompt: build_system_prompt(BuildSystemPromptOptions {
            custom_prompt: system_prompt_source.as_deref(),
            selected_tools: enabled_tool_names,
            append_system_prompt: append_system_prompt.as_deref(),
            cwd: &config.cwd,
            context_files: &context_files,
            skills: &parsed.skills,
        }),
        prompt_templates: parsed.prompts,
        skills: parsed.skills,
        themes: parsed.themes,
        plugin_runtime: plugin_runtime
            .registry
            .map(|registry| Arc::new(Mutex::new(registry))),
        plugin_startup_summary,
        startup_summary,
    }
}

fn resource_scope_for_package(scope: PackageInstallScope) -> ResourceScope {
    match scope {
        PackageInstallScope::Project => ResourceScope::Project,
        PackageInstallScope::User | PackageInstallScope::Temporary => ResourceScope::Global,
    }
}

fn build_startup_resource_summary(
    discovered: &DiscoveredResources,
    parsed: &ParsedResources,
    context_files: &[ContextDocument],
    context_diagnostics: &[ResourceDiagnostic],
) -> StartupResourceSummary {
    let conflicts = parsed
        .diagnostics
        .iter()
        .map(|diagnostic| StartupResourceNotice {
            section: classify_resource_notice_section(&diagnostic.path, discovered),
            path: diagnostic.path.clone(),
            message: diagnostic.message.clone(),
        })
        .collect::<Vec<_>>();
    let mut notices = Vec::new();
    notices.extend(
        context_diagnostics
            .iter()
            .map(|diagnostic| StartupResourceNotice {
                section: StartupResourceNoticeSection::Context,
                path: diagnostic.path.clone(),
                message: diagnostic.message.clone(),
            }),
    );
    notices.extend(conflicts.iter().cloned());

    StartupResourceSummary {
        context_paths: context_files
            .iter()
            .map(|document| document.path.clone())
            .collect(),
        skills: discovered
            .skills
            .iter()
            .map(|resource| resource.path.clone())
            .collect(),
        prompts: discovered
            .prompts
            .iter()
            .map(|resource| resource.path.clone())
            .collect(),
        extensions: Vec::new(),
        extension_summaries: Vec::new(),
        themes: discovered
            .themes
            .iter()
            .map(|resource| resource.path.clone())
            .collect(),
        conflicts,
        notices,
    }
}

fn classify_resource_notice_section(
    path: &std::path::Path,
    discovered: &DiscoveredResources,
) -> StartupResourceNoticeSection {
    if discovered
        .skills
        .iter()
        .any(|resource| resource.path == path)
    {
        StartupResourceNoticeSection::Skill
    } else if discovered
        .prompts
        .iter()
        .any(|resource| resource.path == path)
    {
        StartupResourceNoticeSection::Prompt
    } else if discovered
        .themes
        .iter()
        .any(|resource| resource.path == path)
    {
        StartupResourceNoticeSection::Theme
    } else {
        StartupResourceNoticeSection::Resource
    }
}

fn load_runtime_plugin_registry(
    package_manager: &PackageManager,
    cwd: &std::path::Path,
) -> LoadedPluginRuntime {
    let discovery_roots = package_manager
        .resource_roots()
        .into_iter()
        .map(|(_, path)| path)
        .collect::<Vec<_>>();
    if discovery_roots.is_empty() {
        return LoadedPluginRuntime {
            registry: None,
            summary: PluginStartupSummary::default(),
        };
    }

    PluginHost::new(PluginHostConfig {
        discovery_roots,
        workspace_root: Some(cwd.to_path_buf()),
        ..PluginHostConfig::default()
    })
    .discover_and_load_runtime_plugins()
}

fn format_startup_plugin_summary(summary: &cell_plugin_host::RegisteredPluginSummary) -> String {
    let capability_parts = [
        pluralize_summary_count(summary.commands.len(), "command"),
        pluralize_summary_count(summary.tools.len(), "tool"),
        pluralize_summary_count(summary.flags.len(), "flag"),
        pluralize_summary_count(summary.hooks.len(), "hook"),
        pluralize_summary_count(summary.providers.len(), "provider"),
        pluralize_summary_count(summary.models.len(), "model"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let capability_text = if capability_parts.is_empty() {
        "no registered capabilities".to_string()
    } else {
        capability_parts.join(", ")
    };

    format!(
        "{} [{}] v{} - {} - {}",
        summary.plugin_name,
        summary.plugin_id,
        summary.manifest_version,
        capability_text,
        summary.descriptor_path.to_string_lossy()
    )
}

fn pluralize_summary_count(count: usize, label: &'static str) -> Option<String> {
    if count == 0 {
        None
    } else if count == 1 {
        Some(format!("1 {label}"))
    } else {
        Some(format!("{count} {label}s"))
    }
}

fn plugin_warning_to_notice(warning: PluginHostWarning) -> StartupResourceNotice {
    let label = match (warning.plugin_name.as_deref(), warning.plugin_id.as_deref()) {
        (Some(name), Some(plugin_id)) => format!("{name} [{plugin_id}]"),
        (Some(name), None) => name.to_string(),
        (None, Some(plugin_id)) => plugin_id.to_string(),
        (None, None) => warning.path.to_string_lossy().to_string(),
    };

    StartupResourceNotice {
        section: StartupResourceNoticeSection::Extension,
        path: warning.path,
        message: format!("{label}: {}", warning.message),
    }
}
