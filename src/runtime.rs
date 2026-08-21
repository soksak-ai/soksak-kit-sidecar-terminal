use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use interprocess::local_socket::{prelude::*, GenericFilePath, Listener, ListenerOptions, Stream};
use serde_json::{json, Value};

use crate::checkpoint::{key_from_base64, CheckpointStore, KEY_ENV};
use crate::daemon::{
    ControlClient, ObservationFrame, ObservationStream, PreparedObservationStream, SessionInfo,
};
use crate::{proto, MirrorFactory, TerminalStateMirror};

type PaneKey = (Option<String>, String);
type Registry = Arc<Mutex<HashMap<PaneKey, SessionState>>>;

struct SessionState {
    session: u64,
    generation: u64,
    window: Option<String>,
    pane: String,
    mirror: Arc<Mutex<Box<dyn TerminalStateMirror>>>,
    source_event_sequence: u64,
    source_output_sequence: u64,
    gaps: u64,
    frame_signal: Arc<(Mutex<u64>, Condvar)>,
}

#[derive(Clone)]
pub struct Runtime {
    runtime_root: PathBuf,
    unit_name: &'static str,
    factory: MirrorFactory,
    registry: Registry,
    control: Arc<Mutex<ControlClient>>,
    prepared: Arc<Mutex<HashMap<String, ObservationStream>>>,
    checkpoints: Option<Arc<CheckpointStore>>,
}

impl Runtime {
    pub fn connect(
        home: PathBuf,
        runtime_root: PathBuf,
        unit_name: &'static str,
        factory: MirrorFactory,
    ) -> io::Result<Self> {
        let control = ControlClient::connect(&runtime_root)?;
        let checkpoints = match std::env::var(KEY_ENV) {
            Ok(encoded) if !encoded.is_empty() => Some(Arc::new(CheckpointStore::new(
                &home,
                unit_name,
                key_from_base64(&encoded)?,
            )?)),
            _ => None,
        };
        Ok(Self {
            runtime_root,
            unit_name,
            factory,
            registry: Arc::new(Mutex::new(HashMap::new())),
            control: Arc::new(Mutex::new(control)),
            prepared: Arc::new(Mutex::new(HashMap::new())),
            checkpoints,
        })
    }

    pub fn subscribe_existing(&self, cols: u16, rows: u16) -> io::Result<()> {
        let sessions = self.control.lock().unwrap().list_sessions()?;
        for session in sessions {
            self.subscribe(session, cols, rows)?;
        }
        Ok(())
    }

    fn subscribe(&self, info: SessionInfo, cols: u16, rows: u16) -> io::Result<bool> {
        let key = (info.window_label.clone(), info.pane_id.clone());
        if self.registry.lock().unwrap().contains_key(&key) {
            return Ok(false);
        }
        let observation = ObservationStream::subscribe(&self.runtime_root, info.session)?;
        let mirror = Arc::new(Mutex::new((self.factory)(cols, rows)));
        self.registry.lock().unwrap().insert(
            key.clone(),
            SessionState {
                session: info.session,
                generation: observation.generation(),
                window: info.window_label,
                pane: info.pane_id,
                mirror: mirror.clone(),
                source_event_sequence: observation.start_event_sequence(),
                source_output_sequence: observation.start_output_sequence(),
                gaps: 0,
                frame_signal: Arc::new((
                    Mutex::new(observation.start_output_sequence()),
                    Condvar::new(),
                )),
            },
        );
        self.start_consumer(observation, mirror, key);
        Ok(true)
    }

