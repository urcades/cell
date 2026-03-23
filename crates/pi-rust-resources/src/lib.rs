use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(test)]
use pi_rust_config::PROJECT_CONFIG_DIR_NAME;
use pi_rust_config::{SettingsManager, SettingsScope, get_agent_dir, get_project_config_dir};
use serde::Deserialize;
use thiserror::Error;

pub const SUPPORTED_RESOURCE_TYPES: &[&str] = &["skills", "prompts", "themes", "agents", "system"];

#[derive(Clone, Debug, Default, Deserialize)]
struct PiManifest {
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    prompts: Option<Vec<String>>,
    #[serde(default)]
    themes: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IgnoreRule {
    pattern: String,
    negated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceScope {
    Global,
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedPath {
    pub scope: ResourceScope,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveredResources {
    pub skills: Vec<ScopedPath>,
    pub prompts: Vec<ScopedPath>,
    pub themes: Vec<ScopedPath>,
}

#[derive(Clone, Debug, Default)]
pub struct ResourceDiscoveryOptions {
    pub cwd: PathBuf,
    pub agent_dir: Option<PathBuf>,
    pub settings_manager: Option<SettingsManager>,
    pub package_roots: Vec<ScopedPath>,
    pub explicit_skill_paths: Vec<PathBuf>,
    pub explicit_prompt_paths: Vec<PathBuf>,
    pub explicit_theme_paths: Vec<PathBuf>,
    pub no_skills: bool,
    pub no_prompt_templates: bool,
    pub no_themes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedTextResource {
    pub scope: ResourceScope,
    pub path: PathBuf,
    pub content: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadedResources {
    pub skills: Vec<LoadedTextResource>,
    pub prompts: Vec<LoadedTextResource>,
    pub themes: Vec<LoadedTextResource>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextDocument {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub content: String,
    pub scope: ResourceScope,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub content: String,
    pub scope: ResourceScope,
    pub path: PathBuf,
    pub disable_model_invocation: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedResources {
    pub prompts: Vec<PromptTemplate>,
    pub skills: Vec<SkillDefinition>,
    pub themes: Vec<LoadedTextResource>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceDiagnostic {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum ResourceError {
    #[error("failed to read resource file {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

pub fn discover_resources(
    cwd: impl AsRef<Path>,
    agent_dir: Option<PathBuf>,
) -> DiscoveredResources {
    discover_resources_with_options(&ResourceDiscoveryOptions {
        cwd: cwd.as_ref().to_path_buf(),
        agent_dir,
        ..ResourceDiscoveryOptions::default()
    })
}

pub fn discover_resources_with_options(options: &ResourceDiscoveryOptions) -> DiscoveredResources {
    let agent_dir = options.agent_dir.clone().unwrap_or_else(get_agent_dir);
    let discovered = discover_resources_internal(&options.cwd, &agent_dir, home_dir().as_deref());
    let mut resources = DiscoveredResources::default();
    let mut seen_skills = HashMap::new();
    let mut seen_prompts = HashMap::new();
    let mut seen_themes = HashMap::new();

    if !options.no_skills {
        add_scoped_paths_with_scope(&mut resources.skills, &mut seen_skills, discovered.skills);
        add_settings_resource_paths(
            &mut resources.skills,
            &mut seen_skills,
            options.settings_manager.as_ref(),
            "skills",
            &options.cwd,
            &agent_dir,
            true,
        );
        add_package_resource_paths(
            &mut resources.skills,
            &mut seen_skills,
            &options.package_roots,
            "skills",
            true,
        );
    }

    if !options.no_prompt_templates {
        add_scoped_paths_with_scope(
            &mut resources.prompts,
            &mut seen_prompts,
            discovered.prompts,
        );
        add_settings_resource_paths(
            &mut resources.prompts,
            &mut seen_prompts,
            options.settings_manager.as_ref(),
            "prompts",
            &options.cwd,
            &agent_dir,
            false,
        );
        add_package_resource_paths(
            &mut resources.prompts,
            &mut seen_prompts,
            &options.package_roots,
            "prompts",
            false,
        );
    }

    if !options.no_themes {
        add_scoped_paths_with_scope(&mut resources.themes, &mut seen_themes, discovered.themes);
        add_settings_resource_paths(
            &mut resources.themes,
            &mut seen_themes,
            options.settings_manager.as_ref(),
            "themes",
            &options.cwd,
            &agent_dir,
            false,
        );
        add_package_resource_paths(
            &mut resources.themes,
            &mut seen_themes,
            &options.package_roots,
            "themes",
            false,
        );
    }

    add_explicit_resource_paths(
        &mut resources.skills,
        &mut seen_skills,
        &options.explicit_skill_paths,
        ResourceScope::Project,
        true,
    );
    add_explicit_resource_paths(
        &mut resources.prompts,
        &mut seen_prompts,
        &options.explicit_prompt_paths,
        ResourceScope::Project,
        false,
    );
    add_explicit_resource_paths(
        &mut resources.themes,
        &mut seen_themes,
        &options.explicit_theme_paths,
        ResourceScope::Project,
        false,
    );

    resources
}

pub fn load_discovered_resources(discovered: &DiscoveredResources) -> LoadedResources {
    let mut diagnostics = Vec::new();
    let skills = load_scoped_paths(&discovered.skills, &mut diagnostics);
    let prompts = load_scoped_paths(&discovered.prompts, &mut diagnostics);
    let themes = load_scoped_paths(&discovered.themes, &mut diagnostics);

    LoadedResources {
        skills,
        prompts,
        themes,
        diagnostics,
    }
}

pub fn parse_loaded_resources(loaded: &LoadedResources) -> ParsedResources {
    let mut diagnostics = loaded.diagnostics.clone();
    let mut prompts = Vec::new();
    let mut seen_prompts = HashSet::new();
    for resource in &loaded.prompts {
        match parse_prompt_template(resource) {
            Ok(prompt) => {
                if seen_prompts.insert(prompt.name.clone()) {
                    prompts.push(prompt);
                } else {
                    diagnostics.push(ResourceDiagnostic {
                        path: resource.path.clone(),
                        message: "name collision".to_string(),
                    });
                }
            }
            Err(message) => diagnostics.push(ResourceDiagnostic {
                path: resource.path.clone(),
                message,
            }),
        }
    }

    let mut skills = Vec::new();
    let mut seen_skills = HashSet::new();
    for resource in &loaded.skills {
        match parse_skill_definition(resource) {
            Ok(Some(skill)) => {
                if seen_skills.insert(skill.name.clone()) {
                    skills.push(skill);
                } else {
                    diagnostics.push(ResourceDiagnostic {
                        path: resource.path.clone(),
                        message: "name collision".to_string(),
                    });
                }
            }
            Ok(None) => {}
            Err(message) => diagnostics.push(ResourceDiagnostic {
                path: resource.path.clone(),
                message,
            }),
        }
    }

    ParsedResources {
        prompts,
        skills,
        themes: loaded.themes.clone(),
        diagnostics,
    }
}

pub fn format_skills_for_prompt(skills: &[SkillDefinition]) -> String {
    let visible_skills = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect::<Vec<_>>();
    if visible_skills.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        String::new(),
        String::new(),
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        "Use the read tool to load a skill's file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];
    for skill in visible_skills {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.path.to_string_lossy())
        ));
        lines.push("  </skill>".to_string());
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    let Some(template_name) = text.strip_prefix('/') else {
        return text.to_string();
    };
    let (command_name, args) = if let Some(index) = template_name.find(' ') {
        (&template_name[..index], &template_name[index + 1..])
    } else {
        (template_name, "")
    };
    let Some(template) = templates
        .iter()
        .find(|template| template.name == command_name)
    else {
        return text.to_string();
    };
    substitute_args(&template.content, &parse_command_args(args))
}

pub fn discover_agents_context_paths(
    cwd: impl AsRef<Path>,
    agent_dir: Option<PathBuf>,
) -> Vec<PathBuf> {
    let cwd = cwd.as_ref();
    let agent_dir = agent_dir.unwrap_or_else(get_agent_dir);
    discover_agents_context_paths_internal(cwd, &agent_dir)
}

pub fn load_agents_context_files(
    cwd: impl AsRef<Path>,
    agent_dir: Option<PathBuf>,
) -> (Vec<ContextDocument>, Vec<ResourceDiagnostic>) {
    let paths = discover_agents_context_paths(cwd, agent_dir);
    let mut diagnostics = Vec::new();
    let mut files = Vec::new();

    for path in paths {
        match read_text_file(&path) {
            Ok(content) => files.push(ContextDocument { path, content }),
            Err(error) => diagnostics.push(ResourceDiagnostic {
                path: path.clone(),
                message: error.to_string(),
            }),
        }
    }

    (files, diagnostics)
}

pub fn discover_system_prompt_path(
    cwd: impl AsRef<Path>,
    agent_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    let cwd = cwd.as_ref();
    let agent_dir = agent_dir.unwrap_or_else(get_agent_dir);

    let project = get_project_config_dir(cwd).join("SYSTEM.md");
    if project.exists() {
        return Some(project);
    }

    let global = agent_dir.join("SYSTEM.md");
    if global.exists() {
        return Some(global);
    }

    None
}

pub fn discover_append_system_prompt_path(
    cwd: impl AsRef<Path>,
    agent_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    let cwd = cwd.as_ref();
    let agent_dir = agent_dir.unwrap_or_else(get_agent_dir);

    let project = get_project_config_dir(cwd).join("APPEND_SYSTEM.md");
    if project.exists() {
        return Some(project);
    }

    let global = agent_dir.join("APPEND_SYSTEM.md");
    if global.exists() {
        return Some(global);
    }

    None
}

pub fn load_system_prompt(
    cwd: impl AsRef<Path>,
    agent_dir: Option<PathBuf>,
) -> Result<Option<ContextDocument>, ResourceError> {
    let Some(path) = discover_system_prompt_path(cwd, agent_dir) else {
        return Ok(None);
    };
    let content = read_text_file(&path)?;
    Ok(Some(ContextDocument { path, content }))
}

pub fn load_append_system_prompt(
    cwd: impl AsRef<Path>,
    agent_dir: Option<PathBuf>,
) -> Result<Option<ContextDocument>, ResourceError> {
    let Some(path) = discover_append_system_prompt_path(cwd, agent_dir) else {
        return Ok(None);
    };
    let content = read_text_file(&path)?;
    Ok(Some(ContextDocument { path, content }))
}

fn discover_resources_internal(
    cwd: &Path,
    agent_dir: &Path,
    home: Option<&Path>,
) -> DiscoveredResources {
    let mut resources = DiscoveredResources::default();
    let mut seen_skills = HashMap::new();
    let mut seen_prompts = HashMap::new();
    let mut seen_themes = HashMap::new();

    let global_skills_dir = agent_dir.join("skills");
    add_scoped_paths(
        &mut resources.skills,
        &mut seen_skills,
        collect_skill_entries(&global_skills_dir, true),
        ResourceScope::Global,
    );

    if let Some(home_dir) = home {
        let agents_skills = home_dir.join(".agents").join("skills");
        add_scoped_paths(
            &mut resources.skills,
            &mut seen_skills,
            collect_skill_entries(&agents_skills, true),
            ResourceScope::Global,
        );
    }

    let project_base = get_project_config_dir(cwd);
    add_scoped_paths(
        &mut resources.skills,
        &mut seen_skills,
        collect_skill_entries(&project_base.join("skills"), true),
        ResourceScope::Project,
    );
    for ancestor_dir in collect_ancestor_agents_skill_dirs(cwd) {
        add_scoped_paths(
            &mut resources.skills,
            &mut seen_skills,
            collect_skill_entries(&ancestor_dir, true),
            ResourceScope::Project,
        );
    }

    add_scoped_paths(
        &mut resources.prompts,
        &mut seen_prompts,
        collect_top_level_files(&agent_dir.join("prompts"), "md"),
        ResourceScope::Global,
    );
    add_scoped_paths(
        &mut resources.prompts,
        &mut seen_prompts,
        collect_top_level_files(&project_base.join("prompts"), "md"),
        ResourceScope::Project,
    );

    add_scoped_paths(
        &mut resources.themes,
        &mut seen_themes,
        collect_top_level_files(&agent_dir.join("themes"), "json"),
        ResourceScope::Global,
    );
    add_scoped_paths(
        &mut resources.themes,
        &mut seen_themes,
        collect_top_level_files(&project_base.join("themes"), "json"),
        ResourceScope::Project,
    );

    resources
}

fn add_scoped_paths(
    target: &mut Vec<ScopedPath>,
    seen: &mut HashMap<PathBuf, usize>,
    paths: Vec<PathBuf>,
    scope: ResourceScope,
) {
    for path in paths {
        push_scoped_path(target, seen, ScopedPath { scope, path });
    }
}

fn add_scoped_paths_with_scope(
    target: &mut Vec<ScopedPath>,
    seen: &mut HashMap<PathBuf, usize>,
    entries: Vec<ScopedPath>,
) {
    for entry in entries {
        push_scoped_path(target, seen, entry);
    }
}

fn push_scoped_path(
    target: &mut Vec<ScopedPath>,
    seen: &mut HashMap<PathBuf, usize>,
    entry: ScopedPath,
) {
    if let Some(index) = seen.get(&entry.path).copied() {
        let existing_rank = scope_rank(target[index].scope);
        let next_rank = scope_rank(entry.scope);
        if next_rank > existing_rank {
            target[index] = entry;
        }
        return;
    }

    seen.insert(entry.path.clone(), target.len());
    target.push(entry);
}

fn scope_rank(scope: ResourceScope) -> u8 {
    match scope {
        ResourceScope::Global => 0,
        ResourceScope::Project => 1,
    }
}

fn add_settings_resource_paths(
    target: &mut Vec<ScopedPath>,
    seen: &mut HashMap<PathBuf, usize>,
    settings_manager: Option<&SettingsManager>,
    key: &str,
    cwd: &Path,
    agent_dir: &Path,
    skill_mode: bool,
) {
    let Some(settings_manager) = settings_manager else {
        return;
    };

    for (resource_scope, settings_scope, base_dir) in [
        (
            ResourceScope::Global,
            SettingsScope::Global,
            agent_dir.to_path_buf(),
        ),
        (
            ResourceScope::Project,
            SettingsScope::Project,
            get_project_config_dir(cwd),
        ),
    ] {
        let paths = settings_manager
            .get_string_list(key, Some(settings_scope))
            .into_iter()
            .flat_map(|entry| collect_path_input_entries(&base_dir, &entry, key, skill_mode))
            .collect::<Vec<_>>();
        add_scoped_paths(target, seen, paths, resource_scope);
    }
}

fn add_package_resource_paths(
    target: &mut Vec<ScopedPath>,
    seen: &mut HashMap<PathBuf, usize>,
    package_roots: &[ScopedPath],
    resource_dir_name: &str,
    skill_mode: bool,
) {
    for root in package_roots {
        let paths = collect_package_resource_paths(&root.path, resource_dir_name, skill_mode);
        add_scoped_paths(target, seen, paths, root.scope);
    }
}

fn add_explicit_resource_paths(
    target: &mut Vec<ScopedPath>,
    seen: &mut HashMap<PathBuf, usize>,
    explicit_paths: &[PathBuf],
    scope: ResourceScope,
    skill_mode: bool,
) {
    for path in explicit_paths {
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let key = if skill_mode {
            "skills"
        } else if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("json"))
        {
            "themes"
        } else {
            "prompts"
        };
        let paths = collect_path_input_entries(base_dir, &path.to_string_lossy(), key, skill_mode);
        add_scoped_paths(target, seen, paths, scope);
    }
}

fn load_scoped_paths(
    paths: &[ScopedPath],
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Vec<LoadedTextResource> {
    let mut loaded = Vec::new();
    for scoped in paths {
        match read_text_file(&scoped.path) {
            Ok(content) => loaded.push(LoadedTextResource {
                scope: scoped.scope,
                path: scoped.path.clone(),
                content,
            }),
            Err(error) => diagnostics.push(ResourceDiagnostic {
                path: scoped.path.clone(),
                message: error.to_string(),
            }),
        }
    }
    loaded
}

fn discover_agents_context_paths_internal(cwd: &Path, agent_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();

    if let Some(global) = discover_context_file_in_dir(agent_dir) {
        seen.insert(global.clone());
        paths.push(global);
    }

    let mut ancestor_paths = Vec::new();
    let mut current = cwd.to_path_buf();
    loop {
        if let Some(context_path) = discover_context_file_in_dir(&current) {
            if seen.insert(context_path.clone()) {
                ancestor_paths.push(context_path);
            }
        }

        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }

    ancestor_paths.reverse();
    paths.extend(ancestor_paths);
    paths
}

fn discover_context_file_in_dir(dir: &Path) -> Option<PathBuf> {
    for candidate in ["AGENTS.md", "CLAUDE.md"] {
        let path = dir.join(candidate);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn collect_ancestor_agents_skill_dirs(start_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut current = start_dir.to_path_buf();

    loop {
        dirs.push(current.join(".agents").join("skills"));
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }

    dirs
}

fn collect_package_resource_paths(
    package_root: &Path,
    resource_dir_name: &str,
    skill_mode: bool,
) -> Vec<PathBuf> {
    if let Some(manifest) = read_pi_manifest(package_root) {
        let manifest_entries = match resource_dir_name {
            "skills" => manifest.skills.as_deref(),
            "prompts" => manifest.prompts.as_deref(),
            "themes" => manifest.themes.as_deref(),
            _ => None,
        };
        if let Some(entries) = manifest_entries {
            return collect_manifest_resource_entries(
                package_root,
                resource_dir_name,
                entries,
                skill_mode,
            );
        }
    }

    let base = package_root.join(resource_dir_name);
    if skill_mode {
        collect_skill_entries(&base, true)
    } else if resource_dir_name == "themes" {
        collect_top_level_files(&base, "json")
    } else {
        collect_top_level_files(&base, "md")
    }
}

fn read_pi_manifest(package_root: &Path) -> Option<PiManifest> {
    let package_json_path = package_root.join("package.json");
    let content = fs::read_to_string(package_json_path).ok()?;
    #[derive(Deserialize)]
    struct PackageJson {
        #[serde(default)]
        pi: Option<PiManifest>,
    }

    serde_yaml::from_str::<PackageJson>(&content).ok()?.pi
}

fn collect_manifest_resource_entries(
    package_root: &Path,
    resource_dir_name: &str,
    entries: &[String],
    skill_mode: bool,
) -> Vec<PathBuf> {
    if entries.is_empty() {
        return Vec::new();
    }

    let (plain, patterns) = split_patterns(entries);
    let mut files = Vec::new();

    if plain.is_empty() {
        let base = package_root.join(resource_dir_name);
        files = if skill_mode {
            collect_skill_entries(&base, true)
        } else if resource_dir_name == "themes" {
            collect_top_level_files(&base, "json")
        } else {
            collect_top_level_files(&base, "md")
        };
    } else {
        for entry in plain {
            files.extend(collect_manifest_entry_paths(
                package_root,
                resource_dir_name,
                &entry,
                skill_mode,
            ));
        }
    }

    if patterns.is_empty() {
        files.sort();
        files.dedup();
        return files;
    }

    let enabled = apply_patterns(&files, &patterns, package_root);
    let mut filtered = files
        .into_iter()
        .filter(|path| enabled.contains(path))
        .collect::<Vec<_>>();
    filtered.sort();
    filtered.dedup();
    filtered
}

fn collect_manifest_entry_paths(
    package_root: &Path,
    resource_dir_name: &str,
    entry: &str,
    skill_mode: bool,
) -> Vec<PathBuf> {
    let path = if Path::new(entry).is_absolute() {
        PathBuf::from(entry)
    } else {
        package_root.join(entry)
    };

    if path.is_file() {
        if resource_matches_path(&path, resource_dir_name, skill_mode) {
            return vec![path];
        }
        return Vec::new();
    }
    if !path.is_dir() {
        return Vec::new();
    }

    if skill_mode {
        return collect_skill_entries(&path, true);
    }
    if resource_dir_name == "themes" {
        return collect_top_level_files(&path, "json");
    }
    collect_top_level_files(&path, "md")
}

fn split_patterns(entries: &[String]) -> (Vec<String>, Vec<String>) {
    let mut plain = Vec::new();
    let mut patterns = Vec::new();
    for entry in entries {
        if is_pattern(entry) {
            patterns.push(entry.clone());
        } else {
            plain.push(entry.clone());
        }
    }
    (plain, patterns)
}

fn is_pattern(value: &str) -> bool {
    value.starts_with('!')
        || value.starts_with('+')
        || value.starts_with('-')
        || value.contains('*')
        || value.contains('?')
}

fn apply_patterns(all_paths: &[PathBuf], patterns: &[String], base_dir: &Path) -> HashSet<PathBuf> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    let mut force_includes = Vec::new();
    let mut force_excludes = Vec::new();

    for pattern in patterns {
        if let Some(rest) = pattern.strip_prefix('+') {
            force_includes.push(rest.to_string());
        } else if let Some(rest) = pattern.strip_prefix('-') {
            force_excludes.push(rest.to_string());
        } else if let Some(rest) = pattern.strip_prefix('!') {
            excludes.push(rest.to_string());
        } else {
            includes.push(pattern.to_string());
        }
    }

    let mut result: Vec<PathBuf> = if includes.is_empty() {
        all_paths.to_vec()
    } else {
        all_paths
            .iter()
            .filter(|path| matches_any_pattern(path, &includes, base_dir))
            .cloned()
            .collect()
    };

    if !excludes.is_empty() {
        result.retain(|path| !matches_any_pattern(path, &excludes, base_dir));
    }

    if !force_includes.is_empty() {
        for path in all_paths {
            if !result.contains(path) && matches_any_exact_pattern(path, &force_includes, base_dir)
            {
                result.push(path.clone());
            }
        }
    }

    if !force_excludes.is_empty() {
        result.retain(|path| !matches_any_exact_pattern(path, &force_excludes, base_dir));
    }

    result.into_iter().collect()
}

fn matches_any_pattern(file_path: &Path, patterns: &[String], base_dir: &Path) -> bool {
    let rel = to_posix_path(
        relative_path(base_dir, file_path)
            .to_string_lossy()
            .as_ref(),
    );
    let name = file_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let parent = file_path.parent().unwrap_or_else(|| Path::new("."));
    let parent_rel = to_posix_path(relative_path(base_dir, parent).to_string_lossy().as_ref());

    patterns.iter().any(|pattern| {
        let normalized = normalize_pattern(pattern);
        path_pattern_matches(&normalized, &rel)
            || path_pattern_matches(&normalized, name)
            || path_pattern_matches(&normalized, &file_path.to_string_lossy())
            || path_pattern_matches(&normalized, &parent_rel)
    })
}

fn matches_any_exact_pattern(file_path: &Path, patterns: &[String], base_dir: &Path) -> bool {
    let rel = to_posix_path(
        relative_path(base_dir, file_path)
            .to_string_lossy()
            .as_ref(),
    );
    let name = file_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let parent = file_path.parent().unwrap_or_else(|| Path::new("."));
    let parent_rel = to_posix_path(relative_path(base_dir, parent).to_string_lossy().as_ref());

    patterns.iter().any(|pattern| {
        let normalized = normalize_pattern(pattern);
        normalized == rel
            || normalized == file_path.to_string_lossy()
            || normalized == name
            || normalized == parent_rel
    })
}

fn normalize_pattern(pattern: &str) -> String {
    let mut normalized = pattern.trim().replace('\\', "/");
    while normalized.starts_with("./") {
        normalized = normalized[2..].to_string();
    }
    if normalized.starts_with('/') {
        normalized = normalized[1..].to_string();
    }
    normalized
}

fn path_pattern_matches(pattern: &str, candidate: &str) -> bool {
    if pattern.is_empty() {
        return candidate.is_empty();
    }
    if pattern.ends_with('/') {
        let prefix = pattern.trim_end_matches('/');
        return candidate == prefix || candidate.starts_with(&format!("{prefix}/"));
    }
    if !pattern.contains('/') {
        return glob_matches(pattern, candidate.rsplit('/').next().unwrap_or(candidate));
    }
    glob_matches(pattern, candidate)
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    fn inner(
        pattern: &[char],
        value: &[char],
        p_index: usize,
        v_index: usize,
        memo: &mut HashMap<(usize, usize), bool>,
    ) -> bool {
        if let Some(result) = memo.get(&(p_index, v_index)) {
            return *result;
        }

        let result = if p_index == pattern.len() {
            v_index == value.len()
        } else {
            match pattern[p_index] {
                '*' => {
                    if p_index + 1 < pattern.len() && pattern[p_index + 1] == '*' {
                        inner(pattern, value, p_index + 2, v_index, memo)
                            || (v_index < value.len()
                                && inner(pattern, value, p_index, v_index + 1, memo))
                    } else {
                        inner(pattern, value, p_index + 1, v_index, memo)
                            || (v_index < value.len()
                                && value[v_index] != '/'
                                && inner(pattern, value, p_index, v_index + 1, memo))
                    }
                }
                '?' => {
                    v_index < value.len()
                        && value[v_index] != '/'
                        && inner(pattern, value, p_index + 1, v_index + 1, memo)
                }
                ch => {
                    v_index < value.len()
                        && ch == value[v_index]
                        && inner(pattern, value, p_index + 1, v_index + 1, memo)
                }
            }
        };

        memo.insert((p_index, v_index), result);
        result
    }

    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    inner(&pattern, &value, 0, 0, &mut HashMap::new())
}

fn collect_skill_entries(dir: &Path, include_root_files: bool) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    collect_skill_entries_recursive(dir, dir, include_root_files, &mut Vec::new(), &mut entries);
    entries.sort();
    entries.dedup();
    entries
}

fn collect_skill_entries_recursive(
    current: &Path,
    root: &Path,
    include_root_files: bool,
    inherited_rules: &mut Vec<IgnoreRule>,
    entries: &mut Vec<PathBuf>,
) {
    if !current.exists() {
        return;
    }

    let previous_rule_count = inherited_rules.len();
    inherited_rules.extend(load_ignore_rules(current, root));

    let Ok(read_dir) = fs::read_dir(current) else {
        inherited_rules.truncate(previous_rule_count);
        return;
    };

    for item in read_dir {
        let Ok(item) = item else {
            continue;
        };
        let path = item.path();
        let Ok(file_type) = item.file_type() else {
            continue;
        };
        let filename = item.file_name().to_string_lossy().to_string();
        if filename.starts_with('.') || filename == "node_modules" {
            continue;
        }

        let rel = path_to_posix_relative(root, &path);
        if is_ignored(&rel, file_type.is_dir(), inherited_rules) {
            continue;
        }

        if file_type.is_dir() {
            collect_skill_entries_recursive(
                &path,
                root,
                include_root_files,
                inherited_rules,
                entries,
            );
            continue;
        }

        let is_root_child = current == root;
        if is_root_child && include_root_files && filename.ends_with(".md") {
            entries.push(path);
            continue;
        }
        if filename == "SKILL.md" {
            entries.push(path);
        }
    }

    inherited_rules.truncate(previous_rule_count);
}

fn collect_top_level_files(dir: &Path, extension_without_dot: &str) -> Vec<PathBuf> {
    if !dir.exists() {
        return Vec::new();
    }

    let mut entries = Vec::new();
    let rules = load_ignore_rules(dir, dir);
    let Ok(read_dir) = fs::read_dir(dir) else {
        return entries;
    };

    for item in read_dir {
        let Ok(item) = item else {
            continue;
        };
        let Ok(file_type) = item.file_type() else {
            continue;
        };
        let name = item.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" || !file_type.is_file() {
            continue;
        }

        let path = item.path();
        let rel = path_to_posix_relative(dir, &path);
        if is_ignored(&rel, false, &rules) {
            continue;
        }

        if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(extension_without_dot))
        {
            entries.push(path);
        }
    }

    entries.sort();
    entries.dedup();
    entries
}

fn collect_path_input_entries(
    base_dir: &Path,
    entry: &str,
    key: &str,
    skill_mode: bool,
) -> Vec<PathBuf> {
    let path = if Path::new(entry).is_absolute() {
        PathBuf::from(entry)
    } else {
        base_dir.join(entry)
    };

    if path.is_file() {
        return vec![path];
    }
    if !path.is_dir() {
        return vec![path];
    }

    if skill_mode {
        return collect_skill_entries(&path, true);
    }
    if key == "themes" {
        return collect_top_level_files(&path, "json");
    }
    collect_top_level_files(&path, "md")
}

fn resource_matches_path(path: &Path, resource_dir_name: &str, skill_mode: bool) -> bool {
    if skill_mode {
        return path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("md"));
    }
    match resource_dir_name {
        "themes" => path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("json")),
        _ => path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("md")),
    }
}

fn load_ignore_rules(dir: &Path, root: &Path) -> Vec<IgnoreRule> {
    let mut rules = Vec::new();
    let prefix = path_prefix(root, dir);
    for filename in [".gitignore", ".ignore", ".fdignore"] {
        let ignore_path = dir.join(filename);
        let Ok(content) = fs::read_to_string(ignore_path) else {
            continue;
        };
        for line in content.lines() {
            if let Some(rule) = prefix_ignore_pattern(line, &prefix) {
                rules.push(rule);
            }
        }
    }
    rules
}

fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<IgnoreRule> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("\\#") {
        return None;
    }

    let mut pattern = trimmed.to_string();
    let mut negated = false;
    if pattern.starts_with('!') {
        negated = true;
        pattern.remove(0);
    } else if pattern.starts_with("\\!") {
        pattern.remove(0);
    }

    if pattern.starts_with('/') {
        pattern.remove(0);
    }
    if pattern.starts_with("\\#") {
        pattern.remove(0);
    }

    let pattern = if prefix.is_empty() {
        pattern
    } else {
        format!("{prefix}{pattern}")
    };
    Some(IgnoreRule { pattern, negated })
}

fn is_ignored(path: &str, is_dir: bool, rules: &[IgnoreRule]) -> bool {
    let mut ignored = false;
    for rule in rules {
        if path_pattern_matches(&normalize_pattern(&rule.pattern), path)
            || (is_dir
                && path_pattern_matches(&normalize_pattern(&rule.pattern), &format!("{path}/")))
        {
            ignored = !rule.negated;
        }
    }
    ignored
}

fn path_prefix(root: &Path, current: &Path) -> String {
    let relative = relative_path(root, current);
    let relative = relative.to_string_lossy();
    if relative.is_empty() || relative == "." {
        String::new()
    } else {
        format!("{}/", to_posix_path(relative.as_ref()))
    }
}

fn to_posix_path(value: &str) -> String {
    value.replace('\\', "/")
}

fn relative_path(from: &Path, to: &Path) -> PathBuf {
    to.strip_prefix(from)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| to.to_path_buf())
}

fn path_to_posix_relative(base: &Path, path: &Path) -> String {
    to_posix_path(relative_path(base, path).to_string_lossy().as_ref())
}

fn read_text_file(path: &Path) -> Result<String, ResourceError> {
    fs::read_to_string(path).map_err(|source| ResourceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[derive(Default, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "disable-model-invocation")]
    disable_model_invocation: Option<bool>,
}

fn parse_prompt_template(resource: &LoadedTextResource) -> Result<PromptTemplate, String> {
    let (frontmatter, body) = parse_frontmatter(&resource.content)?;
    let description = frontmatter
        .description
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            first_non_empty_line(&body)
                .map(|line| truncate_description(line, 60))
                .unwrap_or_default()
        });
    let label = match resource.scope {
        ResourceScope::Global => "(user)".to_string(),
        ResourceScope::Project => "(project)".to_string(),
    };
    Ok(PromptTemplate {
        name: resource
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string(),
        description: if description.is_empty() {
            label
        } else {
            format!("{description} {label}")
        },
        content: body,
        scope: resource.scope,
        path: resource.path.clone(),
    })
}

fn parse_skill_definition(
    resource: &LoadedTextResource,
) -> Result<Option<SkillDefinition>, String> {
    let (frontmatter, body) = parse_frontmatter(&resource.content)?;
    let Some(description) = frontmatter
        .description
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    let skill_dir = resource.path.parent().unwrap_or_else(|| Path::new("."));
    let default_name = skill_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string();
    Ok(Some(SkillDefinition {
        name: frontmatter.name.unwrap_or(default_name),
        description,
        content: body,
        scope: resource.scope,
        path: resource.path.clone(),
        disable_model_invocation: frontmatter.disable_model_invocation.unwrap_or(false),
    }))
}

fn parse_frontmatter(content: &str) -> Result<(Frontmatter, String), String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return Ok((Frontmatter::default(), normalized));
    }
    let Some(end_index) = normalized[3..].find("\n---") else {
        return Ok((Frontmatter::default(), normalized));
    };
    let end_index = end_index + 3;
    let yaml = &normalized[4..end_index];
    let body = normalized[end_index + 4..].trim().to_string();
    let frontmatter =
        serde_yaml::from_str::<Frontmatter>(yaml).map_err(|error| error.to_string())?;
    Ok((frontmatter, body))
}

