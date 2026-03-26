use super::*;

pub(super) fn append_rule_line(target: &mut Vec<RenderedLine>, width: u16) {
    if width == 0 {
        return;
    }
    target.push(RenderedLine::Text(style_border(
        &"─".repeat(width as usize),
    )));
}

pub(super) fn clip_render_output_to_height(mut output: RenderOutput, height: u16) -> RenderOutput {
    let max_lines = height as usize;
    if max_lines == 0 || output.lines.len() <= max_lines {
        return output;
    }

    let clip_start = output.lines.len().saturating_sub(max_lines);
    output.lines = output.lines.split_off(clip_start);
    output.cursor = output.cursor.and_then(|cursor| {
        (cursor.row as usize >= clip_start).then_some(CursorPosition {
            row: cursor.row.saturating_sub(clip_start as u16),
            col: cursor.col,
        })
    });
    output
}

pub(super) fn append_overlay_banner(
    target: &mut RenderOutput,
    title: &str,
    subtitle: &str,
    width: u16,
) {
    let width = width as usize;
    if width < 6 {
        append_output(
            target,
            Text::new(style_title(title)).render(width as u16),
            false,
        );
        if !subtitle.is_empty() {
            for line in subtitle.lines() {
                append_output(
                    target,
                    Text::new(style_subtitle(line)).render(width as u16),
                    false,
                );
            }
        }
        return;
    }

    let inner_width = width.max(1);
    let header_title = truncate_to_width(title, inner_width.saturating_sub(1).max(1));
    let filler = "─".repeat(inner_width.saturating_sub(visible_width(&header_title) + 1));
    target.lines.push(RenderedLine::Text(format!(
        "{}{}{}{}{}",
        style_border("╭─ "),
        style_title(&header_title),
        style_border(" "),
        style_border(&filler),
        style_border("╮"),
    )));
    for subtitle_line in subtitle.lines().filter(|line| !line.is_empty()) {
        target.lines.push(RenderedLine::Text(format!(
            "{} {} {}",
            style_border("│"),
            fit_line(&style_subtitle(subtitle_line), inner_width as u16),
            style_border("│"),
        )));
    }
    target.lines.push(RenderedLine::Text(style_border(&format!(
        "╰{}╯",
        "─".repeat(width.saturating_sub(2))
    ))));
}

pub(super) fn render_footer_panel(
    state: &RpcSessionState,
    stats: &RpcSessionStats,
    cwd: &Path,
    git_branch: Option<&str>,
    width: u16,
    _is_streaming: bool,
    available_provider_count: usize,
    using_oauth_subscription: bool,
    _pending_count: usize,
    _tool_expand_mode: ToolExpandMode,
) -> RenderOutput {
    let line_width = width as usize;
    let session_name = state
        .session_name
        .clone()
        .filter(|value| !value.trim().is_empty());
    let subtitle = footer_subtitle(cwd, git_branch, line_width);
    let header_line = if let Some(session_name) = session_name.as_deref() {
        format!(
            "{} {} {}",
            subtitle,
            style_dim("•"),
            style_subtitle(&truncate_to_width(session_name, line_width / 3))
        )
    } else {
        truncate_to_width(&subtitle, line_width)
    };
    let mut lines = Vec::new();
    lines.push(fit_line(
        &truncate_to_width(&header_line, line_width),
        width,
    ));

    let mut usage_segments = Vec::new();
    if stats.tokens.input > 0 {
        usage_segments.push(style_subtitle(&format!(
            "↑{}",
            format_token_count(stats.tokens.input)
        )));
    }
    if stats.tokens.output > 0 {
        usage_segments.push(style_subtitle(&format!(
            "↓{}",
            format_token_count(stats.tokens.output)
        )));
    }
    if stats.tokens.cache_read > 0 {
        usage_segments.push(style_subtitle(&format!(
            "R{}",
            format_token_count(stats.tokens.cache_read)
        )));
    }
    if stats.tokens.cache_write > 0 {
        usage_segments.push(style_subtitle(&format!(
            "W{}",
            format_token_count(stats.tokens.cache_write)
        )));
    }
    if stats.cost > 0.0 || using_oauth_subscription {
        let mut cost = format_cost(stats.cost);
        if using_oauth_subscription {
            cost.push_str(" (sub)");
        }
        usage_segments.push(style_subtitle(&cost));
    }
    let mut context_usage = footer_context_usage(state, stats);
    if state.auto_compaction_enabled {
        context_usage.push(' ');
        context_usage.push_str(&style_subtitle("(auto)"));
    }
    usage_segments.push(context_usage);
    let status_line = state
        .model
        .as_ref()
        .map(|model| {
            let mut label = String::new();
            if available_provider_count > 1 {
                label.push_str(&style_hint(&format!("({}) ", model.provider.0)));
            }
            label.push_str(&style_subtitle(&model.id));
            if model.reasoning {
                let thinking_level = normalized_thinking_level(&state.thinking_level);
                label.push_str(&format!(
                    " {} {}",
                    style_dim("•"),
                    style_subtitle(if thinking_level == "off" {
                        "thinking off"
                    } else {
                        thinking_level
                    })
                ));
            }
            label
        })
        .unwrap_or_else(|| style_hint("no-model"));
    lines.push(align_footer_row(
        &usage_segments.join(" "),
        &status_line,
        line_width,
    ));

    Text::new(lines.join("\n")).render(width)
}