    fn ensure_session(&self, request: &Value) -> Value {
        let Some((window, pane)) = pane_key(request) else {
            return result_error("INVALID_PARAMS", "window and pane are required");
        };
        let cols = request
            .get("cols")
            .and_then(Value::as_u64)
            .unwrap_or(80)
            .max(1) as u16;
        let rows = request
            .get("rows")
            .and_then(Value::as_u64)
            .unwrap_or(24)
            .max(1) as u16;
        if let Some(token) = request.get("observerToken").and_then(Value::as_str) {
            let observation = self.prepared.lock().unwrap().remove(token);
            let Some(mut observation) = observation else {
                return result_error(
                    "OBSERVER_NOT_READY",
                    "prepared observer token is absent or expired",
                );
            };
            if let Err(error) = observation.receive_opened() {
                return result_error("OBSERVER_OPEN_FAILED", &error.to_string());
            }
            let info = SessionInfo {
                session: observation.session(),
                generation: observation.generation(),
                pane_id: pane.clone(),
                window_label: window.clone(),
            };
            return match self.subscribe_stream(info, observation, cols, rows) {
                Ok(()) => result_ok(json!({ "pane": pane, "subscribed": true })),
                Err(error) => result_error("SUBSCRIBE_FAILED", &error.to_string()),
            };
        }
        let sessions = match self.control.lock().unwrap().list_sessions() {
            Ok(sessions) => sessions,
            Err(error) => return result_error("PTY_UNAVAILABLE", &error.to_string()),
        };
        let Some(info) = sessions
            .into_iter()
            .find(|info| info.window_label.as_deref() == window.as_deref() && info.pane_id == pane)
        else {
            return result_error("NOT_FOUND", "no live PTY session for this terminal key");
        };
        match self.subscribe(info, cols, rows) {
            Ok(subscribed) => result_ok(json!({ "pane": pane, "subscribed": subscribed })),
            Err(error) => result_error("SUBSCRIBE_FAILED", &error.to_string()),
        }
    }

