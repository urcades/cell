use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PromptAutocompleteKind {
    SlashCommand,
    ModelArgument,
    Path,
    FileReference,
}

pub(super) struct PromptAutocompleteState {
    pub(super) kind: PromptAutocompleteKind,
    pub(super) title: String,
    pub(super) subtitle: String,
    pub(super) hint: String,
    pub(super) replace_prefix: String,
    pub(super) list: SelectList,
}

pub(super) fn split_prompt_lines(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized
        .split('\n')
        .map(str::to_string)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(super) fn build_prompt_autocomplete<F>(
    text: &str,
    cursor: (usize, usize),
    session_commands: &[RpcSlashCommand],
    load_models: F,
    cwd: &Path,
    force_file_completion: bool,
) -> Result<Option<PromptAutocompleteState>, String>
where
    F: FnOnce() -> Result<Vec<Model>, String>,
{
    let lines = split_prompt_lines(text);
    let (cursor_line, cursor_col) = cursor;
    if cursor_line >= lines.len() {
        return Ok(None);
    }
    let current_line = &lines[cursor_line];
    if cursor_col > current_line.len() {
        return Ok(None);
    }

    let before_cursor = &current_line[..cursor_col];
    let trimmed_before = before_cursor.trim_start();

    if trimmed_before.starts_with('/') && cursor_line > 0 && !force_file_completion {
        return Ok(None);
    }

    if cursor_line == 0 && trimmed_before.starts_with('/') {
        if let Some(model_filter) = trimmed_before.strip_prefix("/model ") {
            let models = load_models()?;
            if models.is_empty() {
                return Ok(None);
            }
            let (items, _) = build_model_overlay_items(&models, None);
            let mut list = SelectList::new(items, 8);
            if !model_filter.trim().is_empty() {
                list.set_filter(model_filter.trim());
            }
            return Ok(Some(PromptAutocompleteState {
                kind: PromptAutocompleteKind::ModelArgument,
                title: "Models".to_string(),
                subtitle: "Complete /model with an available model".to_string(),
                hint: "Tab inserts · Enter inserts and runs · Esc dismisses".to_string(),
                replace_prefix: model_filter.to_string(),
                list,
            }));
        }

        if !trimmed_before[1..].contains(' ') {
            let filter = &trimmed_before[1..];
            let mut list = SelectList::new(build_prompt_command_items(session_commands), 8);
            if !filter.is_empty() {
                list.set_filter(filter);
            }
            return Ok(Some(PromptAutocompleteState {
                kind: PromptAutocompleteKind::SlashCommand,
                title: "Commands".to_string(),
                subtitle: "Type a slash command and press Enter to run it".to_string(),
                hint: "Tab inserts · Enter runs · Esc dismisses".to_string(),
                replace_prefix: trimmed_before.to_string(),
                list,
            }));
        }
    }

    if let Some(prefix) = extract_at_prefix(before_cursor) {
        let items = build_fuzzy_file_reference_items(cwd, &prefix);
        if !items.is_empty() {
            return Ok(Some(PromptAutocompleteState {
                kind: PromptAutocompleteKind::FileReference,
                title: "Files".to_string(),
                subtitle: "Attach a file reference from the workspace".to_string(),
                hint: "Tab inserts · Enter inserts and runs · Esc dismisses".to_string(),
                replace_prefix: prefix,
                list: SelectList::new(items, 8),
            }));
        }
    }

    let path_prefix = extract_path_prefix(before_cursor, force_file_completion);
    if let Some(prefix) = path_prefix {
        let items = build_path_completion_items(cwd, &prefix);
        if !items.is_empty() {
            return Ok(Some(PromptAutocompleteState {
                kind: PromptAutocompleteKind::Path,
                title: "Paths".to_string(),
                subtitle: "Complete a path from the current workspace".to_string(),
                hint: "Tab inserts · Esc dismisses".to_string(),
                replace_prefix: prefix,
                list: SelectList::new(items, 8),
            }));
        }
    }

    Ok(None)
}

pub(super) fn prompt_autocomplete_should_submit_current_prompt(
    autocomplete: Option<&PromptAutocompleteState>,
    text: &str,
    cursor: (usize, usize),
) -> bool {
    let Some(autocomplete) = autocomplete else {
        return false;
    };
    let lines = split_prompt_lines(text);
    let (cursor_line, _) = cursor;
    let Some(current_line) = lines.get(cursor_line) else {
        return false;
    };
    let trimmed = current_line.trim();
    if trimmed.is_empty() {
        return false;
    }
    match autocomplete.kind {
        PromptAutocompleteKind::SlashCommand => autocomplete.list.contains_value(trimmed),
        PromptAutocompleteKind::ModelArgument => trimmed
            .strip_prefix("/model ")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|value| autocomplete.list.contains_value(value)),
        PromptAutocompleteKind::Path | PromptAutocompleteKind::FileReference => false,
    }
}
#[derive(Clone, Copy, Debug)]
struct BuiltinSlashCommand {
    name: &'static str,
    description: &'static str,
}

