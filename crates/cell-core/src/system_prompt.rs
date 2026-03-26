use std::path::Path;

use cell_resources::{ContextDocument, SkillDefinition, format_skills_for_prompt};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub struct BuildSystemPromptOptions<'a> {
    pub custom_prompt: Option<&'a str>,
    pub selected_tools: &'a [String],
    pub append_system_prompt: Option<&'a str>,
    pub cwd: &'a Path,
    pub context_files: &'a [ContextDocument],
    pub skills: &'a [SkillDefinition],
}

pub fn build_system_prompt(options: BuildSystemPromptOptions<'_>) -> String {
    let append_section = options
        .append_system_prompt
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("\n\n{value}"))
        .unwrap_or_default();
    let date_time = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());

    let mut prompt = if let Some(custom_prompt) = options
        .custom_prompt
        .filter(|value| !value.trim().is_empty())
    {
        custom_prompt.to_string()
    } else {
        let tools = options
            .selected_tools
            .iter()
            .filter_map(|name| {
                tool_description(name).map(|description| format!("- {name}: {description}"))
            })
            .collect::<Vec<_>>();
        let guidelines = build_guidelines(options.selected_tools)
            .into_iter()
            .map(|guideline| format!("- {guideline}"))
            .collect::<Vec<_>>();
        format!(
            "You are an expert coding assistant operating inside cell, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.\n\nAvailable tools:\n{}\n\nGuidelines:\n{}",
            if tools.is_empty() {
                "(none)".to_string()
            } else {
                tools.join("\n")
            },
            guidelines.join("\n")
        )
    };

    if !append_section.is_empty() {
        prompt.push_str(&append_section);
    }

    if !options.context_files.is_empty() {
        prompt
            .push_str("\n\n# Project Context\n\nProject-specific instructions and guidelines:\n\n");
        for document in options.context_files {
            prompt.push_str(&format!(
                "## {}\n\n{}\n\n",
                document.path.to_string_lossy(),
                document.content
            ));
        }
    }

    if !options.skills.is_empty() {
        prompt.push_str(&format_skills_for_prompt(options.skills));
    }

    prompt.push_str(&format!(
        "\nCurrent date and time: {date_time}\nCurrent working directory: {}",
        options.cwd.to_string_lossy()
    ));
    prompt
}

fn tool_description(name: &str) -> Option<&'static str> {
    match name {
        "read" => Some("Read file contents"),
        "bash" => Some("Execute bash commands (ls, grep, find, etc.)"),
        "edit" => Some("Make surgical edits to files (find exact text and replace)"),
        "write" => Some("Create or overwrite files"),
        "grep" => Some("Search file contents for patterns (respects ignore files)"),
        "find" => Some("Find files by glob pattern (respects ignore files)"),
        "ls" => Some("List directory contents"),
        _ => None,
    }
}

fn build_guidelines(selected_tools: &[String]) -> Vec<&'static str> {
    let has = |name| selected_tools.iter().any(|tool| tool == name);
    let mut guidelines = Vec::new();

    if has("bash") && !(has("grep") || has("find") || has("ls")) {
        guidelines.push("Use bash for file operations like ls, rg, and find");
    } else if has("bash") && (has("grep") || has("find") || has("ls")) {
        guidelines.push("Prefer grep/find/ls over bash for file exploration");
    }
    if has("read") && has("edit") {
        guidelines.push("Use read to examine files before editing");
    }
    if has("edit") {
        guidelines.push("Use edit for precise changes (old text must match exactly)");
    }
    if has("write") {
        guidelines.push("Use write only for new files or complete rewrites");
    }
    if has("edit") || has("write") {
        guidelines.push("When summarizing actions, output plain text directly");
    }
    guidelines.push("Be concise in your responses");
    guidelines.push("Show file paths clearly when working with files");
    guidelines
}
