use std::io;

use base64::Engine;
use unicode_width::UnicodeWidthChar;

use crate::terminal::{ImageProtocol, Terminal, TerminalCapabilities};

const SYNC_UPDATES_START: &str = "\u{1b}[?2026h";
const SYNC_UPDATES_END: &str = "\u{1b}[?2026l";
const RESET_LINE_STATE: &str = "\u{1b}[0m\u{1b}]8;;\u{7}";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorPosition {
    pub row: u16,
    pub col: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageLine {
    pub alt_text: String,
    pub mime_type: Option<String>,
    pub data: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderedLine {
    Text(String),
    Image(ImageLine),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderOutput {
    pub lines: Vec<RenderedLine>,
    pub cursor: Option<CursorPosition>,
}

pub trait Component {
    fn render(&self, width: u16) -> RenderOutput;

    fn invalidate(&mut self) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderAnchor {
    pub col: u16,
    pub row: u16,
}

#[derive(Clone, Debug)]
pub struct LineDiffRenderer {
    anchor: RenderAnchor,
    previous_width: u16,
    previous_lines: Vec<String>,
}

impl LineDiffRenderer {
    pub fn new(anchor: RenderAnchor) -> Self {
        Self {
            anchor,
            previous_width: 0,
            previous_lines: Vec::new(),
        }
    }

    pub fn anchor(&self) -> RenderAnchor {
        self.anchor
    }

    pub fn set_anchor(&mut self, anchor: RenderAnchor) {
        self.anchor = anchor;
    }

    pub fn render<T: Terminal>(
        &mut self,
        terminal: &mut T,
        output: &RenderOutput,
        width: u16,
    ) -> io::Result<()> {
        let capabilities = terminal.capabilities();
        let (_, terminal_height) = terminal.size()?;
        let available_height = terminal_height.saturating_sub(self.anchor.row) as usize;
        let materialized = materialize_output_lines(&output.lines, width, &capabilities);
        let all_lines = materialized.lines;
        let clip_start = if available_height == 0 {
            all_lines.len()
        } else {
            all_lines.len().saturating_sub(available_height)
        };
        let current_lines = all_lines[clip_start..].to_vec();
        let visible_cursor_row = output.cursor.and_then(|cursor| {
            let physical_row = materialized
                .logical_row_offsets
                .get(cursor.row as usize)
                .copied()
                .unwrap_or(all_lines.len());
            (physical_row >= clip_start).then_some(physical_row - clip_start)
        });

        let full_redraw =
            self.previous_width != width || current_lines.len() < self.previous_lines.len();
        let first_changed = if full_redraw {
            Some(0)
        } else {
            current_lines
                .iter()
                .zip(self.previous_lines.iter())
                .position(|(left, right)| left != right)
        };

        let extra_previous = self
            .previous_lines
            .len()
            .saturating_sub(current_lines.len());

        terminal.write(SYNC_UPDATES_START)?;
        if let Some(first) = first_changed {
            for (index, line) in current_lines.iter().enumerate().skip(first) {
                terminal.move_to(
                    self.anchor.col,
                    self.anchor.row.saturating_add(index as u16),
                )?;
                terminal.clear_line()?;
                terminal.write(line)?;
                terminal.write(RESET_LINE_STATE)?;
            }

            for index in 0..extra_previous {
                let row = self
                    .anchor
                    .row
                    .saturating_add((current_lines.len() + index) as u16);
                terminal.move_to(self.anchor.col, row)?;
                terminal.clear_line()?;
            }
        }

        if let Some((cursor, physical_row)) = output.cursor.zip(visible_cursor_row) {
            terminal.move_to(
                self.anchor.col.saturating_add(cursor.col),
                self.anchor.row.saturating_add(physical_row as u16),
            )?;
        } else if current_lines.is_empty() {
            terminal.move_to(self.anchor.col, self.anchor.row)?;
        } else {
            terminal.move_to(
                self.anchor.col,
                self.anchor
                    .row
                    .saturating_add(current_lines.len().saturating_sub(1) as u16),
            )?;
        }
        terminal.write(SYNC_UPDATES_END)?;
        terminal.flush()?;

        self.previous_width = width;
        self.previous_lines = current_lines;
        Ok(())
    }

    pub fn clear<T: Terminal>(&mut self, terminal: &mut T) -> io::Result<()> {
        for index in 0..self.previous_lines.len() {
            terminal.move_to(
                self.anchor.col,
                self.anchor.row.saturating_add(index as u16),
            )?;
            terminal.clear_line()?;
        }
        terminal.move_to(self.anchor.col, self.anchor.row)?;
        terminal.flush()?;
        self.previous_lines.clear();
        self.previous_width = 0;
        Ok(())
    }
}

struct MaterializedRender {
    lines: Vec<String>,
    logical_row_offsets: Vec<usize>,
}

fn materialize_output_lines(
    lines: &[RenderedLine],
    width: u16,
    capabilities: &TerminalCapabilities,
) -> MaterializedRender {
    let mut rendered = Vec::new();
    let mut logical_row_offsets = Vec::with_capacity(lines.len() + 1);

    for line in lines {
        logical_row_offsets.push(rendered.len());
        match line {
            RenderedLine::Text(text) => rendered.push(fit_line(text, width)),
            RenderedLine::Image(image) => {
                rendered.extend(render_image_lines(image, width, capabilities))
            }
        }
    }

    logical_row_offsets.push(rendered.len());
    MaterializedRender {
        lines: rendered,
        logical_row_offsets,
    }
}

fn render_image_lines(
    image: &ImageLine,
    width: u16,
    capabilities: &TerminalCapabilities,
) -> Vec<String> {
    let Some(protocol) = capabilities.image_protocol else {
        return vec![fit_line(&format!("[image] {}", image.alt_text), width)];
    };
    let Some(data) = image.data.as_deref() else {
        return vec![fit_line(&format!("[image] {}", image.alt_text), width)];
    };
    let mime_type = image.mime_type.as_deref().unwrap_or("image/png");
    let dimensions = image_dimensions(data, mime_type).unwrap_or(ImageDimensions {
        width_px: 800,
        height_px: 600,
    });
    let max_width = width.saturating_sub(2).max(1);
    let rows = calculate_image_rows(&dimensions, max_width);
    let Some(sequence) = render_image_sequence(protocol, data, mime_type, max_width, rows) else {
        return vec![fit_line(&format!("[image] {}", image.alt_text), width)];
    };

    let mut lines = Vec::new();
    for _ in 0..rows.saturating_sub(1) {
        lines.push(" ".repeat(width as usize));
    }
    let move_up = if rows > 1 {
        format!("\u{1b}[{}A", rows - 1)
    } else {
        String::new()
    };
    lines.push(format!("{move_up}{sequence}"));
    lines
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImageDimensions {
    width_px: u32,
    height_px: u32,
}

fn calculate_image_rows(dimensions: &ImageDimensions, width_cells: u16) -> u16 {
    let width_px = u32::from(width_cells).saturating_mul(9).max(1);
    let scaled_height = dimensions
        .height_px
        .saturating_mul(width_px)
        .checked_div(dimensions.width_px.max(1))
        .unwrap_or(dimensions.height_px.max(1));
    let rows = (scaled_height.saturating_add(17)) / 18;
    rows.max(1).min(u32::from(u16::MAX)) as u16
}

fn image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    let buffer = base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .ok()?;
    match mime_type {
        "image/png" => png_dimensions(&buffer),
        "image/jpeg" => jpeg_dimensions(&buffer),
        "image/gif" => gif_dimensions(&buffer),
        _ => None,
    }
}

fn png_dimensions(buffer: &[u8]) -> Option<ImageDimensions> {
    if buffer.len() < 24 || &buffer[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    Some(ImageDimensions {
        width_px: u32::from_be_bytes(buffer[16..20].try_into().ok()?),
        height_px: u32::from_be_bytes(buffer[20..24].try_into().ok()?),
    })
}

fn jpeg_dimensions(buffer: &[u8]) -> Option<ImageDimensions> {
    if buffer.len() < 4 || buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }
    let mut offset = 2usize;
    while offset + 9 < buffer.len() {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = buffer[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            return Some(ImageDimensions {
                height_px: u16::from_be_bytes(buffer[offset + 5..offset + 7].try_into().ok()?)
                    as u32,
                width_px: u16::from_be_bytes(buffer[offset + 7..offset + 9].try_into().ok()?)
                    as u32,
            });
        }
        let length = u16::from_be_bytes(buffer[offset + 2..offset + 4].try_into().ok()?) as usize;
        if length < 2 {
            return None;
        }
        offset = offset.saturating_add(2 + length);
    }
    None
}

fn gif_dimensions(buffer: &[u8]) -> Option<ImageDimensions> {
    if buffer.len() < 10 || (&buffer[0..6] != b"GIF87a" && &buffer[0..6] != b"GIF89a") {
        return None;
    }
    Some(ImageDimensions {
        width_px: u16::from_le_bytes(buffer[6..8].try_into().ok()?) as u32,
        height_px: u16::from_le_bytes(buffer[8..10].try_into().ok()?) as u32,
    })
}

fn render_image_sequence(
    protocol: ImageProtocol,
    base64_data: &str,
    mime_type: &str,
    width_cells: u16,
    rows: u16,
) -> Option<String> {
    match protocol {
        ImageProtocol::Kitty => Some(encode_kitty(base64_data, width_cells, rows)),
        ImageProtocol::ITerm2 => Some(encode_iterm2(base64_data, mime_type, width_cells)),
    }
}

fn encode_kitty(base64_data: &str, width_cells: u16, rows: u16) -> String {
    const CHUNK_SIZE: usize = 4096;
    let params = format!("a=T,f=100,q=2,c={width_cells},r={rows}");
    if base64_data.len() <= CHUNK_SIZE {
        return format!("\u{1b}_G{params};{base64_data}\u{1b}\\");
    }

    let mut chunks = Vec::new();
    let mut offset = 0usize;
    let mut first = true;
    while offset < base64_data.len() {
        let end = (offset + CHUNK_SIZE).min(base64_data.len());
        let chunk = &base64_data[offset..end];
        let last = end == base64_data.len();
        if first {
            chunks.push(format!("\u{1b}_G{params},m=1;{chunk}\u{1b}\\"));
            first = false;
        } else if last {
            chunks.push(format!("\u{1b}_Gm=0;{chunk}\u{1b}\\"));
        } else {
            chunks.push(format!("\u{1b}_Gm=1;{chunk}\u{1b}\\"));
        }
        offset = end;
    }
    chunks.join("")
}

fn encode_iterm2(base64_data: &str, mime_type: &str, width_cells: u16) -> String {
    let name = mime_type.replace('/', "_");
    let name = base64::engine::general_purpose::STANDARD.encode(name);
    format!(
        "\u{1b}]1337;File=inline=1;width={width_cells};height=auto;preserveAspectRatio=1;name={name}:{base64_data}\u{7}"
    )
}

pub fn fit_line(text: &str, width: u16) -> String {
    let truncated = truncate_to_width(text, width as usize);
    let visible = visible_width(&truncated);
    if visible >= width as usize {
        return truncated;
    }

    format!("{truncated}{}", " ".repeat(width as usize - visible))
}

pub fn truncate_to_width(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let mut truncated = String::new();
    let mut visible = 0usize;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            truncated.push(ch);
            while let Some(next) = chars.next() {
                truncated.push(next);
                if next.is_ascii_alphabetic() || next == '\u{7}' {
                    break;
                }
            }
            continue;
        }

        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if visible + char_width > width {
            break;
        }

        truncated.push(ch);
        visible += char_width;
    }

    truncated
}

pub fn visible_width(text: &str) -> usize {
    let mut width = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if matches!(chars.peek(), Some(']')) {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                }
            } else {
                while let Some(next) = chars.next() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }

        width += UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    width
}

#[cfg(test)]
mod tests {
    use super::{
        CursorPosition, ImageLine, RenderAnchor, RenderOutput, RenderedLine, fit_line,
        truncate_to_width, visible_width,
    };
    use crate::terminal::{ImageProtocol, Terminal, TerminalCapabilities};
    use std::io;

    struct RecordingTerminal {
        writes: Vec<String>,
        moves: Vec<(u16, u16)>,
        capabilities: TerminalCapabilities,
    }

    impl Default for RecordingTerminal {
        fn default() -> Self {
            Self {
                writes: Vec::new(),
                moves: Vec::new(),
                capabilities: TerminalCapabilities::default(),
            }
        }
    }

    impl Terminal for RecordingTerminal {
        fn start(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn stop(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) -> io::Result<()> {
            Ok(())
        }

        fn read_events(&mut self) -> io::Result<Vec<crate::KeyEvent>> {
            Ok(Vec::new())
        }

        fn write(&mut self, data: &str) -> io::Result<()> {
            self.writes.push(data.to_string());
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn size(&self) -> io::Result<(u16, u16)> {
            Ok((80, 24))
        }

        fn cursor_position(&self) -> io::Result<(u16, u16)> {
            Ok((0, 0))
        }

        fn move_to(&mut self, col: u16, row: u16) -> io::Result<()> {
            self.moves.push((col, row));
            Ok(())
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn clear_line(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn clear_from_cursor(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn clear_screen(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn set_title(&mut self, _title: &str) -> io::Result<()> {
            Ok(())
        }

        fn capabilities(&self) -> TerminalCapabilities {
            self.capabilities.clone()
        }
    }

    #[test]
    fn visible_width_ignores_ansi_sequences() {
        assert_eq!(visible_width("\u{1b}[31mred\u{1b}[0m"), 3);
    }

    #[test]
    fn truncation_respects_unicode_width() {
        assert_eq!(truncate_to_width("abc世界", 5), "abc世");
    }

    #[test]
    fn fit_line_pads_short_lines() {
        assert_eq!(fit_line("hi", 5), "hi   ");
    }

    #[test]
    fn renderer_rewrites_changed_lines_only() {
        let mut terminal = RecordingTerminal::default();
        let mut renderer = super::LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });
        renderer
            .render(
                &mut terminal,
                &RenderOutput {
                    lines: vec![RenderedLine::Text("first".to_string())],
                    cursor: None,
                },
                10,
            )
            .expect("initial render");
        let move_count = terminal.moves.len();

        renderer
            .render(
                &mut terminal,
                &RenderOutput {
                    lines: vec![RenderedLine::Text("first".to_string())],
                    cursor: None,
                },
                10,
            )
            .expect("second render");

        assert_eq!(terminal.moves.len(), move_count + 1);
    }

    #[test]
    fn renderer_uses_inline_image_sequences_when_supported() {
        let mut terminal = RecordingTerminal {
            capabilities: TerminalCapabilities {
                kitty_keyboard: false,
                inline_images: true,
                image_protocol: Some(ImageProtocol::Kitty),
                hyperlinks: true,
            },
            ..Default::default()
        };
        let mut renderer = super::LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });

        renderer
            .render(
                &mut terminal,
                &RenderOutput {
                    lines: vec![RenderedLine::Image(ImageLine {
                        alt_text: "image/png".to_string(),
                        mime_type: Some("image/png".to_string()),
                        data: Some(
                            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO7Z0k8AAAAASUVORK5CYII="
                                .to_string(),
                        ),
                    })],
                    cursor: None,
                },
                40,
            )
            .expect("render image");

        assert!(
            terminal
                .writes
                .iter()
                .any(|write| write.contains("\u{1b}_G"))
        );
    }

    #[test]
    fn renderer_falls_back_to_text_for_unsupported_image_terminals() {
        let mut terminal = RecordingTerminal::default();
        let mut renderer = super::LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });

        renderer
            .render(
                &mut terminal,
                &RenderOutput {
                    lines: vec![RenderedLine::Image(ImageLine {
                        alt_text: "image/png".to_string(),
                        mime_type: Some("image/png".to_string()),
                        data: Some("AAAA".to_string()),
                    })],
                    cursor: None,
                },
                20,
            )
            .expect("render fallback image");

        assert!(
            terminal
                .writes
                .iter()
                .any(|write| write.contains("[image] image/png"))
        );
    }

    #[test]
    fn renderer_clips_to_terminal_height_and_keeps_cursor_visible() {
        let mut terminal = RecordingTerminal::default();
        let mut renderer = super::LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });
        let lines = (0..30)
            .map(|index| RenderedLine::Text(format!("line {index}")))
            .collect();

        renderer
            .render(
                &mut terminal,
                &RenderOutput {
                    lines,
                    cursor: Some(CursorPosition { row: 29, col: 4 }),
                },
                20,
            )
            .expect("render clipped viewport");

        assert_eq!(renderer.previous_lines.len(), 24);
        assert_eq!(
            renderer.previous_lines.first(),
            Some(&fit_line("line 6", 20))
        );
        assert_eq!(terminal.moves.last().copied(), Some((4, 23)));
    }

    #[test]
    fn renderer_does_not_move_cursor_below_viewport_without_cursor_marker() {
        let mut terminal = RecordingTerminal::default();
        let mut renderer = super::LineDiffRenderer::new(RenderAnchor { col: 0, row: 0 });
        let lines = (0..30)
            .map(|index| RenderedLine::Text(format!("line {index}")))
            .collect();

        renderer
            .render(
                &mut terminal,
                &RenderOutput {
                    lines,
                    cursor: None,
                },
                20,
            )
            .expect("render clipped viewport");

        assert_eq!(terminal.moves.last().copied(), Some((0, 23)));
    }
}
