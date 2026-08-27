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
// The raw pointer owns Metal objects that are documented thread-safe; the
// canvas is used behind the runtime's own locks.
unsafe impl Send for Canvas {}

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