    fn prepare_session(&self, request: &Value) -> Value {
        let Some((window, pane)) = pane_key(request) else {
            return result_error("INVALID_PARAMS", "window and pane are required");
        };
        let Some(window) = window else {
            return result_error("INVALID_PARAMS", "window is required");
        };
        let token =
            match self
                .control
                .lock()
                .unwrap()
                .prepare_observer(&window, &pane, self.unit_name)
            {
                Ok(token) => token,
                Err(error) => return result_error("OBSERVER_PREPARE_FAILED", &error.to_string()),
            };
        let observation = match PreparedObservationStream::connect(&self.runtime_root, &token) {
            Ok(stream) => stream.into_inner(),
            Err(error) => return result_error("OBSERVER_STREAM_FAILED", &error.to_string()),
        };
        self.prepared
            .lock()
            .unwrap()
            .insert(token.clone(), observation);
        let prepared = self.prepared.clone();
        let expiring = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(30));
            prepared.lock().unwrap().remove(&expiring);
        });
        result_ok(json!({ "observerToken": token }))
    }

    fn subscribe_stream(
        &self,
        info: SessionInfo,
        observation: ObservationStream,
        cols: u16,
        rows: u16,
    ) -> io::Result<()> {
        let key = (info.window_label.clone(), info.pane_id.clone());
        let mirror = Arc::new(Mutex::new((self.factory)(cols, rows)));
        self.registry.lock().unwrap().insert(
            key.clone(),
            SessionState {
                session: info.session,
                generation: observation.generation(),
                window: info.window_label,
                pane: info.pane_id,
                mirror: mirror.clone(),
                source_event_sequence: observation.start_event_sequence(),
                source_output_sequence: observation.start_output_sequence(),
                gaps: 0,
                frame_signal: Arc::new((
                    Mutex::new(observation.start_output_sequence()),
                    Condvar::new(),
                )),
            },
        );
        self.start_consumer(observation, mirror, key);
        Ok(())
    }

    fn start_consumer(
        &self,
        observation: ObservationStream,
        mirror: Arc<Mutex<Box<dyn TerminalStateMirror>>>,
        key: PaneKey,
    ) {
        let registry = self.registry.clone();
        let checkpoint = self.checkpoints.as_ref().map(|store| {
            start_checkpoint_worker(store.clone(), self.unit_name, key.clone(), mirror.clone())
        });
        std::thread::spawn(move || {
            consume_observations(observation, mirror, registry, key, checkpoint)
        });
    }

    fn rehydrate(&self, request: &Value) -> Value {
        let Some(key) = pane_key(request) else {
            return result_error("INVALID_PARAMS", "window and pane are required");
        };
        let registry = self.registry.lock().unwrap();
        let Some(state) = registry.get(&key) else {
            return result_error("NOT_FOUND", "no live terminal-state mirror for this key");
        };
        if state.gaps > 0 {
            return result_error(
                "SOURCE_GAP",
                "the terminal-state observer missed source events",
            );
        }
        let session = state.session;
        let sequence = state.source_output_sequence;
        let generation = state.generation;
        let event_sequence = state.source_event_sequence;
        let mirror = state.mirror.lock().unwrap();
        let paint = mirror.rehydrate();
        let frame = mirror.frame();
        let alt_active = mirror.alt_active();
        drop(mirror);
        drop(registry);
        let lease = match self.control.lock().unwrap().lease(session, sequence) {
            Ok(lease) => lease,
            Err(error) => return result_error("LEASE_FAILED", &error.to_string()),
        };
        result_ok(json!({
            "paint": B64.encode(paint),
            "frame": frame,
            "uptoSeq": sequence,
            "eventSequence": event_sequence,
            "generation": generation,
            "altActive": alt_active,
            "leaseToken": lease.token,
            "leaseExpiresAtMs": lease.expires_at_ms,
        }))
    }

    fn resize(&self, request: &Value) -> Value {
        let Some(key) = pane_key(request) else {
            return result_error("INVALID_PARAMS", "window and pane are required");
        };
        let Some(cols) = request
            .get("cols")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
        else {
            return result_error("INVALID_PARAMS", "positive cols are required");
        };
        let Some(rows) = request
            .get("rows")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
        else {
            return result_error("INVALID_PARAMS", "positive rows are required");
        };
        let registry = self.registry.lock().unwrap();
        let Some(state) = registry.get(&key) else {
            return result_error("NOT_FOUND", "no live terminal-state mirror for this key");
        };
        state
            .mirror
            .lock()
            .unwrap()
            .resize(cols as u16, rows as u16);
        result_ok(json!({ "cols": cols, "rows": rows }))
    }

    fn status(&self) -> Value {
        let registry = self.registry.lock().unwrap();
        let sessions: Vec<Value> = registry
            .values()
            .map(|state| {
                let mirror = state.mirror.lock().unwrap();
                json!({
                    "session": state.session,
                    "generation": state.generation,
                    "window": state.window,
                    "pane": state.pane,
                    "eventSequence": state.source_event_sequence,
                    "outputSequence": state.source_output_sequence,
                    "gaps": state.gaps,
                    "altActive": mirror.alt_active(),
                    "suppressedReplies": mirror.suppressed_replies(),
                })
            })
            .collect();
        result_ok(json!({ "sessions": sessions }))
    }

    fn frame(&self, request: &Value) -> Value {
        let Some(key) = pane_key(request) else {
            return result_error("INVALID_PARAMS", "window and pane are required");
        };
        let (mirror, signal) = {
            let registry = self.registry.lock().unwrap();
            let Some(state) = registry.get(&key) else {
                return result_error("NOT_FOUND", "no live terminal-state mirror for this key");
            };
            (state.mirror.clone(), state.frame_signal.clone())
        };
        if let Some(after) = request.get("afterSequence").and_then(Value::as_u64) {
            let (sequence, ready) = &*signal;
            let mut sequence = sequence.lock().unwrap();
            while *sequence < after {
                sequence = ready.wait(sequence).unwrap();
            }
        }
        let frame = mirror.lock().unwrap().frame();
        match serde_json::to_value(frame) {
            Ok(frame) => result_ok(frame),
            Err(error) => result_error("FRAME_FAILED", &error.to_string()),
        }
    }

    fn archived(&self, request: &Value) -> Value {
        let Some((window, pane)) = pane_key(request) else {
            return result_error("INVALID_PARAMS", "window and pane are required");
        };
        let Some(store) = &self.checkpoints else {
            return result_error(
                "CHECKPOINT_UNAVAILABLE",
                "this provider has no checkpoint key",
            );
        };
        let window = window.as_deref().unwrap_or("__no-window__");
        match store.read(window, &pane) {
            Ok(Some(checkpoint)) => result_ok(json!({
                "paint": B64.encode(&checkpoint.paint),
                "frame": checkpoint.frame,
                "generation": checkpoint.generation,
                "uptoSeq": checkpoint.sequence,
            })),
            Ok(None) => result_error("NOT_FOUND", "no archived checkpoint for this terminal key"),
            Err(error) => result_error("CHECKPOINT_CORRUPT", &error.to_string()),
        }
    }

    fn archive(&self, request: &Value) -> Value {
        let Some(key) = pane_key(request) else {
            return result_error("INVALID_PARAMS", "window and pane are required");
        };
        let Some(store) = &self.checkpoints else {
            return result_error(
                "CHECKPOINT_UNAVAILABLE",
                "this provider has no checkpoint key",
            );
        };
        let registry = self.registry.lock().unwrap();
        let Some(state) = registry.get(&key) else {
            return result_error("NOT_FOUND", "no live terminal-state mirror for this key");
        };
        let generation = state.generation;
        let sequence = state.source_output_sequence;
        let mirror = state.mirror.lock().unwrap();
        let paint = mirror.cold_paint();
        let frame = mirror.frame();
        let window = key.0.as_deref().unwrap_or("__no-window__");
        match store.write(window, &key.1, generation, sequence, &paint, &frame) {
            Ok(()) => result_ok(json!({
                "generation": generation, "uptoSeq": sequence, "bytes": paint.len(),
            })),
            Err(error) => result_error("CHECKPOINT_WRITE_FAILED", &error.to_string()),
        }
    }

    fn retire(&self, request: &Value) -> Value {
        let Some((window, pane)) = pane_key(request) else {
            return result_error("INVALID_PARAMS", "window and pane are required");
        };
        let Some(store) = &self.checkpoints else {
            return result_error(
                "CHECKPOINT_UNAVAILABLE",
                "this provider has no checkpoint key",
            );
        };
        let window = window.as_deref().unwrap_or("__no-window__");
        match store.remove(window, &pane) {
            Ok(removed) => result_ok(json!({ "removed": removed })),
            Err(error) => result_error("CHECKPOINT_REMOVE_FAILED", &error.to_string()),
        }
    }

    fn command(&self, command: &str, request: &Value) -> Value {
        match command {
            "terminal.prepareSession" => self.prepare_session(request),
            "terminal.ensureSession" => self.ensure_session(request),
            "terminal.rehydrate" => self.rehydrate(request),
            "terminal.resize" => self.resize(request),
            "terminal.status" => self.status(),
            "terminal.archived" => self.archived(request),
            "terminal.archive" => self.archive(request),
            "terminal.retire" => self.retire(request),
            "terminal.frame" => self.frame(request),
            _ => result_error("UNKNOWN_COMMAND", "unknown terminal-state command"),
        }
    }

    pub fn serve(self, listener: Listener, token: String) {
        for connection in listener.incoming().flatten() {
            let runtime = self.clone();
            let token = token.clone();
            std::thread::spawn(move || {
                if let Err(error) = serve_connection(connection, &runtime, &token) {
                    eprintln!("{}: service connection failed: {error}", runtime.unit_name);
                }
            });
        }
    }
}

