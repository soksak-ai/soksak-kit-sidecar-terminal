//! The boundary to the darwin canvas. Negative return codes name the failing
//! stage; the Rust side turns them into refusals — it never falls back.

#[cfg(target_os = "macos")]
mod darwin {
    use std::os::raw::c_int;

    #[repr(C)]
    pub(super) struct RawCanvas {
        _opaque: [u8; 0],
    }

    unsafe extern "C" {
        pub(super) fn soksak_canvas_create() -> *mut RawCanvas;
        pub(super) fn soksak_canvas_free(canvas: *mut RawCanvas);
        pub(super) fn soksak_canvas_spike(
            canvas: *mut RawCanvas,
            width: u32,
            height: u32,
            ink_pixels: *mut u64,
        ) -> c_int;
        pub(super) fn soksak_canvas_font_metrics(
            canvas: *mut RawCanvas,
            family: *const std::os::raw::c_char,
            pt: f64,
            scale: f64,
            out: *mut RawFontMetrics,
        ) -> c_int;
        pub(super) fn soksak_canvas_raster_glyph(
            canvas: *mut RawCanvas,
            family: *const std::os::raw::c_char,
            pt: f64,
            scale: f64,
            codepoint: u32,
            coverage: *mut u8,
            cap_w: u32,
            cap_h: u32,
            placed: *mut RawGlyphBitmap,
        ) -> c_int;
    }

    #[repr(C)]
    #[derive(Default)]
    pub(super) struct RawFontMetrics {
        pub cell_w: f64,
        pub cell_h: f64,
        pub ascent: f64,
    }

    #[repr(C)]
    #[derive(Default)]
    pub(super) struct RawGlyphBitmap {
        pub width: u32,
        pub height: u32,
        pub left: i32,
        pub top: i32,
    }
}

/// Monospace cell geometry measured by CoreText, in device pixels.
#[cfg(target_os = "macos")]
pub struct FontMetrics {
    pub cell_w: f64,
    pub cell_h: f64,
    pub ascent: f64,
}

/// One Metal device, command queue and pipeline per process; every pane on the
/// same engine shares it.
#[cfg(target_os = "macos")]
pub struct Canvas {
    raw: *mut darwin::RawCanvas,
}

#[cfg(target_os = "macos")]
// The raw pointer owns Metal objects that are documented thread-safe: the
// device, queue and pipeline take concurrent calls, the pipeline is built once
// at creation, and the font cache synchronizes itself. Pane render threads
// share one canvas.
unsafe impl Send for Canvas {}

#[cfg(target_os = "macos")]
unsafe impl Sync for Canvas {}

#[cfg(target_os = "macos")]
impl Canvas {
    pub fn create() -> Result<Self, String> {
        let raw = unsafe { darwin::soksak_canvas_create() };
        if raw.is_null() {
            return Err("METAL_UNAVAILABLE: no system Metal device".to_string());
        }
        Ok(Self { raw })
    }

    /// First-contact probe: paint a glyph grid into a fresh IOSurface and count
    /// the pixels that received ink. Proves the Rust/Metal/CoreText/IOSurface
    /// boundary end to end before any session is wired to it.
    pub fn spike(&self, width: u32, height: u32) -> Result<u64, String> {
        let mut ink: u64 = 0;
        let code = unsafe { darwin::soksak_canvas_spike(self.raw, width, height, &mut ink) };
        if code != 0 {
            return Err(format!("SPIKE_STAGE_{code}: the canvas refused the probe"));
        }
        Ok(ink)
    }
}

#[cfg(target_os = "macos")]
impl Canvas {
    /// CoreText cell geometry for a face; an unknown face refuses by name.
    pub fn font_metrics(&self, family: &str, pt: f64, scale: f64) -> Result<FontMetrics, String> {
        let family = std::ffi::CString::new(family).map_err(|_| "FONT_NAME_INVALID".to_string())?;
        let mut raw = darwin::RawFontMetrics::default();
        let code = unsafe {
            darwin::soksak_canvas_font_metrics(self.raw, family.as_ptr(), pt, scale, &mut raw)
        };
        if code != 0 {
            return Err(format!("FONT_STAGE_{code}: the face did not measure"));
        }
        Ok(FontMetrics { cell_w: raw.cell_w, cell_h: raw.cell_h, ascent: raw.ascent })
    }

