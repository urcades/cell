use super::super::*;
use super::session::{SessionNameFilter, SessionScope, SessionSortMode};
use super::tree::TreeFilterMode;

pub(crate) enum InputOverlayAction {
    RenameSession {
        path: PathBuf,
        selected_value: String,
        scope: SessionScope,
        sort_mode: SessionSortMode,
        name_filter: SessionNameFilter,
        show_path: bool,
        query: String,
    },
    EditTreeLabel {
        entry_id: String,
        selected_value: String,
        filter_mode: TreeFilterMode,
        query: String,
    },
    TreeSummaryCustomPrompt {
        entry_id: String,
        filter_mode: TreeFilterMode,
        query: String,
    },
}

pub(crate) struct InputOverlayState {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) message_lines: Vec<String>,
    pub(crate) hint: String,
    pub(crate) input: Input,
    pub(crate) action: InputOverlayAction,
}

impl Component for InputOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput {
            lines: Vec::new(),
            cursor: None,
        };
        append_overlay_banner(&mut output, &self.title, &self.subtitle, width);
        append_blank_lines(&mut output, width, 1);
        if !self.message_lines.is_empty() {
            append_output(
                &mut output,
                Text::new(self.message_lines.join("\n")).render(width),
                false,
            );
            append_blank_lines(&mut output, width, 1);
        }
        append_output(&mut output, self.input.render(width), true);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_hint(&self.hint)).render(width),
            false,
        );
        output
    }
}
