use super::*;

pub(super) struct TranscriptRenderContext<'a> {
    pub width: u16,
    pub hide_thinking: bool,
    pub show_images: bool,
    pub terminal_capabilities: &'a TerminalCapabilities,
    pub tool_expand_mode: ToolExpandMode,
    pub latest_tool_panel: Option<&'a str>,
    pub expand_hint: &'a str,
}

impl<'a> TranscriptRenderContext<'a> {
    pub(super) fn new(
        width: u16,
        hide_thinking: bool,
        show_images: bool,
        terminal_capabilities: &'a TerminalCapabilities,
        tool_expand_mode: ToolExpandMode,
        latest_tool_panel: Option<&'a str>,
        expand_hint: &'a str,
    ) -> Self {
        Self {
            width,
            hide_thinking,
            show_images,
            terminal_capabilities,
            tool_expand_mode,
            latest_tool_panel,
            expand_hint,
        }
    }
}

pub(super) fn session_transcript_lines_with_context(
    entries: &[TranscriptEntry],
    context: &TranscriptRenderContext<'_>,
) -> Vec<RenderedLine> {
    session_transcript_lines(
        entries,
        context.width,
        context.hide_thinking,
        context.show_images,
        context.terminal_capabilities,
        context.tool_expand_mode,
        context.latest_tool_panel,
        context.expand_hint,
    )
}

pub(super) fn active_tool_render_lines_with_context(
    tools: &[ActiveToolExecution],
    context: &TranscriptRenderContext<'_>,
) -> Vec<RenderedLine> {
    active_tool_render_lines(
        tools,
        context.width,
        context.show_images,
        context.terminal_capabilities,
        context.tool_expand_mode,
        context.latest_tool_panel,
        context.expand_hint,
    )
}

pub(super) fn build_transcript_entries(session: &AgentSession) -> Vec<TranscriptEntry> {
    let mut transcript = Vec::new();
    for entry in session.session().get_entries() {
        let Ok(parsed) = serde_json::from_value::<SessionEntry>(entry.clone()) else {
            continue;
        };
        match parsed {
            SessionEntry::Message(message) => {
                transcript.push(TranscriptEntry::Message(message.message));
            }
            SessionEntry::CustomMessage(entry) if entry.display => {
                transcript.push(TranscriptEntry::CustomMessage {
                    custom_type: entry.custom_type,
                    content: entry.content,
                    details: entry.details,
                });
            }
            SessionEntry::Compaction(entry) => {
                transcript.push(TranscriptEntry::Summary {
                    kind: SummaryKind::Compaction,
                    title: "Compaction Summary",
                    text: entry.summary,
                    tokens_before: Some(entry.tokens_before),
                });
            }
            SessionEntry::BranchSummary(entry) => {
                transcript.push(TranscriptEntry::Summary {
                    kind: SummaryKind::Branch,
                    title: "Branch Summary",
                    text: entry.summary,
                    tokens_before: None,
                });
            }
            _ => {}
        }
    }
    transcript
}

pub(super) fn session_transcript_lines(
    entries: &[TranscriptEntry],
    width: u16,
    hide_thinking: bool,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
    tool_expand_mode: ToolExpandMode,
    latest_tool_panel: Option<&str>,
    expand_hint: &str,
) -> Vec<RenderedLine> {
    let content_width = width.saturating_sub(2).max(20) as usize;
    let mut lines = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        let tool_call_args = match entry {
            TranscriptEntry::Message(Message::ToolResult(result)) => {
                find_tool_call_arguments(entries, index, &result.tool_call_id)
            }
            _ => None,
        };
        render_transcript_entry(
            &mut lines,
            entry,
            index,
            tool_call_args,
            content_width,
            hide_thinking,
            show_images,
            terminal_capabilities,
            tool_expand_mode,
            latest_tool_panel,
            expand_hint,
        );
        lines.push(RenderedLine::Text(String::new()));
    }

    lines
}

pub(super) fn session_selection_detail(record: &SessionRecord, is_current: bool) -> String {
    let preview = truncate_to_width(&record.preview.replace('\n', " ").trim(), 96);
    let mut meta = vec![
        format!("{} msg", record.message_count),
        format_relative_age(record.modified_epoch_ms),
        format!("cwd {}", shorten_home_path(&record.cwd.to_string_lossy())),
    ];
    if is_current {
        meta.push("current".to_string());
    }
    let mut lines = vec![preview, meta.join(" · ")];
    let mut path_line = format!(
        "session {}",
        shorten_home_path(&record.path.to_string_lossy())
    );
    if let Some(parent) = record.parent_session.as_deref() {
        path_line.push_str(" · parent ");
        path_line.push_str(&shorten_home_path(parent));
    }
    lines.push(truncate_to_width(&path_line, 96));
    lines.join("\n")
}

fn find_tool_call_arguments<'a>(
    entries: &'a [TranscriptEntry],
    entry_index: usize,
    tool_call_id: &str,
) -> Option<&'a Value> {
    entries[..entry_index]
        .iter()
        .rev()
        .find_map(|entry| match entry {
            TranscriptEntry::Message(Message::Assistant(assistant)) => assistant
                .content
                .iter()
                .rev()
                .find_map(|block| match block {
                    AssistantContentBlock::ToolCall { id, arguments, .. } if id == tool_call_id => {
                        Some(arguments)
                    }
                    _ => None,
                }),
            _ => None,
        })
}

pub(super) fn render_transcript_entry(
    target: &mut Vec<RenderedLine>,
    entry: &TranscriptEntry,
    entry_index: usize,
    tool_call_args: Option<&Value>,
    width: usize,
    hide_thinking: bool,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
    tool_expand_mode: ToolExpandMode,
    latest_tool_panel: Option<&str>,
    expand_hint: &str,
) {
    match entry {
        TranscriptEntry::Message(message) => render_message(
            target,
            message,
            tool_call_args,
            width,
            hide_thinking,
            show_images,
            terminal_capabilities,
            tool_expand_mode,
            latest_tool_panel,
            expand_hint,
        ),
        TranscriptEntry::CustomMessage {
            custom_type,
            content,
            details,
        } => render_custom_message(
            target,
            custom_type,
            content,
            details.as_ref(),
            entry_index,
            width,
            show_images,
            terminal_capabilities,
            tool_expand_mode,
            latest_tool_panel,
            expand_hint,
        ),
        TranscriptEntry::Summary {
            kind,
            title,
            text,
            tokens_before,
        } => render_summary_entry(
            target,
            *kind,
            title,
            text,
            *tokens_before,
            width,
            expand_hint,
        ),
    }
}

fn render_message(
    target: &mut Vec<RenderedLine>,
    message: &Message,
    tool_call_args: Option<&Value>,
    width: usize,
    hide_thinking: bool,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
    tool_expand_mode: ToolExpandMode,
    latest_tool_panel: Option<&str>,
    expand_hint: &str,
) {
    match message {
        Message::User(user) => {
            if let Some(skill) = parse_skill_block(&content_text(&user.content)) {
                render_skill_invocation_message(
                    target,
                    &skill,
                    width,
                    tool_expand_mode,
                    latest_tool_panel,
                    expand_hint,
                );
                if let Some(user_message) = skill.user_message.as_deref() {
                    render_plain_user_content(
                        target,
                        "You",
                        &UserContent::Text(user_message.to_string()),
                        width,
                        show_images,
                        terminal_capabilities,
                    );
                }
            } else {
                render_plain_user_content(
                    target,
                    "You",
                    &user.content,
                    width,
                    show_images,
                    terminal_capabilities,
                );
            }
        }
        Message::Assistant(assistant) => {
            render_assistant_message(target, assistant, width, hide_thinking)
        }
        Message::ToolResult(result) => render_tool_result(
            target,
            result,
            tool_call_args,
            width,
            show_images,
            terminal_capabilities,
            tool_expand_mode,
            latest_tool_panel,
            expand_hint,
        ),
    }
}

