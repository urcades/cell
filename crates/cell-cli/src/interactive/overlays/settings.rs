use super::super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingKey {
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

pub(crate) struct SettingsOverlayState {
    #[allow(dead_code)]
    pub(crate) title: String,
    #[allow(dead_code)]
    pub(crate) subtitle: String,
    #[allow(dead_code)]
    pub(crate) hint: String,
    pub(crate) list: SettingsList,
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

pub(crate) fn setting_key_value(key: SettingKey) -> String {
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