pub(super) fn render_prompt_panel(
    editor: &Editor,
    autocomplete: Option<&PromptAutocompleteState>,
    state: &RpcSessionState,
    _keybindings: &KeybindingsManager,
    width: u16,
    max_visible_lines: Option<usize>,
    _is_streaming: bool,
    _pending_count: usize,
) -> RenderOutput {
    let mut editor = editor.clone();
    editor.set_prompt("");
    let prompt_text = editor.get_text();
    let rules = prompt_composer_border_rules(&prompt_text, &state.thinking_level, width as usize);
    editor.render_composer_with_rules(
        width,
        max_visible_lines,
        pi_rust_tui::ComposerBorderRules::new(&rules.0, &rules.1),
        autocomplete.map(|autocomplete| render_prompt_autocomplete(autocomplete, width)),
    )
}

fn display_shortcut(display: String) -> String {
    display
        .replace("ctrl+shift+", "shift+ctrl+")
        .replace("Ctrl+Shift+", "Shift+Ctrl+")
}

pub(super) fn display_app_shortcut(keybindings: &KeybindingsManager, action: AppAction) -> String {
    display_shortcut(keybindings.display(action))
}

pub(super) fn display_editor_shortcut(
    keybindings: &KeybindingsManager,
    action: EditorAction,
) -> String {
    display_shortcut(keybindings.display_editor(action))
}

pub(super) fn prompt_composer_border_rules(
    prompt_text: &str,
    thinking_level: &str,
    width: usize,
) -> (String, String) {
    let rule = "─".repeat(width);
    let bash_mode = prompt_text.trim_start().starts_with('!');
    let border = if bash_mode {
        style_bash_border_rule(&rule)
    } else {
        style_thinking_border_rule(thinking_level, &rule)
    };
    (border.clone(), border)
}

pub(super) fn normalized_thinking_level(level: &str) -> &str {
    match level.trim() {
        "" => "off",
        value => value,
    }
}

pub(super) fn startup_hint_lines(keybindings: &KeybindingsManager) -> Vec<String> {
    vec![
        style_hint("escape to interrupt"),
        style_hint(&format!(
            "{} to clear",
            display_app_shortcut(keybindings, AppAction::Clear)
        )),
        style_hint(&format!(
            "{} twice to exit",
            display_app_shortcut(keybindings, AppAction::Clear)
        )),
        style_hint(&format!(
            "{} to exit (empty)",
            display_app_shortcut(keybindings, AppAction::Exit)
        )),
        style_hint(&format!(
            "{} to suspend",
            display_app_shortcut(keybindings, AppAction::Suspend)
        )),
        style_hint(&format!(
            "{} to delete to end",
            display_editor_shortcut(keybindings, EditorAction::DeleteToLineEnd)
        )),
        style_hint(&format!(
            "{} to cycle thinking level",
            display_app_shortcut(keybindings, AppAction::CycleThinkingLevel)
        )),
        style_hint(&format!(
            "{}/{} to cycle models",
            display_app_shortcut(keybindings, AppAction::CycleModelForward),
            display_app_shortcut(keybindings, AppAction::CycleModelBackward)
        )),
        style_hint(&format!(
            "{} to select model",
            display_app_shortcut(keybindings, AppAction::SelectModel)
        )),
        style_hint(&format!(
            "{} to expand tools",
            display_app_shortcut(keybindings, AppAction::ExpandTools)
        )),
        style_hint(&format!(
            "{} to expand thinking",
            display_app_shortcut(keybindings, AppAction::ToggleThinking)
        )),
        style_hint(&format!(
            "{} for external editor",
            display_app_shortcut(keybindings, AppAction::ExternalEditor)
        )),
        style_hint("/ for commands"),
        style_hint("! to run bash"),
        style_hint("!! to run bash (no context)"),
        style_hint(&format!(
            "{} to queue follow-up",
            display_app_shortcut(keybindings, AppAction::FollowUp)
        )),
        style_hint(&format!(
            "{} to edit all queued messages",
            display_app_shortcut(keybindings, AppAction::Dequeue)
        )),
        style_hint(&format!(
            "{} to paste image",
            display_app_shortcut(keybindings, AppAction::PasteImage)
        )),
        style_hint("drop files to attach"),
    ]
}

