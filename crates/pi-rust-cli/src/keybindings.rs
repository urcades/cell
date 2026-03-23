use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pi_rust_config::get_agent_dir;
use pi_rust_tui::{Editor, EditorEvent, KeyCode, KeyEvent, KeyModifiers, SelectEvent, SelectList};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppAction {
    Interrupt,
    Clear,
    Exit,
    Suspend,
    CycleThinkingLevel,
    CycleModelForward,
    CycleModelBackward,
    SelectModel,
    ExpandTools,
    ToggleThinking,
    ToggleSessionNamedFilter,
    ToggleSessionScope,
    ToggleSessionSort,
    ToggleSessionPath,
    RenameSession,
    DeleteSession,
    EditTreeLabel,
    ExternalEditor,
    FollowUp,
    Dequeue,
    PasteImage,
    NewSession,
    Tree,
    Fork,
    Resume,
}

impl AppAction {
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "interrupt" => Some(Self::Interrupt),
            "clear" => Some(Self::Clear),
            "exit" => Some(Self::Exit),
            "suspend" => Some(Self::Suspend),
            "cycleThinkingLevel" => Some(Self::CycleThinkingLevel),
            "cycleModelForward" => Some(Self::CycleModelForward),
            "cycleModelBackward" => Some(Self::CycleModelBackward),
            "selectModel" => Some(Self::SelectModel),
            "expandTools" => Some(Self::ExpandTools),
            "toggleThinking" => Some(Self::ToggleThinking),
            "toggleSessionNamedFilter" => Some(Self::ToggleSessionNamedFilter),
            "toggleSessionScope" => Some(Self::ToggleSessionScope),
            "toggleSessionSort" => Some(Self::ToggleSessionSort),
            "toggleSessionPath" => Some(Self::ToggleSessionPath),
            "renameSession" => Some(Self::RenameSession),
            "deleteSession" => Some(Self::DeleteSession),
            "editTreeLabel" => Some(Self::EditTreeLabel),
            "externalEditor" => Some(Self::ExternalEditor),
            "followUp" => Some(Self::FollowUp),
            "dequeue" => Some(Self::Dequeue),
            "pasteImage" => Some(Self::PasteImage),
            "newSession" => Some(Self::NewSession),
            "tree" => Some(Self::Tree),
            "fork" => Some(Self::Fork),
            "resume" => Some(Self::Resume),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EditorAction {
    CursorUp,
    CursorDown,
    CursorLeft,
    CursorRight,
    CursorWordLeft,
    CursorWordRight,
    CursorLineStart,
    CursorLineEnd,
    DeleteCharBackward,
    DeleteCharForward,
    DeleteWordBackward,
    DeleteWordForward,
    DeleteToLineStart,
    DeleteToLineEnd,
    NewLine,
    Submit,
    Tab,
    SelectUp,
    SelectDown,
    SelectConfirm,
    SelectCancel,
    Yank,
    YankPop,
    Undo,
}

impl EditorAction {
    fn from_str(value: &str) -> Option<Self> {
        match value {
            "cursorUp" => Some(Self::CursorUp),
            "cursorDown" => Some(Self::CursorDown),
            "cursorLeft" => Some(Self::CursorLeft),
            "cursorRight" => Some(Self::CursorRight),
            "cursorWordLeft" => Some(Self::CursorWordLeft),
            "cursorWordRight" => Some(Self::CursorWordRight),
            "cursorLineStart" => Some(Self::CursorLineStart),
            "cursorLineEnd" => Some(Self::CursorLineEnd),
            "deleteCharBackward" => Some(Self::DeleteCharBackward),
            "deleteCharForward" => Some(Self::DeleteCharForward),
            "deleteWordBackward" => Some(Self::DeleteWordBackward),
            "deleteWordForward" => Some(Self::DeleteWordForward),
            "deleteToLineStart" => Some(Self::DeleteToLineStart),
            "deleteToLineEnd" => Some(Self::DeleteToLineEnd),
            "newLine" => Some(Self::NewLine),
            "submit" => Some(Self::Submit),
            "tab" => Some(Self::Tab),
            "selectUp" => Some(Self::SelectUp),
            "selectDown" => Some(Self::SelectDown),
            "selectConfirm" => Some(Self::SelectConfirm),
            "selectCancel" => Some(Self::SelectCancel),
            "yank" => Some(Self::Yank),
            "yankPop" => Some(Self::YankPop),
            "undo" => Some(Self::Undo),
            _ => None,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn apply_to_editor(self, editor: &mut Editor) -> EditorEvent {
        match self {
            Self::CursorUp => editor_changed(editor.move_up()),
            Self::CursorDown => editor_changed(editor.move_down()),
            Self::CursorLeft => editor_changed(editor.move_left()),
            Self::CursorRight => editor_changed(editor.move_right()),
            Self::CursorWordLeft => editor_changed(editor.move_word_left()),
            Self::CursorWordRight => editor_changed(editor.move_word_right()),
            Self::CursorLineStart => editor_changed(editor.move_home()),
            Self::CursorLineEnd => editor_changed(editor.move_end()),
            Self::DeleteCharBackward => editor_changed(editor.backspace()),
            Self::DeleteCharForward => editor_changed(editor.delete()),
            Self::DeleteWordBackward => editor_changed(editor.delete_word_backward()),
            Self::DeleteWordForward => editor_changed(editor.delete_word_forward()),
            Self::DeleteToLineStart => editor_changed(editor.delete_to_line_start()),
            Self::DeleteToLineEnd => editor_changed(editor.delete_to_line_end()),
            Self::NewLine => editor_changed(editor.insert_newline()),
            Self::Submit => editor.submit(),
            Self::Yank => editor_changed(editor.yank()),
            Self::YankPop => editor_changed(editor.yank_pop()),
            Self::Undo => editor_changed(editor.undo()),
            Self::Tab
            | Self::SelectUp
            | Self::SelectDown
            | Self::SelectConfirm
            | Self::SelectCancel => EditorEvent::None,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn apply_to_select_list(self, list: &mut SelectList) -> SelectEvent {
        match self {
            Self::SelectUp => list.select_previous(),
            Self::SelectDown => list.select_next(),
            Self::SelectConfirm => list.confirm_selection(),
            Self::SelectCancel => list.cancel(),
            _ => SelectEvent::None,
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptEditorInput {
    Action(EditorAction),
    TriggerAutocomplete,
    InsertText(String),
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptAutocompleteInput {
    NavigateUp,
    NavigateDown,
    ConfirmSelection,
    Cancel,
    AcceptCompletion,
}

impl PromptAutocompleteInput {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn apply_to_select_list(self, list: &mut SelectList) -> Option<SelectEvent> {
        match self {
            Self::NavigateUp => Some(list.select_previous()),
            Self::NavigateDown => Some(list.select_next()),
            Self::ConfirmSelection => Some(list.confirm_selection()),
            Self::Cancel => Some(list.cancel()),
            Self::AcceptCompletion => None,
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
const PROMPT_EDITOR_ACTIONS: [EditorAction; 19] = [
    EditorAction::Submit,
    EditorAction::NewLine,
    EditorAction::Undo,
    EditorAction::Yank,
    EditorAction::YankPop,
    EditorAction::CursorLeft,
    EditorAction::CursorRight,
    EditorAction::CursorWordLeft,
    EditorAction::CursorWordRight,
    EditorAction::CursorLineStart,
    EditorAction::CursorLineEnd,
    EditorAction::DeleteCharBackward,
    EditorAction::DeleteCharForward,
    EditorAction::DeleteWordBackward,
    EditorAction::DeleteWordForward,
    EditorAction::DeleteToLineStart,
    EditorAction::DeleteToLineEnd,
    EditorAction::CursorUp,
    EditorAction::CursorDown,
];

#[cfg_attr(not(test), allow(dead_code))]
const PROMPT_AUTOCOMPLETE_ACTIONS: [EditorAction; 4] = [
    EditorAction::SelectCancel,
    EditorAction::SelectUp,
    EditorAction::SelectDown,
    EditorAction::SelectConfirm,
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyBinding {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl KeyBinding {
    fn parse(value: &str) -> Option<Self> {
        let mut modifiers = KeyModifiers::NONE;
        let mut code = None;

        for part in value.split('+') {
            let token = part.trim().to_lowercase();
            match token.as_str() {
                "ctrl" | "control" => modifiers.ctrl = true,
                "alt" | "option" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                "enter" => code = Some(KeyCode::Enter),
                "escape" | "esc" => code = Some(KeyCode::Escape),
                "tab" => code = Some(KeyCode::Tab),
                "backtab" | "shift+tab" => {
                    code = Some(KeyCode::BackTab);
                    modifiers.shift = true;
                }
                "up" => code = Some(KeyCode::Up),
                "down" => code = Some(KeyCode::Down),
                "left" => code = Some(KeyCode::Left),
                "right" => code = Some(KeyCode::Right),
                "home" => code = Some(KeyCode::Home),
                "end" => code = Some(KeyCode::End),
                "backspace" => code = Some(KeyCode::Backspace),
                "delete" => code = Some(KeyCode::Delete),
                value if value.len() == 1 => code = value.chars().next().map(KeyCode::Char),
                _ => return None,
            }
        }

        if matches!(code, Some(KeyCode::Tab)) && modifiers.shift {
            code = Some(KeyCode::BackTab);
        }

        code.map(|code| Self { code, modifiers })
    }

    fn matches(&self, event: &KeyEvent) -> bool {
        if self.code == event.code && self.modifiers == event.modifiers {
            return true;
        }

        let event_is_backtab = matches!(event.code, KeyCode::BackTab)
            || matches!(event.code, KeyCode::Tab) && event.modifiers.shift;
        matches!(&self.code, KeyCode::BackTab)
            && event_is_backtab
            && self.modifiers.ctrl == event.modifiers.ctrl
            && self.modifiers.alt == event.modifiers.alt
            && self.modifiers.shift
    }

    fn display_string(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.ctrl {
            parts.push("ctrl");
        }
        if self.modifiers.alt {
            parts.push("alt");
        }
        if self.modifiers.shift {
            parts.push("shift");
        }

        let key = match &self.code {
            KeyCode::Enter => "enter".to_string(),
            KeyCode::Escape => "escape".to_string(),
            KeyCode::Tab | KeyCode::BackTab => "tab".to_string(),
            KeyCode::Up => "up".to_string(),
            KeyCode::Down => "down".to_string(),
            KeyCode::Left => "left".to_string(),
            KeyCode::Right => "right".to_string(),
            KeyCode::Home => "home".to_string(),
            KeyCode::End => "end".to_string(),
            KeyCode::PasteStart => "paste-start".to_string(),
            KeyCode::PasteEnd => "paste-end".to_string(),
            KeyCode::Backspace => "backspace".to_string(),
            KeyCode::Delete => "delete".to_string(),
            KeyCode::Char(ch) => ch.to_string(),
            KeyCode::Paste(value) => value.clone(),
            KeyCode::Unknown(value) => value.clone(),
        };

        if parts.is_empty() {
            key
        } else {
            format!("{}+{}", parts.join("+"), key)
        }
    }
}

fn canonicalize_binding_values(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| {
            KeyBinding::parse(&value)
                .map(|binding| binding.display_string())
                .unwrap_or(value)
        })
        .collect()
}

fn default_bindings() -> BTreeMap<AppAction, Vec<String>> {
    BTreeMap::from([
        (AppAction::Interrupt, vec!["escape".to_string()]),
        (AppAction::Clear, vec!["ctrl+c".to_string()]),
        (AppAction::Exit, vec!["ctrl+d".to_string()]),
        (AppAction::Suspend, vec!["ctrl+z".to_string()]),
        (AppAction::CycleThinkingLevel, vec!["shift+tab".to_string()]),
        (AppAction::CycleModelForward, vec!["ctrl+p".to_string()]),
        (
            AppAction::CycleModelBackward,
            vec!["ctrl+shift+p".to_string()],
        ),
        (AppAction::SelectModel, vec!["ctrl+l".to_string()]),
        (AppAction::ExpandTools, vec!["ctrl+o".to_string()]),
        (AppAction::ToggleThinking, vec!["ctrl+t".to_string()]),
        (
            AppAction::ToggleSessionNamedFilter,
            vec!["ctrl+n".to_string()],
        ),
        (AppAction::ToggleSessionScope, vec!["tab".to_string()]),
        (AppAction::ToggleSessionSort, vec!["ctrl+s".to_string()]),
        (AppAction::ToggleSessionPath, vec!["ctrl+p".to_string()]),
        (AppAction::RenameSession, vec!["ctrl+r".to_string()]),
        (AppAction::DeleteSession, vec!["ctrl+d".to_string()]),
        (AppAction::EditTreeLabel, vec!["shift+l".to_string()]),
        (AppAction::ExternalEditor, vec!["ctrl+g".to_string()]),
        (AppAction::FollowUp, vec!["alt+enter".to_string()]),
        (AppAction::Dequeue, vec!["alt+up".to_string()]),
        (AppAction::PasteImage, vec!["ctrl+v".to_string()]),
        (AppAction::NewSession, Vec::new()),
        (AppAction::Tree, Vec::new()),
        (AppAction::Fork, Vec::new()),
        (AppAction::Resume, Vec::new()),
    ])
}

fn default_editor_bindings() -> BTreeMap<EditorAction, Vec<String>> {
    BTreeMap::from([
        (EditorAction::CursorUp, vec!["up".to_string()]),
        (EditorAction::CursorDown, vec!["down".to_string()]),
        (
            EditorAction::CursorLeft,
            vec!["left".to_string(), "ctrl+b".to_string()],
        ),
        (
            EditorAction::CursorRight,
            vec!["right".to_string(), "ctrl+f".to_string()],
        ),
        (
            EditorAction::CursorWordLeft,
            vec![
                "alt+left".to_string(),
                "ctrl+left".to_string(),
                "alt+b".to_string(),
            ],
        ),
        (
            EditorAction::CursorWordRight,
            vec![
                "alt+right".to_string(),
                "ctrl+right".to_string(),
                "alt+f".to_string(),
            ],
        ),
        (
            EditorAction::CursorLineStart,
            vec!["home".to_string(), "ctrl+a".to_string()],
        ),
        (
            EditorAction::CursorLineEnd,
            vec!["end".to_string(), "ctrl+e".to_string()],
        ),
        (
            EditorAction::DeleteCharBackward,
            vec!["backspace".to_string()],
        ),
        (
            EditorAction::DeleteCharForward,
            vec!["delete".to_string(), "ctrl+d".to_string()],
        ),
        (
            EditorAction::DeleteWordBackward,
            vec!["ctrl+w".to_string(), "alt+backspace".to_string()],
        ),
        (
            EditorAction::DeleteWordForward,
            vec!["alt+d".to_string(), "alt+delete".to_string()],
        ),
        (EditorAction::DeleteToLineStart, vec!["ctrl+u".to_string()]),
        (EditorAction::DeleteToLineEnd, vec!["ctrl+k".to_string()]),
        (EditorAction::NewLine, vec!["shift+enter".to_string()]),
        (EditorAction::Submit, vec!["enter".to_string()]),
        (EditorAction::Tab, vec!["tab".to_string()]),
        (EditorAction::SelectUp, vec!["up".to_string()]),
        (EditorAction::SelectDown, vec!["down".to_string()]),
        (EditorAction::SelectConfirm, vec!["enter".to_string()]),
        (
            EditorAction::SelectCancel,
            vec!["escape".to_string(), "ctrl+c".to_string()],
        ),
        (EditorAction::Yank, vec!["ctrl+y".to_string()]),
        (EditorAction::YankPop, vec!["alt+y".to_string()]),
        (EditorAction::Undo, vec!["ctrl+-".to_string()]),
    ])
}

#[derive(Clone, Debug)]
pub struct KeybindingsManager {
    app_bindings: BTreeMap<AppAction, Vec<KeyBinding>>,
    app_display: BTreeMap<AppAction, Vec<String>>,
    editor_bindings: BTreeMap<EditorAction, Vec<KeyBinding>>,
    editor_display: BTreeMap<EditorAction, Vec<String>>,
}

impl KeybindingsManager {
    pub fn create(agent_dir: Option<PathBuf>) -> Self {
        let path = agent_dir
            .unwrap_or_else(get_agent_dir)
            .join("keybindings.json");
        let (app_overrides, editor_overrides) = load_overrides(&path);
        Self::from_overrides(app_overrides, editor_overrides)
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::from_overrides(BTreeMap::new(), BTreeMap::new())
    }

    fn from_overrides(
        app_overrides: BTreeMap<AppAction, Vec<String>>,
        editor_overrides: BTreeMap<EditorAction, Vec<String>>,
    ) -> Self {
        let mut app_display = default_bindings();
        for (action, values) in app_overrides {
            app_display.insert(action, values);
        }
        let app_display = app_display
            .into_iter()
            .map(|(action, values)| (action, canonicalize_binding_values(values)))
            .collect::<BTreeMap<_, _>>();

        let app_bindings = app_display
            .iter()
            .map(|(action, values)| {
                let parsed = values
                    .iter()
                    .filter_map(|value| KeyBinding::parse(value))
                    .collect::<Vec<_>>();
                (*action, parsed)
            })
            .collect::<BTreeMap<_, _>>();

        let mut editor_display = default_editor_bindings();
        for (action, values) in editor_overrides {
            editor_display.insert(action, values);
        }
        let editor_display = editor_display
            .into_iter()
            .map(|(action, values)| (action, canonicalize_binding_values(values)))
            .collect::<BTreeMap<_, _>>();

        let editor_bindings = editor_display
            .iter()
            .map(|(action, values)| {
                let parsed = values
                    .iter()
                    .filter_map(|value| KeyBinding::parse(value))
                    .collect::<Vec<_>>();
                (*action, parsed)
            })
            .collect::<BTreeMap<_, _>>();

        Self {
            app_bindings,
            app_display,
            editor_bindings,
            editor_display,
        }
    }

    pub fn matches(&self, event: &KeyEvent, action: AppAction) -> bool {
        self.app_bindings
            .get(&action)
            .into_iter()
            .flatten()
            .any(|binding| binding.matches(event))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn resolve_app_action_in(
        &self,
        event: &KeyEvent,
        actions: &[AppAction],
    ) -> Option<AppAction> {
        actions
            .iter()
            .copied()
            .find(|action| self.matches(event, *action))
    }

    pub fn display(&self, action: AppAction) -> String {
        self.app_display
            .get(&action)
            .cloned()
            .unwrap_or_default()
            .join(" / ")
    }

    pub fn matches_editor(&self, event: &KeyEvent, action: EditorAction) -> bool {
        self.editor_bindings
            .get(&action)
            .into_iter()
            .flatten()
            .any(|binding| binding.matches(event))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn resolve_editor_action_in(
        &self,
        event: &KeyEvent,
        actions: &[EditorAction],
    ) -> Option<EditorAction> {
        actions
            .iter()
            .copied()
            .find(|action| self.matches_editor(event, *action))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn resolve_prompt_editor_input(&self, event: &KeyEvent) -> Option<PromptEditorInput> {
        if let Some(action) = self.resolve_editor_action_in(event, &PROMPT_EDITOR_ACTIONS) {
            return Some(PromptEditorInput::Action(action));
        }

        if self.matches_editor(event, EditorAction::Tab) {
            return Some(PromptEditorInput::TriggerAutocomplete);
        }

        match &event.code {
            KeyCode::Paste(value) => Some(PromptEditorInput::InsertText(value.clone())),
            KeyCode::Char(ch) if !event.modifiers.ctrl && !event.modifiers.alt => {
                Some(PromptEditorInput::InsertText(ch.to_string()))
            }
            _ => None,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn resolve_prompt_autocomplete_input(
        &self,
        event: &KeyEvent,
    ) -> Option<PromptAutocompleteInput> {
        if let Some(action) = self.resolve_editor_action_in(event, &PROMPT_AUTOCOMPLETE_ACTIONS) {
            return Some(match action {
                EditorAction::SelectCancel => PromptAutocompleteInput::Cancel,
                EditorAction::SelectUp => PromptAutocompleteInput::NavigateUp,
                EditorAction::SelectDown => PromptAutocompleteInput::NavigateDown,
                EditorAction::SelectConfirm => PromptAutocompleteInput::ConfirmSelection,
                _ => unreachable!("prompt autocomplete candidates only contain select actions"),
            });
        }

        if self.matches_editor(event, EditorAction::Tab) {
            return Some(PromptAutocompleteInput::AcceptCompletion);
        }

        None
    }

    pub fn display_editor(&self, action: EditorAction) -> String {
        self.editor_display
            .get(&action)
            .cloned()
            .unwrap_or_default()
            .join(" / ")
    }
}

fn load_overrides(
    path: &Path,
) -> (
    BTreeMap<AppAction, Vec<String>>,
    BTreeMap<EditorAction, Vec<String>>,
) {
    let Ok(content) = fs::read_to_string(path) else {
        return (BTreeMap::new(), BTreeMap::new());
    };
    let Ok(value) = serde_json::from_str::<Value>(&content) else {
        return (BTreeMap::new(), BTreeMap::new());
    };
    let Some(map) = value.as_object() else {
        return (BTreeMap::new(), BTreeMap::new());
    };

    let mut app_overrides = BTreeMap::new();
    let mut editor_overrides = BTreeMap::new();
    for (key, value) in map {
        let bindings = match value {
            Value::String(single) => vec![single.clone()],
            Value::Array(values) => values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>(),
            _ => continue,
        };
        if let Some(action) = AppAction::from_str(key) {
            app_overrides.insert(action, bindings.clone());
        }
        if let Some(action) = EditorAction::from_str(key) {
            editor_overrides.insert(action, bindings);
        }
    }
    (app_overrides, editor_overrides)
}

#[cfg_attr(not(test), allow(dead_code))]
fn editor_changed(changed: bool) -> EditorEvent {
    if changed {
        EditorEvent::Changed
    } else {
        EditorEvent::None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppAction, EditorAction, KeybindingsManager, PromptAutocompleteInput, PromptEditorInput,
    };
    use pi_rust_tui::{
        Editor, EditorEvent, KeyCode, KeyEvent, KeyModifiers, SelectEvent, SelectItem, SelectList,
    };

    #[test]
    fn defaults_match_expected_actions() {
        let manager = KeybindingsManager::in_memory();
        assert!(manager.matches(
            &KeyEvent::with_modifiers(KeyCode::Char('l'), KeyModifiers::CTRL),
            AppAction::SelectModel,
        ));
        assert!(manager.matches(
            &KeyEvent::with_modifiers(KeyCode::Enter, KeyModifiers::ALT),
            AppAction::FollowUp,
        ));
        assert!(manager.matches(&KeyEvent::new(KeyCode::Tab), AppAction::ToggleSessionScope,));
        assert!(manager.matches(
            &KeyEvent::with_modifiers(KeyCode::Tab, KeyModifiers::SHIFT),
            AppAction::CycleThinkingLevel,
        ));
        assert!(manager.matches(
            &KeyEvent::new(KeyCode::BackTab),
            AppAction::CycleThinkingLevel,
        ));
        assert!(manager.matches(
            &KeyEvent::with_modifiers(KeyCode::Tab, KeyModifiers::SHIFT),
            AppAction::CycleThinkingLevel,
        ));
        assert_eq!(manager.display(AppAction::CycleModelForward), "ctrl+p");
        assert_eq!(
            manager.display(AppAction::CycleModelBackward),
            "ctrl+shift+p"
        );
        assert_eq!(
            manager.resolve_app_action_in(
                &KeyEvent::with_modifiers(KeyCode::Char('z'), KeyModifiers::CTRL),
                &[AppAction::Suspend, AppAction::Clear],
            ),
            Some(AppAction::Suspend)
        );
        assert_eq!(
            manager.resolve_app_action_in(
                &KeyEvent::with_modifiers(
                    KeyCode::Char('p'),
                    KeyModifiers {
                        ctrl: true,
                        alt: false,
                        shift: true,
                    },
                ),
                &[AppAction::CycleModelForward, AppAction::CycleModelBackward],
            ),
            Some(AppAction::CycleModelBackward)
        );
        assert_eq!(
            manager.resolve_app_action_in(
                &KeyEvent::with_modifiers(KeyCode::Char('g'), KeyModifiers::CTRL),
                &[AppAction::ExternalEditor, AppAction::PasteImage],
            ),
            Some(AppAction::ExternalEditor)
        );
        assert_eq!(
            manager.resolve_app_action_in(
                &KeyEvent::with_modifiers(KeyCode::Char('v'), KeyModifiers::CTRL),
                &[AppAction::ExternalEditor, AppAction::PasteImage],
            ),
            Some(AppAction::PasteImage)
        );
        assert!(manager.matches_editor(
            &KeyEvent::with_modifiers(KeyCode::Char('b'), KeyModifiers::CTRL),
            EditorAction::CursorLeft,
        ));
        assert!(manager.matches_editor(
            &KeyEvent::with_modifiers(KeyCode::Char('c'), KeyModifiers::CTRL),
            EditorAction::SelectCancel,
        ));
        assert_eq!(manager.display_editor(EditorAction::Submit), "enter");
        assert_eq!(manager.display(AppAction::ToggleSessionPath), "ctrl+p");
        assert_eq!(manager.display(AppAction::DeleteSession), "ctrl+d");
    }

    #[test]
    fn prompt_editor_input_resolution_handles_actions_and_text() {
        let manager = KeybindingsManager::in_memory();

        assert_eq!(
            manager.resolve_prompt_editor_input(&KeyEvent::new(KeyCode::Enter)),
            Some(PromptEditorInput::Action(EditorAction::Submit))
        );
        assert_eq!(
            manager.resolve_prompt_editor_input(&KeyEvent::with_modifiers(
                KeyCode::Enter,
                KeyModifiers::SHIFT,
            )),
            Some(PromptEditorInput::Action(EditorAction::NewLine))
        );
        assert_eq!(
            manager.resolve_prompt_editor_input(&KeyEvent::new(KeyCode::Tab)),
            Some(PromptEditorInput::TriggerAutocomplete)
        );
        assert_eq!(
            manager.resolve_prompt_editor_input(&KeyEvent::with_modifiers(
                KeyCode::Tab,
                KeyModifiers::SHIFT,
            )),
            None
        );
        assert_eq!(
            manager.resolve_prompt_editor_input(&KeyEvent::new(KeyCode::BackTab)),
            None
        );
        assert_eq!(
            manager
                .resolve_prompt_editor_input(&KeyEvent::new(KeyCode::Paste("hello".to_string(),))),
            Some(PromptEditorInput::InsertText("hello".to_string()))
        );
        assert_eq!(
            manager.resolve_prompt_editor_input(&KeyEvent::new(KeyCode::Char('x'))),
            Some(PromptEditorInput::InsertText("x".to_string()))
        );
        assert_eq!(
            manager.resolve_prompt_editor_input(&KeyEvent::with_modifiers(
                KeyCode::Char('x'),
                KeyModifiers::CTRL,
            )),
            None
        );
    }

    #[test]
    fn prompt_autocomplete_input_resolution_is_context_specific() {
        let manager = KeybindingsManager::in_memory();

        assert_eq!(
            manager.resolve_prompt_autocomplete_input(&KeyEvent::new(KeyCode::Up)),
            Some(PromptAutocompleteInput::NavigateUp)
        );
        assert_eq!(
            manager.resolve_prompt_autocomplete_input(&KeyEvent::new(KeyCode::Down)),
            Some(PromptAutocompleteInput::NavigateDown)
        );
        assert_eq!(
            manager.resolve_prompt_autocomplete_input(&KeyEvent::new(KeyCode::Enter)),
            Some(PromptAutocompleteInput::ConfirmSelection)
        );
        assert_eq!(
            manager.resolve_prompt_autocomplete_input(&KeyEvent::new(KeyCode::Escape)),
            Some(PromptAutocompleteInput::Cancel)
        );
        assert_eq!(
            manager.resolve_prompt_autocomplete_input(&KeyEvent::new(KeyCode::Tab)),
            Some(PromptAutocompleteInput::AcceptCompletion)
        );
        assert_eq!(
            manager.resolve_prompt_autocomplete_input(&KeyEvent::new(KeyCode::BackTab)),
            None
        );
    }

    #[test]
    fn editor_actions_apply_to_editor_without_interactive_dispatch() {
        let mut editor = Editor::new();
        editor.set_text("alpha beta");
        editor.set_cursor(0, "alpha beta".len());

        assert_eq!(
            EditorAction::DeleteWordBackward.apply_to_editor(&mut editor),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "alpha ");

        assert_eq!(
            EditorAction::Yank.apply_to_editor(&mut editor),
            EditorEvent::Changed
        );
        assert_eq!(editor.get_text(), "alpha beta");

        assert_eq!(
            EditorAction::Submit.apply_to_editor(&mut editor),
            EditorEvent::Submitted("alpha beta".to_string())
        );
    }

    #[test]
    fn autocomplete_inputs_apply_to_select_list() {
        let mut list = SelectList::new(
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
            PromptAutocompleteInput::NavigateUp.apply_to_select_list(&mut list),
            Some(SelectEvent::Changed)
        );
        assert_eq!(
            PromptAutocompleteInput::ConfirmSelection.apply_to_select_list(&mut list),
            Some(SelectEvent::Selected(SelectItem {
                value: "b".to_string(),
                label: "Beta".to_string(),
                description: None,
            }))
        );
        assert_eq!(
            EditorAction::SelectDown.apply_to_select_list(&mut list),
            SelectEvent::Changed
        );
        assert_eq!(
            PromptAutocompleteInput::AcceptCompletion.apply_to_select_list(&mut list),
            None
        );
    }
}
