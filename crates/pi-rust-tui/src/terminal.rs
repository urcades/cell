use std::env;
use std::io::{self, Read, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType, SetTitle, disable_raw_mode, enable_raw_mode, size};

use crate::key::{KeyEvent, parse_input_bytes};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    ITerm2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub kitty_keyboard: bool,
    pub inline_images: bool,
    pub image_protocol: Option<ImageProtocol>,
    pub hyperlinks: bool,
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self {
            kitty_keyboard: false,
            inline_images: false,
            image_protocol: None,
            hyperlinks: true,
        }
    }
}

pub trait Terminal {
    fn start(&mut self) -> io::Result<()>;
    fn stop(&mut self) -> io::Result<()>;
    fn drain_input(&mut self, max_ms: u64, idle_ms: u64) -> io::Result<()>;
    fn read_events(&mut self) -> io::Result<Vec<KeyEvent>>;
    fn write(&mut self, data: &str) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    fn size(&self) -> io::Result<(u16, u16)>;
    fn cursor_position(&self) -> io::Result<(u16, u16)>;
    fn move_to(&mut self, col: u16, row: u16) -> io::Result<()>;
    fn hide_cursor(&mut self) -> io::Result<()>;
    fn show_cursor(&mut self) -> io::Result<()>;
    fn clear_line(&mut self) -> io::Result<()>;
    fn clear_from_cursor(&mut self) -> io::Result<()>;
    fn clear_screen(&mut self) -> io::Result<()>;
    fn set_title(&mut self, title: &str) -> io::Result<()>;
    fn capabilities(&self) -> TerminalCapabilities;
}

pub struct ProcessTerminal {
    stdin: io::Stdin,
    stdout: io::Stdout,
    capabilities: TerminalCapabilities,
    raw_enabled: bool,
    bracketed_paste_enabled: bool,
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self {
            stdin: io::stdin(),
            stdout: io::stdout(),
            capabilities: detect_capabilities(),
            raw_enabled: false,
            bracketed_paste_enabled: false,
        }
    }
}

impl ProcessTerminal {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Terminal for ProcessTerminal {
    fn start(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        self.raw_enabled = true;
        execute!(self.stdout, EnableBracketedPaste)?;
        self.bracketed_paste_enabled = true;
        Ok(())
    }

    fn stop(&mut self) -> io::Result<()> {
        if self.bracketed_paste_enabled {
            let _ = execute!(self.stdout, DisableBracketedPaste);
            self.bracketed_paste_enabled = false;
        }
        if self.raw_enabled {
            let _ = disable_raw_mode();
            self.raw_enabled = false;
        }
        self.flush()
    }

    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) -> io::Result<()> {
        Ok(())
    }

    fn read_events(&mut self) -> io::Result<Vec<KeyEvent>> {
        let mut buffer = [0u8; 64];
        let bytes_read = self.stdin.read(&mut buffer)?;
        Ok(parse_input_bytes(&buffer[..bytes_read]))
    }

    fn write(&mut self, data: &str) -> io::Result<()> {
        self.stdout.write_all(data.as_bytes())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()
    }

    fn size(&self) -> io::Result<(u16, u16)> {
        size()
    }

    fn cursor_position(&self) -> io::Result<(u16, u16)> {
        crossterm::cursor::position()
    }

    fn move_to(&mut self, col: u16, row: u16) -> io::Result<()> {
        execute!(self.stdout, MoveTo(col, row))
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(self.stdout, Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(self.stdout, Show)
    }

    fn clear_line(&mut self) -> io::Result<()> {
        execute!(self.stdout, Clear(ClearType::CurrentLine))
    }

    fn clear_from_cursor(&mut self) -> io::Result<()> {
        execute!(self.stdout, Clear(ClearType::FromCursorDown))
    }

    fn clear_screen(&mut self) -> io::Result<()> {
        execute!(self.stdout, Clear(ClearType::All), MoveTo(0, 0))
    }

    fn set_title(&mut self, title: &str) -> io::Result<()> {
        execute!(self.stdout, SetTitle(title))
    }

    fn capabilities(&self) -> TerminalCapabilities {
        self.capabilities.clone()
    }
}

impl Drop for ProcessTerminal {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn detect_capabilities() -> TerminalCapabilities {
    detect_capabilities_with(|key| env::var(key).ok())
}

fn detect_capabilities_with(get_var: impl Fn(&str) -> Option<String>) -> TerminalCapabilities {
    let term_program = get_var("TERM_PROGRAM").unwrap_or_default().to_lowercase();
    let term = get_var("TERM").unwrap_or_default().to_lowercase();

    let image_protocol = if get_var("KITTY_WINDOW_ID").is_some()
        || term_program == "kitty"
        || term_program == "ghostty"
        || term.contains("ghostty")
        || get_var("GHOSTTY_RESOURCES_DIR").is_some()
        || get_var("WEZTERM_PANE").is_some()
        || term_program == "wezterm"
    {
        Some(ImageProtocol::Kitty)
    } else if get_var("ITERM_SESSION_ID").is_some() || term_program == "iterm.app" {
        Some(ImageProtocol::ITerm2)
    } else {
        None
    };

    TerminalCapabilities {
        kitty_keyboard: false,
        inline_images: image_protocol.is_some(),
        image_protocol,
        hyperlinks: true,
    }
}

#[cfg(test)]
mod tests {
    use super::{ImageProtocol, TerminalCapabilities, detect_capabilities_with};

    fn caps(entries: &[(&str, &str)]) -> TerminalCapabilities {
        detect_capabilities_with(|key| {
            entries
                .iter()
                .find_map(|(name, value)| (*name == key).then(|| (*value).to_string()))
        })
    }

    #[test]
    fn kitty_like_terminals_enable_inline_images() {
        for capabilities in [
            caps(&[("KITTY_WINDOW_ID", "123")]),
            caps(&[("TERM_PROGRAM", "kitty")]),
            caps(&[("TERM_PROGRAM", "ghostty")]),
            caps(&[("TERM", "xterm-ghostty")]),
            caps(&[("WEZTERM_PANE", "1")]),
        ] {
            assert!(capabilities.inline_images);
            assert_eq!(capabilities.image_protocol, Some(ImageProtocol::Kitty));
        }
    }

    #[test]
    fn iterm_enables_inline_images() {
        for capabilities in [
            caps(&[("ITERM_SESSION_ID", "abc")]),
            caps(&[("TERM_PROGRAM", "iTerm.app")]),
        ] {
            assert!(capabilities.inline_images);
            assert_eq!(capabilities.image_protocol, Some(ImageProtocol::ITerm2));
        }
    }

    #[test]
    fn unsupported_terminals_fall_back_to_text_images() {
        for capabilities in [
            caps(&[("TERM_PROGRAM", "vscode")]),
            caps(&[("TERM_PROGRAM", "Apple_Terminal")]),
            caps(&[]),
        ] {
            assert!(!capabilities.inline_images);
            assert_eq!(capabilities.image_protocol, None);
        }
    }
}
