use std::path::PathBuf;

use pi_rust_tui::{
    Component, Focusable, Input, InputEvent, KeyCode, KeyEvent, RenderOutput, SelectEvent,
    SelectItem, SelectList, Text, truncate_to_width,
};

use super::super::{
    append_blank_lines, append_output, append_overlay_banner, append_rule_line, matches_ctrl_char,
    style_hint, style_subtitle, style_title,
};
use super::TreeFilterMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OverlaySelection {
    Model {
        provider: String,
        model_id: String,
    },
    Session {
        path: PathBuf,
    },
    Fork {
        entry_id: String,
    },
    Tree {
        entry_id: String,
        label: Option<String>,
    },
    AuthProvider {
        provider: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchOverlayKind {
    Tree,
    OAuthLogin,
    OAuthLogout,
}

pub(crate) struct SearchOverlay {
    title: String,
    pub(crate) subtitle: String,
    pub(crate) detail: Option<String>,
    pub(crate) hint: String,
    pub(crate) search: Input,
    pub(crate) search_visible: bool,
    pub(crate) list: SelectList,
}

impl SearchOverlay {
    pub(crate) fn new(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        items: Vec<SelectItem>,
        initial_search: Option<&str>,
        hint: impl Into<String>,
    ) -> Self {
        let mut search = Input::with_prompt("> ");
        search.set_focused(true);
        if let Some(initial_search) = initial_search {
            search.set_value(initial_search.to_string());
        }
        let mut list = SelectList::new(items, 10);
        if let Some(initial_search) = initial_search {
            list.set_filter(initial_search);
        }
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            detail: None,
            hint: hint.into(),
            search,
            search_visible: true,
            list,
        }
    }

    pub(crate) fn selected_value(&self) -> Option<&str> {
        self.list.selected_item().map(|item| item.value.as_str())
    }

    pub(crate) fn set_subtitle(&mut self, subtitle: impl Into<String>) {
        self.subtitle = subtitle.into();
    }

    pub(crate) fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    pub(crate) fn set_detail(&mut self, detail: Option<String>) {
        self.detail = detail;
    }

    pub(crate) fn set_hint(&mut self, hint: impl Into<String>) {
        self.hint = hint.into();
    }

    pub(crate) fn set_search_prompt(&mut self, prompt: impl Into<String>) {
        let value = self.search.get_value().to_string();
        let mut search = Input::with_prompt(prompt.into());
        search.set_focused(true);
        if !value.is_empty() {
            search.set_value(value);
        }
        self.search = search;
    }

    pub(crate) fn set_search_visible(&mut self, visible: bool) {
        self.search_visible = visible;
        self.search.set_focused(visible);
    }

    pub(crate) fn selected_item(&self) -> Option<&SelectItem> {
        self.list.selected_item()
    }

    pub(crate) fn replace_items_preserving_selection(
        &mut self,
        items: Vec<SelectItem>,
        selected_value: Option<&str>,
    ) {
        let filter = self.search.get_value().to_string();
        let mut list = SelectList::new(items, 10);
        if !filter.is_empty() {
            list.set_filter(&filter);
        }
        if let Some(selected_value) = selected_value {
            list.set_selected_value(selected_value);
        }
        self.list = list;
    }

    pub(crate) fn handle_key(&mut self, event: &KeyEvent) -> SearchOverlayEvent {
        if matches_ctrl_char(event, 'c') {
            return SearchOverlayEvent::Cancelled;
        }

        if !self.search_visible {
            return match self.list.handle_key(event) {
                SelectEvent::Selected(item) => SearchOverlayEvent::Selected(item),
                SelectEvent::Cancelled => SearchOverlayEvent::Cancelled,
                SelectEvent::Changed | SelectEvent::None => SearchOverlayEvent::Continue,
            };
        }

        match event.code {
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Enter
            | KeyCode::Escape => match self.list.handle_key(event) {
                SelectEvent::Selected(item) => SearchOverlayEvent::Selected(item),
                SelectEvent::Cancelled => SearchOverlayEvent::Cancelled,
                SelectEvent::Changed | SelectEvent::None => SearchOverlayEvent::Continue,
            },
            _ => match self.search.handle_key(event) {
                InputEvent::Changed => {
                    self.list.set_filter(self.search.get_value());
                    SearchOverlayEvent::Continue
                }
                InputEvent::Cancelled => SearchOverlayEvent::Cancelled,
                InputEvent::Submitted(_) => self
                    .list
                    .selected_item()
                    .cloned()
                    .map(SearchOverlayEvent::Selected)
                    .unwrap_or(SearchOverlayEvent::Continue),
                InputEvent::None => SearchOverlayEvent::Continue,
            },
        }
    }
}

impl Component for SearchOverlay {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput {
            lines: Vec::new(),
            cursor: None,
        };

        append_overlay_banner(&mut output, &self.title, &self.subtitle, width);
        append_blank_lines(&mut output, width, 1);
        if self.search_visible {
            append_output(&mut output, self.search.render(width), false);
            append_blank_lines(&mut output, width, 1);
        }
        append_output(&mut output, self.list.render(width), false);
        if let Some(detail) = self.detail.as_deref() {
            append_blank_lines(&mut output, width, 1);
            append_output(
                &mut output,
                Text::new(style_subtitle(detail)).render(width),
                false,
            );
        }
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_hint(&self.hint)).render(width),
            false,
        );

        output
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SearchOverlayEvent {
    Continue,
    Selected(SelectItem),
    Cancelled,
}

