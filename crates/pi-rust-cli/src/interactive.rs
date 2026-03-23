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
use pi_rust_config::get_sessions_dir;
use pi_rust_core::{
    AgentControl, AgentEvent, AgentSession, ForkableUserMessage, NonInteractiveRequest, PromptRun,
    SessionTreeNode, StartupResourceNoticeSection, StartupResourceSummary, create_agent_session,
};
use pi_rust_models::{ModelRegistry, supports_xhigh};
use pi_rust_oauth::{
    AuthCredential, AuthSource, OAuthAuthInfo, OAuthCredentials, OAuthLoginBridge, OAuthPrompt,
    get_oauth_providers, login_oauth_provider,
};
use pi_rust_packages::{InstalledPackage, PackageInstallScope, PackageManager};
use pi_rust_protocol::{
    QueueMode, RpcCommandLocation, RpcCommandSource, RpcSessionState, RpcSessionStats,
    RpcSlashCommand,
};
use pi_rust_resources::{
    ResourceDiscoveryOptions, ResourceScope, ScopedPath, discover_resources_with_options,
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
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use crate::keybindings::{
    AppAction, EditorAction, KeybindingsManager, PromptAutocompleteInput, PromptEditorInput,
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut package_manager = PackageManager::create(&cwd, None);
    let mut terminal = ProcessTerminal::new();
    terminal.start().map_err(|error| error.to_string())?;
    terminal
        .set_title("Resource Configuration")
        .map_err(|error| error.to_string())?;
    terminal.hide_cursor().map_err(|error| error.to_string())?;

    let mut renderer = LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });

    let mut entries = build_config_entries(&package_manager);
    let mut overlay = SearchOverlay::new(
        "Resource Configuration",
        "Type to filter resources",
        entries.iter().map(|entry| entry.item.clone()).collect(),
        None,
        "Enter details · Ctrl+R updates · Delete removes · Esc closes",
    );
    let mut status = None;
    let mut armed_remove: Option<(String, PackageInstallScope, Instant)> = None;

    loop {
        let (width, _height) = terminal.size().map_err(|error| error.to_string())?;
        let mut output = overlay.render(width);
        if let Some(status) = status.as_deref() {
            append_blank_lines(&mut output, width, 1);
            append_output(&mut output, Text::new(status).render(width), false);
        }
        renderer
            .render(&mut terminal, &output, width)
            .map_err(|error| error.to_string())?;

        let events = terminal.read_events().map_err(|error| error.to_string())?;
        for event in events {
            let selected = overlay
                .selected_value()
                .and_then(|value| find_config_selection(&entries, value).cloned());

            if matches_ctrl_char(&event, 'r') {
                match selected {
                    Some(ConfigSelection::Package { source, .. }) => {
                        let updated = package_manager
                            .update(Some(&source))
                            .map_err(|error| error.to_string())?;
                        let selected_value = overlay.selected_value().map(ToOwned::to_owned);
                        entries = build_config_entries(&package_manager);
                        overlay.replace_items_preserving_selection(
                            entries.iter().map(|entry| entry.item.clone()).collect(),
                            selected_value.as_deref(),
                        );
                        status = Some(if updated.is_empty() {
                            format!("No installed package matched {source}.")
                        } else {
                            format!("Updated {source}.")
                        });
                        armed_remove = None;
                    }
                    _ => {
                        status = Some("Select an installed package to update it.".to_string());
                    }
                }
                continue;
            }

            if matches!(event.code, KeyCode::Delete | KeyCode::Backspace)
                && event.modifiers == KeyModifiers::NONE
            {
                match selected {
                    Some(ConfigSelection::Package { source, scope, .. }) => {
                        let now = Instant::now();
                        let remove_confirmed = armed_remove.as_ref().is_some_and(
                            |(armed_source, armed_scope, armed_at)| {
                                armed_source == &source
                                    && armed_scope == &scope
                                    && now.duration_since(*armed_at) <= Duration::from_secs(2)
                            },
                        );
                        if remove_confirmed {
                            let removed = package_manager
                                .remove(&source, scope)
                                .map_err(|error| error.to_string())?;
                            entries = build_config_entries(&package_manager);
                            overlay.replace_items_preserving_selection(
                                entries.iter().map(|entry| entry.item.clone()).collect(),
                                None,
                            );
                            status = Some(if removed {
                                format!("Removed {source}.")
                            } else {
                                format!("Package {source} was already absent.")
                            });
                            armed_remove = None;
                        } else {
                            armed_remove = Some((source.clone(), scope, now));
                            status =
                                Some(format!("Press Delete again within 2s to remove {source}."));
                        }
                    }
                    _ => {
                        status = Some("Select an installed package to remove it.".to_string());
                    }
                }
                continue;
            }

            match overlay.handle_key(&event) {
                SearchOverlayEvent::Continue => {}
                SearchOverlayEvent::Cancelled => {
                    let _ = renderer.clear(&mut terminal);
                    let _ = terminal.show_cursor();
                    let _ = terminal.stop();
                    return Ok(());
                }
                SearchOverlayEvent::Selected(item) => {
                    status = Some(
                        find_config_selection(&entries, &item.value)
                            .map(config_selection_status)
                            .unwrap_or_else(|| item.description.unwrap_or_else(|| item.label)),
                    );
                    armed_remove = None;
                }
            }
        }
    }
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

struct ActivePrompt {
    event_rx: mpsc::Receiver<AgentEvent>,
    result_rx: mpsc::Receiver<Result<PromptRun, String>>,
    handle: Option<thread::JoinHandle<()>>,
    aborted: bool,
    started_at: Instant,
    completion_result: Option<Result<PromptRun, String>>,
    linger_after_completion: bool,
}