fn consume_observations(
    mut observations: ObservationStream,
    mirror: Arc<Mutex<Box<dyn TerminalStateMirror>>>,
    registry: Registry,
    key: PaneKey,
    checkpoint: Option<Sender<CheckpointEvent>>,
) {
    loop {
        let frame = match observations.next_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) | Err(_) => break,
        };
        let mut states = registry.lock().unwrap();
        let Some(state) = states.get_mut(&key) else {
            return;
        };
        match frame {
            ObservationFrame::Output {
                event_sequence,
                through_sequence,
                bytes,
                ..
            } => {
                mirror.lock().unwrap().feed(&bytes);
                state.source_event_sequence = event_sequence;
                state.source_output_sequence = through_sequence;
                let (sequence, ready) = &*state.frame_signal;
                *sequence.lock().unwrap() = through_sequence;
                ready.notify_all();
                if let Some(checkpoint) = &checkpoint {
                    let _ = checkpoint.send(CheckpointEvent::Dirty {
                        generation: state.generation,
                        sequence: through_sequence,
                    });
                }
            }
            ObservationFrame::Resize {
                event_sequence,
                cols,
                rows,
            } => {
                mirror.lock().unwrap().resize(cols, rows);
                state.source_event_sequence = event_sequence;
            }
            ObservationFrame::Gap {
                through_event_sequence,
                through_sequence,
                ..
            } => {
                state.gaps += 1;
                state.source_event_sequence = through_event_sequence;
                state.source_output_sequence = through_sequence;
                let (sequence, ready) = &*state.frame_signal;
                *sequence.lock().unwrap() = through_sequence;
                ready.notify_all();
            }
            ObservationFrame::End { .. } => {
                if let Some(checkpoint) = &checkpoint {
                    let _ = checkpoint.send(CheckpointEvent::Final {
                        generation: state.generation,
                        sequence: state.source_output_sequence,
                    });
                }
                break;
            }
            ObservationFrame::Opened { .. } => {
                state.gaps += 1;
                break;
            }
        }
    }
    registry.lock().unwrap().remove(&key);
}

