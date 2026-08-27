//! CoreText behind the boundary: metrics and coverage answered in numbers.
#![cfg(target_os = "macos")]

use soksak_kit_sidecar_terminal::render::native::Canvas;

#[test]
fn menlo_measures_a_monospace_cell() {
    let canvas = Canvas::create().expect("a Metal device exists on this host");
    let metrics = canvas
        .font_metrics("Menlo", 13.0, 2.0)
        .expect("a shipped face measures");
    assert!(metrics.cell_w > 0.0 && metrics.cell_h > 0.0);
    assert!(metrics.ascent > 0.0 && metrics.ascent < metrics.cell_h);
}

#[test]
fn a_glyph_rasters_with_ink() {
    let canvas = Canvas::create().expect("a Metal device exists on this host");
    let bitmap = canvas
        .raster_glyph("Menlo", 13.0, 2.0, 'A' as u32)
        .expect("a plain latin glyph rasters");
    assert!(bitmap.w > 0 && bitmap.h > 0);
    assert!(
        bitmap.coverage.iter().any(|&byte| byte > 128),
        "coverage carries ink"
    );
}

#[test]
fn a_missing_face_refuses_by_name() {
    let canvas = Canvas::create().expect("a Metal device exists on this host");
    let refusal = canvas.font_metrics("NoSuchFace-SoksakProbe", 13.0, 2.0);
    assert!(refusal.is_err(), "an unknown face is refused, not substituted");
}

#[test]
fn a_glyph_the_face_lacks_rasters_through_the_fallback() {
    let canvas = Canvas::create().expect("a Metal device exists on this host");
    let bitmap = canvas
        .raster_glyph("Menlo", 13.0, 2.0, '한' as u32)
        .expect("hangul rasters through the system fallback");
    assert!(bitmap.w > 0 && bitmap.coverage.iter().any(|&byte| byte > 128));
}