struct ActiveAuthFlow {
    provider: String,
    ui_rx: mpsc::Receiver<AuthUiRequest>,
    response_tx: mpsc::Sender<AuthUiResponse>,
    result_rx: mpsc::Receiver<Result<OAuthCredentials, String>>,
    cancel_flag: Arc<AtomicBool>,
    handle: thread::JoinHandle<()>,
    started_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthPromptKind {
    Prompt,
    ManualCode,
}

#[derive(Clone, Debug)]
enum AuthUiRequest {
    ShowAuth(OAuthAuthInfo),
    Prompt {
        prompt: OAuthPrompt,
        kind: AuthPromptKind,
    },
    Progress(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AuthUiResponse {
    Input(String),
    Cancelled,
}

struct ChannelOAuthLoginBridge {
    ui_tx: mpsc::Sender<AuthUiRequest>,
    response_tx: mpsc::Sender<AuthUiResponse>,
    response_rx: Arc<Mutex<mpsc::Receiver<AuthUiResponse>>>,
    cancel_flag: Arc<AtomicBool>,
}

impl OAuthLoginBridge for ChannelOAuthLoginBridge {
    fn show_auth(&self, info: OAuthAuthInfo) -> Result<(), String> {
        self.ui_tx
            .send(AuthUiRequest::ShowAuth(info))
            .map_err(|_| "Failed to send auth URL to interactive UI.".to_string())
    }

    fn prompt(&self, prompt: OAuthPrompt) -> Result<String, String> {
        self.ui_tx
            .send(AuthUiRequest::Prompt {
                prompt,
                kind: AuthPromptKind::Prompt,
            })
            .map_err(|_| "Failed to request login input.".to_string())?;
        match self
            .response_rx
            .lock()
            .map_err(|_| "Failed to lock login input receiver.".to_string())?
            .recv()
            .map_err(|_| "Login input channel disconnected.".to_string())?
        {
            AuthUiResponse::Input(value) => Ok(value),
            AuthUiResponse::Cancelled => Err("Login cancelled".to_string()),
        }
    }

    fn manual_code_input(&self, prompt: OAuthPrompt) -> Result<String, String> {
        self.ui_tx
            .send(AuthUiRequest::Prompt {
                prompt,
                kind: AuthPromptKind::ManualCode,
            })
            .map_err(|_| "Failed to request authorization code input.".to_string())?;
        match self
            .response_rx
            .lock()
            .map_err(|_| "Failed to lock login input receiver.".to_string())?
            .recv()
            .map_err(|_| "Login input channel disconnected.".to_string())?
        {
            AuthUiResponse::Input(value) => Ok(value),
            AuthUiResponse::Cancelled => Err("Login cancelled".to_string()),
        }
    }

    fn progress(&self, message: &str) -> Result<(), String> {
        self.ui_tx
            .send(AuthUiRequest::Progress(message.to_string()))
            .map_err(|_| "Failed to send login progress to interactive UI.".to_string())
    }

    fn cancel_pending_input(&self) {
        let _ = self.response_tx.send(AuthUiResponse::Cancelled);
    }

    fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OverlaySelection {
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
enum SearchOverlayKind {
    Tree,
    OAuthLogin,
    OAuthLogout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelOverlayScope {
    All,
    Scoped,
}

impl ModelOverlayScope {
    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Scoped => "scoped",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Scoped,
            Self::Scoped => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionScope {
    Current,
    All,
}

impl SessionScope {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::All => "all",
        }
    }

    fn toggle(self) -> Self {
        match self {
            Self::Current => Self::All,
            Self::All => Self::Current,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionSortMode {
    Threaded,
    Recent,
    Relevance,
}

impl SessionSortMode {
    fn label(self) -> &'static str {
        match self {
            Self::Threaded => "threaded",
            Self::Recent => "recent",
            Self::Relevance => "relevance",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Threaded => Self::Recent,
            Self::Recent => Self::Relevance,
            Self::Relevance => Self::Threaded,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionNameFilter {
    All,
    Named,
}

impl SessionNameFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Named => "named",
        }
    }

    fn toggle(self) -> Self {
        match self {
            Self::All => Self::Named,
            Self::Named => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthFlowMode {
    Login,
    Logout,
}

#[derive(Clone, Debug)]
struct SessionRecord {
    path: PathBuf,
    cwd: PathBuf,
    name: Option<String>,
    preview: String,
    message_count: usize,
    modified_epoch_ms: i64,
    parent_session: Option<String>,
}

#[derive(Clone, Debug)]
struct SessionOverlayRow {
    record: SessionRecord,
    depth: usize,
    is_last: bool,
    ancestor_continues: Vec<bool>,
}

struct SessionOverlayState {
    overlay: SearchOverlay,
    selections: Vec<OverlaySelection>,
    records: Vec<SessionRecord>,
    rows: Vec<SessionOverlayRow>,
    current_session_file: Option<PathBuf>,
    standalone: bool,
    scope: SessionScope,
    sort_mode: SessionSortMode,
    name_filter: SessionNameFilter,
    show_path: bool,
    confirming_delete: Option<PathBuf>,
}

struct ModelOverlayState {
    overlay: SearchOverlay,
    selections: Vec<OverlaySelection>,
    models: Vec<Model>,
    current_model: Option<Model>,
    scope: ModelOverlayScope,
    available_count: usize,
    scoped_count: usize,
}

struct ScopedModelsOverlayState {
    overlay: SearchOverlay,
    models: Vec<Model>,
    enabled_ids: Option<Vec<String>>,
    dirty: bool,
}

struct SettingsOverlayState {
    title: String,
    subtitle: String,
    hint: String,
    list: SettingsList,
}

struct ForkOverlayState {
    title: String,
    subtitle: String,
    hint: String,
    list: SelectList,
    selections: Vec<OverlaySelection>,
    messages: Vec<ForkableUserMessage>,
}

struct TreeSummaryOverlayState {
    title: String,
    hint: String,
    list: SelectList,
    target_entry_id: String,
    filter_mode: TreeFilterMode,
    query: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionSearchMode {
    Tokens,
    Regex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionSearchTokenKind {
    Fuzzy,
    Phrase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionSearchToken {
    kind: SessionSearchTokenKind,
    value: String,
}

#[derive(Debug)]
struct ParsedSessionSearchQuery {
    mode: SessionSearchMode,
    tokens: Vec<SessionSearchToken>,
    regex: Option<Regex>,
    error: Option<String>,
}

enum InputOverlayAction {
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

struct InputOverlayState {
    title: String,
    subtitle: String,
    message_lines: Vec<String>,
    hint: String,
    input: Input,
    action: InputOverlayAction,
}

struct AuthOverlayState {
    provider: String,
    subtitle: String,
    message_lines: Vec<String>,
    input: Input,
    awaiting_input: bool,
    prompt_kind: Option<AuthPromptKind>,
}

impl AuthOverlayState {
    fn new(provider: &str) -> Self {
        let mut input = Input::with_prompt("Input: ");
        input.set_focused(true);
        Self {
            provider: provider.to_string(),
            subtitle: "Waiting for provider instructions...".to_string(),
            message_lines: vec!["Preparing OAuth login flow...".to_string()],
            input,
            awaiting_input: false,
            prompt_kind: None,
        }
    }

    fn set_auth_info(&mut self, info: OAuthAuthInfo) {
        self.subtitle = "Open the URL below and complete login in your browser.".to_string();
        self.message_lines = vec![info.url];
        if let Some(instructions) = info.instructions {
            self.message_lines.push(instructions);
        }
        self.message_lines.push(if cfg!(target_os = "macos") {
            "Cmd+click the URL if the browser did not open automatically.".to_string()
        } else {
            "Ctrl+click the URL if the browser did not open automatically.".to_string()
        });
    }

    fn set_prompt(&mut self, prompt: OAuthPrompt, kind: AuthPromptKind) {
        self.awaiting_input = true;
        self.prompt_kind = Some(kind);
        self.input.clear();
        self.input.set_focused(true);
        self.message_lines.push(prompt.message);
        if let Some(placeholder) = prompt.placeholder {
            self.message_lines.push(format!("e.g., {placeholder}"));
        }
    }

    fn push_progress(&mut self, message: String) {
        if self.message_lines.last() != Some(&message) {
            self.message_lines.push(message);
        }
    }

    fn hint(&self) -> &'static str {
        if self.awaiting_input {
            "Enter submits - Esc cancels"
        } else {
            "Esc cancels"
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingKey {
    AutoCompact,
    SteeringMode,
    FollowUpMode,
    Transport,
    ThinkingLevel,
    Theme,
    HideThinking,
    CollapseChangelog,
    QuietStartup,
    ShowImages,
    AutoResizeImages,
    BlockImages,
    SkillCommands,
    ShowHardwareCursor,
    EditorPadding,
    AutocompleteMaxVisible,
    ClearOnShrink,
    DoubleEscapeAction,
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

    fn next(self) -> Self {
        match self {
            Self::Tree => Self::Fork,
            Self::Fork => Self::None,
            Self::None => Self::Tree,
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
enum TreeFilterMode {
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

impl TreeFilterMode {
    fn next(self) -> Self {
        match self {
            Self::Default => Self::NoTools,
            Self::NoTools => Self::UserOnly,
            Self::UserOnly => Self::LabeledOnly,
            Self::LabeledOnly => Self::All,
            Self::All => Self::Default,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::NoTools => "no-tools",
            Self::UserOnly => "user-only",
            Self::LabeledOnly => "labeled-only",
            Self::All => "all",
        }
    }
}

struct SearchOverlay {
    title: String,
    subtitle: String,
    detail: Option<String>,
    hint: String,
    search: Input,
    search_visible: bool,
    list: SelectList,
}

impl SearchOverlay {
    fn new(
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

    fn selected_value(&self) -> Option<&str> {
        self.list.selected_item().map(|item| item.value.as_str())
    }

    fn set_subtitle(&mut self, subtitle: impl Into<String>) {
        self.subtitle = subtitle.into();
    }

    fn set_title(&mut self, title: impl Into<String>) {
        self.title = title.into();
    }

    fn set_detail(&mut self, detail: Option<String>) {
        self.detail = detail;
    }

    fn set_hint(&mut self, hint: impl Into<String>) {
        self.hint = hint.into();
    }

    fn set_search_prompt(&mut self, prompt: impl Into<String>) {
        let value = self.search.get_value().to_string();
        let mut search = Input::with_prompt(prompt.into());
        search.set_focused(true);
        if !value.is_empty() {
            search.set_value(value);
        }
        self.search = search;
    }

    fn set_search_visible(&mut self, visible: bool) {
        self.search_visible = visible;
        self.search.set_focused(visible);
    }

    fn selected_item(&self) -> Option<&SelectItem> {
        self.list.selected_item()
    }

    fn replace_items_preserving_selection(
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

    fn handle_key(&mut self, event: &KeyEvent) -> SearchOverlayEvent {
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
enum SearchOverlayEvent {
    Continue,
    Selected(SelectItem),
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptAutocompleteKind {
    SlashCommand,
    ModelArgument,
    Path,
    FileReference,
}

struct PromptAutocompleteState {
    kind: PromptAutocompleteKind,
    title: String,
    subtitle: String,
    hint: String,
    replace_prefix: String,
    list: SelectList,
}

#[derive(Clone, Copy, Debug)]
struct BuiltinSlashCommand {
    name: &'static str,
    description: &'static str,
}

const BUILTIN_SLASH_COMMANDS: &[BuiltinSlashCommand] = &[
    BuiltinSlashCommand {
        name: "settings",
        description: "Open settings menu",
    },
    BuiltinSlashCommand {
        name: "model",
        description: "Select model (opens selector UI)",
    },
    BuiltinSlashCommand {
        name: "scoped-models",
        description: "Configure which models Ctrl+P cycles through",
    },
    BuiltinSlashCommand {
        name: "export",
        description: "Export session to HTML file",
    },
    BuiltinSlashCommand {
        name: "share",
        description: "Share session as a secret GitHub gist",
    },
    BuiltinSlashCommand {
        name: "copy",
        description: "Copy last agent message to clipboard",
    },
    BuiltinSlashCommand {
        name: "name",
        description: "Set session display name",
    },
    BuiltinSlashCommand {
        name: "session",
        description: "Show session info and stats",
    },
    BuiltinSlashCommand {
        name: "changelog",
        description: "Show changelog entries",
    },
    BuiltinSlashCommand {
        name: "hotkeys",
        description: "Show all keyboard shortcuts",
    },
    BuiltinSlashCommand {
        name: "fork",
        description: "Create a new fork from a previous message",
    },
    BuiltinSlashCommand {
        name: "tree",
        description: "Navigate session tree (switch branches)",
    },
    BuiltinSlashCommand {
        name: "login",
        description: "Login with OAuth provider",
    },
    BuiltinSlashCommand {
        name: "logout",
        description: "Logout from OAuth provider",
    },
    BuiltinSlashCommand {
        name: "new",
        description: "Start a new session",
    },
    BuiltinSlashCommand {
        name: "compact",
        description: "Manually compact the session context",
    },
    BuiltinSlashCommand {
        name: "resume",
        description: "Resume a different session",
    },
    BuiltinSlashCommand {
        name: "reload",
        description: "Reload settings, prompts, skills, and themes from disk",
    },
    BuiltinSlashCommand {
        name: "quit",
        description: "Quit pi-rust",
    },
];

const PATH_DELIMITERS: &[char] = &[' ', '\t', '"', '\'', '='];

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

impl Component for ModelOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput {
            lines: Vec::new(),
            cursor: None,
        };
        append_rule_line(&mut output.lines, width);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(model_overlay_scope_line(self)).render(width),
            false,
        );
        if self.scoped_count > 0 {
            append_output(
                &mut output,
                Text::new(style_hint("Tab scope (all/scoped)")).render(width),
                false,
            );
        }
        append_blank_lines(&mut output, width, 1);
        append_output(&mut output, self.overlay.search.render(width), false);
        append_blank_lines(&mut output, width, 1);
        append_model_overlay_rows(&mut output.lines, self, width as usize);
        if let Some(model) = selected_model_overlay_model(self) {
            append_blank_lines(&mut output, width, 1);
            append_output(
                &mut output,
                Text::new(style_hint(&format!("  Model Name: {}", model.name))).render(width),
                false,
            );
        }
        append_blank_lines(&mut output, width, 1);
        append_rule_line(&mut output.lines, width);
        output
    }
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

impl Component for ScopedModelsOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput::default();
        append_rule_line(&mut output.lines, width);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_title("Model Configuration")).render(width),
            false,
        );
        append_output(
            &mut output,
            Text::new(style_subtitle("Session-only. Ctrl+S to save to settings.")).render(width),
            false,
        );
        append_blank_lines(&mut output, width, 1);
        append_output(&mut output, self.overlay.search.render(width), false);
        append_blank_lines(&mut output, width, 1);
        append_scoped_models_overlay_rows(&mut output.lines, self, width as usize);
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_hint(&scoped_models_footer_text(self))).render(width),
            false,
        );
        append_blank_lines(&mut output, width, 1);
        append_rule_line(&mut output.lines, width);
        output
    }
}

impl Component for SettingsOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput::default();
        append_rule_line(&mut output.lines, width);
        append_blank_lines(&mut output, width, 1);
        append_output(&mut output, self.list.render(width), true);
        append_blank_lines(&mut output, width, 1);
        append_rule_line(&mut output.lines, width);
        output
    }
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

impl Component for AuthOverlayState {
    fn render(&self, width: u16) -> RenderOutput {
        let mut output = RenderOutput {
            lines: Vec::new(),
            cursor: None,
        };
        append_overlay_banner(
            &mut output,
            &format!("Login to {}", oauth_provider_label(&self.provider)),
            &self.subtitle,
            width,
        );
        append_blank_lines(&mut output, width, 1);
        if !self.message_lines.is_empty() {
            append_output(
                &mut output,
                Text::new(self.message_lines.join("\n")).render(width),
                false,
            );
        }
        if self.awaiting_input {
            append_blank_lines(&mut output, width, 1);
            append_output(&mut output, self.input.render(width), true);
        }
        append_blank_lines(&mut output, width, 1);
        append_output(
            &mut output,
            Text::new(style_hint(self.hint())).render(width),
            false,
        );
        output
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum ConfigSelection {
    GlobalSettings {
        path: PathBuf,
    },
    ProjectSettings {
        path: PathBuf,
    },
    Package {
        source: String,
        scope: PackageInstallScope,
        install_path: PathBuf,
    },
    Resource {
        kind: &'static str,
        scope: ResourceScope,
        path: PathBuf,
        owner: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigEntry {
    item: SelectItem,
    selection: ConfigSelection,
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

struct InteractiveApp {
    session: Arc<Mutex<AgentSession>>,
    control: AgentControl,
    keybindings: KeybindingsManager,
    package_manager: PackageManager,
    session_dir_override: Option<PathBuf>,
    terminal_capabilities: TerminalCapabilities,
    editor: Editor,
    prompt_autocomplete: Option<PromptAutocompleteState>,
    status: Option<String>,
    ctrl_c_armed_at: Option<Instant>,
    last_empty_escape_at: Option<Instant>,
    active_prompt: Option<ActivePrompt>,
    active_auth: Option<ActiveAuthFlow>,
    overlay: Option<OverlayState>,
    pending_messages: Vec<QueuedMessage>,
    active_tools: Vec<ActiveToolExecution>,
    cached_messages: Vec<Message>,
    cached_transcript: Vec<TranscriptEntry>,
    transient_transcript: Vec<TranscriptEntry>,
    cached_state: RpcSessionState,
    cached_stats: RpcSessionStats,
    hide_thinking: bool,
    show_images: bool,
    tool_expand_mode: ToolExpandMode,
    double_escape_action: DoubleEscapeAction,
    quiet_startup: bool,
    startup_context_files: Vec<String>,
    startup_resource_summary: StartupResourceSummary,
    startup_notices: Vec<String>,
    show_new_session_banner: bool,
    available_model_count: usize,
    spinner_frame: usize,
    cwd: PathBuf,
    git_branch: Option<String>,
}

impl InteractiveApp {
    fn new(
        session: Arc<Mutex<AgentSession>>,
        control: AgentControl,
        keybindings: KeybindingsManager,
        session_dir_override: Option<PathBuf>,
        terminal_capabilities: TerminalCapabilities,
        cwd: &Path,
    ) -> Result<Self, String> {
        let mut editor = Editor::new();
        editor.set_focused(true);
        editor.set_max_visible_lines(None);
        let initial_state = session
            .lock()
            .map_err(|_| "Failed to lock interactive session".to_string())?
            .get_state();
        let initial_stats = session
            .lock()
            .map_err(|_| "Failed to lock interactive session".to_string())?
            .get_session_stats();
        let package_manager = PackageManager::create(cwd, None);
        let merged_settings = package_manager.settings_manager().merged_settings();
        let hide_thinking = bool_setting(&merged_settings, &["hideThinkingBlock"], false);
        let show_images = bool_setting(&merged_settings, &["terminal", "showImages"], true);
        let quiet_startup = bool_setting(&merged_settings, &["quietStartup"], false)
            || bool_setting(&merged_settings, &["terminal", "quietStartup"], false);
        let double_escape_action = DoubleEscapeAction::from_settings(
            string_setting(&merged_settings, &["doubleEscapeAction"]).as_deref(),
        );
        let steering_mode = queue_mode_setting(&merged_settings, &["steeringMode"]);
        let follow_up_mode = queue_mode_setting(&merged_settings, &["followUpMode"]);
        let auto_compact = bool_setting(&merged_settings, &["compaction", "enabled"], true);
        {
            let mut guard = session
                .lock()
                .map_err(|_| "Failed to lock interactive session".to_string())?;
            guard.set_steering_mode(steering_mode);
            guard.set_follow_up_mode(follow_up_mode);
            guard.set_auto_compaction(auto_compact);
        }
        let mut app = Self {
            session,
            control,
            keybindings,
            package_manager,
            session_dir_override,
            terminal_capabilities,
            editor,
            prompt_autocomplete: None,
            status: None,
            ctrl_c_armed_at: None,
            last_empty_escape_at: None,
            active_prompt: None,
            active_auth: None,
            overlay: None,
            pending_messages: Vec::new(),
            active_tools: Vec::new(),
            cached_messages: Vec::new(),
            cached_transcript: Vec::new(),
            transient_transcript: Vec::new(),
            cached_state: initial_state,
            cached_stats: initial_stats,
            hide_thinking,
            show_images,
            tool_expand_mode: ToolExpandMode::Collapsed,
            double_escape_action,
            quiet_startup,
            startup_context_files: discover_startup_context_files(cwd),
            startup_resource_summary: StartupResourceSummary::default(),
            startup_notices: Vec::new(),
            show_new_session_banner: false,
            available_model_count: 0,
            spinner_frame: 0,
            cwd: cwd.to_path_buf(),
            git_branch: detect_git_branch(cwd),
        };
        app.refresh_snapshot()?;
        app.update_prompt_autocomplete()?;
        Ok(app)
    }

    fn needs_periodic_redraw(&self) -> bool {
        self.active_prompt.is_some() || self.active_auth.is_some()
    }

    fn prompt_text(&self) -> String {
        self.editor.get_text()
    }

    fn prompt_is_empty(&self) -> bool {
        self.prompt_text().trim().is_empty()
    }

    fn set_prompt_text(&mut self, text: impl AsRef<str>) -> Result<(), String> {
        self.editor.set_text(text.as_ref());
        self.update_prompt_autocomplete()
    }

    fn clear_prompt(&mut self) -> Result<(), String> {
        self.editor.clear();
        self.update_prompt_autocomplete()
    }

    fn poll_background(&mut self) -> Result<(), String> {
        if self.active_prompt.is_none() {
            return self.poll_auth_background();
        }

        let mut pending_events = Vec::new();
        loop {
            match self
                .active_prompt
                .as_mut()
                .expect("active prompt")
                .event_rx
                .try_recv()
            {
                Ok(event) => pending_events.push(event),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        for event in pending_events {
            self.handle_agent_event(event);
        }

        let completion_state = {
            let active = self.active_prompt.as_mut().expect("active prompt");
            if active.completion_result.is_some() {
                if active.linger_after_completion {
                    active.linger_after_completion = false;
                    Some(false)
                } else {
                    Some(true)
                }
            } else {
                None
            }
        };
        if let Some(should_finalize_completion) = completion_state {
            if should_finalize_completion {
                let result = self
                    .active_prompt
                    .as_mut()
                    .expect("active prompt")
                    .completion_result
                    .take()
                    .expect("active prompt completion");
                return self.finish_active_prompt_completion(result);
            }
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
            return self.poll_auth_background();
        }

        match self
            .active_prompt
            .as_mut()
            .expect("active prompt")
            .result_rx
            .try_recv()
        {
            Ok(result) => {
                let mut pending_events = Vec::new();
                while let Ok(event) = self
                    .active_prompt
                    .as_mut()
                    .expect("active prompt")
                    .event_rx
                    .try_recv()
                {
                    pending_events.push(event);
                }
                for event in pending_events {
                    self.handle_agent_event(event);
                }
                let active = self.active_prompt.as_mut().expect("active prompt");
                if let Some(handle) = active.handle.take() {
                    let _ = handle.join();
                }
                active.completion_result = Some(result);
                active.linger_after_completion = true;
                self.spinner_frame = self.spinner_frame.wrapping_add(1);
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.spinner_frame = self.spinner_frame.wrapping_add(1);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let active = self.active_prompt.take().expect("active prompt");
                if let Some(handle) = active.handle {
                    let _ = handle.join();
                }
                self.status = Some("Prompt worker disconnected unexpectedly.".to_string());
                self.pending_messages.clear();
                self.active_tools.clear();
                self.refresh_snapshot()?;
            }
        }

        self.poll_auth_background()
    }

    fn finish_active_prompt_completion(
        &mut self,
        result: Result<PromptRun, String>,
    ) -> Result<(), String> {
        let active = self.active_prompt.take().expect("active prompt");
        match result {
            Ok(run) => {
                let aborted =
                    active.aborted || run.assistant_message.stop_reason == StopReason::Aborted;
                self.refresh_snapshot()?;
                self.active_tools.clear();
                if aborted {
                    let restored = self.restore_pending_messages();
                    self.status = Some(if restored > 0 {
                        format!("Request aborted. Restored {restored} queued message(s).")
                    } else {
                        "Request aborted.".to_string()
                    });
                } else {
                    self.pending_messages.clear();
                    self.status = None;
                }
            }
            Err(error) => {
                let restored = self.restore_pending_messages();
                self.refresh_snapshot()?;
                self.active_tools.clear();
                self.status = Some(if restored > 0 {
                    format!("{error} Restored {restored} queued message(s).")
                } else {
                    error
                });
            }
        }
        Ok(())
    }

    fn poll_auth_background(&mut self) -> Result<(), String> {
        if self.active_auth.is_none() {
            return Ok(());
        }

        let mut requests = Vec::new();
        loop {
            match self
                .active_auth
                .as_mut()
                .expect("active auth")
                .ui_rx
                .try_recv()
            {
                Ok(request) => requests.push(request),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        for request in requests {
            self.handle_auth_ui_request(request);
        }

        match self
            .active_auth
            .as_mut()
            .expect("active auth")
            .result_rx
            .try_recv()
        {
            Ok(result) => {
                let mut requests = Vec::new();
                while let Ok(request) = self
                    .active_auth
                    .as_mut()
                    .expect("active auth")
                    .ui_rx
                    .try_recv()
                {
                    requests.push(request);
                }
                for request in requests {
                    self.handle_auth_ui_request(request);
                }
                let active = self.active_auth.take().expect("active auth");
                let _ = active.handle.join();
                match result {
                    Ok(credentials) => {
                        let provider = active.provider.clone();
                        self.with_session_mut(|session| {
                            let registry = session.model_registry_mut();
                            registry
                                .auth_storage_mut()
                                .set(&provider, AuthCredential::OAuth(credentials.clone()))
                                .map_err(|error| error.to_string())?;
                            registry.refresh();
                            Ok(())
                        })?;
                        self.refresh_snapshot()?;
                        self.overlay = None;
                        self.status = Some(format!(
                            "Logged in to {}. Credentials saved to auth.json.",
                            oauth_provider_label(&provider)
                        ));
                    }
                    Err(error) => {
                        self.overlay = None;
                        self.status = Some(error);
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.spinner_frame = self.spinner_frame.wrapping_add(1);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let active = self.active_auth.take().expect("active auth");
                let _ = active.handle.join();
                self.overlay = None;
                self.status = Some("Login worker disconnected unexpectedly.".to_string());
            }
        }

        Ok(())
    }

    fn render(&self, width: u16, height: u16) -> RenderOutput {
        let transcript_entries = self.combined_transcript();
        let overlay_presentation = self.overlay.as_ref().map(overlay_presentation);
        let show_startup_stack = should_show_startup_stack(
            transcript_entries.is_empty(),
            overlay_presentation,
            self.active_prompt.is_some(),
            self.active_auth.is_some(),
            self.show_new_session_banner,
        );
        let mut header_lines = Vec::new();
        if show_startup_stack {
            header_lines.push(style_brand(&format!("pi v{}", env!("CARGO_PKG_VERSION"))));
            if !self.quiet_startup {
                header_lines.push(String::new());
                header_lines.extend(startup_hint_lines(&self.keybindings));
            }
            let startup_notice_lines = startup_notice_lines(
                self.cached_state.model.is_none() || self.available_model_count == 0,
                &self.startup_notices,
                width as usize,
            );
            if !startup_notice_lines.is_empty() {
                if !header_lines.last().is_some_and(|line| line.is_empty()) {
                    header_lines.push(String::new());
                }
                header_lines.extend(startup_notice_lines);
            }
            let startup_resource_lines = startup_resource_lines(
                &self.startup_context_files,
                &self.startup_resource_summary,
                width as usize,
            );
            if !startup_resource_lines.is_empty() {
                if !header_lines.last().is_some_and(|line| line.is_empty()) {
                    header_lines.push(String::new());
                }
                header_lines.extend(startup_resource_lines);
            }
        }
        let latest_tool_panel = latest_active_tool_panel_id(&self.active_tools)
            .or_else(|| latest_transcript_tool_panel_id(&transcript_entries));
        let mut transcript_lines = session_transcript_lines(
            &transcript_entries,
            width,
            self.hide_thinking,
            self.show_images,
            &self.terminal_capabilities,
            self.tool_expand_mode,
            latest_tool_panel.as_deref(),
            &self.keybindings.display(AppAction::ExpandTools),
        );
        if self.show_new_session_banner {
            let mut banner_lines = vec![
                RenderedLine::Text(style_success("✓ New session started")),
                RenderedLine::Text(String::new()),
            ];
            banner_lines.extend(transcript_lines);
            transcript_lines = banner_lines;
        }
        if let Some(active) = &self.active_prompt {
            if !transcript_lines.is_empty() {
                transcript_lines.push(RenderedLine::Text(String::new()));
            }
            transcript_lines.extend(active_prompt_transcript_lines(
                active,
                width as usize,
                self.spinner_frame,
            ));
        }
        let tool_lines = active_tool_render_lines(
            &self.active_tools,
            width,
            self.show_images,
            &self.terminal_capabilities,
            self.tool_expand_mode,
            latest_tool_panel.as_deref(),
            &self.keybindings.display(AppAction::ExpandTools),
        );
        let footer = render_footer_panel(
            &self.cached_state,
            &self.cached_stats,
            &self.cwd,
            self.git_branch.as_deref(),
            width,
            self.active_prompt.is_some(),
            self.pending_messages.len(),
            self.tool_expand_mode,
        );
        let status_lines = self.render_status_lines(width);
        let pending_lines = pending_message_lines(&self.pending_messages, width);
        let mut header = Text::new(header_lines.join("\n")).render(width);
        let pending =
            (!pending_lines.is_empty()).then(|| Text::new(pending_lines.join("\n")).render(width));
        let overlay = self.overlay.as_ref().map(|overlay| match overlay {
            OverlayState::Model(overlay) => overlay.render(width),
            OverlayState::ScopedModels(overlay) => overlay.render(width),
            OverlayState::Settings(overlay) => overlay.render(width),
            OverlayState::Fork(overlay) => overlay.render(width),
            OverlayState::TreeSummary(overlay) => overlay.render(width),
            OverlayState::Search {
                kind,
                overlay,
                tree_filter,
                ..
            } => render_search_overlay_shell(*kind, overlay, *tree_filter, width),
            OverlayState::Session(overlay) => overlay.render(width),
            OverlayState::Input(overlay) => overlay.render(width),
            OverlayState::Auth(overlay) => overlay.render(width),
        });
        let status =
            (!status_lines.is_empty()).then(|| Text::new(status_lines.join("\n")).render(width));
        let prompt_max_visible_lines = Some(composer_max_visible_lines(height));
        let prompt = if self.overlay.is_none() {
            Some(render_prompt_panel(
                &self.editor,
                self.prompt_autocomplete.as_ref(),
                &self.cached_state,
                &self.keybindings,
                width,
                prompt_max_visible_lines,
                self.active_prompt.is_some(),
                self.pending_messages.len(),
            ))
        } else {
            None
        };

        if matches!(overlay_presentation, Some(OverlayPresentation::Standalone))
            && let Some(overlay) = overlay
        {
            return clip_render_output_to_height(overlay, height);
        }

        let mut header_gap = usize::from(!header.lines.is_empty());
        let overlay_is_in_shell =
            matches!(overlay_presentation, Some(OverlayPresentation::InShell));
        if overlay_is_in_shell {
            let mut body = RenderOutput {
                lines: Vec::new(),
                cursor: None,
            };
            append_output(&mut body, header, false);
            if header_gap > 0 {
                append_blank_lines(&mut body, width, header_gap);
            }
            append_output(&mut body, overlay.unwrap_or_default(), true);

            let body_budget = (height as usize).saturating_sub(footer.lines.len());
            let body = clip_render_output_to_height(body, body_budget as u16);
            let body_padding = body_budget.saturating_sub(body.lines.len());

            let mut output = RenderOutput {
                lines: Vec::new(),
                cursor: None,
            };
            append_output(&mut output, body, true);
            if body_padding > 0 {
                append_blank_lines(&mut output, width, body_padding);
            }
            append_output(&mut output, footer, false);
            return clip_render_output_to_height(output, height);
        }

        let mut lower_section_lengths = Vec::new();
        if !tool_lines.is_empty() {
            lower_section_lengths.push(tool_lines.len());
        }
        if let Some(pending) = &pending {
            lower_section_lengths.push(pending.lines.len());
        }
        if let Some(status) = &status {
            lower_section_lengths.push(status.lines.len());
        }
        let lower_reserved = lower_section_lengths.iter().sum::<usize>()
            + lower_section_lengths.len().saturating_sub(1)
            + usize::from(!lower_section_lengths.is_empty());
        let has_content_before_prompt = !header.lines.is_empty()
            || !transcript_lines.is_empty()
            || !tool_lines.is_empty()
            || pending.is_some()
            || status.is_some();
        let prompt_separator = usize::from(prompt.is_some() && has_content_before_prompt);
        let reserved = header.lines.len()
            + header_gap
            + lower_reserved
            + footer.lines.len()
            + prompt
                .as_ref()
                .map_or(0, |prompt| prompt.lines.len() + prompt_separator);

        if self.active_prompt.is_some() && reserved > height as usize {
            let minimum_middle_lines = if self.show_new_session_banner {
                10usize
            } else {
                6usize
            };
            let header_budget = (height as usize).saturating_sub(
                lower_reserved
                    + footer.lines.len()
                    + prompt
                        .as_ref()
                        .map_or(0, |prompt| prompt.lines.len() + prompt_separator)
                    + minimum_middle_lines
                    + header_gap,
            );
            if header.lines.len() > header_budget {
                header = clip_render_output_to_height(header, header_budget as u16);
                header_gap = usize::from(!header.lines.is_empty());
            }
        }

        let reserved = header.lines.len()
            + header_gap
            + lower_reserved
            + footer.lines.len()
            + prompt
                .as_ref()
                .map_or(0, |prompt| prompt.lines.len() + prompt_separator);

        let middle_budget = (height as usize).saturating_sub(reserved);
        let middle_output = {
            let visible_transcript = if transcript_lines.len() > middle_budget {
                transcript_lines[transcript_lines.len() - middle_budget..].to_vec()
            } else {
                transcript_lines
            };
            RenderOutput {
                lines: visible_transcript,
                cursor: None,
            }
        };
        let middle_padding = middle_budget.saturating_sub(middle_output.lines.len());

        let mut output = RenderOutput {
            lines: Vec::new(),
            cursor: None,
        };
        append_output(&mut output, header, false);
        if header_gap > 0 {
            append_blank_lines(&mut output, width, header_gap);
        }
        append_output(&mut output, middle_output, overlay_is_in_shell);
        if middle_padding > 0 {
            append_blank_lines(&mut output, width, middle_padding);
        }
        if !overlay_is_in_shell {
            if !tool_lines.is_empty() {
                if !output.lines.is_empty() {
                    append_blank_lines(&mut output, width, 1);
                }
                output.lines.extend(tool_lines);
            }
            if let Some(pending) = pending {
                append_blank_lines(&mut output, width, 1);
                append_output(&mut output, pending, false);
            }
            if let Some(status) = status {
                append_blank_lines(&mut output, width, 1);
                append_output(&mut output, status, false);
            }
            if let Some(prompt) = prompt {
                if !output.lines.is_empty() {
                    append_blank_lines(&mut output, width, 1);
                }
                append_output(&mut output, prompt, true);
            }
        }
        append_output(&mut output, footer, false);
        clip_render_output_to_height(output, height)
    }

    fn render_status_lines(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(message) = &self.status {
            lines.extend(
                wrap_text(message, width as usize)
                    .into_iter()
                    .map(|line| style_subtitle(&line)),
            );
        }
        if let Some(active) = &self.active_prompt {
            let elapsed = active.started_at.elapsed().as_secs();
            lines.push(truncate_to_width(
                &style_warning(&format!(
                    "| Working for {elapsed}s - Esc aborts - Enter queues steer - alt+enter queues follow-up"
                )),
                width as usize,
            ));
        }
        if let Some(active) = &self.active_auth {
            let spinner = ["-", "\\", "|", "/"][self.spinner_frame % 4];
            let elapsed = active.started_at.elapsed().as_secs();
            lines.push(truncate_to_width(
                &style_warning(&format!(
                    "{spinner} Authenticating with {} for {elapsed}s - Esc cancels",
                    oauth_provider_label(&active.provider)
                )),
                width as usize,
            ));
        }
        lines
    }

    fn handle_auth_ui_request(&mut self, request: AuthUiRequest) {
        let Some(OverlayState::Auth(state)) = self.overlay.as_mut() else {
            return;
        };
        match request {
            AuthUiRequest::ShowAuth(info) => state.set_auth_info(info),
            AuthUiRequest::Prompt { prompt, kind } => state.set_prompt(prompt, kind),
            AuthUiRequest::Progress(message) => state.push_progress(message),
        }
    }

    fn handle_key(&mut self, event: KeyEvent) -> Result<LoopAction, String> {
        if matches!(self.overlay, Some(OverlayState::ScopedModels(_)))
            && let Some(mut overlay) = self.overlay.take()
        {
            let (outcome, action) = self.handle_overlay_key(&mut overlay, event)?;
            if matches!(outcome, OverlayOutcome::KeepOpen) && self.overlay.is_none() {
                self.overlay = Some(overlay);
            }
            return Ok(action);
        }

        match self.handle_global_keybinding(&event)? {
            GlobalKeyAction::None => {}
            GlobalKeyAction::Continue => return Ok(LoopAction::Continue),
            GlobalKeyAction::Suspend => return Ok(LoopAction::Suspend),
            GlobalKeyAction::Quit => return Ok(LoopAction::Quit),
        }

        if let Some(mut overlay) = self.overlay.take() {
            let (outcome, action) = self.handle_overlay_key(&mut overlay, event)?;
            if matches!(outcome, OverlayOutcome::KeepOpen) && self.overlay.is_none() {
                self.overlay = Some(overlay);
            }
            return Ok(action);
        }

        if self.active_prompt.is_some() {
            return self.handle_active_prompt_key(event);
        }

        if matches!(event.code, KeyCode::Escape)
            && event.modifiers == KeyModifiers::NONE
            && self.prompt_autocomplete.is_none()
            && self.prompt_is_empty()
        {
            let now = Instant::now();
            if self
                .last_empty_escape_at
                .is_some_and(|instant| now.duration_since(instant) <= Duration::from_millis(500))
            {
                self.last_empty_escape_at = None;
                match self.double_escape_action {
                    DoubleEscapeAction::Tree => self.open_tree_overlay(None)?,
                    DoubleEscapeAction::Fork => self.open_fork_overlay(None)?,
                    DoubleEscapeAction::None => {
                        self.status = Some("Double-escape action is disabled.".to_string());
                    }
                }
                return Ok(LoopAction::Continue);
            }
            self.last_empty_escape_at = Some(now);
            self.status = Some(format!(
                "Press Esc again to open {}.",
                self.double_escape_action.as_str()
            ));
            return Ok(LoopAction::Continue);
        }
        self.last_empty_escape_at = None;
        self.handle_prompt_key(event, false)
    }

    fn handle_global_keybinding(&mut self, event: &KeyEvent) -> Result<GlobalKeyAction, String> {
        if self.keybindings.matches(event, AppAction::Clear) {
            if self.active_auth.is_some() {
                self.cancel_auth_flow();
                self.status = Some("Cancelling login...".to_string());
                return Ok(GlobalKeyAction::Continue);
            }
            if self.active_prompt.is_some() {
                self.control.abort();
                if let Some(active) = &mut self.active_prompt {
                    active.aborted = true;
                }
                self.status = Some("Abort requested.".to_string());
                return Ok(GlobalKeyAction::Continue);
            }
            if !self.prompt_text().is_empty() {
                self.clear_prompt()?;
                self.status = Some("Cleared input.".to_string());
                self.ctrl_c_armed_at = None;
                return Ok(GlobalKeyAction::Continue);
            }
            let now = Instant::now();
            if self
                .ctrl_c_armed_at
                .is_some_and(|instant| now.duration_since(instant) <= Duration::from_secs(1))
            {
                return Ok(GlobalKeyAction::Quit);
            }
            self.ctrl_c_armed_at = Some(now);
            self.status = Some("Press Ctrl+C again to exit.".to_string());
            return Ok(GlobalKeyAction::Continue);
        }

        self.ctrl_c_armed_at = None;

        if self.keybindings.matches(event, AppAction::Suspend) {
            return Ok(GlobalKeyAction::Suspend);
        }

        if self.keybindings.matches(event, AppAction::Exit) && self.prompt_is_empty() {
            if self.active_auth.is_some() {
                self.cancel_auth_flow();
                self.status = Some("Cancelling login...".to_string());
                return Ok(GlobalKeyAction::Continue);
            }
            return Ok(GlobalKeyAction::Quit);
        }
        if self.keybindings.matches(event, AppAction::Interrupt)
            && (self.active_prompt.is_some() || self.active_auth.is_some())
        {
            if self.active_auth.is_some() {
                self.cancel_auth_flow();
                self.status = Some("Cancelling login...".to_string());
            } else {
                self.control.abort();
                if let Some(active) = &mut self.active_prompt {
                    active.aborted = true;
                }
                self.status = Some("Abort requested.".to_string());
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::SelectModel) {
            if self.active_prompt.is_some() || self.active_auth.is_some() {
                self.status = Some(
                    "Wait for the current operation or press Esc to cancel first.".to_string(),
                );
            } else {
                self.open_model_overlay(None)?;
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self
            .keybindings
            .matches(event, AppAction::CycleModelForward)
        {
            if self.active_prompt.is_some() || self.active_auth.is_some() {
                self.status =
                    Some("Model switching is unavailable while the agent is working.".to_string());
            } else {
                let result = self.with_session_mut(|session| {
                    session.cycle_model().map_err(|error| error.to_string())
                })?;
                self.refresh_snapshot()?;
                self.status = Some(match result {
                    Some(result) => {
                        format!(
                            "Switched to {}/{}",
                            result.model.provider.0, result.model.id
                        )
                    }
                    None => "No models available to cycle.".to_string(),
                });
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self
            .keybindings
            .matches(event, AppAction::CycleModelBackward)
        {
            if self.active_prompt.is_some() || self.active_auth.is_some() {
                self.status =
                    Some("Model switching is unavailable while the agent is working.".to_string());
            } else {
                let result = self.with_session_mut(|session| {
                    session
                        .cycle_model_backward()
                        .map_err(|error| error.to_string())
                })?;
                self.refresh_snapshot()?;
                self.status = Some(match result {
                    Some(result) => {
                        format!(
                            "Switched to {}/{}",
                            result.model.provider.0, result.model.id
                        )
                    }
                    None => "No models available to cycle.".to_string(),
                });
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self
            .keybindings
            .matches(event, AppAction::CycleThinkingLevel)
        {
            if self.active_prompt.is_some() || self.active_auth.is_some() {
                self.status = Some(
                    "Thinking level changes are unavailable while the agent is working."
                        .to_string(),
                );
            } else {
                match self.with_session_mut(|session| {
                    session
                        .cycle_thinking_level()
                        .map_err(|error| error.to_string())
                }) {
                    Ok(result) => {
                        self.refresh_snapshot()?;
                        self.status = Some(match result {
                            Some(level) => format!("Thinking level: {level}"),
                            None => "Current model does not support thinking".to_string(),
                        });
                    }
                    Err(error) => {
                        self.status = Some(if error.contains("does not support") {
                            "Current model does not support thinking".to_string()
                        } else {
                            error
                        });
                    }
                }
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::ToggleThinking) {
            self.hide_thinking = !self.hide_thinking;
            self.status = Some(if self.hide_thinking {
                "Thinking blocks hidden.".to_string()
            } else {
                "Thinking blocks visible.".to_string()
            });
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::ExpandTools) {
            self.tool_expand_mode = self.tool_expand_mode.next();
            self.status = Some(self.tool_expand_mode.status().to_string());
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::NewSession) {
            if self.active_prompt.is_some() || self.active_auth.is_some() {
                self.status = Some(
                    "Wait for the current operation or press Esc to cancel first.".to_string(),
                );
            } else {
                let _ = self.with_session_mut(|session| {
                    session.new_session(None).map_err(|error| error.to_string())
                })?;
                self.clear_transient_entries();
                self.pending_messages.clear();
                self.refresh_snapshot()?;
                self.status = Some("Started a new session.".to_string());
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::Resume) {
            if self.active_prompt.is_some() || self.active_auth.is_some() {
                self.status = Some(
                    "Wait for the current operation or press Esc to cancel first.".to_string(),
                );
            } else {
                self.open_session_overlay(None)?;
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::Tree) {
            if self.active_prompt.is_some() || self.active_auth.is_some() {
                self.status = Some(
                    "Wait for the current operation or press Esc to cancel first.".to_string(),
                );
            } else {
                self.open_tree_overlay(None)?;
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::Fork) {
            if self.active_prompt.is_some() || self.active_auth.is_some() {
                self.status = Some(
                    "Wait for the current operation or press Esc to cancel first.".to_string(),
                );
            } else {
                self.open_fork_overlay(None)?;
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::ExternalEditor) {
            if self.active_prompt.is_some() || self.active_auth.is_some() {
                self.status = Some(
                    "Wait for the current operation or press Esc to cancel first.".to_string(),
                );
            } else if self.overlay.is_some() {
                self.status =
                    Some("Close the current selector before opening the editor.".to_string());
            } else {
                return Ok(GlobalKeyAction::None);
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::PasteImage) {
            if self.overlay.is_none()
                && self.active_prompt.is_none()
                && self.active_auth.is_none()
                && let Ok(Some(path)) = paste_clipboard_image_to_temp_file()
            {
                self.editor.handle_key(&KeyEvent::new(KeyCode::Paste(
                    path.to_string_lossy().to_string(),
                )));
                self.update_prompt_autocomplete()?;
                self.status = None;
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::Dequeue) && self.active_prompt.is_some() {
            if self.dequeue_last_pending() {
                self.status = Some("Restored the most recent queued message.".to_string());
            } else {
                self.status = Some("No queued messages to restore.".to_string());
            }
            return Ok(GlobalKeyAction::Continue);
        }
        if self.keybindings.matches(event, AppAction::FollowUp) && self.active_prompt.is_some() {
            let value = self.prompt_text().trim().to_string();
            if !value.is_empty() {
                self.clear_prompt()?;
                self.queue_message(QueuedMessageKind::FollowUp, value);
            }
            return Ok(GlobalKeyAction::Continue);
        }

        Ok(GlobalKeyAction::None)
    }

    fn handle_overlay_key(
        &mut self,
        overlay: &mut OverlayState,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        if let OverlayState::Session(state) = overlay {
            if self.keybindings.matches(&event, AppAction::RenameSession) {
                if let Some(selected_value) = state.overlay.selected_value().map(ToOwned::to_owned)
                {
                    *overlay = OverlayState::Input(
                        self.build_session_rename_input_overlay(state, &selected_value)?,
                    );
                    return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
                }
            }
        }
        if let OverlayState::Search {
            kind,
            overlay: search_overlay,
            selection,
            tree_filter,
        } = overlay
        {
            if *kind == SearchOverlayKind::Tree
                && self.keybindings.matches(&event, AppAction::EditTreeLabel)
            {
                if let Some(selected_value) = search_overlay.selected_value().map(ToOwned::to_owned)
                {
                    *overlay = OverlayState::Input(self.build_tree_label_input_overlay(
                        selection,
                        tree_filter.unwrap_or(TreeFilterMode::Default),
                        search_overlay.search.get_value(),
                        &selected_value,
                    )?);
                    return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
                }
            }
        }
        match overlay {
            OverlayState::Model(state) => self.handle_model_overlay_key(state, event),
            OverlayState::ScopedModels(state) => {
                self.handle_scoped_models_overlay_key(state, event)
            }
            OverlayState::Settings(state) => self.handle_settings_overlay_key(state, event),
            OverlayState::Fork(state) => self.handle_fork_overlay_key(state, event),
            OverlayState::TreeSummary(state) => self.handle_tree_summary_overlay_key(state, event),
            OverlayState::Search {
                kind,
                overlay,
                selection,
                tree_filter,
            } => self.handle_search_overlay_key(kind, overlay, selection, tree_filter, event),
            OverlayState::Session(state) => self.handle_session_overlay_key(state, event),
            OverlayState::Input(state) => self.handle_input_overlay_key(state, event),
            OverlayState::Auth(state) => self.handle_auth_overlay_key(state, event),
        }
    }

    fn handle_search_overlay_key(
        &mut self,
        kind: &mut SearchOverlayKind,
        overlay: &mut SearchOverlay,
        selection: &mut Vec<OverlaySelection>,
        tree_filter: &mut Option<TreeFilterMode>,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        if *kind == SearchOverlayKind::Tree
            && matches!(event.code, KeyCode::Tab)
            && event.modifiers == KeyModifiers::NONE
        {
            let next = tree_filter.unwrap_or(TreeFilterMode::Default).next();
            *tree_filter = Some(next);
            let (items, selections) = self.build_tree_overlay_items(next)?;
            let selected_value = overlay.selected_value().map(ToOwned::to_owned);
            overlay.replace_items_preserving_selection(items, selected_value.as_deref());
            overlay.set_subtitle(
                "↑/↓: move. ←/→: page. Shift+L: label. ^D/^T/^U/^L/^A: filters (^O/⇧^O cycle)",
            );
            overlay.set_hint("Enter navigates · Esc cancels");
            overlay.set_detail(Some(style_hint(&format!("[{}]", next.label()))));
            *selection = selections;
            self.status = Some(format!("Tree filter: {}", next.label()));
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }

        let selected = match overlay.handle_key(&event) {
            SearchOverlayEvent::Continue => {
                return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
            }
            SearchOverlayEvent::Cancelled => {
                self.status = Some("Selection cancelled.".to_string());
                return Ok((OverlayOutcome::Close, LoopAction::Continue));
            }
            SearchOverlayEvent::Selected(item) => selection
                .iter()
                .find(|candidate| match candidate {
                    OverlaySelection::Model { provider, model_id } => {
                        item.value == format!("{provider}/{model_id}")
                    }
                    OverlaySelection::Session { path } => item.value == path.to_string_lossy(),
                    OverlaySelection::Fork { entry_id } => item.value == *entry_id,
                    OverlaySelection::Tree { entry_id, .. } => item.value == *entry_id,
                    OverlaySelection::AuthProvider { provider } => item.value == *provider,
                })
                .cloned(),
        };

        let outcome = match selected {
            Some(OverlaySelection::Model { provider, model_id }) => {
                let model = self.with_session_mut(|session| {
                    session
                        .set_model(&provider, &model_id)
                        .map_err(|error| error.to_string())
                })?;
                self.refresh_snapshot()?;
                self.status = Some(format!("Switched to {}/{}", model.provider.0, model.id));
                OverlayOutcome::Close
            }
            Some(OverlaySelection::Fork { entry_id }) => {
                let (selected_text, _cancelled) = self.with_session_mut(|session| {
                    session.fork(&entry_id).map_err(|error| error.to_string())
                })?;
                self.refresh_snapshot()?;
                self.pending_messages.clear();
                self.active_tools.clear();
                self.set_prompt_text(selected_text)?;
                self.status = Some("Branched to new session".to_string());
                OverlayOutcome::Close
            }
            Some(OverlaySelection::Tree { entry_id, .. }) => {
                let current_leaf = self.with_session(|session| session.get_leaf_id())?;
                if current_leaf.as_deref() == Some(entry_id.as_str()) {
                    self.status = Some("Already at this point".to_string());
                    OverlayOutcome::Close
                } else {
                    self.overlay = Some(OverlayState::TreeSummary(
                        self.build_tree_summary_overlay_state(
                            &entry_id,
                            tree_filter.unwrap_or(TreeFilterMode::Default),
                            overlay.search.get_value(),
                        )?,
                    ));
                    return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
                }
            }
            Some(OverlaySelection::AuthProvider { provider }) => match kind {
                SearchOverlayKind::OAuthLogin => {
                    self.start_oauth_login(&provider)?;
                    OverlayOutcome::KeepOpen
                }
                SearchOverlayKind::OAuthLogout => {
                    let removed = self.with_session_mut(|session| {
                        let registry = session.model_registry_mut();
                        let removed = registry
                            .auth_storage_mut()
                            .logout(&provider)
                            .map_err(|error| error.to_string())?;
                        registry.refresh();
                        Ok(removed)
                    })?;
                    self.refresh_snapshot()?;
                    self.status = Some(if removed {
                        format!("Logged out of {}", oauth_provider_label(&provider))
                    } else {
                        format!(
                            "{} was already logged out.",
                            oauth_provider_label(&provider)
                        )
                    });
                    OverlayOutcome::Close
                }
                _ => {
                    self.status = Some("Invalid OAuth selection.".to_string());
                    OverlayOutcome::Close
                }
            },
            Some(OverlaySelection::Session { .. }) | None => {
                self.status = Some("Invalid selection.".to_string());
                OverlayOutcome::Close
            }
        };

        Ok((outcome, LoopAction::Continue))
    }

    fn handle_model_overlay_key(
        &mut self,
        state: &mut ModelOverlayState,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        if matches!(event.code, KeyCode::Tab) && event.modifiers == KeyModifiers::NONE {
            self.toggle_model_overlay_scope(state)?;
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }

        match event.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Escape => {
                match state.overlay.list.handle_key(&event) {
                    SelectEvent::Changed => self.update_model_overlay_metadata(state),
                    SelectEvent::None => {}
                    SelectEvent::Cancelled => {
                        self.status = Some("Selection cancelled.".to_string());
                        return Ok((OverlayOutcome::Close, LoopAction::Continue));
                    }
                    SelectEvent::Selected(item) => {
                        let Some(OverlaySelection::Model { provider, model_id }) = state
                            .selections
                            .iter()
                            .find(|candidate| match candidate {
                                OverlaySelection::Model { provider, model_id } => {
                                    item.value == format!("{provider}/{model_id}")
                                }
                                _ => false,
                            })
                            .cloned()
                        else {
                            self.status = Some("Invalid selection.".to_string());
                            return Ok((OverlayOutcome::Close, LoopAction::Continue));
                        };
                        let model = self.with_session_mut(|session| {
                            session
                                .set_model(&provider, &model_id)
                                .map_err(|error| error.to_string())
                        })?;
                        self.refresh_snapshot()?;
                        self.status =
                            Some(format!("Switched to {}/{}", model.provider.0, model.id));
                        return Ok((OverlayOutcome::Close, LoopAction::Continue));
                    }
                }
            }
            _ => match state.overlay.search.handle_key(&event) {
                InputEvent::Changed => {
                    let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
                    self.reload_model_overlay(state, selected_value.as_deref())?;
                }
                InputEvent::Cancelled => {
                    self.status = Some("Selection cancelled.".to_string());
                    return Ok((OverlayOutcome::Close, LoopAction::Continue));
                }
                InputEvent::Submitted(_) => {
                    if let Some(item) = state.overlay.list.selected_item().cloned() {
                        let Some(OverlaySelection::Model { provider, model_id }) = state
                            .selections
                            .iter()
                            .find(|candidate| match candidate {
                                OverlaySelection::Model { provider, model_id } => {
                                    item.value == format!("{provider}/{model_id}")
                                }
                                _ => false,
                            })
                            .cloned()
                        else {
                            self.status = Some("Invalid selection.".to_string());
                            return Ok((OverlayOutcome::Close, LoopAction::Continue));
                        };
                        let model = self.with_session_mut(|session| {
                            session
                                .set_model(&provider, &model_id)
                                .map_err(|error| error.to_string())
                        })?;
                        self.refresh_snapshot()?;
                        self.status =
                            Some(format!("Switched to {}/{}", model.provider.0, model.id));
                        return Ok((OverlayOutcome::Close, LoopAction::Continue));
                    }
                }
                InputEvent::None => {}
            },
        }
        Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
    }

    fn handle_scoped_models_overlay_key(
        &mut self,
        state: &mut ScopedModelsOverlayState,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        if matches_ctrl_char(&event, 'a') {
            state.enabled_ids = None;
            state.dirty = true;
            self.sync_scoped_models_overlay_to_session(state)?;
            self.reload_scoped_models_overlay(
                state,
                state
                    .overlay
                    .selected_value()
                    .map(ToOwned::to_owned)
                    .as_deref(),
            )?;
            self.status = Some("Enabled all models for this session.".to_string());
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }
        if matches_ctrl_char(&event, 'x') {
            state.enabled_ids = Some(Vec::new());
            state.dirty = true;
            self.sync_scoped_models_overlay_to_session(state)?;
            self.reload_scoped_models_overlay(
                state,
                state
                    .overlay
                    .selected_value()
                    .map(ToOwned::to_owned)
                    .as_deref(),
            )?;
            self.status = Some("Cleared all scoped models for this session.".to_string());
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }
        if matches_ctrl_char(&event, 'p') {
            if let Some(selected_value) = state.overlay.selected_value().map(ToOwned::to_owned) {
                toggle_scoped_models_provider(state, &selected_value);
                state.dirty = true;
                self.sync_scoped_models_overlay_to_session(state)?;
                self.reload_scoped_models_overlay(state, Some(&selected_value))?;
                self.status = Some("Toggled provider models.".to_string());
            }
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }
        if matches_ctrl_char(&event, 's') {
            match state.enabled_ids.as_ref() {
                Some(_) => {
                    self.sync_scoped_models_overlay_to_session(state)?;
                    let saved = self.with_session_mut(|session| {
                        session
                            .save_current_scoped_models()
                            .map_err(|error| error.to_string())
                    })?;
                    state.dirty = false;
                    self.update_scoped_models_overlay_metadata(state);
                    self.status = Some(format!("Saved {} scoped model pattern(s).", saved.len()));
                }
                None => {
                    self.with_session_mut(|session| {
                        session
                            .clear_persisted_enabled_models()
                            .map_err(|error| error.to_string())
                    })?;
                    state.dirty = false;
                    self.update_scoped_models_overlay_metadata(state);
                    self.status = Some(
                        "Cleared persisted scoped model filter. All models enabled.".to_string(),
                    );
                }
            }
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }
        if matches_alt_key(&event, KeyCode::Up) || matches_alt_key(&event, KeyCode::Down) {
            if let Some(selected_value) = state.overlay.selected_value().map(ToOwned::to_owned) {
                let delta = if matches_alt_key(&event, KeyCode::Up) {
                    -1
                } else {
                    1
                };
                if move_scoped_model_selection(state, &selected_value, delta) {
                    state.dirty = true;
                    self.sync_scoped_models_overlay_to_session(state)?;
                    self.reload_scoped_models_overlay(state, Some(&selected_value))?;
                    self.status = Some("Reordered scoped models.".to_string());
                }
            }
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }

        match event.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Escape => {
                match state.overlay.list.handle_key(&event) {
                    SelectEvent::Changed => self.update_scoped_models_overlay_metadata(state),
                    SelectEvent::None => {}
                    SelectEvent::Cancelled => {
                        self.status = Some("Selection cancelled.".to_string());
                        return Ok((OverlayOutcome::Close, LoopAction::Continue));
                    }
                    SelectEvent::Selected(item) => {
                        toggle_scoped_model(state, &item.value);
                        state.dirty = true;
                        self.sync_scoped_models_overlay_to_session(state)?;
                        self.reload_scoped_models_overlay(state, Some(&item.value))?;
                        self.status = Some(format!("Toggled {}.", item.value));
                    }
                }
            }
            _ => match state.overlay.search.handle_key(&event) {
                InputEvent::Changed => {
                    state
                        .overlay
                        .list
                        .set_filter(state.overlay.search.get_value());
                    self.update_scoped_models_overlay_metadata(state);
                }
                InputEvent::Cancelled => {
                    self.status = Some("Selection cancelled.".to_string());
                    return Ok((OverlayOutcome::Close, LoopAction::Continue));
                }
                InputEvent::Submitted(_) => {
                    if let Some(item) = state.overlay.list.selected_item().cloned() {
                        toggle_scoped_model(state, &item.value);
                        state.dirty = true;
                        self.sync_scoped_models_overlay_to_session(state)?;
                        self.reload_scoped_models_overlay(state, Some(&item.value))?;
                        self.status = Some(format!("Toggled {}.", item.value));
                    }
                }
                InputEvent::None => {}
            },
        }

        Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
    }

    fn handle_settings_overlay_key(
        &mut self,
        state: &mut SettingsOverlayState,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        match state.list.handle_key(&event) {
            SettingsListEvent::None => Ok((OverlayOutcome::KeepOpen, LoopAction::Continue)),
            SettingsListEvent::Cancelled => {
                self.status = Some("Settings cancelled.".to_string());
                Ok((OverlayOutcome::Close, LoopAction::Continue))
            }
            SettingsListEvent::Changed { id, value } => {
                self.apply_setting_value(&id, &value)?;
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
        }
    }

    fn handle_fork_overlay_key(
        &mut self,
        state: &mut ForkOverlayState,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        match state.list.handle_key(&event) {
            SelectEvent::Changed | SelectEvent::None => {
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
            SelectEvent::Cancelled => {
                self.status = Some("Fork cancelled.".to_string());
                Ok((OverlayOutcome::Close, LoopAction::Continue))
            }
            SelectEvent::Selected(item) => {
                let Some(OverlaySelection::Fork { entry_id }) = state
                    .selections
                    .iter()
                    .find(|candidate| {
                        matches!(
                            candidate,
                            OverlaySelection::Fork { entry_id } if item.value == *entry_id
                        )
                    })
                    .cloned()
                else {
                    self.status = Some("Invalid selection.".to_string());
                    return Ok((OverlayOutcome::Close, LoopAction::Continue));
                };
                let (selected_text, _cancelled) = self.with_session_mut(|session| {
                    session.fork(&entry_id).map_err(|error| error.to_string())
                })?;
                self.refresh_snapshot()?;
                self.pending_messages.clear();
                self.active_tools.clear();
                self.set_prompt_text(selected_text)?;
                self.status = Some("Branched to new session.".to_string());
                Ok((OverlayOutcome::Close, LoopAction::Continue))
            }
        }
    }

    fn handle_tree_summary_overlay_key(
        &mut self,
        state: &mut TreeSummaryOverlayState,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        match state.list.handle_key(&event) {
            SelectEvent::Changed | SelectEvent::None => {
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
            SelectEvent::Cancelled => {
                self.overlay = Some(self.build_tree_overlay_state(
                    state.filter_mode,
                    Some(state.query.as_str()),
                    Some(state.target_entry_id.as_str()),
                )?);
                self.status = Some("Navigation cancelled".to_string());
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
            SelectEvent::Selected(item) => match item.value.as_str() {
                "no-summary" => {
                    self.navigate_tree_target(&state.target_entry_id, false, None)?;
                    Ok((OverlayOutcome::Close, LoopAction::Continue))
                }
                "summarize" => {
                    self.navigate_tree_target(&state.target_entry_id, true, None)?;
                    Ok((OverlayOutcome::Close, LoopAction::Continue))
                }
                "summarize-custom" => {
                    self.overlay = Some(OverlayState::Input(
                        self.build_tree_summary_custom_prompt_overlay(
                            &state.target_entry_id,
                            state.filter_mode,
                            &state.query,
                        )?,
                    ));
                    Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
                }
                _ => {
                    self.status = Some("Invalid selection".to_string());
                    Ok((OverlayOutcome::Close, LoopAction::Continue))
                }
            },
        }
    }

    fn handle_session_overlay_key(
        &mut self,
        state: &mut SessionOverlayState,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        if let Some(confirming_path) = state.confirming_delete.clone() {
            if matches!(event.code, KeyCode::Escape) || matches_ctrl_char(&event, 'c') {
                state.confirming_delete = None;
                self.update_session_overlay_metadata(state);
                self.status = Some("Session deletion cancelled.".to_string());
                return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
            }
            if matches!(event.code, KeyCode::Enter) {
                if let Some(selected_value) = state.overlay.selected_value() {
                    let selected_path = PathBuf::from(selected_value);
                    if selected_path == confirming_path {
                        let current_session =
                            self.cached_state.session_file.as_ref().map(PathBuf::from);
                        if current_session.as_ref() == Some(&selected_path) {
                            state.confirming_delete = None;
                            self.update_session_overlay_metadata(state);
                            self.status =
                                Some("Cannot delete the currently active session.".to_string());
                            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
                        }
                        fs::remove_file(&selected_path).map_err(|error| error.to_string())?;
                        state.confirming_delete = None;
                        self.reload_session_overlay(state, None)?;
                        self.status = Some(format!("Deleted {}.", selected_path.to_string_lossy()));
                        return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
                    }
                }
            }
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }

        if self
            .keybindings
            .matches(&event, AppAction::ToggleSessionScope)
        {
            state.scope = state.scope.toggle();
            let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
            self.reload_session_overlay(state, selected_value.as_deref())?;
            self.status = Some(format!("Session scope: {}", state.scope.label()));
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }
        if self
            .keybindings
            .matches(&event, AppAction::ToggleSessionNamedFilter)
        {
            state.name_filter = state.name_filter.toggle();
            let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
            self.reload_session_overlay(state, selected_value.as_deref())?;
            self.status = Some(format!(
                "Session name filter: {}",
                state.name_filter.label()
            ));
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }
        if self
            .keybindings
            .matches(&event, AppAction::ToggleSessionSort)
        {
            state.sort_mode = state.sort_mode.next();
            let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
            self.reload_session_overlay(state, selected_value.as_deref())?;
            self.status = Some(format!("Session sort: {}", state.sort_mode.label()));
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }
        if self
            .keybindings
            .matches(&event, AppAction::ToggleSessionPath)
        {
            state.show_path = !state.show_path;
            let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
            self.reload_session_overlay(state, selected_value.as_deref())?;
            self.status = Some(if state.show_path {
                "Session paths visible.".to_string()
            } else {
                "Session paths hidden.".to_string()
            });
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }
        if self.keybindings.matches(&event, AppAction::DeleteSession)
            || (matches!(event.code, KeyCode::Backspace) && event.modifiers == KeyModifiers::NONE)
        {
            if let Some(selected_value) = state.overlay.selected_value() {
                let selected_path = PathBuf::from(selected_value);
                let current_session = self.cached_state.session_file.as_ref().map(PathBuf::from);
                if current_session.as_ref() == Some(&selected_path) {
                    self.status = Some("Cannot delete the currently active session.".to_string());
                } else {
                    state.confirming_delete = Some(selected_path);
                    self.update_session_overlay_metadata(state);
                    self.status = Some(
                        "Press Enter to confirm session deletion, or Esc to cancel.".to_string(),
                    );
                }
            }
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }

        match event.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Escape => {
                match state.overlay.list.handle_key(&event) {
                    SelectEvent::Changed => {
                        self.update_session_overlay_metadata(state);
                    }
                    SelectEvent::None => {}
                    SelectEvent::Cancelled => {
                        self.status = Some("Selection cancelled.".to_string());
                        return Ok((OverlayOutcome::Close, LoopAction::Continue));
                    }
                    SelectEvent::Selected(item) => {
                        let path = PathBuf::from(item.value);
                        let cancelled = self.with_session_mut(|session| {
                            session
                                .switch_session(&path.to_string_lossy())
                                .map_err(|error| error.to_string())
                        })?;
                        self.clear_transient_entries();
                        self.show_new_session_banner = false;
                        self.refresh_snapshot()?;
                        self.pending_messages.clear();
                        self.active_tools.clear();
                        self.status = Some(if cancelled {
                            "Switched sessions after cancelling active work.".to_string()
                        } else {
                            format!("Switched to {}", path.to_string_lossy())
                        });
                        return Ok((OverlayOutcome::Close, LoopAction::Continue));
                    }
                }
            }
            _ => match state.overlay.search.handle_key(&event) {
                InputEvent::Changed => {
                    let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
                    self.reload_session_overlay(state, selected_value.as_deref())?;
                }
                InputEvent::Cancelled => {
                    self.status = Some("Selection cancelled.".to_string());
                    return Ok((OverlayOutcome::Close, LoopAction::Continue));
                }
                InputEvent::Submitted(_) => {
                    if let Some(item) = state.overlay.list.selected_item() {
                        let path = PathBuf::from(item.value.clone());
                        let cancelled = self.with_session_mut(|session| {
                            session
                                .switch_session(&path.to_string_lossy())
                                .map_err(|error| error.to_string())
                        })?;
                        self.clear_transient_entries();
                        self.show_new_session_banner = false;
                        self.refresh_snapshot()?;
                        self.pending_messages.clear();
                        self.active_tools.clear();
                        self.status = Some(if cancelled {
                            "Switched sessions after cancelling active work.".to_string()
                        } else {
                            format!("Switched to {}", path.to_string_lossy())
                        });
                        return Ok((OverlayOutcome::Close, LoopAction::Continue));
                    }
                }
                InputEvent::None => {}
            },
        }

        Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
    }

    fn handle_input_overlay_key(
        &mut self,
        state: &mut InputOverlayState,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        match state.input.handle_key(&event) {
            InputEvent::Changed | InputEvent::None => {
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
            InputEvent::Cancelled => {
                self.restore_overlay_from_input_action(&state.action)?;
                match &state.action {
                    InputOverlayAction::EditTreeLabel { .. }
                    | InputOverlayAction::TreeSummaryCustomPrompt { .. } => {
                        self.status = None;
                    }
                    InputOverlayAction::RenameSession { .. } => {
                        self.status = Some("Dialog cancelled.".to_string());
                    }
                }
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
            InputEvent::Submitted(value) => {
                self.apply_input_overlay_submit(&state.action, value)?;
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
        }
    }

    fn handle_auth_overlay_key(
        &mut self,
        state: &mut AuthOverlayState,
        event: KeyEvent,
    ) -> Result<(OverlayOutcome, LoopAction), String> {
        if matches_ctrl_char(&event, 'c') || matches!(event.code, KeyCode::Escape) {
            self.cancel_auth_flow();
            self.status = Some("Cancelling login...".to_string());
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }

        if !state.awaiting_input {
            return Ok((OverlayOutcome::KeepOpen, LoopAction::Continue));
        }

        match state.input.handle_key(&event) {
            InputEvent::Changed | InputEvent::None => {
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
            InputEvent::Cancelled => {
                self.cancel_auth_flow();
                self.status = Some("Cancelling login...".to_string());
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
            InputEvent::Submitted(value) => {
                if let Some(active) = &self.active_auth {
                    active
                        .response_tx
                        .send(AuthUiResponse::Input(value))
                        .map_err(|_| "Failed to deliver login input.".to_string())?;
                }
                state.awaiting_input = false;
                state.prompt_kind = None;
                state.input.clear();
                state.push_progress("Waiting for provider response...".to_string());
                Ok((OverlayOutcome::KeepOpen, LoopAction::Continue))
            }
        }
    }

    fn build_session_rename_input_overlay(
        &self,
        state: &SessionOverlayState,
        selected_value: &str,
    ) -> Result<InputOverlayState, String> {
        let path = PathBuf::from(selected_value);
        let record = load_session_record(&path)?;
        let mut input = Input::with_prompt("Name: ");
        input.set_focused(true);
        input.set_value(record.name.clone().unwrap_or_default());
        Ok(InputOverlayState {
            title: "Rename Session".to_string(),
            subtitle: truncate_to_width(&path.to_string_lossy(), 120),
            message_lines: vec!["Set a human-readable session name.".to_string()],
            hint: "Enter saves - Esc cancels".to_string(),
            input,
            action: InputOverlayAction::RenameSession {
                path,
                selected_value: selected_value.to_string(),
                scope: state.scope,
                sort_mode: state.sort_mode,
                name_filter: state.name_filter,
                show_path: state.show_path,
                query: state.overlay.search.get_value().to_string(),
            },
        })
    }

    fn build_tree_label_input_overlay(
        &self,
        selection: &[OverlaySelection],
        filter_mode: TreeFilterMode,
        query: &str,
        selected_value: &str,
    ) -> Result<InputOverlayState, String> {
        let selected = selection
            .iter()
            .find_map(|candidate| match candidate {
                OverlaySelection::Tree { entry_id, label } if entry_id == selected_value => {
                    Some((entry_id.clone(), label.clone()))
                }
                _ => None,
            })
            .ok_or_else(|| "No tree item selected for label editing.".to_string())?;
        let mut input = Input::with_prompt("Label (empty to remove): ");
        input.set_focused(true);
        input.set_value(selected.1.clone().unwrap_or_default());
        Ok(InputOverlayState {
            title: "Session Tree".to_string(),
            subtitle: String::new(),
            message_lines: Vec::new(),
            hint: "Enter saves · Esc cancels".to_string(),
            input,
            action: InputOverlayAction::EditTreeLabel {
                entry_id: selected.0,
                selected_value: selected_value.to_string(),
                filter_mode,
                query: query.to_string(),
            },
        })
    }

    fn build_tree_summary_overlay_state(
        &self,
        entry_id: &str,
        filter_mode: TreeFilterMode,
        query: &str,
    ) -> Result<TreeSummaryOverlayState, String> {
        let items = vec![
            SelectItem {
                value: "no-summary".to_string(),
                label: "  No summary".to_string(),
                description: None,
            },
            SelectItem {
                value: "summarize".to_string(),
                label: "  Summarize".to_string(),
                description: None,
            },
            SelectItem {
                value: "summarize-custom".to_string(),
                label: "  Summarize with custom prompt".to_string(),
                description: None,
            },
        ];
        let mut list = SelectList::new(items, 6);
        list.set_selected_index(0);
        Ok(TreeSummaryOverlayState {
            title: "Summarize branch?".to_string(),
            hint: "↑/↓ navigate  Enter select  Esc cancel".to_string(),
            list,
            target_entry_id: entry_id.to_string(),
            filter_mode,
            query: query.to_string(),
        })
    }

    fn build_tree_summary_custom_prompt_overlay(
        &self,
        entry_id: &str,
        filter_mode: TreeFilterMode,
        query: &str,
    ) -> Result<InputOverlayState, String> {
        let mut input = Input::with_prompt("Instructions: ");
        input.set_focused(true);
        Ok(InputOverlayState {
            title: "Custom summarization instructions".to_string(),
            subtitle: String::new(),
            message_lines: Vec::new(),
            hint: "Enter navigates · Esc cancels".to_string(),
            input,
            action: InputOverlayAction::TreeSummaryCustomPrompt {
                entry_id: entry_id.to_string(),
                filter_mode,
                query: query.to_string(),
            },
        })
    }

    fn restore_overlay_from_input_action(
        &mut self,
        action: &InputOverlayAction,
    ) -> Result<(), String> {
        match action {
            InputOverlayAction::RenameSession {
                selected_value,
                scope,
                sort_mode,
                name_filter,
                show_path,
                query,
                ..
            } => {
                self.overlay = Some(OverlayState::Session(self.build_session_overlay_state(
                    *scope,
                    *sort_mode,
                    *name_filter,
                    *show_path,
                    Some(query.as_str()),
                    Some(selected_value.as_str()),
                )?));
            }
            InputOverlayAction::EditTreeLabel {
                selected_value,
                filter_mode,
                query,
                ..
            } => {
                self.overlay = Some(self.build_tree_overlay_state(
                    *filter_mode,
                    Some(query.as_str()),
                    Some(selected_value.as_str()),
                )?);
            }
            InputOverlayAction::TreeSummaryCustomPrompt {
                entry_id,
                filter_mode,
                query,
            } => {
                self.overlay = Some(OverlayState::TreeSummary(
                    self.build_tree_summary_overlay_state(entry_id, *filter_mode, query)?,
                ));
            }
        }
        Ok(())
    }

    fn apply_input_overlay_submit(
        &mut self,
        action: &InputOverlayAction,
        value: String,
    ) -> Result<(), String> {
        match action {
            InputOverlayAction::RenameSession {
                path,
                selected_value,
                scope,
                sort_mode,
                name_filter,
                show_path,
                query,
            } => {
                let next = value.trim();
                if !next.is_empty() {
                    let mut manager =
                        SessionManager::open(path).map_err(|error| error.to_string())?;
                    manager
                        .append_session_info(next)
                        .map_err(|error| error.to_string())?;
                    let is_current_session = self
                        .cached_state
                        .session_file
                        .as_deref()
                        .map(PathBuf::from)
                        .as_ref()
                        == Some(path);
                    if is_current_session {
                        self.refresh_snapshot()?;
                    }
                    self.status = Some(format!("Renamed session to {next}."));
                } else {
                    self.status = Some("Session name unchanged.".to_string());
                }
                self.overlay = Some(OverlayState::Session(self.build_session_overlay_state(
                    *scope,
                    *sort_mode,
                    *name_filter,
                    *show_path,
                    Some(query.as_str()),
                    Some(selected_value.as_str()),
                )?));
            }
            InputOverlayAction::EditTreeLabel {
                entry_id,
                selected_value,
                filter_mode,
                query,
            } => {
                let label = value.trim();
                self.with_session_mut(|session| {
                    session
                        .set_entry_label(
                            entry_id,
                            if label.is_empty() {
                                None
                            } else {
                                Some(label.to_string())
                            },
                        )
                        .map_err(|error| error.to_string())
                })?;
                self.refresh_snapshot()?;
                self.status = None;
                self.overlay = Some(self.build_tree_overlay_state(
                    *filter_mode,
                    Some(query.as_str()),
                    Some(selected_value.as_str()),
                )?);
            }
            InputOverlayAction::TreeSummaryCustomPrompt {
                entry_id,
                filter_mode: _,
                query: _,
            } => {
                self.navigate_tree_target(entry_id, true, Some(value.as_str()))?;
                self.overlay = None;
            }
        }
        Ok(())
    }

    fn start_oauth_login(&mut self, provider: &str) -> Result<(), String> {
        if self.active_auth.is_some() {
            return Err("A login flow is already running.".to_string());
        }

        let (ui_tx, ui_rx) = mpsc::channel();
        let (response_tx, response_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let bridge = Arc::new(ChannelOAuthLoginBridge {
            ui_tx,
            response_tx: response_tx.clone(),
            response_rx: Arc::new(Mutex::new(response_rx)),
            cancel_flag: Arc::clone(&cancel_flag),
        });
        let provider_id = provider.to_string();
        let handle = thread::spawn({
            let bridge = Arc::clone(&bridge);
            let provider_id = provider_id.clone();
            move || {
                let result = login_oauth_provider(&provider_id, bridge);
                let _ = result_tx.send(result);
            }
        });

        self.overlay = Some(OverlayState::Auth(AuthOverlayState::new(provider)));
        self.active_auth = Some(ActiveAuthFlow {
            provider: provider.to_string(),
            ui_rx,
            response_tx,
            result_rx,
            cancel_flag,
            handle,
            started_at: Instant::now(),
        });
        self.status = Some(format!(
            "Login dialog open for {}.",
            oauth_provider_label(provider)
        ));
        Ok(())
    }

    fn cancel_auth_flow(&mut self) {
        if let Some(active) = &self.active_auth {
            active.cancel_flag.store(true, Ordering::SeqCst);
            let _ = active.response_tx.send(AuthUiResponse::Cancelled);
        }
    }

    fn handle_active_prompt_key(&mut self, event: KeyEvent) -> Result<LoopAction, String> {
        self.handle_prompt_key(event, true)
    }

    fn handle_prompt_key(
        &mut self,
        event: KeyEvent,
        active_prompt: bool,
    ) -> Result<LoopAction, String> {
        if self.handle_prompt_autocomplete_key(&event, active_prompt)? {
            return Ok(LoopAction::Continue);
        }

        if matches!(event.code, KeyCode::Escape)
            && event.modifiers == KeyModifiers::NONE
            && active_prompt
        {
            self.control.abort();
            if let Some(active) = &mut self.active_prompt {
                active.aborted = true;
            }
            self.status = Some("Abort requested.".to_string());
            return Ok(LoopAction::Continue);
        }

        if matches!(event.code, KeyCode::Escape)
            && event.modifiers == KeyModifiers::NONE
            && !self.prompt_is_empty()
        {
            self.clear_prompt()?;
            self.status = Some("Cancelled input.".to_string());
            return Ok(LoopAction::Continue);
        }

        if self.keybindings.matches(&event, AppAction::ExternalEditor) {
            return Ok(LoopAction::OpenExternalEditor);
        }

        let Some(input) = self.keybindings.resolve_prompt_editor_input(&event) else {
            return Ok(LoopAction::Continue);
        };

        match input {
            PromptEditorInput::TriggerAutocomplete => {
                self.update_prompt_autocomplete_with_force(true)?;
            }
            PromptEditorInput::InsertText(text) => {
                if self.editor.insert_text(&text) {
                    self.update_prompt_autocomplete()?;
                }
            }
            PromptEditorInput::Action(EditorAction::CursorUp) => {
                if self.handle_editor_up() {
                    self.update_prompt_autocomplete()?;
                }
            }
            PromptEditorInput::Action(EditorAction::CursorDown) => {
                if self.handle_editor_down() {
                    self.update_prompt_autocomplete()?;
                }
            }
            PromptEditorInput::Action(EditorAction::Submit) => {
                return self.submit_prompt_from_editor(active_prompt);
            }
            PromptEditorInput::Action(action) => match action.apply_to_editor(&mut self.editor) {
                EditorEvent::Changed => {
                    self.update_prompt_autocomplete()?;
                }
                EditorEvent::Submitted(_) => {
                    return self.submit_prompt_from_editor(active_prompt);
                }
                EditorEvent::Cancelled | EditorEvent::None => {}
            },
        }

        Ok(LoopAction::Continue)
    }

    fn handle_editor_up(&mut self) -> bool {
        let (line, _) = self.editor.cursor();
        if line == 0 {
            match self.editor.history_previous() {
                EditorEvent::Changed => true,
                EditorEvent::None | EditorEvent::Cancelled | EditorEvent::Submitted(_) => {
                    self.editor.move_up()
                }
            }
        } else {
            self.editor.move_up()
        }
    }

    fn handle_editor_down(&mut self) -> bool {
        let (line, _) = self.editor.cursor();
        let total_lines = split_prompt_lines(&self.prompt_text()).len();
        if line + 1 >= total_lines {
            match self.editor.history_next() {
                EditorEvent::Changed => true,
                EditorEvent::None | EditorEvent::Cancelled | EditorEvent::Submitted(_) => {
                    self.editor.move_down()
                }
            }
        } else {
            self.editor.move_down()
        }
    }

    fn submit_prompt_from_editor(&mut self, active_prompt: bool) -> Result<LoopAction, String> {
        let value = self.prompt_text().trim().to_string();
        if value.is_empty() {
            return Ok(LoopAction::Continue);
        }
        self.editor.add_history_entry(&value);
        self.clear_prompt()?;
        if active_prompt {
            self.queue_message(QueuedMessageKind::Steer, value);
            return Ok(LoopAction::Continue);
        }
        self.submit(value)
    }

    fn handle_prompt_autocomplete_key(
        &mut self,
        event: &KeyEvent,
        active_prompt: bool,
    ) -> Result<bool, String> {
        let Some(input) = self.keybindings.resolve_prompt_autocomplete_input(event) else {
            return Ok(false);
        };
        if self.prompt_autocomplete.is_none() {
            return Ok(false);
        }

        match input {
            PromptAutocompleteInput::Cancel => {
                self.prompt_autocomplete = None;
                self.status = Some("Autocomplete dismissed.".to_string());
            }
            PromptAutocompleteInput::NavigateUp
            | PromptAutocompleteInput::NavigateDown
            | PromptAutocompleteInput::ConfirmSelection => {
                if let Some(autocomplete) = self.prompt_autocomplete.as_mut() {
                    let event = input.apply_to_select_list(&mut autocomplete.list);
                    if matches!(event, Some(SelectEvent::Selected(_))) {
                        if prompt_autocomplete_should_submit_current_prompt(
                            self.prompt_autocomplete.as_ref(),
                            &self.prompt_text(),
                            self.editor.cursor(),
                        ) {
                            let _ = self.submit_prompt_from_editor(active_prompt)?;
                        } else {
                            self.accept_prompt_completion(true, active_prompt)?;
                        }
                    }
                }
            }
            PromptAutocompleteInput::AcceptCompletion => {
                self.accept_prompt_completion(false, active_prompt)?;
            }
        }

        Ok(true)
    }

    fn accept_prompt_completion(
        &mut self,
        submit_after: bool,
        active_prompt: bool,
    ) -> Result<(), String> {
        let Some(item) = self
            .prompt_autocomplete
            .as_ref()
            .and_then(|autocomplete| autocomplete.list.selected_item().cloned())
        else {
            return Ok(());
        };
        let kind = self
            .prompt_autocomplete
            .as_ref()
            .map(|autocomplete| autocomplete.kind)
            .unwrap_or(PromptAutocompleteKind::SlashCommand);

        match kind {
            PromptAutocompleteKind::SlashCommand => {
                self.apply_slash_completion(&item.value)?;
            }
            PromptAutocompleteKind::ModelArgument => {
                self.apply_model_completion(&item.value)?;
            }
            PromptAutocompleteKind::Path | PromptAutocompleteKind::FileReference => {
                let replace_prefix = self
                    .prompt_autocomplete
                    .as_ref()
                    .map(|autocomplete| autocomplete.replace_prefix.clone())
                    .unwrap_or_default();
                self.apply_path_completion(
                    &item,
                    &replace_prefix,
                    matches!(kind, PromptAutocompleteKind::FileReference),
                )?;
            }
        }
        self.prompt_autocomplete = None;

        if submit_after
            && matches!(
                kind,
                PromptAutocompleteKind::SlashCommand | PromptAutocompleteKind::ModelArgument
            )
        {
            let _ = self.submit_prompt_from_editor(active_prompt)?;
        } else {
            self.update_prompt_autocomplete()?;
        }
        Ok(())
    }

    fn update_prompt_autocomplete(&mut self) -> Result<(), String> {
        self.update_prompt_autocomplete_with_force(false)
    }

    fn update_prompt_autocomplete_with_force(
        &mut self,
        force_file_completion: bool,
    ) -> Result<(), String> {
        let commands = self.with_session(|session| session.get_commands())?;
        self.prompt_autocomplete = build_prompt_autocomplete(
            &self.prompt_text(),
            self.editor.cursor(),
            &commands,
            || {
                let all = self.with_session(|session| session.get_available_models())?;
                let scoped = self.with_session(|session| session.get_scoped_models())?;
                Ok::<_, String>(if scoped.is_empty() { all } else { scoped })
            },
            &self.cwd,
            force_file_completion,
        )?;
        Ok(())
    }

    fn apply_slash_completion(&mut self, command: &str) -> Result<(), String> {
        let text = self.prompt_text();
        let mut lines = split_prompt_lines(&text);
        let (cursor_line, cursor_col) = self.editor.cursor();
        if cursor_line >= lines.len() {
            return Ok(());
        }
        let current_line = lines[cursor_line].clone();
        let token_start = current_line
            .char_indices()
            .find_map(|(index, ch)| (!ch.is_whitespace()).then_some(index))
            .unwrap_or(0);
        let token_end = current_line[cursor_col..]
            .find(char::is_whitespace)
            .map(|offset| cursor_col + offset)
            .unwrap_or(cursor_col);
        let needs_trailing_space = current_line[token_end..]
            .chars()
            .next()
            .is_none_or(|value| !value.is_whitespace());
        let replacement = if needs_trailing_space {
            format!("{command} ")
        } else {
            command.to_string()
        };
        lines[cursor_line].replace_range(token_start..token_end, &replacement);
        let next_text = lines.join("\n");
        self.editor.set_text(next_text);
        self.editor
            .set_cursor(cursor_line, token_start + replacement.len());
        Ok(())
    }

    fn apply_model_completion(&mut self, model_value: &str) -> Result<(), String> {
        let text = self.prompt_text();
        let mut lines = split_prompt_lines(&text);
        let (cursor_line, cursor_col) = self.editor.cursor();
        if cursor_line >= lines.len() {
            return Ok(());
        }
        let current_line = lines[cursor_line].clone();
        let command_prefix = "/model ";
        let Some(prefix_start) = current_line.find(command_prefix) else {
            return Ok(());
        };
        let value_start = prefix_start + command_prefix.len();
        let value_end = current_line[cursor_col..]
            .find(char::is_whitespace)
            .map(|offset| cursor_col + offset)
            .unwrap_or(cursor_col);
        lines[cursor_line].replace_range(value_start..value_end, model_value);
        let next_text = lines.join("\n");
        self.editor.set_text(next_text);
        self.editor
            .set_cursor(cursor_line, value_start + model_value.len());
        Ok(())
    }

    fn apply_path_completion(
        &mut self,
        item: &SelectItem,
        prefix: &str,
        add_space_for_files: bool,
    ) -> Result<(), String> {
        let text = self.prompt_text();
        let mut lines = split_prompt_lines(&text);
        let (cursor_line, cursor_col) = self.editor.cursor();
        if cursor_line >= lines.len() {
            return Ok(());
        }
        if prefix.len() > cursor_col {
            return Ok(());
        }
        let current_line = lines[cursor_line].clone();
        let before_prefix = &current_line[..cursor_col - prefix.len()];
        let after_cursor = &current_line[cursor_col..];
        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor.starts_with('"');
        let has_trailing_quote_in_item = item.value.ends_with('"');
        let adjusted_after_cursor =
            if is_quoted_prefix && has_trailing_quote_in_item && has_leading_quote_after_cursor {
                &after_cursor[1..]
            } else {
                after_cursor
            };
        let is_directory = item.label.ends_with('/');
        let suffix = if add_space_for_files && !is_directory {
            " "
        } else {
            ""
        };
        let next_line = format!(
            "{before_prefix}{}{}{}",
            item.value, suffix, adjusted_after_cursor
        );
        let mut next_cursor = before_prefix.len() + item.value.len() + suffix.len();
        if is_directory && has_trailing_quote_in_item {
            next_cursor = next_cursor.saturating_sub(1);
        }
        lines[cursor_line] = next_line;
        self.editor.set_text(lines.join("\n"));
        self.editor.set_cursor(cursor_line, next_cursor);
        Ok(())
    }

    fn submit(&mut self, value: String) -> Result<LoopAction, String> {
        let text = value.trim().to_string();
        if text.is_empty() {
            return Ok(LoopAction::Continue);
        }

        self.clear_prompt()?;
        if text == "/quit" || text == "/exit" {
            return Ok(LoopAction::Quit);
        }
        if text == "/new" {
            let _ = self.with_session_mut(|session| {
                session.new_session(None).map_err(|error| error.to_string())
            })?;
            self.clear_transient_entries();
            self.show_new_session_banner = false;
            self.pending_messages.clear();
            self.refresh_snapshot()?;
            self.status = Some("Started a new session.".to_string());
            return Ok(LoopAction::Continue);
        }
        if text == "/session" {
            self.append_summary_entry(
                "Session Info",
                format_session_summary_markdown(&self.cached_state, &self.cached_stats),
            );
            self.status = Some("Session Info added to the transcript.".to_string());
            return Ok(LoopAction::Continue);
        }
        if text == "/resume" {
            self.open_session_overlay(None)?;
            return Ok(LoopAction::Continue);
        }
        if text == "/settings" {
            self.open_settings_overlay(None)?;
            return Ok(LoopAction::Continue);
        }
        if text == "/scoped-models" {
            self.open_scoped_models_overlay(None)?;
            return Ok(LoopAction::Continue);
        }
        if text == "/tree" {
            self.open_tree_overlay(None)?;
            return Ok(LoopAction::Continue);
        }
        if text == "/fork" {
            self.open_fork_overlay(None)?;
            return Ok(LoopAction::Continue);
        }
        if text == "/login" {
            self.open_oauth_selector(AuthFlowMode::Login)?;
            return Ok(LoopAction::Continue);
        }
        if text == "/logout" {
            self.open_oauth_selector(AuthFlowMode::Logout)?;
            return Ok(LoopAction::Continue);
        }
        if text == "/model" || text.starts_with("/model ") {
            let filter = text
                .strip_prefix("/model ")
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if let Some(exact) = filter {
                let exact_match =
                    self.with_session(|session| session.find_model_for_selection(exact))?;
                if let Some(model_match) = exact_match {
                    let model = self.with_session_mut(|session| {
                        session
                            .set_model(&model_match.provider.0, &model_match.id)
                            .map_err(|error| error.to_string())
                    })?;
                    self.refresh_snapshot()?;
                    self.status = Some(format!("Switched to {}/{}", model.provider.0, model.id));
                    return Ok(LoopAction::Continue);
                }
            }
            self.open_model_overlay(filter)?;
            return Ok(LoopAction::Continue);
        }
        if text == "/copy" {
            let copied = self.with_session(|session| session.get_last_assistant_text())?;
            let Some(copied) = copied.filter(|value| !value.trim().is_empty()) else {
                self.status = Some("No agent messages to copy yet.".to_string());
                return Ok(LoopAction::Continue);
            };
            copy_to_clipboard(&copied)?;
            self.status = Some("Copied last agent message to clipboard".to_string());
            return Ok(LoopAction::Continue);
        }
        if text == "/share" {
            let temp_path = share_export_path();
            let exported = self.with_session(|session| {
                session
                    .export_html(Some(&temp_path))
                    .map_err(|error| error.to_string())
            })??;
            let share_result = create_secret_gist(&exported);
            let _ = fs::remove_file(&exported);
            match share_result {
                Ok((viewer_url, gist_url)) => {
                    self.status = Some(format!(
                        "Secret share created.\nPreview: {viewer_url}\nGist: {gist_url}"
                    ));
                }
                Err(error) => {
                    self.status = Some(error);
                }
            }
            return Ok(LoopAction::Continue);
        }
        if text == "/reload" {
            self.package_manager = PackageManager::create(&self.cwd, None);
            self.keybindings = KeybindingsManager::create(self.session_dir_override.clone());
            let merged_settings = self.package_manager.settings_manager().merged_settings();
            self.hide_thinking = bool_setting(&merged_settings, &["hideThinkingBlock"], false);
            self.show_images = bool_setting(&merged_settings, &["terminal", "showImages"], true);
            self.double_escape_action = DoubleEscapeAction::from_settings(
                string_setting(&merged_settings, &["doubleEscapeAction"]).as_deref(),
            );
            let steering_mode = queue_mode_setting(&merged_settings, &["steeringMode"]);
            let follow_up_mode = queue_mode_setting(&merged_settings, &["followUpMode"]);
            let auto_compact = bool_setting(&merged_settings, &["compaction", "enabled"], true);
            self.with_session_mut(|session| {
                session.set_steering_mode(steering_mode);
                session.set_follow_up_mode(follow_up_mode);
                session.set_auto_compaction(auto_compact);
                session
                    .reload_runtime_resources()
                    .map_err(|error| error.to_string())?;
                Ok(())
            })?;
            self.refresh_snapshot()?;
            self.update_prompt_autocomplete()?;
            let settings_errors =
                self.with_session_mut(|session| Ok(session.drain_settings_errors()))?;
            self.status = Some(if settings_errors.is_empty() {
                "Reloaded extensions, skills, prompts, themes".to_string()
            } else {
                format!(
                    "Reloaded with {} settings warning(s). Check your settings files.",
                    settings_errors.len()
                )
            });
            return Ok(LoopAction::Continue);
        }
        if text == "/changelog" {
            self.append_summary_entry("What's New", load_changelog_markdown()?);
            self.status = Some("Changelog added to the transcript.".to_string());
            return Ok(LoopAction::Continue);
        }
        if text == "/hotkeys" {
            self.append_summary_entry(
                "Keyboard Shortcuts",
                format_hotkeys_markdown(&self.keybindings),
            );
            self.status = Some("Keyboard shortcuts added to the transcript.".to_string());
            return Ok(LoopAction::Continue);
        }
        if text == "/compact" || text.starts_with("/compact ") {
            let custom_instructions = text
                .strip_prefix("/compact ")
                .map(str::trim)
                .filter(|value| !value.is_empty());
            self.with_session_mut(|session| {
                session
                    .compact(custom_instructions)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })?;
            self.refresh_snapshot()?;
            self.status = Some("Compacted the current session.".to_string());
            return Ok(LoopAction::Continue);
        }
        if text == "/export" || text.starts_with("/export ") {
            let output_path = text
                .strip_prefix("/export ")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from);
            let exported = self.with_session(|session| {
                session
                    .export_html(output_path.as_deref())
                    .map_err(|error| error.to_string())
            })??;
            self.status = Some(format!("Session exported to: {}", exported.display()));
            return Ok(LoopAction::Continue);
        }
        if let Some(name) = text.strip_prefix("/name ").map(str::trim) {
            if name.is_empty() {
                self.status = Some("Usage: /name <session name>".to_string());
            } else {
                self.with_session_mut(|session| {
                    session
                        .set_session_name(name)
                        .map_err(|error| error.to_string())
                })?;
                self.refresh_snapshot()?;
                self.status = Some(format!("Session name set to {name}."));
            }
            return Ok(LoopAction::Continue);
        }
        if let Some(command) = text.strip_prefix("!!").or_else(|| text.strip_prefix('!')) {
            let excluded = text.starts_with("!!");
            self.with_session_mut(|session| {
                session
                    .manual_bash(command.trim(), excluded)
                    .map_err(|error| error.to_string())
            })?;
            self.refresh_snapshot()?;
            self.status = None;
            return Ok(LoopAction::Continue);
        }

        self.start_prompt(text)?;
        Ok(LoopAction::Continue)
    }

    fn start_prompt(&mut self, prompt: String) -> Result<(), String> {
        if self.active_prompt.is_some() {
            return Err("A prompt is already running.".to_string());
        }

        let session = Arc::clone(&self.session);
        let (result_tx, result_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
            let outcome = {
                let mut session = session.lock().expect("interactive session lock");
                session.prepare_prompt();
                let _ = started_tx.send(());
                runtime.block_on(session.prompt_text_prepared_with_events(prompt, event_tx))
            }
            .map_err(|error| error.to_string());
            let _ = result_tx.send(outcome);
        });

        started_rx
            .recv()
            .map_err(|_| "Failed to start prompt worker".to_string())?;
        let fresh_session =
            self.cached_transcript.is_empty() && self.transient_transcript.is_empty();
        if fresh_session {
            self.show_new_session_banner = true;
        }
        self.active_prompt = Some(ActivePrompt {
            event_rx,
            result_rx,
            handle: Some(handle),
            aborted: false,
            started_at: Instant::now(),
            completion_result: None,
            linger_after_completion: false,
        });
        self.status = None;
        Ok(())
    }

    fn queue_message(&mut self, kind: QueuedMessageKind, text: String) {
        let message = Message::User(UserMessage {
            content: UserContent::Text(text.clone()),
            timestamp: 0,
        });
        match kind {
            QueuedMessageKind::Steer => self.control.steer(message),
            QueuedMessageKind::FollowUp => self.control.follow_up(message),
        }
        self.pending_messages.push(QueuedMessage {
            kind,
            text: text.clone(),
        });
        self.status = Some(format!(
            "Queued {} message ({} pending).",
            kind.label(),
            self.pending_messages.len()
        ));
    }

    fn dequeue_last_pending(&mut self) -> bool {
        while let Some(queued) = self.pending_messages.pop() {
            let restored = match queued.kind {
                QueuedMessageKind::Steer => self.control.pop_last_steering(),
                QueuedMessageKind::FollowUp => self.control.pop_last_follow_up(),
            };
            if restored.is_some() {
                let _ = self.set_prompt_text(queued.text);
                return true;
            }
        }
        false
    }

    fn restore_pending_messages(&mut self) -> usize {
        let mut restored = Vec::new();
        for queued in self.pending_messages.iter().rev() {
            let popped = match queued.kind {
                QueuedMessageKind::Steer => self.control.pop_last_steering(),
                QueuedMessageKind::FollowUp => self.control.pop_last_follow_up(),
            };
            if popped.is_some() {
                restored.push(queued.text.clone());
            }
        }
        self.pending_messages.clear();
        restored.reverse();
        if !restored.is_empty() {
            let _ = self.set_prompt_text(restored.join("\n"));
        }
        restored.len()
    }

    fn refresh_snapshot(&mut self) -> Result<(), String> {
        let (messages, transcript, state, stats, available_model_count, startup_summary) = self
            .with_session(|session| {
                (
                    session.get_messages(),
                    build_transcript_entries(session),
                    session.get_state(),
                    session.get_session_stats(),
                    session.get_available_models().len(),
                    session.startup_resource_summary().clone(),
                )
            })?;
        let startup_notices = self.with_session_mut(|session| {
            Ok(session
                .drain_settings_errors()
                .into_iter()
                .map(|error| format!("{:?}: {}", error.scope, error.message))
                .collect::<Vec<_>>())
        })?;
        self.cached_messages = messages;
        self.cached_transcript = transcript;
        self.cached_state = state;
        self.cached_stats = stats;
        self.available_model_count = available_model_count;
        self.startup_resource_summary = startup_summary.clone();
        self.startup_context_files = if startup_summary.context_paths.is_empty() {
            discover_startup_context_files(&self.cwd)
        } else {
            startup_summary
                .context_paths
                .iter()
                .map(|path| shorten_home_path(&path.to_string_lossy()))
                .collect()
        };
        self.startup_notices = startup_notices;
        Ok(())
    }

    fn combined_transcript(&self) -> Vec<TranscriptEntry> {
        let mut transcript = self.cached_transcript.clone();
        transcript.extend(self.transient_transcript.clone());
        transcript
    }

    fn append_summary_entry(&mut self, title: &'static str, text: String) {
        self.transient_transcript.push(TranscriptEntry::Summary {
            kind: SummaryKind::Generic,
            title,
            text,
            tokens_before: None,
        });
    }

    fn clear_transient_entries(&mut self) {
        self.transient_transcript.clear();
    }

    fn open_model_overlay(&mut self, filter: Option<&str>) -> Result<(), String> {
        let scoped_models = self.with_session(|session| session.get_scoped_models())?;
        let scope = if scoped_models.is_empty() {
            ModelOverlayScope::All
        } else {
            ModelOverlayScope::Scoped
        };
        self.overlay = Some(OverlayState::Model(
            self.build_model_overlay_state(scope, filter, None)?,
        ));
        Ok(())
    }

    fn open_scoped_models_overlay(&mut self, filter: Option<&str>) -> Result<(), String> {
        self.overlay = Some(OverlayState::ScopedModels(
            self.build_scoped_models_overlay_state(filter, None)?,
        ));
        Ok(())
    }

    fn build_model_overlay_state(
        &self,
        scope: ModelOverlayScope,
        filter: Option<&str>,
        selected_value: Option<&str>,
    ) -> Result<ModelOverlayState, String> {
        let overlay = SearchOverlay::new(String::new(), String::new(), Vec::new(), filter, "");
        let mut state = ModelOverlayState {
            overlay,
            selections: Vec::new(),
            models: Vec::new(),
            current_model: self.cached_state.model.clone(),
            scope,
            available_count: 0,
            scoped_count: 0,
        };
        self.reload_model_overlay(&mut state, selected_value)?;
        Ok(state)
    }

    fn reload_model_overlay(
        &self,
        state: &mut ModelOverlayState,
        selected_value: Option<&str>,
    ) -> Result<(), String> {
        let all_models = self.with_session(|session| session.get_available_models())?;
        let scoped_models = self.with_session(|session| session.get_scoped_models())?;
        state.available_count = all_models.len();
        state.scoped_count = scoped_models.len();
        if state.scoped_count == 0 {
            state.scope = ModelOverlayScope::All;
        }
        let active_models = match state.scope {
            ModelOverlayScope::All => &all_models,
            ModelOverlayScope::Scoped => &scoped_models,
        };
        let sorted_models =
            sort_model_overlay_models(active_models, self.cached_state.model.as_ref());
        let (items, selections) =
            build_model_overlay_items(&sorted_models, self.cached_state.model.as_ref());
        state
            .overlay
            .replace_items_preserving_selection(items, selected_value);
        state.models = sorted_models;
        state.current_model = self.cached_state.model.clone();
        state.selections = selections;
        self.update_model_overlay_metadata(state);
        Ok(())
    }

    fn build_scoped_models_overlay_state(
        &self,
        filter: Option<&str>,
        selected_value: Option<&str>,
    ) -> Result<ScopedModelsOverlayState, String> {
        let overlay = SearchOverlay::new("Scoped Models", String::new(), Vec::new(), filter, "");
        let enabled_ids = self.with_session(|session| {
            let entries = session.get_scoped_model_entries();
            if entries.is_empty() {
                None
            } else {
                Some(
                    entries
                        .into_iter()
                        .map(|entry| format!("{}/{}", entry.model.provider.0, entry.model.id))
                        .collect::<Vec<_>>(),
                )
            }
        })?;
        let mut state = ScopedModelsOverlayState {
            overlay,
            models: Vec::new(),
            enabled_ids,
            dirty: false,
        };
        self.reload_scoped_models_overlay(&mut state, selected_value)?;
        Ok(state)
    }

    fn reload_scoped_models_overlay(
        &self,
        state: &mut ScopedModelsOverlayState,
        selected_value: Option<&str>,
    ) -> Result<(), String> {
        let models = self.with_session(|session| session.get_available_models())?;
        state.models = sort_model_overlay_models(&models, self.cached_state.model.as_ref());
        state.overlay.replace_items_preserving_selection(
            build_scoped_model_items(&state.models, state.enabled_ids.as_deref()),
            selected_value,
        );
        self.update_scoped_models_overlay_metadata(state);
        Ok(())
    }

    fn update_scoped_models_overlay_metadata(&self, state: &mut ScopedModelsOverlayState) {
        let enabled_count = state
            .enabled_ids
            .as_ref()
            .map_or(state.models.len(), Vec::len);
        let count_text = if state.enabled_ids.is_none() {
            format!("all {} enabled", state.models.len())
        } else {
            format!("{enabled_count}/{} enabled", state.models.len())
        };
        let detail = state.overlay.selected_item().and_then(|item| {
            state
                .models
                .iter()
                .find(|model| model_full_id(model) == item.value)
                .map(|model| {
                    format!(
                        "Model Name: {} · {} · {} ctx",
                        model.name,
                        if model.reasoning { "reasoning" } else { "text" },
                        format_token_count(model.context_window as u64)
                    )
                })
        });
        state.overlay.set_title("Scoped Models");
        state.overlay.set_subtitle(format!(
            "Session-only model cycle filter · {count_text}{}",
            if state.dirty { " · unsaved" } else { "" }
        ));
        state.overlay.set_detail(detail);
        state.overlay.set_hint(format!(
            "Enter toggles · Ctrl+A all · Ctrl+X clear · Ctrl+P provider · Alt+Up/Down reorder · Ctrl+S save\nSearch filters id/provider/name · Esc cancels"
        ));
    }

    fn sync_scoped_models_overlay_to_session(
        &mut self,
        state: &ScopedModelsOverlayState,
    ) -> Result<(), String> {
        match state.enabled_ids.as_ref() {
            Some(enabled_ids) => {
                let patterns = enabled_ids.to_vec();
                self.with_session_mut(|session| {
                    session.set_scoped_models_from_patterns(&patterns);
                    Ok(())
                })?;
            }
            None => {
                self.with_session_mut(|session| {
                    session.set_scoped_models(Vec::new());
                    Ok(())
                })?;
            }
        }
        Ok(())
    }

    fn toggle_model_overlay_scope(&mut self, state: &mut ModelOverlayState) -> Result<(), String> {
        if state.scoped_count == 0 {
            state.scope = ModelOverlayScope::All;
            self.update_model_overlay_metadata(state);
            return Ok(());
        }

        state.scope = state.scope.next();
        let selected_value = state.overlay.selected_value().map(ToOwned::to_owned);
        self.reload_model_overlay(state, selected_value.as_deref())?;
        self.status = Some(format!("Model scope: {}", state.scope.label()));
        Ok(())
    }

    fn update_model_overlay_metadata(&self, state: &mut ModelOverlayState) {
        update_model_overlay_metadata(
            &mut state.overlay,
            state.available_count,
            state.scoped_count,
            self.cached_state.model.as_ref(),
            state.scope,
        );
    }

    fn open_session_overlay(&mut self, filter: Option<&str>) -> Result<(), String> {
        self.overlay = Some(OverlayState::Session(self.build_session_overlay_state(
            SessionScope::Current,
            SessionSortMode::Threaded,
            SessionNameFilter::All,
            false,
            filter,
            None,
        )?));
        Ok(())
    }

    fn open_fork_overlay(&mut self, filter: Option<&str>) -> Result<(), String> {
        let state = self.build_fork_overlay_state(filter)?;
        if state.selections.is_empty() {
            self.status = Some("No messages to fork from".to_string());
            return Ok(());
        }
        self.overlay = Some(OverlayState::Fork(state));
        self.status = None;
        Ok(())
    }

    fn open_settings_overlay(&mut self, filter: Option<&str>) -> Result<(), String> {
        self.overlay = Some(OverlayState::Settings(
            self.build_settings_overlay_state(filter)?,
        ));
        Ok(())
    }

    fn open_oauth_selector(&mut self, mode: AuthFlowMode) -> Result<(), String> {
        let providers = self.with_session(|session| {
            let storage = session.model_registry().auth_storage();
            let registered = get_oauth_providers();
            let has_logged_in_oauth = registered
                .iter()
                .any(|provider| matches!(storage.get(provider), Some(AuthCredential::OAuth(_))));
            if matches!(mode, AuthFlowMode::Logout) && !has_logged_in_oauth {
                return None;
            }
            let mut items = Vec::new();
            let mut selections = Vec::new();
            for provider in registered {
                let status = storage.get_status(&provider);
                items.push(SelectItem {
                    value: provider.clone(),
                    label: format!(
                        "{}{}",
                        oauth_provider_label(&provider),
                        if status.authenticated {
                            " ✓ logged in"
                        } else {
                            ""
                        }
                    ),
                    description: None,
                });
                selections.push(OverlaySelection::AuthProvider { provider });
            }
            Some((items, selections))
        })?;

        let Some(providers) = providers else {
            self.status = Some(match mode {
                AuthFlowMode::Login => "No OAuth providers available".to_string(),
                AuthFlowMode::Logout => {
                    "No OAuth providers logged in. Use /login first.".to_string()
                }
            });
            return Ok(());
        };

        if providers.0.is_empty() {
            self.status = Some("No OAuth providers are registered.".to_string());
            return Ok(());
        }

        self.overlay = Some(OverlayState::Search {
            kind: match mode {
                AuthFlowMode::Login => SearchOverlayKind::OAuthLogin,
                AuthFlowMode::Logout => SearchOverlayKind::OAuthLogout,
            },
            overlay: {
                let mut overlay = SearchOverlay::new(
                    match mode {
                        AuthFlowMode::Login => "Select provider to login:",
                        AuthFlowMode::Logout => "Select provider to logout:",
                    },
                    "",
                    providers.0,
                    None,
                    "",
                );
                overlay.set_search_visible(false);
                overlay
            },
            selection: providers.1,
            tree_filter: None,
        });
        self.status = None;
        Ok(())
    }

    fn open_tree_overlay(&mut self, filter: Option<&str>) -> Result<(), String> {
        match self.build_tree_overlay_state(TreeFilterMode::Default, filter, None) {
            Ok(overlay) => {
                self.overlay = Some(overlay);
                self.status = None;
            }
            Err(error) if error == "No entries in session" => {
                self.overlay = None;
                self.status = Some(error);
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn build_tree_overlay_state(
        &self,
        filter_mode: TreeFilterMode,
        filter: Option<&str>,
        selected_value: Option<&str>,
    ) -> Result<OverlayState, String> {
        let (items, selections) = self.build_tree_overlay_items(filter_mode)?;
        if items.is_empty() {
            return Err("No entries in session".to_string());
        }
        let mut overlay = SearchOverlay::new(
            "Session Tree",
            "↑/↓: move. ←/→: page. Shift+L: label. ^D/^T/^U/^L/^A: filters (^O/⇧^O cycle)",
            items,
            filter,
            "↑/↓: move. ←/→: page. Shift+L: label. ^D/^T/^U/^L/^A: filters (^O/⇧^O cycle)",
        );
        overlay.set_search_prompt("Type to search: ");
        overlay.set_detail(Some(style_hint(&format!("[{}]", filter_mode.label()))));
        let selected_value = selected_value.map(ToOwned::to_owned).or_else(|| {
            self.with_session(|session| session.get_leaf_id())
                .ok()
                .flatten()
        });
        if let Some(selected_value) = selected_value.as_deref() {
            overlay.list.set_selected_value(selected_value);
        }
        Ok(OverlayState::Search {
            kind: SearchOverlayKind::Tree,
            overlay,
            selection: selections,
            tree_filter: Some(filter_mode),
        })
    }

    fn navigate_tree_target(
        &mut self,
        entry_id: &str,
        summarize: bool,
        custom_instructions: Option<&str>,
    ) -> Result<(), String> {
        let result = self.with_session_mut(|session| {
            session
                .navigate_tree(entry_id, summarize, custom_instructions)
                .map_err(|error| error.to_string())
        })?;
        self.refresh_snapshot()?;
        self.active_tools.clear();
        if let Some(editor_text) = result.editor_text.filter(|_| self.prompt_is_empty()) {
            self.set_prompt_text(editor_text)?;
        }
        self.status = Some("Navigated to selected point".to_string());
        Ok(())
    }

    fn build_session_overlay_state(
        &self,
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
            current_session_file: self.cached_state.session_file.as_ref().map(PathBuf::from),
            standalone: false,
            scope,
            sort_mode,
            name_filter,
            show_path,
            confirming_delete: None,
        };
        self.reload_session_overlay(&mut state, selected_value)?;
        Ok(state)
    }

    fn reload_session_overlay(
        &self,
        state: &mut SessionOverlayState,
        selected_value: Option<&str>,
    ) -> Result<(), String> {
        let query = state.overlay.search.get_value().to_string();
        let records = self.discover_session_records(state.scope)?;
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
        state.current_session_file = self.cached_state.session_file.as_ref().map(PathBuf::from);
        state.selections = selections;
        self.update_session_overlay_metadata(state);
        Ok(())
    }

    fn update_session_overlay_metadata(&self, state: &mut SessionOverlayState) {
        update_session_overlay_metadata_with_options(
            state,
            &self.keybindings,
            self.cached_state.session_file.as_deref().map(Path::new),
            true,
        );
    }

    fn discover_session_records(&self, scope: SessionScope) -> Result<Vec<SessionRecord>, String> {
        let current_session_dir =
            self.with_session(|session| session.session().get_session_dir().to_path_buf())?;
        let root = session_scope_root(
            scope,
            Some(current_session_dir.as_path()),
            &self.cwd,
            self.session_dir_override.as_deref(),
        );
        discover_session_records(&root)
    }

    fn build_fork_overlay_items(
        &self,
    ) -> Result<
        (
            Vec<SelectItem>,
            Vec<OverlaySelection>,
            Vec<ForkableUserMessage>,
        ),
        String,
    > {
        let messages = self.with_session(|session| session.forkable_user_messages())?;
        let mut items = Vec::new();
        let mut selections = Vec::new();
        for message in &messages {
            let preview = fork_message_preview_text(&message.text);
            items.push(SelectItem {
                value: message.entry_id.clone(),
                label: truncate_to_width(&preview, 72),
                description: Some(format!("Message {}", message.index.saturating_add(1))),
            });
            selections.push(OverlaySelection::Fork {
                entry_id: message.entry_id.clone(),
            });
        }
        Ok((items, selections, messages))
    }

    fn build_fork_overlay_state(&self, _filter: Option<&str>) -> Result<ForkOverlayState, String> {
        let (items, selections, messages) = self.build_fork_overlay_items()?;
        let mut list = SelectList::new(items, 10);
        if !messages.is_empty() {
            list.set_selected_index(messages.len().saturating_sub(1));
        }
        Ok(ForkOverlayState {
            title: "Branch from Message".to_string(),
            subtitle: "Select a message to create a new branch from that point".to_string(),
            hint: "↑/↓ select · Enter branches · Esc cancels".to_string(),
            list,
            selections,
            messages,
        })
    }

    fn build_settings_overlay_state(
        &self,
        filter: Option<&str>,
    ) -> Result<SettingsOverlayState, String> {
        let mut list = SettingsList::with_options(
            self.build_settings_items()?,
            10,
            SettingsListOptions {
                enable_search: true,
            },
        );
        list.set_focused(true);
        if let Some(filter) = filter.filter(|value| !value.trim().is_empty()) {
            let _ = list.handle_key(&KeyEvent::new(KeyCode::Paste(filter.trim().to_string())));
        }
        Ok(SettingsOverlayState {
            title: String::new(),
            subtitle: String::new(),
            hint: String::new(),
            list,
        })
    }

    fn build_settings_items(&self) -> Result<Vec<SettingItem>, String> {
        let merged_settings = self.package_manager.settings_manager().merged_settings();
        let mut thinking_levels = vec!["off".to_string()];
        if let Some(model) = self.cached_state.model.clone() {
            if model.reasoning {
                thinking_levels = vec![
                    "off".to_string(),
                    "minimal".to_string(),
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                ];
                if supports_xhigh(&model) {
                    thinking_levels.push("xhigh".to_string());
                }
            }
        }

        let mut themes = vec!["dark".to_string(), "light".to_string()];
        for theme in self.with_session(|session| session.get_themes())? {
            if let Some(name) = theme
                .path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToOwned::to_owned)
            {
                themes.push(name);
            }
        }
        themes.sort();
        themes.dedup();

        let transport =
            string_setting(&merged_settings, &["transport"]).unwrap_or_else(|| "sse".to_string());
        let current_theme =
            string_setting(&merged_settings, &["theme"]).unwrap_or_else(|| "dark".to_string());
        let auto_resize_images = bool_setting(&merged_settings, &["images", "autoResize"], true);
        let block_images = bool_setting(&merged_settings, &["images", "blockImages"], false);
        let skill_commands = bool_setting(&merged_settings, &["enableSkillCommands"], true);
        let show_hardware_cursor = bool_setting(
            &merged_settings,
            &["showHardwareCursor"],
            std::env::var("PI_HARDWARE_CURSOR").ok().as_deref() == Some("1"),
        );
        let editor_padding = navigate_setting(&merged_settings, &["editorPaddingX"])
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 3);
        let autocomplete_max_visible =
            navigate_setting(&merged_settings, &["autocompleteMaxVisible"])
                .and_then(Value::as_i64)
                .unwrap_or(5)
                .clamp(3, 20);
        let quiet_startup = bool_setting(&merged_settings, &["quietStartup"], false)
            || bool_setting(&merged_settings, &["terminal", "quietStartup"], false);
        let clear_on_shrink = bool_setting(&merged_settings, &["terminal", "clearOnShrink"], false);
        let collapse_changelog = bool_setting(&merged_settings, &["collapseChangelog"], false);

        let settings_item = |key: SettingKey,
                             label: &str,
                             description: &str,
                             current_value: String,
                             values: Vec<&str>|
         -> SettingItem {
            SettingItem {
                id: setting_key_value(key),
                label: label.to_string(),
                description: Some(description.to_string()),
                current_value,
                values: values.into_iter().map(ToOwned::to_owned).collect(),
                submenu: None,
            }
        };

        let mut items = vec![settings_item(
            SettingKey::AutoCompact,
            "Auto-compact",
            "Automatically compact context when it gets too large",
            bool_value(self.cached_state.auto_compaction_enabled),
            vec!["true", "false"],
        )];

        if self.terminal_capabilities.inline_images {
            items.push(settings_item(
                SettingKey::ShowImages,
                "Show images",
                "Render images inline in terminal",
                bool_value(self.show_images),
                vec!["true", "false"],
            ));
        }

        items.push(settings_item(
            SettingKey::AutoResizeImages,
            "Auto-resize images",
            "Resize large images to 2000x2000 max for better model compatibility",
            bool_value(auto_resize_images),
            vec!["true", "false"],
        ));
        items.push(settings_item(
            SettingKey::BlockImages,
            "Block images",
            "Prevent images from being sent to LLM providers",
            bool_value(block_images),
            vec!["true", "false"],
        ));
        items.push(settings_item(
            SettingKey::SkillCommands,
            "Skill commands",
            "Register skills as /skill:name commands",
            bool_value(skill_commands),
            vec!["true", "false"],
        ));
        items.push(settings_item(
            SettingKey::ShowHardwareCursor,
            "Show hardware cursor",
            "Show the terminal cursor while still positioning it for IME support",
            bool_value(show_hardware_cursor),
            vec!["true", "false"],
        ));
        items.push(settings_item(
            SettingKey::EditorPadding,
            "Editor padding",
            "Horizontal padding for input editor (0-3)",
            editor_padding.to_string(),
            vec!["0", "1", "2", "3"],
        ));
        items.push(settings_item(
            SettingKey::AutocompleteMaxVisible,
            "Autocomplete max items",
            "Max visible items in autocomplete dropdown (3-20)",
            autocomplete_max_visible.to_string(),
            vec!["3", "5", "7", "10", "15", "20"],
        ));
        items.push(settings_item(
            SettingKey::ClearOnShrink,
            "Clear on shrink",
            "Clear empty rows when content shrinks (may cause flicker)",
            bool_value(clear_on_shrink),
            vec!["true", "false"],
        ));
        items.push(settings_item(
            SettingKey::SteeringMode,
            "Steering mode",
            "Enter while streaming queues steering messages. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.",
            queue_mode_value(self.cached_state.steering_mode),
            vec!["one-at-a-time", "all"],
        ));
        items.push(settings_item(
            SettingKey::FollowUpMode,
            "Follow-up mode",
            "Alt+Enter queues follow-up messages until agent stops. 'one-at-a-time': deliver one, wait for response. 'all': deliver all at once.",
            queue_mode_value(self.cached_state.follow_up_mode),
            vec!["one-at-a-time", "all"],
        ));
        items.push(settings_item(
            SettingKey::Transport,
            "Transport",
            "Preferred transport for providers that support multiple transports",
            transport,
            vec!["sse", "websocket", "auto"],
        ));
        items.push(settings_item(
            SettingKey::HideThinking,
            "Hide thinking",
            "Hide thinking blocks in assistant responses",
            bool_value(self.hide_thinking),
            vec!["true", "false"],
        ));
        items.push(settings_item(
            SettingKey::CollapseChangelog,
            "Collapse changelog",
            "Show condensed changelog after updates",
            bool_value(collapse_changelog),
            vec!["true", "false"],
        ));
        items.push(settings_item(
            SettingKey::QuietStartup,
            "Quiet startup",
            "Disable verbose printing at startup",
            bool_value(quiet_startup),
            vec!["true", "false"],
        ));
        items.push(settings_item(
            SettingKey::DoubleEscapeAction,
            "Double-escape action",
            "Action when pressing Escape twice with empty editor",
            self.double_escape_action.as_str().to_string(),
            vec!["tree", "fork", "none"],
        ));
        items.push(SettingItem {
            id: setting_key_value(SettingKey::ThinkingLevel),
            label: "Thinking level".to_string(),
            description: Some("Reasoning depth for thinking-capable models".to_string()),
            current_value: self.cached_state.thinking_level.clone(),
            values: Vec::new(),
            submenu: Some(SettingSubmenu {
                title: "Thinking Level".to_string(),
                description: Some("Select reasoning depth for thinking-capable models".to_string()),
                options: thinking_levels
                    .iter()
                    .map(|level| SelectItem {
                        value: level.clone(),
                        label: level.clone(),
                        description: Some(match level.as_str() {
                            "off" => "No reasoning".to_string(),
                            "minimal" => "Very brief reasoning (~1k tokens)".to_string(),
                            "low" => "Light reasoning (~2k tokens)".to_string(),
                            "medium" => "Moderate reasoning (~8k tokens)".to_string(),
                            "high" => "Deep reasoning (~16k tokens)".to_string(),
                            "xhigh" => "Maximum reasoning (~32k tokens)".to_string(),
                            _ => String::new(),
                        }),
                    })
                    .collect(),
                current_value: self.cached_state.thinking_level.clone(),
            }),
        });
        items.push(SettingItem {
            id: setting_key_value(SettingKey::Theme),
            label: "Theme".to_string(),
            description: Some("Color theme for the interface".to_string()),
            current_value: current_theme,
            values: Vec::new(),
            submenu: Some(SettingSubmenu {
                title: "Theme".to_string(),
                description: Some("Select color theme".to_string()),
                options: themes
                    .iter()
                    .map(|theme| SelectItem {
                        value: theme.clone(),
                        label: theme.clone(),
                        description: None,
                    })
                    .collect(),
                current_value: string_setting(&merged_settings, &["theme"])
                    .unwrap_or_else(|| "dark".to_string()),
            }),
        });
        Ok(items)
    }

    fn build_tree_overlay_items(
        &self,
        filter_mode: TreeFilterMode,
    ) -> Result<(Vec<SelectItem>, Vec<OverlaySelection>), String> {
        let tree = self.with_session(|session| session.get_tree())?;
        let current_leaf = self.with_session(|session| session.get_leaf_id())?;
        let mut flat = Vec::new();
        flatten_tree_items(&tree, 0, &mut flat);
        let filtered = flat
            .into_iter()
            .filter(|item| tree_item_matches_mode(item, filter_mode))
            .collect::<Vec<_>>();
        let items = filtered
            .iter()
            .map(|item| {
                let current_marker = if current_leaf.as_deref() == Some(item.entry_id.as_str()) {
                    style_warning("• ")
                } else {
                    "  ".to_string()
                };
                let prefix = if item.depth == 0 {
                    String::new()
                } else {
                    format!("{}└─ ", "   ".repeat(item.depth.saturating_sub(1)))
                };
                let mut label_text = format!(
                    "{}{}{}",
                    current_marker,
                    style_dim(&prefix),
                    truncate_to_width(&item.preview, 58)
                );
                if let Some(label) = &item.label {
                    label_text.push(' ');
                    label_text.push_str(&style_warning(&format!("[{label}]")));
                }
                SelectItem {
                    value: item.entry_id.clone(),
                    label: label_text,
                    description: Some(format!("[{}]", filter_mode.label())),
                }
            })
            .collect::<Vec<_>>();
        let selections = filtered
            .iter()
            .map(|item| OverlaySelection::Tree {
                entry_id: item.entry_id.clone(),
                label: item.label.clone(),
            })
            .collect::<Vec<_>>();
        Ok((items, selections))
    }

    fn apply_setting_value(&mut self, id: &str, value: &str) -> Result<(), String> {
        let bool_value_selected = value == "true";
        match id {
            "setting:auto_compact" => {
                self.with_session_mut(|session| {
                    session.set_auto_compaction(bool_value_selected);
                    Ok(())
                })?;
                self.persist_global_settings(
                    json!({"compaction": {"enabled": bool_value_selected}}),
                )?;
                self.refresh_snapshot()?;
                self.status = Some(format!("Auto-compact: {}", bool_value(bool_value_selected)));
            }
            "setting:steering_mode" => {
                let mode = if value == "all" {
                    QueueMode::All
                } else {
                    QueueMode::OneAtATime
                };
                self.with_session_mut(|session| {
                    session.set_steering_mode(mode);
                    Ok(())
                })?;
                self.persist_global_settings(json!({"steeringMode": queue_mode_value(mode)}))?;
                self.refresh_snapshot()?;
                self.status = Some(format!("Steering mode: {}", queue_mode_value(mode)));
            }
            "setting:follow_up_mode" => {
                let mode = if value == "all" {
                    QueueMode::All
                } else {
                    QueueMode::OneAtATime
                };
                self.with_session_mut(|session| {
                    session.set_follow_up_mode(mode);
                    Ok(())
                })?;
                self.persist_global_settings(json!({"followUpMode": queue_mode_value(mode)}))?;
                self.refresh_snapshot()?;
                self.status = Some(format!("Follow-up mode: {}", queue_mode_value(mode)));
            }
            "setting:transport" => {
                self.persist_global_settings(json!({"transport": value}))?;
                self.status = Some(format!(
                    "Saved transport: {value}. Run /reload before the next prompt."
                ));
            }
            "setting:thinking_level" => {
                self.with_session_mut(|session| {
                    session
                        .set_thinking_level(value)
                        .map_err(|error| error.to_string())
                })?;
                self.persist_global_settings(json!({"defaultThinkingLevel": value}))?;
                self.refresh_snapshot()?;
                self.status = Some(format!(
                    "Thinking level: {}",
                    self.cached_state.thinking_level
                ));
            }
            "setting:theme" => {
                self.persist_global_settings(json!({"theme": value}))?;
                self.status = Some(format!(
                    "Saved theme: {value}. Theme rendering parity is still in progress."
                ));
            }
            "setting:hide_thinking" => {
                self.hide_thinking = bool_value_selected;
                self.persist_global_settings(json!({"hideThinkingBlock": self.hide_thinking}))?;
                self.status = Some(format!("Hide thinking: {}", bool_value(self.hide_thinking)));
            }
            "setting:collapse_changelog" => {
                self.persist_global_settings(json!({"collapseChangelog": bool_value_selected}))?;
                self.status = Some(format!(
                    "Collapse changelog: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:quiet_startup" => {
                self.quiet_startup = bool_value_selected;
                self.persist_global_settings(json!({"quietStartup": bool_value_selected}))?;
                self.status = Some(format!(
                    "Quiet startup: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:show_images" => {
                self.show_images = bool_value_selected;
                self.persist_global_settings(
                    json!({"terminal": {"showImages": bool_value_selected}}),
                )?;
                self.status = Some(format!("Show images: {}", bool_value(bool_value_selected)));
            }
            "setting:auto_resize_images" => {
                self.persist_global_settings(
                    json!({"images": {"autoResize": bool_value_selected}}),
                )?;
                self.status = Some(format!(
                    "Auto-resize images: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:block_images" => {
                self.persist_global_settings(
                    json!({"images": {"blockImages": bool_value_selected}}),
                )?;
                self.status = Some(format!("Block images: {}", bool_value(bool_value_selected)));
            }
            "setting:skill_commands" => {
                self.persist_global_settings(json!({"enableSkillCommands": bool_value_selected}))?;
                self.update_prompt_autocomplete()?;
                self.status = Some(format!(
                    "Skill commands: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:show_hardware_cursor" => {
                self.persist_global_settings(json!({"showHardwareCursor": bool_value_selected}))?;
                self.status = Some(format!(
                    "Saved hardware cursor preference: {}.",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:editor_padding" => {
                let padding = value.parse::<i64>().unwrap_or(0).clamp(0, 3);
                self.persist_global_settings(json!({"editorPaddingX": padding}))?;
                self.status = Some(format!("Saved editor padding: {padding}."));
            }
            "setting:autocomplete_max_visible" => {
                let max_visible = value.parse::<i64>().unwrap_or(5).clamp(3, 20);
                self.persist_global_settings(json!({"autocompleteMaxVisible": max_visible}))?;
                self.status = Some(format!("Saved autocomplete max items: {max_visible}."));
            }
            "setting:clear_on_shrink" => {
                self.persist_global_settings(
                    json!({"terminal": {"clearOnShrink": bool_value_selected}}),
                )?;
                self.status = Some(format!(
                    "Clear on shrink: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:double_escape_action" => {
                self.double_escape_action = DoubleEscapeAction::from_settings(Some(value));
                self.persist_global_settings(json!({"doubleEscapeAction": value}))?;
                self.status = Some(format!("Double-escape action: {value}"));
            }
            _ => {
                self.status = Some(format!("Unknown setting: {id}"));
            }
        }
        Ok(())
    }

    fn persist_global_settings(&mut self, patch: Value) -> Result<(), String> {
        self.package_manager
            .settings_manager_mut()
            .update_global_settings(patch)
            .map_err(|error| error.to_string())
    }

    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::AgentStart => {
                self.cached_state.is_streaming = true;
                self.status = None;
            }
            AgentEvent::AgentEnd { .. } => {
                self.cached_state.is_streaming = false;
            }
            AgentEvent::MessageStart { message } => {
                apply_live_message(&mut self.cached_messages, message.clone());
                apply_live_transcript_message(&mut self.cached_transcript, message);
            }
            AgentEvent::MessageUpdate { message, .. } => {
                apply_live_message(&mut self.cached_messages, message.clone());
                apply_live_transcript_message(&mut self.cached_transcript, message);
            }
            AgentEvent::MessageEnd { message } => {
                apply_live_message(&mut self.cached_messages, message.clone());
                apply_live_transcript_message(&mut self.cached_transcript, message);
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                if !self
                    .active_tools
                    .iter()
                    .any(|tool| tool.tool_call_id == tool_call_id)
                {
                    self.active_tools.push(ActiveToolExecution {
                        tool_call_id,
                        tool_name,
                        args,
                        partial_result: None,
                    });
                }
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => {
                if let Some(tool) = self
                    .active_tools
                    .iter_mut()
                    .find(|tool| tool.tool_call_id == tool_call_id)
                {
                    tool.partial_result = Some(partial_result);
                }
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                self.active_tools
                    .retain(|tool| tool.tool_call_id != tool_call_id);
            }
            AgentEvent::TurnStart
            | AgentEvent::TurnEnd { .. }
            | AgentEvent::AutoCompactionStart { .. }
            | AgentEvent::AutoCompactionEnd { .. }
            | AgentEvent::AutoRetryStart { .. }
            | AgentEvent::AutoRetryEnd { .. } => {}
        }
        self.cached_state.pending_message_count = self.pending_messages.len();
    }

    fn open_external_editor(
        &mut self,
        terminal: &mut ProcessTerminal,
        renderer: &mut LineDiffRenderer,
    ) -> Result<(), String> {
        let editor_command = std::env::var("VISUAL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            });
        let Some(editor_command) = editor_command else {
            self.status = Some("Set $VISUAL or $EDITOR to use the external editor.".to_string());
            return Ok(());
        };

        let mut temp = NamedTempFile::new().map_err(|error| error.to_string())?;
        temp.write_all(self.prompt_text().as_bytes())
            .map_err(|error| error.to_string())?;
        temp.flush().map_err(|error| error.to_string())?;

        let _ = renderer.clear(terminal);
        let _ = terminal.show_cursor();
        terminal.stop().map_err(|error| error.to_string())?;

        let status = Command::new("sh")
            .arg("-lc")
            .arg("exec ${PI_RUST_EDITOR} \"$1\"")
            .arg("pi-rust-external-editor")
            .arg(temp.path())
            .env("PI_RUST_EDITOR", &editor_command)
            .status()
            .map_err(|error| format!("Failed to launch external editor: {error}"));

        terminal.start().map_err(|error| error.to_string())?;
        terminal
            .set_title("pi-rust")
            .map_err(|error| error.to_string())?;
        terminal.hide_cursor().map_err(|error| error.to_string())?;
        *renderer = LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });
        let _ = terminal.drain_input(25, 5);

        match status? {
            exit_status if exit_status.success() => {
                let edited = fs::read_to_string(temp.path()).map_err(|error| error.to_string())?;
                self.set_prompt_text(edited)?;
                self.status = Some("Loaded text from external editor.".to_string());
            }
            exit_status => {
                self.status = Some(format!("External editor exited with status {exit_status}."));
            }
        }
        Ok(())
    }

    fn with_session<T>(&self, f: impl FnOnce(&AgentSession) -> T) -> Result<T, String> {
        let guard = self
            .session
            .lock()
            .map_err(|_| "Failed to lock interactive session".to_string())?;
        Ok(f(&guard))
    }

    fn with_session_mut<T>(
        &self,
        f: impl FnOnce(&mut AgentSession) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut guard = self
            .session
            .lock()
            .map_err(|_| "Failed to lock interactive session".to_string())?;
        f(&mut guard)
    }
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

fn oauth_provider_label(provider: &str) -> &'static str {
    match provider {
        "openai-codex" => "OpenAI Codex",
        "anthropic" => "Anthropic",
        _ => "OAuth Provider",
    }
}

fn auth_source_label(source: &AuthSource) -> &'static str {
    match source {
        AuthSource::RuntimeOverride => "runtime override",
        AuthSource::StoredApiKey => "stored api key",
        AuthSource::StoredOAuth => "stored oauth",
        AuthSource::Environment => "environment",
        AuthSource::Fallback => "fallback",
        AuthSource::Missing => "missing",
    }
}

fn discover_session_records(root: &Path) -> Result<Vec<SessionRecord>, String> {
    let mut records = Vec::new();
    for path in discover_session_paths(root) {
        records.push(load_session_record(&path)?);
    }
    Ok(records)
}

fn load_session_record(path: &Path) -> Result<SessionRecord, String> {
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

fn path_modified_epoch_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(0))
        .unwrap_or(0)
}

#[cfg_attr(not(test), allow(dead_code))]
fn build_session_overlay_items(
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

fn sort_session_records(
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

fn build_model_overlay_items(
    models: &[Model],
    current_model: Option<&Model>,
) -> (Vec<SelectItem>, Vec<OverlaySelection>) {
    let models = sort_model_overlay_models(models, current_model);

    let items = models
        .iter()
        .map(|model| {
            let is_current = current_model
                .map(|current| current.provider == model.provider && current.id == model.id)
                .unwrap_or(false);
            let label = format!(
                "{} [{}]{}",
                model.id,
                model.provider.0,
                if is_current { " ✓" } else { "" }
            );
            let description = format!(
                "{} · {} · {} ctx",
                model.name,
                if model.reasoning { "reasoning" } else { "text" },
                format_token_count(model.context_window as u64)
            );
            SelectItem {
                value: format!("{}/{}", model.provider.0, model.id),
                label,
                description: Some(description),
            }
        })
        .collect::<Vec<_>>();
    let selections = models
        .iter()
        .map(|model| OverlaySelection::Model {
            provider: model.provider.0.clone(),
            model_id: model.id.clone(),
        })
        .collect::<Vec<_>>();
    (items, selections)
}

fn update_model_overlay_metadata(
    overlay: &mut SearchOverlay,
    available_count: usize,
    scoped_count: usize,
    current_model: Option<&Model>,
    scope: ModelOverlayScope,
) {
    overlay.set_title("Model Selector");
    let current = current_model
        .map(|model| format!("{}/{}", model.provider.0, model.id))
        .unwrap_or_else(|| "no-model".to_string());
    let selected = overlay
        .selected_item()
        .map(|item| truncate_to_width(&item.label.replace('\n', " "), 48));
    let detail = overlay
        .selected_item()
        .and_then(|item| item.description.clone());
    let subtitle = if scoped_count > 0 {
        format!(
            "scope {} · {} all · {} scoped\ncurrent {}{}",
            scope.label(),
            available_count,
            scoped_count,
            current,
            selected
                .map(|value| format!(" · {value}"))
                .unwrap_or_default()
        )
    } else {
        format!(
            "{} available\ncurrent {}{}",
            available_count,
            current,
            selected
                .map(|value| format!(" · {value}"))
                .unwrap_or_default()
        )
    };
    overlay.set_subtitle(subtitle);
    overlay.set_detail(detail);
    overlay.set_hint(if scoped_count > 0 {
        "Tab toggles scope · Enter selects · Search filters id/provider/name · Esc cancels"
    } else {
        "Enter selects · Search filters id/provider/name · Esc cancels"
    });
}

fn sort_model_overlay_models(models: &[Model], current_model: Option<&Model>) -> Vec<Model> {
    let mut models = models.to_vec();
    models.sort_by(|left, right| {
        let left_current = current_model
            .map(|current| current.provider == left.provider && current.id == left.id)
            .unwrap_or(false);
        let right_current = current_model
            .map(|current| current.provider == right.provider && current.id == right.id)
            .unwrap_or(false);
        right_current
            .cmp(&left_current)
            .then(left.provider.0.cmp(&right.provider.0))
            .then(left.id.cmp(&right.id))
    });
    models
}

fn model_full_id(model: &Model) -> String {
    format!("{}/{}", model.provider.0, model.id)
}

fn build_scoped_model_items(models: &[Model], enabled_ids: Option<&[String]>) -> Vec<SelectItem> {
    models
        .iter()
        .map(|model| {
            let full_id = model_full_id(model);
            let enabled = enabled_ids
                .map(|ids| ids.iter().any(|id| id == &full_id))
                .unwrap_or(true);
            SelectItem {
                value: full_id,
                label: format!(
                    "{} [{}]{}",
                    model.id,
                    model.provider.0,
                    if enabled { " ✓" } else { " ✗" }
                ),
                description: Some(model.name.clone()),
            }
        })
        .collect()
}

fn toggle_scoped_model(state: &mut ScopedModelsOverlayState, model_id: &str) {
    match state.enabled_ids.as_mut() {
        None => {
            state.enabled_ids = Some(vec![model_id.to_string()]);
        }
        Some(enabled_ids) => {
            if let Some(index) = enabled_ids.iter().position(|id| id == model_id) {
                enabled_ids.remove(index);
            } else {
                enabled_ids.push(model_id.to_string());
            }
        }
    }
}

fn toggle_scoped_models_provider(state: &mut ScopedModelsOverlayState, selected_value: &str) {
    let Some(selected_model) = state
        .models
        .iter()
        .find(|model| model_full_id(model) == selected_value)
    else {
        return;
    };
    let provider = selected_model.provider.0.as_str();
    let provider_ids = state
        .models
        .iter()
        .filter(|model| model.provider.0 == provider)
        .map(model_full_id)
        .collect::<Vec<_>>();
    let all_provider_enabled = provider_ids.iter().all(|id| {
        state
            .enabled_ids
            .as_ref()
            .is_none_or(|ids| ids.iter().any(|value| value == id))
    });
    if all_provider_enabled {
        let mut next = state
            .enabled_ids
            .clone()
            .unwrap_or_else(|| state.models.iter().map(model_full_id).collect());
        next.retain(|id| !provider_ids.iter().any(|provider_id| provider_id == id));
        state.enabled_ids = Some(next);
    } else {
        let mut next = state.enabled_ids.clone().unwrap_or_default();
        for id in provider_ids {
            if !next.iter().any(|existing| existing == &id) {
                next.push(id);
            }
        }
        if next.len() == state.models.len() {
            state.enabled_ids = None;
        } else {
            state.enabled_ids = Some(next);
        }
    }
}

fn move_scoped_model_selection(
    state: &mut ScopedModelsOverlayState,
    selected_value: &str,
    delta: isize,
) -> bool {
    let enabled_ids = state
        .enabled_ids
        .clone()
        .unwrap_or_else(|| state.models.iter().map(model_full_id).collect::<Vec<_>>());
    let Some(index) = enabled_ids.iter().position(|id| id == selected_value) else {
        return false;
    };
    let next_index = index as isize + delta;
    if next_index < 0 || next_index >= enabled_ids.len() as isize {
        return false;
    }
    let mut next = enabled_ids;
    next.swap(index, next_index as usize);
    state.enabled_ids = Some(next);
    true
}

fn build_session_overlay_rows(
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

fn session_overlay_rows_to_items(
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

fn append_rule_line(target: &mut Vec<RenderedLine>, width: u16) {
    if width == 0 {
        return;
    }
    target.push(RenderedLine::Text(style_border(
        &"─".repeat(width as usize),
    )));
}

fn select_list_visible_bounds(list: &SelectList) -> (usize, usize) {
    let filtered_len = list.filtered_indices().len();
    let start = list
        .selected_index()
        .saturating_sub(list.max_visible().saturating_sub(1) / 2);
    let end = (start + list.max_visible()).min(filtered_len);
    (start, end)
}

fn model_overlay_scope_line(state: &ModelOverlayState) -> String {
    if state.scoped_count == 0 {
        return style_warning(
            "Only showing models with configured API keys (see README for details)",
        );
    }
    format!(
        "{}{}{}{}",
        style_hint("Scope: "),
        if matches!(state.scope, ModelOverlayScope::All) {
            style_brand("all")
        } else {
            style_hint("all")
        },
        style_hint(" | "),
        if matches!(state.scope, ModelOverlayScope::Scoped) {
            style_brand("scoped")
        } else {
            style_hint("scoped")
        }
    )
}

fn selected_model_overlay_model(state: &ModelOverlayState) -> Option<&Model> {
    state
        .overlay
        .list
        .filtered_indices()
        .get(state.overlay.list.selected_index())
        .and_then(|index| state.models.get(*index))
}

fn append_model_overlay_rows(
    target: &mut Vec<RenderedLine>,
    state: &ModelOverlayState,
    width: usize,
) {
    let filtered_indices = state.overlay.list.filtered_indices();
    if filtered_indices.is_empty() {
        target.push(RenderedLine::Text(fit_line(
            &style_hint("  No matching models"),
            width as u16,
        )));
        return;
    }

    let selected_index = state.overlay.list.selected_index();
    let (start, end) = select_list_visible_bounds(&state.overlay.list);
    for visible_index in start..end {
        let Some(model) = filtered_indices
            .get(visible_index)
            .and_then(|index| state.models.get(*index))
        else {
            continue;
        };
        let is_selected = visible_index == selected_index;
        let is_current = state
            .current_model
            .as_ref()
            .is_some_and(|current| current.provider == model.provider && current.id == model.id);
        let prefix = if is_selected {
            style_brand("→ ")
        } else {
            "  ".to_string()
        };
        let available = width
            .saturating_sub(visible_width(&prefix))
            .saturating_sub(visible_width(" [] ✓"));
        let model_id = truncate_to_width(&model.id, available.max(16));
        let model_text = if is_selected {
            style_brand(&model_id)
        } else {
            model_id
        };
        let provider_badge = style_hint(&format!("[{}]", model.provider.0));
        let checkmark = if is_current {
            style_success(" ✓")
        } else {
            String::new()
        };
        target.push(RenderedLine::Text(fit_line(
            &format!("{prefix}{model_text} {provider_badge}{checkmark}"),
            width as u16,
        )));
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

fn session_overlay_header_line(state: &SessionOverlayState, width: usize) -> String {
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

fn session_overlay_hint_line_one(state: &SessionOverlayState, width: usize) -> String {
    if state.confirming_delete.is_some() {
        return truncate_to_width(
            &style_error("Delete session? [Enter] confirm · [Esc/Ctrl+C] cancel"),
            width,
        );
    }
    let first = state.overlay.hint.lines().next().unwrap_or_default();
    truncate_to_width(&style_hint(first), width)
}

fn session_overlay_hint_line_two(state: &SessionOverlayState, width: usize) -> String {
    if state.confirming_delete.is_some() {
        return String::new();
    }
    let second = state.overlay.hint.lines().nth(1).unwrap_or_default();
    truncate_to_width(&style_hint(second), width)
}

fn update_session_overlay_metadata_with_options(
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

fn session_scope_root(
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

fn append_session_overlay_rows(
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

fn append_fork_overlay_rows(
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

fn fork_message_preview_text(text: &str) -> String {
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

fn append_scoped_models_overlay_rows(
    target: &mut Vec<RenderedLine>,
    state: &ScopedModelsOverlayState,
    width: usize,
) {
    let filtered_indices = state.overlay.list.filtered_indices();
    if filtered_indices.is_empty() {
        target.push(RenderedLine::Text(fit_line(
            &style_hint("  No matching models"),
            width as u16,
        )));
        return;
    }

    let selected_index = state.overlay.list.selected_index();
    let (start, end) = select_list_visible_bounds(&state.overlay.list);
    let all_enabled = state.enabled_ids.is_none();

    for visible_index in start..end {
        let Some(model) = filtered_indices
            .get(visible_index)
            .and_then(|index| state.models.get(*index))
        else {
            continue;
        };
        let is_selected = visible_index == selected_index;
        let full_id = model_full_id(model);
        let enabled = state
            .enabled_ids
            .as_ref()
            .map(|ids| ids.iter().any(|id| id == &full_id))
            .unwrap_or(true);
        let prefix = if is_selected {
            style_brand("→ ")
        } else {
            "  ".to_string()
        };
        let model_text = if is_selected {
            style_brand(&model.id)
        } else {
            model.id.clone()
        };
        let provider_badge = style_hint(&format!("[{}]", model.provider.0));
        let status = if all_enabled {
            String::new()
        } else if enabled {
            style_success(" ✓")
        } else {
            style_hint(" ✗")
        };
        target.push(RenderedLine::Text(fit_line(
            &format!("{prefix}{model_text} {provider_badge}{status}"),
            width as u16,
        )));
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

    if let Some(model) = filtered_indices
        .get(selected_index)
        .and_then(|index| state.models.get(*index))
    {
        target.push(RenderedLine::Text(String::new()));
        target.push(RenderedLine::Text(fit_line(
            &style_hint(&format!("  Model Name: {}", model.name)),
            width as u16,
        )));
    }
}

fn scoped_models_footer_text(state: &ScopedModelsOverlayState) -> String {
    let enabled_count = state
        .enabled_ids
        .as_ref()
        .map_or(state.models.len(), Vec::len);
    let count_text = if state.enabled_ids.is_none() {
        "all enabled".to_string()
    } else {
        format!("{enabled_count}/{} enabled", state.models.len())
    };
    let base = format!(
        "  Enter toggle · Ctrl+A all · Ctrl+X clear · Ctrl+P provider · Alt+Up/Down reorder · Ctrl+S save · {count_text}"
    );
    if state.dirty {
        format!("{base} {}", style_warning("(unsaved)"))
    } else {
        base
    }
}

fn session_tree_prefix(row: &SessionOverlayRow) -> String {
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

fn session_record_search_text(record: &SessionRecord) -> String {
    format!(
        "{} {} {} {}",
        record.name.as_deref().unwrap_or_default(),
        record.preview,
        record.path.to_string_lossy(),
        record.cwd.to_string_lossy()
    )
}

fn normalize_whitespace_lower(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_session_search_query(query: &str) -> ParsedSessionSearchQuery {
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

fn session_record_match_score(
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

fn fuzzy_text_score(query: &str, text: &str) -> Option<i64> {
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

fn flatten_session_record_tree(records: Vec<SessionRecord>) -> Vec<SessionOverlayRow> {
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

fn format_relative_age(epoch_ms: i64) -> String {
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

fn split_prompt_lines(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized
        .split('\n')
        .map(str::to_string)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn build_prompt_autocomplete<F>(
    text: &str,
    cursor: (usize, usize),
    session_commands: &[RpcSlashCommand],
    load_models: F,
    cwd: &Path,
    force_file_completion: bool,
) -> Result<Option<PromptAutocompleteState>, String>
where
    F: FnOnce() -> Result<Vec<Model>, String>,
{
    let lines = split_prompt_lines(text);
    let (cursor_line, cursor_col) = cursor;
    if cursor_line >= lines.len() {
        return Ok(None);
    }
    let current_line = &lines[cursor_line];
    if cursor_col > current_line.len() {
        return Ok(None);
    }

    let before_cursor = &current_line[..cursor_col];
    let trimmed_before = before_cursor.trim_start();

    if trimmed_before.starts_with('/') && cursor_line > 0 && !force_file_completion {
        return Ok(None);
    }

    if cursor_line == 0 && trimmed_before.starts_with('/') {
        if let Some(model_filter) = trimmed_before.strip_prefix("/model ") {
            let models = load_models()?;
            if models.is_empty() {
                return Ok(None);
            }
            let (items, _) = build_model_overlay_items(&models, None);
            let mut list = SelectList::new(items, 8);
            if !model_filter.trim().is_empty() {
                list.set_filter(model_filter.trim());
            }
            return Ok(Some(PromptAutocompleteState {
                kind: PromptAutocompleteKind::ModelArgument,
                title: "Models".to_string(),
                subtitle: "Complete /model with an available model".to_string(),
                hint: "Tab inserts · Enter inserts and runs · Esc dismisses".to_string(),
                replace_prefix: model_filter.to_string(),
                list,
            }));
        }

        if !trimmed_before[1..].contains(' ') {
            let filter = &trimmed_before[1..];
            let mut list = SelectList::new(build_prompt_command_items(session_commands), 8);
            if !filter.is_empty() {
                list.set_filter(filter);
            }
            return Ok(Some(PromptAutocompleteState {
                kind: PromptAutocompleteKind::SlashCommand,
                title: "Commands".to_string(),
                subtitle: "Type a slash command and press Enter to run it".to_string(),
                hint: "Tab inserts · Enter runs · Esc dismisses".to_string(),
                replace_prefix: trimmed_before.to_string(),
                list,
            }));
        }
    }

    if let Some(prefix) = extract_at_prefix(before_cursor) {
        let items = build_fuzzy_file_reference_items(cwd, &prefix);
        if !items.is_empty() {
            return Ok(Some(PromptAutocompleteState {
                kind: PromptAutocompleteKind::FileReference,
                title: "Files".to_string(),
                subtitle: "Attach a file reference from the workspace".to_string(),
                hint: "Tab inserts · Enter inserts and runs · Esc dismisses".to_string(),
                replace_prefix: prefix,
                list: SelectList::new(items, 8),
            }));
        }
    }

    let path_prefix = extract_path_prefix(before_cursor, force_file_completion);
    if let Some(prefix) = path_prefix {
        let items = build_path_completion_items(cwd, &prefix);
        if !items.is_empty() {
            return Ok(Some(PromptAutocompleteState {
                kind: PromptAutocompleteKind::Path,
                title: "Paths".to_string(),
                subtitle: "Complete a path from the current workspace".to_string(),
                hint: "Tab inserts · Esc dismisses".to_string(),
                replace_prefix: prefix,
                list: SelectList::new(items, 8),
            }));
        }
    }

    Ok(None)
}

fn prompt_autocomplete_should_submit_current_prompt(
    autocomplete: Option<&PromptAutocompleteState>,
    text: &str,
    cursor: (usize, usize),
) -> bool {
    let Some(autocomplete) = autocomplete else {
        return false;
    };
    let lines = split_prompt_lines(text);
    let (cursor_line, _) = cursor;
    let Some(current_line) = lines.get(cursor_line) else {
        return false;
    };
    let trimmed = current_line.trim();
    if trimmed.is_empty() {
        return false;
    }
    match autocomplete.kind {
        PromptAutocompleteKind::SlashCommand => autocomplete.list.contains_value(trimmed),
        PromptAutocompleteKind::ModelArgument => trimmed
            .strip_prefix("/model ")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|value| autocomplete.list.contains_value(value)),
        PromptAutocompleteKind::Path | PromptAutocompleteKind::FileReference => false,
    }
}

fn build_prompt_command_items(session_commands: &[RpcSlashCommand]) -> Vec<SelectItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for command in BUILTIN_SLASH_COMMANDS {
        let value = format!("/{}", command.name);
        if seen.insert(value.clone()) {
            items.push(SelectItem {
                value: value.clone(),
                label: value,
                description: Some(command.description.to_string()),
            });
        }
    }
    for command in session_commands {
        let value = format!("/{}", command.name);
        if seen.insert(value.clone()) {
            items.push(SelectItem {
                value: value.clone(),
                label: value,
                description: Some(format_dynamic_command_description(command)),
            });
        }
    }
    items
}

fn format_dynamic_command_description(command: &RpcSlashCommand) -> String {
    let mut segments = vec![match command.source {
        RpcCommandSource::Prompt => "Prompt template".to_string(),
        RpcCommandSource::Skill => "Skill".to_string(),
        RpcCommandSource::Extension => "Extension".to_string(),
    }];
    if let Some(description) = command
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        segments.push(description.to_string());
    }
    if let Some(location) = command.location {
        segments.push(match location {
            RpcCommandLocation::User => "(user)".to_string(),
            RpcCommandLocation::Project => "(project)".to_string(),
            RpcCommandLocation::Path => "(path)".to_string(),
        });
    }
    segments.join(" · ")
}

fn find_last_delimiter(text: &str) -> Option<usize> {
    text.char_indices()
        .rev()
        .find_map(|(index, character)| PATH_DELIMITERS.contains(&character).then_some(index))
}

fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut quote_start = None;
    for (index, character) in text.char_indices() {
        if character == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = Some(index);
            }
        }
    }
    if in_quotes { quote_start } else { None }
}

fn is_token_start(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    text[..index]
        .chars()
        .next_back()
        .is_some_and(|character| PATH_DELIMITERS.contains(&character))
}

fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;
    if quote_start > 0 && text[..quote_start].ends_with('@') {
        let at_index = quote_start.saturating_sub(1);
        if !is_token_start(text, at_index) {
            return None;
        }
        return Some(text[at_index..].to_string());
    }
    if !is_token_start(text, quote_start) {
        return None;
    }
    Some(text[quote_start..].to_string())
}

#[derive(Clone, Debug)]
struct ParsedPathPrefix {
    raw_prefix: String,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

fn parse_path_prefix(prefix: &str) -> ParsedPathPrefix {
    if let Some(value) = prefix.strip_prefix("@\"") {
        return ParsedPathPrefix {
            raw_prefix: value.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(value) = prefix.strip_prefix('"') {
        return ParsedPathPrefix {
            raw_prefix: value.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(value) = prefix.strip_prefix('@') {
        return ParsedPathPrefix {
            raw_prefix: value.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    ParsedPathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

fn build_completion_value(path: &str, is_at_prefix: bool, is_quoted_prefix: bool) -> String {
    let needs_quotes = is_quoted_prefix || path.contains(' ');
    let prefix = if is_at_prefix { "@" } else { "" };
    if !needs_quotes {
        return format!("{prefix}{path}");
    }
    format!(r#"{prefix}"{path}""#)
}

fn extract_at_prefix(text: &str) -> Option<String> {
    if let Some(prefix) = extract_quoted_prefix(text)
        && prefix.starts_with("@\"")
    {
        return Some(prefix);
    }
    let token_start = find_last_delimiter(text).map_or(0, |index| index + 1);
    text[token_start..]
        .starts_with('@')
        .then_some(text[token_start..].to_string())
}

fn extract_path_prefix(text: &str, force_extract: bool) -> Option<String> {
    if let Some(prefix) = extract_quoted_prefix(text) {
        return Some(prefix);
    }
    let token_start = find_last_delimiter(text).map_or(0, |index| index + 1);
    let path_prefix = text[token_start..].to_string();
    if force_extract {
        let trimmed = text.trim_start();
        if trimmed.starts_with('/') && !trimmed.contains(' ') {
            return None;
        }
        return Some(path_prefix);
    }
    if path_prefix.contains('/') || path_prefix.starts_with('.') || path_prefix.starts_with("~/") {
        return Some(path_prefix);
    }
    None
}

fn expand_home_path(path: &str) -> String {
    if path == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| path.to_string());
    }
    if let Some(value) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Path::new(&home).join(value).to_string_lossy().to_string();
    }
    path.to_string()
}

fn keep_completion_walkdir_entry(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !name.starts_with('.') && name != "node_modules" && name != ".git"
}

fn build_path_completion_items(cwd: &Path, prefix: &str) -> Vec<SelectItem> {
    let parsed = parse_path_prefix(prefix);
    let mut expanded_prefix = parsed.raw_prefix.clone();
    if expanded_prefix.starts_with('~') {
        expanded_prefix = expand_home_path(&expanded_prefix);
    }

    let is_root_prefix = parsed.raw_prefix.is_empty()
        || matches!(parsed.raw_prefix.as_str(), "./" | "../" | "~" | "~/" | "/");
    let (search_dir, display_prefix, search_prefix) = if is_root_prefix {
        let search_dir = if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
            PathBuf::from(&expanded_prefix)
        } else {
            cwd.join(&expanded_prefix)
        };
        (search_dir, parsed.raw_prefix.clone(), String::new())
    } else if parsed.raw_prefix.ends_with('/') {
        let search_dir = if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
            PathBuf::from(&expanded_prefix)
        } else {
            cwd.join(&expanded_prefix)
        };
        (search_dir, parsed.raw_prefix.clone(), String::new())
    } else {
        let expanded_path = Path::new(&expanded_prefix);
        let search_dir = if parsed.raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
            expanded_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        } else {
            cwd.join(expanded_path.parent().unwrap_or_else(|| Path::new(".")))
        };
        let display_prefix = Path::new(&parsed.raw_prefix)
            .parent()
            .map(|path| {
                let value = path.to_string_lossy().to_string();
                if value == "." { String::new() } else { value }
            })
            .unwrap_or_default();
        let search_prefix = Path::new(&parsed.raw_prefix)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_string();
        (search_dir, display_prefix, search_prefix)
    };

    let Ok(entries) = fs::read_dir(&search_dir) else {
        return Vec::new();
    };

    let mut suggestions = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" || name == ".git" {
            continue;
        }
        if !name
            .to_lowercase()
            .starts_with(&search_prefix.to_lowercase())
        {
            continue;
        }
        let file_type = entry.file_type().ok();
        let is_directory = file_type.as_ref().is_some_and(|value| value.is_dir());
        let relative_path = if display_prefix.is_empty() {
            name.clone()
        } else if display_prefix.ends_with('/') {
            format!("{display_prefix}{name}")
        } else {
            format!("{display_prefix}/{name}")
        };
        let path_value = if is_directory {
            format!("{relative_path}/")
        } else {
            relative_path
        };
        suggestions.push(SelectItem {
            value: build_completion_value(
                &path_value,
                parsed.is_at_prefix,
                parsed.is_quoted_prefix,
            ),
            label: format!("{name}{}", if is_directory { "/" } else { "" }),
            description: None,
        });
    }
    suggestions.sort_by(|left, right| {
        let left_is_dir = left.label.ends_with('/');
        let right_is_dir = right.label.ends_with('/');
        right_is_dir
            .cmp(&left_is_dir)
            .then(left.label.to_lowercase().cmp(&right.label.to_lowercase()))
    });
    suggestions
}

fn build_fuzzy_file_reference_items(cwd: &Path, prefix: &str) -> Vec<SelectItem> {
    let parsed = parse_path_prefix(prefix);
    let scoped = resolve_scoped_fuzzy_query(cwd, &parsed.raw_prefix);
    let (base_dir, display_base, query) = match scoped {
        Some((base_dir, display_base, query)) => (base_dir, display_base, query),
        None => (cwd.to_path_buf(), String::new(), parsed.raw_prefix.clone()),
    };
    if !base_dir.exists() {
        return Vec::new();
    }

    let mut scored = Vec::new();
    for entry in WalkDir::new(&base_dir)
        .follow_links(true)
        .into_iter()
        .filter_entry(keep_completion_walkdir_entry)
        .filter_map(Result::ok)
        .take(2000)
    {
        if entry.path() == base_dir {
            continue;
        }
        let is_directory = entry.file_type().is_dir();
        let relative = entry
            .path()
            .strip_prefix(&base_dir)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| entry.path().to_string_lossy().replace('\\', "/"));
        let display_path = if display_base.is_empty() {
            relative
        } else if display_base.ends_with('/') {
            format!("{display_base}{relative}")
        } else {
            format!("{display_base}/{relative}")
        };
        let score = fuzzy_file_score(&display_path, &query, is_directory);
        if score <= 0 {
            continue;
        }
        scored.push((score, display_path, is_directory));
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    scored.truncate(20);
    scored
        .into_iter()
        .map(|(_, display_path, is_directory)| {
            let entry_name = Path::new(&display_path)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(display_path.as_str())
                .to_string();
            let path_value = if is_directory {
                format!("{display_path}/")
            } else {
                display_path.clone()
            };
            SelectItem {
                value: build_completion_value(&path_value, true, parsed.is_quoted_prefix),
                label: format!("{entry_name}{}", if is_directory { "/" } else { "" }),
                description: Some(display_path),
            }
        })
        .collect()
}

fn resolve_scoped_fuzzy_query(cwd: &Path, raw_query: &str) -> Option<(PathBuf, String, String)> {
    let slash_index = raw_query.rfind('/')?;
    let display_base = raw_query[..=slash_index].to_string();
    let query = raw_query[slash_index + 1..].to_string();
    let base_dir = if display_base.starts_with("~/") {
        PathBuf::from(expand_home_path(&display_base))
    } else if display_base.starts_with('/') {
        PathBuf::from(&display_base)
    } else {
        cwd.join(&display_base)
    };
    base_dir.is_dir().then_some((base_dir, display_base, query))
}

fn fuzzy_file_score(path: &str, query: &str, is_directory: bool) -> i32 {
    if query.is_empty() {
        return if is_directory { 11 } else { 1 };
    }
    let filename = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(path)
        .to_lowercase();
    let lower_query = query.to_lowercase();
    let lower_path = path.to_lowercase();
    let mut score = if filename == lower_query {
        100
    } else if filename.starts_with(&lower_query) {
        80
    } else if filename.contains(&lower_query) {
        50
    } else if lower_path.contains(&lower_query) {
        30
    } else {
        0
    };
    if is_directory && score > 0 {
        score += 10;
    }
    score
}

fn clip_render_output_to_height(mut output: RenderOutput, height: u16) -> RenderOutput {
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

fn append_overlay_banner(target: &mut RenderOutput, title: &str, subtitle: &str, width: u16) {
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

fn render_search_overlay_shell(
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

#[cfg(test)]
fn build_config_items(package_manager: &PackageManager) -> Vec<SelectItem> {
    build_config_entries(package_manager)
        .into_iter()
        .map(|entry| entry.item)
        .collect()
}

fn build_config_entries(package_manager: &PackageManager) -> Vec<ConfigEntry> {
    let global_settings_path = package_manager.settings_manager().global_settings_path();
    let project_settings_path = package_manager.settings_manager().project_settings_path();
    let all_packages = package_manager.list_all();
    let mut entries = vec![
        ConfigEntry {
            item: SelectItem {
                value: "config:global-settings".to_string(),
                label: "Global settings".to_string(),
                description: None,
            },
            selection: ConfigSelection::GlobalSettings {
                path: global_settings_path.to_path_buf(),
            },
        },
        ConfigEntry {
            item: SelectItem {
                value: "config:project-settings".to_string(),
                label: "Project settings".to_string(),
                description: None,
            },
            selection: ConfigSelection::ProjectSettings {
                path: project_settings_path.to_path_buf(),
            },
        },
    ];

    for package in package_manager.list_by_scope(PackageInstallScope::User) {
        entries.push(package_entry("User", package));
    }
    for package in package_manager.list_by_scope(PackageInstallScope::Project) {
        entries.push(package_entry("Project", package));
    }

    entries.extend(build_resource_entries(package_manager, &all_packages));

    entries
}

fn package_entry(scope_label: &str, package: InstalledPackage) -> ConfigEntry {
    ConfigEntry {
        item: SelectItem {
            value: format!("package:{:?}:{}", package.scope, package.identity),
            label: format!("{scope_label}: {}", package.source),
            description: None,
        },
        selection: ConfigSelection::Package {
            source: package.source,
            scope: package.scope,
            install_path: package.install_path,
        },
    }
}

fn find_config_selection<'a>(
    entries: &'a [ConfigEntry],
    value: &str,
) -> Option<&'a ConfigSelection> {
    entries
        .iter()
        .find(|entry| entry.item.value == value)
        .map(|entry| &entry.selection)
}

fn config_selection_status(selection: &ConfigSelection) -> String {
    match selection {
        ConfigSelection::GlobalSettings { path } => {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("settings");
            format!("Global settings · {name}")
        }
        ConfigSelection::ProjectSettings { path } => {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("settings");
            format!("Project settings · {name}")
        }
        ConfigSelection::Package { source, scope, .. } => {
            format!(
                "{} package {}",
                match scope {
                    PackageInstallScope::User => "User",
                    PackageInstallScope::Project => "Project",
                    PackageInstallScope::Temporary => "Temporary",
                },
                source
            )
        }
        ConfigSelection::Resource {
            kind,
            scope,
            path,
            owner,
        } => {
            let scope_label = match scope {
                ResourceScope::Global => "Global",
                ResourceScope::Project => "Project",
            };
            let owner_label = owner
                .as_ref()
                .map(|value| format!(" · {value}"))
                .unwrap_or_default();
            format!(
                "{scope_label} {kind} · {}{owner_label}",
                shorten_home_path(&path.to_string_lossy())
            )
        }
    }
}

fn build_resource_entries(
    package_manager: &PackageManager,
    packages: &[InstalledPackage],
) -> Vec<ConfigEntry> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let package_roots = package_manager
        .resource_roots()
        .into_iter()
        .map(|(scope, path)| ScopedPath {
            scope: resource_scope_from_package_scope(scope),
            path,
        })
        .collect::<Vec<_>>();
    let discovered = discover_resources_with_options(&ResourceDiscoveryOptions {
        cwd,
        agent_dir: Some(package_manager.agent_dir().to_path_buf()),
        settings_manager: Some(package_manager.settings_manager().clone()),
        package_roots,
        ..ResourceDiscoveryOptions::default()
    });

    let mut entries = Vec::new();
    append_resource_entries(&mut entries, "Skill", &discovered.skills, packages);
    append_resource_entries(&mut entries, "Prompt", &discovered.prompts, packages);
    append_resource_entries(&mut entries, "Theme", &discovered.themes, packages);
    entries.sort_by(|left, right| left.item.label.cmp(&right.item.label));
    entries
}

fn append_resource_entries(
    target: &mut Vec<ConfigEntry>,
    kind: &'static str,
    resources: &[ScopedPath],
    packages: &[InstalledPackage],
) {
    for resource in resources {
        let owner = resource_owner_label(&resource.path, packages);
        let scope_label = match resource.scope {
            ResourceScope::Global => "Global",
            ResourceScope::Project => "Project",
        };
        let display_name = resource
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .or_else(|| resource.path.file_name().and_then(|value| value.to_str()))
            .unwrap_or("resource");
        let owner_suffix = owner
            .as_ref()
            .map(|value| format!(" · {value}"))
            .unwrap_or_default();
        target.push(ConfigEntry {
            item: SelectItem {
                value: format!("resource:{kind}:{}", resource.path.to_string_lossy()),
                label: format!("[x] {scope_label} {kind} · {display_name}{owner_suffix}"),
                description: Some(shorten_home_path(&resource.path.to_string_lossy())),
            },
            selection: ConfigSelection::Resource {
                kind,
                scope: resource.scope,
                path: resource.path.clone(),
                owner,
            },
        });
    }
}

fn resource_owner_label(path: &Path, packages: &[InstalledPackage]) -> Option<String> {
    packages
        .iter()
        .find(|package| path.starts_with(&package.install_path))
        .map(|package| {
            let scope = match package.scope {
                PackageInstallScope::User => "User package",
                PackageInstallScope::Project => "Project package",
                PackageInstallScope::Temporary => "Temporary package",
            };
            format!("{scope} {}", package.source)
        })
}

fn resource_scope_from_package_scope(scope: PackageInstallScope) -> ResourceScope {
    match scope {
        PackageInstallScope::User => ResourceScope::Global,
        PackageInstallScope::Project | PackageInstallScope::Temporary => ResourceScope::Project,
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

fn build_transcript_entries(session: &AgentSession) -> Vec<TranscriptEntry> {
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

fn session_transcript_lines(
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

fn session_selection_detail(record: &SessionRecord, is_current: bool) -> String {
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

fn render_transcript_entry(
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
struct ParsedSkillBlock {
    name: String,
    location: String,
    content: String,
    user_message: Option<String>,
}

fn parse_skill_block(text: &str) -> Option<ParsedSkillBlock> {
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

fn shorten_home_path(path: &str) -> String {
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

fn collect_numbered_code_lines(lines: &[String], start_line: usize, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for (index, raw_line) in lines.iter().enumerate() {
        let line_no = start_line + index;
        let prefix = format!("{line_no:>4} ");
        let available = width.saturating_sub(prefix.len()).max(1);
        let content = if raw_line.is_empty() {
            String::new()
        } else {
            truncate_to_width(raw_line, available)
        };
        out.push(style_code_block_line(&format!("{prefix}{content}")));
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

fn collect_diff_lines(diff: &str, width: usize) -> Vec<String> {
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

fn apply_persistent_style(code: &str, text: &str) -> String {
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

fn latest_active_tool_panel_id(tools: &[ActiveToolExecution]) -> Option<String> {
    tools.last().map(active_tool_panel_id)
}

fn latest_transcript_tool_panel_id(entries: &[TranscriptEntry]) -> Option<String> {
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

fn active_tool_render_lines(
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

fn content_text(content: &UserContent) -> String {
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

fn custom_message_label(custom_type: &str) -> String {
    match custom_type {
        "bash_execution" => "Bash".to_string(),
        _ => custom_type
            .split('_')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => {
                        format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
                    }
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[derive(Clone, Debug)]
struct TreeListItem {
    entry_id: String,
    entry_type: String,
    message_role: Option<String>,
    assistant_tool_only: bool,
    preview: String,
    search_text: String,
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
        let search_text = format!("{} {}", preview, node.label.clone().unwrap_or_default());
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
            search_text,
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
        return truncate_to_width(&parts.join(" "), 80);
    }
    truncate_to_width(&value.to_string(), 80)
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

fn next_queue_mode(mode: QueueMode) -> QueueMode {
    match mode {
        QueueMode::All => QueueMode::OneAtATime,
        QueueMode::OneAtATime => QueueMode::All,
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

fn next_thinking_level(state: &RpcSessionState) -> Result<String, String> {
    let levels = if state.model.as_ref().is_some_and(|model| model.reasoning) {
        if state.model.as_ref().is_some_and(supports_xhigh) {
            vec!["off", "minimal", "low", "medium", "high", "xhigh"]
        } else {
            vec!["off", "minimal", "low", "medium", "high"]
        }
    } else {
        vec!["off"]
    };
    let current_index = levels
        .iter()
        .position(|level| *level == state.thinking_level)
        .unwrap_or(0);
    Ok(levels[(current_index + 1) % levels.len()].to_string())
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

fn setting_item(
    key: SettingKey,
    label: &str,
    current_value: String,
    description: &str,
) -> SelectItem {
    SelectItem {
        value: setting_key_value(key),
        label: format!("{label}: {current_value}"),
        description: Some(description.to_string()),
    }
}

fn setting_key_value(key: SettingKey) -> String {
    match key {
        SettingKey::AutoCompact => "setting:auto_compact",
        SettingKey::SteeringMode => "setting:steering_mode",
        SettingKey::FollowUpMode => "setting:follow_up_mode",
        SettingKey::Transport => "setting:transport",
        SettingKey::ThinkingLevel => "setting:thinking_level",
        SettingKey::Theme => "setting:theme",
        SettingKey::HideThinking => "setting:hide_thinking",
        SettingKey::CollapseChangelog => "setting:collapse_changelog",
        SettingKey::QuietStartup => "setting:quiet_startup",
        SettingKey::ShowImages => "setting:show_images",
        SettingKey::AutoResizeImages => "setting:auto_resize_images",
        SettingKey::BlockImages => "setting:block_images",
        SettingKey::SkillCommands => "setting:skill_commands",
        SettingKey::ShowHardwareCursor => "setting:show_hardware_cursor",
        SettingKey::EditorPadding => "setting:editor_padding",
        SettingKey::AutocompleteMaxVisible => "setting:autocomplete_max_visible",
        SettingKey::ClearOnShrink => "setting:clear_on_shrink",
        SettingKey::DoubleEscapeAction => "setting:double_escape_action",
    }
    .to_string()
}

fn format_tool_arguments_summary(tool_name: &str, args: &Value) -> String {
    let key = match tool_name {
        "read" | "write" | "edit" | "ls" => "path",
        "find" | "grep" => "pattern",
        "bash" => "command",
        _ => "",
    };
    if key.is_empty() {
        return truncate_to_width(&args.to_string(), 48);
    }
    args.get(key)
        .and_then(Value::as_str)
        .map(|value| truncate_to_width(value, 48))
        .unwrap_or_else(|| truncate_to_width(&args.to_string(), 48))
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

fn render_footer_panel(
    state: &RpcSessionState,
    stats: &RpcSessionStats,
    cwd: &Path,
    git_branch: Option<&str>,
    width: u16,
    is_streaming: bool,
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
    usage_segments.push(style_subtitle(&format_cost(stats.cost)));
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
            let mut label = style_subtitle(&model.id);
            if model.reasoning && !is_streaming {
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

fn render_prompt_panel(
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

fn display_app_shortcut(keybindings: &KeybindingsManager, action: AppAction) -> String {
    display_shortcut(keybindings.display(action))
}

fn display_editor_shortcut(keybindings: &KeybindingsManager, action: EditorAction) -> String {
    display_shortcut(keybindings.display_editor(action))
}

fn prompt_composer_border_rules(
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

fn normalized_thinking_level(level: &str) -> &str {
    match level.trim() {
        "" => "off",
        value => value,
    }
}

fn startup_hint_lines(keybindings: &KeybindingsManager) -> Vec<String> {
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

fn startup_notice_lines(
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

fn format_startup_resource_notice(notice: pi_rust_core::StartupResourceNotice) -> String {
    format!(
        "{}: {} ({})",
        notice.section.heading(),
        notice.message,
        shorten_home_path(&notice.path.to_string_lossy())
    )
}

fn startup_resource_lines(
    context_files: &[String],
    summary: &StartupResourceSummary,
    width: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    append_startup_resource_section(&mut lines, "[Context]", context_files, width);
    append_startup_resource_path_section(&mut lines, "[Skills]", &summary.skills, width);
    append_startup_resource_path_section(&mut lines, "[Prompts]", &summary.prompts, width);
    append_startup_resource_path_section(&mut lines, "[Extensions]", &summary.extensions, width);
    append_startup_resource_path_section(&mut lines, "[Themes]", &summary.themes, width);

    if !summary.conflicts.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        for section in [
            StartupResourceNoticeSection::Context,
            StartupResourceNoticeSection::Skill,
            StartupResourceNoticeSection::Prompt,
            StartupResourceNoticeSection::Theme,
            StartupResourceNoticeSection::Resource,
        ] {
            let notices = summary
                .conflicts
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

fn composer_max_visible_lines(height: u16) -> usize {
    ((height as usize * 3) / 10).max(5)
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

fn render_prompt_autocomplete(autocomplete: &PromptAutocompleteState, width: u16) -> RenderOutput {
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

fn create_secret_gist(exported_path: &Path) -> Result<(String, String), String> {
    let auth = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                "GitHub CLI (gh) is not installed. Install it from https://cli.github.com/"
                    .to_string()
            }
            _ => format!("Failed to invoke gh auth status: {error}"),
        })?;
    if !auth.status.success() {
        return Err("GitHub CLI is not logged in. Run 'gh auth login' first.".to_string());
    }

    let gist = Command::new("gh")
        .args(["gist", "create", "--public=false"])
        .arg(exported_path)
        .output()
        .map_err(|error| format!("Failed to create gist: {error}"))?;
    if !gist.status.success() {
        let stderr = String::from_utf8_lossy(&gist.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "Failed to create gist.".to_string()
        } else {
            format!("Failed to create gist: {stderr}")
        });
    }

    let gist_url = String::from_utf8_lossy(&gist.stdout).trim().to_string();
    let gist_id = gist_url
        .split('/')
        .next_back()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Failed to parse gist ID from gh output.".to_string())?;
    let viewer_url = pi_rust_config::get_share_viewer_url(gist_id);
    Ok((viewer_url, gist_url))
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return write_clipboard_command("pbcopy", &[], text);
    }
    #[cfg(target_os = "windows")]
    {
        return write_clipboard_command("clip", &[], text);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        for (program, args) in [
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ] {
            if write_clipboard_command(program, &args, text).is_ok() {
                return Ok(());
            }
        }
        Err("No supported clipboard command found (tried wl-copy, xclip, xsel).".to_string())
    }
}

fn paste_clipboard_image_to_temp_file() -> Result<Option<PathBuf>, String> {
    #[cfg(target_os = "macos")]
    {
        return paste_macos_clipboard_image();
    }
    #[cfg(target_os = "linux")]
    {
        return paste_linux_clipboard_image();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
fn paste_macos_clipboard_image() -> Result<Option<PathBuf>, String> {
    let path = temp_clipboard_image_path("png");
    let script = r#"
on run argv
    set outPath to item 1 of argv
    try
        set imageData to the clipboard as «class PNGf»
    on error
        return "NO_IMAGE"
    end try
    set fileRef to open for access POSIX file outPath with write permission
    try
        set eof fileRef to 0
        write imageData to fileRef
        close access fileRef
    on error errMsg
        try
            close access fileRef
        end try
        error errMsg
    end try
    return "OK"
end run
"#;
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg("--")
        .arg(&path)
        .output();
    let Ok(output) = output else {
        return Ok(None);
    };
    if !output.status.success() {
        let _ = fs::remove_file(&path);
        return Ok(None);
    }
    if String::from_utf8_lossy(&output.stdout).trim() == "NO_IMAGE" {
        let _ = fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(path))
}

#[cfg(target_os = "linux")]
fn paste_linux_clipboard_image() -> Result<Option<PathBuf>, String> {
    if let Some((bytes, mime_type)) = read_linux_clipboard_image() {
        let extension = image_extension_for_mime_type(mime_type).unwrap_or("png");
        let path = temp_clipboard_image_path(extension);
        fs::write(&path, bytes).map_err(|error| error.to_string())?;
        return Ok(Some(path));
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
fn read_linux_clipboard_image() -> Option<(Vec<u8>, &'static str)> {
    let preferred_types = ["image/png", "image/jpeg", "image/webp", "image/gif"];
    let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE")
            .ok()
            .is_some_and(|value| value == "wayland");
    if is_wayland {
        let list = Command::new("wl-paste")
            .args(["--list-types"])
            .output()
            .ok()?;
        if !list.status.success() {
            return None;
        }
        let available = String::from_utf8_lossy(&list.stdout);
        let mime_type = preferred_types
            .iter()
            .find(|candidate| available.lines().any(|line| line.trim() == **candidate))?;
        let data = Command::new("wl-paste")
            .args(["--type", mime_type, "--no-newline"])
            .output()
            .ok()?;
        if data.status.success() && !data.stdout.is_empty() {
            return Some((data.stdout, mime_type));
        }
    }

    let targets = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
        .output()
        .ok();
    let target_text = targets
        .as_ref()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    for mime_type in preferred_types {
        if !target_text.is_empty() && !target_text.lines().any(|line| line.trim() == mime_type) {
            continue;
        }
        let data = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", mime_type, "-o"])
            .output()
            .ok()?;
        if data.status.success() && !data.stdout.is_empty() {
            return Some((data.stdout, mime_type));
        }
    }
    None
}

fn image_extension_for_mime_type(mime_type: &str) -> Option<&'static str> {
    match mime_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or(mime_type)
    {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn temp_clipboard_image_path(extension: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "pi-clipboard-{}-{}.{}",
        std::process::id(),
        millis,
        extension
    ))
}

fn write_clipboard_command(program: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => format!("{program} is not installed."),
            _ => format!("Failed to start {program}: {error}"),
        })?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|error| format!("Failed to write to {program}: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("Failed to wait for {program}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("{program} exited with {}", output.status)
        } else {
            stderr
        })
    }
}

fn footer_subtitle(cwd: &Path, git_branch: Option<&str>, width: usize) -> String {
    let cwd_display = shorten_home_path(&cwd.to_string_lossy());
    let plain = if let Some(branch) = git_branch.filter(|branch| !branch.is_empty()) {
        format!("{cwd_display} ({branch})")
    } else {
        cwd_display
    };
    style_subtitle(&truncate_to_width(&plain, width))
}

fn footer_context_usage(state: &RpcSessionState, stats: &RpcSessionStats) -> String {
    let context_window = footer_context_window(state);
    if context_window == 0 {
        return style_hint("unknown-context");
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

fn footer_context_window(state: &RpcSessionState) -> u64 {
    let Some(model) = state.model.as_ref() else {
        return 0;
    };
    match (model.provider.0.as_str(), model.id.as_str()) {
        ("openai", "gpt-4.1" | "gpt-4.1-mini" | "gpt-4.1-nano") => 1_047_576,
        _ => model.context_window as u64,
    }
}

fn align_footer_row(left: &str, right: &str, width: usize) -> String {
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

fn format_token_count(value: u64) -> String {
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

fn format_cost(cost: f64) -> String {
    if cost >= 1.0 {
        format!("${cost:.2}")
    } else if cost > 0.0 {
        format!("${cost:.3}")
    } else {
        "$0.00".to_string()
    }
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
        apply_live_message, apply_live_transcript_message, build_config_entries,
        build_config_items, build_prompt_autocomplete, build_session_overlay_items,
        collect_diff_lines, content_text, discover_session_paths,
        prompt_autocomplete_should_submit_current_prompt, render_footer_panel,
        render_prompt_autocomplete, render_prompt_panel, render_transcript_entry,
        session_selection_detail, session_transcript_lines, tree_item_matches_mode, wrap_text,
    };
    use pi_rust_ai_core::{
        ApiId, AssistantContentBlock, AssistantMessage, Message, Model, ModelCost, ProviderId,
        StopReason, Usage, UsageCost, UserContent, UserContentBlock, UserMessage,
    };
    use pi_rust_packages::{PackageInstallScope, PackageManager};
    use pi_rust_protocol::{
        QueueMode, RpcCommandLocation, RpcCommandSource, RpcSessionState, RpcSessionStats,
        RpcSlashCommand, RpcTokenStats,
    };
    use pi_rust_tui::{
        Component, CursorPosition, Editor, KeyCode, KeyEvent, RenderOutput, RenderedLine,
        SelectItem, TerminalCapabilities,
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
        assert!(!text.contains("high"));
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
    fn config_items_include_settings_paths() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let manager = PackageManager::create(&cwd, Some(tempdir.path().join("agent")));
        let items = build_config_items(&manager);
        assert!(items.iter().any(|item| item.label == "Global settings"));
        assert!(items.iter().any(|item| item.label == "Project settings"));
    }

    #[test]
    fn overlay_filters_list_from_search_input() {
        let mut overlay = SearchOverlay::new(
            "Title",
            "Subtitle",
            vec![
                pi_rust_tui::SelectItem {
                    value: "one".to_string(),
                    label: "Alpha".to_string(),
                    description: None,
                },
                pi_rust_tui::SelectItem {
                    value: "two".to_string(),
                    label: "Beta".to_string(),
                    description: None,
                },
            ],
            None,
            "Esc cancels",
        );

        let _ = overlay.handle_key(&KeyEvent::new(KeyCode::Char('b')));
        assert_eq!(overlay.list.selected_item().expect("item").label, "Beta");
    }

    #[test]
    fn apply_live_message_replaces_existing_assistant_partial() {
        let mut messages = vec![
            Message::User(UserMessage {
                content: UserContent::Text("hello".to_string()),
                timestamp: 0,
            }),
            Message::Assistant(assistant_message("partial")),
        ];

        apply_live_message(
            &mut messages,
            Message::Assistant(assistant_message("final")),
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1], Message::Assistant(assistant_message("final")));
    }

    #[test]
    fn config_entries_include_installed_packages() {
        let tempdir = tempdir().expect("tempdir");
        let cwd = tempdir.path().join("workspace");
        let package_dir = tempdir.path().join("local-package");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::create_dir_all(&package_dir).expect("package dir");
        std::fs::write(package_dir.join("SYSTEM.md"), "system").expect("system");
        let mut manager = PackageManager::create(&cwd, Some(tempdir.path().join("agent")));
        manager
            .install(
                package_dir.to_string_lossy().as_ref(),
                PackageInstallScope::Project,
            )
            .expect("install");

        let entries = build_config_entries(&manager);
        assert!(
            entries
                .iter()
                .any(|entry| entry.item.label == "Global settings")
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.item.label.contains("local-package"))
        );
    }

    #[test]
    fn tree_no_tools_filter_hides_tool_results_and_tool_only_assistants() {
        let tool_result = TreeListItem {
            entry_id: "tool".to_string(),
            entry_type: "message".to_string(),
            message_role: Some("toolResult".to_string()),
            assistant_tool_only: false,
            preview: "tool result".to_string(),
            search_text: "tool result".to_string(),
            depth: 0,
            label: None,
        };
        let assistant_tool_only = TreeListItem {
            entry_id: "assistant".to_string(),
            entry_type: "message".to_string(),
            message_role: Some("assistant".to_string()),
            assistant_tool_only: true,
            preview: "assistant: tool".to_string(),
            search_text: "assistant tool".to_string(),
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

        let (items, selections) = build_session_overlay_items(
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
                assert_eq!(path, &PathBuf::from("/tmp/current.jsonl"));
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

        let (items, _) = build_session_overlay_items(
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

        let (items, _) = build_session_overlay_items(
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
    fn apply_live_transcript_message_replaces_existing_assistant_partial() {
        let mut transcript = vec![
            TranscriptEntry::Message(Message::User(UserMessage {
                content: UserContent::Text("hello".to_string()),
                timestamp: 0,
            })),
            TranscriptEntry::Message(Message::Assistant(assistant_message("partial"))),
        ];

        apply_live_transcript_message(
            &mut transcript,
            Message::Assistant(assistant_message("final")),
        );

        assert_eq!(transcript.len(), 2);
        assert_eq!(
            transcript[1],
            TranscriptEntry::Message(Message::Assistant(assistant_message("final")))
        );
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

    #[test]
    fn session_transcript_expand_mode_all_expands_finished_tool_blocks() {
        let lines = session_transcript_lines(
            &[
                TranscriptEntry::Message(Message::ToolResult(pi_rust_ai_core::ToolResultMessage {
                    tool_call_id: "call-old".to_string(),
                    tool_name: "grep".to_string(),
                    content: vec![UserContentBlock::Text {
                        text: "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\nthirteen\nfourteen\nfifteen\nsixteen".to_string(),
                        text_signature: None,
                    }],
                    details: None,
                    is_error: false,
                    timestamp: 0,
                })),
                TranscriptEntry::CustomMessage {
                    custom_type: "bash_execution".to_string(),
                    content: UserContent::Blocks(vec![UserContentBlock::Text {
                        text: "$ seq 1 24\n1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n21\n22\n23\n24\nExit code: 0"
                            .to_string(),
                        text_signature: None,
                    }]),
                    details: None,
                },
            ],
            80,
            false,
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
        assert!(rendered.iter().any(|line| line.contains("sixteen")));
        assert!(rendered.iter().any(|line| line.contains("24")));
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
