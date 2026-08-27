//! Shared fixtures for the render tests: a scriptable grid mirror and a theme.
#![allow(dead_code)]

use soksak_kit_sidecar_terminal::mirror::{
    MirrorCapabilities, TerminalCell, TerminalColor, TerminalFrame, TerminalModes,
};
use soksak_kit_sidecar_terminal::render::instances::{pack_bgra, Palette};
use soksak_kit_sidecar_terminal::TerminalStateMirror;

pub fn plain(ch: char) -> TerminalCell {
    TerminalCell {
        ch,
        fg: TerminalColor::Default,
        bg: TerminalColor::Default,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        inverse: false,
        strikeout: false,
        hidden: false,
        wide: false,
        spacer: false,
        wrapline: false,
        zerowidth: Vec::new(),
        link: None,
    }
}

pub struct GridMirror {
    pub cols: u16,
    pub grid: Vec<Vec<TerminalCell>>,
    pub cursor: (usize, usize),
}

impl GridMirror {
    pub fn from_rows(cols: u16, rows: &[&str]) -> Self {
        let grid = rows
            .iter()
            .map(|text| {
                let mut cells: Vec<TerminalCell> = Vec::new();
                for ch in text.chars() {
                    if ch == '\u{0}' {
                        let mut spacer = plain(' ');
                        spacer.spacer = true;
                        cells.push(spacer);
                        continue;
                    }
                    let mut cell = plain(ch);
                    if (ch as u32) >= 0x1100 {
                        cell.wide = true;
                    }
                    cells.push(cell);
                }
                while cells.len() < cols as usize {
                    cells.push(plain(' '));
                }
                cells
            })
            .collect();
        Self { cols, grid, cursor: (0, 0) }
    }
}

impl TerminalStateMirror for GridMirror {
    fn feed(&mut self, _bytes: &[u8]) {}
    fn resize(&mut self, _cols: u16, _rows: u16) {}
    fn rehydrate(&self) -> Vec<u8> {
        Vec::new()
    }
    fn cold_paint(&self) -> Vec<u8> {
        Vec::new()
    }
    fn frame_at(&self, offset: usize) -> TerminalFrame {
        TerminalFrame {
            cols: self.cols,
            rows: self.grid.len() as u16,
            cursor: self.cursor,
            cursor_visible: false,
            alt_active: false,
            history_size: 0,
            offset,
            modes: TerminalModes::default(),
            lines: Vec::new(),
        }
    }
    fn history_size(&self) -> usize {
        0
    }
    fn modes(&self) -> TerminalModes {
        TerminalModes::default()
    }
    fn capabilities(&self) -> MirrorCapabilities {
        MirrorCapabilities::default()
    }
    fn alt_active(&self) -> bool {
        false
    }
    fn suppressed_replies(&self) -> u64 {
        0
    }
    fn cols(&self) -> u16 {
        self.cols
    }
    fn rows(&self) -> u16 {
        self.grid.len() as u16
    }
    fn cursor(&self) -> (usize, usize) {
        self.cursor
    }
    fn line_cells(&self, line: i32) -> Vec<TerminalCell> {
        if line < 0 {
            return Vec::new();
        }
        self.grid.get(line as usize).cloned().unwrap_or_default()
    }
}

pub fn palette() -> Palette {
    Palette {
        fg: pack_bgra(230, 230, 230, 255),
        bg: pack_bgra(10, 10, 10, 255),
        ansi: [pack_bgra(10, 10, 10, 255); 256],
    }
}

/// Ink pixels inside a cell-rect: anything that is not the dark background.
pub fn ink_in_cells(
    pixels: &[u8],
    pixel_width: u32,
    cell: (u16, u16),
    col_range: std::ops::Range<u16>,
    row: u16,
) -> u64 {
    let mut ink = 0;
    for y in (row as u32 * cell.1 as u32)..((row as u32 + 1) * cell.1 as u32) {
        for x in (col_range.start as u32 * cell.0 as u32)..(col_range.end as u32 * cell.0 as u32) {
            let offset = ((y * pixel_width + x) * 4) as usize;
            let pixel = &pixels[offset..offset + 3];
            if pixel.iter().any(|&channel| channel > 32) {
                ink += 1;
            }
        }
    }
    ink
}

/// A grid mirror the test can keep mutating while the runtime holds it as a
/// boxed trait object.
pub struct SharedGrid(pub std::sync::Arc<std::sync::Mutex<GridMirror>>);

impl TerminalStateMirror for SharedGrid {
    fn feed(&mut self, bytes: &[u8]) {
        self.0.lock().unwrap().feed(bytes);
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        self.0.lock().unwrap().resize(cols, rows);
    }
    fn rehydrate(&self) -> Vec<u8> {
        self.0.lock().unwrap().rehydrate()
    }
    fn cold_paint(&self) -> Vec<u8> {
        self.0.lock().unwrap().cold_paint()
    }
    fn frame_at(&self, offset: usize) -> TerminalFrame {
        self.0.lock().unwrap().frame_at(offset)
    }
    fn history_size(&self) -> usize {
        self.0.lock().unwrap().history_size()
    }
    fn modes(&self) -> TerminalModes {
        self.0.lock().unwrap().modes()
    }
    fn capabilities(&self) -> MirrorCapabilities {
        self.0.lock().unwrap().capabilities()
    }
    fn alt_active(&self) -> bool {
        self.0.lock().unwrap().alt_active()
    }
    fn suppressed_replies(&self) -> u64 {
        self.0.lock().unwrap().suppressed_replies()
    }
    fn cols(&self) -> u16 {
        self.0.lock().unwrap().cols()
    }
    fn rows(&self) -> u16 {
        self.0.lock().unwrap().rows()
    }
    fn cursor(&self) -> (usize, usize) {
        self.0.lock().unwrap().cursor()
    }
    fn line_cells(&self, line: i32) -> Vec<TerminalCell> {
        self.0.lock().unwrap().line_cells(line)
    }
}
