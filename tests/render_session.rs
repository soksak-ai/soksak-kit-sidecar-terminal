//! The session loop end to end, in one process: open answers cells and pushes
//! hello, the ring and a first frame; pause silences it; close ends it.
#![cfg(target_os = "macos")]

mod common;

use std::sync::{Arc, Condvar, Mutex};

use common::{GridMirror, SharedGrid};
use serde_json::json;
use soksak_contract_surface::Message;
use soksak_kit_sidecar_terminal::mirror::{
    TerminalCursorShape, TerminalCursorStyle, TerminalRgb,
};
use soksak_kit_sidecar_terminal::render::channel::ChannelHost;
use soksak_kit_sidecar_terminal::render::session::{FrameSignal, SharedMirror, SurfaceSessions};
use soksak_kit_sidecar_terminal::TerminalStateMirror;

struct Bench {
    host: ChannelHost,
    sessions: SurfaceSessions,
    grid: Arc<Mutex<GridMirror>>,
    mirror: SharedMirror,
    signal: FrameSignal,
    identifier: String,
}

fn bench(tag: &str) -> Bench {
    let identifier = format!("soksak-session-{}-{}", tag, std::process::id());
    let host = ChannelHost::check_in(&identifier).expect("the application half checks in");
    let grid = Arc::new(Mutex::new(GridMirror::from_rows(8, &["hi      ", "        "])));
    let mirror: SharedMirror =
        Arc::new(Mutex::new(Box::new(SharedGrid(Arc::clone(&grid))) as Box<dyn TerminalStateMirror>));
    let signal: FrameSignal = Arc::new((Mutex::new(0), Condvar::new()));
    Bench { host, sessions: SurfaceSessions::new(), grid, mirror, signal, identifier }
}

fn theme() -> serde_json::Value {
    json!({
        "mode": "dark",
        "fg": "#e6e6e6", "bg": "#0a0a0a", "cursor": "#ffffff", "cursorAccent": "#000000",
        "selectionBg": "#334455", "selectionFg": "#ffffff",
        "ansi": vec!["#0a0a0a"; 256],
    })
}

fn open_and_receive(bench: &Bench) -> (serde_json::Value, Vec<(Message, Vec<u32>)>) {
    let sessions = &bench.sessions;
    let identifier = bench.identifier.clone();
    let mirror = bench.mirror.clone();
    let signal = bench.signal.clone();
    std::thread::scope(|scope| {
        let opened = scope.spawn(move || {
            let request = json!({
                "identifier": identifier,
                "pane": "tab-test.1",
                "pixelW": 64.0, "pixelH": 48.0, "scale": 2.0,
                "font": {"family": "Menlo", "pt": 13.0},
                "theme": theme(),
            });
            sessions
                .command(
                    "soksak-sidecar-terminal-test",
                    "surface.open",
                    &request,
                    Some((mirror, signal)),
                )
                .unwrap_or_else(|error| panic!("{}: {}", error.code, error.message))
        });
        let mut messages: Vec<_> = (0..2)
            .map(|_| bench.host.recv(2000).expect("host answers").expect("open message arrives"))
            .collect();
        let reply = opened.join().expect("surface.open returns");
        assert!(
            bench.host.recv(100).expect("host answers").is_none(),
            "the initial mismatched mirror cannot emit a speculative frame"
        );
        let cols = reply["cols"].as_u64().unwrap() as u16;
        let rows = reply["rows"].as_u64().unwrap() as u16;
        let first = format!("hi{}", " ".repeat(cols.saturating_sub(2) as usize));
        let blank = " ".repeat(cols as usize);
        let mut owned = vec![blank; rows as usize];
        if let Some(row) = owned.first_mut() {
            *row = first;
        }
        let refs: Vec<_> = owned.iter().map(String::as_str).collect();
        let mut grid = bench.grid.lock().unwrap();
        let mut matching = GridMirror::from_rows(cols, &refs);
        matching.cursor = grid.cursor;
        matching.cursor_visible = grid.cursor_visible;
        matching.cursor_style = grid.cursor_style;
        matching.cursor_animation = grid.cursor_animation;
        matching.theme_overrides = grid.theme_overrides.clone();
        *grid = matching;
        drop(grid);
        progressed(bench);
        messages.push(
            bench.host.recv(2000).expect("host answers").expect("first matching frame arrives"),
        );
        (reply, messages)
    })
}

