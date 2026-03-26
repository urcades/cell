use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pi_rust_ai_core::{
    AssistantContentBlock, Message, Model, StopReason, UserContent, UserContentBlock, UserMessage,
};
use pi_rust_ai_providers::ProviderRegistry;
use pi_rust_config::{
    DoubleEscapeActionSetting, GlobalSettingChange, QueueModeSetting, SettingsScope,
    TransportSetting, get_project_config_dir, get_sessions_dir,
};
use pi_rust_core::{
    AgentControl, AgentEvent, AgentSession, ForkableUserMessage, NonInteractiveRequest, PromptRun,
    SessionTreeNode, StartupResourceNoticeSection, StartupResourceSummary, create_agent_session,
};
use pi_rust_models::{ModelRegistry, supports_xhigh};
use pi_rust_oauth::{
    AuthCredential, OAuthAuthInfo, OAuthCredentials, OAuthLoginBridge, OAuthPrompt,
    get_oauth_providers, login_oauth_provider,
};
use pi_rust_packages::{InstalledPackage, PackageConfigEntry, PackageManager};
use pi_rust_protocol::{
    QueueMode, RpcCommandLocation, RpcCommandSource, RpcSessionState, RpcSessionStats,
    RpcSlashCommand,
};
use pi_rust_resources::{
    ResourceCatalog, ResourceCatalogGroup, ResourceDiscoveryOptions, ResourceOrigin, ResourceScope,
    ScopedPath, catalog_resources_with_options,
};
use pi_rust_session::{
    SessionEntry, SessionManager, encode_session_dir_name, parse_header, parse_session_entries,
};
use pi_rust_tui::{
    Component, CursorPosition, Editor, EditorEvent, Focusable, Input, InputEvent, KeyCode,
    KeyEvent, KeyModifiers, LineDiffRenderer, ProcessTerminal, RenderAnchor, RenderOutput,
    RenderedLine, SelectEvent, SelectItem, SelectList, SettingItem, SettingSubmenu, SettingsList,
    SettingsListEvent, SettingsListOptions, Terminal, TerminalCapabilities, Text, fit_line,
    parse_input_bytes, truncate_to_width, visible_width,
};
use regex::Regex;
use serde_json::Value;
use similar::{ChangeTag, TextDiff};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use crate::keybindings::{
    AppAction, EditorAction, KeybindingsManager, PromptAutocompleteInput, PromptEditorInput,
};

#[path = "interactive/app.rs"]
mod app;
#[path = "interactive/background.rs"]
mod background;
#[path = "interactive/chrome.rs"]
mod chrome;
#[path = "interactive/config_browser.rs"]
mod config_browser;
#[path = "interactive/overlays/mod.rs"]
mod overlays;
#[path = "interactive/prompt.rs"]
mod prompt;
#[path = "interactive/transcript.rs"]
mod transcript;

use self::app::InteractiveApp;
use self::background::{
    ActiveAuthFlow, ActivePrompt, ActiveShare, AuthPromptKind, AuthUiRequest, AuthUiResponse,
    ChannelOAuthLoginBridge, ShareTaskResult, copy_to_clipboard, ensure_github_cli_ready,
    paste_clipboard_image_to_temp_file, run_share_task,
};
use self::chrome::{
    align_footer_row, append_overlay_banner, append_rule_line, clip_render_output_to_height,
    composer_max_visible_lines, format_cost, format_token_count, normalized_thinking_level,
    render_footer_panel, render_prompt_panel, startup_hint_lines, startup_notice_lines,
    startup_resource_lines,
};
#[cfg(test)]
use self::chrome::{prompt_composer_border_rules, render_prompt_autocomplete};
use self::overlays::*;
use self::prompt::{
    PromptAutocompleteKind, PromptAutocompleteState, build_prompt_autocomplete,
    prompt_autocomplete_should_submit_current_prompt, split_prompt_lines,
};
use self::transcript::{
    TranscriptRenderContext, active_tool_render_lines_with_context, apply_persistent_style,
    build_transcript_entries, latest_active_tool_panel_id, latest_transcript_tool_panel_id,
    parse_skill_block, session_selection_detail, session_transcript_lines_with_context,
    shorten_home_path,
};
#[cfg(test)]
use self::transcript::{
    active_tool_render_lines, collect_diff_lines, content_text, render_transcript_entry,
    session_transcript_lines,
};

pub fn run_interactive(
    request: NonInteractiveRequest,
    resume_picker: bool,
    providers: &ProviderRegistry,
    models: &mut ModelRegistry,
) -> Result<(), String> {
    let mut terminal = ProcessTerminal::new();
    terminal.start().map_err(|error| error.to_string())?;
    terminal
        .set_title("pi-rust")
        .map_err(|error| error.to_string())?;
    terminal.hide_cursor().map_err(|error| error.to_string())?;

    let mut renderer = LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });

    let result = run_interactive_started(
        request,
        resume_picker,
        providers,
        models,
        &mut terminal,
        &mut renderer,
    );

    let _ = renderer.clear(&mut terminal);
    let _ = terminal.show_cursor();
    let _ = terminal.stop();
    result
}

