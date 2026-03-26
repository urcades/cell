//! Custom terminal renderer/input pipeline for the Rust port.
//!
//! The renderer is intentionally custom and line-oriented. `ratatui` is not
//! used as the primary UI framework.

mod key;
mod render;
mod terminal;
mod widgets;

pub use key::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, parse_input_bytes};
pub use render::{
    Component, CursorPosition, ImageLine, LineDiffRenderer, RenderAnchor, RenderOutput,
    RenderedLine, fit_line, truncate_to_width, visible_width,
};
pub use terminal::{ImageProtocol, ProcessTerminal, Terminal, TerminalCapabilities};
pub use widgets::{
    BoxWidget, ComposerBorderRules, Container, Editor, EditorEvent, Focusable, Input, InputEvent,
    SelectEvent, SelectItem, SelectList, SettingItem, SettingSubmenu, SettingsList,
    SettingsListEvent, SettingsListOptions, Spacer, Text, WidgetEvent,
};

pub const RENDERING_STRATEGY: &str = "custom-line-diff";