fn progressed(bench: &Bench) {
    let (seq, ready) = &*bench.signal;
    *seq.lock().unwrap() += 1;
    ready.notify_all();
}

#[test]
fn open_answers_cells_and_pushes_hello_ring_and_a_first_frame() {
    let bench = bench("open");
    let (reply, messages) = open_and_receive(&bench);
    assert!(reply["cols"].as_u64().unwrap() >= 1);
    assert!(reply["rows"].as_u64().unwrap() >= 1);
    let (hello, hello_ports) = &messages[0];
    assert!(matches!(hello, Message::Hello { .. }));
    assert_eq!(hello_ports.len(), 1);
    let (ring, surface_ports) = &messages[1];
    assert!(matches!(ring, Message::Ring { .. }));
    assert_eq!(surface_ports.len(), 3);
    let (frame, _) = &messages[2];
    match frame {
        Message::FrameReady { seq, ring_index, damage, .. } => {
            assert_eq!(*seq, 1, "the first signal takes sequence one");
            assert_eq!(*ring_index, 0);
            assert!(!damage.is_empty(), "the first frame owes everything");
        }
        other => panic!("expected frameReady, got {other:?}"),
    }
}

#[test]
fn first_paint_waits_for_the_terminal_grid_to_match_the_surface_grid() {
    let bench = bench("grid-handshake");
    bench.grid.lock().unwrap().strict_bounds = true;
    let request = json!({
        "identifier": bench.identifier,
        "pane": "tab-test.1",
        "pixelW": 640.0, "pixelH": 480.0, "scale": 2.0,
        "font": {"family": "Menlo", "pt": 13.0},
        "theme": theme(),
    });
    let reply = bench
        .sessions
        .command(
            "soksak-sidecar-terminal-test",
            "surface.open",
            &request,
            Some((bench.mirror.clone(), bench.signal.clone())),
        )
        .expect("surface.open returns its required terminal grid");
    let cols = reply["cols"].as_u64().unwrap() as u16;
    let rows = reply["rows"].as_u64().unwrap() as u16;
    assert_ne!((cols, rows), (8, 2), "fixture must begin with a mismatched grid");
    assert!(bench.host.recv(2000).unwrap().is_some(), "hello arrives");
    assert!(bench.host.recv(2000).unwrap().is_some(), "ring arrives");
    assert!(
        bench.host.recv(100).unwrap().is_none(),
        "no frame may read a mirror whose grid has not reached the surface grid"
    );

    let blank = " ".repeat(cols as usize);
    let row_refs = vec![blank.as_str(); rows as usize];
    let mut matching = GridMirror::from_rows(cols, &row_refs);
    matching.strict_bounds = true;
    *bench.grid.lock().unwrap() = matching;
    progressed(&bench);

    let (message, _) = bench
        .host
        .recv(2000)
        .expect("host answers")
        .expect("the matching grid event produces the first frame");
    assert!(matches!(message, Message::FrameReady { .. }));
}

#[test]
fn paused_produces_no_frame_and_resume_catches_up() {
    let bench = bench("pause");
    open_and_receive(&bench);
    bench
        .sessions
        .command(
            "soksak-sidecar-terminal-test",
            "surface.setPaused",
            &json!({"pane": "tab-test.1", "paused": true}),
            None,
        )
        .expect("pause lands");
    bench.grid.lock().unwrap().grid[1] =
        GridMirror::from_rows(8, &["there   "]).grid.remove(0);
    progressed(&bench);
    assert!(
        bench.host.recv(300).expect("host answers").is_none(),
        "a paused pane pushes no frame"
    );
    bench
        .sessions
        .command(
            "soksak-sidecar-terminal-test",
            "surface.setPaused",
            &json!({"pane": "tab-test.1", "paused": false}),
            None,
        )
        .expect("resume lands");
    let (frame, _) = bench.host.recv(2000).expect("host answers").expect("the catch-up frame");
    assert!(matches!(frame, Message::FrameReady { .. }));
}