pub(crate) fn select_list_visible_bounds(list: &SelectList) -> (usize, usize) {
    let filtered_len = list.filtered_indices().len();
    let start = list
        .selected_index()
        .saturating_sub(list.max_visible().saturating_sub(1) / 2);
    let end = (start + list.max_visible()).min(filtered_len);
    (start, end)
}

pub(crate) fn render_search_overlay_shell(
    kind: SearchOverlayKind,
    overlay: &SearchOverlay,
    tree_filter: Option<TreeFilterMode>,
    width: u16,
) -> RenderOutput {
    if matches!(kind, SearchOverlayKind::Tree) {
        return render_tree_overlay_shell(overlay, tree_filter, width);
    }

    let mut output = RenderOutput::default();
    append_rule_line(&mut output.lines, width);
    append_blank_lines(&mut output, width, 1);

    let title = match kind {
        SearchOverlayKind::Tree => "Session Tree",
        SearchOverlayKind::OAuthLogin => "Select provider to login:",
        SearchOverlayKind::OAuthLogout => "Select provider to logout:",
    };
    append_output(
        &mut output,
        Text::new(style_title(title)).render(width),
        false,
    );
    if !overlay.subtitle.trim().is_empty() {
        append_output(
            &mut output,
            Text::new(style_subtitle(&overlay.subtitle)).render(width),
            false,
        );
    }
    if let Some(filter_mode) = tree_filter {
        append_output(
            &mut output,
            Text::new(style_hint(&format!("[{}]", filter_mode.label()))).render(width),
            false,
        );
    }
    append_blank_lines(&mut output, width, 1);
    if overlay.search_visible {
        append_output(&mut output, overlay.search.render(width), false);
        append_blank_lines(&mut output, width, 1);
    }
    append_output(&mut output, overlay.list.render(width), false);
    if tree_filter.is_none()
        && let Some(detail) = overlay.detail.as_deref()
    {
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_subtitle(detail)).render(width),
            false,
        );
    }
    if !overlay.hint.trim().is_empty() {
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_hint(&overlay.hint)).render(width),
            false,
        );
    }
    append_blank_lines(&mut output, width, 1);
    append_rule_line(&mut output.lines, width);
    output
}

fn render_tree_overlay_shell(
    overlay: &SearchOverlay,
    tree_filter: Option<TreeFilterMode>,
    width: u16,
) -> RenderOutput {
    let mut output = RenderOutput::default();
    append_rule_line(&mut output.lines, width);
    append_blank_lines(&mut output, width, 1);
    append_output(
        &mut output,
        Text::new(style_title("Session Tree")).render(width),
        false,
    );
    if !overlay.hint.trim().is_empty() {
        append_output(
            &mut output,
            Text::new(style_hint(&overlay.hint)).render(width),
            false,
        );
    }
    let mut search_line = format!("Type to search: {}", overlay.search.get_value());
    if let Some(filter_mode) = tree_filter
        && !matches!(filter_mode, TreeFilterMode::Default)
    {
        search_line.push(' ');
        search_line.push_str(&style_hint(&format!("[{}]", filter_mode.label())));
    }
    append_output(
        &mut output,
        Text::new(truncate_to_width(&search_line, width as usize)).render(width),
        overlay.search_visible,
    );
    append_blank_lines(&mut output, width, 1);
    append_output(&mut output, overlay.list.render(width), false);
    append_blank_lines(&mut output, width, 1);
    append_rule_line(&mut output.lines, width);
    output
}
