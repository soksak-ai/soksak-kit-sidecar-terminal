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
    }
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
