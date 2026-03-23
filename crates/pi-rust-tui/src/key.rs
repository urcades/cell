use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyModifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };

    pub const CTRL: Self = Self {
        ctrl: true,
        alt: false,
        shift: false,
    };

    pub const ALT: Self = Self {
        ctrl: false,
        alt: true,
        shift: false,
    };

    pub const SHIFT: Self = Self {
        ctrl: false,
        alt: false,
        shift: true,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEventKind {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Delete,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PasteStart,
    PasteEnd,
    Paste(String),
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub kind: KeyEventKind,
}

impl KeyEvent {
    pub fn new(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
        }
    }

    pub fn with_modifiers(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self {
            code,
            modifiers,
            kind: KeyEventKind::Press,
        }
    }
}

impl fmt::Display for KeyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyCode::Char(value) => write!(f, "{value}"),
            KeyCode::Enter => write!(f, "enter"),
            KeyCode::Escape => write!(f, "escape"),
            KeyCode::Backspace => write!(f, "backspace"),
            KeyCode::Delete => write!(f, "delete"),
            KeyCode::Tab => write!(f, "tab"),
            KeyCode::BackTab => write!(f, "backtab"),
            KeyCode::Up => write!(f, "up"),
            KeyCode::Down => write!(f, "down"),
            KeyCode::Left => write!(f, "left"),
            KeyCode::Right => write!(f, "right"),
            KeyCode::Home => write!(f, "home"),
            KeyCode::End => write!(f, "end"),
            KeyCode::PasteStart => write!(f, "paste-start"),
            KeyCode::PasteEnd => write!(f, "paste-end"),
            KeyCode::Paste(value) => write!(f, "paste({value})"),
            KeyCode::Unknown(value) => write!(f, "{value}"),
        }
    }
}

pub fn parse_input_bytes(bytes: &[u8]) -> Vec<KeyEvent> {
    let mut events = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'\r' | b'\n' => {
                events.push(KeyEvent::new(KeyCode::Enter));
                index += 1;
            }
            b'\t' => {
                events.push(KeyEvent::new(KeyCode::Tab));
                index += 1;
            }
            0x7f => {
                events.push(KeyEvent::new(KeyCode::Backspace));
                index += 1;
            }
            0x1b => {
                if let Some((event, consumed)) = parse_escape_sequence(&bytes[index..]) {
                    events.push(event);
                    index += consumed;
                } else {
                    events.push(KeyEvent::new(KeyCode::Escape));
                    index += 1;
                }
            }
            value if (1..=26).contains(&value) => {
                let ctrl_char = ((value - 1) + b'a') as char;
                events.push(KeyEvent::with_modifiers(
                    KeyCode::Char(ctrl_char),
                    KeyModifiers::CTRL,
                ));
                index += 1;
            }
            _ => {
                if let Some(character) = decode_char(&bytes[index..]) {
                    index += character.len_utf8();
                    events.push(KeyEvent::new(KeyCode::Char(character)));
                } else {
                    events.push(KeyEvent::new(KeyCode::Unknown(format!("0x{byte:02x}"))));
                    index += 1;
                }
            }
        }
    }

    collapse_paste_events(events)
}