fn render_plain_user_content(
    target: &mut Vec<RenderedLine>,
    _prefix: &str,
    content: &UserContent,
    width: usize,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
) {
    let inner_width = width.saturating_sub(2).max(1);
    let mut body = Vec::new();
    let mut images = Vec::new();

    match content {
        UserContent::Text(text) => {
            body.extend(collect_markdown_lines(
                text,
                inner_width.saturating_sub(2).max(1),
            ));
        }
        UserContent::Blocks(blocks) => {
            for block in blocks {
                match block {
                    UserContentBlock::Text { text, .. } => {
                        if !body.is_empty() {
                            body.push(String::new());
                        }
                        body.extend(collect_markdown_lines(
                            text,
                            inner_width.saturating_sub(2).max(1),
                        ));
                    }
                    UserContentBlock::Image {
                        mime_type, data, ..
                    } => {
                        if show_images && terminal_capabilities.inline_images {
                            images.push((mime_type.clone(), Some(data.clone())));
                        } else {
                            if !body.is_empty() {
                                body.push(String::new());
                            }
                            body.push(style_hint(&format!("[image: {mime_type}]")));
                        }
                    }
                }
            }
        }
    }

    append_user_message_block(target, &body, width);
    for (mime_type, data) in images {
        append_image_block(
            target,
            "",
            &mime_type,
            data.as_deref(),
            width,
            show_images,
            terminal_capabilities,
        );
    }
}

fn render_user_content(
    target: &mut Vec<RenderedLine>,
    prefix: &str,
    content: &UserContent,
    width: usize,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
) {
    if let Some(skill) = parse_skill_block(&content_text(content)) {
        render_skill_invocation_message(target, &skill, width, ToolExpandMode::All, None, "ctrl+o");
        if let Some(user_message) = skill.user_message.as_deref() {
            render_plain_user_content(
                target,
                prefix,
                &UserContent::Text(user_message.to_string()),
                width,
                show_images,
                terminal_capabilities,
            );
        }
        return;
    }

    render_plain_user_content(
        target,
        prefix,
        content,
        width,
        show_images,
        terminal_capabilities,
    );
}

fn render_custom_message(
    target: &mut Vec<RenderedLine>,
    custom_type: &str,
    content: &UserContent,
    details: Option<&Value>,
    entry_index: usize,
    width: usize,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
    tool_expand_mode: ToolExpandMode,
    latest_tool_panel: Option<&str>,
    expand_hint: &str,
) {
    match custom_type {
        "bash_execution" => render_bash_execution_message(
            target,
            content,
            details,
            entry_index,
            width,
            tool_expand_mode,
            latest_tool_panel,
            expand_hint,
        ),
        _ => {
            if let Some(skill) = parse_skill_block(&content_text(content)) {
                render_skill_invocation_message(
                    target,
                    &skill,
                    width,
                    tool_expand_mode,
                    latest_tool_panel,
                    expand_hint,
                );
                if let Some(user_message) = skill.user_message.as_deref() {
                    render_user_content(
                        target,
                        "",
                        &UserContent::Text(user_message.to_string()),
                        width,
                        show_images,
                        terminal_capabilities,
                    );
                }
            } else {
                render_generic_custom_message(
                    target,
                    custom_type,
                    content,
                    width,
                    show_images,
                    terminal_capabilities,
                );
            }
        }
    }
}

