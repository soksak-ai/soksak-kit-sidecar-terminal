use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Value};

use crate::daemon::{ControlClient, LiveStream};
use crate::proto;
use crate::runtime::ServiceClient;

const MARKER: &str = "SOKSAK-OBSERVE-WARM-RESTORE";
const LIVE_MARKER: &str = "SOKSAK-LIVE-AFTER-SNAPSHOT";
const CHECKPOINT_MARKER: &str = "SOKSAK-ENCRYPTED-CHECKPOINT";

struct Process {
    name: &'static str,
    child: Child,
    stdout: BufReader<ChildStdout>,
    stderr: BufReader<ChildStderr>,
}

impl Process {
    fn start(name: &'static str, mut command: Command) -> Self {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = BufReader::new(child.stderr.take().unwrap());
        Self {
            name,
            child,
            stdout,
            stderr,
        }
    }

    fn announcement(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        if line.is_empty() {
            let mut error = String::new();
            self.stderr.read_line(&mut error).unwrap();
            panic!("{} exited without readiness: {}", self.name, error.trim());
        }
        serde_json::from_str(line.trim()).unwrap()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn assert_warm_restore(pty_binary: &Path, service_binary: &Path, sidecar_id: &'static str) {
    let home = fresh_home();
    let runtime_root = fresh_runtime();
    let mut pty_command = Command::new(pty_binary);
    pty_command.args([
        "-home",
        home.to_str().unwrap(),
        "-runtime",
        runtime_root.to_str().unwrap(),
        "-shell",
        "/bin/sh",
    ]);
    let mut pty = Process::start("PTY", pty_command);
    assert_eq!(pty.announcement()["protocol"], proto::CONTROL_PROTOCOL);

    let mut service_command = Command::new(service_binary);
    service_command.args([
        "-home",
        home.to_str().unwrap(),
        "-runtime",
        runtime_root.to_str().unwrap(),
    ]);
    service_command.env(crate::checkpoint::KEY_ENV, B64.encode([0x51u8; 32]));
    let mut service = Process::start("terminal-state service", service_command);
    let announcement = service.announcement();
    assert_eq!(announcement["protocol"], proto::CONTROL_PROTOCOL);
    let token = announcement["token"].as_str().unwrap();
    let mut client = ServiceClient::connect(&runtime_root, sidecar_id, token).unwrap();
    let window = "win-integration";
    let pane = "tab-integration";
    let prepared = response_data(
        client
            .request(proto::request(
                "prepare",
                "terminal.prepareSession",
                json!({ "window": window, "pane": pane, "cols": 80, "rows": 24 }),
            ))
            .unwrap(),
    );
    let observer_token = prepared["observerToken"].as_str().unwrap();
    let mut control = ControlClient::connect(&runtime_root).unwrap();
    let session = open_shell(&mut control, window, pane, observer_token);
    let ensured = response_data(
        client
            .request(proto::request(
                "ensure",
                "terminal.ensureSession",
                json!({
                    "window": window, "pane": pane, "cols": 80, "rows": 24,
                    "observerToken": observer_token,
                }),
            ))
            .unwrap(),
    );
    assert_eq!(ensured["subscribed"], true);

    write(&mut control, session, &format!("printf '{MARKER}\\n'\n"));
    let mut restored = None;
    for _ in 0..100 {
        let data = response_data(client.rehydrate(Some(window), pane).unwrap());
        let paint = data["paint"]
            .as_str()
            .and_then(|value| B64.decode(value).ok())
            .unwrap_or_default();
        if paint
            .windows(MARKER.len())
            .any(|window| window == MARKER.as_bytes())
        {
            restored = Some((data, paint));
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (data, _paint) = restored.expect("marker did not reach terminal-state mirror");
    let frame = response_data(
        client
            .request(proto::request(
                "frame",
                "terminal.frame",
                json!({ "window": window, "pane": pane, "afterSequence": data["uptoSeq"] }),
            ))
            .unwrap(),
    );
    let frame_text = frame["lines"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|line| {
            line.as_array()
                .unwrap()
                .iter()
                .filter_map(|cell| cell["text"].as_str())
        })
        .collect::<String>();
    assert!(
        frame_text.contains(MARKER),
        "provider frame did not contain the observed marker: {frame_text:?}"
    );
    write(
        &mut control,
        session,
        &format!("printf '{CHECKPOINT_MARKER}\n'\n"),
    );
    std::thread::sleep(std::time::Duration::from_millis(350));
    let archived = response_data(
        client
            .request(proto::request(
                "archived",
                "terminal.archived",
                json!({ "window": window, "pane": pane }),
            ))
            .unwrap(),
    );
    let archived_paint = B64.decode(archived["paint"].as_str().unwrap()).unwrap();
    assert!(archived_paint
        .windows(CHECKPOINT_MARKER.len())
        .any(|value| value == CHECKPOINT_MARKER.as_bytes()));
    let checkpoint_dir = home.join("terminal-checkpoints").join(sidecar_id);
    for entry in std::fs::read_dir(&checkpoint_dir).unwrap() {
        let disk = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(!disk
            .windows(CHECKPOINT_MARKER.len())
            .any(|value| value == CHECKPOINT_MARKER.as_bytes()));
    }
    assert!(data["uptoSeq"].as_u64().unwrap() >= MARKER.len() as u64);
    let snapshot_sequence = data["uptoSeq"].as_u64().unwrap();
    let lease = data["leaseToken"]
        .as_str()
        .expect("rehydrate returned no lease");
    let mut live = LiveStream::attach_lease(&runtime_root, lease).unwrap();
    assert_eq!(live.start_sequence, snapshot_sequence);
    write(
        &mut control,
        session,
        &format!("printf '{LIVE_MARKER}\\n'\n"),
    );
    let mut received = Vec::new();
    let mut buffer = [0u8; 4096];
    while !received
        .windows(LIVE_MARKER.len())
        .any(|window| window == LIVE_MARKER.as_bytes())
    {
        let count = live.read(&mut buffer).unwrap();
        assert!(
            count > 0,
            "live stream ended before the post-snapshot marker"
        );
        received.extend_from_slice(&buffer[..count]);
    }
    let retired = response_data(
        client
            .request(proto::request(
                "retire",
                "terminal.retire",
                json!({ "window": window, "pane": pane }),
            ))
            .unwrap(),
    );
    assert_eq!(retired["removed"], true);

    drop(client);
    drop(service);
    drop(control);
    drop(pty);
    std::fs::remove_dir_all(home).unwrap();
    std::fs::remove_dir_all(runtime_root).unwrap();
}

pub fn assert_absent_service_fails(home: &Path, sidecar_id: &str) {
    let result = ServiceClient::connect(home, sidecar_id, "unused");
    assert!(result.is_err(), "absent service must fail");
}

fn fresh_runtime() -> PathBuf {
    static RUN: AtomicU64 = AtomicU64::new(1);
    let sequence = RUN.fetch_add(1, Ordering::Relaxed);
    let root = PathBuf::from(format!("/tmp/str-{}-{sequence}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn fresh_home() -> PathBuf {
    static RUN: AtomicU64 = AtomicU64::new(1);
    let sequence = RUN.fetch_add(1, Ordering::Relaxed);
    let home = PathBuf::from(format!(
        "/tmp/soksak-term-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn open_shell(control: &mut ControlClient, window: &str, pane: &str, observer_token: &str) -> u64 {
    control
        .request(
            "pty.open",
            json!({
                "paneId": pane, "cols": 80, "rows": 24, "shell": "/bin/sh",
                "env": [["TERM", "xterm-256color"]], "windowLabel": window,
                "observerToken": observer_token,
            }),
        )
        .unwrap()["session"]
        .as_u64()
        .unwrap()
}

fn write(control: &mut ControlClient, session: u64, text: &str) {
    control
        .request(
            "pty.write",
            json!({
                "session": session, "dataB64": B64.encode(text.as_bytes())
            }),
        )
        .unwrap();
}

fn response_data(response: Value) -> Value {
    assert_eq!(response["ok"], true, "{response}");
    response
        .pointer("/result/data")
        .cloned()
        .unwrap_or(Value::Null)
}
