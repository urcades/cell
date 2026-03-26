use super::super::*;
use super::base::{OverlaySelection, SearchOverlay, select_list_visible_bounds};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionScope {
    Current,
    All,
}

impl SessionScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::All => "all",
        }
    }

    pub(crate) fn toggle(self) -> Self {
        match self {
            Self::Current => Self::All,
            Self::All => Self::Current,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionSortMode {
    Threaded,
    Recent,
    Relevance,
}

impl SessionSortMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Threaded => "threaded",
            Self::Recent => "recent",
            Self::Relevance => "relevance",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Threaded => Self::Recent,
            Self::Recent => Self::Relevance,
            Self::Relevance => Self::Threaded,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionNameFilter {
    All,
    Named,
}

impl SessionNameFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Named => "named",
        }
    }

    pub(crate) fn toggle(self) -> Self {
        match self {
            Self::All => Self::Named,
            Self::Named => Self::All,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SessionRecord {
    pub(crate) path: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) name: Option<String>,
    pub(crate) preview: String,
    pub(crate) message_count: usize,
    pub(crate) modified_epoch_ms: i64,
    pub(crate) parent_session: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionOverlayRow {
    pub(crate) record: SessionRecord,
    pub(crate) depth: usize,
    pub(crate) is_last: bool,
    pub(crate) ancestor_continues: Vec<bool>,
}

pub(crate) struct SessionOverlayState {
    pub(crate) overlay: SearchOverlay,
    pub(crate) selections: Vec<OverlaySelection>,
    pub(crate) records: Vec<SessionRecord>,
    pub(crate) rows: Vec<SessionOverlayRow>,
    pub(crate) current_session_file: Option<PathBuf>,
    pub(crate) standalone: bool,
    pub(crate) scope: SessionScope,
    pub(crate) sort_mode: SessionSortMode,
    pub(crate) name_filter: SessionNameFilter,
    pub(crate) show_path: bool,
    pub(crate) confirming_delete: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionSearchMode {
    Tokens,
    Regex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionSearchTokenKind {
    Fuzzy,
    Phrase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionSearchToken {
    pub(crate) kind: SessionSearchTokenKind,
    pub(crate) value: String,
}

#[derive(Debug)]
pub(crate) struct ParsedSessionSearchQuery {
    pub(crate) mode: SessionSearchMode,
    pub(crate) tokens: Vec<SessionSearchToken>,
    pub(crate) regex: Option<Regex>,
    pub(crate) error: Option<String>,
}

impl Component for SessionOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput {
            lines: Vec::new(),
            cursor: None,
        };
        append_rule_line(&mut output.lines, width);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(session_overlay_header_line(self, width as usize)).render(width),
            false,
        );
        append_output(
            &mut output,
            Text::new(session_overlay_hint_line_one(self, width as usize)).render(width),
            false,
        );
        let hint_line_two = session_overlay_hint_line_two(self, width as usize);
        if !hint_line_two.is_empty() {
            append_output(&mut output, Text::new(hint_line_two).render(width), false);
        }
        append_blank_lines(&mut output, width, 1);
        append_output(&mut output, self.overlay.search.render(width), false);
        append_blank_lines(&mut output, width, 1);
        append_session_overlay_rows(&mut output.lines, self, width as usize);
        append_blank_lines(&mut output, width, 1);
        append_rule_line(&mut output.lines, width);
        output
    }
}

pub(crate) fn discover_session_records(root: &Path) -> Result<Vec<SessionRecord>, String> {
    let mut records = Vec::new();
    for path in discover_session_paths(root) {
        records.push(load_session_record(&path)?);
    }
    Ok(records)
}

pub(crate) fn load_session_record(path: &Path) -> Result<SessionRecord, String> {
    let content = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let entries = parse_session_entries(&content);
    let header = entries
        .iter()
        .find_map(|entry| parse_header(entry).ok().flatten())
        .ok_or_else(|| format!("Missing session header in {}", path.to_string_lossy()))?;

    let mut name = None;
    let mut preview = None;
    let mut message_count = 0usize;
    for entry in &entries {
        let parsed = serde_json::from_value::<SessionEntry>(entry.clone()).ok();
        if matches!(parsed, Some(SessionEntry::Message(_))) {
            message_count += 1;
            if preview.is_none() {
                preview = entry
                    .get("message")
                    .map(extract_message_preview)
                    .filter(|value| !value.trim().is_empty());
            }
        }
        if let Some(SessionEntry::SessionInfo(info)) = parsed {
            if let Some(next_name) = info.name.filter(|value| !value.trim().is_empty()) {
                name = Some(next_name);
            }
        }
    }

    Ok(SessionRecord {
        path: path.to_path_buf(),
        cwd: PathBuf::from(header.cwd),
        name,
        preview: preview.unwrap_or_else(|| "Empty session".to_string()),
        message_count,
        modified_epoch_ms: path_modified_epoch_ms(path),
        parent_session: header.parent_session,
    })
}

pub(crate) fn path_modified_epoch_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(0))
        .unwrap_or(0)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_session_overlay_items(
    records: Vec<SessionRecord>,
    query: Option<&str>,
    sort_mode: SessionSortMode,
    name_filter: SessionNameFilter,
    _show_path: bool,
    _current_session_file: Option<&str>,
) -> (Vec<SelectItem>, Vec<OverlaySelection>) {
    let rows = build_session_overlay_rows(records, query, sort_mode, name_filter);
    session_overlay_rows_to_items(&rows)
}

pub(crate) fn sort_session_records(
    mut records: Vec<SessionRecord>,
    sort_mode: SessionSortMode,
    query: Option<&ParsedSessionSearchQuery>,
) -> Vec<SessionRecord> {
    match sort_mode {
        SessionSortMode::Threaded | SessionSortMode::Recent => {
            records.sort_by(|left, right| right.modified_epoch_ms.cmp(&left.modified_epoch_ms));
        }
        SessionSortMode::Relevance => {
            if let Some(parsed) = query {
                records.sort_by(|left, right| {
                    session_record_match_score(left, parsed)
                        .unwrap_or(i64::MAX)
                        .cmp(&session_record_match_score(right, parsed).unwrap_or(i64::MAX))
                        .then(right.modified_epoch_ms.cmp(&left.modified_epoch_ms))
                });
            } else {
                records.sort_by(|left, right| right.modified_epoch_ms.cmp(&left.modified_epoch_ms));
            }
        }
    }
    records
}

pub(crate) fn build_session_overlay_rows(
    mut records: Vec<SessionRecord>,
    query: Option<&str>,
    sort_mode: SessionSortMode,
    name_filter: SessionNameFilter,
) -> Vec<SessionOverlayRow> {
    if matches!(name_filter, SessionNameFilter::Named) {
        records.retain(|record| record.name.is_some());
    }

    let query = query.map(str::trim).filter(|value| !value.is_empty());
    let parsed_query = query.map(parse_session_search_query);
    if parsed_query
        .as_ref()
        .and_then(|parsed| parsed.error.as_ref())
        .is_some()
    {
        return Vec::new();
    }

    if let Some(parsed) = parsed_query.as_ref() {
        records.retain(|record| session_record_match_score(record, parsed).is_some());
    }

    if matches!(sort_mode, SessionSortMode::Threaded) && query.is_none() {
        flatten_session_record_tree(records)
    } else {
        sort_session_records(records, sort_mode, parsed_query.as_ref())
            .into_iter()
            .map(|record| SessionOverlayRow {
                record,
                depth: 0,
                is_last: true,
                ancestor_continues: Vec::new(),
            })
            .collect()
    }
}

pub(crate) fn session_overlay_rows_to_items(
    rows: &[SessionOverlayRow],
) -> (Vec<SelectItem>, Vec<OverlaySelection>) {
    let mut items = Vec::new();
    let mut selections = Vec::new();
    for row in rows {
        let primary = row
            .record
            .name
            .as_deref()
            .unwrap_or(row.record.preview.as_str())
            .replace('\n', " ");
        let description = format!(
            "{} msg · {}",
            row.record.message_count,
            format_relative_age(row.record.modified_epoch_ms)
        );
        items.push(SelectItem {
            value: row.record.path.to_string_lossy().to_string(),
            label: truncate_to_width(&primary, 96),
            description: Some(description),
        });
        selections.push(OverlaySelection::Session {
            path: row.record.path.clone(),
        });
    }
    (items, selections)
}

pub(crate) fn session_overlay_header_line(state: &SessionOverlayState, width: usize) -> String {
    let title = style_title(match state.scope {
        SessionScope::Current => "Resume Session (Current Folder)",
        SessionScope::All => "Resume Session (All)",
    });
    let scope_text = match state.scope {
        SessionScope::Current => {
            format!(
                "{}{}",
                style_brand("◉ Current Folder"),
                style_hint(" | ○ All")
            )
        }
        SessionScope::All => format!(
            "{}{}",
            style_hint("○ Current Folder | "),
            style_brand("◉ All")
        ),
    };
    let name_text = format!(
        "{}{}",
        style_hint("Name: "),
        style_brand(if matches!(state.name_filter, SessionNameFilter::All) {
            "All"
        } else {
            "Named"
        })
    );
    let sort_text = format!(
        "{}{}",
        style_hint("Sort: "),
        style_brand(match state.sort_mode {
            SessionSortMode::Threaded => "Threaded",
            SessionSortMode::Recent => "Recent",
            SessionSortMode::Relevance => "Fuzzy",
        })
    );
    align_footer_row(
        &title,
        &format!("{scope_text}  {name_text}  {sort_text}"),
        width,
    )
}

pub(crate) fn session_overlay_hint_line_one(state: &SessionOverlayState, width: usize) -> String {
    if state.confirming_delete.is_some() {
        return truncate_to_width(
            &style_error("Delete session? [Enter] confirm · [Esc/Ctrl+C] cancel"),
            width,
        );
    }
    let first = state.overlay.hint.lines().next().unwrap_or_default();
    truncate_to_width(&style_hint(first), width)
}

pub(crate) fn session_overlay_hint_line_two(state: &SessionOverlayState, width: usize) -> String {
    if state.confirming_delete.is_some() {
        return String::new();
    }
    let second = state.overlay.hint.lines().nth(1).unwrap_or_default();
    truncate_to_width(&style_hint(second), width)
}

pub(crate) fn update_session_overlay_metadata_with_options(
    state: &mut SessionOverlayState,
    keybindings: &KeybindingsManager,
    current_session_file: Option<&Path>,
    allow_rename: bool,
) {
    state.overlay.set_title(match state.scope {
        SessionScope::Current => "Resume Session (Current Folder)",
        SessionScope::All => "Resume Session (All)",
    });
    let scope_line = match state.scope {
        SessionScope::Current => "◉ current folder | ○ all",
        SessionScope::All => "○ current folder | ◉ all",
    };
    let selected = state
        .overlay
        .selected_item()
        .map(|item| truncate_to_width(&item.label.replace('\n', " "), 48));
    let detail = state
        .overlay
        .selected_value()
        .and_then(|value| {
            state
                .records
                .iter()
                .find(|record| record.path == PathBuf::from(value))
        })
        .map(|record| {
            session_selection_detail(
                record,
                current_session_file.is_some_and(|path| path == record.path.as_path()),
            )
        });
    let count = state.selections.len();
    state.overlay.set_subtitle(format!(
        "{} · sort {} · names {} · path ({})\n{} sessions{}",
        scope_line,
        state.sort_mode.label(),
        state.name_filter.label(),
        if state.show_path { "on" } else { "off" },
        count,
        selected
            .as_ref()
            .map(|value| format!(" · {value}"))
            .unwrap_or_default()
    ));
    state.overlay.set_detail(detail);
    if state.confirming_delete.is_some() {
        state
            .overlay
            .set_hint("Delete session? Enter confirms - Esc cancels");
    } else {
        state.overlay.set_subtitle(format!(
            "{} · sort {} · names {} · path ({})\n{} sessions{}",
            scope_line,
            state.sort_mode.label(),
            state.name_filter.label(),
            if state.show_path { "on" } else { "off" },
            count,
            selected
                .as_ref()
                .map(|value| format!(" · {value}"))
                .unwrap_or_default()
        ));
        state.overlay.set_hint(format!(
            "{} scope · re:<pattern> regex · \"phrase\" exact\n{} sort · {} named · {} delete · {} path ({}){}",
            keybindings.display(AppAction::ToggleSessionScope),
            keybindings.display(AppAction::ToggleSessionSort),
            keybindings.display(AppAction::ToggleSessionNamedFilter),
            keybindings.display(AppAction::DeleteSession),
            keybindings.display(AppAction::ToggleSessionPath),
            if state.show_path { "on" } else { "off" },
            if allow_rename {
                format!(" · {} rename", keybindings.display(AppAction::RenameSession))
            } else {
                String::new()
            },
        ));
    }
}

pub(crate) fn session_scope_root(
    scope: SessionScope,
    current_session_dir: Option<&Path>,
    cwd: &Path,
    session_dir_override: Option<&Path>,
) -> PathBuf {
    match scope {
        SessionScope::All => session_dir_override
            .map(Path::to_path_buf)
            .unwrap_or_else(get_sessions_dir),
        SessionScope::Current => current_session_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| {
                session_dir_override
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| {
                        get_sessions_dir().join(encode_session_dir_name(&cwd.to_string_lossy()))
                    })
            }),
    }
}