fn render_summary_entry(
    target: &mut Vec<RenderedLine>,
    kind: SummaryKind,
    title: &str,
    text: &str,
    tokens_before: Option<u64>,
    width: usize,
    expand_hint: &str,
) {
    match kind {
        SummaryKind::Generic => append_panel_block(
            target,
            &style_title(title),
            &collect_markdown_lines(text, width.saturating_sub(4).max(1)),
            width,
        ),
        SummaryKind::Branch => append_custom_surface_block(
            target,
            &[
                style_custom_label("[branch]"),
                String::new(),
                style_custom_text(&format!("Branch summary ({expand_hint} to expand)")),
                String::new(),
                style_custom_text(&truncate_to_width(text, width.saturating_sub(2).max(1))),
            ],
            width,
        ),
        SummaryKind::Compaction => append_custom_surface_block(
            target,
            &[
                style_custom_label("[compaction]"),
                String::new(),
                style_custom_text(&format!(
                    "Compacted from {} tokens ({expand_hint} to expand)",
                    tokens_before
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "?".to_string())
                )),
                String::new(),
                style_custom_text(&truncate_to_width(text, width.saturating_sub(2).max(1))),
            ],
            width,
        ),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ParsedSkillBlock {
    pub(super) name: String,
    pub(super) location: String,
    pub(super) content: String,
    pub(super) user_message: Option<String>,
}

pub(super) fn parse_skill_block(text: &str) -> Option<ParsedSkillBlock> {
    let captures = Regex::new(
        r#"(?s)^<skill name="([^"]+)" location="([^"]+)">\s*(.*?)\s*</skill>(?:\s+(.*))?$"#,
    )
    .ok()?
    .captures(text)?;
    Some(ParsedSkillBlock {
        name: captures.get(1)?.as_str().to_string(),
        location: captures.get(2)?.as_str().to_string(),
        content: captures.get(3)?.as_str().to_string(),
        user_message: captures
            .get(4)
            .map(|value| value.as_str().trim().to_string())
            .filter(|value| !value.is_empty()),
    })
}

fn render_skill_invocation_message(
    target: &mut Vec<RenderedLine>,
    skill: &ParsedSkillBlock,
    width: usize,
    tool_expand_mode: ToolExpandMode,
    latest_tool_panel: Option<&str>,
    expand_hint: &str,
) {
    let panel_id = format!("skill:{}", skill.name);
    let expanded =
        should_expand_tool_panel(Some(panel_id.as_str()), tool_expand_mode, latest_tool_panel);
    let mut body = vec![style_custom_label("[skill]"), String::new()];
    if expanded {
        body.push(style_custom_text(&skill.name));
        body.push(String::new());
        body.extend(
            collect_markdown_lines(&skill.content, width.saturating_sub(2).max(1))
                .into_iter()
                .map(|line| style_custom_text(&line)),
        );
    } else {
        body.push(style_custom_text(&format!(
            "{} ({expand_hint} to expand)",
            skill.name
        )));
    }
    append_custom_surface_block(target, &body, width);
}

fn render_generic_custom_message(
    target: &mut Vec<RenderedLine>,
    custom_type: &str,
    content: &UserContent,
    width: usize,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
) {
    let mut body = vec![
        style_custom_label(&format!("[{}]", custom_type)),
        String::new(),
    ];
    let mut images = Vec::new();
    match content {
        UserContent::Text(text) => {
            body.extend(
                collect_markdown_lines(text, width.saturating_sub(2).max(1))
                    .into_iter()
                    .map(|line| style_custom_text(&line)),
            );
        }
        UserContent::Blocks(blocks) => {
            for block in blocks {
                match block {
                    UserContentBlock::Text { text, .. } => {
                        if body.last().is_some_and(|line| !line.is_empty()) {
                            body.push(String::new());
                        }
                        body.extend(
                            collect_markdown_lines(text, width.saturating_sub(2).max(1))
                                .into_iter()
                                .map(|line| style_custom_text(&line)),
                        );
                    }
                    UserContentBlock::Image {
                        mime_type, data, ..
                    } => {
                        if show_images && terminal_capabilities.inline_images {
                            images.push((mime_type.clone(), Some(data.clone())));
                        } else {
                            body.push(style_hint(&format!("[image: {mime_type}]")));
                        }
                    }
                }
            }
        }
    }
    append_custom_surface_block(target, &body, width);
    for (mime_type, data) in images {
        append_image_block(
            target,
            "",
            &mime_type,
            data.as_deref(),
            width,
            show_images,
            terminal_capabilities,
        );
    }
}

fn render_bash_execution_message(
    target: &mut Vec<RenderedLine>,
    content: &UserContent,
    details: Option<&Value>,
    entry_index: usize,
    width: usize,
    tool_expand_mode: ToolExpandMode,
    latest_tool_panel: Option<&str>,
    expand_hint: &str,
) {
    let text = content_text(content);
    let inner_width = width.max(1);
    let mut body = Vec::new();
    let excluded_from_context = details
        .and_then(|details| details.get("excludeFromContext"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut lines = text.lines();
    let title = if let Some(command) =
        details.and_then(|details| details.get("command").and_then(Value::as_str))
    {
        let _ = lines.next();
        if command.is_empty() {
            "$".to_string()
        } else {
            format!("$ {command}")
        }
    } else {
        let first = lines.next().unwrap_or("$").trim();
        if let Some(command) = first.strip_prefix("$ ") {
            format!("$ {command}")
        } else {
            first.to_string()
        }
    };

    for line in lines {
        if line.is_empty() {
            body.push(String::new());
        } else if line.starts_with("Exit code: 0") {
            continue;
        } else if line.starts_with("Exit code:") {
            let code = line.trim_start_matches("Exit code:").trim();
            body.push(style_warning(&format!("(exit {code})")));
        } else if line.starts_with("Command cancelled") {
            body.push(style_warning("(cancelled)"));
        } else if line.starts_with("Full output:") {
            body.push(style_hint(line));
        } else {
            body.push(style_dim(&truncate_to_width(line, inner_width)));
        }
    }

    if let Some(details) = details {
        if details
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && !text.contains("Full output:")
        {
            if let Some(path) = details.get("fullOutputPath").and_then(Value::as_str) {
                body.push(style_hint(&format!("Full output: {path}")));
            } else {
                body.push(style_warning("Output truncated"));
            }
        }
    }

    let panel_id = bash_panel_id(entry_index);
    let body = collapse_panel_body(
        &body,
        1,
        20,
        true,
        should_expand_tool_panel(Some(panel_id.as_str()), tool_expand_mode, latest_tool_panel),
        expand_hint,
    );
    append_tool_surface_block(target, &title, &body, width, excluded_from_context);
}

fn render_assistant_message(
    target: &mut Vec<RenderedLine>,
    assistant: &pi_rust_ai_core::AssistantMessage,
    width: usize,
    hide_thinking: bool,
) {
    let mut first = true;
    let has_tool_calls = assistant
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall { .. }));

    for block in &assistant.content {
        match block {
            AssistantContentBlock::Text { text, .. } => {
                if !text.trim().is_empty() {
                    if !first {
                        target.push(RenderedLine::Text(String::new()));
                    }
                    append_markdown_block(target, text, width);
                    first = false;
                }
            }
            AssistantContentBlock::Thinking { thinking, .. } if !hide_thinking => {
                if !thinking.trim().is_empty() {
                    if !first {
                        target.push(RenderedLine::Text(String::new()));
                    }
                    append_thinking_block(target, thinking, width);
                    first = false;
                }
            }
            AssistantContentBlock::Thinking { .. } if hide_thinking => {
                if !first {
                    target.push(RenderedLine::Text(String::new()));
                }
                target.push(RenderedLine::Text(style_thinking_surface("Thinking...")));
                first = false;
            }
            AssistantContentBlock::ToolCall {
                name, arguments, ..
            } => {
                if !first {
                    target.push(RenderedLine::Text(String::new()));
                }
                append_tool_call_block(target, name, arguments, width);
                first = false;
            }
            _ => {}
        }
    }

    if !has_tool_calls {
        match assistant.stop_reason {
            StopReason::Aborted => {
                if !first {
                    target.push(RenderedLine::Text(String::new()));
                }
                let message = assistant
                    .error_message
                    .as_deref()
                    .filter(|value| *value != "Request was aborted")
                    .unwrap_or("Operation aborted");
                target.push(RenderedLine::Text(style_warning(message)));
                first = false;
            }
            StopReason::Error => {
                if !first {
                    target.push(RenderedLine::Text(String::new()));
                }
                let message = assistant
                    .error_message
                    .as_deref()
                    .unwrap_or("Unknown error");
                target.push(RenderedLine::Text(style_error(&format!(
                    "Error: {message}"
                ))));
                first = false;
            }
            _ => {}
        }
    }

    if first {
        target.push(RenderedLine::Text(style_hint("...")));
    }
}

fn render_tool_result(
    target: &mut Vec<RenderedLine>,
    result: &pi_rust_ai_core::ToolResultMessage,
    args: Option<&Value>,
    width: usize,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
    tool_expand_mode: ToolExpandMode,
    latest_tool_panel: Option<&str>,
    expand_hint: &str,
) {
    let inner_width = width.max(1);
    let (title, mut body, images) = build_tool_result_panel(result, args, inner_width);
    body.extend(tool_notice_lines(result));
    let panel_id = tool_result_panel_id(result);
    let collapsed_body = collapse_panel_body(
        &body,
        1,
        tool_preview_config(&result.tool_name).0,
        tool_preview_config(&result.tool_name).1,
        should_expand_tool_panel(Some(panel_id.as_str()), tool_expand_mode, latest_tool_panel),
        expand_hint,
    );
    append_tool_surface_block(target, &title, &collapsed_body, width, false);
    for (mime_type, data) in images {
        append_image_block(
            target,
            "",
            &mime_type,
            data.as_deref(),
            width,
            show_images,
            terminal_capabilities,
        );
    }
}

fn tool_panel_title(
    tool_name: &str,
    args: Option<&Value>,
    details: Option<&Value>,
    fallback_text: Option<&str>,
) -> String {
    match tool_name {
        "read" => {
            let mut title = "read".to_string();
            if let Some(path) = tool_argument_path(args) {
                title.push(' ');
                title.push_str(&path);
                if let Some(range) = tool_read_range(args) {
                    title.push_str(&range);
                }
            }
            title
        }
        "write" => format!(
            "write{}",
            tool_argument_path(args)
                .map(|path| format!(" {path}"))
                .unwrap_or_default()
        ),
        "edit" => {
            let mut title = "edit".to_string();
            if let Some(path) = tool_argument_path(args) {
                title.push(' ');
                title.push_str(&path);
            }
            if let Some(line) = details
                .and_then(|details| details.get("firstChangedLine"))
                .and_then(Value::as_u64)
            {
                title.push(':');
                title.push_str(&line.to_string());
            }
            title
        }
        "bash" => tool_argument_string(args, &["command"])
            .filter(|command| !command.is_empty())
            .map(|command| format!("$ {}", truncate_to_width(command, 48)))
            .unwrap_or_else(|| "$".to_string()),
        "ls" => format!(
            "ls{}",
            tool_argument_path(args)
                .map(|path| format!(" {path}"))
                .unwrap_or_default()
        ),
        "find" => {
            let pattern = tool_argument_string(args, &["pattern"]).unwrap_or_default();
            let path = tool_argument_path(args).unwrap_or_else(|| ".".to_string());
            if pattern.is_empty() {
                format!("find {path}")
            } else {
                format!("find {} in {path}", truncate_to_width(pattern, 28))
            }
        }
        "grep" => {
            let pattern = tool_argument_string(args, &["pattern"]).unwrap_or_default();
            let path = tool_argument_path(args).unwrap_or_else(|| ".".to_string());
            if pattern.is_empty() {
                format!("grep {path}")
            } else {
                format!("grep /{}/ in {path}", truncate_to_width(pattern, 24))
            }
        }
        _ => fallback_text
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("Tool {tool_name}")),
    }
}

