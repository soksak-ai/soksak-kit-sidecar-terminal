//! The painter: one mirror's viewport becomes pixels on an IOSurface. Rust
//! hashes rows, keeps the instance grid and decides damage; the canvas paints
//! exactly the dirty band. Rows that did not change are never touched — the
//! damage list is the painter's testimony and its test surface.

use std::sync::Arc;

use super::atlas::{Atlas, GlyphKey, ATLAS_PAGE_SIZE};
use super::instances::{apply_cursor, row_instances, GlyphPlacement, GpuCell, Palette};
use super::native::{AtlasTexture, Canvas, Surface};

/// What one target surface currently shows, row by row. Three ring surfaces
/// carry three of these; each catches up on its own damage.
pub struct TargetState {
    painted: Vec<Option<u64>>,
}

impl TargetState {
    pub fn new(rows: u16) -> Self {
        Self { painted: vec![None; rows as usize] }
    }
}
use crate::mirror::{TerminalCell, TerminalColor};
use crate::mirror::TerminalThemeOverrides;
use crate::TerminalStateMirror;

/// An IME composition shown at the cursor. It paints as an overlay and its
/// bytes never reach the pty — only confirmed text is ever written (P11).
pub struct Preedit {
    pub text: String,
    /// Caret position in characters within the composition.
    pub cursor: usize,
}

/// Composition width, engine-agnostic: CJK and Hangul compositions are wide.
/// The composition never carries box drawing or latin typography, so the
/// coarse boundary is exact for what actually arrives here.
fn char_cells(ch: char) -> u16 {
    if (ch as u32) >= 0x1100 { 2 } else { 1 }
}

fn preedit_cell(ch: char, wide: bool) -> TerminalCell {
    TerminalCell {
        ch,
        fg: TerminalColor::Default,
        bg: TerminalColor::Default,
        bold: false,
        dim: false,
        italic: false,
        underline: true,
        inverse: false,
        strikeout: false,
        hidden: false,
        wide,
        spacer: false,
        wrapline: false,
        zerowidth: Vec::new(),
        link: None,
    }
}

fn spacer_cell() -> TerminalCell {
    TerminalCell { spacer: true, ..preedit_cell(' ', false) }
}

fn blank_cell() -> TerminalCell {
    TerminalCell { underline: false, ..preedit_cell(' ', false) }
}

pub struct Painter {
    canvas: Arc<Canvas>,
    atlas: Atlas,
    atlas_texture: AtlasTexture,
    base_palette: Palette,
    palette: Palette,
    palette_revision: u64,
    family: String,
    pt: f64,
    scale: f64,
    ascent: f64,
    cell_w: u16,
    cell_h: u16,
    cols: u16,
    rows: u16,
    cells: Vec<GpuCell>,
    hashes: Vec<Option<u64>>,
}

impl Painter {
    pub fn new(
        canvas: Arc<Canvas>,
        family: &str,
        pt: f64,
        scale: f64,
        cols: u16,
        rows: u16,
        palette: Palette,
    ) -> Result<Self, String> {
        if cols == 0 || rows == 0 {
            return Err("PAINTER_EMPTY: a zero-cell grid paints nothing".to_string());
        }
        let metrics = canvas.font_metrics(family, pt, scale)?;
        let cell_w = metrics.cell_w.ceil() as u16;
        let cell_h = metrics.cell_h.ceil() as u16;
        let atlas_texture = canvas.atlas_texture(ATLAS_PAGE_SIZE)?;
        Ok(Self {
            canvas,
            atlas: Atlas::default(),
            atlas_texture,
            base_palette: palette.clone(),
            palette,
            palette_revision: 0,
            family: family.to_string(),
            pt,
            scale,
            ascent: metrics.ascent,
            cell_w,
            cell_h,
            cols,
            rows,
            cells: vec![GpuCell::default(); cols as usize * rows as usize],
            hashes: vec![None; rows as usize],
        })
    }

    pub fn cell_size(&self) -> (u16, u16) {
        (self.cell_w, self.cell_h)
    }

    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    pub fn pixel_size(&self) -> (u32, u32) {
        (
            self.cols as u32 * self.cell_w as u32,
            self.rows as u32 * self.cell_h as u32,
        )
    }

