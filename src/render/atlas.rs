//! Glyph atlas bookkeeping. Rust decides where a glyph lives on a page and
//! remembers it; the darwin unit only measures fonts and rasters coverage.
//! One atlas per engine process — every pane shares the same pages.

use std::collections::HashMap;

/// A glyph identity: the face, the device pixel size and the codepoint.
/// `px` carries pt × scale quantized to quarter pixels, so the same glyph on
/// the same screen never rasters twice and a DPR change rasters exactly once.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct GlyphKey {
    pub family: String,
    pub px: u32,
    pub codepoint: u32,
}

impl GlyphKey {
    pub fn quantize(family: &str, pt: f64, scale: f64, codepoint: u32) -> Self {
        Self {
            family: family.to_string(),
            px: (pt * scale * 4.0).round() as u32,
            codepoint,
        }
    }
}

/// Where a rastered glyph sits on the page, and how it hangs off the baseline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// Ink offset from the cell's left edge; can be negative for overhang.
    pub left: i16,
    /// Ink offset up from the baseline to the bitmap's top row.
    pub top: i16,
}

/// A rastered coverage bitmap handed back by the canvas.
pub struct Bitmap {
    pub w: u16,
    pub h: u16,
    pub left: i16,
    pub top: i16,
    pub coverage: Vec<u8>,
}

/// Shelf placement on one square page: rows of glyphs, each row as tall as its
/// tallest occupant. Deterministic and append-only — eviction is a whole-page
/// reset when a font generation retires, never a per-glyph hole.
pub struct AtlasPage {
    size: u16,
    shelf_y: u16,
    shelf_h: u16,
    cursor_x: u16,
}

impl AtlasPage {
    pub fn new(size: u16) -> Self {
        Self { size, shelf_y: 0, shelf_h: 0, cursor_x: 0 }
    }

    /// The next free spot for a w×h bitmap, or None when the page is full.
    pub fn place(&mut self, w: u16, h: u16) -> Option<(u16, u16)> {
        if w > self.size || h > self.size {
            return None;
        }
        if self.cursor_x + w > self.size {
            let next_y = self.shelf_y + self.shelf_h;
            if next_y + h > self.size {
                return None;
            }
            self.shelf_y = next_y;
            self.shelf_h = 0;
            self.cursor_x = 0;
        }
        if self.shelf_y + h > self.size {
            return None;
        }
        let spot = (self.cursor_x, self.shelf_y);
        self.cursor_x += w;
        if h > self.shelf_h {
            self.shelf_h = h;
        }
        Some(spot)
    }
}

/// The atlas: one coverage page plus the slot registry. The raster callback is
/// injected so placement logic answers without a GPU under it.
pub struct Atlas {
    page: AtlasPage,
    slots: HashMap<GlyphKey, Slot>,
}

pub const ATLAS_PAGE_SIZE: u16 = 1024;

impl Default for Atlas {
    fn default() -> Self {
        Self::new(ATLAS_PAGE_SIZE)
    }
}

impl Atlas {
    pub fn new(page_size: u16) -> Self {
        Self { page: AtlasPage::new(page_size), slots: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The slot for a glyph, rastering and uploading at most once per key.
    /// `raster` produces coverage; `upload` moves it onto the page.
    pub fn ensure(
        &mut self,
        key: &GlyphKey,
        raster: &mut dyn FnMut(&GlyphKey) -> Result<Bitmap, String>,
        upload: &mut dyn FnMut(u16, u16, &Bitmap) -> Result<(), String>,
    ) -> Result<Slot, String> {
        if let Some(slot) = self.slots.get(key) {
            return Ok(*slot);
        }
        let bitmap = raster(key)?;
        let (x, y) = self
            .page
            .place(bitmap.w, bitmap.h)
            .ok_or_else(|| "ATLAS_FULL: the coverage page has no room left".to_string())?;
        upload(x, y, &bitmap)?;
        let slot = Slot { x, y, w: bitmap.w, h: bitmap.h, left: bitmap.left, top: bitmap.top };
        self.slots.insert(key.clone(), slot);
        Ok(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(w: u16, h: u16) -> Bitmap {
        Bitmap { w, h, left: 0, top: h as i16, coverage: vec![255; w as usize * h as usize] }
    }

    #[test]
    fn shelves_advance_and_a_full_page_refuses() {
        let mut page = AtlasPage::new(32);
        assert_eq!(page.place(16, 16), Some((0, 0)));
        assert_eq!(page.place(16, 16), Some((16, 0)));
        assert_eq!(page.place(16, 16), Some((0, 16)), "a full shelf opens the next one");
        assert_eq!(page.place(16, 16), Some((16, 16)));
        assert_eq!(page.place(16, 16), None, "a full page places nothing");
        assert_eq!(page.place(64, 8), None, "wider than the page is refused outright");
    }

    #[test]
    fn a_key_rasters_once_and_returns_the_same_slot() {
        let mut atlas = Atlas::new(64);
        let key = GlyphKey::quantize("Menlo", 13.0, 2.0, 'A' as u32);
        let mut rasters = 0;
        let mut raster = |_: &GlyphKey| {
            rasters += 1;
            Ok(white(10, 20))
        };
        let mut upload = |_x: u16, _y: u16, _b: &Bitmap| Ok(());
        let first = atlas.ensure(&key, &mut raster, &mut upload).expect("places");
        let second = atlas.ensure(&key, &mut raster, &mut upload).expect("cached");
        assert_eq!(first, second);
        assert_eq!(rasters, 1, "the same key never rasters twice");
    }

    #[test]
    fn dpr_change_is_a_new_key() {
        let one = GlyphKey::quantize("Menlo", 13.0, 1.0, 'A' as u32);
        let two = GlyphKey::quantize("Menlo", 13.0, 2.0, 'A' as u32);
        assert_ne!(one, two, "a DPR change rasters again at the new density");
    }
}