fn first_non_empty_line(value: &str) -> Option<&str> {
    value.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn truncate_description(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

fn parse_command_args(args: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in args.chars() {
        match quote {
            Some(active_quote) if character == active_quote => quote = None,
            Some(_) => current.push(character),
            None if character == '"' || character == '\'' => quote = Some(character),
            None if character == ' ' || character == '\t' => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn substitute_args(content: &str, args: &[String]) -> String {
    let mut result = content.to_string();
    let all_args = args.join(" ");

    for index in (1..=args.len()).rev() {
        result = result.replace(&format!("${index}"), args[index - 1].as_str());
    }

    while let Some(start) = result.find("${@:") {
        let Some(end) = result[start..].find('}') else {
            break;
        };
        let expression = &result[start + 4..start + end];
        let replacement = slice_args_expression(expression, args);
        result.replace_range(start..start + end + 1, &replacement);
    }

    result = result.replace("$ARGUMENTS", &all_args);
    result.replace("$@", &all_args)
}

fn slice_args_expression(expression: &str, args: &[String]) -> String {
    let mut parts = expression.split(':');
    let start = parts
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .saturating_sub(1);
    if let Some(length) = parts.next().and_then(|value| value.parse::<usize>().ok()) {
        args.iter()
            .skip(start)
            .take(length)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        args.iter()
            .skip(start)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use pi_rust_config::SettingsManager;
    use tempfile::tempdir;

    use super::*;

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, content).expect("write file");
    }

    #[test]
    fn discovers_skills_prompts_and_themes_from_global_and_project_roots() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace").join("app");
        let agent_dir = tempdir.path().join("agent");
        let fake_home = tempdir.path().join("home");

        write_file(&agent_dir.join("skills").join("global.md"), "global skill");
        write_file(
            &agent_dir.join("skills").join("nested").join("SKILL.md"),
            "global nested skill",
        );
        write_file(
            &agent_dir.join("prompts").join("global.md"),
            "global prompt",
        );
        write_file(&agent_dir.join("themes").join("global.json"), "{}");

        write_file(
            &fake_home.join(".agents").join("skills").join("home.md"),
            "home skill",
        );

        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("skills")
                .join("project.md"),
            "project skill",
        );
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("skills")
                .join("pkg")
                .join("SKILL.md"),
            "project package skill",
        );
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("prompts")
                .join("project.md"),
            "project prompt",
        );
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("themes")
                .join("project.json"),
            "{}",
        );
        write_file(
            &cwd.join(".agents").join("skills").join("cwd-agent.md"),
            "cwd agent skill",
        );

        let discovered = discover_resources_internal(&cwd, &agent_dir, Some(&fake_home));
        let skill_paths = discovered
            .skills
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let prompt_paths = discovered
            .prompts
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let theme_paths = discovered
            .themes
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();

        assert!(skill_paths.contains(&agent_dir.join("skills").join("global.md")));
        assert!(skill_paths.contains(&agent_dir.join("skills").join("nested").join("SKILL.md")));
        assert!(skill_paths.contains(&fake_home.join(".agents").join("skills").join("home.md")));
        assert!(
            skill_paths.contains(
                &cwd.join(PROJECT_CONFIG_DIR_NAME)
                    .join("skills")
                    .join("project.md")
            )
        );
        assert!(
            skill_paths.contains(
                &cwd.join(PROJECT_CONFIG_DIR_NAME)
                    .join("skills")
                    .join("pkg")
                    .join("SKILL.md")
            )
        );
        assert!(skill_paths.contains(&cwd.join(".agents").join("skills").join("cwd-agent.md")));

        assert_eq!(
            prompt_paths,
            vec![
                agent_dir.join("prompts").join("global.md"),
                cwd.join(PROJECT_CONFIG_DIR_NAME)
                    .join("prompts")
                    .join("project.md")
            ]
        );
        assert_eq!(
            theme_paths,
            vec![
                agent_dir.join("themes").join("global.json"),
                cwd.join(PROJECT_CONFIG_DIR_NAME)
                    .join("themes")
                    .join("project.json")
            ]
        );
    }

    #[test]
    fn discovers_context_files_with_global_then_root_to_leaf_order() {
        let tempdir = tempdir().expect("tempdir");
        let root = tempdir.path().join("workspace");
        let cwd = root.join("a").join("b").join("c");
        let agent_dir = tempdir.path().join("agent");

        write_file(&agent_dir.join("AGENTS.md"), "global");
        write_file(&root.join("AGENTS.md"), "root");
        write_file(&root.join("a").join("CLAUDE.md"), "a");
        write_file(&cwd.join("AGENTS.md"), "cwd");

        let paths = discover_agents_context_paths_internal(&cwd, &agent_dir);
        assert_eq!(
            paths,
            vec![
                agent_dir.join("AGENTS.md"),
                root.join("AGENTS.md"),
                root.join("a").join("CLAUDE.md"),
                cwd.join("AGENTS.md"),
            ]
        );
    }

    #[test]
    fn project_system_prompt_takes_precedence_over_global() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");

        let global_system = agent_dir.join("SYSTEM.md");
        let project_system = cwd.join(PROJECT_CONFIG_DIR_NAME).join("SYSTEM.md");
        write_file(&global_system, "global");
        write_file(&project_system, "project");

        assert_eq!(
            discover_system_prompt_path(&cwd, Some(agent_dir.clone())),
            Some(project_system.clone())
        );
        fs::remove_file(&project_system).expect("remove project system");
        assert_eq!(
            discover_system_prompt_path(&cwd, Some(agent_dir)),
            Some(global_system)
        );
    }

    #[test]
    fn loads_discovered_resources_and_emits_diagnostics_for_missing_files() {
        let tempdir = tempdir().expect("tempdir");
        let existing = tempdir.path().join("skills").join("ok.md");
        write_file(&existing, "ok");
        let missing = tempdir.path().join("skills").join("missing.md");

        let discovered = DiscoveredResources {
            skills: vec![
                ScopedPath {
                    scope: ResourceScope::Global,
                    path: existing.clone(),
                },
                ScopedPath {
                    scope: ResourceScope::Project,
                    path: missing.clone(),
                },
            ],
            prompts: Vec::new(),
            themes: Vec::new(),
        };

        let loaded = load_discovered_resources(&discovered);
        assert_eq!(loaded.skills.len(), 1);
        assert_eq!(loaded.skills[0].path, existing);
        assert_eq!(loaded.skills[0].content, "ok");
        assert_eq!(loaded.diagnostics.len(), 1);
        assert_eq!(loaded.diagnostics[0].path, missing);
    }

    #[test]
    fn discovery_options_include_settings_package_and_explicit_paths() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let package_root = tempdir.path().join("package");
        let explicit_prompt = tempdir.path().join("explicit").join("prompt.md");
        write_file(
            &cwd.join(PROJECT_CONFIG_DIR_NAME)
                .join("skills")
                .join("project.md"),
            "project",
        );
        write_file(&agent_dir.join("skill-extra.md"), "global extra");
        write_file(&package_root.join("skills").join("pkg.md"), "package");
        write_file(&explicit_prompt, "prompt");

        let settings_manager = SettingsManager::create(&cwd, Some(agent_dir.clone()));
        let mut settings_manager = settings_manager;
        settings_manager
            .set_string_list(
                SettingsScope::Global,
                "skills",
                &["skill-extra.md".to_string()],
            )
            .expect("set global skills");

        let discovered = discover_resources_with_options(&ResourceDiscoveryOptions {
            cwd: cwd.clone(),
            agent_dir: Some(agent_dir.clone()),
            settings_manager: Some(settings_manager),
            package_roots: vec![ScopedPath {
                scope: ResourceScope::Project,
                path: package_root.clone(),
            }],
            explicit_skill_paths: Vec::new(),
            explicit_prompt_paths: vec![explicit_prompt.clone()],
            explicit_theme_paths: Vec::new(),
            no_skills: false,
            no_prompt_templates: false,
            no_themes: false,
        });

        let skill_paths = discovered
            .skills
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let prompt_paths = discovered
            .prompts
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();

        assert!(
            skill_paths.contains(
                &cwd.join(PROJECT_CONFIG_DIR_NAME)
                    .join("skills")
                    .join("project.md")
            )
        );
        assert!(skill_paths.contains(&agent_dir.join("skill-extra.md")));
        assert!(skill_paths.contains(&package_root.join("skills").join("pkg.md")));
        assert!(prompt_paths.contains(&explicit_prompt));
    }

    #[test]
    fn package_manifest_filters_and_ignore_files_are_respected() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let package_root = tempdir.path().join("package");

        write_file(
            &package_root.join("package.json"),
            r#"{
                "pi": {
                    "skills": ["skills", "!skills/excluded.md", "+skills/forced.md"],
                    "prompts": ["prompts"],
                    "themes": ["themes"]
                }
            }"#,
        );
        write_file(&package_root.join("skills").join("keep.md"), "keep");
        write_file(&package_root.join("skills").join("excluded.md"), "exclude");
        write_file(&package_root.join("skills").join("forced.md"), "forced");
        write_file(&package_root.join("skills").join("ignored.md"), "ignored");
        write_file(
            &package_root.join("skills").join(".gitignore"),
            "ignored.md\n",
        );
        write_file(&package_root.join("prompts").join("prompt.md"), "prompt");
        write_file(&package_root.join("themes").join("theme.json"), "{}");

        let discovered = discover_resources_with_options(&ResourceDiscoveryOptions {
            cwd: cwd.clone(),
            agent_dir: Some(agent_dir),
            settings_manager: None,
            package_roots: vec![ScopedPath {
                scope: ResourceScope::Project,
                path: package_root.clone(),
            }],
            explicit_skill_paths: Vec::new(),
            explicit_prompt_paths: Vec::new(),
            explicit_theme_paths: Vec::new(),
            no_skills: false,
            no_prompt_templates: false,
            no_themes: false,
        });

        let skill_paths = discovered
            .skills
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let prompt_paths = discovered
            .prompts
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let theme_paths = discovered
            .themes
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();

        assert!(skill_paths.contains(&package_root.join("skills").join("keep.md")));
        assert!(skill_paths.contains(&package_root.join("skills").join("forced.md")));
        assert!(!skill_paths.contains(&package_root.join("skills").join("excluded.md")));
        assert!(!skill_paths.contains(&package_root.join("skills").join("ignored.md")));
        assert_eq!(
            prompt_paths,
            vec![package_root.join("prompts").join("prompt.md")]
        );
        assert_eq!(
            theme_paths,
            vec![package_root.join("themes").join("theme.json")]
        );
    }

    #[test]
    fn empty_manifest_arrays_disable_resource_types() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let package_root = tempdir.path().join("package");

        write_file(
            &package_root.join("package.json"),
            r#"{"pi":{"skills":[],"prompts":[],"themes":[]}}"#,
        );
        write_file(&package_root.join("skills").join("keep.md"), "keep");
        write_file(&package_root.join("prompts").join("prompt.md"), "prompt");
        write_file(&package_root.join("themes").join("theme.json"), "{}");

        let discovered = discover_resources_with_options(&ResourceDiscoveryOptions {
            cwd,
            agent_dir: Some(agent_dir),
            settings_manager: None,
            package_roots: vec![ScopedPath {
                scope: ResourceScope::Project,
                path: package_root,
            }],
            explicit_skill_paths: Vec::new(),
            explicit_prompt_paths: Vec::new(),
            explicit_theme_paths: Vec::new(),
            no_skills: false,
            no_prompt_templates: false,
            no_themes: false,
        });

        assert!(discovered.skills.is_empty());
        assert!(discovered.prompts.is_empty());
        assert!(discovered.themes.is_empty());
    }

    #[test]
    fn project_scope_replaces_same_path_user_discovery() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");
        let shared_root = tempdir.path().join("shared");

        write_file(&shared_root.join("skills").join("shared.md"), "shared");

        let discovered = discover_resources_with_options(&ResourceDiscoveryOptions {
            cwd,
            agent_dir: Some(agent_dir),
            settings_manager: None,
            package_roots: vec![
                ScopedPath {
                    scope: ResourceScope::Global,
                    path: shared_root.clone(),
                },
                ScopedPath {
                    scope: ResourceScope::Project,
                    path: shared_root,
                },
            ],
            explicit_skill_paths: Vec::new(),
            explicit_prompt_paths: Vec::new(),
            explicit_theme_paths: Vec::new(),
            no_skills: false,
            no_prompt_templates: false,
            no_themes: false,
        });

        assert_eq!(discovered.skills.len(), 1);
        assert_eq!(discovered.skills[0].scope, ResourceScope::Project);
    }

    #[test]
    fn append_system_prompt_prefers_project_then_global() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let agent_dir = tempdir.path().join("agent");

        let global_append = agent_dir.join("APPEND_SYSTEM.md");
        let project_append = cwd.join(PROJECT_CONFIG_DIR_NAME).join("APPEND_SYSTEM.md");
        write_file(&global_append, "global");
        write_file(&project_append, "project");

        assert_eq!(
            discover_append_system_prompt_path(&cwd, Some(agent_dir.clone())),
            Some(project_append.clone())
        );
        fs::remove_file(&project_append).expect("remove project append");
        assert_eq!(
            discover_append_system_prompt_path(&cwd, Some(agent_dir)),
            Some(global_append)
        );
    }
}
