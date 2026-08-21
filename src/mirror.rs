#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColor {
    Default,
    Named(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    fn alt_active(&self) -> bool;
    fn history_size(&self) -> usize;
    fn modes(&self) -> TerminalModes;
    fn line_cells(&self, line: i32) -> Vec<TerminalCell>;
    fn suppressed_replies(&self) -> u64;
}

pub struct RecoveryMirror<E: TerminalEngine> {
    engine: E,
    frozen_primary: Option<FrozenPrimary>,
    held: Vec<u8>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct TerminalFrame {
    pub cols: u16,
    pub rows: u16,
    pub cursor: (usize, usize),
    pub alt_active: bool,
    pub lines: Vec<Vec<FrameCell>>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FrameCell {
    pub text: String,
    pub fg: String,
    pub bg: String,
    pub attrs: u16,
    pub wide: bool,
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
    pub fn modes(&self) -> TerminalModes {
        self.engine.modes()
    }
    pub fn history_size(&self) -> usize {
        self.engine.history_size()
    }
    pub fn line_cells(&self, line: i32) -> Vec<TerminalCell> {
        self.engine.line_cells(line)
    }
    pub fn frame(&self) -> TerminalFrame {
        let lines = (0..self.engine.rows() as i32)
            .map(|line| {
                self.engine
                    .line_cells(line)
                    .into_iter()
                    .filter(|cell| !cell.spacer)
                    .map(|cell| {
                        let mut text = String::new();
                        text.push(cell.ch);
                        text.extend(cell.zerowidth);
                        let attrs = (cell.bold as u16)
                            | ((cell.dim as u16) << 1)
                            | ((cell.italic as u16) << 2)
                            | ((cell.underline as u16) << 3)
                            | ((cell.inverse as u16) << 4)
                            | ((cell.strikeout as u16) << 5)
                            | ((cell.hidden as u16) << 6);
                        FrameCell {
                            text,
                            fg: frame_color(cell.fg),
                            bg: frame_color(cell.bg),
                            attrs,
                            wide: cell.wide,
                        }
                    })
                    .collect()
            })
            .collect();
        TerminalFrame {
            cols: self.engine.cols(),
            rows: self.engine.rows(),
            cursor: self.engine.cursor(),
            alt_active: self.engine.alt_active(),
            lines,
        }
    }
}
fn frame_color(color: TerminalColor) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

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