fn tool_argument_string<'a>(args: Option<&'a Value>, keys: &[&str]) -> Option<&'a str> {
    let args = args?;
    keys.iter()
        .find_map(|key| args.get(*key).and_then(Value::as_str))
}

fn tool_argument_path(args: Option<&Value>) -> Option<String> {
    tool_argument_string(args, &["path", "file_path"]).map(shorten_home_path)
}

fn tool_read_range(args: Option<&Value>) -> Option<String> {
    let args = args?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1);
    let limit = args.get("limit").and_then(Value::as_u64);
    match limit {
        Some(limit) if limit > 0 => Some(format!(":{offset}-{}", offset + limit - 1)),
        Some(_) => None,
        None if offset > 1 => Some(format!(":{offset}+")),
        None => None,
    }
}

pub(super) fn shorten_home_path(path: &str) -> String {
    std::env::var("HOME")
        .ok()
        .filter(|home| path.starts_with(home))
        .map(|home| format!("~{}", &path[home.len()..]))
        .unwrap_or_else(|| path.to_string())
}

fn build_read_result_lines(text: &str, _args: Option<&Value>, width: usize) -> Vec<String> {
    let (content, notices) = split_bracket_notices(text);
    let mut lines = Vec::new();
    if content.is_empty() {
        lines.push(style_hint("No file content returned"));
    } else if content.len() == 1 && content[0].starts_with("Read image file [") {
        lines.push(style_hint(&content[0]));
    } else {
        lines.extend(collect_code_block_lines(&content, width));
    }
    for notice in notices {
        lines.push(style_hint(&notice));
    }
    lines
}

fn build_write_result_lines(
    text: &str,
    args: Option<&Value>,
    width: usize,
    is_error: bool,
) -> Vec<String> {
    if is_error {
        return collect_literal_lines(text, width, style_error);
    }

    let mut lines = Vec::new();
    if let Some(content) = tool_argument_string(args, &["content"])
        && !content.is_empty()
    {
        let preview_lines = content
            .replace('\t', "   ")
            .lines()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        lines.extend(collect_code_block_lines(&preview_lines, width));
    }
    lines
}

fn build_edit_result_lines(text: &str, width: usize, is_error: bool) -> Vec<String> {
    if is_error {
        collect_literal_lines(text, width, style_error)
    } else {
        let mut lines = Vec::new();
        if text.contains('\n') {
            let preview_lines = text
                .replace('\t', "   ")
                .lines()
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            lines.extend(collect_code_block_lines(&preview_lines, width));
        }
        lines
    }
}

fn build_grep_result_lines(text: &str, width: usize) -> Vec<String> {
    let (content, notices) = split_bracket_notices(text);
    let mut lines = Vec::new();
    if content.is_empty() {
        lines.push(style_hint("No matches found"));
    } else {
        let mut current_path: Option<String> = None;
        for line in content {
            if let Some((path, line_number, body)) = parse_grep_match_line(&line) {
                if current_path.as_deref() != Some(path) {
                    if !lines.is_empty() {
                        lines.push(String::new());
                    }
                    lines.push(style_title(&truncate_to_width(path, width)));
                    current_path = Some(path.to_string());
                }
                lines.extend(wrap_with_prefix(
                    body,
                    &style_subtitle(&format!("  {line_number}: ")),
                    width,
                ));
            } else if let Some((path, line_number, body)) = parse_grep_context_line(&line) {
                if current_path.as_deref() != Some(path) {
                    if !lines.is_empty() {
                        lines.push(String::new());
                    }
                    lines.push(style_title(&truncate_to_width(path, width)));
                    current_path = Some(path.to_string());
                }
                lines.extend(
                    wrap_with_prefix(body, &style_hint(&format!("  {line_number}- ")), width)
                        .into_iter()
                        .map(|line| style_dim(&line)),
                );
            } else {
                lines.extend(collect_literal_lines(&line, width, style_dim));
            }
        }
    }
    for notice in notices {
        lines.push(style_hint(&notice));
    }
    lines
}

fn build_find_result_lines(text: &str, width: usize) -> Vec<String> {
    let (content, notices) = split_bracket_notices(text);
    let mut lines = Vec::new();
    if content.is_empty() {
        lines.push(style_hint("No files found matching pattern"));
    } else {
        for line in content {
            lines.push(if line.ends_with('/') {
                style_title(&truncate_to_width(&format!("dir  {line}"), width))
            } else {
                style_code_block_line(&truncate_to_width(&format!("file {line}"), width))
            });
        }
    }
    for notice in notices {
        lines.push(style_hint(&notice));
    }
    lines
}

fn build_ls_result_lines(text: &str, width: usize) -> Vec<String> {
    let (content, notices) = split_bracket_notices(text);
    let mut lines = Vec::new();
    if content.is_empty() {
        lines.push(style_hint("(empty directory)"));
    } else {
        for line in content {
            lines.push(if line.ends_with('/') {
                style_title(&truncate_to_width(&format!("dir  {line}"), width))
            } else {
                style_dim(&truncate_to_width(&format!("file {line}"), width))
            });
        }
    }
    for notice in notices {
        lines.push(style_hint(&notice));
    }
    lines
}

fn parse_grep_match_line(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(3, ':');
    let path = parts.next()?;
    let line_number = parts.next()?;
    let body = parts.next()?;
    if line_number.chars().all(|ch| ch.is_ascii_digit()) {
        Some((path, line_number, body.trim_start()))
    } else {
        None
    }
}

fn parse_grep_context_line(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.splitn(3, '-');
    let path = parts.next()?;
    let line_number = parts.next()?;
    let body = parts.next()?;
    if line_number.chars().all(|ch| ch.is_ascii_digit()) {
        Some((path, line_number, body.trim_start()))
    } else {
        None
    }
}

fn split_bracket_notices(text: &str) -> (Vec<String>, Vec<String>) {
    let mut content = Vec::new();
    let mut notices = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            notices.push(trimmed.to_string());
        } else if !line.is_empty() || !content.is_empty() {
            content.push(line.to_string());
        }
    }
    while content.last().is_some_and(|line| line.is_empty()) {
        content.pop();
    }
    (content, notices)
}

fn build_tool_result_panel(
    result: &pi_rust_ai_core::ToolResultMessage,
    args: Option<&Value>,
    width: usize,
) -> (String, Vec<String>, Vec<(String, Option<String>)>) {
    let title = tool_panel_title(&result.tool_name, args, result.details.as_ref(), None);
    let mut body = Vec::new();
    let mut images = Vec::new();

    if let Some(details) = &result.details
        && let Some(diff) = details.get("diff").and_then(Value::as_str)
    {
        body.extend(collect_diff_lines(diff, width));
        return (title, body, images);
    }

    for block in &result.content {
        match block {
            UserContentBlock::Text { text, .. } => body.extend(match result.tool_name.as_str() {
                "read" => build_read_result_lines(text, args, width),
                "write" => build_write_result_lines(text, args, width, result.is_error),
                "edit" => build_edit_result_lines(text, width, result.is_error),
                "grep" => build_grep_result_lines(text, width),
                "find" => build_find_result_lines(text, width),
                "ls" => build_ls_result_lines(text, width),
                "bash" => collect_bash_result_lines(text, width),
                _ if result.is_error => collect_literal_lines(text, width, style_error),
                _ => collect_markdown_lines(text, width),
            }),
            UserContentBlock::Image {
                mime_type, data, ..
            } => {
                images.push((mime_type.clone(), Some(data.clone())));
            }
        }
    }

    (title, body, images)
}

