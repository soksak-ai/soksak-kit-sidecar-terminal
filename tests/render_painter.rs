//! The painter's testimony: pixels land where cells say, only dirty rows are
//! touched, wide glyphs cover both their cells, clean rows stay byte-stable.
#![cfg(target_os = "macos")]

mod common;

use std::sync::Arc;

use common::{ink_in_cells, palette, GridMirror};
use soksak_kit_sidecar_terminal::mirror::TerminalRgb;
use soksak_kit_sidecar_terminal::render::instances::pack_bgra;
use soksak_kit_sidecar_terminal::render::native::{Canvas, Surface};
use soksak_kit_sidecar_terminal::render::painter::{Painter, Preedit, TargetState};

fn painter(cols: u16, rows: u16) -> (Painter, Surface, TargetState) {
    let canvas = Arc::new(Canvas::create().expect("a Metal device exists on this host"));
    let painter =
        Painter::new(Arc::clone(&canvas), "Menlo", 13.0, 2.0, cols, rows, palette())
            .expect("the painter builds");
    let (width, height) = painter.pixel_size();
    let surface = canvas.surface(width, height).expect("a target allocates");
    (painter, surface, TargetState::new(rows))
}

fn paint(
    painter: &mut Painter,
    surface: &Surface,
    state: &mut TargetState,
    mirror: &GridMirror,
) -> Vec<u16> {
    painter.refresh(mirror, 0, None, true, true).expect("refreshes");
    painter.paint_into(surface, state).expect("paints")
}

fn canvas_read(painter: &Painter, surface: &Surface) -> Vec<u8> {
    painter.canvas().surface_read(surface).expect("reads")
}

#[test]
fn a_painted_row_shows_its_glyphs_where_its_cells_are() {
    let (mut painter, surface, mut state) = painter(8, 2);
    let mirror = GridMirror::from_rows(8, &["AB      ", "        "]);
    let dirty = paint(&mut painter, &surface, &mut state, &mirror);
    assert_eq!(dirty, vec![0, 1], "the first paint owes every row");
    let pixels = canvas_read(&painter, &surface);
    let (cell, (width, _)) = (painter.cell_size(), painter.pixel_size());
    assert!(ink_in_cells(&pixels, width, cell, 0..2, 0) > 0, "AB leaves ink in its cells");
    assert_eq!(ink_in_cells(&pixels, width, cell, 4..8, 0), 0, "blank cells stay background");
    assert_eq!(ink_in_cells(&pixels, width, cell, 0..8, 1), 0, "the empty row stays background");
}

#[test]
fn a_second_paint_touches_only_the_changed_row() {
    let (mut painter, surface, mut state) = painter(8, 3);
    let mut mirror = GridMirror::from_rows(8, &["AA      ", "BB      ", "CC      "]);
    paint(&mut painter, &surface, &mut state, &mirror);
    let before = canvas_read(&painter, &surface);
    mirror.grid[1] = GridMirror::from_rows(8, &["DD      "]).grid.remove(0);
    let dirty = paint(&mut painter, &surface, &mut state, &mirror);
    assert_eq!(dirty, vec![1], "only the changed row is owed");
    let after = canvas_read(&painter, &surface);
    let (cell, (width, _)) = (painter.cell_size(), painter.pixel_size());
    let row_bytes = (width * cell.1 as u32 * 4) as usize;
    assert_eq!(before[..row_bytes], after[..row_bytes], "row 0 pixels never moved");
    assert_eq!(before[2 * row_bytes..], after[2 * row_bytes..], "row 2 pixels never moved");
    assert_ne!(before[row_bytes..2 * row_bytes], after[row_bytes..2 * row_bytes]);
}

#[test]
fn a_wide_glyph_covers_both_of_its_cells() {
    let (mut painter, surface, mut state) = painter(6, 1);
    let mirror = GridMirror::from_rows(6, &["한\u{0}    "]);
    paint(&mut painter, &surface, &mut state, &mirror);
    let pixels = canvas_read(&painter, &surface);
    let (cell, (width, _)) = (painter.cell_size(), painter.pixel_size());
    assert!(ink_in_cells(&pixels, width, cell, 0..1, 0) > 0, "the leading cell has ink");
    assert!(ink_in_cells(&pixels, width, cell, 1..2, 0) > 0, "the spacer cell continues the glyph");
    assert_eq!(ink_in_cells(&pixels, width, cell, 3..6, 0), 0, "blank cells stay background");
}