    /// One codepoint's coverage bitmap at pt × scale, with baseline offsets.
    pub fn raster_glyph(
        &self,
        family: &str,
        pt: f64,
        scale: f64,
        codepoint: u32,
    ) -> Result<super::atlas::Bitmap, String> {
        let family = std::ffi::CString::new(family).map_err(|_| "FONT_NAME_INVALID".to_string())?;
        const CAP: u32 = 256;
        let mut coverage = vec![0u8; (CAP * CAP) as usize];
        let mut placed = darwin::RawGlyphBitmap::default();
        let code = unsafe {
            darwin::soksak_canvas_raster_glyph(
                self.raw,
                family.as_ptr(),
                pt,
                scale,
                codepoint,
                coverage.as_mut_ptr(),
                CAP,
                CAP,
                &mut placed,
            )
        };
        if code != 0 {
            return Err(format!("GLYPH_STAGE_{code}: the glyph did not raster"));
        }
        let (w, h) = (placed.width as u16, placed.height as u16);
        let mut packed = Vec::with_capacity(w as usize * h as usize);
        for y in 0..h as usize {
            let row = &coverage[y * CAP as usize..y * CAP as usize + w as usize];
            packed.extend_from_slice(row);
        }
        Ok(super::atlas::Bitmap {
            w,
            h,
            left: placed.left as i16,
            top: placed.top as i16,
            coverage: packed,
        })
    }
}

#[cfg(target_os = "macos")]
impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe { darwin::soksak_canvas_free(self.raw) };
    }
}

/// Off macOS the canvas refuses by name. There is no software rendering path.
#[cfg(not(target_os = "macos"))]
pub struct Canvas;

#[cfg(not(target_os = "macos"))]
impl Canvas {
    pub fn create() -> Result<Self, String> {
        Err("RENDER_UNSUPPORTED: the surface canvas exists only on macOS".to_string())
    }

    pub fn spike(&self, _width: u32, _height: u32) -> Result<u64, String> {
        Err("RENDER_UNSUPPORTED: the surface canvas exists only on macOS".to_string())
    }
}

#[cfg(target_os = "macos")]
mod darwin_paint {
    use std::os::raw::c_int;

    #[repr(C)]
    pub(super) struct RawAtlas {
        _opaque: [u8; 0],
    }

    #[repr(C)]
    pub(super) struct RawSurface {
        _opaque: [u8; 0],
    }

    unsafe extern "C" {
        pub(super) fn soksak_canvas_atlas_create(
            canvas: *mut super::darwin::RawCanvas,
            size: u32,
        ) -> *mut RawAtlas;
        pub(super) fn soksak_canvas_atlas_free(atlas: *mut RawAtlas);
        pub(super) fn soksak_canvas_atlas_upload(
            atlas: *mut RawAtlas,
            x: u32,
            y: u32,
            w: u32,
            h: u32,
            coverage: *const u8,
            stride: u32,
        ) -> c_int;
        pub(super) fn soksak_canvas_surface_create(
            canvas: *mut super::darwin::RawCanvas,
            width: u32,
            height: u32,
        ) -> *mut RawSurface;
        pub(super) fn soksak_canvas_surface_free(surface: *mut RawSurface);
        pub(super) fn soksak_canvas_paint(
            canvas: *mut super::darwin::RawCanvas,
            atlas: *mut RawAtlas,
            surface: *mut RawSurface,
            cells: *const u8,
            cols: u32,
            rows: u32,
            cell_w: u32,
            cell_h: u32,
            row_start: u32,
            row_count: u32,
        ) -> c_int;
        pub(super) fn soksak_canvas_surface_read(
            surface: *mut RawSurface,
            bgra: *mut u8,
            cap: u64,
        ) -> c_int;
        pub(super) fn soksak_canvas_surface_mach_port(surface: *mut RawSurface) -> u32;
    }
}

/// The process-wide R8 coverage page on the GPU; the atlas bookkeeping in
/// `render::atlas` decides where every glyph lands on it.
#[cfg(target_os = "macos")]
pub struct AtlasTexture {
    raw: *mut darwin_paint::RawAtlas,
}

#[cfg(target_os = "macos")]
unsafe impl Send for AtlasTexture {}

