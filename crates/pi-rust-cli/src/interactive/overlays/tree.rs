use super::super::*;

pub(crate) struct TreeSummaryOverlayState {
    pub(crate) title: String,
    pub(crate) hint: String,
    pub(crate) list: SelectList,
    pub(crate) target_entry_id: String,
    pub(crate) filter_mode: TreeFilterMode,
    pub(crate) query: String,
}

impl Component for TreeSummaryOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput::default();
        append_rule_line(&mut output.lines, width);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_title(&self.title)).render(width),
            false,
        );
        append_blank_lines(&mut output, width, 1);
        append_output(&mut output, self.list.render(width), true);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TreeFilterMode {
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

impl TreeFilterMode {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::Default => Self::NoTools,
            Self::NoTools => Self::UserOnly,
            Self::UserOnly => Self::LabeledOnly,
            Self::LabeledOnly => Self::All,
            Self::All => Self::Default,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::NoTools => "no-tools",
            Self::UserOnly => "user-only",
            Self::LabeledOnly => "labeled-only",
            Self::All => "all",
        }
    }
}