enum CheckpointEvent {
    Dirty { generation: u64, sequence: u64 },
    Final { generation: u64, sequence: u64 },
}

fn start_checkpoint_worker(
    store: Arc<CheckpointStore>,
    unit_name: &'static str,
    key: PaneKey,
    mirror: Arc<Mutex<Box<dyn TerminalStateMirror>>>,
) -> Sender<CheckpointEvent> {
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("{unit_name}-checkpoint"))
        .spawn(move || checkpoint_worker(store, key, mirror, receive))
        .expect("checkpoint worker thread");
    send
}

fn checkpoint_worker(
    store: Arc<CheckpointStore>,
    key: PaneKey,
    mirror: Arc<Mutex<Box<dyn TerminalStateMirror>>>,
    receive: Receiver<CheckpointEvent>,
) {
    while let Ok(first) = receive.recv() {
        let deadline = Instant::now() + Duration::from_millis(250);
        let mut latest = first;
        let mut final_event = matches!(latest, CheckpointEvent::Final { .. });
        while !final_event {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match receive.recv_timeout(remaining) {
                Ok(event) => {
                    final_event = matches!(event, CheckpointEvent::Final { .. });
                    latest = event;
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let (generation, sequence) = match latest {
            CheckpointEvent::Dirty {
                generation,
                sequence,
            }
            | CheckpointEvent::Final {
                generation,
                sequence,
            } => (generation, sequence),
        };
        let mirror = mirror.lock().unwrap();
        let paint = mirror.cold_paint();
        let frame = mirror.frame();
        let window = key.0.as_deref().unwrap_or("__no-window__");
        if let Err(error) = store.write(window, &key.1, generation, sequence, &paint, &frame) {
            eprintln!("checkpoint write failed: {error}");
        }
        if final_event {
            return;
        }
    }
}

fn pane_key(request: &Value) -> Option<PaneKey> {
    Some((
        request
            .get("window")
            .and_then(Value::as_str)
            .map(str::to_string),
        request.get("pane")?.as_str()?.to_string(),
    ))
}

fn serve_connection(connection: Stream, runtime: &Runtime, token: &str) -> io::Result<()> {
    let (recv, mut send) = connection.split();
    let reader = BufReader::new(recv);
    let mut greeted = false;
    for line in reader.lines() {
        let line = line?;
        let request: Value = serde_json::from_str(&line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let id = request.get("id").and_then(Value::as_str).unwrap_or("");
        let command = request.get("command").and_then(Value::as_str).unwrap_or("");
        let response = if command == "system.hello" {
            let protocol = request.pointer("/args/protocol").and_then(Value::as_u64);
            let received_token = request.pointer("/args/token").and_then(Value::as_str);
            if protocol != Some(proto::CONTROL_PROTOCOL as u64) || received_token != Some(token) {
                envelope_error(id, "GREETING", "invalid protocol or token")
            } else {
                greeted = true;
                envelope_ok(
                    id,
                    json!({
                        "protocol": proto::CONTROL_PROTOCOL,
                        "identity": runtime.unit_name,
                    "commands": { "commands": [
                        { "name": "terminal.prepareSession", "owner": "plugin" },
                            { "name": "terminal.ensureSession", "owner": "plugin" },
                            { "name": "terminal.rehydrate", "owner": "plugin" },
                            { "name": "terminal.resize", "owner": "plugin" },
                            { "name": "terminal.status", "owner": "plugin" }
                            ,{ "name": "terminal.archived", "owner": "plugin" }
                            ,{ "name": "terminal.archive", "owner": "plugin" }
                            ,{ "name": "terminal.retire", "owner": "plugin" }
                            ,{ "name": "terminal.frame", "owner": "plugin" }
                        ], "unserved": [] },
                        "language": "en", "languages": ["en"]
                    }),
                )
            }
        } else if !greeted {
            envelope_error(id, "GREETING", "system.hello is required")
        } else {
            let request = request
                .pointer("/args/request")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = runtime.command(command, &request);
            if result["ok"] == true {
                envelope_ok(id, result.get("data").cloned().unwrap_or(Value::Null))
            } else {
                envelope_error(
                    id,
                    result
                        .get("code")
                        .and_then(Value::as_str)
                        .unwrap_or("FAILED"),
                    result
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("request failed"),
                )
            }
        };
        writeln!(send, "{response}")?;
    }
    Ok(())
}

fn result_ok(data: Value) -> Value {
    json!({ "ok": true, "code": "OK", "data": data })
}

fn result_error(code: &str, message: &str) -> Value {
    json!({ "ok": false, "code": code, "message": message })
}

fn envelope_ok(id: &str, data: Value) -> Value {
    json!({ "id": id, "ok": true, "result": { "code": "OK", "data": data } })
}

fn envelope_error(id: &str, code: &str, message: &str) -> Value {
    json!({ "id": id, "ok": false, "error": message, "result": { "code": code, "data": null } })
}

pub fn bind_service(home: &Path, unit_name: &str) -> io::Result<Listener> {
    let path = proto::service_socket_path(home, unit_name);
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let name = path.as_os_str().to_fs_name::<GenericFilePath>()?;
    ListenerOptions::new().name(name).create_sync()
}

pub fn service_token_path(runtime_root: &Path, unit_name: &str) -> PathBuf {
    runtime_root.join(format!("{unit_name}-p1.token"))
}

pub fn run_service(
    unit_name: &'static str,
    factory: MirrorFactory,
    arguments: impl IntoIterator<Item = String>,
) -> io::Result<()> {
    let (home, runtime_root) = parse_roots(arguments)?;
    let runtime = Runtime::connect(home, runtime_root.clone(), unit_name, factory)?;
    runtime.subscribe_existing(80, 24)?;
    let listener = bind_service(&runtime_root, unit_name)?;
    let token = load_or_create_token(&runtime_root, unit_name)?;
    println!(
        "{}",
        json!({
            "protocol": proto::CONTROL_PROTOCOL,
            "socket": proto::service_socket_path(&runtime_root, unit_name),
            "token": token,
        })
    );
    std::io::stdout().flush()?;
    runtime.serve(listener, token);
    Ok(())
}

fn parse_roots(arguments: impl IntoIterator<Item = String>) -> io::Result<(PathBuf, PathBuf)> {
    let mut arguments = arguments.into_iter();
    let mut home = None;
    let mut runtime_root = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-home" => home = arguments.next().map(PathBuf::from),
            "-runtime" => runtime_root = arguments.next().map(PathBuf::from),
            _ => {}
        }
    }
    let home = home.filter(|path| path.is_absolute()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "-home requires an absolute path",
        )
    })?;
    let runtime_root = runtime_root
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "-runtime requires an absolute path",
            )
        })?;
    Ok((home, runtime_root))
}