fn append_panel_block(
    target: &mut Vec<RenderedLine>,
    title: &str,
    body_lines: &[String],
    width: usize,
) {
    if width < 6 {
        target.push(RenderedLine::Text(truncate_to_width(title, width)));
        for line in body_lines {
            target.push(RenderedLine::Text(truncate_to_width(line, width)));
        }
        return;
    }

    let inner_width = width.saturating_sub(4).max(1);
    let title = truncate_to_width(title, inner_width.saturating_sub(1).max(1));
    let filler = "─".repeat(inner_width.saturating_sub(visible_width(&title) + 1));
    target.push(RenderedLine::Text(format!(
        "{}{}{}{}{}",
        style_border("╭─ "),
        title,
        style_border(" "),
        style_border(&filler),
        style_border("╮"),
    )));

    let body_lines = if body_lines.is_empty() {
        vec![String::new()]
    } else {
        body_lines.to_vec()
    };
    for line in body_lines {
        target.push(RenderedLine::Text(format!(
            "{} {} {}",
            style_border("│"),
            fit_line(&line, inner_width as u16),
            style_border("│"),
        )));
    }

    target.push(RenderedLine::Text(style_border(&format!(
        "╰{}╯",
        "─".repeat(width.saturating_sub(2))
    ))));
}

fn append_tool_surface_block(
    target: &mut Vec<RenderedLine>,
    title: &str,
    body_lines: &[String],
    width: usize,
    dimmed: bool,
) {
    let body_lines = if body_lines.is_empty() {
        vec![String::new()]
    } else {
        body_lines.to_vec()
    };
    if !title.is_empty() {
        let title_line = truncate_to_width(title, width);
        target.push(RenderedLine::Text(if dimmed {
            style_dim(&title_line)
        } else {
            style_tool_title(&title_line)
        }));
    }
    for line in body_lines {
        let content = fit_line(&line, width as u16);
        target.push(RenderedLine::Text(if dimmed {
            style_dim(&content)
        } else {
            content
        }));
    }
}

fn append_user_message_block(target: &mut Vec<RenderedLine>, body_lines: &[String], width: usize) {
    if width < 4 {
        let fallback = if body_lines.is_empty() {
            vec![String::new()]
        } else {
            body_lines.to_vec()
        };
        for line in fallback {
            target.push(RenderedLine::Text(style_user_surface(&truncate_to_width(
                &line, width,
            ))));
        }
        return;
    }

    let inner_width = width.saturating_sub(2).max(1);
    target.push(RenderedLine::Text(style_user_surface(&" ".repeat(width))));
    let body_lines = if body_lines.is_empty() {
        vec![String::new()]
    } else {
        body_lines.to_vec()
    };
    for line in body_lines {
        let padded = format!(" {} ", fit_line(&line, inner_width as u16));
        target.push(RenderedLine::Text(style_user_surface(&padded)));
    }
    target.push(RenderedLine::Text(style_user_surface(&" ".repeat(width))));
}

fn append_custom_surface_block(
    target: &mut Vec<RenderedLine>,
    body_lines: &[String],
    width: usize,
) {
    if width < 4 {
        let fallback = if body_lines.is_empty() {
            vec![String::new()]
        } else {
            body_lines.to_vec()
        };
        for line in fallback {
            target.push(RenderedLine::Text(style_custom_surface(
                &truncate_to_width(&line, width),
            )));
        }
        return;
    }

    let inner_width = width.saturating_sub(2).max(1);
    target.push(RenderedLine::Text(style_custom_surface(&" ".repeat(width))));
    let body_lines = if body_lines.is_empty() {
        vec![String::new()]
    } else {
        body_lines.to_vec()
    };
    for line in body_lines {
        let padded = format!(" {} ", fit_line(&line, inner_width as u16));
        target.push(RenderedLine::Text(style_custom_surface(&padded)));
    }
    target.push(RenderedLine::Text(style_custom_surface(&" ".repeat(width))));
}

fn collapse_panel_body(
    body_lines: &[String],
    locked_prefix: usize,
    preview_lines: usize,
    take_from_end: bool,
    expanded: bool,
    expand_hint: &str,
) -> Vec<String> {
    if body_lines.len() <= locked_prefix {
        return body_lines.to_vec();
    }

    let locked_prefix = locked_prefix.min(body_lines.len());
    let fixed = &body_lines[..locked_prefix];
    let variable = &body_lines[locked_prefix..];
    if variable.len() <= preview_lines {
        return body_lines.to_vec();
    }

    let mut out = fixed.to_vec();
    if expanded {
        out.extend_from_slice(variable);
        return out;
    }

    let hidden = variable.len().saturating_sub(preview_lines);
    if take_from_end {
        out.push(style_hint(&format!(
            "... {hidden} earlier lines ({expand_hint} to expand)"
        )));
        out.extend_from_slice(&variable[variable.len().saturating_sub(preview_lines)..]);
    } else {
        out.extend_from_slice(&variable[..preview_lines]);
        out.push(style_hint(&format!(
            "... {hidden} more lines ({expand_hint} to expand)"
        )));
    }
    out
}

fn tool_preview_config(tool_name: &str) -> (usize, bool) {
    match tool_name {
        "bash" => (5, true),
        "ls" | "find" => (20, false),
        "grep" => (15, false),
        _ => (10, false),
    }
}

fn tool_notice_lines(result: &pi_rust_ai_core::ToolResultMessage) -> Vec<String> {
    let Some(details) = result.details.as_ref() else {
        return Vec::new();
    };

    let mut notices = Vec::new();
    if details.get("truncation").is_some() {
        notices.push(style_warning("Output truncated"));
    }
    if let Some(path) = details.get("fullOutputPath").and_then(Value::as_str) {
        notices.push(style_hint(&format!("Full output: {path}")));
    }
    if let Some(limit) = details.get("entryLimitReached").and_then(Value::as_u64) {
        notices.push(style_warning(&format!("Entry limit reached: {limit}")));
    }
    if let Some(limit) = details.get("resultLimitReached").and_then(Value::as_u64) {
        notices.push(style_warning(&format!("Result limit reached: {limit}")));
    }
    if let Some(limit) = details.get("matchLimitReached").and_then(Value::as_u64) {
        notices.push(style_warning(&format!("Match limit reached: {limit}")));
    }
    if details
        .get("linesTruncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        notices.push(style_warning("Some lines were truncated"));
    }
    notices
}

fn append_markdown_block(target: &mut Vec<RenderedLine>, text: &str, width: usize) {
    for line in collect_markdown_lines(text, width) {
        target.push(RenderedLine::Text(line));
    }
}