#[cfg(target_os = "macos")]
impl Drop for AtlasTexture {
    fn drop(&mut self) {
        unsafe { darwin_paint::soksak_canvas_atlas_free(self.raw) };
    }
}

/// One IOSurface and its texture view; the application composites this exact
/// object, and the ring will hold three of them.
#[cfg(target_os = "macos")]
pub struct Surface {
    raw: *mut darwin_paint::RawSurface,
    width: u32,
    height: u32,
}

#[cfg(target_os = "macos")]
unsafe impl Send for Surface {}

#[cfg(target_os = "macos")]
impl Surface {
    pub fn width(&self) -> u32 {
        self.width
    }

    /// A fresh send right for this surface; the channel ships it once and the
    /// application looks the surface up from it.
    pub fn mach_port(&self) -> Result<u32, String> {
        let port = unsafe { darwin_paint::soksak_canvas_surface_mach_port(self.raw) };
        if port == 0 {
            return Err("SURFACE_PORT_UNAVAILABLE: the surface minted no send right".to_string());
        }
        Ok(port)
    }

    pub fn height(&self) -> u32 {
        self.height
    }
}

#[cfg(target_os = "macos")]
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { darwin_paint::soksak_canvas_surface_free(self.raw) };
    }
}

#[cfg(target_os = "macos")]
impl Canvas {
    pub fn atlas_texture(&self, size: u16) -> Result<AtlasTexture, String> {
        let raw = unsafe { darwin_paint::soksak_canvas_atlas_create(self.raw, size as u32) };
        if raw.is_null() {
            return Err("ATLAS_UNAVAILABLE: the coverage page did not allocate".to_string());
        }
        Ok(AtlasTexture { raw })
    }

    pub fn atlas_upload(
        &self,
        atlas: &AtlasTexture,
        x: u16,
        y: u16,
        bitmap: &super::atlas::Bitmap,
    ) -> Result<(), String> {
        let code = unsafe {
            darwin_paint::soksak_canvas_atlas_upload(
                atlas.raw,
                x as u32,
                y as u32,
                bitmap.w as u32,
                bitmap.h as u32,
                bitmap.coverage.as_ptr(),
                bitmap.w as u32,
            )
        };
        if code != 0 {
            return Err(format!("ATLAS_STAGE_{code}: the coverage did not upload"));
        }
        Ok(())
    }

    pub fn surface(&self, width: u32, height: u32) -> Result<Surface, String> {
        let raw = unsafe { darwin_paint::soksak_canvas_surface_create(self.raw, width, height) };
        if raw.is_null() {
            return Err("SURFACE_UNAVAILABLE: the IOSurface did not allocate".to_string());
        }
        Ok(Surface { raw, width, height })
    }

    /// Paint rows [row_start, row_start + row_count) of the instance grid.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &self,
        atlas: &AtlasTexture,
        surface: &Surface,
        cells: &[super::instances::GpuCell],
        cols: u16,
        rows: u16,
        cell_w: u16,
        cell_h: u16,
        row_start: u16,
        row_count: u16,
    ) -> Result<(), String> {
        if cells.len() != cols as usize * rows as usize {
            return Err("PAINT_GRID_MISMATCH: the instance buffer is not cols x rows".to_string());
        }
        let code = unsafe {
            darwin_paint::soksak_canvas_paint(
                self.raw,
                atlas.raw,
                surface.raw,
                cells.as_ptr().cast(),
                cols as u32,
                rows as u32,
                cell_w as u32,
                cell_h as u32,
                row_start as u32,
                row_count as u32,
            )
        };
        if code != 0 {
            return Err(format!("PAINT_STAGE_{code}: the pass did not complete"));
        }
        Ok(())
    }

    /// The surface pixels, BGRA rows tightly packed. Parking and tests read
    /// the verdict from here, never from the encoder.
    pub fn surface_read(&self, surface: &Surface) -> Result<Vec<u8>, String> {
        let mut bgra = vec![0u8; surface.width as usize * surface.height as usize * 4];
        let code = unsafe {
            darwin_paint::soksak_canvas_surface_read(surface.raw, bgra.as_mut_ptr(), bgra.len() as u64)
        };
        if code != 0 {
            return Err(format!("READ_STAGE_{code}: the surface did not read back"));
        }
        Ok(bgra)
    }
}
