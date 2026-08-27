//! The ring binding: three distinct send rights, and every slot catches up on
//! exactly the damage it missed while the others were shown.
#![cfg(target_os = "macos")]

mod common;

use std::sync::Arc;

use common::{palette, GridMirror};
use soksak_kit_sidecar_terminal::render::native::Canvas;
use soksak_kit_sidecar_terminal::render::painter::Painter;
use soksak_kit_sidecar_terminal::render::surface_ring::SurfaceRing;

#[test]
fn the_ring_mints_three_distinct_send_rights() {
    let canvas = Canvas::create().expect("a Metal device exists on this host");
    let ring = SurfaceRing::new(&canvas, 64, 64, 2).expect("three surfaces allocate");
    let ports = ring.mach_ports().expect("ports mint");
    assert_eq!(ports.len(), 3);
    assert!(ports.iter().all(|&port| port != 0));
    assert_ne!(ports[0], ports[1]);
    assert_ne!(ports[1], ports[2]);
}

#[test]
fn every_slot_catches_up_on_its_own_missed_damage() {
    let canvas = Arc::new(Canvas::create().expect("a Metal device exists on this host"));
    let mut painter =
        Painter::new(Arc::clone(&canvas), "Menlo", 13.0, 2.0, 8, 3, palette()).expect("builds");
    let (width, height) = painter.pixel_size();
    let mut ring = SurfaceRing::new(&canvas, width, height, 3).expect("allocates");
    let mut mirror = GridMirror::from_rows(8, &["AA      ", "BB      ", "CC      "]);

    let first = ring.acquire().expect("a free slot");
    painter.refresh(&mirror, 0, None).expect("refreshes");
    let (surface, state) = ring.target(first);
    let owed = painter.paint_into(surface, state).expect("paints");
    assert_eq!(owed, vec![0, 1, 2], "a fresh slot is owed everything");
    let seq = ring.signal(first);
    assert_eq!(seq, 1);
    ring.shown(first).expect("the app moved onto it");

    mirror.grid[1] = GridMirror::from_rows(8, &["DD      "]).grid.remove(0);
    let second = ring.acquire().expect("another free slot");
    assert_ne!(second, first, "the displayed slot is never painted");
    painter.refresh(&mirror, 0, None).expect("refreshes");
    let (surface, state) = ring.target(second);
    let owed = painter.paint_into(surface, state).expect("paints");
    assert_eq!(owed, vec![0, 1, 2], "a never-painted slot is owed everything");
    ring.signal(second);
    ring.shown(second).expect("the app moved");
    ring.released(first).expect("the old slot returns");

    mirror.grid[2] = GridMirror::from_rows(8, &["EE      "]).grid.remove(0);
    let third = ring.acquire().expect("a slot");
    painter.refresh(&mirror, 0, None).expect("refreshes");
    let (surface, state) = ring.target(third);
    let owed = painter.paint_into(surface, state).expect("paints");
    ring.signal(third);
    if third == first {
        assert_eq!(owed, vec![1, 2], "the returned slot owes what it missed, not everything");
    } else {
        assert_eq!(owed, vec![0, 1, 2]);
    }
}
