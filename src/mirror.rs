use crate::frame::encode_line;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColor {
    Default,
    Named(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalCursorShape {
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCursorStyle {
    pub shape: TerminalCursorShape,
    pub blinking: bool,
    /// Renderer policy selected by this provider. Zero means the provider has
    /// no animation interval and therefore cannot schedule blinking.
    pub blink_interval_ms: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalModes {
    pub bracketed_paste: bool,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub mouse_click: bool,
    pub mouse_drag: bool,
    pub mouse_motion: bool,
    pub sgr_mouse: bool,
    pub utf8_mouse: bool,
    pub focus_in_out: bool,
    pub alternate_scroll: bool,
    pub show_cursor: bool,
    pub line_wrap: bool,
    pub insert: bool,
}

/// What the engine behind a mirror can report on a cell. Published once in `terminal.status`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorCapabilities {
    pub hyperlinks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCell {
    pub ch: char,
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strikeout: bool,
    pub hidden: bool,
    pub wide: bool,
    pub spacer: bool,
    pub wrapline: bool,
    pub zerowidth: Vec<char>,
    /// Hyperlink target (OSC 8), when the engine tracks one.
    pub link: Option<String>,
}

pub trait TerminalEngine: Send + Sized {
    fn new(cols: u16, rows: u16) -> Self;
    fn initialize(&mut self) {}
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16);
    fn resize_with_replay(&mut self, cols: u16, rows: u16, _replay: &[u8]) {
        self.resize(cols, rows);
    }
    fn cols(&self) -> u16;
    fn rows(&self) -> u16;
    fn cursor(&self) -> (usize, usize);
    fn cursor_style(&self) -> TerminalCursorStyle;
    fn alt_active(&self) -> bool;
    fn history_size(&self) -> usize;
    fn modes(&self) -> TerminalModes;
    fn line_cells(&self, line: i32) -> Vec<TerminalCell>;
    /// Every viewport row with the view scrolled `offset` rows into history: row `y` is engine
    /// line `y - offset`. `offset` is already clamped by the caller. Engines with a cheaper
    /// consecutive read override this.
    fn viewport_cells(&self, offset: usize) -> Vec<Vec<TerminalCell>> {
        let offset = i32::try_from(offset).unwrap_or(i32::MAX);
        (0..self.rows() as i32)
            .map(|y| self.line_cells(y - offset))
            .collect()
    }
    fn capabilities(&self) -> MirrorCapabilities {
        MirrorCapabilities::default()
    }
    fn suppressed_replies(&self) -> u64;
}

pub struct RecoveryMirror<E: TerminalEngine> {
    engine: E,
    frozen_primary: Option<FrozenPrimary>,
    held: Vec<u8>,
}

/// The viewport as runs. Checkpoints store it; `terminal.frame` derives deltas from it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFrame {
    pub cols: u16,
    pub rows: u16,
    /// `(row, col)`, 0-based.
    pub cursor: (usize, usize),
    pub cursor_visible: bool,
    pub cursor_style: TerminalCursorStyle,
    pub alt_active: bool,
    pub history_size: usize,
    pub offset: usize,
    pub modes: TerminalModes,
    pub lines: Vec<FrameLine>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FrameLine {
    pub y: u16,
    pub wrapped: bool,
    pub runs: Vec<FrameRun>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FrameRun {
    pub text: String,
    pub fg: String,
    pub bg: String,
    pub attrs: u16,
    pub n: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub wide: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

struct FrozenPrimary {
    paint: Vec<u8>,
    cursor: (usize, usize),
}

enum AltCandidate {
    NeedMore,
    Enter(usize),
    No,
}

impl<E: TerminalEngine> RecoveryMirror<E> {
    pub fn new(cols: u16, rows: u16) -> Self {
        let mut engine = E::new(cols.max(1), rows.max(1));
        engine.initialize();
        Self {
            engine,
            frozen_primary: None,
            held: Vec::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut data = std::mem::take(&mut self.held);
        data.extend_from_slice(bytes);
        let mut fed = 0;
        let mut index = 0;
        while index < data.len() {
            if data[index] != 0x1b {
                index += 1;
                continue;
            }
            match classify_alt_enter(&data[index..]) {
                AltCandidate::NeedMore => {
                    self.engine.feed(&data[fed..index]);
                    self.held.extend_from_slice(&data[index..]);
                    return;
                }
                AltCandidate::Enter(length) => {
                    self.engine.feed(&data[fed..index]);
                    if !self.engine.alt_active() {
                        self.frozen_primary = Some(FrozenPrimary {
                            paint: paint_primary(&self.engine),
                            cursor: self.engine.cursor(),
                        });
                    }
                    self.engine.feed(&data[index..index + length]);
                    fed = index + length;
                    index = fed;
                }
                AltCandidate::No => index += 1,
            }
        }
        self.engine.feed(&data[fed..]);
        if !self.engine.alt_active() {
            self.frozen_primary = None;
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let replay = self.rehydrate();
        self.engine
            .resize_with_replay(cols.max(1), rows.max(1), &replay);
    }

    pub fn rehydrate(&self) -> Vec<u8> {
        let mut output = b"\x1b[0m".to_vec();
        if self.engine.alt_active() {
            if let Some(primary) = &self.frozen_primary {
                output.extend_from_slice(&primary.paint);
                output.extend(cup(primary.cursor));
            }
            output.extend_from_slice(b"\x1b[?1049h");
            output.extend(paint_alt(&self.engine));
            output.extend(cup(self.engine.cursor()));
        } else {
            output.extend(paint_primary(&self.engine));
            output.extend(cup(self.engine.cursor()));
        }
        output.extend(mode_sets(self.engine.modes()));
        output.extend(cursor_style_set(self.engine.cursor_style()));
        output
    }

    pub fn cold_paint(&self) -> Vec<u8> {
        let mut output = b"\x1b[0m".to_vec();
        if self.engine.alt_active() {
            if let Some(primary) = &self.frozen_primary {
                output.extend_from_slice(&primary.paint);
            }
            output.extend_from_slice(b"\r\n");
            output.extend(paint_alt_flat(&self.engine));
        } else {
            output.extend(paint_primary(&self.engine));
        }
        output.extend_from_slice(b"\x1b[0m\r\n");
        output
    }

    pub fn alt_active(&self) -> bool {
        self.engine.alt_active()
    }
    pub fn suppressed_replies(&self) -> u64 {
        self.engine.suppressed_replies()
    }
    pub fn cols(&self) -> u16 {
        self.engine.cols()
    }
    pub fn rows(&self) -> u16 {
        self.engine.rows()
    }
    pub fn cursor(&self) -> (usize, usize) {
        self.engine.cursor()
    }
    pub fn cursor_style(&self) -> TerminalCursorStyle {
        self.engine.cursor_style()
    }
    pub fn modes(&self) -> TerminalModes {
        self.engine.modes()
    }
    pub fn history_size(&self) -> usize {
        self.engine.history_size()
    }
    pub fn capabilities(&self) -> MirrorCapabilities {
        self.engine.capabilities()
    }
    pub fn line_cells(&self, line: i32) -> Vec<TerminalCell> {
        self.engine.line_cells(line)
    }

    /// The viewport scrolled `offset` rows into history. Clamped to the history size; 0 while
    /// the alternate screen is active. The effective offset is echoed in the frame.
    pub fn frame_at(&self, offset: usize) -> TerminalFrame {
        let alt_active = self.engine.alt_active();
        let history_size = self.engine.history_size();
        let offset = if alt_active {
            0
        } else {
            offset.min(history_size)
        };
        let modes = self.engine.modes();
        let lines = self
            .engine
            .viewport_cells(offset)
            .iter()
            .enumerate()
            .map(|(y, cells)| encode_line(y as u16, cells))
            .collect();
        TerminalFrame {
            cols: self.engine.cols(),
            rows: self.engine.rows(),
            cursor: self.engine.cursor(),
            cursor_visible: modes.show_cursor,
            cursor_style: self.engine.cursor_style(),
            alt_active,
            history_size,
            offset,
            modes,
            lines,
        }
    }

    pub fn frame(&self) -> TerminalFrame {
        self.frame_at(0)
    }
}

pub(crate) fn frame_color(color: TerminalColor) -> String {
    match color {
        TerminalColor::Default => "default".into(),
        TerminalColor::Named(i) | TerminalColor::Indexed(i) => format!("palette:{i}"),
        TerminalColor::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn classify_alt_enter(bytes: &[u8]) -> AltCandidate {
    if bytes.len() < 2 {
        return AltCandidate::NeedMore;
    }
    if bytes[1] != b'[' {
        return AltCandidate::No;
    }
    if bytes.len() < 3 {
        return AltCandidate::NeedMore;
    }
    if bytes[2] != b'?' {
        return AltCandidate::No;
    }
    let mut end = 3;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b';') {
        end += 1;
        if end - 3 > 32 {
            return AltCandidate::No;
        }
    }
    if end == bytes.len() {
        return AltCandidate::NeedMore;
    }
    if bytes[end] != b'h' {
        return AltCandidate::No;
    }
    if bytes[3..end]
        .split(|byte| *byte == b';')
        .any(|mode| mode == b"47" || mode == b"1047" || mode == b"1049")
    {
        AltCandidate::Enter(end + 1)
    } else {
        AltCandidate::No
    }
}

#[derive(Default, PartialEq, Eq)]
struct Sgr {
    foreground: Option<String>,
    background: Option<String>,
    attributes: Vec<&'static str>,
}

fn cup((row, column): (usize, usize)) -> Vec<u8> {
    format!("\x1b[{};{}H", row + 1, column + 1).into_bytes()
}

fn is_blank(cell: &TerminalCell) -> bool {
    cell.ch == ' '
        && cell.zerowidth.is_empty()
        && cell.fg == TerminalColor::Default
        && cell.bg == TerminalColor::Default
        && !cell.bold
        && !cell.dim
        && !cell.italic
        && !cell.underline
        && !cell.inverse
        && !cell.strikeout
        && !cell.hidden
}

fn sgr(cell: &TerminalCell) -> Sgr {
    let mut attributes = Vec::new();
    if cell.bold {
        attributes.push("1");
    }
    if cell.dim {
        attributes.push("2");
    }
    if cell.italic {
        attributes.push("3");
    }
    if cell.underline {
        attributes.push("4");
    }
    if cell.inverse {
        attributes.push("7");
    }
    if cell.hidden {
        attributes.push("8");
    }
    if cell.strikeout {
        attributes.push("9");
    }
    Sgr {
        foreground: color_code(cell.fg, false),
        background: color_code(cell.bg, true),
        attributes,
    }
}

fn color_code(color: TerminalColor, background: bool) -> Option<String> {
    let base = if background { 40 } else { 30 };
    let bright = if background { 100 } else { 90 };
    let extended = if background { 48 } else { 38 };
    match color {
        TerminalColor::Default => None,
        TerminalColor::Named(index) if index < 8 => Some((base + index as usize).to_string()),
        TerminalColor::Named(index) if index < 16 => {
            Some((bright + index as usize - 8).to_string())
        }
        TerminalColor::Named(_) => None,
        TerminalColor::Indexed(index) => Some(format!("{extended};5;{index}")),
        TerminalColor::Rgb(red, green, blue) => Some(format!("{extended};2;{red};{green};{blue}")),
    }
}

fn emit_sgr(output: &mut Vec<u8>, value: &Sgr) {
    let mut parts = vec!["0".to_string()];
    parts.extend(value.attributes.iter().map(|value| (*value).to_string()));
    if let Some(value) = &value.foreground {
        parts.push(value.clone());
    }
    if let Some(value) = &value.background {
        parts.push(value.clone());
    }
    output.extend(format!("\x1b[{}m", parts.join(";")).into_bytes());
}

fn paint_row<E: TerminalEngine>(
    output: &mut Vec<u8>,
    engine: &E,
    line: i32,
    current: &mut Sgr,
) -> bool {
    let cells = engine.line_cells(line);
    let wrapped = cells.last().is_some_and(|cell| cell.wrapline);
    let mut end = cells.len();
    if !wrapped {
        while end > 0 && is_blank(&cells[end - 1]) {
            end -= 1;
        }
    }
    for cell in cells.iter().take(end).filter(|cell| !cell.spacer) {
        let next = sgr(cell);
        if next != *current {
            emit_sgr(output, &next);
            *current = next;
        }
        let mut encoded = [0; 4];
        output.extend_from_slice(cell.ch.encode_utf8(&mut encoded).as_bytes());
        for codepoint in &cell.zerowidth {
            output.extend_from_slice(codepoint.encode_utf8(&mut encoded).as_bytes());
        }
    }
    wrapped
}

fn paint_primary<E: TerminalEngine>(engine: &E) -> Vec<u8> {
    let mut output = Vec::new();
    let mut current = Sgr::default();
    let history = engine.history_size() as i32;
    let rows = engine.rows() as i32;
    for line in -history..rows {
        if !paint_row(&mut output, engine, line, &mut current) && line != rows - 1 {
            output.extend_from_slice(b"\x1b[0m\r\n");
            current = Sgr::default();
        }
    }
    output.extend_from_slice(b"\x1b[0m");
    output
}

fn paint_alt<E: TerminalEngine>(engine: &E) -> Vec<u8> {
    let mut output = b"\x1b[2J".to_vec();
    let mut current = Sgr::default();
    for line in 0..engine.rows() as i32 {
        let row_start = output.len();
        output.extend(format!("\x1b[{};1H", line + 1).into_bytes());
        let content_start = output.len();
        paint_row(&mut output, engine, line, &mut current);
        if output.len() == content_start {
            output.truncate(row_start);
        }
    }
    output.extend_from_slice(b"\x1b[0m");
    output
}

fn paint_alt_flat<E: TerminalEngine>(engine: &E) -> Vec<u8> {
    let mut rows = Vec::new();
    let mut current = Sgr::default();
    for line in 0..engine.rows() as i32 {
        let mut row = Vec::new();
        paint_row(&mut row, engine, line, &mut current);
        rows.push(row);
    }
    while rows.last().is_some_and(Vec::is_empty) {
        rows.pop();
    }
    rows.join(b"\x1b[0m\r\n".as_slice())
}

fn mode_sets(modes: TerminalModes) -> Vec<u8> {
    let mut output = Vec::new();
    let values = [
        (modes.bracketed_paste, "\x1b[?2004h"),
        (modes.app_cursor, "\x1b[?1h"),
        (modes.app_keypad, "\x1b="),
        (modes.mouse_click, "\x1b[?1000h"),
        (modes.mouse_drag, "\x1b[?1002h"),
        (modes.mouse_motion, "\x1b[?1003h"),
        (modes.sgr_mouse, "\x1b[?1006h"),
        (modes.utf8_mouse, "\x1b[?1005h"),
        (modes.focus_in_out, "\x1b[?1004h"),
        (modes.insert, "\x1b[4h"),
        (modes.alternate_scroll, "\x1b[?1007h"),
        (!modes.line_wrap, "\x1b[?7l"),
        (!modes.show_cursor, "\x1b[?25l"),
    ];
    for (enabled, value) in values {
        if enabled {
            output.extend_from_slice(value.as_bytes());
        }
    }
    output
}

fn cursor_style_set(style: TerminalCursorStyle) -> Vec<u8> {
    let parameter = match (style.shape, style.blinking) {
        (TerminalCursorShape::Block, true) => 1,
        (TerminalCursorShape::Block, false) => 2,
        (TerminalCursorShape::Underline, true) => 3,
        (TerminalCursorShape::Underline, false) => 4,
        (TerminalCursorShape::Bar, true) => 5,
        (TerminalCursorShape::Bar, false) => 6,
    };
    format!("\x1b[{parameter} q").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HiddenCursorEngine;

    impl TerminalEngine for HiddenCursorEngine {
        fn new(_: u16, _: u16) -> Self {
            Self
        }
        fn feed(&mut self, _: &[u8]) {}
        fn resize(&mut self, _: u16, _: u16) {}
        fn cols(&self) -> u16 {
            1
        }
        fn rows(&self) -> u16 {
            1
        }
        fn cursor(&self) -> (usize, usize) {
            (0, 0)
        }
        fn cursor_style(&self) -> TerminalCursorStyle {
            TerminalCursorStyle {
                shape: TerminalCursorShape::Bar,
                blinking: false,
                blink_interval_ms: 750,
            }
        }
        fn alt_active(&self) -> bool {
            false
        }
        fn history_size(&self) -> usize {
            0
        }
        fn modes(&self) -> TerminalModes {
            TerminalModes {
                show_cursor: false,
                ..TerminalModes::default()
            }
        }
        fn line_cells(&self, _: i32) -> Vec<TerminalCell> {
            vec![]
        }
        fn suppressed_replies(&self) -> u64 {
            0
        }
    }

    #[test]
    fn frame_exposes_the_engine_cursor_visibility() {
        let frame = RecoveryMirror::<HiddenCursorEngine>::new(1, 1).frame();
        assert!(!frame.cursor_visible);
        let value = serde_json::to_value(frame).unwrap();
        assert_eq!(value["cursorVisible"], serde_json::Value::Bool(false));
        assert_eq!(value["modes"]["showCursor"], serde_json::Value::Bool(false));
        assert_eq!(value["cursorStyle"]["shape"], serde_json::Value::String("bar".into()));
        assert_eq!(value["cursorStyle"]["blinking"], serde_json::Value::Bool(false));
        assert!(value.get("cursor_visible").is_none());
        assert!(RecoveryMirror::<HiddenCursorEngine>::new(1, 1)
            .rehydrate()
            .windows(b"\x1b[6 q".len())
            .any(|bytes| bytes == b"\x1b[6 q"));
    }

    #[test]
    fn recognizes_alt_entry_across_chunks() {
        assert!(matches!(
            classify_alt_enter(b"\x1b[?104"),
            AltCandidate::NeedMore
        ));
        assert!(matches!(
            classify_alt_enter(b"\x1b[?1049h"),
            AltCandidate::Enter(8)
        ));
    }

    #[test]
    fn serializes_named_and_rgb_colors() {
        assert_eq!(
            color_code(TerminalColor::Named(9), false).as_deref(),
            Some("91")
        );
        assert_eq!(
            color_code(TerminalColor::Rgb(1, 2, 3), true).as_deref(),
            Some("48;2;1;2;3")
        );
    }
}
