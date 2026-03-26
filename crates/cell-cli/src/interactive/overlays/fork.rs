use super::super::*;
use super::base::{OverlaySelection, select_list_visible_bounds};

pub(crate) struct ForkOverlayState {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) hint: String,
    pub(crate) list: SelectList,
    pub(crate) selections: Vec<OverlaySelection>,
    pub(crate) messages: Vec<ForkableUserMessage>,
}

impl Component for ForkOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput::default();
        append_rule_line(&mut output.lines, width);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_title(&self.title)).render(width),
            false,
        );
        append_output(
            &mut output,
            Text::new(style_subtitle(&self.subtitle)).render(width),
            false,
        );
        append_blank_lines(&mut output, width, 1);
        append_fork_overlay_rows(&mut output.lines, self, width as usize);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_hint(&self.hint)).render(width),
            false,
        );
        append_blank_lines(&mut output, width, 1);
        append_rule_line(&mut output.lines, width);
        output
    }
}

pub(crate) fn append_fork_overlay_rows(
    target: &mut Vec<RenderedLine>,
    state: &ForkOverlayState,
    width: usize,
) {
    if state.messages.is_empty() {
        target.push(RenderedLine::Text(fit_line(
            &style_hint("  No messages to fork from"),
            width as u16,
        )));
        return;
    }

    let selected_index = state.list.selected_index();
    let (start, end) = select_list_visible_bounds(&state.list);
    let total = state.messages.len();
    for visible_index in start..end {
        let Some(message) = state.messages.get(visible_index) else {
            continue;
        };
        let is_selected = visible_index == selected_index;
        let prefix = if is_selected {
            style_brand("› ")
        } else {
            "  ".to_string()
        };
        let counter = style_hint(&format!(
            "Message {} of {}",
            message.index.saturating_add(1),
            total
        ));
        let header = fit_line(&format!("{prefix}{counter}"), width as u16);
        target.push(RenderedLine::Text(if is_selected {
            style_selected_row(&header)
        } else {
            header
        }));

        let body_width = width.saturating_sub(4).max(1);
        let preview = truncate_to_width(&fork_message_preview_text(&message.text), body_width);
        let body = fit_line(&format!("  {}", preview), width as u16);
        target.push(RenderedLine::Text(if is_selected {
            style_selected_row(&style_title(&body))
        } else {
            body
        }));

        if visible_index + 1 < end {
            target.push(RenderedLine::Text(String::new()));
        }
    }
}

pub(crate) fn fork_message_preview_text(text: &str) -> String {
    parse_skill_block(text)
        .and_then(|skill| {
            skill
                .user_message
                .filter(|value| !value.trim().is_empty())
                .or_else(|| Some(skill.name))
        })
        .unwrap_or_else(|| text.to_string())
        .replace('\n', " ")
}