pub(crate) fn append_session_overlay_rows(
    target: &mut Vec<RenderedLine>,
    state: &SessionOverlayState,
    width: usize,
) {
    let filtered_indices = state.overlay.list.filtered_indices();
    if filtered_indices.is_empty() {
        if let Some(error) = parse_session_search_query(state.overlay.search.get_value()).error {
            target.push(RenderedLine::Text(fit_line(
                &style_error(&format!("  Invalid regex: {error}")),
                width as u16,
            )));
            return;
        }
        let message = match (state.scope, state.name_filter) {
            (SessionScope::Current, SessionNameFilter::Named) => {
                "  No named sessions in current folder. Press Tab to view all."
            }
            (SessionScope::All, SessionNameFilter::Named) => "  No named sessions found.",
            (SessionScope::Current, SessionNameFilter::All) => {
                "  No sessions in current folder. Press Tab to view all."
            }
            (SessionScope::All, SessionNameFilter::All) => "  No sessions found",
        };
        target.push(RenderedLine::Text(fit_line(
            &style_hint(message),
            width as u16,
        )));
        return;
    }

    let selected_index = state.overlay.list.selected_index();
    let (start, end) = select_list_visible_bounds(&state.overlay.list);
    for visible_index in start..end {
        let Some(row) = filtered_indices
            .get(visible_index)
            .and_then(|index| state.rows.get(*index))
        else {
            continue;
        };
        let is_selected = visible_index == selected_index;
        let is_current = state.current_session_file.as_ref() == Some(&row.record.path);
        let is_confirming_delete = state.confirming_delete.as_ref() == Some(&row.record.path);
        let has_name = row.record.name.is_some();
        let display = row
            .record
            .name
            .as_deref()
            .unwrap_or(row.record.preview.as_str())
            .replace('\n', " ")
            .trim()
            .to_string();
        let cursor = if is_selected {
            style_brand("› ")
        } else {
            "  ".to_string()
        };
        let prefix = style_dim(&session_tree_prefix(row));
        let mut right_parts = Vec::new();
        if matches!(state.scope, SessionScope::All) {
            right_parts.push(shorten_home_path(&row.record.cwd.to_string_lossy()));
        }
        if state.show_path {
            right_parts.push(shorten_home_path(&row.record.path.to_string_lossy()));
        }
        right_parts.push(format!(
            "{} {}",
            row.record.message_count,
            format_relative_age(row.record.modified_epoch_ms)
        ));
        let right = style_dim(&right_parts.join(" "));
        let available = width
            .saturating_sub(visible_width(&cursor))
            .saturating_sub(visible_width(&prefix))
            .saturating_sub(visible_width(&right))
            .saturating_sub(1);
        let truncated = truncate_to_width(&display, available.max(12));
        let styled_message = if is_confirming_delete {
            style_error(&truncated)
        } else if is_current {
            style_brand(&truncated)
        } else if has_name {
            style_warning(&truncated)
        } else {
            truncated
        };
        let message = if is_selected {
            style_title(&styled_message)
        } else {
            styled_message
        };
        let left = format!("{cursor}{prefix}{message}");
        let spacing = width.saturating_sub(visible_width(&left) + visible_width(&right));
        let line = fit_line(
            &format!("{left}{}{right}", " ".repeat(spacing.max(1))),
            width as u16,
        );
        target.push(RenderedLine::Text(if is_selected {
            style_selected_row(&line)
        } else {
            line
        }));
    }

    if start > 0 || end < filtered_indices.len() {
        target.push(RenderedLine::Text(fit_line(
            &style_hint(&format!(
                "  ({}/{})",
                selected_index + 1,
                filtered_indices.len()
            )),
            width as u16,
        )));
    }
}