    /// Fold the mirror's viewport into the instance grid. `offset` scrolls
    /// into history; the cursor paints only at the bottom (offset 0) and only
    /// while the mirror shows it.
    pub fn refresh(
        &mut self,
        mirror: &dyn TerminalStateMirror,
        offset: usize,
        preedit: Option<&Preedit>,
        cursor_on: bool,
    ) -> Result<TerminalThemeOverrides, String> {
        let overrides = mirror.theme_overrides();
        let effective_palette = self.base_palette.resolve(&overrides);
        if self.palette != effective_palette {
            self.palette = effective_palette;
            self.palette_revision = self.palette_revision.wrapping_add(1);
            self.invalidate();
        }
        let show_cursor = mirror.modes().show_cursor && offset == 0 && cursor_on;
        let cursor = mirror.cursor();
        let cursor_style = mirror.cursor_style();
        for row in 0..self.rows {
            let line = row as i32 - offset as i32;
            let cells = mirror.line_cells(line);
            let mut hash = row_hash(&cells);
            hash ^= self.palette_revision.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let preedit_here =
                preedit.is_some() && offset == 0 && cursor.0 == row as usize;
            let cursor_here = show_cursor && cursor.0 == row as usize && !preedit_here;
            if cursor_here {
                hash ^= 0x9E37_79B9_7F4A_7C15u64.wrapping_add(cursor.1 as u64);
                hash ^= match (cursor_style.shape, cursor_style.blinking) {
                    (crate::mirror::TerminalCursorShape::Block, false) => 0x10,
                    (crate::mirror::TerminalCursorShape::Block, true) => 0x11,
                    (crate::mirror::TerminalCursorShape::Underline, false) => 0x20,
                    (crate::mirror::TerminalCursorShape::Underline, true) => 0x21,
                    (crate::mirror::TerminalCursorShape::Bar, false) => 0x30,
                    (crate::mirror::TerminalCursorShape::Bar, true) => 0x31,
                };
            }
            if preedit_here {
                let composition = preedit.unwrap();
                hash = hash.wrapping_mul(31).wrapping_add(composition.cursor as u64);
                for byte in composition.text.as_bytes() {
                    hash = hash.wrapping_mul(31).wrapping_add(*byte as u64 + 1);
                }
                hash = hash.wrapping_mul(31).wrapping_add(cursor.1 as u64 + 1);
            }
            if self.hashes[row as usize] == Some(hash) {
                continue;
            }
            let cursor_cell = if cursor_here { Some((cursor.1, cursor_style)) } else { None };
            let overlay = if preedit_here {
                Some((preedit.unwrap(), cursor.1))
            } else {
                None
            };
            self.build_row(row, &cells, cursor_cell, overlay)?;
            self.hashes[row as usize] = Some(hash);
        }
        Ok(overrides)
    }

    /// Bring one target surface up to the current grid and return the rows it
    /// was owed. A fresh target is owed everything; a current one, nothing.
    pub fn paint_into(
        &mut self,
        target: &Surface,
        state: &mut TargetState,
    ) -> Result<Vec<u16>, String> {
        if state.painted.len() != self.rows as usize {
            return Err("TARGET_ROWS_MISMATCH: the target state is not this grid".to_string());
        }
        let mut dirty: Vec<u16> = Vec::new();
        for row in 0..self.rows {
            if state.painted[row as usize] != self.hashes[row as usize] {
                dirty.push(row);
            }
        }
        let mut index = 0;
        while index < dirty.len() {
            let start = dirty[index];
            let mut end = start;
            while index + 1 < dirty.len() && dirty[index + 1] == end + 1 {
                index += 1;
                end = dirty[index];
            }
            self.canvas.paint(
                &self.atlas_texture,
                target,
                &self.cells,
                self.cols,
                self.rows,
                self.cell_w,
                self.cell_h,
                start,
                end - start + 1,
            )?;
            index += 1;
        }
        for row in &dirty {
            state.painted[*row as usize] = self.hashes[*row as usize];
        }
        Ok(dirty)
    }

    /// Everything repaints on the next call — a theme change or a resize
    /// invalidates every row at once.
    pub fn invalidate(&mut self) {
        for hash in &mut self.hashes {
            *hash = None;
        }
    }