#[test]
fn an_unchanged_screen_owes_no_rows() {
    let (mut painter, surface, mut state) = painter(4, 2);
    let mirror = GridMirror::from_rows(4, &["ok  ", "    "]);
    paint(&mut painter, &surface, &mut state, &mirror);
    let dirty = paint(&mut painter, &surface, &mut state, &mirror);
    assert!(dirty.is_empty(), "nothing changed, nothing repaints: {dirty:?}");
}

#[test]
fn engine_selection_range_repaints_only_its_cells_with_selection_colors() {
    let (mut painter, surface, mut state) = painter(8, 1);
    let mut mirror = GridMirror::from_rows(8, &["        "]);
    paint(&mut painter, &surface, &mut state, &mirror);
    mirror.selected = Some((0, 2, 4));
    let dirty = paint(&mut painter, &surface, &mut state, &mirror);
    assert_eq!(dirty, vec![0]);
    let pixels = canvas_read(&painter, &surface);
    let (cell, (width, _)) = (painter.cell_size(), painter.pixel_size());
    let pixel = |col: u32| {
        let x = col * u32::from(cell.0) + u32::from(cell.0) / 2;
        let y = u32::from(cell.1) / 2;
        let at = ((y * width + x) * 4) as usize;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    };
    assert_eq!(pixel(1), [10, 10, 10], "unselected background stays base");
    assert_eq!(pixel(3), [60, 60, 60], "selected cell uses selection background");

    mirror.selected = None;
    assert_eq!(paint(&mut painter, &surface, &mut state, &mirror), vec![0]);
    let cleared = canvas_read(&painter, &surface);
    let x = 3 * u32::from(cell.0) + u32::from(cell.0) / 2;
    let y = u32::from(cell.1) / 2;
    let at = ((y * width + x) * 4) as usize;
    assert_eq!(&cleared[at..at + 3], &[10, 10, 10]);
}

#[test]
fn engine_background_override_repaints_and_reset_restores_the_base() {
    let (mut painter, surface, mut state) = painter(4, 2);
    let mut mirror = GridMirror::from_rows(4, &["    ", "    "]);
    paint(&mut painter, &surface, &mut state, &mirror);
    let base = canvas_read(&painter, &surface);

    mirror.theme_overrides.background = Some(TerminalRgb { r: 255, g: 0, b: 0 });
    let overridden = paint(&mut painter, &surface, &mut state, &mirror);
    assert_eq!(overridden, vec![0, 1], "a palette change invalidates every row");
    let red = canvas_read(&painter, &surface);
    assert_eq!(&red[..3], &[0, 0, 255], "OSC 11 background reaches BGRA pixels");

    let mut changed_base = palette();
    changed_base.bg = pack_bgra(0, 255, 0, 255);
    painter.set_base_palette(changed_base);
    let held = paint(&mut painter, &surface, &mut state, &mirror);
    assert!(held.is_empty(), "an active terminal override keeps the same effective pixels");

    mirror.theme_overrides.background = None;
    let reset = paint(&mut painter, &surface, &mut state, &mirror);
    assert_eq!(reset, vec![0, 1], "OSC 111 reset invalidates every row");
    let current_base = canvas_read(&painter, &surface);
    assert_ne!(current_base, base, "the changed base replaces the original base");
    assert_eq!(&current_base[..3], &[0, 255, 0], "reset reveals the current base theme");
}