pub(crate) fn session_tree_prefix(row: &SessionOverlayRow) -> String {
    if row.depth == 0 {
        return String::new();
    }
    let mut prefix = String::new();
    for continues in &row.ancestor_continues {
        prefix.push_str(if *continues { "│  " } else { "   " });
    }
    prefix.push_str(if row.is_last { "└─ " } else { "├─ " });
    prefix
}

pub(crate) fn session_record_search_text(record: &SessionRecord) -> String {
    format!(
        "{} {} {} {}",
        record.name.as_deref().unwrap_or_default(),
        record.preview,
        record.path.to_string_lossy(),
        record.cwd.to_string_lossy()
    )
}

pub(crate) fn normalize_whitespace_lower(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn parse_session_search_query(query: &str) -> ParsedSessionSearchQuery {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return ParsedSessionSearchQuery {
            mode: SessionSearchMode::Tokens,
            tokens: Vec::new(),
            regex: None,
            error: None,
        };
    }

    if let Some(pattern) = trimmed.strip_prefix("re:") {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return ParsedSessionSearchQuery {
                mode: SessionSearchMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some("Empty regex".to_string()),
            };
        }
        return match regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
        {
            Ok(regex) => ParsedSessionSearchQuery {
                mode: SessionSearchMode::Regex,
                tokens: Vec::new(),
                regex: Some(regex),
                error: None,
            },
            Err(error) => ParsedSessionSearchQuery {
                mode: SessionSearchMode::Regex,
                tokens: Vec::new(),
                regex: None,
                error: Some(error.to_string()),
            },
        };
    }

    let mut tokens = Vec::new();
    let mut buffer = String::new();
    let mut in_quote = false;
    let mut had_unclosed_quote = false;

    let flush = |kind: SessionSearchTokenKind,
                 buffer: &mut String,
                 tokens: &mut Vec<SessionSearchToken>| {
        let value = buffer.trim();
        if !value.is_empty() {
            tokens.push(SessionSearchToken {
                kind,
                value: value.to_string(),
            });
        }
        buffer.clear();
    };

    for ch in trimmed.chars() {
        if ch == '"' {
            if in_quote {
                flush(SessionSearchTokenKind::Phrase, &mut buffer, &mut tokens);
                in_quote = false;
            } else {
                flush(SessionSearchTokenKind::Fuzzy, &mut buffer, &mut tokens);
                in_quote = true;
            }
            continue;
        }

        if !in_quote && ch.is_whitespace() {
            flush(SessionSearchTokenKind::Fuzzy, &mut buffer, &mut tokens);
            continue;
        }

        buffer.push(ch);
    }

    if in_quote {
        had_unclosed_quote = true;
    }

    if had_unclosed_quote {
        return ParsedSessionSearchQuery {
            mode: SessionSearchMode::Tokens,
            tokens: trimmed
                .split_whitespace()
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(|token| SessionSearchToken {
                    kind: SessionSearchTokenKind::Fuzzy,
                    value: token.to_string(),
                })
                .collect(),
            regex: None,
            error: None,
        };
    }

    flush(
        if in_quote {
            SessionSearchTokenKind::Phrase
        } else {
            SessionSearchTokenKind::Fuzzy
        },
        &mut buffer,
        &mut tokens,
    );

    ParsedSessionSearchQuery {
        mode: SessionSearchMode::Tokens,
        tokens,
        regex: None,
        error: None,
    }
}