fn load_or_create_token(home: &Path, unit_name: &str) -> io::Result<String> {
    let path = service_token_path(home, unit_name);
    if let Ok(token) = std::fs::read_to_string(&path) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| io::Error::other(format!("random token generation failed: {error}")))?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(token)
}

pub struct ServiceClient {
    send: interprocess::local_socket::SendHalf,
    recv: BufReader<interprocess::local_socket::RecvHalf>,
}

impl ServiceClient {
    pub fn connect(home: &Path, unit_name: &str, token: &str) -> io::Result<Self> {
        let path = proto::service_socket_path(home, unit_name);
        let name = path.as_os_str().to_fs_name::<GenericFilePath>()?;
        let (recv, send) = Stream::connect(name)?.split();
        let mut client = Self {
            send,
            recv: BufReader::new(recv),
        };
        client.send(&proto::hello("greeting", token))?;
        proto::response_data(&client.receive()?)
            .map_err(|message| io::Error::new(io::ErrorKind::PermissionDenied, message))?;
        Ok(client)
    }

    pub fn request(&mut self, request: Value) -> io::Result<Value> {
        self.send(&request)?;
        self.receive()
    }

    pub fn rehydrate(&mut self, window: Option<&str>, pane: &str) -> io::Result<Value> {
        self.request(proto::request(
            "client",
            "terminal.rehydrate",
            json!({ "window": window, "pane": pane }),
        ))
    }

