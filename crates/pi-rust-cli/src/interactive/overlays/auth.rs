use super::super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthFlowMode {
    Login,
    Logout,
}

pub(crate) struct AuthOverlayState {
    pub(crate) provider: String,
    pub(crate) subtitle: String,
    pub(crate) message_lines: Vec<String>,
    pub(crate) input: Input,
    pub(crate) awaiting_input: bool,
    pub(crate) prompt_kind: Option<AuthPromptKind>,
}

impl AuthOverlayState {
    pub(crate) fn new(provider: &str) -> Self {
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

    pub(crate) fn set_auth_info(&mut self, info: OAuthAuthInfo) {
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

    pub(crate) fn set_prompt(&mut self, prompt: OAuthPrompt, kind: AuthPromptKind) {
        self.awaiting_input = true;
        self.prompt_kind = Some(kind);
        self.input.clear();
        self.input.set_focused(true);
        self.message_lines.push(prompt.message);
        if let Some(placeholder) = prompt.placeholder {
            self.message_lines.push(format!("e.g., {placeholder}"));
        }
    }

    pub(crate) fn push_progress(&mut self, message: String) {
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