pub(crate) fn session_record_match_score(
    record: &SessionRecord,
    parsed: &ParsedSessionSearchQuery,
) -> Option<i64> {
    let text = session_record_search_text(record);

    match parsed.mode {
        SessionSearchMode::Regex => {
            let regex = parsed.regex.as_ref()?;
            regex
                .find(&text)
                .map(|matched| matched.start() as i64 * 100)
        }
        SessionSearchMode::Tokens => {
            if parsed.tokens.is_empty() {
                return Some(0);
            }

            let mut total_score = 0i64;
            let mut normalized_text = None::<String>;
            for token in &parsed.tokens {
                match token.kind {
                    SessionSearchTokenKind::Phrase => {
                        let haystack = normalized_text
                            .get_or_insert_with(|| normalize_whitespace_lower(&text));
                        let phrase = normalize_whitespace_lower(&token.value);
                        if phrase.is_empty() {
                            continue;
                        }
                        let index = haystack.find(&phrase)?;
                        total_score += index as i64 * 100;
                    }
                    SessionSearchTokenKind::Fuzzy => {
                        total_score += fuzzy_text_score(&token.value, &text)?;
                    }
                }
            }
            Some(total_score)
        }
    }
}

pub(crate) fn fuzzy_text_score(query: &str, text: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }

    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();
    if let Some(index) = text_lower.find(&query_lower) {
        return Some(index as i64 * 10);
    }

    let text_chars = text_lower.chars().collect::<Vec<_>>();
    let mut first_index = None;
    let mut previous_index = None;
    let mut search_index = 0usize;
    let mut gaps = 0i64;

    for query_char in query_lower.chars() {
        let mut matched = None;
        while search_index < text_chars.len() {
            if text_chars[search_index] == query_char {
                matched = Some(search_index);
                search_index += 1;
                break;
            }
            search_index += 1;
        }
        let matched = matched?;
        if let Some(previous) = previous_index {
            gaps += matched.saturating_sub(previous + 1) as i64;
        } else {
            first_index = Some(matched);
        }
        previous_index = Some(matched);
    }

    Some(first_index.unwrap_or(0) as i64 * 20 + gaps)
}