const BUILTIN_SLASH_COMMANDS: &[BuiltinSlashCommand] = &[
    BuiltinSlashCommand {
        name: "settings",
        description: "Open settings menu",
    },
    BuiltinSlashCommand {
        name: "model",
        description: "Select model (opens selector UI)",
    },
    BuiltinSlashCommand {
        name: "scoped-models",
        description: "Configure which models Ctrl+P cycles through",
    },
    BuiltinSlashCommand {
        name: "export",
        description: "Export session to HTML file",
    },
    BuiltinSlashCommand {
        name: "share",
        description: "Share session as a secret GitHub gist",
    },
    BuiltinSlashCommand {
        name: "copy",
        description: "Copy last agent message to clipboard",
    },
    BuiltinSlashCommand {
        name: "name",
        description: "Set session display name",
    },
    BuiltinSlashCommand {
        name: "session",
        description: "Show session info and stats",
    },
    BuiltinSlashCommand {
        name: "changelog",
        description: "Show changelog entries",
    },
    BuiltinSlashCommand {
        name: "hotkeys",
        description: "Show all keyboard shortcuts",
    },
    BuiltinSlashCommand {
        name: "fork",
        description: "Create a new fork from a previous message",
    },
    BuiltinSlashCommand {
        name: "tree",
        description: "Navigate session tree (switch branches)",
    },
    BuiltinSlashCommand {
        name: "login",
        description: "Login with OAuth provider",
    },
    BuiltinSlashCommand {
        name: "logout",
        description: "Logout from OAuth provider",
    },
    BuiltinSlashCommand {
        name: "new",
        description: "Start a new session",
    },
    BuiltinSlashCommand {
        name: "compact",
        description: "Manually compact the session context",
    },
    BuiltinSlashCommand {
        name: "resume",
        description: "Resume a different session",
    },
    BuiltinSlashCommand {
        name: "reload",
        description: "Reload settings, prompts, skills, and themes from disk",
    },
    BuiltinSlashCommand {
        name: "quit",
        description: "Quit cell",
    },
];

const PATH_DELIMITERS: &[char] = &[' ', '\t', '"', '\'', '='];

fn build_prompt_command_items(session_commands: &[RpcSlashCommand]) -> Vec<SelectItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for command in BUILTIN_SLASH_COMMANDS {
        let value = format!("/{}", command.name);
        if seen.insert(value.clone()) {
            items.push(SelectItem {
                value: value.clone(),
                label: value,
                description: Some(command.description.to_string()),
            });
        }
    }
    for command in session_commands {
        let value = format!("/{}", command.name);
        if seen.insert(value.clone()) {
            items.push(SelectItem {
                value: value.clone(),
                label: value,
                description: Some(format_dynamic_command_description(command)),
            });
        }
    }
    items
}

fn format_dynamic_command_description(command: &RpcSlashCommand) -> String {
    let mut segments = vec![match command.source {
        RpcCommandSource::Prompt => "Prompt template".to_string(),
        RpcCommandSource::Skill => "Skill".to_string(),
        RpcCommandSource::Extension => "Extension".to_string(),
    }];
    if let Some(description) = command
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        segments.push(description.to_string());
    }
    if let Some(location) = command.location {
        segments.push(match location {
            RpcCommandLocation::User => "(user)".to_string(),
            RpcCommandLocation::Project => "(project)".to_string(),
            RpcCommandLocation::Path => "(path)".to_string(),
        });
    }
    segments.join(" · ")
}

fn find_last_delimiter(text: &str) -> Option<usize> {
    text.char_indices()
        .rev()
        .find_map(|(index, character)| PATH_DELIMITERS.contains(&character).then_some(index))
}

fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut quote_start = None;
    for (index, character) in text.char_indices() {
        if character == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = Some(index);
            }
        }
    }
    if in_quotes { quote_start } else { None }
}