#[test]
fn preedit_paints_underlined_at_the_cursor_and_leaves_when_cleared() {
    let (mut painter, surface, mut state) = painter(8, 1);
    let mirror = GridMirror::from_rows(8, &["        "]);
    paint(&mut painter, &surface, &mut state, &mirror);
    let preedit = Preedit { text: "한".to_string(), cursor: 1 };
    painter.refresh(&mirror, 0, Some(&preedit), true, true).expect("refreshes");
    let dirty = painter.paint_into(&surface, &mut state).expect("paints");
    assert_eq!(dirty, vec![0], "the composition owes its row");
    let pixels = canvas_read(&painter, &surface);
    let (cell, (width, _)) = (painter.cell_size(), painter.pixel_size());
    assert!(ink_in_cells(&pixels, width, cell, 0..2, 0) > 0, "the composition shows at the cursor");
    let band = (cell.1 - 2) as u32..cell.1 as u32;
    let mut underline = 0u64;
    for y in band {
        for x in 0..(2 * cell.0 as u32) {
            let offset = ((y * width + x) * 4) as usize;
            if pixels[offset..offset + 3].iter().any(|&channel| channel > 32) {
                underline += 1;
            }
        }
    }
    assert!(underline >= 2 * cell.0 as u64, "the composition is underlined: {underline}");
    painter.refresh(&mirror, 0, None, true, true).expect("refreshes");
    let cleared = painter.paint_into(&surface, &mut state).expect("paints");
    assert_eq!(cleared, vec![0], "clearing the composition owes the row again");
    let after = canvas_read(&painter, &surface);
    assert_eq!(ink_in_cells(&after, width, cell, 0..8, 0), 0, "the blank row returns");
}

// A dim takes light off what the surface paints.
//
// It was declared as transparency instead, so the document behind a dimmed surface was on screen
// through it: the picture that stands in for a parked surface brightened the pane by
// 0.5×(191−127) for two frames while it was staged under one, and taking the surface off before
// that picture was drawn darkened it for two more (measured 2026-09-04).
#[test]
fn a_dim_darkens_every_pixel_the_surface_paints() {
    let (mut painter, surface, mut state) = painter(8, 2);
    let mirror = GridMirror::from_rows(8, &["AB      ", "        "]);
    paint(&mut painter, &surface, &mut state, &mirror);
    let lit = canvas_read(&painter, &surface);

    painter.set_dim(0.5);
    let dirty = paint(&mut painter, &surface, &mut state, &mirror);
    assert_eq!(dirty, vec![0, 1], "a dim owes every row: it changes every pixel");
    let dimmed = canvas_read(&painter, &surface);

    let sum = |pixels: &[u8]| pixels.iter().map(|byte| u64::from(*byte)).sum::<u64>();
    assert!(sum(&dimmed) < sum(&lit), "the dimmed surface is darker than the lit one");

    painter.set_dim(0.0);
    paint(&mut painter, &surface, &mut state, &mirror);
    assert_eq!(canvas_read(&painter, &surface), lit, "no dim paints what it painted before");
}

#[test]
fn a_dim_that_did_not_change_owes_no_row() {
    let (mut painter, surface, mut state) = painter(8, 2);
    let mirror = GridMirror::from_rows(8, &["AB      ", "        "]);
    painter.set_dim(0.4);
    paint(&mut painter, &surface, &mut state, &mirror);
    painter.set_dim(0.4);
    assert!(paint(&mut painter, &surface, &mut state, &mirror).is_empty(), "nothing changed");
}

/// The application hands the sidecar a pixel box and the grid is what fits in it; the box is
/// rarely a whole number of cells. What is left over is the sidecar's to paint too — it owns the
/// pixels — and a strip it leaves untouched shows whatever is behind the surface. Measured
/// 2026-09-05 in a three-pane window: each card's right edge showed a strip of a different
/// width, the document behind the surface, dimmed where the surface was not.
#[test]
fn a_target_larger_than_the_grid_is_painted_to_its_edge_with_the_background() {
    let (mut painter, _grid_sized, mut state) = painter(8, 2);
    let (grid_w, grid_h) = painter.pixel_size();
    let (width, height) = (grid_w + 11, grid_h + 7);
    let surface = painter.canvas().surface(width, height).expect("a wider target allocates");
    let mirror = GridMirror::from_rows(8, &["AB      ", "        "]);
    paint(&mut painter, &surface, &mut state, &mirror);
    let pixels = canvas_read(&painter, &surface);
    let bg = pack_bgra(10, 10, 10, 255).to_le_bytes();
    let at = |x: u32, y: u32| -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        [pixels[offset], pixels[offset + 1], pixels[offset + 2], pixels[offset + 3]]
    };
    // The strip right of the last column, on a painted row and on the row below the grid.
    assert_eq!(at(grid_w + 5, 3), bg, "the strip right of the grid is background");
    assert_eq!(at(width - 1, grid_h / 2), bg, "the last pixel column is background");
    assert_eq!(at(2, grid_h + 3), bg, "the strip below the grid is background");
    assert_eq!(at(width - 1, height - 1), bg, "the far corner is background");
}