pub(crate) fn flatten_session_record_tree(records: Vec<SessionRecord>) -> Vec<SessionOverlayRow> {
    let mut by_path = std::collections::HashMap::new();
    for (index, record) in records.iter().enumerate() {
        by_path.insert(record.path.to_string_lossy().to_string(), index);
    }
    let mut children: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    let mut roots = Vec::new();
    for (index, record) in records.iter().enumerate() {
        match record
            .parent_session
            .as_deref()
            .and_then(|path| by_path.get(path).copied())
        {
            Some(parent_index) if parent_index != index => {
                children.entry(parent_index).or_default().push(index);
            }
            _ => roots.push(index),
        }
    }
    let sort_indices = |indices: &mut Vec<usize>| {
        indices.sort_by(|left, right| {
            records[*right]
                .modified_epoch_ms
                .cmp(&records[*left].modified_epoch_ms)
        });
    };
    sort_indices(&mut roots);
    for values in children.values_mut() {
        sort_indices(values);
    }

    fn walk(
        index: usize,
        depth: usize,
        is_last: bool,
        ancestor_continues: &[bool],
        records: &[SessionRecord],
        children: &std::collections::HashMap<usize, Vec<usize>>,
        out: &mut Vec<SessionOverlayRow>,
    ) {
        out.push(SessionOverlayRow {
            record: records[index].clone(),
            depth,
            is_last,
            ancestor_continues: ancestor_continues.to_vec(),
        });
        if let Some(next_children) = children.get(&index) {
            let next_ancestor_continues = if depth == 0 {
                ancestor_continues.to_vec()
            } else {
                let mut next = ancestor_continues.to_vec();
                next.push(!is_last);
                next
            };
            for (child_index, child) in next_children.iter().enumerate() {
                walk(
                    *child,
                    depth + 1,
                    child_index + 1 == next_children.len(),
                    &next_ancestor_continues,
                    records,
                    children,
                    out,
                );
            }
        }
    }

    let root_count = roots.len();
    let mut out = Vec::new();
    for (root_index, root) in roots.into_iter().enumerate() {
        walk(
            root,
            0,
            root_index + 1 == root_count,
            &[],
            &records,
            &children,
            &mut out,
        );
    }
    out
}

pub(crate) fn format_relative_age(epoch_ms: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(epoch_ms);
    let diff = (now - epoch_ms).max(0);
    let minutes = diff / 60_000;
    if minutes < 1 {
        return "now".to_string();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    if days < 30 {
        return format!("{}w", days / 7);
    }
    if days < 365 {
        return format!("{}mo", days / 30);
    }
    format!("{}y", days / 365)
}