fn parse_escape_sequence(bytes: &[u8]) -> Option<(KeyEvent, usize)> {
    if bytes.starts_with(b"\x1b[200~") {
        return Some((KeyEvent::new(KeyCode::PasteStart), 6));
    }
    if bytes.starts_with(b"\x1b[201~") {
        return Some((KeyEvent::new(KeyCode::PasteEnd), 6));
    }
    if bytes.starts_with(b"\x1b\r") || bytes.starts_with(b"\x1b\n") {
        return Some((
            KeyEvent::with_modifiers(KeyCode::Enter, KeyModifiers::ALT),
            2,
        ));
    }
    if bytes.starts_with(b"\x1b[A") {
        return Some((KeyEvent::new(KeyCode::Up), 3));
    }
    if bytes.starts_with(b"\x1b[B") {
        return Some((KeyEvent::new(KeyCode::Down), 3));
    }
    if bytes.starts_with(b"\x1b[C") {
        return Some((KeyEvent::new(KeyCode::Right), 3));
    }
    if bytes.starts_with(b"\x1b[D") {
        return Some((KeyEvent::new(KeyCode::Left), 3));
    }
    if bytes.starts_with(b"\x1b[H") || bytes.starts_with(b"\x1bOH") {
        return Some((KeyEvent::new(KeyCode::Home), 3));
    }
    if bytes.starts_with(b"\x1b[F") || bytes.starts_with(b"\x1bOF") {
        return Some((KeyEvent::new(KeyCode::End), 3));
    }
    if bytes.starts_with(b"\x1b[3~") {
        return Some((KeyEvent::new(KeyCode::Delete), 4));
    }
    if bytes.starts_with(b"\x1b[Z") {
        return Some((
            KeyEvent::with_modifiers(KeyCode::BackTab, KeyModifiers::SHIFT),
            3,
        ));
    }
    if let Some((event, consumed)) = parse_modified_csi(bytes) {
        return Some((event, consumed));
    }
    if let Some((event, consumed)) = parse_csi_u(bytes) {
        return Some((event, consumed));
    }
    if bytes.len() >= 2 && bytes[0] == 0x1b && bytes[1].is_ascii() && !bytes[1].is_ascii_control() {
        return Some((
            KeyEvent::with_modifiers(KeyCode::Char(bytes[1] as char), KeyModifiers::ALT),
            2,
        ));
    }

    None
}

fn parse_modified_csi(bytes: &[u8]) -> Option<(KeyEvent, usize)> {
    if bytes.len() < 6 || bytes[0] != 0x1b || bytes[1] != b'[' {
        return None;
    }
    let consumed = bytes
        .iter()
        .enumerate()
        .skip(2)
        .find_map(|(index, byte)| byte.is_ascii_alphabetic().then_some(index + 1))?;
    let final_byte = bytes[consumed - 1];
    if !matches!(final_byte, b'A' | b'B' | b'C' | b'D' | b'H' | b'F' | b'Z') {
        return None;
    }

    let payload = std::str::from_utf8(&bytes[2..consumed - 1]).ok()?;
    let mut parts = payload.split(';').filter(|part| !part.is_empty());
    let prefix = parts.next()?;
    let modifier = parts.next_back().or_else(|| parts.next())?;
    if prefix.is_empty() || modifier.is_empty() {
        return None;
    }
    let modifier = parse_modifier_value(modifier.parse::<u8>().ok()?);
    let code = match final_byte {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'Z' => KeyCode::BackTab,
        _ => return None,
    };
    Some((KeyEvent::with_modifiers(code, modifier), consumed))
}

fn parse_csi_u(bytes: &[u8]) -> Option<(KeyEvent, usize)> {
    if bytes.len() < 5 || !bytes.starts_with(b"\x1b[") {
        return None;
    }
    let consumed = bytes
        .iter()
        .enumerate()
        .skip(2)
        .find_map(|(index, byte)| (*byte == b'u').then_some(index + 1))?;
    if bytes[consumed - 1] != b'u' {
        return None;
    }

    let payload = std::str::from_utf8(&bytes[2..consumed - 1]).ok()?;
    let (codepoint, modifier) = payload.split_once(';')?;
    let codepoint = codepoint.parse::<u32>().ok()?;
    let modifiers = parse_modifier_value(modifier.parse::<u8>().ok()?);
    let code = match codepoint {
        9 => {
            if modifiers.shift {
                KeyCode::BackTab
            } else {
                KeyCode::Tab
            }
        }
        13 => KeyCode::Enter,
        27 => KeyCode::Escape,
        127 => KeyCode::Backspace,
        value => char::from_u32(value).map(KeyCode::Char)?,
    };
    Some((KeyEvent::with_modifiers(code, modifiers), consumed))
}

