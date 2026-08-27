//! First-contact probe (plan risk table): one glyph grid painted into an
//! IOSurface through the whole Rust → Metal → CoreText → IOSurface boundary,
//! asserted by reading pixels back. No session, no channel, no ring yet.
#![cfg(target_os = "macos")]

use soksak_kit_sidecar_terminal::render::native::Canvas;

#[test]
fn the_probe_paints_ink_into_an_iosurface() {
    let canvas = Canvas::create().expect("a Metal device exists on this host");
    let ink = canvas
        .spike(640, 384)
        .expect("the probe paints and reads back");
    assert!(ink > 0, "a painted glyph grid leaves ink; {ink} pixels have it");
}
