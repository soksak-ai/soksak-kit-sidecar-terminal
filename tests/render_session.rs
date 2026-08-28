//! The session loop end to end, in one process: open answers cells and pushes
//! hello, the ring and a first frame; pause silences it; close ends it.
#![cfg(target_os = "macos")]

mod common;

use std::sync::{Arc, Condvar, Mutex};

use common::{GridMirror, SharedGrid};
use serde_json::json;
use soksak_contract_surface::Message;
use soksak_kit_sidecar_terminal::mirror::{TerminalCursorShape, TerminalCursorStyle};
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
        let messages = (0..3)
            .map(|_| bench.host.recv(2000).expect("host answers").expect("open message arrives"))
            .collect();
        (opened.join().expect("surface.open returns"), messages)
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
            blink_interval_ms: 20,
        };
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