fn is_token_start(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    text[..index]
        .chars()
        .next_back()
        .is_some_and(|character| PATH_DELIMITERS.contains(&character))
}

fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;
    if quote_start > 0 && text[..quote_start].ends_with('@') {
        let at_index = quote_start.saturating_sub(1);
        if !is_token_start(text, at_index) {
            return None;
        }
        return Some(text[at_index..].to_string());
    }
    if !is_token_start(text, quote_start) {
        return None;
    }
    Some(text[quote_start..].to_string())
}

#[derive(Clone, Debug)]
struct ParsedPathPrefix {
    raw_prefix: String,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    if let Some(value) = prefix.strip_prefix("@\"") {
        return ParsedPathPrefix {
            raw_prefix: value.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(value) = prefix.strip_prefix('"') {
        return ParsedPathPrefix {
            raw_prefix: value.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(value) = prefix.strip_prefix('@') {
        return ParsedPathPrefix {
            raw_prefix: value.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    ParsedPathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

fn build_completion_value(path: &str, is_at_prefix: bool, is_quoted_prefix: bool) -> String {
    let needs_quotes = is_quoted_prefix || path.contains(' ');
    let prefix = if is_at_prefix { "@" } else { "" };
    if !needs_quotes {
        return format!("{prefix}{path}");
    }
    format!(r#"{prefix}"{path}""#)
}

fn extract_at_prefix(text: &str) -> Option<String> {
    if let Some(prefix) = extract_quoted_prefix(text)
        && prefix.starts_with("@\"")
    {
        return Some(prefix);
    }
    let token_start = find_last_delimiter(text).map_or(0, |index| index + 1);
    text[token_start..]
        .starts_with('@')
        .then_some(text[token_start..].to_string())
}

fn extract_path_prefix(text: &str, force_extract: bool) -> Option<String> {
    if let Some(prefix) = extract_quoted_prefix(text) {
        return Some(prefix);
    }
    let token_start = find_last_delimiter(text).map_or(0, |index| index + 1);
    let path_prefix = text[token_start..].to_string();
    if force_extract {
        let trimmed = text.trim_start();
        if trimmed.starts_with('/') && !trimmed.contains(' ') {
            return None;
        }
        return Some(path_prefix);
    }
    if path_prefix.contains('/') || path_prefix.starts_with('.') || path_prefix.starts_with("~/") {
        return Some(path_prefix);
    }
    None
}

fn expand_home_path(path: &str) -> String {
    if path == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| path.to_string());
    }
    if let Some(value) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Path::new(&home).join(value).to_string_lossy().to_string();
    }
    path.to_string()
}

fn keep_completion_walkdir_entry(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !name.starts_with('.') && name != "node_modules" && name != ".git"
}

fn build_path_completion_items(cwd: &Path, prefix: &str) -> Vec<SelectItem> {
    let parsed = parse_path_prefix(prefix);
    let mut expanded_prefix = parsed.raw_prefix.clone();
    if expanded_prefix.starts_with('~') {
        expanded_prefix = expand_home_path(&expanded_prefix);
    }

    let is_root_prefix = parsed.raw_prefix.is_empty()
        || matches!(parsed.raw_prefix.as_str(), "./" | "../" | "~" | "~/" | "/");
    let (search_dir, display_prefix, search_prefix) = if is_root_prefix {
        let search_dir = if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
            PathBuf::from(&expanded_prefix)
        } else {
            cwd.join(&expanded_prefix)
        };
        (search_dir, parsed.raw_prefix.clone(), String::new())
    } else if parsed.raw_prefix.ends_with('/') {
        let search_dir = if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
            PathBuf::from(&expanded_prefix)
        } else {
            cwd.join(&expanded_prefix)
        };
        (search_dir, parsed.raw_prefix.clone(), String::new())
    } else {
        let expanded_path = Path::new(&expanded_prefix);
        let search_dir = if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
            expanded_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        } else {
            cwd.join(expanded_path.parent().unwrap_or_else(|| Path::new(".")))
        };
        let display_prefix = Path::new(&parsed.raw_prefix)
            .parent()
            .map(|path| {
                let value = path.to_string_lossy().to_string();
                if value == "." { String::new() } else { value }
            })
            .unwrap_or_default();
        let search_prefix = Path::new(&parsed.raw_prefix)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();
        (search_dir, display_prefix, search_prefix)
    };

    let Ok(entries) = fs::read_dir(&search_dir) else {
        return Vec::new();
    };

    let mut suggestions = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" || name == ".git" {
            continue;
        }
        if !name
            .to_lowercase()
            .starts_with(&search_prefix.to_lowercase())
        {
            continue;
        }
        let file_type = entry.file_type().ok();
        let is_directory = file_type.as_ref().is_some_and(|value| value.is_dir());
        let relative_path = if display_prefix.is_empty() {
            name.clone()
        } else if display_prefix.ends_with('/') {
            format!("{display_prefix}{name}")
        } else {
            format!("{display_prefix}/{name}")
        };
        let path_value = if is_directory {
            format!("{relative_path}/")
        } else {
            relative_path
        };
        suggestions.push(SelectItem {
            value: build_completion_value(
                &path_value,
                parsed.is_at_prefix,
                parsed.is_quoted_prefix,
            ),
            label: format!("{name}{}", if is_directory { "/" } else { "" }),
            description: None,
        });
    }
    suggestions.sort_by(|left, right| {
        let left_is_dir = left.label.ends_with('/');
        let right_is_dir = right.label.ends_with('/');
        right_is_dir
            .cmp(&left_is_dir)
            .then(left.label.to_lowercase().cmp(&right.label.to_lowercase()))
    });
    suggestions
}