#[test]
fn close_ends_the_ring() {
    let bench = bench("close");
    open_and_receive(&bench);
    bench
        .sessions
        .command(
            "soksak-sidecar-terminal-test",
            "surface.close",
            &json!({"pane": "tab-test.1"}),
            None,
        )
        .expect("close lands");
    let (ended, _) = bench.host.recv(2000).expect("host answers").expect("ended arrives");
    assert!(matches!(ended, Message::Ended { .. }));
}

#[test]
fn read_returns_the_viewport_text() {
    let bench = bench("read");
    open_and_receive(&bench);
    let reply = bench
        .sessions
        .command(
            "soksak-sidecar-terminal-test",
            "surface.read",
            &json!({"pane": "tab-test.1"}),
            Some((bench.mirror.clone(), bench.signal.clone())),
        )
        .expect("read answers");
    assert!(reply["text"].as_str().unwrap().starts_with("hi"));
}

#[test]
fn engine_blink_state_schedules_a_cursor_frame_without_output_polling() {
    let bench = bench("blink");
    {
        let mut grid = bench.grid.lock().unwrap();
        grid.cursor_visible = true;
        grid.cursor_style = TerminalCursorStyle {
            shape: TerminalCursorShape::Bar,
            blinking: true,
        };
        grid.cursor_animation.interval_ms = 20;
    }
    open_and_receive(&bench);
    let (frame, _) = bench
        .host
        .recv(500)
        .expect("host answers")
        .expect("the renderer-owned blink clock emits a frame");
    assert!(matches!(frame, Message::FrameReady { .. }));
    bench
        .sessions
        .command(
            "soksak-sidecar-terminal-test",
            "surface.close",
            &json!({"pane": "tab-test.1"}),
            None,
        )
        .expect("close lands");
}

#[test]
fn applied_override_frame_updates_surface_theme_state() {
    let bench = bench("theme");
    open_and_receive(&bench);
    bench.grid.lock().unwrap().theme_overrides.foreground =
        Some(TerminalRgb { r: 0xab, g: 0xcd, b: 0xef });
    progressed(&bench);
    let (frame, _) = bench.host.recv(2000).expect("host answers").expect("theme frame arrives");
    assert!(matches!(frame, Message::FrameReady { .. }));
    let state = bench.sessions.command(
        "soksak-sidecar-terminal-test",
        "surface.state",
        &json!({"pane": "tab-test.1"}),
        None,
    ).expect("state answers");
    assert_eq!(state["terminalOverrides"]["foreground"], "#abcdef");
    assert_eq!(state["effectiveTheme"]["foreground"], "#abcdef");
}

// A dim is what the surface paints, so it is a command on the surface, not a transparency on the
// layer. Measured 2026-09-04: declared as transparency, the document behind a dimmed surface was on
// screen through it and the picture that stands in for a parked surface flashed the pane.
#[test]
fn a_dim_is_accepted_and_a_bad_amount_is_refused() {
    let bench = bench("dim");
    open_and_receive(&bench);
    bench.sessions.command(
        "soksak-sidecar-terminal-test",
        "surface.dim",
        &json!({"window": "win-test", "pane": "tab-test.1", "dim": 0.5}),
        None,
    ).expect("a dim is accepted");
    let state = bench.sessions.command(
        "soksak-sidecar-terminal-test",
        "surface.state",
        &json!({"pane": "tab-test.1"}),
        None,
    ).expect("state answers");
    assert_eq!(state["dim"], 0.5, "the state reports the amount the pane paints with");

    for bad in [json!({"window": "win-test", "pane": "tab-test.1", "dim": 1.5}),
                json!({"window": "win-test", "pane": "tab-test.1"})] {
        assert!(
            bench.sessions.command("soksak-sidecar-terminal-test", "surface.dim", &bad, None).is_err(),
            "an amount outside 0..1, or none at all, is refused",
        );
    }
}
