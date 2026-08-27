//! The painter's testimony: pixels land where cells say, only dirty rows are
//! touched, wide glyphs cover both their cells, clean rows stay byte-stable.
#![cfg(target_os = "macos")]

use std::sync::Arc;

use soksak_kit_sidecar_terminal::mirror::{
    MirrorCapabilities, TerminalCell, TerminalColor, TerminalFrame, TerminalModes,
};
use soksak_kit_sidecar_terminal::render::instances::{pack_bgra, Palette};
use soksak_kit_sidecar_terminal::render::native::Canvas;
use soksak_kit_sidecar_terminal::render::painter::Painter;
use soksak_kit_sidecar_terminal::TerminalStateMirror;

fn plain(ch: char) -> TerminalCell {
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

struct GridMirror {
    cols: u16,
    grid: Vec<Vec<TerminalCell>>,
}

impl GridMirror {
    fn from_rows(cols: u16, rows: &[&str]) -> Self {
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
        Self { cols, grid }
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
            cursor: (0, 0),
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
        (0, 0)
    }
    fn line_cells(&self, line: i32) -> Vec<TerminalCell> {
        if line < 0 {
            return Vec::new();
        }
        self.grid.get(line as usize).cloned().unwrap_or_default()
    }
}

fn palette() -> Palette {
    Palette {
        fg: pack_bgra(230, 230, 230, 255),
        bg: pack_bgra(10, 10, 10, 255),
        ansi: [pack_bgra(10, 10, 10, 255); 256],
    }
}

fn painter(cols: u16, rows: u16) -> Painter {
    let canvas = Arc::new(Canvas::create().expect("a Metal device exists on this host"));
    Painter::new(canvas, "Menlo", 13.0, 2.0, cols, rows, palette()).expect("the painter builds")
}

/// Ink pixels inside a cell-rect: anything that is not the dark background.
fn ink_in_cells(
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

#[test]
fn a_painted_row_shows_its_glyphs_where_its_cells_are() {
    let mut painter = painter(8, 2);
    let mirror = GridMirror::from_rows(8, &["AB      ", "        "]);
    let dirty = painter.paint(&mirror, 0).expect("paints");
    assert_eq!(dirty, vec![0, 1], "the first paint owes every row");
    let pixels = painter.read_pixels().expect("reads");
    let (cell, (width, _)) = (painter.cell_size(), painter.pixel_size());
    assert!(ink_in_cells(&pixels, width, cell, 0..2, 0) > 0, "AB leaves ink in its cells");
    assert_eq!(ink_in_cells(&pixels, width, cell, 4..8, 0), 0, "blank cells stay background");
    assert_eq!(ink_in_cells(&pixels, width, cell, 0..8, 1), 0, "the empty row stays background");
}

#[test]
fn a_second_paint_touches_only_the_changed_row() {
    let mut painter = painter(8, 3);
    let mut mirror = GridMirror::from_rows(8, &["AA      ", "BB      ", "CC      "]);
    painter.paint(&mirror, 0).expect("first paint");
    let before = painter.read_pixels().expect("reads");
    mirror.grid[1] = GridMirror::from_rows(8, &["DD      "]).grid.remove(0);
    let dirty = painter.paint(&mirror, 0).expect("second paint");
    assert_eq!(dirty, vec![1], "only the changed row is owed");
    let after = painter.read_pixels().expect("reads");
    let (cell, (width, _)) = (painter.cell_size(), painter.pixel_size());
    let row_bytes = (width * cell.1 as u32 * 4) as usize;
    assert_eq!(before[..row_bytes], after[..row_bytes], "row 0 pixels never moved");
    assert_eq!(before[2 * row_bytes..], after[2 * row_bytes..], "row 2 pixels never moved");
    assert_ne!(before[row_bytes..2 * row_bytes], after[row_bytes..2 * row_bytes]);
}

#[test]
fn a_wide_glyph_covers_both_of_its_cells() {
    let mut painter = painter(6, 1);
    let mirror = GridMirror::from_rows(6, &["한\u{0}    "]);
    painter.paint(&mirror, 0).expect("paints");
    let pixels = painter.read_pixels().expect("reads");
    let (cell, (width, _)) = (painter.cell_size(), painter.pixel_size());
    assert!(ink_in_cells(&pixels, width, cell, 0..1, 0) > 0, "the leading cell has ink");
    assert!(ink_in_cells(&pixels, width, cell, 1..2, 0) > 0, "the spacer cell continues the glyph");
    assert_eq!(ink_in_cells(&pixels, width, cell, 3..6, 0), 0, "blank cells stay background");
}

#[test]
fn an_unchanged_screen_owes_no_rows() {
    let mut painter = painter(4, 2);
    let mirror = GridMirror::from_rows(4, &["ok  ", "    "]);
    painter.paint(&mirror, 0).expect("first paint");
    let dirty = painter.paint(&mirror, 0).expect("second paint");
    assert!(dirty.is_empty(), "nothing changed, nothing repaints: {dirty:?}");
}
