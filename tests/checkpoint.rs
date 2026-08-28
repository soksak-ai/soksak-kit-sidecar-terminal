use std::fs;
use std::sync::{Arc, Barrier};

use soksak_kit_sidecar_terminal::checkpoint::CheckpointStore;
use soksak_kit_sidecar_terminal::mirror::{
    TerminalCursorAnimation, TerminalCursorShape, TerminalCursorStyle, TerminalFrame,
    TerminalModes,
};

fn key() -> [u8; 32] {
    [0x42; 32]
}

fn frame() -> TerminalFrame {
    TerminalFrame {
        cols: 80,
        rows: 24,
        cursor: (0, 0),
        cursor_visible: true,
        cursor_style: TerminalCursorStyle { shape: TerminalCursorShape::Block, blinking: false },
        cursor_animation: TerminalCursorAnimation { interval_ms: 750 },
        alt_active: false,
        history_size: 0,
        offset: 0,
        modes: TerminalModes::default(),
        lines: vec![],
    }
}

#[test]
fn checkpoint_round_trip_never_writes_plaintext() {
    let home = std::env::temp_dir().join(format!("soksak-checkpoint-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let store = CheckpointStore::new(&home, "soksak-sidecar-terminal-test", key()).unwrap();
    store.claim_generation("window-a", "pane-a", 7).unwrap();
    store
        .write(
            "window-a",
            "pane-a",
            7,
            41,
            b"VISIBLE-SECRET-SCREEN",
            &frame(),
        )
        .unwrap();
    let path = store.path("window-a", "pane-a").unwrap();
    let disk = fs::read(&path).unwrap();
    assert!(
        !disk
            .windows(21)
            .any(|bytes| bytes == b"VISIBLE-SECRET-SCREEN")
    );
    let opened = store.read("window-a", "pane-a").unwrap().unwrap();
    assert_eq!(opened.generation, 7);
    assert_eq!(opened.sequence, 41);
    assert_eq!(opened.paint, b"VISIBLE-SECRET-SCREEN");
    assert_eq!(opened.frame, frame());
    let _ = fs::remove_dir_all(home);
}

#[test]
fn corrupt_checkpoint_is_isolated_and_rejected() {
    let home =
        std::env::temp_dir().join(format!("soksak-checkpoint-corrupt-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let store = CheckpointStore::new(&home, "soksak-sidecar-terminal-test", key()).unwrap();
    store.claim_generation("window-a", "pane-a", 1).unwrap();
    store.claim_generation("window-a", "pane-b", 1).unwrap();
    store
        .write("window-a", "pane-a", 1, 2, b"screen-a", &frame())
        .unwrap();
    store
        .write("window-a", "pane-b", 1, 3, b"screen-b", &frame())
        .unwrap();
    let path = store.path("window-a", "pane-a").unwrap();
    let mut disk = fs::read(&path).unwrap();
    let last = disk.len() - 1;
    disk[last] ^= 1;
    fs::write(path, disk).unwrap();
    assert!(store.read("window-a", "pane-a").is_err());
    assert_eq!(
        store.read("window-a", "pane-b").unwrap().unwrap().paint,
        b"screen-b"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn path_coordinates_cannot_escape_the_provider_directory() {
    let home = std::env::temp_dir();
    let store = CheckpointStore::new(&home, "soksak-sidecar-terminal-test", key()).unwrap();
    assert!(store.path("../window", "pane").is_err());
    assert!(store.path("window", "pane/child").is_err());
}

#[test]
fn key_environment_requires_exact_base64_key_material() {
    assert!(soksak_kit_sidecar_terminal::checkpoint::key_from_base64("not-base64").is_err());
    assert!(soksak_kit_sidecar_terminal::checkpoint::key_from_base64("c2hvcnQ=").is_err());
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [7u8; 32]);
    assert_eq!(
        soksak_kit_sidecar_terminal::checkpoint::key_from_base64(&encoded).unwrap(),
        [7u8; 32]
    );
}

#[test]
fn older_checkpoint_cannot_replace_a_newer_commit() {
    let home = std::env::temp_dir().join(format!("soksak-checkpoint-order-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    let store = CheckpointStore::new(&home, "soksak-sidecar-terminal-test", key()).unwrap();

    store.claim_generation("window-a", "pane-a", 8).unwrap();
    store
        .write("window-a", "pane-a", 8, 3, b"new-generation", &frame())
        .unwrap();
    let stale = store
        .write("window-a", "pane-a", 7, 900, b"old-generation", &frame())
        .unwrap_err();
    assert_eq!(stale.kind(), std::io::ErrorKind::PermissionDenied);
    store
        .write("window-a", "pane-a", 8, 2, b"old-sequence", &frame())
        .unwrap();

    let opened = store.read("window-a", "pane-a").unwrap().unwrap();
    assert_eq!((opened.generation, opened.sequence), (8, 3));
    assert_eq!(opened.paint, b"new-generation");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn a_new_generation_is_selected_by_ownership_not_numeric_magnitude() {
    let home = std::env::temp_dir().join(format!(
        "soksak-checkpoint-generation-owner-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&home);
    let store = CheckpointStore::new(&home, "soksak-sidecar-terminal-test", key()).unwrap();

    store
        .claim_generation("window-a", "pane-a", u64::MAX - 1)
        .unwrap();
    store
        .write("window-a", "pane-a", u64::MAX - 1, 10, b"old-owner", &frame())
        .unwrap();
    store.claim_generation("window-a", "pane-a", 7).unwrap();
    store
        .write("window-a", "pane-a", 7, 1, b"new-owner", &frame())
        .unwrap();
    let stale = store
        .write("window-a", "pane-a", u64::MAX - 1, 11, b"late-old-owner", &frame())
        .unwrap_err();
    assert_eq!(stale.kind(), std::io::ErrorKind::PermissionDenied);

    let opened = store.read("window-a", "pane-a").unwrap().unwrap();
    assert_eq!((opened.generation, opened.sequence), (7, 1));
    assert_eq!(opened.paint, b"new-owner");
    let _ = fs::remove_dir_all(home);
}

#[test]
fn concurrent_checkpoint_commits_do_not_collide_or_regress() {
    const WRITERS: usize = 16;
    let home = std::env::temp_dir().join(format!(
        "soksak-checkpoint-concurrent-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&home);
    let store =
        Arc::new(CheckpointStore::new(&home, "soksak-sidecar-terminal-test", key()).unwrap());
    store.claim_generation("window-a", "pane-a", 9).unwrap();
    let barrier = Arc::new(Barrier::new(WRITERS));
    let writers = (0..WRITERS)
        .map(|sequence| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let paint = vec![sequence as u8; 256 * 1024];
                barrier.wait();
                store.write("window-a", "pane-a", 9, sequence as u64, &paint, &frame())
            })
        })
        .collect::<Vec<_>>();

    for writer in writers {
        writer.join().unwrap().unwrap();
    }
    let opened = store.read("window-a", "pane-a").unwrap().unwrap();
    assert_eq!(opened.sequence, (WRITERS - 1) as u64);
    assert_eq!(opened.paint[0], (WRITERS - 1) as u8);
    let _ = fs::remove_dir_all(home);
}