fn collect_markdown_lines(text: &str, width: usize) -> Vec<String> {
    let mut in_code_block = false;
    let mut lines = Vec::new();
    for raw_line in text.replace('\t', "   ").lines() {
        let trimmed = raw_line.trim_end();
        let compact = trimmed.trim();
        if compact.is_empty() {
            lines.push(String::new());
            continue;
        }
        if compact.starts_with("```") {
            in_code_block = !in_code_block;
            lines.push(style_code_block_border(compact));
            continue;
        }

        if in_code_block || raw_line.starts_with("    ") {
            lines.extend(wrap_with_prefix(
                &style_code_block_line(compact),
                "  ",
                width,
            ));
            continue;
        }

        if is_markdown_rule(compact) {
            lines.push(style_md_hr(&"─".repeat(width.min(80).max(3))));
            continue;
        }

        if let Some((level, heading_text)) = parse_heading(compact) {
            let rendered = render_inline_markdown(heading_text);
            lines.push(style_markdown_heading(level, &rendered));
            continue;
        }

        if let Some((quote_depth, quote_text)) = parse_blockquote(compact) {
            let prefix = style_quote_border(&format!("{} ", "│".repeat(quote_depth.max(1))));
            let quote_lines = wrap_text(
                &render_inline_markdown(quote_text),
                width.saturating_sub(quote_depth + 1),
            );
            for line in quote_lines {
                lines.push(format!("{prefix}{}", style_quote_text(&line)));
            }
            continue;
        }

        if let Some((prefix, rest)) = parse_list_item(raw_line) {
            lines.extend(wrap_with_prefix(
                &render_inline_markdown(rest),
                &style_list_bullet(&prefix),
                width,
            ));
            continue;
        }

        if compact.starts_with("@@") || compact.starts_with('+') || compact.starts_with('-') {
            lines.push(truncate_to_width(compact, width));
            continue;
        }

        if compact.starts_with('|') {
            lines.push(style_dim(&truncate_to_width(compact, width)));
            continue;
        }

        lines.extend(wrap_text(&render_inline_markdown(compact), width));
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn collect_literal_lines(text: &str, width: usize, style: fn(&str) -> String) -> Vec<String> {
    let mut lines = Vec::new();
    for raw_line in text.replace('\t', "   ").lines() {
        if raw_line.is_empty() {
            lines.push(String::new());
        } else {
            lines.push(style(&truncate_to_width(raw_line, width)));
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn collect_code_block_lines(lines: &[String], width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in lines {
        if raw_line.is_empty() {
            out.push(String::new());
        } else {
            out.push(style_code_block_line(&truncate_to_width(raw_line, width)));
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn collect_bash_result_lines(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for raw_line in text.lines() {
        if raw_line.is_empty() {
            lines.push(String::new());
        } else if raw_line.starts_with("Exit code: 0") {
            continue;
        } else if raw_line.starts_with("Exit code:") || raw_line.starts_with("Command cancelled") {
            let status = if raw_line.starts_with("Exit code:") {
                let code = raw_line.trim_start_matches("Exit code:").trim();
                format!("(exit {code})")
            } else {
                "(cancelled)".to_string()
            };
            lines.push(style_warning(&truncate_to_width(&status, width)));
        } else if raw_line.starts_with("Full output:") {
            lines.push(style_hint(&truncate_to_width(raw_line, width)));
        } else {
            lines.push(style_dim(&truncate_to_width(raw_line, width)));
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[derive(Clone, Debug)]
struct ParsedDiffLine {
    prefix: char,
    line_num: String,
    content: String,
}

fn parse_diff_line(line: &str) -> Option<ParsedDiffLine> {
    let prefix = line.chars().next()?;
    if !matches!(prefix, '+' | '-' | ' ') {
        return None;
    }
    let rest = &line[prefix.len_utf8()..];
    let separator = rest.find(' ')?;
    let line_num = &rest[..separator];
    if !line_num
        .chars()
        .all(|ch| ch.is_ascii_digit() || ch.is_ascii_whitespace())
    {
        return None;
    }
    Some(ParsedDiffLine {
        prefix,
        line_num: line_num.to_string(),
        content: rest[separator + 1..].replace('\t', "   "),
    })
}

fn render_intra_line_diff(old_content: &str, new_content: &str) -> (String, String) {
    let diff = TextDiff::from_words(old_content, new_content);
    let mut removed_line = String::new();
    let mut added_line = String::new();
    let mut is_first_removed = true;
    let mut is_first_added = true;

    for change in diff.iter_all_changes() {
        let mut value = change.to_string();
        match change.tag() {
            ChangeTag::Delete => {
                if is_first_removed {
                    let trimmed = value.trim_start_matches(char::is_whitespace).to_string();
                    removed_line.push_str(&value[..value.len().saturating_sub(trimmed.len())]);
                    value = trimmed;
                    is_first_removed = false;
                }
                if !value.is_empty() {
                    removed_line.push_str(&style_diff_highlight(&value));
                }
            }
            ChangeTag::Insert => {
                if is_first_added {
                    let trimmed = value.trim_start_matches(char::is_whitespace).to_string();
                    added_line.push_str(&value[..value.len().saturating_sub(trimmed.len())]);
                    value = trimmed;
                    is_first_added = false;
                }
                if !value.is_empty() {
                    added_line.push_str(&style_diff_highlight(&value));
                }
            }
            ChangeTag::Equal => {
                removed_line.push_str(&value);
                added_line.push_str(&value);
            }
        }
    }

    (removed_line, added_line)
}

fn style_diff_removed_line(text: &str) -> String {
    apply_persistent_style("38;5;203", text)
}

fn style_diff_added_line(text: &str) -> String {
    apply_persistent_style("38;5;78", text)
}

fn style_diff_context_line(text: &str) -> String {
    apply_persistent_style("38;5;244", text)
}

fn style_diff_highlight(text: &str) -> String {
    ansi("7", text)
}

fn format_diff_line(parsed: &ParsedDiffLine, content: &str) -> String {
    format!("{}{} {}", parsed.prefix, parsed.line_num, content)
}

pub(super) fn collect_diff_lines(diff: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let diff_lines = diff.lines().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < diff_lines.len() {
        let rendered = diff_lines[index].replace('\t', "   ");
        let Some(parsed) = parse_diff_line(&rendered) else {
            let styled = if rendered.starts_with("@@") {
                style_warning(&rendered)
            } else if rendered.starts_with("diff --git")
                || rendered.starts_with("index ")
                || rendered.starts_with("--- ")
                || rendered.starts_with("+++ ")
            {
                style_subtitle(&rendered)
            } else {
                style_diff_context_line(&rendered)
            };
            lines.push(truncate_to_width(&styled, width));
            index += 1;
            continue;
        };

        if parsed.prefix == '-' {
            let mut removed = vec![parsed.clone()];
            index += 1;
            while index < diff_lines.len() {
                let next = diff_lines[index].replace('\t', "   ");
                match parse_diff_line(&next) {
                    Some(next_parsed) if next_parsed.prefix == '-' => {
                        removed.push(next_parsed);
                        index += 1;
                    }
                    _ => break,
                }
            }

            let mut added = Vec::new();
            while index < diff_lines.len() {
                let next = diff_lines[index].replace('\t', "   ");
                match parse_diff_line(&next) {
                    Some(next_parsed) if next_parsed.prefix == '+' => {
                        added.push(next_parsed);
                        index += 1;
                    }
                    _ => break,
                }
            }

            if removed.len() == 1 && added.len() == 1 {
                let (removed_content, added_content) =
                    render_intra_line_diff(&removed[0].content, &added[0].content);
                lines.push(truncate_to_width(
                    &style_diff_removed_line(&format_diff_line(&removed[0], &removed_content)),
                    width,
                ));
                lines.push(truncate_to_width(
                    &style_diff_added_line(&format_diff_line(&added[0], &added_content)),
                    width,
                ));
            } else {
                for removed_line in removed {
                    lines.push(truncate_to_width(
                        &style_diff_removed_line(&format_diff_line(
                            &removed_line,
                            &removed_line.content,
                        )),
                        width,
                    ));
                }
                for added_line in added {
                    lines.push(truncate_to_width(
                        &style_diff_added_line(&format_diff_line(&added_line, &added_line.content)),
                        width,
                    ));
                }
            }
            continue;
        }

        let styled = if parsed.prefix == '+' {
            style_diff_added_line(&format_diff_line(&parsed, &parsed.content))
        } else {
            style_diff_context_line(&format_diff_line(&parsed, &parsed.content))
        };
        lines.push(truncate_to_width(&styled, width));
        index += 1;
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn append_tool_call_block(
    target: &mut Vec<RenderedLine>,
    name: &str,
    arguments: &Value,
    width: usize,
) {
    let inner_width = width.saturating_sub(4).max(1);
    let body = build_tool_call_body(name, arguments, inner_width);
    append_tool_surface_block(
        target,
        &tool_panel_title(name, Some(arguments), None, None),
        &body,
        width,
        false,
    );
}

fn append_thinking_block(target: &mut Vec<RenderedLine>, text: &str, width: usize) {
    for line in collect_markdown_lines(text, width) {
        if line.is_empty() {
            target.push(RenderedLine::Text(String::new()));
        } else {
            target.push(RenderedLine::Text(style_thinking_surface(&line)));
        }
    }
}

fn build_tool_call_body(tool_name: &str, arguments: &Value, width: usize) -> Vec<String> {
    match tool_name {
        "write" => build_write_call_lines(arguments, width),
        "edit" => build_edit_call_lines(arguments, width),
        "read" => build_read_call_lines(arguments, width),
        _ => {
            let pretty =
                serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string());
            let mut body = Vec::new();
            for line in pretty.lines() {
                body.push(style_dim(&truncate_to_width(line, width)));
            }
            body
        }
    }
}

fn build_read_call_lines(arguments: &Value, _width: usize) -> Vec<String> {
    let mut body = Vec::new();
    let offset = arguments.get("offset").and_then(Value::as_u64).unwrap_or(1);
    let limit = arguments.get("limit").and_then(Value::as_u64);
    if offset > 1 || limit.is_some() {
        let range = match limit {
            Some(limit) if limit > 0 => format!("lines {offset}-{}", offset + limit - 1),
            _ => format!("lines {offset}+"),
        };
        body.push(style_hint(&range));
    }
    body
}

fn build_write_call_lines(arguments: &Value, width: usize) -> Vec<String> {
    let mut body = Vec::new();
    let Some(content) = tool_argument_string(Some(arguments), &["content"]) else {
        body.push(style_error("[invalid content arg - expected string]"));
        return body;
    };
    if content.is_empty() {
        body.push(style_hint("(empty file)"));
        return body;
    }

    let preview = content
        .replace('\t', "   ")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let preview_limit = 10usize;
    body.extend(collect_code_block_lines(
        &preview
            .iter()
            .take(preview_limit)
            .cloned()
            .collect::<Vec<_>>(),
        width,
    ));
    if preview.len() > preview_limit {
        body.push(style_hint(&format!(
            "... {} more lines (wait for tool output or ctrl+o later)",
            preview.len() - preview_limit
        )));
    }
    body
}

fn build_edit_call_lines(arguments: &Value, width: usize) -> Vec<String> {
    let mut body = Vec::new();
    let old_text = tool_argument_string(Some(arguments), &["oldText"]);
    let new_text = tool_argument_string(Some(arguments), &["newText"]);
    let (Some(old_text), Some(new_text)) = (old_text, new_text) else {
        let pretty =
            serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string());
        for line in pretty.lines() {
            body.push(style_dim(&truncate_to_width(line, width)));
        }
        return body;
    };

    body.extend(collect_edit_call_preview_lines(
        old_text, new_text, width, 8,
    ));
    body
}

fn collect_edit_call_preview_lines(
    old_text: &str,
    new_text: &str,
    width: usize,
    preview_limit: usize,
) -> Vec<String> {
    let old_lines = old_text
        .replace('\t', "   ")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let new_lines = new_text
        .replace('\t', "   ")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let mut lines = Vec::new();
    let mut old_take = if !old_lines.is_empty() && !new_lines.is_empty() {
        (preview_limit / 2).max(1).min(old_lines.len())
    } else {
        preview_limit.min(old_lines.len())
    };
    let mut new_take = preview_limit.saturating_sub(old_take).min(new_lines.len());
    if new_take == 0 && !new_lines.is_empty() && old_take > 1 {
        old_take -= 1;
        new_take = 1;
    }

    for line in old_lines.iter().take(old_take) {
        lines.push(style_error(&truncate_to_width(&format!("- {line}"), width)));
    }
    for line in new_lines.iter().take(new_take) {
        lines.push(style_success(&truncate_to_width(
            &format!("+ {line}"),
            width,
        )));
    }
    let hidden = old_lines
        .len()
        .saturating_add(new_lines.len())
        .saturating_sub(old_take + new_take);
    if hidden > 0 {
        lines.push(style_hint(&format!(
            "... {} more lines (wait for tool output or ctrl+o later)",
            hidden
        )));
    }
    if lines.is_empty() {
        lines.push(style_hint("(no visible diff preview)"));
    }
    lines
}

fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let bytes = line.as_bytes();
    let mut count = 0usize;
    while count < bytes.len() && bytes[count] == b'#' {
        count += 1;
    }
    if count == 0 || count > 6 || bytes.get(count) != Some(&b' ') {
        return None;
    }
    Some((count, line[count + 1..].trim()))
}

fn parse_blockquote(line: &str) -> Option<(usize, &str)> {
    let mut rest = line.trim_start();
    let mut depth = 0usize;
    while let Some(stripped) = rest.strip_prefix('>') {
        depth += 1;
        rest = stripped.trim_start();
    }
    if depth == 0 {
        None
    } else {
        Some((depth, rest))
    }
}

fn parse_list_item(line: &str) -> Option<(String, &str)> {
    let indent = line.chars().take_while(|ch| *ch == ' ').count();
    let trimmed = &line[indent..];
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return Some((
                format!("{}{}", " ".repeat(indent), bullet),
                rest.trim_start(),
            ));
        }
    }

    let digit_count = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count > 0
        && trimmed.chars().nth(digit_count).is_some_and(|ch| ch == '.')
        && trimmed
            .chars()
            .nth(digit_count + 1)
            .is_some_and(|ch| ch == ' ')
    {
        let prefix = &trimmed[..digit_count + 2];
        let rest = trimmed[digit_count + 2..].trim_start();
        return Some((format!("{}{}", " ".repeat(indent), prefix), rest));
    }

    None
}

fn is_markdown_rule(line: &str) -> bool {
    let compact = line.trim();
    compact.len() >= 3 && compact.chars().all(|ch| matches!(ch, '-' | '_' | '*'))
}

fn wrap_with_prefix(text: &str, prefix: &str, width: usize) -> Vec<String> {
    let available = width.saturating_sub(visible_width(prefix)).max(1);
    let wrapped = wrap_text(text, available);
    let continuation = " ".repeat(visible_width(prefix));
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("{prefix}{line}")
            } else {
                format!("{continuation}{line}")
            }
        })
        .collect()
}

fn render_inline_markdown(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    render_inline_markdown_slice(&chars)
}

fn render_inline_markdown_slice(chars: &[char]) -> String {
    let mut out = String::new();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] == '\\' && index + 1 < chars.len() {
            out.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if chars[index] == '`'
            && let Some(end) = find_char(chars, index + 1, '`')
        {
            let inner = chars[index + 1..end].iter().collect::<String>();
            out.push_str(&style_inline_code(&inner));
            index = end + 1;
            continue;
        }
        if chars[index] == '['
            && let Some(close_bracket) = find_char(chars, index + 1, ']')
            && chars.get(close_bracket + 1) == Some(&'(')
            && let Some(close_paren) = find_char(chars, close_bracket + 2, ')')
        {
            let label = render_inline_markdown_slice(&chars[index + 1..close_bracket]);
            let url = chars[close_bracket + 2..close_paren]
                .iter()
                .collect::<String>();
            let plain_label = chars[index + 1..close_bracket].iter().collect::<String>();
            if plain_label == url {
                out.push_str(&style_markdown_link(&label));
            } else {
                out.push_str(&style_markdown_link(&label));
                out.push_str(&style_markdown_link_url(&format!(" ({url})")));
            }
            index = close_paren + 1;
            continue;
        }
        if chars[index..].starts_with(&['*', '*'])
            && let Some(end) = find_sequence(chars, index + 2, &['*', '*'])
        {
            let inner = render_inline_markdown_slice(&chars[index + 2..end]);
            out.push_str(&style_markdown_bold(&inner));
            index = end + 2;
            continue;
        }
        if chars[index..].starts_with(&['_', '_'])
            && let Some(end) = find_sequence(chars, index + 2, &['_', '_'])
        {
            let inner = render_inline_markdown_slice(&chars[index + 2..end]);
            out.push_str(&style_markdown_bold(&inner));
            index = end + 2;
            continue;
        }
        if chars[index..].starts_with(&['~', '~'])
            && let Some(end) = find_sequence(chars, index + 2, &['~', '~'])
        {
            let inner = render_inline_markdown_slice(&chars[index + 2..end]);
            out.push_str(&style_markdown_strikethrough(&inner));
            index = end + 2;
            continue;
        }
        if matches!(chars[index], '*' | '_')
            && let Some(end) = find_char(chars, index + 1, chars[index])
        {
            let inner = chars[index + 1..end].iter().collect::<String>();
            if !inner.trim().is_empty() {
                out.push_str(&style_markdown_italic(&render_inline_markdown(&inner)));
                index = end + 1;
                continue;
            }
        }

        out.push(chars[index]);
        index += 1;
    }
    out
}

fn find_char(chars: &[char], start: usize, needle: char) -> Option<usize> {
    chars[start..]
        .iter()
        .position(|candidate| *candidate == needle)
        .map(|offset| start + offset)
}

fn find_sequence(chars: &[char], start: usize, needle: &[char]) -> Option<usize> {
    if needle.is_empty() || start >= chars.len() || needle.len() > chars.len().saturating_sub(start)
    {
        return None;
    }

    chars[start..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

pub(super) fn apply_persistent_style(code: &str, text: &str) -> String {
    let mut styled = format!("\u{1b}[{code}m");
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        styled.push(ch);
        if ch != '\u{1b}' {
            continue;
        }
        if !matches!(chars.peek(), Some('[')) {
            continue;
        }
        while let Some(next) = chars.next() {
            styled.push(next);
            if next == 'm' {
                break;
            }
        }
        if styled.ends_with("[0m") {
            styled.push_str(&format!("\u{1b}[{code}m"));
        }
    }
    styled.push_str(ANSI_RESET);
    styled
}

fn append_prefixed_wrapped_text(
    target: &mut Vec<RenderedLine>,
    prefix: &str,
    text: &str,
    width: usize,
) {
    let effective_prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("{}: ", style_prefix(prefix))
    };
    let available = width.saturating_sub(visible_width(&effective_prefix));
    for (index, line) in wrap_text(text, available).into_iter().enumerate() {
        let rendered = if index == 0 {
            format!("{effective_prefix}{line}")
        } else if effective_prefix.is_empty() {
            line
        } else {
            format!("{}{}", " ".repeat(visible_width(&effective_prefix)), line)
        };
        target.push(RenderedLine::Text(rendered));
    }
}

fn append_image_block(
    target: &mut Vec<RenderedLine>,
    prefix: &str,
    mime_type: &str,
    data: Option<&str>,
    width: usize,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
) {
    append_prefixed_wrapped_text(
        target,
        prefix,
        &style_hint(&format!("[image: {mime_type}]")),
        width,
    );
    if show_images && terminal_capabilities.inline_images {
        target.push(RenderedLine::Image(pi_rust_tui::ImageLine {
            alt_text: mime_type.to_string(),
            mime_type: Some(mime_type.to_string()),
            data: data.map(ToOwned::to_owned),
        }));
    }
}

fn active_tool_panel_id(tool: &ActiveToolExecution) -> String {
    format!("active:{}", tool.tool_call_id)
}

fn tool_result_panel_id(result: &pi_rust_ai_core::ToolResultMessage) -> String {
    format!("result:{}", result.tool_call_id)
}

fn bash_panel_id(entry_index: usize) -> String {
    format!("bash:{entry_index}")
}

pub(super) fn latest_active_tool_panel_id(tools: &[ActiveToolExecution]) -> Option<String> {
    tools.last().map(active_tool_panel_id)
}

pub(super) fn latest_transcript_tool_panel_id(entries: &[TranscriptEntry]) -> Option<String> {
    entries
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, entry)| match entry {
            TranscriptEntry::Message(Message::ToolResult(result)) => {
                Some(tool_result_panel_id(result))
            }
            TranscriptEntry::CustomMessage { custom_type, .. }
                if custom_type == "bash_execution" =>
            {
                Some(bash_panel_id(index))
            }
            _ => None,
        })
}

fn should_expand_tool_panel(
    _panel_id: Option<&str>,
    tool_expand_mode: ToolExpandMode,
    _latest_tool_panel: Option<&str>,
) -> bool {
    match tool_expand_mode {
        ToolExpandMode::Collapsed => false,
        ToolExpandMode::All => true,
    }
}

pub(super) fn active_tool_render_lines(
    tools: &[ActiveToolExecution],
    width: u16,
    show_images: bool,
    terminal_capabilities: &TerminalCapabilities,
    tool_expand_mode: ToolExpandMode,
    latest_tool_panel: Option<&str>,
    expand_hint: &str,
) -> Vec<RenderedLine> {
    if tools.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let inner_width = width as usize;
    for (index, tool) in tools.iter().enumerate() {
        let title = if tool.tool_name == "bash" {
            let command = tool_argument_string(Some(&tool.args), &["command"]).unwrap_or_default();
            if command.is_empty() {
                "$".to_string()
            } else {
                format!("$ {}", truncate_to_width(command, 48))
            }
        } else {
            tool_panel_title(&tool.tool_name, Some(&tool.args), None, None)
        };
        let mut body = Vec::new();
        let mut images = Vec::new();
        if let Some(partial) = &tool.partial_result {
            collect_live_tool_partial_block(
                &mut body,
                &mut images,
                &tool.tool_name,
                partial,
                inner_width,
            );
        }
        if body.is_empty() {
            body.push(style_hint("Waiting for output..."));
        }
        let panel_id = active_tool_panel_id(tool);
        let collapsed_body = collapse_panel_body(
            &body,
            0,
            tool_preview_config(&tool.tool_name).0,
            tool_preview_config(&tool.tool_name).1,
            should_expand_tool_panel(Some(panel_id.as_str()), tool_expand_mode, latest_tool_panel),
            expand_hint,
        );
        append_tool_surface_block(&mut lines, &title, &collapsed_body, width as usize, false);
        for (mime_type, data) in images {
            append_image_block(
                &mut lines,
                "",
                &mime_type,
                data.as_deref(),
                width as usize,
                show_images,
                terminal_capabilities,
            );
        }
        if index + 1 < tools.len() {
            lines.push(RenderedLine::Text(String::new()));
        }
    }
    lines
}

fn collect_live_tool_partial_block(
    body: &mut Vec<String>,
    images: &mut Vec<(String, Option<String>)>,
    tool_name: &str,
    partial: &Value,
    width: usize,
) {
    if let Some(diff) = partial.get("diff").and_then(Value::as_str) {
        body.extend(collect_diff_lines(diff, width));
        return;
    }

    if let Some(output) = partial.get("output").and_then(Value::as_str) {
        body.extend(collect_markdown_lines(output, width));
        return;
    }

    if let Some(stderr) = partial.get("stderr").and_then(Value::as_str) {
        body.extend(collect_markdown_lines(stderr, width));
        return;
    }

    if let Some(text) = partial.as_str() {
        if tool_name == "bash" {
            body.extend(collect_markdown_lines(text, width));
        } else {
            body.extend(wrap_text(text, width));
        }
        return;
    }

    if let Some(content) = partial.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        body.extend(collect_markdown_lines(text, width));
                    }
                }
                Some("image") => {
                    if let Some(mime_type) = block.get("mimeType").and_then(Value::as_str) {
                        images.push((
                            mime_type.to_string(),
                            block
                                .get("data")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                        ));
                    }
                }
                _ => {}
            }
        }
        return;
    }

    let pretty = serde_json::to_string_pretty(partial).unwrap_or_else(|_| partial.to_string());
    for line in pretty.lines() {
        body.push(truncate_to_width(line, width));
    }
}

pub(super) fn content_text(content: &UserContent) -> String {
    match content {
        UserContent::Text(text) => text.clone(),
        UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                UserContentBlock::Text { text, .. } => Some(text.clone()),
                UserContentBlock::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