pub(super) fn startup_notice_lines(
    no_models_available: bool,
    notices: &[String],
    width: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    if no_models_available {
        lines.push(truncate_to_width(
            &style_warning("No models available. Set ANTHROPIC_API_KEY or configure models.json."),
            width,
        ));
    }
    lines.extend(notices.iter().flat_map(|notice| {
        wrap_text(notice, width)
            .into_iter()
            .map(|line| truncate_to_width(&style_warning(&line), width))
            .collect::<Vec<_>>()
    }));
    lines
}

pub(super) fn startup_resource_lines(
    context_files: &[String],
    summary: &StartupResourceSummary,
    width: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    append_startup_resource_section(&mut lines, "[Context]", context_files, width);
    append_startup_resource_path_section(&mut lines, "[Skills]", &summary.skills, width);
    append_startup_resource_path_section(&mut lines, "[Prompts]", &summary.prompts, width);
    let extension_lines = if summary.extension_summaries.is_empty() {
        summary
            .extensions
            .iter()
            .map(|path| shorten_home_path(&path.to_string_lossy()))
            .collect::<Vec<_>>()
    } else {
        summary.extension_summaries.clone()
    };
    append_startup_resource_section(&mut lines, "[Extensions]", &extension_lines, width);
    append_startup_resource_path_section(&mut lines, "[Themes]", &summary.themes, width);

    let rendered_notices = if summary.notices.is_empty() {
        &summary.conflicts
    } else {
        &summary.notices
    };

    if !rendered_notices.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        for section in [
            StartupResourceNoticeSection::Context,
            StartupResourceNoticeSection::Skill,
            StartupResourceNoticeSection::Prompt,
            StartupResourceNoticeSection::Theme,
            StartupResourceNoticeSection::Extension,
            StartupResourceNoticeSection::Resource,
        ] {
            let notices = rendered_notices
                .iter()
                .filter(|notice| notice.section == section)
                .collect::<Vec<_>>();
            if notices.is_empty() {
                continue;
            }
            lines.push(truncate_to_width(
                &style_warning(&format!("[{}]", section.heading())),
                width,
            ));
            for notice in notices {
                lines.push(truncate_to_width(
                    &style_hint(&format!(
                        "  {}: {}",
                        shorten_home_path(&notice.path.to_string_lossy()),
                        notice.message
                    )),
                    width,
                ));
            }
            lines.push(String::new());
        }
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
    }

    lines
}

fn append_startup_resource_section(
    lines: &mut Vec<String>,
    title: &str,
    entries: &[String],
    width: usize,
) {
    if entries.is_empty() {
        return;
    }
    if !lines.is_empty() {
        lines.push(String::new());
    }
    lines.push(truncate_to_width(
        &style_startup_section_title(title),
        width,
    ));
    lines.extend(
        entries
            .iter()
            .map(|path| truncate_to_width(&style_hint(&format!("  {path}")), width)),
    );
}

fn append_startup_resource_path_section(
    lines: &mut Vec<String>,
    title: &str,
    entries: &[PathBuf],
    width: usize,
) {
    let display = entries
        .iter()
        .map(|path| shorten_home_path(&path.to_string_lossy()))
        .collect::<Vec<_>>();
    append_startup_resource_section(lines, title, &display, width);
}

pub(super) fn composer_max_visible_lines(height: u16) -> usize {
    ((height as usize * 3) / 10).max(5)
}