fn run_interactive_started(
    mut request: NonInteractiveRequest,
    resume_picker: bool,
    providers: &ProviderRegistry,
    models: &mut ModelRegistry,
    terminal: &mut ProcessTerminal,
    renderer: &mut LineDiffRenderer,
) -> Result<(), String> {
    if resume_picker && request.session.is_none() {
        request.continue_session = false;
        let selected = run_startup_resume_picker(&request, terminal, renderer)?;
        let Some(path) = selected else {
            return Ok(());
        };
        request.session = Some(path);
    }

    let initial_messages = request.messages.clone();
    let session =
        create_agent_session(&request, providers, models).map_err(|error| error.to_string())?;
    let shared_session = Arc::new(Mutex::new(session));
    let control = shared_session
        .lock()
        .map_err(|_| "Failed to lock interactive session".to_string())?
        .control();
    let mut app = InteractiveApp::new(
        Arc::clone(&shared_session),
        control,
        KeybindingsManager::create(None),
        request.session_dir.clone(),
        terminal.capabilities(),
        &request.cwd,
    )?;

    if request.continue_session && request.session.is_none() {
        app.status = Some("Continuing the most recent session.".to_string());
    }
    if !initial_messages.is_empty() {
        app.start_prompt(initial_messages.join("\n"))?;
    }

    let input_rx = spawn_input_reader();

    loop {
        app.poll_background()?;
        let (width, height) = terminal.size().map_err(|error| error.to_string())?;
        renderer
            .render(terminal, &app.render(width, height), width)
            .map_err(|error| error.to_string())?;

        let timeout = if app.needs_periodic_redraw() {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(250)
        };

        match input_rx.recv_timeout(timeout) {
            Ok(events) => {
                for event in events {
                    match app.handle_key(event)? {
                        LoopAction::Continue => {}
                        LoopAction::OpenExternalEditor => {
                            app.open_external_editor(terminal, renderer)?;
                        }
                        LoopAction::Suspend => {
                            suspend_process(terminal, renderer)?;
                        }
                        LoopAction::Quit => return Ok(()),
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

enum StartupResumePickerAction {
    Continue(Option<String>),
    Cancel,
    Selected(PathBuf),
}

fn run_startup_resume_picker(
    request: &NonInteractiveRequest,
    terminal: &mut ProcessTerminal,
    renderer: &mut LineDiffRenderer,
) -> Result<Option<PathBuf>, String> {
    let keybindings = KeybindingsManager::create(request.session_dir.clone());
    let mut state = build_startup_session_overlay_state(
        &request.cwd,
        request.session_dir.clone(),
        &keybindings,
        SessionScope::Current,
        SessionSortMode::Threaded,
        SessionNameFilter::All,
        false,
        None,
        None,
    )?;
    let mut status: Option<String> = None;
    let input_rx = spawn_input_reader();

    loop {
        let (width, height) = terminal.size().map_err(|error| error.to_string())?;
        let mut output = state.render(width);
        if let Some(message) = status.as_deref() {
            append_blank_lines(&mut output, width, 1);
            append_output(
                &mut output,
                Text::new(style_hint(message)).render(width),
                false,
            );
        }
        let output_line_count = output.lines.len();
        if output_line_count < height as usize {
            append_blank_lines(&mut output, width, height as usize - output_line_count);
        }
        renderer
            .render(terminal, &output, width)
            .map_err(|error| error.to_string())?;

        match input_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(events) => {
                for event in events {
                    match handle_startup_resume_picker_key(
                        &mut state,
                        &keybindings,
                        &request.cwd,
                        request.session_dir.as_deref(),
                        event,
                    )? {
                        StartupResumePickerAction::Continue(next_status) => {
                            if let Some(message) = next_status {
                                status = Some(message);
                            }
                        }
                        StartupResumePickerAction::Cancel => return Ok(None),
                        StartupResumePickerAction::Selected(path) => return Ok(Some(path)),
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
        }
    }
}

fn build_startup_session_overlay_state(
    cwd: &Path,
    session_dir_override: Option<PathBuf>,
    keybindings: &KeybindingsManager,
    scope: SessionScope,
    sort_mode: SessionSortMode,
    name_filter: SessionNameFilter,
    show_path: bool,
    filter: Option<&str>,
    selected_value: Option<&str>,
) -> Result<SessionOverlayState, String> {
    let overlay = SearchOverlay::new(
        "Resume Session",
        String::new(),
        Vec::new(),
        filter,
        String::new(),
    );
    let mut state = SessionOverlayState {
        overlay,
        selections: Vec::new(),
        records: Vec::new(),
        rows: Vec::new(),
        current_session_file: None,
        standalone: true,
        scope,
        sort_mode,
        name_filter,
        show_path,
        confirming_delete: None,
    };
    reload_startup_session_overlay(
        &mut state,
        cwd,
        session_dir_override.as_deref(),
        keybindings,
        selected_value,
    )?;
    Ok(state)
}

fn reload_startup_session_overlay(
    state: &mut SessionOverlayState,
    cwd: &Path,
    session_dir_override: Option<&Path>,
    keybindings: &KeybindingsManager,
    selected_value: Option<&str>,
) -> Result<(), String> {
    let query = state.overlay.search.get_value().to_string();
    let root = session_scope_root(state.scope, None, cwd, session_dir_override);
    let records = discover_session_records(&root)?;
    let rows = build_session_overlay_rows(
        records.clone(),
        if query.is_empty() { None } else { Some(&query) },
        state.sort_mode,
        state.name_filter,
    );
    let (items, selections) = session_overlay_rows_to_items(&rows);
    state
        .overlay
        .replace_items_preserving_selection(items, selected_value);
    state.records = records;
    state.rows = rows;
    state.current_session_file = None;
    state.selections = selections;
    update_session_overlay_metadata_with_options(state, keybindings, None, false);
    Ok(())
}

fn handle_startup_resume_picker_key(
    state: &mut SessionOverlayState,
    keybindings: &KeybindingsManager,
    cwd: &Path,
    session_dir_override: Option<&Path>,
    event: KeyEvent,
) -> Result<StartupResumePickerAction, String> {
    if let Some(confirming_path) = state.confirming_delete.clone() {
        if matches!(event.code, KeyCode::Escape) || matches_ctrl_char(&event, 'c') {
            state.confirming_delete = None;
            update_session_overlay_metadata_with_options(state, keybindings, None, false);
            return Ok(StartupResumePickerAction::Continue(Some(
                "Session deletion cancelled.".to_string(),
            )));
        }
        if matches!(event.code, KeyCode::Enter)
            && state
                .overlay
                .selected_value()
                .is_some_and(|selected| PathBuf::from(selected) == confirming_path)
        {
            fs::remove_file(&confirming_path).map_err(|error| error.to_string())?;
            state.confirming_delete = None;
            reload_startup_session_overlay(state, cwd, session_dir_override, keybindings, None)?;
            return Ok(StartupResumePickerAction::Continue(Some(format!(
                "Deleted {}.",
                confirming_path.to_string_lossy()
            ))));
        }
        return Ok(StartupResumePickerAction::Continue(None));
    }

    if keybindings.matches(&event, AppAction::ToggleSessionScope) {
        state.scope = state.scope.toggle();
        let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
        reload_startup_session_overlay(
            state,
            cwd,
            session_dir_override,
            keybindings,
            selected_value.as_deref(),
        )?;
        return Ok(StartupResumePickerAction::Continue(Some(format!(
            "Session scope: {}",
            state.scope.label()
        ))));
    }
    if keybindings.matches(&event, AppAction::ToggleSessionNamedFilter) {
        state.name_filter = state.name_filter.toggle();
        let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
        reload_startup_session_overlay(
            state,
            cwd,
            session_dir_override,
            keybindings,
            selected_value.as_deref(),
        )?;
        return Ok(StartupResumePickerAction::Continue(Some(format!(
            "Session name filter: {}",
            state.name_filter.label()
        ))));
    }
    if keybindings.matches(&event, AppAction::ToggleSessionSort) {
        state.sort_mode = state.sort_mode.next();
        let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
        reload_startup_session_overlay(
            state,
            cwd,
            session_dir_override,
            keybindings,
            selected_value.as_deref(),
        )?;
        return Ok(StartupResumePickerAction::Continue(Some(format!(
            "Session sort: {}",
            state.sort_mode.label()
        ))));
    }
    if keybindings.matches(&event, AppAction::ToggleSessionPath) {
        state.show_path = !state.show_path;
        let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
        reload_startup_session_overlay(
            state,
            cwd,
            session_dir_override,
            keybindings,
            selected_value.as_deref(),
        )?;
        return Ok(StartupResumePickerAction::Continue(Some(
            if state.show_path {
                "Session paths visible.".to_string()
            } else {
                "Session paths hidden.".to_string()
            },
        )));
    }
    if keybindings.matches(&event, AppAction::DeleteSession)
        || (matches!(event.code, KeyCode::Backspace) && event.modifiers == KeyModifiers::NONE)
    {
        if let Some(selected_value) = state.overlay.selected_value() {
            state.confirming_delete = Some(PathBuf::from(selected_value));
            update_session_overlay_metadata_with_options(state, keybindings, None, false);
            return Ok(StartupResumePickerAction::Continue(Some(
                "Press Enter to confirm session deletion, or Esc to cancel.".to_string(),
            )));
        }
        return Ok(StartupResumePickerAction::Continue(None));
    }

    match event.code {
        KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Escape => {
            match state.overlay.list.handle_key(&event) {
                SelectEvent::Changed => {
                    update_session_overlay_metadata_with_options(state, keybindings, None, false);
                    Ok(StartupResumePickerAction::Continue(None))
                }
                SelectEvent::None => Ok(StartupResumePickerAction::Continue(None)),
                SelectEvent::Cancelled => Ok(StartupResumePickerAction::Cancel),
                SelectEvent::Selected(item) => Ok(StartupResumePickerAction::Selected(
                    PathBuf::from(item.value),
                )),
            }
        }
        _ => match state.overlay.search.handle_key(&event) {
            InputEvent::Changed => {
                let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
                reload_startup_session_overlay(
                    state,
                    cwd,
                    session_dir_override,
                    keybindings,
                    selected_value.as_deref(),
                )?;
                Ok(StartupResumePickerAction::Continue(None))
            }
            InputEvent::Cancelled => Ok(StartupResumePickerAction::Cancel),
            InputEvent::Submitted(_) => {
                if let Some(item) = state.overlay.list.selected_item() {
                    Ok(StartupResumePickerAction::Selected(PathBuf::from(
                        item.value.clone(),
                    )))
                } else {
                    Ok(StartupResumePickerAction::Continue(None))
                }
            }
            InputEvent::None => Ok(StartupResumePickerAction::Continue(None)),
        },
    }
}

pub fn run_config_tui() -> Result<(), String> {
    config_browser::run_config_tui()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopAction {
    Continue,
    OpenExternalEditor,
    Suspend,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlobalKeyAction {
    None,
    Continue,
    Suspend,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueuedMessageKind {
    Steer,
    FollowUp,
}

impl QueuedMessageKind {
    fn label(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "follow-up",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QueuedMessage {
    kind: QueuedMessageKind,
    text: String,
}

#[derive(Clone, Debug)]
struct ActiveToolExecution {
    tool_call_id: String,
    tool_name: String,
    args: Value,
    partial_result: Option<Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DoubleEscapeAction {
    Tree,
    Fork,
    None,
}

impl DoubleEscapeAction {
    fn from_settings(value: Option<&str>) -> Self {
        match value {
            Some("fork") => Self::Fork,
            Some("none") => Self::None,
            _ => Self::Tree,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Tree => "tree",
            Self::Fork => "fork",
            Self::None => "none",
        }
    }

}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolExpandMode {
    Collapsed,
    All,
}

impl ToolExpandMode {
    fn next(self) -> Self {
        match self {
            Self::Collapsed => Self::All,
            Self::All => Self::Collapsed,
        }
    }

    fn status(self) -> &'static str {
        match self {
            Self::Collapsed => "Tool blocks collapsed.",
            Self::All => "Tool blocks expanded.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayOutcome {
    KeepOpen,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayPresentation {
    InShell,
    Standalone,
}

enum OverlayState {
    Model(ModelOverlayState),
    ScopedModels(ScopedModelsOverlayState),
    Settings(SettingsOverlayState),
    Fork(ForkOverlayState),
    TreeSummary(TreeSummaryOverlayState),
    Search {
        kind: SearchOverlayKind,
        overlay: SearchOverlay,
        selection: Vec<OverlaySelection>,
        tree_filter: Option<TreeFilterMode>,
    },
    Session(SessionOverlayState),
    Input(InputOverlayState),
    Auth(AuthOverlayState),
}

fn overlay_presentation(overlay: &OverlayState) -> OverlayPresentation {
    match overlay {
        OverlayState::Search { .. } => OverlayPresentation::InShell,
        OverlayState::Input(_) | OverlayState::Auth(_) => OverlayPresentation::Standalone,
        OverlayState::Model(_)
        | OverlayState::ScopedModels(_)
        | OverlayState::Settings(_)
        | OverlayState::Fork(_) => OverlayPresentation::InShell,
        OverlayState::TreeSummary(_) => OverlayPresentation::Standalone,
        OverlayState::Session(state) => {
            if state.standalone {
                OverlayPresentation::Standalone
            } else {
                OverlayPresentation::InShell
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum TranscriptEntry {
    Message(Message),
    CustomMessage {
        custom_type: String,
        content: UserContent,
        details: Option<Value>,
    },
    Summary {
        kind: SummaryKind,
        title: &'static str,
        text: String,
        tokens_before: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SummaryKind {
    Generic,
    Branch,
    Compaction,
}

fn append_output(target: &mut RenderOutput, child: RenderOutput, prefer_cursor: bool) {
    let row_offset = target.lines.len() as u16;
    if (prefer_cursor || target.cursor.is_none()) && child.cursor.is_some() {
        let cursor = child.cursor.expect("cursor already checked");
        target.cursor = Some(CursorPosition {
            row: row_offset + cursor.row,
            col: cursor.col,
        });
    }
    target.lines.extend(child.lines);
}

fn append_blank_lines(target: &mut RenderOutput, width: u16, count: usize) {
    let blank = RenderedLine::Text(" ".repeat(width as usize));
    for _ in 0..count {
        target.lines.push(blank.clone());
    }
}

fn should_show_startup_stack(
    transcript_entries_empty: bool,
    overlay_presentation: Option<OverlayPresentation>,
    has_active_prompt: bool,
    has_active_auth: bool,
    show_new_session_banner: bool,
) -> bool {
    !matches!(overlay_presentation, Some(OverlayPresentation::Standalone))
        && !has_active_auth
        && (transcript_entries_empty || (has_active_prompt && show_new_session_banner))
}

fn active_prompt_transcript_lines(
    _active: &ActivePrompt,
    _width: usize,
    _spinner_frame: usize,
) -> Vec<RenderedLine> {
    Vec::new()
}

fn spawn_input_reader() -> mpsc::Receiver<Vec<KeyEvent>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        let mut buffer = [0u8; 128];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let events = parse_input_bytes(&buffer[..count]);
                    if tx.send(events).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

fn oauth_provider_label(provider: &str) -> &'static str {
    match provider {
        "openai-codex" => "OpenAI Codex",
        "anthropic" => "Anthropic",
        _ => "OAuth Provider",
    }
}

fn discover_session_paths(dir: &Path) -> Vec<PathBuf> {
    if !dir.exists() {
        return Vec::new();
    }

    let mut paths = WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

#[derive(Clone, Debug)]
struct TreeListItem {
    entry_id: String,
    entry_type: String,
    message_role: Option<String>,
    assistant_tool_only: bool,
    preview: String,
    depth: usize,
    label: Option<String>,
}

fn flatten_tree_items(nodes: &[SessionTreeNode], depth: usize, target: &mut Vec<TreeListItem>) {
    for node in nodes {
        let entry_id = node
            .entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let entry_type = node
            .entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("entry")
            .to_string();
        let preview = tree_entry_preview(&node.entry);
        let message_role = node
            .entry
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let assistant_tool_only = node
            .entry
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .map(|blocks| {
                !blocks.is_empty()
                    && blocks.iter().all(|block| {
                        matches!(block.get("type").and_then(Value::as_str), Some("toolCall"))
                    })
            })
            .unwrap_or(false);
        target.push(TreeListItem {
            entry_id,
            entry_type,
            message_role,
            assistant_tool_only,
            preview,
            depth,
            label: node.label.clone(),
        });
        flatten_tree_items(&node.children, depth + 1, target);
    }
}

fn tree_item_matches_mode(item: &TreeListItem, filter_mode: TreeFilterMode) -> bool {
    match filter_mode {
        TreeFilterMode::Default => {
            if matches!(
                item.entry_type.as_str(),
                "label" | "custom" | "model_change" | "thinking_level_change"
            ) {
                return false;
            }
            !(item.message_role.as_deref() == Some("assistant") && item.assistant_tool_only)
        }
        TreeFilterMode::NoTools => {
            if matches!(
                item.entry_type.as_str(),
                "label" | "custom" | "model_change" | "thinking_level_change"
            ) {
                return false;
            }
            item.message_role.as_deref() != Some("toolResult")
                && !(item.message_role.as_deref() == Some("assistant") && item.assistant_tool_only)
        }
        TreeFilterMode::UserOnly => {
            item.entry_type == "message" && item.message_role.as_deref() == Some("user")
        }
        TreeFilterMode::LabeledOnly => item.label.is_some(),
        TreeFilterMode::All => true,
    }
}

fn tree_entry_preview(entry: &Value) -> String {
    let entry_type = entry.get("type").and_then(Value::as_str).unwrap_or("entry");
    if entry_type == "message" {
        let role = entry
            .get("message")
            .and_then(Value::as_object)
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .unwrap_or("message");
        let text = entry
            .get("message")
            .map(extract_message_preview)
            .unwrap_or_default();
        return format!("{role}: {text}");
    }
    if entry_type == "custom_message" {
        let custom_type = entry
            .get("customType")
            .and_then(Value::as_str)
            .unwrap_or("custom");
        let text = entry
            .get("content")
            .map(extract_message_preview)
            .unwrap_or_default();
        return format!("{custom_type}: {text}");
    }
    if entry_type == "branch_summary" {
        let summary = entry
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return format!("branch: {summary}");
    }
    if entry_type == "compaction" {
        let summary = entry
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return format!("compaction: {summary}");
    }
    if entry_type == "model_change" {
        let provider = entry
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let model_id = entry
            .get("modelId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return format!("model: {provider}/{model_id}");
    }
    if entry_type == "thinking_level_change" {
        return format!(
            "thinking: {}",
            entry
                .get("thinkingLevel")
                .and_then(Value::as_str)
                .unwrap_or_default()
        );
    }
    entry.to_string()
}

fn extract_message_preview(value: &Value) -> String {
    if let Some(text) = value.get("content").and_then(Value::as_str) {
        return truncate_to_width(text, 80);
    }
    if let Some(blocks) = value.get("content").and_then(Value::as_array) {
        let mut parts = Vec::new();
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    }
                }
                Some("thinking") => {
                    if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    }
                }
                Some("toolCall") => {
                    if let Some(name) = block.get("name").and_then(Value::as_str) {
                        parts.push(format!("tool:{name}"));
                    }
                }
                Some("image") => {
                    if let Some(mime_type) = block.get("mimeType").and_then(Value::as_str) {
                        parts.push(format!("[image:{mime_type}]"));
                    }
                }
                _ => {}
            }
        }
        return parts.join(" ");
    }
    String::new()
}

fn bool_setting(settings: &Value, path: &[&str], default: bool) -> bool {
    navigate_setting(settings, path)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn string_setting(settings: &Value, path: &[&str]) -> Option<String> {
    navigate_setting(settings, path)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn navigate_setting<'a>(settings: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = settings;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn queue_mode_setting(settings: &Value, path: &[&str]) -> QueueMode {
    match string_setting(settings, path).as_deref() {
        Some("all") => QueueMode::All,
        _ => QueueMode::OneAtATime,
    }
}

fn queue_mode_value(mode: QueueMode) -> String {
    match mode {
        QueueMode::All => "all".to_string(),
        QueueMode::OneAtATime => "one-at-a-time".to_string(),
    }
}

fn bool_value(value: bool) -> String {
    if value {
        "true".to_string()
    } else {
        "false".to_string()
    }
}

fn apply_live_message(messages: &mut Vec<Message>, message: Message) {
    match &message {
        Message::Assistant(_) => {
            if let Some(Message::Assistant(existing)) = messages.last_mut() {
                if let Message::Assistant(next) = message {
                    *existing = next;
                    return;
                }
            }
            messages.push(message);
        }
        _ => {
            if messages.last() != Some(&message) {
                messages.push(message);
            }
        }
    }
}

fn apply_live_transcript_message(transcript: &mut Vec<TranscriptEntry>, message: Message) {
    match &message {
        Message::Assistant(_) => {
            if let Some(TranscriptEntry::Message(Message::Assistant(existing))) =
                transcript.last_mut()
            {
                if let Message::Assistant(next) = message {
                    *existing = next;
                    return;
                }
            }
            transcript.push(TranscriptEntry::Message(message));
        }
        _ => {
            if transcript.last() != Some(&TranscriptEntry::Message(message.clone())) {
                transcript.push(TranscriptEntry::Message(message));
            }
        }
    }
}

const ANSI_RESET: &str = "\u{1b}[0m";

fn ansi(code: &str, text: &str) -> String {
    format!("\u{1b}[{code}m{text}{ANSI_RESET}")
}

fn ansi_rgb(r: u8, g: u8, b: u8, text: &str) -> String {
    ansi(&format!("38;2;{r};{g};{b}"), text)
}

fn style_brand(text: &str) -> String {
    ansi("1;38;2;138;190;183", text)
}

fn style_title(text: &str) -> String {
    ansi("1;38;2;129;162;190", text)
}

fn style_tool_title(text: &str) -> String {
    ansi("1;38;2;240;198;116", text)
}

fn style_subtitle(text: &str) -> String {
    ansi("38;2;128;128;128", text)
}

fn style_hint(text: &str) -> String {
    ansi("38;2;128;128;128", text)
}

fn style_dim(text: &str) -> String {
    ansi("38;2;102;102;102", text)
}

fn style_border(text: &str) -> String {
    ansi("38;2;80;80;80", text)
}

fn style_startup_section_title(text: &str) -> String {
    ansi("1;38;2;240;198;116", text)
}

fn style_thinking_border_rule(level: &str, text: &str) -> String {
    match normalized_thinking_level(level) {
        "minimal" => ansi_rgb(110, 110, 110, text),
        "low" => ansi_rgb(95, 135, 175, text),
        "medium" => ansi_rgb(129, 162, 190, text),
        "high" => ansi_rgb(178, 148, 187, text),
        "xhigh" => ansi_rgb(209, 131, 232, text),
        _ => ansi_rgb(80, 80, 80, text),
    }
}

fn style_bash_border_rule(text: &str) -> String {
    ansi("38;2;181;189;104", text)
}

fn style_warning(text: &str) -> String {
    ansi("38;2;255;255;0", text)
}

fn style_error(text: &str) -> String {
    ansi("1;38;2;204;102;102", text)
}

fn style_success(text: &str) -> String {
    ansi("38;2;181;189;104", text)
}

fn style_user_surface(text: &str) -> String {
    apply_persistent_style("48;5;237;38;5;255", text)
}

fn style_custom_surface(text: &str) -> String {
    apply_persistent_style("48;5;236;38;5;255", text)
}

fn style_custom_label(text: &str) -> String {
    ansi("1;38;5;141", text)
}

fn style_custom_text(text: &str) -> String {
    ansi("38;5;189", text)
}

fn style_selected_row(text: &str) -> String {
    apply_persistent_style("48;5;236", text)
}

fn style_thinking_surface(text: &str) -> String {
    apply_persistent_style("3;38;5;244", text)
}

fn style_inline_code(text: &str) -> String {
    ansi("38;5;151", text)
}

fn style_code_block_border(text: &str) -> String {
    ansi("38;5;244", text)
}

fn style_code_block_line(text: &str) -> String {
    ansi("38;5;114", text)
}

fn style_markdown_heading(level: usize, text: &str) -> String {
    match level {
        1 => ansi("1;4;38;5;222", text),
        2 => ansi("1;38;5;222", text),
        _ => ansi("1;38;5;222", &format!("{} {text}", "#".repeat(level))),
    }
}

fn style_markdown_link(text: &str) -> String {
    ansi("4;38;5;110", text)
}

fn style_markdown_link_url(text: &str) -> String {
    ansi("38;5;244", text)
}

fn style_markdown_bold(text: &str) -> String {
    ansi("1", text)
}

fn style_markdown_italic(text: &str) -> String {
    ansi("3", text)
}

fn style_markdown_strikethrough(text: &str) -> String {
    ansi("9", text)
}

fn style_quote_border(text: &str) -> String {
    ansi("38;5;244", text)
}

fn style_quote_text(text: &str) -> String {
    ansi("3;38;5;244", text)
}

fn style_list_bullet(text: &str) -> String {
    ansi("38;5;109", text)
}

fn style_md_hr(text: &str) -> String {
    ansi("38;5;240", text)
}

fn style_prefix(prefix: &str) -> String {
    if prefix.starts_with("Tool ") {
        return style_tool_title(prefix);
    }
    match prefix {
        "You" => ansi("1;38;5;81", prefix),
        "Assistant" => ansi("1;38;5;117", prefix),
        "Thinking" => style_hint(prefix),
        "Bash" => ansi("1;38;5;214", prefix),
        "Running" => style_warning(prefix),
        _ => ansi("1", prefix),
    }
}

fn pending_message_lines(messages: &[QueuedMessage], width: u16) -> Vec<String> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut lines = vec![truncate_to_width(
        &style_title("Pending messages:"),
        width as usize,
    )];
    for queued in messages.iter().rev().take(3).rev() {
        let prefix = match queued.kind {
            QueuedMessageKind::Steer => "  [steer] ",
            QueuedMessageKind::FollowUp => "  [follow-up] ",
        };
        let available = (width as usize).saturating_sub(prefix.len());
        for (index, line) in wrap_text(&queued.text, available).into_iter().enumerate() {
            if index == 0 {
                lines.push(format!("{prefix}{line}"));
            } else {
                lines.push(format!("{}{}", " ".repeat(prefix.len()), line));
            }
        }
    }
    if messages.len() > 3 {
        lines.push(format!("  ... {} more", messages.len() - 3));
    }
    lines
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    for raw_line in text.lines() {
        if raw_line.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        for word in raw_line.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if visible_width(&candidate) > width && !current.is_empty() {
                lines.push(truncate_to_width(&current, width));
                current = word.to_string();
            } else if visible_width(word) > width {
                lines.push(truncate_to_width(word, width));
                current.clear();
            } else {
                current = candidate;
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

fn repo_root_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn coding_agent_changelog_path() -> PathBuf {
    repo_root_dir()
        .join("packages")
        .join("coding-agent")
        .join("CHANGELOG.md")
}

fn load_changelog_markdown() -> Result<String, String> {
    let path = coding_agent_changelog_path();
    let content = fs::read_to_string(&path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    let sections = content
        .split("\n## ")
        .filter_map(|section| {
            let trimmed = section.trim();
            if trimmed.is_empty() {
                return None;
            }
            let heading = if trimmed.starts_with("## ") {
                trimmed.to_string()
            } else {
                format!("## {trimmed}")
            };
            Some(heading)
        })
        .filter(|section| section.starts_with("## ["))
        .take(3)
        .collect::<Vec<_>>();
    if sections.is_empty() {
        Ok("No changelog entries found.".to_string())
    } else {
        Ok(sections.join("\n\n"))
    }
}

fn format_session_summary_markdown(state: &RpcSessionState, stats: &RpcSessionStats) -> String {
    let mut lines = vec!["## Session".to_string()];
    if let Some(name) = state
        .session_name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("- Name: {name}"));
    }
    lines.push(format!(
        "- File: {}",
        stats.session_file.as_deref().unwrap_or("In-memory")
    ));
    lines.push(format!("- ID: {}", stats.session_id));
    lines.push(String::new());
    lines.push("## Messages".to_string());
    lines.push(format!(
        "- User: {}",
        format_grouped_decimal(stats.user_messages)
    ));
    lines.push(format!(
        "- Assistant: {}",
        format_grouped_decimal(stats.assistant_messages)
    ));
    lines.push(format!(
        "- Tool Calls: {}",
        format_grouped_decimal(stats.tool_calls)
    ));
    lines.push(format!(
        "- Tool Results: {}",
        format_grouped_decimal(stats.tool_results)
    ));
    lines.push(format!(
        "- Total: {}",
        format_grouped_decimal(stats.total_messages)
    ));
    lines.push(String::new());
    lines.push("## Tokens".to_string());
    lines.push(format!(
        "- Input: {}",
        format_grouped_decimal(stats.tokens.input)
    ));
    lines.push(format!(
        "- Output: {}",
        format_grouped_decimal(stats.tokens.output)
    ));
    if stats.tokens.cache_read > 0 {
        lines.push(format!(
            "- Cache Read: {}",
            format_grouped_decimal(stats.tokens.cache_read)
        ));
    }
    if stats.tokens.cache_write > 0 {
        lines.push(format!(
            "- Cache Write: {}",
            format_grouped_decimal(stats.tokens.cache_write)
        ));
    }
    lines.push(format!(
        "- Total: {}",
        format_grouped_decimal(stats.tokens.total)
    ));
    if stats.cost > 0.0 {
        lines.push(String::new());
        lines.push("## Cost".to_string());
        lines.push(format!("- Total: {}", format_cost(stats.cost)));
    }
    lines.join("\n")
}

fn format_hotkeys_markdown(keybindings: &KeybindingsManager) -> String {
    let submit = keybindings.display_editor(EditorAction::Submit);
    let new_line = keybindings.display_editor(EditorAction::NewLine);
    let cursor_word_left = keybindings.display_editor(EditorAction::CursorWordLeft);
    let cursor_word_right = keybindings.display_editor(EditorAction::CursorWordRight);
    let cursor_line_start = keybindings.display_editor(EditorAction::CursorLineStart);
    let cursor_line_end = keybindings.display_editor(EditorAction::CursorLineEnd);
    let delete_word_backward = keybindings.display_editor(EditorAction::DeleteWordBackward);
    let delete_word_forward = keybindings.display_editor(EditorAction::DeleteWordForward);
    let delete_to_line_start = keybindings.display_editor(EditorAction::DeleteToLineStart);
    let delete_to_line_end = keybindings.display_editor(EditorAction::DeleteToLineEnd);
    let yank = keybindings.display_editor(EditorAction::Yank);
    let yank_pop = keybindings.display_editor(EditorAction::YankPop);
    let undo = keybindings.display_editor(EditorAction::Undo);
    let tab = keybindings.display_editor(EditorAction::Tab);
    let clear = keybindings.display(AppAction::Clear);
    let exit = keybindings.display(AppAction::Exit);
    let suspend = keybindings.display(AppAction::Suspend);
    let external_editor = keybindings.display(AppAction::ExternalEditor);
    let cycle_models_forward = keybindings.display(AppAction::CycleModelForward);
    let cycle_models_backward = keybindings.display(AppAction::CycleModelBackward);
    let select_model = keybindings.display(AppAction::SelectModel);
    let cycle_thinking = keybindings.display(AppAction::CycleThinkingLevel);
    let expand_tools = keybindings.display(AppAction::ExpandTools);
    let toggle_thinking = keybindings.display(AppAction::ToggleThinking);
    let follow_up = keybindings.display(AppAction::FollowUp);
    let dequeue = keybindings.display(AppAction::Dequeue);

    [
        "## Navigation".to_string(),
        "- Arrow keys: Move cursor / browse history (Up when empty)".to_string(),
        format!(
            "- {} / {}: Move by word",
            cursor_word_left, cursor_word_right
        ),
        format!("- {}: Start of line", cursor_line_start),
        format!("- {}: End of line", cursor_line_end),
        String::new(),
        "## Editing".to_string(),
        format!("- {}: Send message", submit),
        format!("- {}: New line", new_line),
        format!("- {}: Delete word backwards", delete_word_backward),
        format!("- {}: Delete word forwards", delete_word_forward),
        format!("- {}: Delete to start of line", delete_to_line_start),
        format!("- {}: Delete to end of line", delete_to_line_end),
        format!("- {}: Paste the most-recently-deleted text", yank),
        format!(
            "- {}: Cycle through the deleted text after pasting",
            yank_pop
        ),
        format!("- {}: Undo", undo),
        String::new(),
        "## Other".to_string(),
        format!("- {}: Path completion / accept autocomplete", tab),
        "- escape: Cancel autocomplete / abort streaming".to_string(),
        format!("- {}: Clear editor (first) / exit (second)", clear),
        format!("- {}: Exit (when editor is empty)", exit),
        format!("- {}: Suspend to background", suspend),
        format!("- {}: Cycle thinking level", cycle_thinking),
        format!(
            "- {} / {}: Cycle models",
            cycle_models_forward, cycle_models_backward
        ),
        format!("- {}: Open model selector", select_model),
        format!("- {}: Toggle tool output expansion", expand_tools),
        format!("- {}: Toggle thinking block visibility", toggle_thinking),
        format!("- {}: Edit message in external editor", external_editor),
        format!("- {}: Queue follow-up message", follow_up),
        format!("- {}: Restore queued messages", dequeue),
        format!(
            "- {}: Paste image from clipboard",
            keybindings.display(AppAction::PasteImage)
        ),
        "- /: Slash commands".to_string(),
        "- !: Run bash command".to_string(),
        "- !!: Run bash command (excluded from context)".to_string(),
    ]
    .join("\n")
}

fn format_grouped_decimal(value: impl ToString) -> String {
    let digits = value.to_string();
    let (sign, body) = if let Some(rest) = digits.strip_prefix('-') {
        ("-", rest)
    } else {
        ("", digits.as_str())
    };
    let len = body.len();
    if len <= 3 {
        return format!("{sign}{body}");
    }

    let mut grouped = String::with_capacity(digits.len() + (len - 1) / 3);
    for (index, ch) in body.chars().enumerate() {
        if index > 0 && (len - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}{grouped}")
}

fn share_export_path() -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("pi-rust-share-{millis}.html"))
}

fn detect_git_branch(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch)
    }
}

fn discover_startup_context_files(cwd: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();

    for ancestor in cwd.ancestors() {
        let path = ancestor.join("AGENTS.md");
        if path.is_file() {
            let display = shorten_home_path(&path.to_string_lossy());
            if seen.insert(display.clone()) {
                files.push(display);
            }
        }
    }

    for relative in [
        ".pi/AGENTS.md",
        ".pi/SYSTEM.md",
        ".pi/APPEND_SYSTEM.md",
        ".pi/SYSTEM",
        ".pi/APPEND_SYSTEM",
    ] {
        let path = cwd.join(relative);
        if path.is_file() {
            let display = shorten_home_path(&path.to_string_lossy());
            if seen.insert(display.clone()) {
                files.push(display);
            }
        }
    }

    files
}

fn suspend_process(
    terminal: &mut ProcessTerminal,
    renderer: &mut LineDiffRenderer,
) -> Result<(), String> {
    let _ = renderer.clear(terminal);
    let _ = terminal.show_cursor();
    terminal.stop().map_err(|error| error.to_string())?;

    let status = Command::new("kill")
        .args(["-TSTP", &std::process::id().to_string()])
        .status()
        .map_err(|error| format!("Failed to suspend process: {error}"));

    terminal.start().map_err(|error| error.to_string())?;
    terminal
        .set_title("pi-rust")
        .map_err(|error| error.to_string())?;
    terminal.hide_cursor().map_err(|error| error.to_string())?;
    *renderer = LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });
    let _ = terminal.drain_input(25, 5);

    match status? {
        exit_status if exit_status.success() => Ok(()),
        exit_status => Err(format!("Suspend command exited with status {exit_status}.")),
    }
}

fn matches_ctrl_char(event: &KeyEvent, expected: char) -> bool {
    matches!(event.code, KeyCode::Char(ch) if ch == expected)
        && event.modifiers == KeyModifiers::CTRL
}

fn matches_alt_key(event: &KeyEvent, code: KeyCode) -> bool {
    event.code == code && event.modifiers == KeyModifiers::ALT
}

#[cfg(test)]
mod tests {
    use super::{
        SearchOverlay, SessionNameFilter, SessionRecord, SessionSortMode, ToolExpandMode,
        TranscriptEntry, TreeFilterMode, TreeListItem, active_tool_render_lines,
        apply_live_transcript_message, build_prompt_autocomplete, collect_diff_lines, content_text, discover_session_paths,
        prompt_autocomplete_should_submit_current_prompt, render_footer_panel,
        render_prompt_autocomplete, render_prompt_panel, render_transcript_entry,
        session_selection_detail, session_transcript_lines, tree_item_matches_mode, wrap_text,
    };
    use pi_rust_ai_core::{
        ApiId, AssistantContentBlock, AssistantMessage, Message, Model, ModelCost, ProviderId,
        StopReason, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
    };
    use pi_rust_protocol::{
        QueueMode, RpcCommandLocation, RpcCommandSource, RpcSessionState, RpcSessionStats,
        RpcSlashCommand, RpcTokenStats,
    };
    use pi_rust_tui::{
        Component, CursorPosition, Editor, RenderOutput, RenderedLine, SelectItem,
        TerminalCapabilities,
    };
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    use crate::keybindings::KeybindingsManager;

    fn no_inline_images() -> TerminalCapabilities {
        TerminalCapabilities {
            kitty_keyboard: false,
            inline_images: false,
            image_protocol: None,
            hyperlinks: true,
        }
    }

    fn strip_ansi(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                if matches!(chars.peek(), Some(']')) {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\u{7}' {
                            break;
                        }
                    }
                } else {
                    for next in chars.by_ref() {
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                continue;
            }
            out.push(ch);
        }
        out
    }

    fn with_inline_images() -> TerminalCapabilities {
        TerminalCapabilities {
            kitty_keyboard: false,
            inline_images: true,
            image_protocol: Some(pi_rust_tui::ImageProtocol::Kitty),
            hyperlinks: true,
        }
    }

    fn output_text(output: &RenderOutput) -> String {
        output
            .lines
            .iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(text),
                RenderedLine::Image(image) => image.alt_text.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample_state() -> RpcSessionState {
        RpcSessionState {
            model: Some(Model {
                id: "gpt-5.1-codex".to_string(),
                name: "GPT-5.1 Codex".to_string(),
                api: ApiId("openai-responses".to_string()),
                provider: ProviderId("openai".to_string()),
                base_url: "https://api.openai.com/v1".to_string(),
                reasoning: true,
                input: vec![],
                cost: ModelCost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                context_window: 200_000,
                max_tokens: 32_768,
                headers: None,
                compat: None,
            }),
            thinking_level: "high".to_string(),
            is_streaming: false,
            is_compacting: false,
            steering_mode: QueueMode::All,
            follow_up_mode: QueueMode::OneAtATime,
            session_file: Some("session.jsonl".to_string()),
            session_id: "session-123".to_string(),
            session_name: Some("Footer Parity".to_string()),
            auto_compaction_enabled: true,
            message_count: 8,
            pending_message_count: 0,
        }
    }

    fn sample_stats() -> RpcSessionStats {
        RpcSessionStats {
            session_file: Some("session.jsonl".to_string()),
            session_id: "session-123".to_string(),
            user_messages: 3,
            assistant_messages: 2,
            tool_calls: 1,
            tool_results: 1,
            total_messages: 6,
            tokens: RpcTokenStats {
                input: 1_200,
                output: 345,
                cache_read: 200,
                cache_write: 0,
                total: 1_745,
            },
            cost: 0.123,
        }
    }

    #[test]
    fn footer_panel_renders_session_stats_and_status() {
        let output = render_footer_panel(
            &sample_state(),
            &sample_stats(),
            Path::new("/workspace/pi"),
            Some("main"),
            140,
            true,
            2,
            false,
            0,
            ToolExpandMode::All,
        );
        let text = output_text(&output);
        assert!(text.contains("/workspace/pi"));
        assert!(text.contains("(main)"));
        assert!(text.contains("Footer Parity"));
        assert!(text.contains("↑1.2k"));
        assert!(text.contains("↓345"));
        assert!(text.contains("R200"));
        assert!(text.contains("$0.123"));
        assert!(text.contains("0.9%"));
        assert!(text.contains("200k"));
        assert!(text.contains("auto"));
        assert!(text.contains("gpt-5.1-codex"));
        assert!(text.contains("high"));
    }

    #[test]
    fn footer_panel_shows_thinking_off_for_reasoning_models() {
        let mut state = sample_state();
        state.thinking_level = "off".to_string();
        let output = render_footer_panel(
            &state,
            &sample_stats(),
            Path::new("/workspace/pi"),
            Some("main"),
            140,
            false,
            0,
            false,
            0,
            ToolExpandMode::Collapsed,
        );
        let text = output_text(&output);
        assert!(text.contains("thinking off"));
    }

    #[test]
    fn prompt_composer_border_rules_use_distinct_thinking_colors() {
        let off = super::prompt_composer_border_rules("", "off", 8);
        let minimal = super::prompt_composer_border_rules("", "minimal", 8);
        let low = super::prompt_composer_border_rules("", "low", 8);
        let medium = super::prompt_composer_border_rules("", "medium", 8);
        let high = super::prompt_composer_border_rules("", "high", 8);
        let xhigh = super::prompt_composer_border_rules("", "xhigh", 8);
        let bash = super::prompt_composer_border_rules("!!printf hi", "medium", 8);

        assert_eq!(off.0, off.1);
        assert_eq!(minimal.0, minimal.1);
        assert_eq!(low.0, low.1);
        assert_eq!(medium.0, medium.1);
        assert_eq!(high.0, high.1);
        assert_eq!(xhigh.0, xhigh.1);
        assert_ne!(off.0, minimal.0);
        assert_ne!(minimal.0, low.0);
        assert_ne!(low.0, medium.0);
        assert_ne!(medium.0, high.0);
        assert_ne!(high.0, xhigh.0);
        assert_eq!(bash.0, bash.1);
        assert_eq!(bash.0, super::style_bash_border_rule(&"─".repeat(8)));
    }

    #[test]
    fn startup_stack_stays_visible_during_first_active_turn() {
        assert!(super::should_show_startup_stack(
            false, None, true, false, true,
        ));
        assert!(!super::should_show_startup_stack(
            false, None, false, false, true,
        ));
        assert!(!super::should_show_startup_stack(
            false,
            Some(super::OverlayPresentation::Standalone),
            true,
            false,
            true,
        ));
    }

    #[test]
    fn prompt_panel_wraps_input_and_preserves_cursor() {
        let mut editor = Editor::new();
        editor.set_text("hello");
        let output = render_prompt_panel(
            &editor,
            None,
            &sample_state(),
            &KeybindingsManager::in_memory(),
            80,
            Some(5),
            false,
            1,
        );
        let text = output_text(&output);
        assert!(text.contains("hello"));
        assert!(!text.contains("> hello"));
        assert_eq!(
            output.lines.len(),
            3,
            "prompt panel should render top rule, editor body, and bottom rule"
        );
        assert_eq!(output.cursor, Some(CursorPosition { row: 1, col: 5 }));
    }

    #[test]
    fn slash_prompt_autocomplete_opens_on_slash() {
        let autocomplete =
            build_prompt_autocomplete("/", (0, 1), &[], || Ok(Vec::new()), Path::new("."), false)
                .expect("autocomplete build")
                .expect("autocomplete state");
        let text = output_text(&render_prompt_autocomplete(&autocomplete, 80));
        assert!(text.contains("Commands"));
        assert!(text.contains("/settings"));
        assert!(text.contains("/scoped-models"));
        assert!(text.contains("/copy"));
        assert!(text.contains("/share"));
        assert!(text.contains("Open settings menu"));
    }

    #[test]
    fn slash_prompt_autocomplete_stays_closed_off_first_line() {
        let autocomplete = build_prompt_autocomplete(
            "hello\n/",
            (1, 1),
            &[],
            || Ok(Vec::new()),
            Path::new("."),
            false,
        )
        .expect("autocomplete build");
        assert!(autocomplete.is_none());
    }

    #[test]
    fn exact_typed_slash_command_submits_current_prompt_instead_of_selected_suggestion() {
        let autocomplete = build_prompt_autocomplete(
            "/session",
            (0, "/session".len()),
            &[],
            || Ok(Vec::new()),
            Path::new("."),
            false,
        )
        .expect("autocomplete build")
        .expect("autocomplete state");
        assert!(prompt_autocomplete_should_submit_current_prompt(
            Some(&autocomplete),
            "/session",
            (0, "/session".len())
        ));
    }

    #[test]
    fn prompt_autocomplete_does_not_open_paths_for_plain_space() {
        let autocomplete = build_prompt_autocomplete(
            "hello ",
            (0, 6),
            &[],
            || Ok(Vec::new()),
            Path::new("."),
            false,
        )
        .expect("autocomplete build");
        assert!(autocomplete.is_none());
    }

    #[test]
    fn prompt_autocomplete_opens_paths_on_forced_tab_after_space() {
        let tempdir = tempdir().expect("tempdir");
        std::fs::create_dir_all(tempdir.path().join("src")).expect("create dir");

        let autocomplete = build_prompt_autocomplete(
            "/export ",
            (0, "/export ".len()),
            &[],
            || Ok(Vec::new()),
            tempdir.path(),
            true,
        )
        .expect("autocomplete build")
        .expect("autocomplete state");
        let text = output_text(&render_prompt_autocomplete(&autocomplete, 100));
        assert!(text.contains("Paths"));
        assert!(text.contains("src/"));
    }

    #[test]
    fn model_prompt_autocomplete_uses_available_models() {
        let autocomplete = build_prompt_autocomplete(
            "/model gp",
            (0, 9),
            &[],
            || {
                Ok(vec![Model {
                    id: "gpt-5.1-codex".to_string(),
                    provider: ProviderId::new("openai"),
                    api: ApiId::new("openai-responses"),
                    name: "GPT-5.1 Codex".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    input: vec![],
                    cost: ModelCost {
                        input: 0.0,
                        output: 0.0,
                        cache_read: 0.0,
                        cache_write: 0.0,
                    },
                    context_window: 400_000,
                    max_tokens: 8_192,
                    headers: None,
                    compat: None,
                    reasoning: true,
                }])
            },
            Path::new("."),
            false,
        )
        .expect("autocomplete build")
        .expect("autocomplete state");
        let text = output_text(&render_prompt_autocomplete(&autocomplete, 80));
        assert!(text.contains("Models"));
        assert!(text.contains("gpt-5.1-codex"));
        assert!(text.contains("GPT-5.1 Codex"));
    }

    #[test]
    fn multiline_model_prompt_autocomplete_stays_closed_off_first_line() {
        let autocomplete = build_prompt_autocomplete(
            "hello\n/model gp",
            (1, 9),
            &[],
            || {
                Ok(vec![Model {
                    id: "gpt-5.1-codex".to_string(),
                    provider: ProviderId::new("openai"),
                    api: ApiId::new("openai-responses"),
                    name: "GPT-5.1 Codex".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    input: vec![],
                    cost: ModelCost {
                        input: 0.0,
                        output: 0.0,
                        cache_read: 0.0,
                        cache_write: 0.0,
                    },
                    context_window: 400_000,
                    max_tokens: 8_192,
                    headers: None,
                    compat: None,
                    reasoning: true,
                }])
            },
            Path::new("."),
            false,
        )
        .expect("autocomplete build");
        assert!(autocomplete.is_none());
    }

    #[test]
    fn exact_typed_model_argument_submits_current_prompt_instead_of_selected_suggestion() {
        let autocomplete = build_prompt_autocomplete(
            "/model openai/gpt-5.1-codex",
            (0, "/model openai/gpt-5.1-codex".len()),
            &[],
            || {
                Ok(vec![Model {
                    id: "gpt-5.1-codex".to_string(),
                    provider: ProviderId::new("openai"),
                    api: ApiId::new("openai-responses"),
                    name: "GPT-5.1 Codex".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    input: vec![],
                    cost: ModelCost {
                        input: 0.0,
                        output: 0.0,
                        cache_read: 0.0,
                        cache_write: 0.0,
                    },
                    context_window: 400_000,
                    max_tokens: 8_192,
                    headers: None,
                    compat: None,
                    reasoning: true,
                }])
            },
            Path::new("."),
            false,
        )
        .expect("autocomplete build")
        .expect("autocomplete state");
        assert!(prompt_autocomplete_should_submit_current_prompt(
            Some(&autocomplete),
            "/model openai/gpt-5.1-codex",
            (0, "/model openai/gpt-5.1-codex".len())
        ));
    }

    #[test]
    fn slash_prompt_autocomplete_includes_dynamic_prompt_and_skill_commands() {
        let prompt_commands = [
            RpcSlashCommand {
                name: "plan".to_string(),
                description: Some("Generate a rollout".to_string()),
                source: RpcCommandSource::Prompt,
                location: Some(RpcCommandLocation::Project),
                path: Some("/tmp/.pi/prompts/plan.md".to_string()),
            },
            RpcSlashCommand {
                name: "skill:checks".to_string(),
                description: Some("Run repo checks".to_string()),
                source: RpcCommandSource::Skill,
                location: Some(RpcCommandLocation::User),
                path: Some("/tmp/skills/checks/SKILL.md".to_string()),
            },
        ];

        let prompt_autocomplete = build_prompt_autocomplete(
            "/pl",
            (0, 3),
            &prompt_commands,
            || Ok(Vec::new()),
            Path::new("."),
            false,
        )
        .expect("autocomplete build")
        .expect("autocomplete state");
        let prompt_text = output_text(&render_prompt_autocomplete(&prompt_autocomplete, 100));
        assert!(prompt_text.contains("/plan"));
        assert!(prompt_text.contains("Prompt template"));

        let skill_autocomplete = build_prompt_autocomplete(
            "/skill",
            (0, 6),
            &[prompt_commands[0].clone(), prompt_commands[1].clone()],
            || Ok(Vec::new()),
            Path::new("."),
            false,
        )
        .expect("autocomplete build")
        .expect("autocomplete state");
        let skill_text = output_text(&render_prompt_autocomplete(&skill_autocomplete, 100));
        assert!(skill_text.contains("/skill:checks"));
        assert!(skill_text.contains("Skill"));
    }

    #[test]
    fn path_prompt_autocomplete_suggests_paths_for_command_arguments() {
        let tempdir = tempdir().expect("tempdir");
        std::fs::create_dir_all(tempdir.path().join("src")).expect("create dir");
        std::fs::write(tempdir.path().join("src").join("main.rs"), "fn main() {}\n")
            .expect("write file");

        let autocomplete = build_prompt_autocomplete(
            "/export src/ma",
            (0, "/export src/ma".len()),
            &[],
            || Ok(Vec::new()),
            tempdir.path(),
            false,
        )
        .expect("autocomplete build")
        .expect("autocomplete state");
        let text = output_text(&render_prompt_autocomplete(&autocomplete, 100));
        assert!(text.contains("Paths"));
        assert!(text.contains("main.rs"));
    }

    #[test]
    fn file_reference_autocomplete_uses_at_prefix() {
        let tempdir = tempdir().expect("tempdir");
        std::fs::create_dir_all(tempdir.path().join("src")).expect("create dir");
        std::fs::write(
            tempdir.path().join("src").join("lib.rs"),
            "pub fn value() {}\n",
        )
        .expect("write file");

        let autocomplete = build_prompt_autocomplete(
            "@lib",
            (0, 4),
            &[],
            || Ok(Vec::new()),
            tempdir.path(),
            false,
        )
        .expect("autocomplete build")
        .expect("autocomplete state");
        let text = output_text(&render_prompt_autocomplete(&autocomplete, 100));
        assert!(text.contains("Files"));
        assert!(text.contains("lib.rs"));
        assert!(text.contains("src/lib.rs"));
    }

    #[test]
    fn clip_render_output_to_height_keeps_bottom_cursor_visible() {
        let output = RenderOutput {
            lines: (0..10)
                .map(|index| RenderedLine::Text(format!("line {index}")))
                .collect(),
            cursor: Some(CursorPosition { row: 9, col: 4 }),
        };

        let clipped = super::clip_render_output_to_height(output, 4);
        let text = output_text(&clipped);

        assert_eq!(clipped.lines.len(), 4);
        assert!(text.contains("line 6"));
        assert!(text.contains("line 9"));
        assert_eq!(clipped.cursor, Some(CursorPosition { row: 3, col: 4 }));
    }

    #[test]
    fn wrap_text_preserves_readable_lines() {
        let lines = wrap_text("one two three four", 7);
        assert_eq!(
            lines,
            vec![
                "one two".to_string(),
                "three".to_string(),
                "four".to_string()
            ]
        );
    }

    #[test]
    fn discover_session_paths_finds_nested_jsonl_files() {
        let tempdir = tempdir().expect("tempdir");
        let dir = tempdir.path().join("sessions");
        std::fs::create_dir_all(dir.join("nested")).expect("nested");
        std::fs::write(dir.join("a.jsonl"), "").expect("a");
        std::fs::write(dir.join("nested").join("b.jsonl"), "").expect("b");

        let paths = discover_session_paths(&dir);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn tree_no_tools_filter_hides_tool_results_and_tool_only_assistants() {
        let tool_result = TreeListItem {
            entry_id: "tool".to_string(),
            entry_type: "message".to_string(),
            message_role: Some("toolResult".to_string()),
            assistant_tool_only: false,
            preview: "tool result".to_string(),
            depth: 0,
            label: None,
        };
        let assistant_tool_only = TreeListItem {
            entry_id: "assistant".to_string(),
            entry_type: "message".to_string(),
            message_role: Some("assistant".to_string()),
            assistant_tool_only: true,
            preview: "assistant: tool".to_string(),
            depth: 0,
            label: None,
        };

        assert!(!tree_item_matches_mode(
            &tool_result,
            TreeFilterMode::NoTools
        ));
        assert!(!tree_item_matches_mode(
            &assistant_tool_only,
            TreeFilterMode::NoTools
        ));
    }

    #[test]
    fn session_overlay_items_mark_current_and_filter_named_records() {
        let records = vec![
            SessionRecord {
                path: PathBuf::from("/tmp/current.jsonl"),
                cwd: PathBuf::from("/workspace"),
                name: Some("Named Session".to_string()),
                preview: "preview".to_string(),
                message_count: 4,
                modified_epoch_ms: 20,
                parent_session: None,
            },
            SessionRecord {
                path: PathBuf::from("/tmp/other.jsonl"),
                cwd: PathBuf::from("/workspace"),
                name: None,
                preview: "fallback preview".to_string(),
                message_count: 2,
                modified_epoch_ms: 10,
                parent_session: None,
            },
        ];

        let (items, selections) = super::overlays::session::build_session_overlay_items(
            records,
            None,
            SessionSortMode::Recent,
            SessionNameFilter::Named,
            false,
            Some("/tmp/current.jsonl"),
        );

        assert_eq!(items.len(), 1);
        assert!(items[0].label.contains("Named Session"));
        assert!(
            items[0]
                .description
                .as_deref()
                .is_some_and(|description| description.contains("4 msg ·"))
        );
        match &selections[0] {
            super::OverlaySelection::Session { path } => {
                assert_eq!(path.as_path(), Path::new("/tmp/current.jsonl"));
            }
            other => panic!("unexpected selection: {other:?}"),
        }
    }

    #[test]
    fn session_overlay_items_support_regex_queries() {
        let records = vec![
            SessionRecord {
                path: PathBuf::from("/tmp/current.jsonl"),
                cwd: PathBuf::from("/workspace"),
                name: Some("Build Session".to_string()),
                preview: "Investigating startup footer".to_string(),
                message_count: 4,
                modified_epoch_ms: 20,
                parent_session: None,
            },
            SessionRecord {
                path: PathBuf::from("/tmp/other.jsonl"),
                cwd: PathBuf::from("/workspace"),
                name: Some("Docs".to_string()),
                preview: "Unrelated notes".to_string(),
                message_count: 2,
                modified_epoch_ms: 10,
                parent_session: None,
            },
        ];

        let (items, _) = super::overlays::session::build_session_overlay_items(
            records,
            Some("re:startup\\s+footer"),
            SessionSortMode::Relevance,
            SessionNameFilter::All,
            false,
            None,
        );

        assert_eq!(items.len(), 1);
        assert!(items[0].label.contains("Build Session"));
    }

    #[test]
    fn session_overlay_items_support_exact_phrase_queries() {
        let records = vec![
            SessionRecord {
                path: PathBuf::from("/tmp/current.jsonl"),
                cwd: PathBuf::from("/workspace"),
                name: Some("Exact Phrase".to_string()),
                preview: "Need the exact phrase for startup footer parity".to_string(),
                message_count: 4,
                modified_epoch_ms: 20,
                parent_session: None,
            },
            SessionRecord {
                path: PathBuf::from("/tmp/other.jsonl"),
                cwd: PathBuf::from("/workspace"),
                name: Some("Partial".to_string()),
                preview: "Need startup parity work".to_string(),
                message_count: 2,
                modified_epoch_ms: 10,
                parent_session: None,
            },
        ];

        let (items, _) = super::overlays::session::build_session_overlay_items(
            records,
            Some("\"exact phrase\" footer"),
            SessionSortMode::Relevance,
            SessionNameFilter::All,
            false,
            None,
        );

        assert_eq!(items.len(), 1);
        assert!(items[0].label.contains("Exact Phrase"));
    }

    #[test]
    fn build_model_overlay_items_marks_current_model_and_context() {
        let model = sample_state().model.expect("model");
        let (items, selections) =
            super::build_model_overlay_items(std::slice::from_ref(&model), Some(&model));
        assert_eq!(items.len(), 1);
        assert!(items[0].label.contains("[openai]"));
        assert!(items[0].label.contains("✓"));
        assert!(
            items[0]
                .description
                .as_deref()
                .is_some_and(|description| description.contains("200k ctx"))
        );
        assert!(matches!(
            selections.first(),
            Some(super::OverlaySelection::Model { provider, model_id })
                if provider == "openai" && model_id == "gpt-5.1-codex"
        ));
    }

    #[test]
    fn model_overlay_metadata_mentions_scope_when_scoped_models_exist() {
        let model = sample_state().model.expect("model");
        let (items, _) =
            super::build_model_overlay_items(std::slice::from_ref(&model), Some(&model));
        let mut overlay =
            SearchOverlay::new("Model Selector", String::new(), items, None, String::new());
        super::update_model_overlay_metadata(
            &mut overlay,
            12,
            3,
            Some(&model),
            super::ModelOverlayScope::Scoped,
        );
        assert!(overlay.subtitle.contains("scope scoped"));
        assert!(overlay.subtitle.contains("12 all"));
        assert!(
            overlay
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("reasoning"))
        );
        assert!(overlay.hint.contains("Tab toggles scope"));
    }

    #[test]
    fn overlay_banner_renders_multiline_subtitle() {
        let overlay = SearchOverlay::new(
            "Resume Session",
            "line one\nline two",
            vec![SelectItem {
                value: "one".to_string(),
                label: "First".to_string(),
                description: None,
            }],
            None,
            "Esc cancels",
        );
        let rendered = output_text(&overlay.render(80));
        assert!(rendered.contains("line one"));
        assert!(rendered.contains("line two"));
    }

    #[test]
    fn search_overlay_renders_detail_line() {
        let mut overlay = SearchOverlay::new(
            "Model Selector",
            "scope scoped",
            vec![SelectItem {
                value: "one".to_string(),
                label: "First".to_string(),
                description: Some("Selected detail".to_string()),
            }],
            None,
            "Esc cancels",
        );
        overlay.set_detail(Some("Selected detail".to_string()));
        let rendered = output_text(&overlay.render(80));
        assert!(rendered.contains("Selected detail"));
    }

    #[test]
    fn session_selection_detail_includes_preview_and_paths() {
        let detail = session_selection_detail(
            &SessionRecord {
                path: PathBuf::from("/tmp/session.jsonl"),
                cwd: PathBuf::from("/workspace/pi"),
                name: Some("Named Session".to_string()),
                preview: "first prompt from transcript".to_string(),
                message_count: 5,
                modified_epoch_ms: 0,
                parent_session: Some("/tmp/parent.jsonl".to_string()),
            },
            true,
        );
        assert!(detail.contains("first prompt from transcript"));
        assert!(detail.contains("cwd /workspace/pi"));
        assert!(detail.contains("session /tmp/session.jsonl"));
        assert!(detail.contains("parent /tmp/parent.jsonl"));
        assert!(detail.contains("current"));
    }

    #[test]
    fn diff_lines_highlight_single_line_replacements() {
        let lines = collect_diff_lines("-  12 let value = 1;\n+  12 let value = 2;", 120);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\u{1b}[7m"));
        assert!(lines[1].contains("\u{1b}[7m"));
    }

    #[test]
    fn active_tool_render_lines_show_bash_output_text() {
        let lines = active_tool_render_lines(
            &[super::ActiveToolExecution {
                tool_call_id: "call-1".to_string(),
                tool_name: "bash".to_string(),
                args: json!({"command": "echo hi"}),
                partial_result: Some(json!({"output": "first line\nsecond line"})),
            }],
            80,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                pi_rust_tui::RenderedLine::Text(text) => strip_ansi(&text),
                pi_rust_tui::RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(!rendered.iter().any(|line| line.contains("Running Bash")));
        assert!(!rendered.iter().any(|line| line.contains("Bash")));
        assert!(rendered.iter().any(|line| line.contains("$ echo hi")));
        assert!(rendered.iter().any(|line| line.contains("echo hi")));
        assert!(rendered.iter().any(|line| line.contains("first line")));
        assert!(rendered.iter().any(|line| line.contains("second line")));
    }

    #[test]
    fn pending_write_tool_call_renders_preview_instead_of_raw_json() {
        let mut lines = Vec::new();
        render_transcript_entry(
            &mut lines,
            &TranscriptEntry::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantContentBlock::ToolCall {
                    id: "call-write".to_string(),
                    name: "write".to_string(),
                    arguments: json!({
                        "path": "src/main.rs",
                        "content": "fn main() {\n    println!(\"hi\");\n}"
                    }),
                    thought_signature: None,
                }],
                ..assistant_message("")
            })),
            0,
            None,
            80,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("write src/main.rs"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("println!(\"hi\");"))
        );
        assert!(!rendered.iter().any(|line| line.contains("preview")));
        assert!(!rendered.iter().any(|line| line.contains("\"content\"")));
    }

    #[test]
    fn pending_edit_tool_call_renders_replace_preview() {
        let mut lines = Vec::new();
        render_transcript_entry(
            &mut lines,
            &TranscriptEntry::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantContentBlock::ToolCall {
                    id: "call-edit".to_string(),
                    name: "edit".to_string(),
                    arguments: json!({
                        "path": "src/lib.rs",
                        "oldText": "let value = 1;",
                        "newText": "let value = 2;"
                    }),
                    thought_signature: None,
                }],
                ..assistant_message("")
            })),
            0,
            None,
            80,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("edit src/lib.rs")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("- let value = 1;"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("+ let value = 2;"))
        );
        assert!(!rendered.iter().any(|line| line.contains("replace")));
        assert!(!rendered.iter().any(|line| line.contains("\"oldText\"")));
    }

    #[test]
    fn active_tool_render_lines_show_diff_lines() {
        let lines = active_tool_render_lines(
            &[super::ActiveToolExecution {
                tool_call_id: "call-2".to_string(),
                tool_name: "edit".to_string(),
                args: json!({"path": "file.txt"}),
                partial_result: Some(json!({"diff": "@@ -1 +1 @@\n-old\n+new"})),
            }],
            80,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                pi_rust_tui::RenderedLine::Text(text) => strip_ansi(&text),
                pi_rust_tui::RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("@@ -1 +1 @@")));
        assert!(rendered.iter().any(|line| line.contains("-old")));
        assert!(rendered.iter().any(|line| line.contains("+new")));
    }

    #[test]
    fn live_assistant_thinking_renders_visible_or_collapsed_based_on_setting() {
        let mut transcript = vec![TranscriptEntry::Message(Message::User(UserMessage {
            content: UserContent::Text("hello".to_string()),
            timestamp: 0,
        }))];
        apply_live_transcript_message(
            &mut transcript,
            Message::Assistant(assistant_message_with_thinking(
                "thinking stream",
                "final answer",
            )),
        );

        let visible = session_transcript_lines(
            &transcript,
            80,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );
        let hidden = session_transcript_lines(
            &transcript,
            80,
            true,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let visible_text = output_text(&RenderOutput {
            lines: visible,
            cursor: None,
        });
        let hidden_text = output_text(&RenderOutput {
            lines: hidden,
            cursor: None,
        });

        assert!(visible_text.contains("thinking stream"));
        assert!(visible_text.contains("final answer"));
        assert!(hidden_text.contains("Thinking..."));
        assert!(!hidden_text.contains("thinking stream"));
    }

    #[test]
    fn user_messages_render_as_padded_surface_without_log_prefix() {
        let mut lines = Vec::new();
        render_transcript_entry(
            &mut lines,
            &TranscriptEntry::Message(Message::User(UserMessage {
                content: UserContent::Text("**hello** world".to_string()),
                timestamp: 0,
            })),
            0,
            None,
            40,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("hello world")));
        assert!(!rendered.iter().any(|line| line.contains("You:")));
    }

    #[test]
    fn assistant_markdown_renders_without_assistant_prefix() {
        let mut lines = Vec::new();
        render_transcript_entry(
            &mut lines,
            &TranscriptEntry::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantContentBlock::Text {
                    text: "# Heading\n\n```rs\nfn main() {}\n```".to_string(),
                    text_signature: None,
                }],
                ..assistant_message("")
            })),
            0,
            None,
            60,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("Heading")));
        assert!(rendered.iter().any(|line| line.contains("fn main() {}")));
        assert!(!rendered.iter().any(|line| line.contains("Assistant:")));
    }

    #[test]
    fn transcript_tool_result_uses_correlated_read_args_for_title_and_range() {
        let lines = session_transcript_lines(
            &[
                TranscriptEntry::Message(Message::Assistant(AssistantMessage {
                    content: vec![AssistantContentBlock::ToolCall {
                        id: "call-read".to_string(),
                        name: "read".to_string(),
                        arguments: json!({"path": "/tmp/example.rs", "offset": 2, "limit": 2}),
                        thought_signature: None,
                    }],
                    ..assistant_message("")
                })),
                TranscriptEntry::Message(Message::ToolResult(pi_rust_ai_core::ToolResultMessage {
                    tool_call_id: "call-read".to_string(),
                    tool_name: "read".to_string(),
                    content: vec![UserContentBlock::Text {
                        text: "fn answer() {}\nreturn 42;\n\n[2 more lines in file. Use offset=4 to continue.]"
                            .to_string(),
                        text_signature: None,
                    }],
                    details: None,
                    is_error: false,
                    timestamp: 0,
                })),
            ],
            80,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("read /tmp/example.rs:2-3"))
        );
        assert!(rendered.iter().any(|line| line.contains("fn answer() {}")));
        assert!(rendered.iter().any(|line| line.contains("return 42;")));
        assert!(!rendered.iter().any(|line| line.contains("contents 2-3")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("[2 more lines in file. Use offset=4 to continue.]"))
        );
    }

    #[test]
    fn transcript_tool_result_uses_correlated_write_args_for_preview() {
        let lines = session_transcript_lines(
            &[
                TranscriptEntry::Message(Message::Assistant(AssistantMessage {
                    content: vec![AssistantContentBlock::ToolCall {
                        id: "call-write".to_string(),
                        name: "write".to_string(),
                        arguments: json!({"path": "src/main.rs", "content": "fn main() {\n    println!(\"hi\");\n}"}),
                        thought_signature: None,
                    }],
                    ..assistant_message("")
                })),
                TranscriptEntry::Message(Message::ToolResult(pi_rust_ai_core::ToolResultMessage {
                    tool_call_id: "call-write".to_string(),
                    tool_name: "write".to_string(),
                    content: vec![UserContentBlock::Text {
                        text: "Successfully wrote 31 bytes to src/main.rs".to_string(),
                        text_signature: None,
                    }],
                    details: None,
                    is_error: false,
                    timestamp: 0,
                })),
            ],
            80,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("write src/main.rs"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("println!(\"hi\");"))
        );
        assert!(!rendered.iter().any(|line| line.contains("preview")));
    }

    #[test]
    fn transcript_tool_result_formats_grep_results_with_structured_prefixes() {
        let lines = session_transcript_lines(
            &[TranscriptEntry::Message(Message::ToolResult(
                pi_rust_ai_core::ToolResultMessage {
                    tool_call_id: "call-grep".to_string(),
                    tool_name: "grep".to_string(),
                    content: vec![UserContentBlock::Text {
                        text: "src/app.ts:2: console.log(value)\nsrc/app.ts-1- const value = 1;\n\n[Some lines truncated to 400 chars. Use read tool to see full lines]"
                            .to_string(),
                        text_signature: None,
                    }],
                    details: None,
                    is_error: false,
                    timestamp: 0,
                },
            ))],
            90,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("grep .")));
        assert!(rendered.iter().any(|line| line.contains("src/app.ts")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("2: console.log(value)"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("console.log(value)"))
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("1- const value = 1;"))
        );
        assert!(rendered.iter().any(|line| line.contains("Use read tool")));
    }

    #[test]
    fn transcript_tool_result_formats_ls_directories_distinctly() {
        let lines = session_transcript_lines(
            &[TranscriptEntry::Message(Message::ToolResult(
                pi_rust_ai_core::ToolResultMessage {
                    tool_call_id: "call-ls".to_string(),
                    tool_name: "ls".to_string(),
                    content: vec![UserContentBlock::Text {
                        text: "alpha.txt\nbeta/\n\n[500 entries limit reached. Use limit=1000 for more]"
                            .to_string(),
                        text_signature: None,
                    }],
                    details: None,
                    is_error: false,
                    timestamp: 0,
                },
            ))],
            80,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("ls")));
        assert!(rendered.iter().any(|line| line.contains("alpha.txt")));
        assert!(rendered.iter().any(|line| line.contains("beta/")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("500 entries limit reached"))
        );
    }

    #[test]
    fn render_custom_bash_message_includes_command_and_output() {
        let mut lines = Vec::new();
        render_transcript_entry(
            &mut lines,
            &TranscriptEntry::CustomMessage {
                custom_type: "bash_execution".to_string(),
                content: UserContent::Blocks(vec![UserContentBlock::Text {
                    text: "$ echo hi\nhi\nExit code: 0".to_string(),
                    text_signature: None,
                }]),
                details: Some(json!({"output":"hi","exitCode":0})),
            },
            0,
            None,
            80,
            false,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );

        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(!rendered.iter().any(|line| line.contains("Bash")));
        assert!(rendered.iter().any(|line| line.contains("$ echo hi")));
        assert!(rendered.iter().any(|line| line.contains("hi")));
        assert!(!rendered.iter().any(|line| line.contains("Exit code: 0")));
    }

    #[test]
    fn transcript_image_blocks_only_emit_image_lines_when_terminal_supports_it() {
        let content = UserContent::Blocks(vec![UserContentBlock::Image {
            data: "AAAA".to_string(),
            mime_type: "image/png".to_string(),
        }]);

        let mut fallback_lines = Vec::new();
        render_transcript_entry(
            &mut fallback_lines,
            &TranscriptEntry::Message(Message::User(UserMessage {
                content: content.clone(),
                timestamp: 0,
            })),
            0,
            None,
            80,
            false,
            true,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );
        assert_eq!(
            fallback_lines
                .iter()
                .filter(|line| matches!(line, RenderedLine::Image(_)))
                .count(),
            0
        );

        let mut inline_lines = Vec::new();
        render_transcript_entry(
            &mut inline_lines,
            &TranscriptEntry::Message(Message::User(UserMessage {
                content,
                timestamp: 0,
            })),
            0,
            None,
            80,
            false,
            true,
            &with_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );
        assert!(
            inline_lines
                .iter()
                .any(|line| matches!(line, RenderedLine::Image(_)))
        );
    }

    #[test]
    fn active_tool_render_lines_show_expand_hint_when_collapsed() {
        let lines = active_tool_render_lines(
            &[super::ActiveToolExecution {
                tool_call_id: "call-3".to_string(),
                tool_name: "grep".to_string(),
                args: json!({"pattern": "foo"}),
                partial_result: Some(json!({
                    "output": "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nsixteen\nseventeen\neighteen\nnineteen\ntwenty"
                })),
            }],
            80,
            false,
            &no_inline_images(),
            ToolExpandMode::Collapsed,
            None,
            "ctrl+o",
        );
        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("ctrl+o to expand"))
        );
    }

    #[test]
    fn active_tool_render_lines_show_collapse_hint_when_expanded() {
        let lines = active_tool_render_lines(
            &[super::ActiveToolExecution {
                tool_call_id: "call-4".to_string(),
                tool_name: "grep".to_string(),
                args: json!({"pattern": "foo"}),
                partial_result: Some(json!({
                    "output": "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nsixteen\nseventeen\neighteen\nnineteen\ntwenty"
                })),
            }],
            80,
            false,
            &no_inline_images(),
            ToolExpandMode::All,
            None,
            "ctrl+o",
        );
        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("twenty")));
        assert!(
            !rendered
                .iter()
                .any(|line| line.contains("ctrl+o to expand"))
        );
    }

    #[test]
    fn content_text_joins_text_blocks_and_skips_images() {
        let text = content_text(&UserContent::Blocks(vec![
            UserContentBlock::Text {
                text: "first".to_string(),
                text_signature: None,
            },
            UserContentBlock::Image {
                data: "AAAA".to_string(),
                mime_type: "image/png".to_string(),
            },
            UserContentBlock::Text {
                text: "second".to_string(),
                text_signature: None,
            },
        ]));

        assert_eq!(text, "first\nsecond");
    }

    #[test]
    fn tool_expand_mode_all_expands_all_active_panels() {
        let lines = active_tool_render_lines(
            &[
                super::ActiveToolExecution {
                    tool_call_id: "call-a".to_string(),
                    tool_name: "grep".to_string(),
                    args: json!({"pattern": "foo"}),
                    partial_result: Some(
                        json!({"output": "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\nthirteen\nfourteen\nfifteen\nsixteen\nseventeen"}),
                    ),
                },
                super::ActiveToolExecution {
                    tool_call_id: "call-b".to_string(),
                    tool_name: "grep".to_string(),
                    args: json!({"pattern": "bar"}),
                    partial_result: Some(
                        json!({"output": "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\niota\nkappa\nlambda\nmu\nnu\nxi\nomicron\npi\nrho"}),
                    ),
                },
            ],
            80,
            false,
            &no_inline_images(),
            ToolExpandMode::All,
            None,
            "ctrl+o",
        );
        let rendered = lines
            .into_iter()
            .map(|line| match line {
                RenderedLine::Text(text) => strip_ansi(&text),
                RenderedLine::Image(_) => "[image]".to_string(),
            })
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains("seventeen")));
        assert!(rendered.iter().any(|line| line.contains("rho")));
        assert!(
            !rendered
                .iter()
                .any(|line| line.contains("ctrl+o to expand"))
        );
    }

    fn assistant_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            content: vec![AssistantContentBlock::Text {
                text: text.to_string(),
                text_signature: None,
            }],
            api: ApiId::new("openai-responses"),
            provider: ProviderId::new("openai"),
            model: "gpt-5.1-codex".to_string(),
            usage: Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                total_tokens: 0,
                cost: UsageCost {
                    input: "0".to_string(),
                    output: "0".to_string(),
                    cache_read: "0".to_string(),
                    cache_write: "0".to_string(),
                    total: "0".to_string(),
                },
            },
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }

    fn assistant_message_with_thinking(thinking: &str, text: &str) -> AssistantMessage {
        AssistantMessage {
            content: vec![
                AssistantContentBlock::Thinking {
                    thinking: thinking.to_string(),
                    thinking_signature: None,
                },
                AssistantContentBlock::Text {
                    text: text.to_string(),
                    text_signature: None,
                },
            ],
            ..assistant_message("")
        }
    }
}