fn parse_modifier_value(value: u8) -> KeyModifiers {
    let normalized = value.saturating_sub(1);
    KeyModifiers {
        shift: normalized & 0b001 != 0,
        alt: normalized & 0b010 != 0,
        ctrl: normalized & 0b100 != 0,
    }
}

fn decode_char(bytes: &[u8]) -> Option<char> {
    let first = *bytes.first()?;
    let width = if first < 0x80 {
        1
    } else if first & 0b1110_0000 == 0b1100_0000 {
        2
    } else if first & 0b1111_0000 == 0b1110_0000 {
        3
    } else if first & 0b1111_1000 == 0b1111_0000 {
        4
    } else {
        return None;
    };

    std::str::from_utf8(bytes.get(..width)?)
        .ok()?
        .chars()
        .next()
}

fn collapse_paste_events(events: Vec<KeyEvent>) -> Vec<KeyEvent> {
    let mut collapsed = Vec::new();
    let mut in_paste = false;
    let mut paste_buffer = String::new();

    for event in events {
        match event.code {
            KeyCode::PasteStart => {
                in_paste = true;
                paste_buffer.clear();
            }
            KeyCode::PasteEnd => {
                if in_paste {
                    collapsed.push(KeyEvent::new(KeyCode::Paste(paste_buffer.clone())));
                }
                in_paste = false;
                paste_buffer.clear();
            }
            KeyCode::Char(ch) if in_paste => paste_buffer.push(ch),
            KeyCode::Enter if in_paste => paste_buffer.push('\n'),
            _ if in_paste => {}
            _ => collapsed.push(event),
        }
    }

    if in_paste && !paste_buffer.is_empty() {
        collapsed.push(KeyEvent::new(KeyCode::Paste(paste_buffer)));
    }

    collapsed
}

#[cfg(test)]
mod tests {
    use super::{KeyCode, KeyModifiers, parse_input_bytes};

    #[test]
    fn parses_basic_arrow_sequences() {
        let events = parse_input_bytes(b"\x1b[A\x1b[B\x1b[C\x1b[D");
        assert_eq!(
            events
                .into_iter()
                .map(|event| event.code)
                .collect::<Vec<_>>(),
            vec![KeyCode::Up, KeyCode::Down, KeyCode::Right, KeyCode::Left]
        );
    }

    #[test]
    fn parses_shift_tab_and_alt_chars() {
        let events = parse_input_bytes(b"\x1b[Z\x1ba");
        assert_eq!(events[0].code, KeyCode::BackTab);
        assert!(events[0].modifiers.shift);
        assert_eq!(events[1].code, KeyCode::Char('a'));
        assert_eq!(events[1].modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parses_modified_csi_backtab_sequences() {
        let events = parse_input_bytes(b"\x1b[1;2Z");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].code, KeyCode::BackTab);
        assert!(events[0].modifiers.shift);
    }

    #[test]
    fn collapses_bracketed_paste() {
        let events = parse_input_bytes(b"\x1b[200~hello\nworld\x1b[201~");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].code, KeyCode::Paste("hello\nworld".to_string()));
    }

    #[test]
    fn parses_alt_enter_and_alt_up() {
        let events = parse_input_bytes(b"\x1b\r\x1b[1;3A");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code, KeyCode::Enter);
        assert_eq!(events[0].modifiers, KeyModifiers::ALT);
        assert_eq!(events[1].code, KeyCode::Up);
        assert_eq!(events[1].modifiers, KeyModifiers::ALT);
    }

    #[test]
    fn parses_csi_u_printable_shortcuts() {
        let events = parse_input_bytes(b"\x1b[112;5u");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].code, KeyCode::Char('p'));
        assert_eq!(events[0].modifiers, KeyModifiers::CTRL);
    }
}
