use super::*;

pub(super) struct InteractiveApp {
    session: Arc<Mutex<AgentSession>>,
    control: AgentControl,
    keybindings: KeybindingsManager,
    package_manager: PackageManager,
    session_dir_override: Option<PathBuf>,
    terminal_capabilities: TerminalCapabilities,
    editor: Editor,
    prompt_autocomplete: Option<PromptAutocompleteState>,
    pub(super) status: Option<String>,
    ctrl_c_armed_at: Option<Instant>,
    last_empty_escape_at: Option<Instant>,
    active_prompt: Option<ActivePrompt>,
    active_auth: Option<ActiveAuthFlow>,
    overlay: Option<OverlayState>,
    pending_messages: Vec<QueuedMessage>,
    active_tools: Vec<ActiveToolExecution>,
    active_share: Option<ActiveShare>,
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
    available_provider_count: usize,
    using_oauth_subscription: bool,
    spinner_frame: usize,
    cwd: PathBuf,
    git_branch: Option<String>,
}

impl InteractiveApp {
    pub(super) fn new(
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
            active_share: None,
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
            available_provider_count: 0,
            using_oauth_subscription: false,
            spinner_frame: 0,
            cwd: cwd.to_path_buf(),
            git_branch: detect_git_branch(cwd),
        };
        app.refresh_snapshot()?;
        app.update_prompt_autocomplete()?;
        Ok(app)
    }

    pub(super) fn needs_periodic_redraw(&self) -> bool {
        self.active_prompt.is_some() || self.active_share.is_some() || self.active_auth.is_some()
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

    pub(super) fn poll_background(&mut self) -> Result<(), String> {
        if self.active_prompt.is_none() {
            self.poll_share_background()?;
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

    fn poll_share_background(&mut self) -> Result<(), String> {
        let Some(active) = self.active_share.as_mut() else {
            return Ok(());
        };

        match active.result_rx.try_recv() {
            Ok(result) => {
                let mut active = self.active_share.take().expect("active share");
                if let Some(handle) = active.handle.take() {
                    let _ = handle.join();
                }
                self.status = Some(match result? {
                    ShareTaskResult::Success {
                        viewer_url,
                        gist_url,
                    } => {
                        format!("Share URL: {viewer_url}\nGist: {gist_url}")
                    }
                    ShareTaskResult::Cancelled => "Share cancelled".to_string(),
                });
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.spinner_frame = self.spinner_frame.wrapping_add(1);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let mut active = self.active_share.take().expect("active share");
                if let Some(handle) = active.handle.take() {
                    let _ = handle.join();
                }
                self.status = Some("Share worker disconnected unexpectedly.".to_string());
            }
        }

        Ok(())
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

    pub(super) fn render(&self, width: u16, height: u16) -> RenderOutput {
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
        let expand_hint = self.keybindings.display(AppAction::ExpandTools);
        let transcript_context = TranscriptRenderContext::new(
            width,
            self.hide_thinking,
            self.show_images,
            &self.terminal_capabilities,
            self.tool_expand_mode,
            latest_tool_panel.as_deref(),
            &expand_hint,
        );
        let mut transcript_lines =
            session_transcript_lines_with_context(&transcript_entries, &transcript_context);
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
        let tool_lines =
            active_tool_render_lines_with_context(&self.active_tools, &transcript_context);
        let footer = render_footer_panel(
            &self.cached_state,
            &self.cached_stats,
            &self.cwd,
            self.git_branch.as_deref(),
            width,
            self.active_prompt.is_some(),
            self.available_provider_count,
            self.using_oauth_subscription,
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
        let prompt = if self.overlay.is_none() && self.active_share.is_none() {
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
        if let Some(_active) = &self.active_share {
            let spinner = ["-", "\\", "|", "/"][self.spinner_frame % 4];
            lines.push(truncate_to_width(
                &style_warning(&format!("{spinner} Creating gist... Esc cancels")),
                width as usize,
            ));
        }
        if let Some(_active) = &self.active_prompt {
            lines.push(truncate_to_width(
                &style_warning("Working... (Esc to interrupt)"),
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

    pub(super) fn handle_key(&mut self, event: KeyEvent) -> Result<LoopAction, String> {
        if self.active_share.is_some() {
            if matches!(event.code, KeyCode::Escape) && event.modifiers == KeyModifiers::NONE {
                if let Some(active) = &self.active_share {
                    active.cancel_flag.store(true, Ordering::SeqCst);
                }
                return Ok(LoopAction::Continue);
            }
            if !matches_ctrl_char(&event, 'c') {
                return Ok(LoopAction::Continue);
            }
        }
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
            ensure_github_cli_ready()?;
            let temp_path = share_export_path();
            let exported = self.with_session(|session| {
                session
                    .export_html(Some(&temp_path))
                    .map_err(|error| error.to_string())
            })??;
            let (result_tx, result_rx) = mpsc::channel();
            let cancel_flag = Arc::new(AtomicBool::new(false));
            let task_cancel = Arc::clone(&cancel_flag);
            let handle = thread::spawn(move || {
                let result = run_share_task(exported, task_cancel);
                let _ = result_tx.send(result);
            });
            self.status = None;
            self.active_share = Some(ActiveShare {
                result_rx,
                cancel_flag,
                handle: Some(handle),
                started_at: Instant::now(),
            });
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

    pub(super) fn start_prompt(&mut self, prompt: String) -> Result<(), String> {
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
        let (
            messages,
            transcript,
            state,
            stats,
            available_model_count,
            available_provider_count,
            using_oauth_subscription,
            startup_summary,
        ) = self.with_session(|session| {
            let state = session.get_state();
            let available_models = session.get_available_models();
            let available_provider_count = available_models
                .iter()
                .map(|model| model.provider.0.clone())
                .collect::<HashSet<_>>()
                .len();
            let using_oauth_subscription = state.model.as_ref().is_some_and(|model| {
                matches!(
                    session
                        .model_registry()
                        .auth_storage()
                        .get(&model.provider.0),
                    Some(AuthCredential::OAuth(_))
                )
            });
            (
                session.get_messages(),
                build_transcript_entries(session),
                state,
                session.get_session_stats(),
                available_models.len(),
                available_provider_count,
                using_oauth_subscription,
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
        self.available_provider_count = available_provider_count;
        self.using_oauth_subscription = using_oauth_subscription;
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
                self.persist_global_settings(GlobalSettingChange::AutoCompact(
                    bool_value_selected,
                ))?;
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
                let persisted_mode = match mode {
                    QueueMode::All => QueueModeSetting::All,
                    QueueMode::OneAtATime => QueueModeSetting::OneAtATime,
                };
                self.persist_global_settings(GlobalSettingChange::SteeringMode(persisted_mode))?;
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
                let persisted_mode = match mode {
                    QueueMode::All => QueueModeSetting::All,
                    QueueMode::OneAtATime => QueueModeSetting::OneAtATime,
                };
                self.persist_global_settings(GlobalSettingChange::FollowUpMode(persisted_mode))?;
                self.refresh_snapshot()?;
                self.status = Some(format!("Follow-up mode: {}", queue_mode_value(mode)));
            }
            "setting:transport" => {
                let transport = match value {
                    "sse" => TransportSetting::Sse,
                    "websocket" => TransportSetting::Websocket,
                    _ => TransportSetting::Auto,
                };
                self.persist_global_settings(GlobalSettingChange::Transport(transport))?;
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
                self.persist_global_settings(GlobalSettingChange::DefaultThinkingLevel(
                    value.to_string(),
                ))?;
                self.refresh_snapshot()?;
                self.status = Some(format!(
                    "Thinking level: {}",
                    self.cached_state.thinking_level
                ));
            }
            "setting:theme" => {
                self.persist_global_settings(GlobalSettingChange::Theme(value.to_string()))?;
                self.status = Some(format!(
                    "Saved theme: {value}. Theme rendering parity is still in progress."
                ));
            }
            "setting:hide_thinking" => {
                self.hide_thinking = bool_value_selected;
                self.persist_global_settings(GlobalSettingChange::HideThinkingBlock(
                    self.hide_thinking,
                ))?;
                self.status = Some(format!("Hide thinking: {}", bool_value(self.hide_thinking)));
            }
            "setting:collapse_changelog" => {
                self.persist_global_settings(GlobalSettingChange::CollapseChangelog(
                    bool_value_selected,
                ))?;
                self.status = Some(format!(
                    "Collapse changelog: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:quiet_startup" => {
                self.quiet_startup = bool_value_selected;
                self.persist_global_settings(GlobalSettingChange::QuietStartup(
                    bool_value_selected,
                ))?;
                self.status = Some(format!(
                    "Quiet startup: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:show_images" => {
                self.show_images = bool_value_selected;
                self.persist_global_settings(GlobalSettingChange::TerminalShowImages(
                    bool_value_selected,
                ))?;
                self.status = Some(format!("Show images: {}", bool_value(bool_value_selected)));
            }
            "setting:auto_resize_images" => {
                self.persist_global_settings(GlobalSettingChange::ImagesAutoResize(
                    bool_value_selected,
                ))?;
                self.status = Some(format!(
                    "Auto-resize images: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:block_images" => {
                self.persist_global_settings(GlobalSettingChange::ImagesBlockImages(
                    bool_value_selected,
                ))?;
                self.status = Some(format!("Block images: {}", bool_value(bool_value_selected)));
            }
            "setting:skill_commands" => {
                self.persist_global_settings(GlobalSettingChange::EnableSkillCommands(
                    bool_value_selected,
                ))?;
                self.update_prompt_autocomplete()?;
                self.status = Some(format!(
                    "Skill commands: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:show_hardware_cursor" => {
                self.persist_global_settings(GlobalSettingChange::ShowHardwareCursor(
                    bool_value_selected,
                ))?;
                self.status = Some(format!(
                    "Saved hardware cursor preference: {}.",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:editor_padding" => {
                let padding = value.parse::<i64>().unwrap_or(0).clamp(0, 3);
                self.persist_global_settings(GlobalSettingChange::EditorPaddingX(padding))?;
                self.status = Some(format!("Saved editor padding: {padding}."));
            }
            "setting:autocomplete_max_visible" => {
                let max_visible = value.parse::<i64>().unwrap_or(5).clamp(3, 20);
                self.persist_global_settings(GlobalSettingChange::AutocompleteMaxVisible(
                    max_visible,
                ))?;
                self.status = Some(format!("Saved autocomplete max items: {max_visible}."));
            }
            "setting:clear_on_shrink" => {
                self.persist_global_settings(GlobalSettingChange::TerminalClearOnShrink(
                    bool_value_selected,
                ))?;
                self.status = Some(format!(
                    "Clear on shrink: {}",
                    bool_value(bool_value_selected)
                ));
            }
            "setting:double_escape_action" => {
                self.double_escape_action = DoubleEscapeAction::from_settings(Some(value));
                let action = match value {
                    "tree" => DoubleEscapeActionSetting::Tree,
                    "fork" => DoubleEscapeActionSetting::Fork,
                    _ => DoubleEscapeActionSetting::None,
                };
                self.persist_global_settings(GlobalSettingChange::DoubleEscapeAction(action))?;
                self.status = Some(format!("Double-escape action: {value}"));
            }
            _ => {
                self.status = Some(format!("Unknown setting: {id}"));
            }
        }
        Ok(())
    }

    fn persist_global_settings(&mut self, change: GlobalSettingChange) -> Result<(), String> {
        self.package_manager
            .settings_manager_mut()
            .apply_setting_change(SettingsScope::Global, change)
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

    pub(super) fn open_external_editor(
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