fn build_fuzzy_file_reference_items(cwd: &Path, prefix: &str) -> Vec<SelectItem> {
    let parsed = parse_path_prefix(prefix);
    let scoped = resolve_scoped_fuzzy_query(cwd, &parsed.raw_prefix);
    let (base_dir, display_base, query) = match scoped {
        Some((base_dir, display_base, query)) => (base_dir, display_base, query),
        None => (cwd.to_path_buf(), String::new(), parsed.raw_prefix.clone()),
    };
    if !base_dir.exists() {
        return Vec::new();
    }

    let mut scored = Vec::new();
    for entry in WalkDir::new(&base_dir)
        .follow_links(true)
        .into_iter()
        .filter_entry(keep_completion_walkdir_entry)
        .filter_map(Result::ok)
        .take(2000)
    {
        if entry.path() == base_dir {
            continue;
        }
        let is_directory = entry.file_type().is_dir();
        let relative = entry
            .path()
            .strip_prefix(&base_dir)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| entry.path().to_string_lossy().replace('\\', "/"));
        let display_path = if display_base.is_empty() {
            relative
        } else if display_base.ends_with('/') {
            format!("{display_base}{relative}")
        } else {
            format!("{display_base}/{relative}")
        };
        let score = fuzzy_file_score(&display_path, &query, is_directory);
        if score <= 0 {
            continue;
        }
        scored.push((score, display_path, is_directory));
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    scored.truncate(20);
    scored
        .into_iter()
        .map(|(_, display_path, is_directory)| {
            let entry_name = Path::new(&display_path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(display_path.as_str())
                .to_string();
            let path_value = if is_directory {
                format!("{display_path}/")
            } else {
                display_path.clone()
            };
            SelectItem {
                value: build_completion_value(&path_value, true, parsed.is_quoted_prefix),
                label: format!("{entry_name}{}", if is_directory { "/" } else { "" }),
                description: Some(display_path),
            }
        })
        .collect()
}

fn resolve_scoped_fuzzy_query(cwd: &Path, raw_query: &str) -> Option<(PathBuf, String, String)> {
    let slash_index = raw_query.rfind('/')?;
    let display_base = raw_query[..=slash_index].to_string();
    let query = raw_query[slash_index + 1..].to_string();
    let base_dir = if display_base.starts_with("~/") {
        PathBuf::from(expand_home_path(&display_base))
    } else if display_base.starts_with('/') {
        PathBuf::from(&display_base)
    } else {
        cwd.join(&display_base)
    };
    base_dir.is_dir().then_some((base_dir, display_base, query))
}

fn fuzzy_file_score(path: &str, query: &str, is_directory: bool) -> i32 {
    if query.is_empty() {
        return if is_directory { 11 } else { 1 };
    }
    let filename = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(path)
        .to_lowercase();
    let lower_query = query.to_lowercase();
    let lower_path = path.to_lowercase();
    let mut score = if filename == lower_query {
        100
    } else if filename.starts_with(&lower_query) {
        80
    } else if filename.contains(&lower_query) {
        50
    } else if lower_path.contains(&lower_query) {
        30
    } else {
        0
    };
    if is_directory && score > 0 {
        score += 10;
    }
    score
}