    fn build_row(
        &mut self,
        row: u16,
        cells: &[TerminalCell],
        cursor: Option<(usize, crate::mirror::TerminalCursorStyle)>,
        preedit: Option<(&Preedit, usize)>,
    ) -> Result<(), String> {
        let cols = self.cols as usize;
        let mut effective: Vec<TerminalCell> = cells.to_vec();
        effective.truncate(cols);
        effective.resize(cols, blank_cell());
        let mut preedit_cursor: Option<usize> = None;
        if let Some((composition, start)) = preedit {
            let mut col = start;
            for (index, ch) in composition.text.chars().enumerate() {
                if col >= cols {
                    break;
                }
                if index == composition.cursor {
                    preedit_cursor = Some(col);
                }
                let wide = char_cells(ch) == 2;
                effective[col] = preedit_cell(ch, wide);
                col += 1;
                if wide && col < cols {
                    effective[col] = spacer_cell();
                    col += 1;
                }
            }
            if preedit_cursor.is_none() {
                preedit_cursor = Some(col.min(cols - 1));
            }
        }
        let cells = effective.as_slice();
        let ascent = self.ascent;
        let (pt, scale, cell_w) = (self.pt, self.scale, self.cell_w);
        let family = &self.family;
        let canvas = &self.canvas;
        let atlas_texture = &self.atlas_texture;
        let atlas = &mut self.atlas;
        let mut glyph = |cell: &TerminalCell| -> Option<GlyphPlacement> {
            let key = GlyphKey::quantize(family, pt, scale, cell.ch as u32);
            let slot = atlas
                .ensure(
                    &key,
                    &mut |wanted: &GlyphKey| {
                        canvas.raster_glyph(family, pt, scale, wanted.codepoint)
                    },
                    &mut |x, y, bitmap| canvas.atlas_upload(atlas_texture, x, y, bitmap),
                )
                .ok()?;
            Some(GlyphPlacement {
                x: slot.x,
                y: slot.y,
                w: slot.w,
                h: slot.h,
                left: slot.left,
                top: (ascent - slot.top as f64).round() as i16,
            })
        };
        let mut instances = row_instances(cells, cell_w, &self.palette, &mut glyph);
        instances.truncate(self.cols as usize);
        instances.resize(self.cols as usize, background_only(&self.palette));
        if let Some((col, style)) = cursor.or_else(|| {
            preedit_cursor.map(|col| (col, crate::mirror::TerminalCursorStyle {
                shape: crate::mirror::TerminalCursorShape::Bar,
                blinking: false,
            }))
        }) {
            if col < instances.len() {
                apply_cursor(&mut instances[col], style, &self.palette);
            }
        }
        let base = row as usize * self.cols as usize;
        self.cells[base..base + self.cols as usize].copy_from_slice(&instances);
        Ok(())
    }
}

fn background_only(palette: &Palette) -> GpuCell {
    GpuCell { fg: palette.fg, bg: palette.bg, ..GpuCell::default() }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv(hash: &mut u64, byte: u8) {
    *hash ^= byte as u64;
    *hash = hash.wrapping_mul(FNV_PRIME);
}

fn fnv32(hash: &mut u64, value: u32) {
    for byte in value.to_le_bytes() {
        fnv(hash, byte);
    }
}

fn fnv_color(hash: &mut u64, color: TerminalColor) {
    match color {
        TerminalColor::Default => fnv(hash, 0),
        TerminalColor::Named(index) => {
            fnv(hash, 1);
            fnv(hash, index);
        }
        TerminalColor::Indexed(index) => {
            fnv(hash, 2);
            fnv(hash, index);
        }
        TerminalColor::Rgb(r, g, b) => {
            fnv(hash, 3);
            fnv(hash, r);
            fnv(hash, g);
            fnv(hash, b);
        }
    }
}

/// What the row paints as, folded to one number. Two rows with the same hash
/// paint identically, so an equal hash skips the row.
fn row_hash(cells: &[TerminalCell]) -> u64 {
    let mut hash = FNV_OFFSET;
    for cell in cells {
        fnv32(&mut hash, cell.ch as u32);
        fnv_color(&mut hash, cell.fg);
        fnv_color(&mut hash, cell.bg);
        let bits = (cell.bold as u16)
            | (cell.dim as u16) << 1
            | (cell.italic as u16) << 2
            | (cell.underline as u16) << 3
            | (cell.inverse as u16) << 4
            | (cell.strikeout as u16) << 5
            | (cell.hidden as u16) << 6
            | (cell.wide as u16) << 7
            | (cell.spacer as u16) << 8;
        fnv(&mut hash, bits as u8);
        fnv(&mut hash, (bits >> 8) as u8);
        for zero in &cell.zerowidth {
            fnv32(&mut hash, *zero as u32);
        }
        if let Some(link) = &cell.link {
            fnv(&mut hash, 1);
            for byte in link.as_bytes() {
                fnv(&mut hash, *byte);
            }
        } else {
            fnv(&mut hash, 0);
        }
    }
    hash
}
