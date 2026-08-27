//! Grid cells become 32-byte GPU instances. One instance per cell; the shader
//! reads its cell's instance, paints the background and mixes glyph coverage
//! over it. Pure Rust — colors resolve here, never on the GPU.

use crate::mirror::{TerminalCell, TerminalColor};

/// Exactly 32 bytes, matched by the shader's struct stride.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuCell {
    /// Atlas rect of the glyph's coverage; a zero width paints background only.
    pub glyph_x: u16,
    pub glyph_y: u16,
    pub glyph_w: u16,
    pub glyph_h: u16,
    /// Ink offset from the cell's top-left, precomputed so the shader adds.
    pub ink_left: i16,
    pub ink_top: i16,
    pub fg: u32,
    pub bg: u32,
    pub flags: u32,
    /// Hyperlink id for hover underline (OSC 8); zero is no link.
    pub link: u32,
    pub reserved: u32,
}

pub const FLAG_UNDERLINE: u32 = 1;
pub const FLAG_STRIKEOUT: u32 = 2;
pub const FLAG_WIDE: u32 = 4;

pub fn pack_bgra(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (b as u32) | ((g as u32) << 8) | ((r as u32) << 16) | ((a as u32) << 24)
}

/// The resolved theme: what Default and the 256 indexed colors mean here.
#[derive(Clone)]
pub struct Palette {
    pub fg: u32,
    pub bg: u32,
    pub ansi: [u32; 256],
}

impl Palette {
    /// Foreground resolution; bold brightens the first eight names (N < 8 → N + 8).
    pub fn resolve_fg(&self, color: TerminalColor, bold: bool) -> u32 {
        match color {
            TerminalColor::Default => self.fg,
            TerminalColor::Named(index) | TerminalColor::Indexed(index) => {
                let index = if bold && index < 8 { index + 8 } else { index };
                self.ansi[index as usize]
            }
            TerminalColor::Rgb(r, g, b) => pack_bgra(r, g, b, 255),
        }
    }

    /// Background resolution; bold never brightens a background.
    pub fn resolve_bg(&self, color: TerminalColor) -> u32 {
        match color {
            TerminalColor::Default => self.bg,
            TerminalColor::Named(index) | TerminalColor::Indexed(index) => self.ansi[index as usize],
            TerminalColor::Rgb(r, g, b) => pack_bgra(r, g, b, 255),
        }
    }
}

/// Where a glyph's coverage sits, handed in by the painter's atlas.
#[derive(Clone, Copy, Debug, Default)]
pub struct GlyphPlacement {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// Ink offset from the cell's left edge.
    pub left: i16,
    /// Ink offset down from the cell's top: ascent − bitmap top.
    pub top: i16,
}

/// One row of cells to instances. `glyph` answers coverage placement for a
/// visible character or None for whitespace and unrastered glyphs (background
/// still paints; the row redraws when the atlas catches up).
pub fn row_instances(
    cells: &[TerminalCell],
    palette: &Palette,
    glyph: &mut dyn FnMut(&TerminalCell) -> Option<GlyphPlacement>,
) -> Vec<GpuCell> {
    let mut instances = Vec::with_capacity(cells.len());
    for cell in cells {
        // Inverse swaps what the colors resolved to — swapping the names would
        // do nothing when both sides are Default.
        let mut fg = palette.resolve_fg(cell.fg, cell.bold);
        let mut bg = palette.resolve_bg(cell.bg);
        if cell.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.hidden {
            fg = bg;
        }
        let mut instance = GpuCell { fg, bg, ..GpuCell::default() };
        if cell.underline {
            instance.flags |= FLAG_UNDERLINE;
        }
        if cell.strikeout {
            instance.flags |= FLAG_STRIKEOUT;
        }
        if cell.wide {
            instance.flags |= FLAG_WIDE;
        }
        let draws = !cell.spacer && !cell.hidden && cell.ch != ' ';
        if draws {
            if let Some(placed) = glyph(cell) {
                instance.glyph_x = placed.x;
                instance.glyph_y = placed.y;
                instance.glyph_w = placed.w;
                instance.glyph_h = placed.h;
                instance.ink_left = placed.left;
                instance.ink_top = placed.top;
            }
        }
        instances.push(instance);
    }
    instances
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn palette() -> Palette {
        let mut ansi = [0u32; 256];
        for (index, slot) in ansi.iter_mut().enumerate() {
            *slot = index as u32;
        }
        Palette { fg: pack_bgra(230, 230, 230, 255), bg: pack_bgra(10, 10, 10, 255), ansi }
    }

    fn slot() -> GlyphPlacement {
        GlyphPlacement { x: 64, y: 32, w: 10, h: 20, left: 1, top: 3 }
    }

    #[test]
    fn an_instance_is_32_bytes() {
        assert_eq!(std::mem::size_of::<GpuCell>(), 32);
    }

    #[test]
    fn a_wide_cell_draws_once_and_its_spacer_paints_background_only() {
        let mut wide = plain('한');
        wide.wide = true;
        let mut spacer = plain('한');
        spacer.spacer = true;
        let out = row_instances(&[wide, spacer], &palette(), &mut |_| Some(slot()));
        assert_eq!(out[0].glyph_w, 10, "the leading cell carries the glyph");
        assert_ne!(out[0].flags & FLAG_WIDE, 0);
        assert_eq!(out[1].glyph_w, 0, "the spacer paints no second glyph");
    }

    #[test]
    fn inverse_swaps_and_hidden_matches_background() {
        let mut inverse = plain('x');
        inverse.inverse = true;
        let mut hidden = plain('x');
        hidden.hidden = true;
        let out = row_instances(&[inverse, hidden], &palette(), &mut |_| Some(slot()));
        let theme = palette();
        assert_eq!(out[0].fg, theme.bg);
        assert_eq!(out[0].bg, theme.fg);
        assert_eq!(out[1].fg, out[1].bg, "hidden ink is invisible");
        assert_eq!(out[1].glyph_w, 0, "hidden draws no glyph");
    }

    #[test]
    fn bold_brightens_only_the_first_eight_names() {
        let theme = palette();
        assert_eq!(theme.resolve_fg(TerminalColor::Named(3), true), theme.ansi[11]);
        assert_eq!(theme.resolve_fg(TerminalColor::Named(11), true), theme.ansi[11]);
        assert_eq!(theme.resolve_bg(TerminalColor::Named(3)), theme.ansi[3], "backgrounds never brighten");
    }

    #[test]
    fn whitespace_paints_background_without_a_glyph() {
        let out = row_instances(&[plain(' ')], &palette(), &mut |_| Some(slot()));
        assert_eq!(out[0].glyph_w, 0);
        assert_eq!(out[0].bg, palette().bg);
    }
}