    fn send(&mut self, request: &Value) -> io::Result<()> {
        writeln!(self.send, "{request}")?;
        self.send.flush()
    }

    fn receive(&mut self) -> io::Result<Value> {
        let mut line = String::new();
        if self.recv.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "service closed",
            ));
        }
        serde_json::from_str(line.trim())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::TerminalFrame;
    use std::sync::mpsc;

    struct BlockingMirror {
        entered: Sender<()>,
        release: Receiver<()>,
    }

    impl TerminalStateMirror for BlockingMirror {
        fn feed(&mut self, _bytes: &[u8]) {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
        }
        fn resize(&mut self, _cols: u16, _rows: u16) {}
        fn rehydrate(&self) -> Vec<u8> { vec![] }
        fn cold_paint(&self) -> Vec<u8> { vec![] }
        fn frame(&self) -> TerminalFrame {
            TerminalFrame { cols: 1, rows: 1, cursor: (0, 0), alt_active: false, lines: vec![] }
        }
        fn alt_active(&self) -> bool { false }
        fn suppressed_replies(&self) -> u64 { 0 }
    }

    #[test]
    fn unit_endpoints_are_independent() {
        let home = Path::new("/identity");
        assert_ne!(
            proto::service_socket_path(home, "soksak-sidecar-terminal-vt100"),
            proto::service_socket_path(home, "soksak-sidecar-terminal-ghostty")
        );
    }

    #[test]
    fn slow_engine_feed_does_not_hold_the_session_registry() {
        let (entered_send, entered_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        let mirror: Arc<Mutex<Box<dyn TerminalStateMirror>>> = Arc::new(Mutex::new(Box::new(
            BlockingMirror { entered: entered_send, release: release_receive },
        )));
        let key = (Some("window".to_string()), "pane".to_string());
        let registry: Registry = Arc::new(Mutex::new(HashMap::from([(
            key.clone(),
            SessionState {
                session: 1, generation: 1, window: key.0.clone(), pane: key.1.clone(),
                mirror: mirror.clone(), source_event_sequence: 0, source_output_sequence: 0,
                gaps: 0, frame_signal: Arc::new((Mutex::new(0), Condvar::new())),
            },
        )])));
        let applying = {
            let registry = registry.clone();
            let mirror = mirror.clone();
            let key = key.clone();
            std::thread::spawn(move || apply_observation(
                ObservationFrame::Output { event_sequence: 1, from_sequence: 0, through_sequence: 1, bytes: vec![b'x'] },
                &mirror, &registry, &key, None,
            ))
        };
        entered_receive.recv().unwrap();
        assert!(registry.try_lock().is_ok(), "engine feed held the session registry");
        release_send.send(()).unwrap();
        assert!(applying.join().unwrap());
    }
}
