//! The channel conformance loop: this process plays both halves. The host
//! checks the derived name in; the sidecar half looks it up, sends hello with
//! its reply right, moves three real surface rights in a ring, pushes a
//! frameReady, and hears the application's released come back.
#![cfg(target_os = "macos")]

use soksak_contract_surface::Message;
use soksak_kit_sidecar_terminal::render::channel::{ChannelHost, SurfaceChannel};
use soksak_kit_sidecar_terminal::render::native::Canvas;
use soksak_kit_sidecar_terminal::render::surface_ring::SurfaceRing;

#[test]
fn the_channel_carries_hello_ring_frame_and_released() {
    let identifier = format!("soksak-kit-conformance-{}", std::process::id());
    let host = ChannelHost::check_in(&identifier).expect("the application half checks in");
    let channel = SurfaceChannel::open(&identifier).expect("the sidecar half looks it up");

    channel
        .send(&Message::Hello { sidecar_id: "soksak-sidecar-terminal-vt100".into() }, &[])
        .expect("hello leaves");
    let (hello, hello_ports) = host.recv(2000).expect("host answers").expect("hello arrives");
    assert_eq!(hello, Message::Hello { sidecar_id: "soksak-sidecar-terminal-vt100".into() });
    assert_eq!(hello_ports.len(), 1, "the reply right rides with hello");
    assert_ne!(hello_ports[0], 0);

    let canvas = Canvas::create().expect("a Metal device exists on this host");
    let ring = SurfaceRing::new(&canvas, 64, 64, 2).expect("three surfaces allocate");
    let ring_message = Message::Ring {
        pane: "tab-test.1".into(),
        pixel_w: 64,
        pixel_h: 64,
        scale: 2.0,
        cell_w: 16.0,
        cell_h: 32.0,
    };
    channel
        .send(&ring_message, &ring.mach_ports().expect("ports mint"))
        .expect("the ring leaves");
    let (ring_received, surface_ports) =
        host.recv(2000).expect("host answers").expect("the ring arrives");
    assert_eq!(ring_received, ring_message);
    assert_eq!(surface_ports.len(), 3, "three surface rights moved");
    assert!(surface_ports.iter().all(|&port| port != 0));

    let frame = Message::FrameReady {
        pane: "tab-test.1".into(),
        ring_index: 0,
        seq: 1,
        cursor_row: 0,
        cursor_col: 2,
        cursor_visible: true,
        damage: vec![(0, 0, 64, 32)],
    };
    channel.send(&frame, &[]).expect("the frame signal leaves");
    let (frame_received, no_ports) =
        host.recv(2000).expect("host answers").expect("the signal arrives");
    assert_eq!(frame_received, frame);
    assert!(no_ports.is_empty());

    let released = Message::Released { pane: "tab-test.1".into(), ring_index: 0 };
    host.send_to(hello_ports[0], &released).expect("released leaves");
    let heard = channel.recv(2000).expect("the sidecar answers").expect("released arrives");
    assert_eq!(heard, released);

    assert_eq!(channel.recv(50).expect("timeout is clean"), None);
}
