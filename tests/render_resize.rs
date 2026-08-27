//! A resize rebuilds the paint surfaces: a fresh painter sized to the new
//! box and a fresh ring with three new send rights.
#![cfg(target_os = "macos")]

mod common;

use std::sync::Arc;

use common::palette;
use soksak_kit_sidecar_terminal::render::native::Canvas;
use soksak_kit_sidecar_terminal::render::session::prepare_render;

#[test]
fn a_new_pixel_box_answers_a_new_grid_and_three_new_rights() {
    let canvas = Arc::new(Canvas::create().expect("a Metal device exists on this host"));
    let (small_painter, small_ring, (small_cols, small_rows)) =
        prepare_render(&canvas, "Menlo", 13.0, 2.0, 200.0, 100.0, palette()).expect("builds");
    let (grown_painter, grown_ring, (grown_cols, grown_rows)) =
        prepare_render(&canvas, "Menlo", 13.0, 2.0, 400.0, 300.0, palette()).expect("builds");

    assert!(grown_cols > small_cols, "wider box holds more columns");
    assert!(grown_rows > small_rows, "taller box holds more rows");
    let (small_w, small_h) = small_painter.pixel_size();
    let (grown_w, grown_h) = grown_painter.pixel_size();
    assert!(grown_w > small_w && grown_h > small_h, "the canvas grew with the box");

    let old_ports = small_ring.mach_ports().expect("ports mint");
    let new_ports = grown_ring.mach_ports().expect("ports mint");
    assert_eq!(new_ports.len(), 3);
    for port in &new_ports {
        assert!(!old_ports.contains(port), "a rebuilt ring never reuses an old right");
    }
}