pub(super) fn render_prompt_autocomplete(
    autocomplete: &PromptAutocompleteState,
    width: u16,
) -> RenderOutput {
    let mut output = RenderOutput::default();
    append_output(
        &mut output,
        Text::new(style_title(&autocomplete.title)).render(width),
        false,
    );
    append_output(
        &mut output,
        Text::new(style_subtitle(&autocomplete.subtitle)).render(width),
        false,
    );
    append_blank_lines(&mut output, width, 1);
    append_output(&mut output, autocomplete.list.render(width), false);
    append_blank_lines(&mut output, width, 1);
    append_output(
        &mut output,
        Text::new(style_hint(&autocomplete.hint)).render(width),
        false,
    );
    output
}

pub(super) fn footer_subtitle(cwd: &Path, git_branch: Option<&str>, width: usize) -> String {
    let cwd_display = shorten_home_path(&cwd.to_string_lossy());
    let plain = if let Some(branch) = git_branch.filter(|branch| !branch.is_empty()) {
        format!("{cwd_display} ({branch})")
    } else {
        cwd_display
    };
    style_subtitle(&truncate_to_width(&plain, width))
}

pub(super) fn footer_context_usage(state: &RpcSessionState, stats: &RpcSessionStats) -> String {
    let context_window = footer_context_window(state);
    if context_window == 0 {
        return format!("{}/{}", style_hint("?"), style_subtitle("0"));
    }
    let percent = (stats.tokens.total as f64 / context_window as f64) * 100.0;
    let styled = if percent >= 90.0 {
        style_error(&format!("{percent:.1}%"))
    } else if percent >= 70.0 {
        style_warning(&format!("{percent:.1}%"))
    } else {
        style_success(&format!("{percent:.1}%"))
    };
    format!(
        "{}/{}",
        styled,
        style_subtitle(&format_token_count(context_window))
    )
}

pub(super) fn footer_context_window(state: &RpcSessionState) -> u64 {
    let Some(model) = state.model.as_ref() else {
        return 0;
    };
    match (model.provider.0.as_str(), model.id.as_str()) {
        ("openai", "gpt-4.1" | "gpt-4.1-mini" | "gpt-4.1-nano") => 1_047_576,
        _ => model.context_window as u64,
    }
}

pub(super) fn align_footer_row(left: &str, right: &str, width: usize) -> String {
    if right.is_empty() {
        return truncate_to_width(left, width);
    }
    let left_width = visible_width(left);
    let right_width = visible_width(right);
    if left_width + right_width + 1 <= width {
        return format!(
            "{left}{}{right}",
            " ".repeat(width.saturating_sub(left_width + right_width))
        );
    }
    if right_width >= width {
        return truncate_to_width(right, width);
    }
    let left_max = width.saturating_sub(right_width + 1);
    format!("{} {right}", truncate_to_width(left, left_max))
}

pub(super) fn format_token_count(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    if value < 10_000 {
        return format!("{:.1}k", value as f64 / 1_000.0);
    }
    if value < 1_000_000 {
        return format!("{}k", (value as f64 / 1_000.0).round() as u64);
    }
    if value < 10_000_000 {
        return format!("{:.1}M", value as f64 / 1_000_000.0);
    }
    format!("{}M", (value as f64 / 1_000_000.0).round() as u64)
}

pub(super) fn format_cost(cost: f64) -> String {
    if cost >= 1.0 {
        format!("${cost:.2}")
    } else if cost > 0.0 {
        format!("${cost:.3}")
    } else {
        "$0.00".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_rust_core::StartupResourceNotice;

    #[test]
    fn startup_resource_lines_render_extension_summaries_and_warnings() {
        let summary = StartupResourceSummary {
            extension_summaries: vec![
                "Good Plugin [good] v1 - 1 command, 1 tool - /tmp/packages/good/pi-plugin-host.json"
                    .to_string(),
            ],
            notices: vec![StartupResourceNotice {
                section: StartupResourceNoticeSection::Extension,
                path: PathBuf::from("/tmp/packages/good/pi-plugin-host.json"),
                message: "Good Plugin [good]: plugin did not respond within 50ms".to_string(),
            }],
            ..Default::default()
        };

        let rendered = startup_resource_lines(&[], &summary, 120).join("\n");

        assert!(rendered.contains("[Extensions]"));
        assert!(rendered.contains("Good Plugin [good] v1"));
        assert!(rendered.contains("Extension warnings"));
        assert!(rendered.contains("plugin did not respond within 50ms"));
    }
}
