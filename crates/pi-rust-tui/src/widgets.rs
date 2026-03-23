use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;

use crate::key::{KeyCode, KeyEvent};
use crate::render::{
    Component, CursorPosition, RenderOutput, RenderedLine, fit_line, truncate_to_width,
    visible_width,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetEvent {
    None,
    Changed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    None,
    Changed,
    Submitted(String),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorEvent {
    None,
    Changed,
    Submitted(String),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectEvent {
    None,
    Changed,
    Selected(SelectItem),
    Cancelled,
}

pub trait Focusable {
    fn set_focused(&mut self, focused: bool);
    fn is_focused(&self) -> bool;
}

pub struct Container {
    children: Vec<Box<dyn Component>>,
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn add_child<C: Component + 'static>(&mut self, child: C) {
        self.children.push(Box::new(child));
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }
}

impl Component for Container {
    fn render(&self, width: u16) -> RenderOutput {
        let mut lines = Vec::new();
        let mut cursor = None;
        for child in &self.children {
            let child_output = child.render(width);
            if cursor.is_none() {
                if let Some(child_cursor) = child_output.cursor {
                    cursor = Some(CursorPosition {
                        row: lines.len() as u16 + child_cursor.row,
                        col: child_cursor.col,
                    });
                }
            }
            lines.extend(child_output.lines);
        }
        RenderOutput { lines, cursor }
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    text: String,
}

impl Text {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    pub fn hint(text: impl Into<String>) -> Self {
        Self::styled(text, Tone::Hint)
    }

    pub fn dim(text: impl Into<String>) -> Self {
        Self::styled(text, Tone::Dim)
    }

    pub fn accent(text: impl Into<String>) -> Self {
        Self::styled(text, Tone::Accent)
    }

    pub fn selected_row(text: impl Into<String>) -> Self {
        Self::styled(text, Tone::SelectedRow)
    }

    pub fn border_active(text: impl Into<String>) -> Self {
        Self::styled(text, Tone::BorderActive)
    }

    pub fn border_muted(text: impl Into<String>) -> Self {
        Self::styled(text, Tone::BorderMuted)
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    fn styled(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: style_tone(&text.into(), tone),
        }
    }
}

impl Component for Text {
    fn render(&self, width: u16) -> RenderOutput {
        RenderOutput {
            lines: self
                .text
                .lines()
                .map(|line| RenderedLine::Text(fit_line(line, width)))
                .collect(),
            cursor: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spacer {
    lines: u16,
}

impl Spacer {
    pub fn new(lines: u16) -> Self {
        Self { lines }
    }
}

impl Component for Spacer {
    fn render(&self, width: u16) -> RenderOutput {
        let blank = " ".repeat(width as usize);
        RenderOutput {
            lines: (0..self.lines)
                .map(|_| RenderedLine::Text(blank.clone()))
                .collect(),
            cursor: None,
        }
    }
}

pub struct BoxWidget {
    padding_x: u16,
    padding_y: u16,
    child: Container,
}

impl BoxWidget {
    pub fn new(padding_x: u16, padding_y: u16) -> Self {
        Self {
            padding_x,
            padding_y,
            child: Container::new(),
        }
    }

    pub fn add_child<C: Component + 'static>(&mut self, child: C) {
        self.child.add_child(child);
    }
}

impl Component for BoxWidget {
    fn render(&self, width: u16) -> RenderOutput {
        if width == 0 {
            return RenderOutput::default();
        }

        let padding_x = self.padding_x.min(width / 2);
        let inner_width = width.saturating_sub(padding_x.saturating_mul(2));
        if inner_width == 0 {
            return RenderOutput::default();
        }

        let inner = self.child.render(inner_width);
        if inner.lines.is_empty() && inner.cursor.is_none() {
            return RenderOutput::default();
        }

        let mut lines = Vec::new();
        let left_padding = " ".repeat(padding_x as usize);
        let blank_line = " ".repeat(width as usize);

        for _ in 0..self.padding_y {
            lines.push(RenderedLine::Text(blank_line.clone()));
        }
        for line in inner.lines {
            match line {
                RenderedLine::Text(text) => {
                    let padded = format!("{left_padding}{text}");
                    lines.push(RenderedLine::Text(fit_line(&padded, width)));
                }
                image => lines.push(image),
            }
        }
        for _ in 0..self.padding_y {
            lines.push(RenderedLine::Text(blank_line.clone()));
        }

        let cursor = inner.cursor.map(|cursor| CursorPosition {
            row: cursor.row + self.padding_y,
            col: cursor.col + padding_x,
        });
        RenderOutput { lines, cursor }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComposerBorderRules<'a> {
    pub top: &'a str,
    pub bottom: &'a str,
}

impl<'a> ComposerBorderRules<'a> {
    pub fn new(top: &'a str, bottom: &'a str) -> Self {
        Self { top, bottom }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    prompt: String,
    value: String,
    cursor: usize,
    focused: bool,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    pub const SELECTOR_SEARCH_PROMPT: &'static str = "> ";

    pub fn new() -> Self {
        Self {
            prompt: "> ".to_string(),
            value: String::new(),
            cursor: 0,
            focused: false,
        }
    }

    pub fn with_prompt(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            ..Self::new()
        }
    }

    pub fn get_value(&self) -> &str {
        &self.value
    }

    pub fn selector_search_prompt() -> &'static str {
        Self::SELECTOR_SEARCH_PROMPT
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.len();
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    pub fn rendered_prompt(&self) -> Cow<'_, str> {
        let trimmed = self.prompt.trim();
        if trimmed == "Search"
            || trimmed == "Search:"
            || trimmed == Self::SELECTOR_SEARCH_PROMPT.trim()
        {
            Cow::Borrowed(Self::SELECTOR_SEARCH_PROMPT)
        } else {
            Cow::Borrowed(self.prompt.as_str())
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> InputEvent {
        match &key.code {
            KeyCode::Enter => InputEvent::Submitted(self.value.clone()),
            KeyCode::Escape => InputEvent::Cancelled,
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let start = previous_grapheme_boundary(&self.value, self.cursor);
                    self.value.replace_range(start..self.cursor, "");
                    self.cursor = start;
                    InputEvent::Changed
                } else {
                    InputEvent::None
                }
            }
            KeyCode::Delete => {
                let end = next_grapheme_boundary(&self.value, self.cursor);
                if end > self.cursor {
                    self.value.replace_range(self.cursor..end, "");
                    InputEvent::Changed
                } else {
                    InputEvent::None
                }
            }
            KeyCode::Left => {
                self.cursor = previous_grapheme_boundary(&self.value, self.cursor);
                InputEvent::None
            }
            KeyCode::Right => {
                self.cursor = next_grapheme_boundary(&self.value, self.cursor);
                InputEvent::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                InputEvent::None
            }
            KeyCode::End => {
                self.cursor = self.value.len();
                InputEvent::None
            }
            KeyCode::Paste(value) => {
                self.insert_text(value);
                InputEvent::Changed
            }
            KeyCode::Char(ch) if !key.modifiers.ctrl => {
                self.insert_plain_text(&ch.to_string());
                InputEvent::Changed
            }
            _ => InputEvent::None,
        }
    }

    fn insert_text(&mut self, text: &str) {
        self.insert_text_internal(text, true);
    }

    fn insert_plain_text(&mut self, text: &str) {
        self.insert_text_internal(text, false);
    }

    fn insert_text_internal(&mut self, text: &str, allow_leading_space_for_path: bool) {
        if text.is_empty() {
            return;
        }

        let normalized = normalize_newlines(text);
        let prefix = if allow_leading_space_for_path
            && should_prepend_space_for_insert(&self.value, self.cursor, &normalized)
        {
            " "
        } else {
            ""
        };

        self.value.insert_str(self.cursor, prefix);
        self.cursor += prefix.len();
        self.value.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
    }
}

impl Focusable for Input {
    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }
}

impl Component for Input {
    fn render(&self, width: u16) -> RenderOutput {
        let prompt = self.rendered_prompt();
        let available = width as usize;
        let prompt_width = visible_width(&prompt);
        if available <= prompt_width {
            return RenderOutput {
                lines: vec![RenderedLine::Text(truncate_to_width(&prompt, available))],
                cursor: Some(CursorPosition { row: 0, col: 0 }),
            };
        }

        let mut visible_text = self.value.clone();
        let cursor_col = prompt_width + visible_width(&self.value[..self.cursor]);
        if prompt_width + visible_width(&visible_text) >= available {
            let remaining = available.saturating_sub(prompt_width + 1);
            let reversed = self
                .value
                .graphemes(true)
                .rev()
                .scan(0usize, |used, grapheme| {
                    let width = visible_width(grapheme);
                    if *used + width > remaining {
                        None
                    } else {
                        *used += width;
                        Some(grapheme)
                    }
                })
                .collect::<Vec<_>>();
            visible_text = reversed.into_iter().rev().collect::<String>();
        }

        let rendered = format!("{}{}", prompt, visible_text);
        let cursor = CursorPosition {
            row: 0,
            col: cursor_col.min(width.saturating_sub(1) as usize) as u16,
        };

        RenderOutput {
            lines: vec![RenderedLine::Text(fit_line(&rendered, width))],
            cursor: Some(cursor),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editor {
    prompt: String,
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    focused: bool,
    history: Vec<String>,
    history_index: Option<usize>,
    history_stash: Option<String>,
    undo_stack: Vec<EditorSnapshot>,
    kill_ring: Vec<String>,
    yank_state: Option<YankState>,
    max_visible_lines: Option<usize>,
    preferred_visual_col: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorSnapshot {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    preferred_visual_col: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct YankState {
    before: EditorSnapshot,
    ring_index: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            prompt: "> ".to_string(),
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
            focused: false,
            history: Vec::new(),
            history_index: None,
            history_stash: None,
            undo_stack: Vec::new(),
            kill_ring: Vec::new(),
            yank_state: None,
            max_visible_lines: None,
            preferred_visual_col: None,
        }
    }

    pub fn with_prompt(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            ..Self::new()
        }
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn set_prompt(&mut self, prompt: impl Into<String>) {
        self.prompt = prompt.into();
    }

    pub fn get_text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn set_text(&mut self, text: impl AsRef<str>) {
        self.reset_history_navigation();
        self.clear_yank_state();
        self.set_text_internal(text.as_ref());
    }

    pub fn clear(&mut self) {
        self.reset_history_navigation();
        self.clear_yank_state();
        self.lines.clear();
        self.lines.push(String::new());
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.preferred_visual_col = None;
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    pub fn set_cursor(&mut self, line: usize, col: usize) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }

        self.cursor_line = line.min(self.lines.len().saturating_sub(1));
        let current_line = self.current_line().to_string();
        self.cursor_col = clamp_to_grapheme_boundary(&current_line, col);
        self.preferred_visual_col = None;
    }

    pub fn max_visible_lines(&self) -> Option<usize> {
        self.max_visible_lines
    }

    pub fn set_max_visible_lines(&mut self, max_visible_lines: Option<usize>) {
        self.max_visible_lines = max_visible_lines.filter(|value| *value > 0);
    }

    pub fn add_history_entry(&mut self, text: impl AsRef<str>) {
        let text = normalize_newlines(text.as_ref());
        if text.trim().is_empty() {
            return;
        }

        if self.history.first().is_some_and(|entry| entry == &text) {
            return;
        }

        self.history.insert(0, text);
        self.history_index = None;
        self.history_stash = None;
    }

    pub fn history_previous(&mut self) -> EditorEvent {
        if self.history.is_empty() {
            return EditorEvent::None;
        }

        let next_index = match self.history_index {
            Some(index) if index + 1 < self.history.len() => index + 1,
            Some(_) => return EditorEvent::None,
            None => {
                self.history_stash = Some(self.get_text());
                0
            }
        };

        self.history_index = Some(next_index);
        let value = self.history[next_index].clone();
        self.set_text_from_history(&value);
        EditorEvent::Changed
    }

    pub fn history_next(&mut self) -> EditorEvent {
        let Some(current_index) = self.history_index else {
            return EditorEvent::None;
        };

        if current_index == 0 {
            self.history_index = None;
            let stash = self.history_stash.take().unwrap_or_default();
            self.set_text_from_history(&stash);
            return EditorEvent::Changed;
        }

        let next_index = current_index - 1;
        self.history_index = Some(next_index);
        let value = self.history[next_index].clone();
        self.set_text_from_history(&value);
        EditorEvent::Changed
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> EditorEvent {
        match &key.code {
            KeyCode::Escape => EditorEvent::Cancelled,
            KeyCode::Enter if key.modifiers.ctrl => self.submit(),
            KeyCode::Enter => changed_event(self.insert_newline()),
            KeyCode::Backspace if key.modifiers.alt => changed_event(self.delete_word_backward()),
            KeyCode::Backspace => changed_event(self.backspace()),
            KeyCode::Delete if key.modifiers.alt => changed_event(self.delete_word_forward()),
            KeyCode::Delete => changed_event(self.delete()),
            KeyCode::Left if key.modifiers.ctrl || key.modifiers.alt => {
                changed_event(self.move_word_left())
            }
            KeyCode::Left => changed_event(self.move_left()),
            KeyCode::Right if key.modifiers.ctrl || key.modifiers.alt => {
                changed_event(self.move_word_right())
            }
            KeyCode::Right => changed_event(self.move_right()),
            KeyCode::Home => changed_event(self.move_home()),
            KeyCode::End => changed_event(self.move_end()),
            KeyCode::Up => changed_event(self.move_up()),
            KeyCode::Down => changed_event(self.move_down()),
            KeyCode::Paste(value) => changed_event(self.insert_text(value)),
            KeyCode::Char('a') if key.modifiers.ctrl => changed_event(self.move_home()),
            KeyCode::Char('e') if key.modifiers.ctrl => changed_event(self.move_end()),
            KeyCode::Char('u') if key.modifiers.ctrl => changed_event(self.delete_to_line_start()),
            KeyCode::Char('k') if key.modifiers.ctrl => changed_event(self.delete_to_line_end()),
            KeyCode::Char('w') if key.modifiers.ctrl => changed_event(self.delete_word_backward()),
            KeyCode::Char('y') if key.modifiers.ctrl => changed_event(self.yank()),
            KeyCode::Char('y') if key.modifiers.alt => changed_event(self.yank_pop()),
            KeyCode::Char('/') if key.modifiers.ctrl => changed_event(self.undo()),
            KeyCode::Char('d') if key.modifiers.alt => changed_event(self.delete_word_forward()),
            KeyCode::Char(ch) if !key.modifiers.ctrl && !key.modifiers.alt => {
                changed_event(self.insert_plain_text(&ch.to_string()))
            }
            _ => EditorEvent::None,
        }
    }

    pub fn submit(&self) -> EditorEvent {
        EditorEvent::Submitted(self.get_text())
    }

    pub fn render_with_max_visible_lines(
        &self,
        width: u16,
        max_visible_lines: Option<usize>,
    ) -> RenderOutput {
        self.render_internal(width, max_visible_lines.or(self.max_visible_lines))
    }

    pub fn render_composer(
        &self,
        width: u16,
        max_visible_lines: Option<usize>,
        top_rule: &str,
        bottom_rule: &str,
        attachment: Option<RenderOutput>,
    ) -> RenderOutput {
        self.render_composer_with_rules(
            width,
            max_visible_lines,
            ComposerBorderRules::new(top_rule, bottom_rule),
            attachment,
        )
    }

    pub fn render_composer_with_rules(
        &self,
        width: u16,
        max_visible_lines: Option<usize>,
        rules: ComposerBorderRules<'_>,
        attachment: Option<RenderOutput>,
    ) -> RenderOutput {
        if width == 0 {
            return RenderOutput::default();
        }

        let mut output = RenderOutput::default();
        output
            .lines
            .push(RenderedLine::Text(fit_line(rules.top, width)));
        append_render_output(
            &mut output,
            self.render_with_max_visible_lines(width, max_visible_lines),
            true,
        );
        output
            .lines
            .push(RenderedLine::Text(fit_line(rules.bottom, width)));
        if let Some(attachment) = attachment {
            append_attached_render_output(&mut output, attachment);
        }
        output
    }

    pub fn insert_text(&mut self, text: &str) -> bool {
        self.push_undo_snapshot();
        self.clear_yank_state();
        self.insert_text_internal(text, true)
    }

    fn insert_plain_text(&mut self, text: &str) -> bool {
        self.push_undo_snapshot();
        self.clear_yank_state();
        self.insert_text_internal(text, false)
    }

    fn insert_text_internal(&mut self, text: &str, allow_leading_space_for_path: bool) -> bool {
        if text.is_empty() {
            return false;
        }

        self.reset_history_navigation();
        let normalized = normalize_newlines(text);
        let parts = normalized.split('\n').collect::<Vec<_>>();
        let current_line = self.current_line().to_string();
        let before = current_line[..self.cursor_col].to_string();
        let after = current_line[self.cursor_col..].to_string();
        let prefix = if allow_leading_space_for_path
            && should_prepend_space_for_insert(&current_line, self.cursor_col, &normalized)
        {
            " "
        } else {
            ""
        };

        self.preferred_visual_col = None;

        if parts.len() == 1 {
            if let Some(line) = self.lines.get_mut(self.cursor_line) {
                line.insert_str(self.cursor_col, prefix);
                line.insert_str(self.cursor_col + prefix.len(), parts[0]);
            }
            self.cursor_col += prefix.len() + parts[0].len();
            return true;
        }

        let base_line = self.cursor_line;
        let last_part_index = parts.len().saturating_sub(1);
        let mut replacement = Vec::with_capacity(parts.len());

        for (index, part) in parts.iter().enumerate() {
            if index == 0 {
                replacement.push(format!("{before}{prefix}{part}"));
            } else if index == last_part_index {
                replacement.push(format!("{part}{after}"));
            } else {
                replacement.push((*part).to_string());
            }
        }

        self.lines.splice(base_line..=base_line, replacement);
        self.cursor_line = base_line + last_part_index;
        self.cursor_col = parts[last_part_index].len();
        true
    }

    pub fn insert_newline(&mut self) -> bool {
        self.insert_text("\n")
    }

    pub fn backspace(&mut self) -> bool {
        self.push_undo_snapshot();
        self.clear_yank_state();
        self.reset_history_navigation();
        let line_index = self.cursor_line;
        let current_line = self.current_line().to_string();

        if self.cursor_col > 0 {
            let start = previous_grapheme_boundary(&current_line, self.cursor_col);
            if let Some(line) = self.lines.get_mut(line_index) {
                line.replace_range(start..self.cursor_col, "");
            }
            self.cursor_col = start;
            self.preferred_visual_col = None;
            return true;
        }

        if line_index == 0 {
            return false;
        }

        let current = self.lines.remove(line_index);
        let previous_index = line_index - 1;
        let previous_len = self.lines[previous_index].len();
        self.lines[previous_index].push_str(&current);
        self.cursor_line = previous_index;
        self.cursor_col = previous_len;
        self.preferred_visual_col = None;
        true
    }

    pub fn delete(&mut self) -> bool {
        self.push_undo_snapshot();
        self.clear_yank_state();
        self.reset_history_navigation();
        let line_index = self.cursor_line;
        let current_line = self.current_line().to_string();

        if self.cursor_col < current_line.len() {
            let end = next_grapheme_boundary(&current_line, self.cursor_col);
            if let Some(line) = self.lines.get_mut(line_index) {
                line.replace_range(self.cursor_col..end, "");
            }
            self.preferred_visual_col = None;
            return true;
        }

        if line_index + 1 >= self.lines.len() {
            return false;
        }

        let next = self.lines.remove(line_index + 1);
        self.lines[line_index].push_str(&next);
        self.preferred_visual_col = None;
        true
    }

    pub fn move_left(&mut self) -> bool {
        let current_line = self.current_line().to_string();
        if self.cursor_col > 0 {
            self.cursor_col = previous_grapheme_boundary(&current_line, self.cursor_col);
            self.preferred_visual_col = None;
            return true;
        }

        if self.cursor_line == 0 {
            return false;
        }

        self.cursor_line -= 1;
        self.cursor_col = self.lines[self.cursor_line].len();
        self.preferred_visual_col = None;
        true
    }

    pub fn move_right(&mut self) -> bool {
        let current_line = self.current_line().to_string();
        if self.cursor_col < current_line.len() {
            self.cursor_col = next_grapheme_boundary(&current_line, self.cursor_col);
            self.preferred_visual_col = None;
            return true;
        }

        if self.cursor_line + 1 >= self.lines.len() {
            return false;
        }

        self.cursor_line += 1;
        self.cursor_col = 0;
        self.preferred_visual_col = None;
        true
    }

    pub fn move_home(&mut self) -> bool {
        if self.cursor_col == 0 {
            return false;
        }
        self.cursor_col = 0;
        self.preferred_visual_col = None;
        true
    }

    pub fn move_end(&mut self) -> bool {
        let line_len = self.current_line().len();
        if self.cursor_col == line_len {
            return false;
        }
        self.cursor_col = line_len;
        self.preferred_visual_col = None;
        true
    }

    pub fn move_up(&mut self) -> bool {
        if self.cursor_line == 0 {
            return false;
        }

        let visual_col = self
            .preferred_visual_col
            .unwrap_or_else(|| visible_width(&self.current_line()[..self.cursor_col]));
        self.preferred_visual_col = Some(visual_col);
        self.cursor_line -= 1;
        self.cursor_col = byte_index_for_visual_col(self.current_line(), visual_col);
        true
    }

    pub fn move_down(&mut self) -> bool {
        if self.cursor_line + 1 >= self.lines.len() {
            return false;
        }

        let visual_col = self
            .preferred_visual_col
            .unwrap_or_else(|| visible_width(&self.current_line()[..self.cursor_col]));
        self.preferred_visual_col = Some(visual_col);
        self.cursor_line += 1;
        self.cursor_col = byte_index_for_visual_col(self.current_line(), visual_col);
        true
    }

    pub fn move_word_left(&mut self) -> bool {
        let current_line = self.current_line().to_string();
        if self.cursor_col == 0 {
            if self.cursor_line == 0 {
                return false;
            }
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            self.preferred_visual_col = None;
            return true;
        }

        let mut cursor = self.cursor_col;
        while cursor > 0 {
            let start = previous_grapheme_boundary(&current_line, cursor);
            let grapheme = &current_line[start..cursor];
            if classify_grapheme(grapheme) != GraphemeClass::Whitespace {
                break;
            }
            cursor = start;
        }

        if cursor == 0 {
            let changed = self.cursor_col != 0;
            self.cursor_col = 0;
            self.preferred_visual_col = None;
            return changed;
        }

        let class = classify_grapheme(
            &current_line[previous_grapheme_boundary(&current_line, cursor)..cursor],
        );
        while cursor > 0 {
            let start = previous_grapheme_boundary(&current_line, cursor);
            let grapheme = &current_line[start..cursor];
            if classify_grapheme(grapheme) != class {
                break;
            }
            cursor = start;
        }

        let changed = cursor != self.cursor_col;
        self.cursor_col = cursor;
        self.preferred_visual_col = None;
        changed
    }

    pub fn move_word_right(&mut self) -> bool {
        let current_line = self.current_line().to_string();
        if self.cursor_col >= current_line.len() {
            if self.cursor_line + 1 >= self.lines.len() {
                return false;
            }
            self.cursor_line += 1;
            self.cursor_col = 0;
            self.preferred_visual_col = None;
            return true;
        }

        let mut cursor = self.cursor_col;
        while cursor < current_line.len() {
            let end = next_grapheme_boundary(&current_line, cursor);
            let grapheme = &current_line[cursor..end];
            if classify_grapheme(grapheme) != GraphemeClass::Whitespace {
                break;
            }
            cursor = end;
        }

        if cursor >= current_line.len() {
            let changed = cursor != self.cursor_col;
            self.cursor_col = cursor;
            self.preferred_visual_col = None;
            return changed;
        }

        let class =
            classify_grapheme(&current_line[cursor..next_grapheme_boundary(&current_line, cursor)]);
        while cursor < current_line.len() {
            let end = next_grapheme_boundary(&current_line, cursor);
            let grapheme = &current_line[cursor..end];
            if classify_grapheme(grapheme) != class {
                break;
            }
            cursor = end;
        }

        let changed = cursor != self.cursor_col;
        self.cursor_col = cursor;
        self.preferred_visual_col = None;
        changed
    }

    pub fn delete_word_backward(&mut self) -> bool {
        self.push_undo_snapshot();
        self.clear_yank_state();
        self.reset_history_navigation();
        if self.cursor_col == 0 {
            if self.cursor_line == 0 {
                return false;
            }
            self.push_kill("\n".to_string());
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            let previous_len = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].push_str(&current);
            self.cursor_col = previous_len;
            self.preferred_visual_col = None;
            return true;
        }

        let old_col = self.cursor_col;
        if !self.move_word_left() {
            return false;
        }
        let delete_from = self.cursor_col;
        let line_index = self.cursor_line;
        self.push_kill(self.lines[line_index][delete_from..old_col].to_string());
        if let Some(line) = self.lines.get_mut(line_index) {
            line.replace_range(delete_from..old_col, "");
        }
        self.cursor_col = delete_from;
        self.preferred_visual_col = None;
        true
    }

    pub fn delete_word_forward(&mut self) -> bool {
        self.push_undo_snapshot();
        self.clear_yank_state();
        self.reset_history_navigation();
        let line_len = self.current_line().len();
        if self.cursor_col >= line_len {
            if self.cursor_line + 1 >= self.lines.len() {
                return false;
            }
            self.push_kill("\n".to_string());
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
            self.preferred_visual_col = None;
            return true;
        }

        let delete_from = self.cursor_col;
        if !self.move_word_right() {
            return false;
        }
        let delete_to = self.cursor_col;
        self.cursor_col = delete_from;
        self.push_kill(self.lines[self.cursor_line][delete_from..delete_to].to_string());
        if let Some(line) = self.lines.get_mut(self.cursor_line) {
            line.replace_range(delete_from..delete_to, "");
        }
        self.preferred_visual_col = None;
        true
    }

    pub fn delete_to_line_start(&mut self) -> bool {
        self.push_undo_snapshot();
        self.clear_yank_state();
        self.reset_history_navigation();
        if self.cursor_col > 0 {
            self.push_kill(self.lines[self.cursor_line][..self.cursor_col].to_string());
            if let Some(line) = self.lines.get_mut(self.cursor_line) {
                line.replace_range(0..self.cursor_col, "");
            }
            self.cursor_col = 0;
            self.preferred_visual_col = None;
            return true;
        }

        if self.cursor_line == 0 {
            return false;
        }

        self.push_kill("\n".to_string());
        let current = self.lines.remove(self.cursor_line);
        self.cursor_line -= 1;
        let previous_len = self.lines[self.cursor_line].len();
        self.lines[self.cursor_line].push_str(&current);
        self.cursor_col = previous_len;
        self.preferred_visual_col = None;
        true
    }

    pub fn delete_to_line_end(&mut self) -> bool {
        self.push_undo_snapshot();
        self.clear_yank_state();
        self.reset_history_navigation();
        let current_len = self.current_line().len();
        if self.cursor_col < current_len {
            self.push_kill(self.lines[self.cursor_line][self.cursor_col..current_len].to_string());
            if let Some(line) = self.lines.get_mut(self.cursor_line) {
                line.replace_range(self.cursor_col..current_len, "");
            }
            self.preferred_visual_col = None;
            return true;
        }

        if self.cursor_line + 1 >= self.lines.len() {
            return false;
        }

        self.push_kill("\n".to_string());
        let next = self.lines.remove(self.cursor_line + 1);
        self.lines[self.cursor_line].push_str(&next);
        self.preferred_visual_col = None;
        true
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo_stack.pop() else {
            return false;
        };
        self.restore_snapshot(snapshot);
        self.clear_yank_state();
        self.reset_history_navigation();
        true
    }

    pub fn yank(&mut self) -> bool {
        let Some(text) = self.kill_ring.first().cloned() else {
            return false;
        };
        let before = self.snapshot();
        self.push_undo_snapshot();
        if !self.insert_plain_text(&text) {
            return false;
        }
        self.yank_state = Some(YankState {
            before,
            ring_index: 0,
        });
        true
    }

    pub fn yank_pop(&mut self) -> bool {
        let Some(mut yank_state) = self.yank_state.clone() else {
            return false;
        };
        if self.kill_ring.len() < 2 {
            return false;
        }
        yank_state.ring_index = (yank_state.ring_index + 1) % self.kill_ring.len();
        let next_text = self.kill_ring[yank_state.ring_index].clone();
        self.restore_snapshot(yank_state.before.clone());
        if !self.insert_plain_text(&next_text) {
            return false;
        }
        self.yank_state = Some(yank_state);
        true
    }

    fn current_line(&self) -> &str {
        self.lines
            .get(self.cursor_line)
            .map(String::as_str)
            .unwrap_or_default()
    }

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
            preferred_visual_col: self.preferred_visual_col,
        }
    }

    fn restore_snapshot(&mut self, snapshot: EditorSnapshot) {
        self.lines = snapshot.lines;
        self.cursor_line = snapshot.cursor_line;
        self.cursor_col = snapshot.cursor_col;
        self.preferred_visual_col = snapshot.preferred_visual_col;
    }

    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > 256 {
            self.undo_stack.remove(0);
        }
    }

    fn push_kill(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if self.kill_ring.first() == Some(&text) {
            return;
        }
        self.kill_ring.insert(0, text);
        if self.kill_ring.len() > 32 {
            self.kill_ring.truncate(32);
        }
    }

    fn clear_yank_state(&mut self) {
        self.yank_state = None;
    }

    fn reset_history_navigation(&mut self) {
        self.history_index = None;
        self.history_stash = None;
    }

    fn set_text_from_history(&mut self, text: &str) {
        self.lines = split_editor_lines(text);
        self.cursor_line = self.lines.len().saturating_sub(1);
        self.cursor_col = self
            .lines
            .get(self.cursor_line)
            .map(String::len)
            .unwrap_or(0);
        self.preferred_visual_col = None;
    }

    fn set_text_internal(&mut self, text: &str) {
        self.lines = split_editor_lines(text);
        self.cursor_line = self.lines.len().saturating_sub(1);
        self.cursor_col = self
            .lines
            .get(self.cursor_line)
            .map(String::len)
            .unwrap_or(0);
        self.preferred_visual_col = None;
    }
}

impl Focusable for Editor {
    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }
}

impl Component for Editor {
    fn render(&self, width: u16) -> RenderOutput {
        self.render_internal(width, self.max_visible_lines)
    }
}

impl Editor {
    fn render_internal(&self, width: u16, max_visible_lines: Option<usize>) -> RenderOutput {
        if width == 0 {
            return RenderOutput::default();
        }

        let available = width as usize;
        let prompt_width = visible_width(&self.prompt);
        if available <= prompt_width {
            return RenderOutput {
                lines: vec![RenderedLine::Text(truncate_to_width(
                    &self.prompt,
                    available,
                ))],
                cursor: Some(CursorPosition {
                    row: 0,
                    col: width.saturating_sub(1),
                }),
            };
        }

        let content_width = available.saturating_sub(prompt_width).max(1);
        let continuation_prefix = " ".repeat(prompt_width);
        let mut rendered_lines = Vec::new();
        let mut cursor = CursorPosition { row: 0, col: 0 };
        let mut first_visual_line = true;

        for (line_index, line) in self.lines.iter().enumerate() {
            let segments = wrap_editor_line(
                line,
                content_width,
                (self.cursor_line == line_index).then_some(self.cursor_col),
            );

            for segment in segments {
                let prefix = if first_visual_line {
                    self.prompt.as_str()
                } else {
                    continuation_prefix.as_str()
                };
                let rendered = fit_line(&format!("{prefix}{}", segment.text), width);
                rendered_lines.push(RenderedLine::Text(rendered));

                if let Some(cursor_col) = segment.cursor_col {
                    cursor = CursorPosition {
                        row: rendered_lines.len().saturating_sub(1) as u16,
                        col: (prompt_width + cursor_col).min(available.saturating_sub(1)) as u16,
                    };
                }

                first_visual_line = false;
            }
        }

        if rendered_lines.is_empty() {
            rendered_lines.push(RenderedLine::Text(fit_line(&self.prompt, width)));
            cursor = CursorPosition {
                row: 0,
                col: prompt_width.min(available.saturating_sub(1)) as u16,
            };
        }

        if let Some(max_visible_lines) = max_visible_lines.filter(|value| *value > 0) {
            if rendered_lines.len() > max_visible_lines {
                let cursor_row = cursor.row as usize;
                let start = cursor_row.saturating_sub(max_visible_lines.saturating_sub(1));
                let end = (start + max_visible_lines).min(rendered_lines.len());
                rendered_lines = rendered_lines[start..end].to_vec();
                cursor.row = cursor_row.saturating_sub(start) as u16;
            }
        }

        RenderOutput {
            lines: rendered_lines,
            cursor: Some(cursor),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

fn normalize_to_single_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ").trim().to_string()
}

const ANSI_RESET: &str = "\u{1b}[0m";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Hint,
    Dim,
    Accent,
    SelectedRow,
    BorderActive,
    BorderMuted,
}

fn ansi(code: &str, text: &str) -> String {
    format!("\u{1b}[{code}m{text}{ANSI_RESET}")
}

fn style_tone(text: &str, tone: Tone) -> String {
    match tone {
        Tone::Hint => ansi("38;5;244", text),
        Tone::Dim => ansi("38;5;244", text),
        Tone::Accent => ansi("1;38;5;111", text),
        Tone::SelectedRow => ansi("48;5;236;38;5;255", text),
        Tone::BorderActive => ansi("1;38;5;111", text),
        Tone::BorderMuted => ansi("38;5;244", text),
    }
}

fn style_selected_row(text: &str) -> String {
    style_tone(text, Tone::SelectedRow)
}

fn style_hint(text: &str) -> String {
    style_tone(text, Tone::Hint)
}

fn style_dim(text: &str) -> String {
    style_tone(text, Tone::Dim)
}

fn style_accent(text: &str) -> String {
    style_tone(text, Tone::Accent)
}

fn selector_divider(width: u16) -> RenderedLine {
    RenderedLine::Text(style_dim(&fit_line(&"─".repeat(width as usize), width)))
}

fn selector_spacer(width: u16) -> RenderedLine {
    RenderedLine::Text(fit_line(" ", width))
}

#[cfg(test)]
fn rendered_text_rows(output: &RenderOutput) -> Vec<String> {
    output
        .lines
        .iter()
        .filter_map(|line| match line {
            RenderedLine::Text(text) => Some(text.clone()),
            RenderedLine::Image(_) => None,
        })
        .collect()
}

fn render_selector_description(
    prefix: &str,
    label: &str,
    description: Option<&str>,
    width: u16,
    selected: bool,
) -> RenderedLine {
    let prefix_width = visible_width(prefix);
    if width as usize <= prefix_width + 1 {
        return RenderedLine::Text(fit_line(&truncate_to_width(prefix, width as usize), width));
    }

    let line = if let Some(description) = description
        .map(normalize_to_single_line)
        .filter(|value| !value.is_empty())
    {
        if width > 40 {
            let max_value_width = usize::min(30, (width as usize).saturating_sub(prefix_width + 4));
            let value = truncate_to_width(label, max_value_width);
            let value_width = visible_width(&value);
            let spacing = " ".repeat(usize::max(1, 32usize.saturating_sub(value_width)));
            let remaining_width =
                (width as usize).saturating_sub(prefix_width + value_width + spacing.len() + 2);
            if remaining_width > 10 {
                format!(
                    "{prefix}{value}{spacing}{}",
                    truncate_to_width(&description, remaining_width)
                )
            } else {
                format!(
                    "{prefix}{}",
                    truncate_to_width(label, (width as usize).saturating_sub(prefix_width + 2))
                )
            }
        } else {
            format!(
                "{prefix}{}",
                truncate_to_width(label, (width as usize).saturating_sub(prefix_width + 2))
            )
        }
    } else {
        format!(
            "{prefix}{}",
            truncate_to_width(label, (width as usize).saturating_sub(prefix_width + 2))
        )
    };
    let rendered = fit_line(&line, width);
    RenderedLine::Text(if selected {
        style_selected_row(&rendered)
    } else {
        rendered
    })
}

fn render_selector_value(
    prefix: &str,
    label: &str,
    current_value: &str,
    width: u16,
    selected: bool,
    max_label_width: usize,
) -> RenderedLine {
    let prefix_width = visible_width(prefix);
    if width as usize <= prefix_width + 1 {
        return RenderedLine::Text(fit_line(&truncate_to_width(prefix, width as usize), width));
    }

    let label_padded = format!(
        "{:width$}",
        label,
        width = max_label_width.max(visible_width(label))
    );
    let label = truncate_to_width(&label_padded, max_label_width.max(visible_width(label)));
    let used_width = prefix_width + visible_width(&label) + 2;
    let value_max_width = (width as usize).saturating_sub(used_width + 2);
    let value = truncate_to_width(current_value, value_max_width);
    let line = format!("{prefix}{label}  {value}");
    let rendered = fit_line(&line, width);
    RenderedLine::Text(if selected {
        style_selected_row(&rendered)
    } else {
        rendered
    })
}

fn changed_event(changed: bool) -> EditorEvent {
    if changed {
        EditorEvent::Changed
    } else {
        EditorEvent::None
    }
}

fn append_render_output(output: &mut RenderOutput, child: RenderOutput, capture_cursor: bool) {
    let row_offset = output.lines.len() as u16;
    if capture_cursor && output.cursor.is_none() {
        if let Some(cursor) = child.cursor {
            output.cursor = Some(CursorPosition {
                row: row_offset + cursor.row,
                col: cursor.col,
            });
        }
    }
    output.lines.extend(child.lines);
}

fn append_attached_render_output(output: &mut RenderOutput, child: RenderOutput) {
    append_render_output(output, child, output.cursor.is_none());
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn split_editor_lines(text: &str) -> Vec<String> {
    let normalized = normalize_newlines(text);
    let mut lines = normalized
        .split('\n')
        .map(str::to_string)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn should_prepend_space_for_insert(current_text: &str, cursor: usize, inserted_text: &str) -> bool {
    if cursor == 0 || inserted_text.is_empty() || !looks_like_inserted_path(inserted_text) {
        return false;
    }

    let Some(previous_grapheme) = current_text[..cursor].graphemes(true).next_back() else {
        return false;
    };
    classify_grapheme(previous_grapheme) == GraphemeClass::Word
}

fn looks_like_inserted_path(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().any(|ch| ch == '\n' || ch == '\r') {
        return false;
    }

    text.starts_with("file://")
        || text.starts_with('/')
        || text.starts_with("./")
        || text.starts_with("../")
        || text.starts_with('~')
        || text.contains('\\')
        || (text.contains('/') && !text.contains("://"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphemeClass {
    Whitespace,
    Word,
    Punctuation,
}

fn classify_grapheme(grapheme: &str) -> GraphemeClass {
    if grapheme.chars().all(char::is_whitespace) {
        GraphemeClass::Whitespace
    } else if grapheme.chars().any(|ch| ch.is_alphanumeric() || ch == '_') {
        GraphemeClass::Word
    } else {
        GraphemeClass::Punctuation
    }
}

fn clamp_to_grapheme_boundary(text: &str, col: usize) -> usize {
    let col = col.min(text.len());
    if col == text.len() {
        return col;
    }

    let mut boundary = 0usize;
    for (offset, _) in text.grapheme_indices(true) {
        if offset > col {
            break;
        }
        boundary = offset;
    }
    boundary
}

fn byte_index_for_visual_col(text: &str, target_visual_col: usize) -> usize {
    let mut visual_col = 0usize;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let grapheme_width = visible_width(grapheme);
        if visual_col + grapheme_width > target_visual_col {
            return offset;
        }
        visual_col += grapheme_width;
    }
    text.len()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WrappedEditorLine {
    text: String,
    cursor_col: Option<usize>,
}

fn wrap_editor_line(line: &str, width: usize, cursor_col: Option<usize>) -> Vec<WrappedEditorLine> {
    if line.is_empty() {
        return vec![WrappedEditorLine {
            text: String::new(),
            cursor_col: cursor_col.map(|_| 0),
        }];
    }

    let mut ranges = Vec::new();
    let mut segment_start = 0usize;
    let mut segment_width = 0usize;

    for (offset, grapheme) in line.grapheme_indices(true) {
        let grapheme_width = visible_width(grapheme);
        if offset > segment_start && segment_width + grapheme_width > width {
            ranges.push((segment_start, offset));
            segment_start = offset;
            segment_width = 0;
        }
        segment_width += grapheme_width;
    }
    ranges.push((segment_start, line.len()));

    ranges
        .iter()
        .enumerate()
        .map(|(index, (start, end))| {
            let is_last_segment = index + 1 == ranges.len();
            let cursor_col = cursor_col.and_then(|cursor_col| {
                let in_segment = if is_last_segment {
                    cursor_col >= *start && cursor_col <= *end
                } else {
                    cursor_col >= *start && cursor_col < *end
                };
                in_segment.then(|| visible_width(&line[*start..cursor_col]))
            });

            WrappedEditorLine {
                text: line[*start..*end].to_string(),
                cursor_col,
            }
        })
        .collect()
}

pub struct SelectList {
    items: Vec<SelectItem>,
    filtered_indices: Vec<usize>,
    selected_index: usize,
    max_visible: usize,
}

impl SelectList {
    pub fn new(items: Vec<SelectItem>, max_visible: usize) -> Self {
        let filtered_indices = (0..items.len()).collect();
        Self {
            items,
            filtered_indices,
            selected_index: 0,
            max_visible,
        }
    }

    pub fn render_item_line(
        prefix: &str,
        label: &str,
        description: Option<&str>,
        width: u16,
        selected: bool,
    ) -> RenderedLine {
        render_selector_description(prefix, label, description, width, selected)
    }

    pub fn visible_bounds(&self) -> (usize, usize) {
        let max_visible = self.max_visible.max(1);
        let start = self
            .selected_index
            .saturating_sub(max_visible.saturating_sub(1) / 2);
        let end = (start + max_visible).min(self.filtered_indices.len());
        (start, end)
    }

    pub fn scroll_status(&self) -> Option<String> {
        if self.filtered_indices.is_empty() {
            return None;
        }

        let (start, end) = self.visible_bounds();
        if start > 0 || end < self.filtered_indices.len() {
            Some(format!(
                "  ({}/{})",
                self.selected_index + 1,
                self.filtered_indices.len()
            ))
        } else {
            None
        }
    }

    pub fn footer_hint(&self) -> &'static str {
        "  Enter/Space to change · Esc to cancel"
    }

    pub fn render_shell(&self, width: u16, standalone: bool) -> RenderOutput {
        let mut output = self.render(width);
        if standalone && width > 0 && !output.lines.is_empty() {
            output.lines.push(selector_divider(width));
            output.lines.push(RenderedLine::Text(style_hint(&fit_line(
                self.footer_hint(),
                width,
            ))));
        }
        output
    }

    pub fn set_items(&mut self, items: Vec<SelectItem>) {
        self.items = items;
        self.filtered_indices = (0..self.items.len()).collect();
        self.selected_index = 0;
    }

    pub fn replace_items_preserving_selection(
        &mut self,
        items: Vec<SelectItem>,
        selected_value: Option<&str>,
    ) {
        self.items = items;
        self.filtered_indices = (0..self.items.len()).collect();
        self.selected_index = 0;
        if let Some(value) = selected_value {
            self.set_selected_value(value);
        }
    }

    pub fn set_filter(&mut self, filter: &str) {
        if filter.is_empty() {
            self.filtered_indices = (0..self.items.len()).collect();
        } else {
            let filter = filter.to_lowercase();
            self.filtered_indices = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let haystack = format!(
                        "{} {} {}",
                        item.label,
                        item.value,
                        item.description.as_deref().unwrap_or_default()
                    )
                    .to_lowercase();
                    haystack.contains(&filter).then_some(index)
                })
                .collect();
        }
        self.selected_index = 0;
    }

    pub fn set_selected_index(&mut self, index: usize) {
        let max = self.filtered_indices.len().saturating_sub(1);
        self.selected_index = index.min(max);
    }

    pub fn set_selected_value(&mut self, value: &str) {
        if let Some(index) = self
            .filtered_indices
            .iter()
            .position(|item_index| self.items[*item_index].value == value)
        {
            self.selected_index = index;
        }
    }

    pub fn selected_item(&self) -> Option<&SelectItem> {
        self.filtered_indices
            .get(self.selected_index)
            .and_then(|index| self.items.get(*index))
    }

    pub fn selected_value(&self) -> Option<&str> {
        self.selected_item().map(|item| item.value.as_str())
    }

    pub fn contains_value(&self, value: &str) -> bool {
        self.filtered_indices
            .iter()
            .any(|index| self.items[*index].value == value)
    }

    pub fn filtered_indices(&self) -> &[usize] {
        &self.filtered_indices
    }

    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub fn max_visible(&self) -> usize {
        self.max_visible
    }

    pub fn select_previous(&mut self) -> SelectEvent {
        if self.filtered_indices.is_empty() {
            return SelectEvent::None;
        }

        self.selected_index = if self.selected_index == 0 {
            self.filtered_indices.len().saturating_sub(1)
        } else {
            self.selected_index.saturating_sub(1)
        };
        SelectEvent::Changed
    }

    pub fn select_next(&mut self) -> SelectEvent {
        if self.filtered_indices.is_empty() {
            return SelectEvent::None;
        }

        self.selected_index = if self.selected_index + 1 >= self.filtered_indices.len() {
            0
        } else {
            self.selected_index + 1
        };
        SelectEvent::Changed
    }

    pub fn confirm_selection(&self) -> SelectEvent {
        self.selected_item()
            .cloned()
            .map(SelectEvent::Selected)
            .unwrap_or(SelectEvent::None)
    }

    pub fn cancel(&self) -> SelectEvent {
        SelectEvent::Cancelled
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> SelectEvent {
        match key.code {
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
            KeyCode::Enter => self.confirm_selection(),
            KeyCode::Char(' ') => self.confirm_selection(),
            KeyCode::Escape => self.cancel(),
            KeyCode::Char('c') if key.modifiers.ctrl => self.cancel(),
            _ => SelectEvent::None,
        }
    }
}

impl Component for SelectList {
    fn render(&self, width: u16) -> RenderOutput {
        if self.filtered_indices.is_empty() {
            return RenderOutput {
                lines: vec![RenderedLine::Text(style_dim(&fit_line(
                    "  No matching items",
                    width,
                )))],
                cursor: None,
            };
        }

        let mut lines = Vec::new();
        let (start, end) = self.visible_bounds();

        for visible_index in start..end {
            let item = &self.items[self.filtered_indices[visible_index]];
            let prefix = if visible_index == self.selected_index {
                "→ "
            } else {
                "  "
            };
            lines.push(Self::render_item_line(
                prefix,
                &item.label,
                item.description.as_deref(),
                width,
                visible_index == self.selected_index,
            ));
        }

        if let Some(status) = self.scroll_status() {
            lines.push(RenderedLine::Text(style_dim(&fit_line(&status, width))));
        }

        RenderOutput {
            lines,
            cursor: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingSubmenu {
    pub title: String,
    pub description: Option<String>,
    pub options: Vec<SelectItem>,
    pub current_value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub current_value: String,
    pub values: Vec<String>,
    pub submenu: Option<SettingSubmenu>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingsListOptions {
    pub enable_search: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsListEvent {
    None,
    Changed { id: String, value: String },
    Cancelled,
}

pub struct SettingsList {
    items: Vec<SettingItem>,
    filtered_indices: Vec<usize>,
    selected_index: usize,
    max_visible: usize,
    search_enabled: bool,
    search_input: Input,
    submenu: Option<SelectList>,
    submenu_item_index: Option<usize>,
}

impl SettingsList {
    pub fn new(items: Vec<SettingItem>, max_visible: usize) -> Self {
        Self::with_options(items, max_visible, SettingsListOptions::default())
    }

    pub fn with_options(
        items: Vec<SettingItem>,
        max_visible: usize,
        options: SettingsListOptions,
    ) -> Self {
        let filtered_indices = (0..items.len()).collect();
        let mut search_input = Input::with_prompt(Input::selector_search_prompt());
        search_input.set_focused(options.enable_search);
        Self {
            items,
            filtered_indices,
            selected_index: 0,
            max_visible,
            search_enabled: options.enable_search,
            search_input,
            submenu: None,
            submenu_item_index: None,
        }
    }

    pub fn render_item_line(
        prefix: &str,
        label: &str,
        current_value: &str,
        width: u16,
        selected: bool,
        max_label_width: usize,
    ) -> RenderedLine {
        render_selector_value(
            prefix,
            label,
            current_value,
            width,
            selected,
            max_label_width,
        )
    }

    pub fn visible_bounds(&self) -> (usize, usize) {
        let start = self
            .selected_index
            .saturating_sub(self.max_visible.saturating_sub(1) / 2);
        let end = (start + self.max_visible).min(self.filtered_indices.len());
        (start, end)
    }

    pub fn scroll_status(&self) -> Option<String> {
        if self.filtered_indices.is_empty() {
            return None;
        }

        let (start, end) = self.visible_bounds();
        if start > 0 || end < self.filtered_indices.len() {
            Some(format!(
                "  ({}/{})",
                self.selected_index + 1,
                self.filtered_indices.len()
            ))
        } else {
            None
        }
    }

    pub fn footer_hint(&self) -> &'static str {
        if self.search_enabled {
            "  Type to search · Enter/Space to change · Esc to cancel"
        } else {
            "  Enter/Space to change · Esc to cancel"
        }
    }

    pub fn set_items(&mut self, items: Vec<SettingItem>) {
        self.items = items;
        let filter = self.search_input.get_value().to_string();
        self.apply_filter(&filter);
        self.selected_index = 0;
        self.submenu = None;
        self.submenu_item_index = None;
    }

    pub fn replace_items_preserving_selection(
        &mut self,
        items: Vec<SettingItem>,
        selected_value: Option<&str>,
    ) {
        self.items = items;
        let filter = self.search_input.get_value().to_string();
        self.apply_filter(&filter);
        self.selected_index = 0;
        if let Some(value) = selected_value {
            self.set_selected_value(value);
        }
        self.submenu = None;
        self.submenu_item_index = None;
    }

    pub fn set_selected_index(&mut self, index: usize) {
        self.selected_index = index.min(self.filtered_indices.len().saturating_sub(1));
    }

    pub fn set_selected_value(&mut self, value: &str) {
        if let Some(index) = self
            .filtered_indices
            .iter()
            .position(|item_index| self.items[*item_index].current_value == value)
        {
            self.selected_index = index;
        }
    }

    pub fn selected_item(&self) -> Option<&SettingItem> {
        self.filtered_indices
            .get(self.selected_index)
            .and_then(|index| self.items.get(*index))
    }

    pub fn selected_value(&self) -> Option<&str> {
        self.selected_item().map(|item| item.current_value.as_str())
    }

    pub fn set_max_visible(&mut self, max_visible: usize) {
        self.max_visible = max_visible.max(1);
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> SettingsListEvent {
        if let Some(submenu) = &mut self.submenu {
            match submenu.handle_key(key) {
                SelectEvent::Changed => return SettingsListEvent::None,
                SelectEvent::Cancelled => {
                    self.close_submenu();
                    return SettingsListEvent::None;
                }
                SelectEvent::Selected(item) => {
                    let Some(setting_index) = self.submenu_item_index else {
                        self.close_submenu();
                        return SettingsListEvent::None;
                    };
                    let event = if let Some(setting) = self.items.get_mut(setting_index) {
                        setting.current_value = item.value.clone();
                        SettingsListEvent::Changed {
                            id: setting.id.clone(),
                            value: item.value,
                        }
                    } else {
                        SettingsListEvent::None
                    };
                    self.close_submenu();
                    return event;
                }
                SelectEvent::None => {}
            }
        }

        match &key.code {
            KeyCode::Escape => SettingsListEvent::Cancelled,
            KeyCode::Char('c') if key.modifiers.ctrl => SettingsListEvent::Cancelled,
            KeyCode::Up => {
                if self.filtered_indices.is_empty() {
                    SettingsListEvent::None
                } else {
                    self.selected_index = if self.selected_index == 0 {
                        self.filtered_indices.len().saturating_sub(1)
                    } else {
                        self.selected_index.saturating_sub(1)
                    };
                    SettingsListEvent::None
                }
            }
            KeyCode::Down => {
                if self.filtered_indices.is_empty() {
                    SettingsListEvent::None
                } else {
                    self.selected_index = if self.selected_index + 1 >= self.filtered_indices.len()
                    {
                        0
                    } else {
                        self.selected_index + 1
                    };
                    SettingsListEvent::None
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.activate_selected_item(),
            KeyCode::Backspace => {
                if self.search_enabled {
                    let filter = self.search_input.get_value().to_string();
                    if self.search_input.handle_key(key) == InputEvent::Changed {
                        self.apply_filter(&filter);
                    }
                }
                SettingsListEvent::None
            }
            KeyCode::Delete => {
                if self.search_enabled {
                    let filter = self.search_input.get_value().to_string();
                    if self.search_input.handle_key(key) == InputEvent::Changed {
                        self.apply_filter(&filter);
                    }
                }
                SettingsListEvent::None
            }
            KeyCode::Paste(value) => {
                if self.search_enabled {
                    self.search_input
                        .handle_key(&KeyEvent::new(KeyCode::Paste(value.clone())));
                    let filter = self.search_input.get_value().to_string();
                    self.apply_filter(&filter);
                }
                SettingsListEvent::None
            }
            KeyCode::Char(ch) => {
                if self.search_enabled && !key.modifiers.ctrl && !key.modifiers.alt && *ch != ' ' {
                    let filter = self.search_input.get_value().to_string();
                    self.search_input.handle_key(key);
                    self.apply_filter(&filter);
                }
                SettingsListEvent::None
            }
            _ => SettingsListEvent::None,
        }
    }

    fn activate_selected_item(&mut self) -> SettingsListEvent {
        let Some(item_index) = self.filtered_indices.get(self.selected_index).copied() else {
            return SettingsListEvent::None;
        };
        let Some(item) = self.items.get_mut(item_index) else {
            return SettingsListEvent::None;
        };

        if let Some(submenu) = &item.submenu {
            let mut select_list = SelectList::new(submenu.options.clone(), 10);
            select_list.set_selected_value(&submenu.current_value);
            self.submenu = Some(select_list);
            self.submenu_item_index = Some(item_index);
            return SettingsListEvent::None;
        }

        if item.values.is_empty() {
            return SettingsListEvent::None;
        }

        let current_index = item
            .values
            .iter()
            .position(|value| value == &item.current_value)
            .unwrap_or(0);
        let next_index = (current_index + 1) % item.values.len();
        let next_value = item.values[next_index].clone();
        item.current_value = next_value.clone();
        SettingsListEvent::Changed {
            id: item.id.clone(),
            value: next_value,
        }
    }

    fn apply_filter(&mut self, filter: &str) {
        if !self.search_enabled {
            self.filtered_indices = (0..self.items.len()).collect();
            return;
        }

        let filter = filter.to_lowercase();
        if filter.is_empty() {
            self.filtered_indices = (0..self.items.len()).collect();
            return;
        }

        self.filtered_indices = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let haystack = format!(
                    "{} {} {}",
                    item.label,
                    item.current_value,
                    item.description.as_deref().unwrap_or_default()
                )
                .to_lowercase();
                haystack.contains(&filter).then_some(index)
            })
            .collect();
    }

    fn close_submenu(&mut self) {
        self.submenu = None;
        self.submenu_item_index = None;
    }
}

impl Focusable for SettingsList {
    fn set_focused(&mut self, focused: bool) {
        self.search_input.set_focused(focused);
    }

    fn is_focused(&self) -> bool {
        self.search_input.is_focused()
    }
}

impl Component for SettingsList {
    fn render(&self, width: u16) -> RenderOutput {
        let compact_spacing = width < 72;
        if let Some(submenu) = &self.submenu {
            let mut output = RenderOutput::default();
            output.lines.push(RenderedLine::Text(style_accent(&fit_line(
                &format!(
                    "  {}",
                    self.items[self.submenu_item_index.unwrap_or(0)].label
                ),
                width,
            ))));
            if let Some(description) = self
                .submenu_item_index
                .and_then(|index| self.items.get(index))
                .and_then(|item| item.description.as_deref())
            {
                output.lines.push(RenderedLine::Text(style_dim(&fit_line(
                    &format!("  {}", description),
                    width,
                ))));
            }
            output.lines.push(if compact_spacing {
                selector_divider(width)
            } else {
                selector_spacer(width)
            });
            append_render_output(&mut output, submenu.render(width), false);
            output.lines.push(if compact_spacing {
                selector_divider(width)
            } else {
                selector_spacer(width)
            });
            output.lines.push(RenderedLine::Text(style_hint(&fit_line(
                self.footer_hint(),
                width,
            ))));
            return output;
        }

        let mut output = RenderOutput::default();
        if self.search_enabled {
            append_render_output(&mut output, self.search_input.render(width), false);
            output.lines.push(if compact_spacing {
                selector_divider(width)
            } else {
                selector_spacer(width)
            });
        }

        if self.items.is_empty() {
            output.lines.push(RenderedLine::Text(style_dim(&fit_line(
                "  No settings available",
                width,
            ))));
            if self.search_enabled {
                output.lines.push(if compact_spacing {
                    selector_divider(width)
                } else {
                    selector_spacer(width)
                });
                output.lines.push(RenderedLine::Text(style_hint(&fit_line(
                    self.footer_hint(),
                    width,
                ))));
            }
            return output;
        }

        if self.filtered_indices.is_empty() {
            output.lines.push(RenderedLine::Text(style_dim(&fit_line(
                "  No matching settings",
                width,
            ))));
            output.lines.push(if compact_spacing {
                selector_divider(width)
            } else {
                selector_spacer(width)
            });
            output.lines.push(RenderedLine::Text(style_hint(&fit_line(
                self.footer_hint(),
                width,
            ))));
            return output;
        }

        let (start, end) = self.visible_bounds();
        let max_label_width = self
            .items
            .iter()
            .map(|item| visible_width(&item.label))
            .max()
            .unwrap_or(0)
            .min(30);

        for visible_index in start..end {
            let item = &self.items[self.filtered_indices[visible_index]];
            let is_selected = visible_index == self.selected_index;
            let prefix = if is_selected { "→ " } else { "  " };
            output.lines.push(Self::render_item_line(
                prefix,
                &item.label,
                &item.current_value,
                width,
                is_selected,
                max_label_width,
            ));
        }

        if let Some(status) = self.scroll_status() {
            output
                .lines
                .push(RenderedLine::Text(style_dim(&fit_line(&status, width))));
        }

        if let Some(selected) = self.selected_item() {
            if let Some(description) = selected.description.as_deref() {
                output.lines.push(RenderedLine::Text(fit_line(" ", width)));
                for line in wrap_plain_text(description, width.saturating_sub(4) as usize) {
                    output.lines.push(RenderedLine::Text(style_dim(&fit_line(
                        &format!("  {line}"),
                        width,
                    ))));
                }
            }
        }

        output.lines.push(RenderedLine::Text(style_hint(&fit_line(
            self.footer_hint(),
            width,
        ))));
        output
    }
}

impl SettingsList {
    pub fn render_shell(&self, width: u16, standalone: bool) -> RenderOutput {
        let mut output = self.render(width);
        if standalone && width > 0 && !output.lines.is_empty() {
            output.lines.push(selector_divider(width));
        }
        output
    }
}

fn wrap_plain_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();

    for paragraph in normalize_newlines(text).lines() {
        if paragraph.trim().is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_width = 0usize;

        for word in paragraph.split_whitespace() {
            let word_width = visible_width(word);
            if current.is_empty() {
                if word_width <= width {
                    current.push_str(word);
                    current_width = word_width;
                } else {
                    let mut chunk = String::new();
                    let mut chunk_width = 0usize;
                    for grapheme in word.graphemes(true) {
                        let grapheme_width = visible_width(grapheme);
                        if chunk_width + grapheme_width > width && !chunk.is_empty() {
                            lines.push(chunk.clone());
                            chunk.clear();
                            chunk_width = 0;
                        }
                        chunk.push_str(grapheme);
                        chunk_width += grapheme_width;
                    }
                    if !chunk.is_empty() {
                        lines.push(chunk);
                    }
                }
                continue;
            }

            if current_width + 1 + word_width <= width {
                current.push(' ');
                current.push_str(word);
                current_width += 1 + word_width;
            } else {
                lines.push(current);
                current = String::new();
                current_width = 0;
                if word_width <= width {
                    current.push_str(word);
                    current_width = word_width;
                } else {
                    let mut chunk = String::new();
                    let mut chunk_width = 0usize;
                    for grapheme in word.graphemes(true) {
                        let grapheme_width = visible_width(grapheme);
                        if chunk_width + grapheme_width > width && !chunk.is_empty() {
                            lines.push(chunk.clone());
                            chunk.clear();
                            chunk_width = 0;
                        }
                        chunk.push_str(grapheme);
                        chunk_width += grapheme_width;
                    }
                    if !chunk.is_empty() {
                        current = chunk;
                        current_width = chunk_width;
                    }
                }
            }
        }

        if !current.is_empty() {
            lines.push(current);
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

fn previous_grapheme_boundary(text: &str, index: usize) -> usize {
    text[..index]
        .grapheme_indices(true)
        .last()
        .map(|(offset, _)| offset)
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    text[index..]
        .grapheme_indices(true)
        .nth(1)
        .map(|(offset, _)| index + offset)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::{
        BoxWidget, Component, ComposerBorderRules, Editor, EditorEvent, Input, InputEvent,
        SelectEvent, SelectItem, SelectList, SettingItem, SettingSubmenu, SettingsList,
        SettingsListEvent, SettingsListOptions, Text, rendered_text_rows,
    };
    use crate::{KeyCode, KeyEvent, KeyModifiers, RenderedLine};

    #[test]
    fn input_handles_grapheme_backspace() {
        let mut input = Input::new();
        input.set_value("a🦀");
        let event = input.handle_key(&KeyEvent::new(KeyCode::Backspace));
        assert_eq!(event, InputEvent::Changed);
        assert_eq!(input.get_value(), "a");
    }

    #[test]
    fn input_submits_current_value() {
        let mut input = Input::new();
        input.set_value("hello");
        let event = input.handle_key(&KeyEvent::new(KeyCode::Enter));
        assert_eq!(event, InputEvent::Submitted("hello".to_string()));
    }

    #[test]
    fn editor_submit_returns_current_buffer() {
        let mut editor = Editor::new();
        editor.set_text("hello\nworld");
        assert_eq!(
            editor.submit(),
            EditorEvent::Submitted("hello\nworld".to_string())
        );
    }

    #[test]
    fn select_list_wraps_navigation_and_selects() {
        let mut select_list = SelectList::new(
            vec![
                SelectItem {
                    value: "a".to_string(),
                    label: "Alpha".to_string(),
                    description: None,
                },
                SelectItem {
                    value: "b".to_string(),
                    label: "Beta".to_string(),
                    description: None,
                },
            ],
            5,
        );

        assert_eq!(
            select_list.handle_key(&KeyEvent::new(KeyCode::Up)),
            SelectEvent::Changed
        );
        assert_eq!(
            select_list.handle_key(&KeyEvent::new(KeyCode::Enter)),
            SelectEvent::Selected(SelectItem {
                value: "b".to_string(),
                label: "Beta".to_string(),
                description: None,
            })
        );
    }

    #[test]
    fn select_list_helper_methods_match_navigation_semantics() {
        let mut select_list = SelectList::new(
            vec![
                SelectItem {
                    value: "a".to_string(),
                    label: "Alpha".to_string(),
                    description: None,
                },
                SelectItem {
                    value: "b".to_string(),
                    label: "Beta".to_string(),
                    description: None,
                },
            ],
            5,
        );

        assert_eq!(select_list.select_previous(), SelectEvent::Changed);
        assert_eq!(
            select_list.confirm_selection(),
            SelectEvent::Selected(SelectItem {
                value: "b".to_string(),
                label: "Beta".to_string(),
                description: None,
            })
        );
        assert_eq!(select_list.select_next(), SelectEvent::Changed);
        assert_eq!(select_list.cancel(), SelectEvent::Cancelled);
    }

    #[test]
    fn select_list_accepts_space_and_ctrl_c_cancel() {
        let mut select_list = SelectList::new(
            vec![
                SelectItem {
                    value: "a".to_string(),
                    label: "Alpha".to_string(),
                    description: None,
                },
                SelectItem {
                    value: "b".to_string(),
                    label: "Beta".to_string(),
                    description: None,
                },
            ],
            5,
        );

        assert_eq!(
            select_list.handle_key(&KeyEvent::new(KeyCode::Char(' '))),
            SelectEvent::Selected(SelectItem {
                value: "a".to_string(),
                label: "Alpha".to_string(),
                description: None,
            })
        );
        assert_eq!(
            select_list.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Char('c'),
                KeyModifiers::CTRL
            )),
            SelectEvent::Cancelled
        );
    }

    #[test]
    fn select_list_replace_items_preserves_selected_value() {
        let mut select_list = SelectList::new(
            vec![
                SelectItem {
                    value: "a".to_string(),
                    label: "Alpha".to_string(),
                    description: None,
                },
                SelectItem {
                    value: "b".to_string(),
                    label: "Beta".to_string(),
                    description: None,
                },
            ],
            5,
        );

        select_list.set_selected_value("b");
        select_list.replace_items_preserving_selection(
            vec![
                SelectItem {
                    value: "c".to_string(),
                    label: "Gamma".to_string(),
                    description: None,
                },
                SelectItem {
                    value: "b".to_string(),
                    label: "Beta".to_string(),
                    description: None,
                },
            ],
            Some("b"),
        );

        assert_eq!(select_list.selected_value(), Some("b"));
        assert!(select_list.contains_value("b"));
    }

    #[test]
    fn select_list_renders_inline_descriptions_and_scroll_info() {
        let select_list = SelectList::new(
            vec![
                SelectItem {
                    value: "a".to_string(),
                    label: "Alpha".to_string(),
                    description: Some("First item\nwith extra details".to_string()),
                },
                SelectItem {
                    value: "b".to_string(),
                    label: "Beta".to_string(),
                    description: Some("Second item".to_string()),
                },
                SelectItem {
                    value: "c".to_string(),
                    label: "Gamma".to_string(),
                    description: Some("Third item".to_string()),
                },
            ],
            2,
        );

        let output = select_list.render(60);
        let lines = output
            .lines
            .into_iter()
            .filter_map(|line| match line {
                RenderedLine::Text(text) => Some(text),
                RenderedLine::Image(_) => None,
            })
            .collect::<Vec<_>>();
        assert!(lines.iter().any(|line| line.contains("→ Alpha")));
        assert!(lines.iter().any(|line| line.contains("First item")));
        assert!(lines.iter().any(|line| line.contains("(1/3)")));
    }

    #[test]
    fn selector_row_helpers_render_on_short_widths() {
        let selected = SelectList::render_item_line(
            "→ ",
            "Alpha Beta Gamma",
            Some("Detailed description"),
            20,
            true,
        );
        let selected_text = match selected {
            RenderedLine::Text(text) => text,
            RenderedLine::Image(_) => panic!("expected text line"),
        };
        assert!(selected_text.contains("→"));
        assert!(selected_text.contains("Alpha"));

        let settings =
            SettingsList::render_item_line("  ", "Editor padding", "medium", 20, false, 14);
        let settings_text = match settings {
            RenderedLine::Text(text) => text,
            RenderedLine::Image(_) => panic!("expected text line"),
        };
        assert!(settings_text.contains("Editor padding"));
    }

    #[test]
    fn selector_helpers_expose_visible_bounds_and_footer_hints() {
        let mut select_list = SelectList::new(
            vec![
                SelectItem {
                    value: "a".to_string(),
                    label: "Alpha".to_string(),
                    description: None,
                },
                SelectItem {
                    value: "b".to_string(),
                    label: "Beta".to_string(),
                    description: None,
                },
                SelectItem {
                    value: "c".to_string(),
                    label: "Gamma".to_string(),
                    description: None,
                },
            ],
            1,
        );
        select_list.set_selected_index(1);

        assert_eq!(select_list.visible_bounds(), (1, 2));
        assert_eq!(select_list.scroll_status(), Some("  (2/3)".to_string()));
        assert_eq!(
            select_list.footer_hint(),
            "  Enter/Space to change · Esc to cancel"
        );

        let settings_list = SettingsList::with_options(
            vec![SettingItem {
                id: "theme".to_string(),
                label: "Theme".to_string(),
                description: None,
                current_value: "light".to_string(),
                values: vec!["light".to_string(), "dark".to_string()],
                submenu: None,
            }],
            1,
            SettingsListOptions {
                enable_search: true,
            },
        );

        assert_eq!(settings_list.visible_bounds(), (0, 1));
        assert_eq!(
            settings_list.footer_hint(),
            "  Type to search · Enter/Space to change · Esc to cancel"
        );
    }

    #[test]
    fn select_list_shell_mode_can_add_footer_support() {
        let select_list = SelectList::new(
            vec![SelectItem {
                value: "a".to_string(),
                label: "Alpha".to_string(),
                description: None,
            }],
            5,
        );

        let output = select_list.render_shell(24, true);
        let rows = rendered_text_rows(&output);
        assert!(rows.iter().any(|line| line.contains("Alpha")));
        assert!(
            rows.iter()
                .any(|line| line.contains("Enter/Space to change"))
        );
    }

    #[test]
    fn settings_list_compact_spacing_uses_dividers_on_narrow_widths() {
        let settings_list = SettingsList::with_options(
            vec![SettingItem {
                id: "theme".to_string(),
                label: "Theme".to_string(),
                description: Some("Color theme".to_string()),
                current_value: "light".to_string(),
                values: vec!["light".to_string(), "dark".to_string()],
                submenu: None,
            }],
            5,
            SettingsListOptions {
                enable_search: true,
            },
        );

        let output = settings_list.render(32);
        let rows = rendered_text_rows(&output);
        assert!(rows.iter().any(|line| line.contains("Theme")));
        assert!(rows.iter().any(|line| line.contains("─")));
    }

    #[test]
    fn box_widget_clips_tight_widths_without_empty_shell_padding() {
        let mut widget = BoxWidget::new(3, 1);
        widget.add_child(Text::new("hello"));

        let output = widget.render(2);
        let rows = rendered_text_rows(&output);
        assert!(rows.is_empty());
    }

    #[test]
    fn selector_search_prompt_constant_matches_plain_arrow_prompt() {
        assert_eq!(Input::selector_search_prompt(), "> ");
    }

    #[test]
    fn text_input_renders_cursor() {
        let input = Input::new();
        let output = input.render(20);
        assert_eq!(output.cursor.expect("cursor").col, 2);
    }

    #[test]
    fn search_input_prompts_render_as_plain_search_row() {
        let input = Input::with_prompt("Search: ");
        let output = input.render(20);
        let text = match &output.lines[0] {
            RenderedLine::Text(text) => text,
            RenderedLine::Image(_) => panic!("expected text line"),
        };
        assert!(text.starts_with("> "));
        assert!(!text.starts_with("Search:"));
        assert_eq!(output.cursor.expect("cursor").col, 2);
    }

    #[test]
    fn editor_paste_path_inserts_separator_after_word_char() {
        let mut editor = Editor::new();
        editor.set_text("open");
        editor.set_cursor(0, "open".len());

        assert_eq!(
            editor.handle_key(&KeyEvent::new(KeyCode::Paste("/tmp/file.txt".to_string()))),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "open /tmp/file.txt");
        assert_eq!(editor.cursor(), (0, "open /tmp/file.txt".len()));
    }

    #[test]
    fn editor_render_composer_accepts_distinct_border_rules() {
        let mut editor = Editor::with_prompt("");
        editor.set_text("body");

        let output = editor.render_composer_with_rules(
            12,
            None,
            ComposerBorderRules::new("normal-top", "thinking-bottom"),
            None,
        );

        let text = output
            .lines
            .into_iter()
            .filter_map(|line| match line {
                RenderedLine::Text(text) => Some(text),
                RenderedLine::Image(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(text.first().map(|line| line.trim_end()), Some("normal-top"));
        assert_eq!(
            text.last().map(|line| line.trim_end()),
            Some("thinking-bot")
        );
    }

    #[test]
    fn editor_render_composer_appends_attachment_after_bottom_border() {
        let mut editor = Editor::with_prompt("");
        editor.set_text("body");

        let output = editor.render_composer_with_rules(
            12,
            None,
            ComposerBorderRules::new("normal-top", "thinking-bottom"),
            Some(crate::RenderOutput {
                lines: vec![RenderedLine::Text("attached".to_string())],
                cursor: Some(crate::CursorPosition { row: 0, col: 3 }),
            }),
        );

        let cursor = output.cursor;
        let text = output
            .lines
            .into_iter()
            .filter_map(|line| match line {
                RenderedLine::Text(text) => Some(text.trim_end().to_string()),
                RenderedLine::Image(_) => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(text, vec!["normal-top", "body", "thinking-bot", "attached"]);
        assert_eq!(cursor, Some(crate::CursorPosition { row: 1, col: 4 }));
    }

    #[test]
    fn editor_handles_multiline_editing_and_grapheme_deletion() {
        let mut editor = Editor::new();

        assert_eq!(
            editor.handle_key(&KeyEvent::new(KeyCode::Paste("hi".to_string()))),
            EditorEvent::Changed
        );
        assert_eq!(
            editor.handle_key(&KeyEvent::new(KeyCode::Enter)),
            EditorEvent::Changed
        );
        assert_eq!(
            editor.handle_key(&KeyEvent::new(KeyCode::Paste("🦀x".to_string()))),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "hi\n🦀x");
        assert_eq!(editor.cursor(), (1, "🦀x".len()));

        assert_eq!(
            editor.handle_key(&KeyEvent::new(KeyCode::Left)),
            EditorEvent::Changed
        );
        assert_eq!(
            editor.handle_key(&KeyEvent::new(KeyCode::Backspace)),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "hi\nx");
        assert_eq!(editor.cursor(), (1, 0));

        assert_eq!(
            editor.handle_key(&KeyEvent::new(KeyCode::Backspace)),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "hix");
        assert_eq!(editor.cursor(), (0, 2));
    }

    #[test]
    fn editor_history_navigation_restores_current_buffer() {
        let mut editor = Editor::new();
        editor.add_history_entry("first");
        editor.add_history_entry("second");
        editor.set_text("draft");

        assert_eq!(editor.history_previous(), EditorEvent::Changed);
        assert_eq!(editor.get_text(), "second");

        assert_eq!(editor.history_previous(), EditorEvent::Changed);
        assert_eq!(editor.get_text(), "first");

        assert_eq!(editor.history_next(), EditorEvent::Changed);
        assert_eq!(editor.get_text(), "second");

        assert_eq!(editor.history_next(), EditorEvent::Changed);
        assert_eq!(editor.get_text(), "draft");
    }

    #[test]
    fn editor_supports_word_navigation_and_deletion() {
        let mut editor = Editor::new();
        editor.set_text("alpha, beta gamma");
        editor.set_cursor(0, "alpha, beta ".len());

        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(KeyCode::Right, KeyModifiers::ALT)),
            EditorEvent::Changed
        );
        assert_eq!(editor.cursor(), (0, "alpha, beta gamma".len()));

        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Backspace,
                KeyModifiers::ALT
            )),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "alpha, beta ");

        editor.set_cursor(0, "alpha,".len());
        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Char('d'),
                KeyModifiers::ALT
            )),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "alpha, ");
    }

    #[test]
    fn editor_wraps_with_prompt_prefix_and_positions_cursor() {
        let mut editor = Editor::with_prompt("> ");
        editor.set_text("abcdefg");
        editor.set_cursor(0, 5);

        let output = editor.render(6);
        let lines = output
            .lines
            .into_iter()
            .filter_map(|line| match line {
                RenderedLine::Text(text) => Some(text),
                RenderedLine::Image(_) => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(lines, vec!["> abcd".to_string(), "  efg ".to_string()]);
        assert_eq!(
            output.cursor,
            Some(crate::CursorPosition { row: 1, col: 3 })
        );
    }

    #[test]
    fn editor_paste_normalizes_newlines_and_advances_cursor() {
        let mut editor = Editor::new();
        let event = editor.handle_key(&KeyEvent::new(KeyCode::Paste(
            "one\r\ntwo\rthree".to_string(),
        )));

        assert_eq!(event, EditorEvent::Changed);
        assert_eq!(editor.get_text(), "one\ntwo\nthree");
        assert_eq!(editor.cursor(), (2, 5));
    }

    #[test]
    fn editor_supports_undo_and_yank_cycle_shortcuts() {
        let mut editor = Editor::new();
        editor.set_text("alpha beta");
        editor.set_cursor(0, "alpha beta".len());

        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Backspace,
                KeyModifiers::ALT
            )),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "alpha ");

        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Char('y'),
                KeyModifiers::CTRL
            )),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "alpha beta");

        editor.set_cursor(0, "alpha beta".len());
        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Backspace,
                KeyModifiers::ALT
            )),
            EditorEvent::Changed
        );
        editor.set_cursor(0, "alpha ".len());
        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Char('w'),
                KeyModifiers::CTRL
            )),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "");
        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Char('y'),
                KeyModifiers::CTRL
            )),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "alpha ");
        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Char('y'),
                KeyModifiers::ALT
            )),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "beta");

        assert_eq!(
            editor.handle_key(&KeyEvent::with_modifiers(
                KeyCode::Char('/'),
                KeyModifiers::CTRL
            )),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn settings_list_cycles_values_and_supports_search_and_submenus() {
        let mut settings_list = SettingsList::with_options(
            vec![
                SettingItem {
                    id: "theme".to_string(),
                    label: "Theme".to_string(),
                    description: Some("Color theme".to_string()),
                    current_value: "light".to_string(),
                    values: vec!["light".to_string(), "dark".to_string()],
                    submenu: None,
                },
                SettingItem {
                    id: "thinking".to_string(),
                    label: "Thinking level".to_string(),
                    description: Some("Reasoning depth".to_string()),
                    current_value: "off".to_string(),
                    values: Vec::new(),
                    submenu: Some(SettingSubmenu {
                        title: "Thinking".to_string(),
                        description: Some("Pick reasoning depth".to_string()),
                        options: vec![
                            SelectItem {
                                value: "off".to_string(),
                                label: "Off".to_string(),
                                description: None,
                            },
                            SelectItem {
                                value: "low".to_string(),
                                label: "Low".to_string(),
                                description: None,
                            },
                        ],
                        current_value: "off".to_string(),
                    }),
                },
            ],
            5,
            SettingsListOptions {
                enable_search: true,
            },
        );

        assert_eq!(
            settings_list.handle_key(&KeyEvent::new(KeyCode::Char('t'))),
            SettingsListEvent::None
        );
        assert_eq!(
            settings_list.handle_key(&KeyEvent::new(KeyCode::Char(' '))),
            SettingsListEvent::Changed {
                id: "theme".to_string(),
                value: "dark".to_string(),
            }
        );

        let render = settings_list.render(50);
        let text_lines = render
            .lines
            .into_iter()
            .filter_map(|line| match line {
                RenderedLine::Text(text) => Some(text),
                RenderedLine::Image(_) => None,
            })
            .collect::<Vec<_>>();
        assert!(text_lines.iter().any(|line| line.contains(">")));
        assert!(text_lines.iter().any(|line| line.contains("Theme")));
        assert!(text_lines.iter().any(|line| line.contains("Color theme")));
    }
}
