use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use interprocess::local_socket::{Listener, ListenerOptions, Stream, prelude::*};
use serde_json::{Value, json};

use crate::checkpoint::{CheckpointStore, KEY_ENV, key_from_base64};
use crate::daemon::{
    ControlClient, ObservationFrame, ObservationStream, PreparedObservationStream, SessionInfo,
};
use crate::frame::{FrameReply, FrameSubscribers, delta};
use crate::mirror::MirrorCapabilities;
use crate::{MirrorFactory, TerminalStateMirror, proto};

type PaneKey = (Option<String>, String);
type Registry = Arc<Mutex<HashMap<PaneKey, SessionState>>>;
type SharedMirror = Arc<Mutex<Box<dyn TerminalStateMirror>>>;

const FRAME_TIMEOUT_DEFAULT_MS: u64 = 10_000;
const FRAME_TIMEOUT_MAX_MS: u64 = 30_000;

struct SessionState {
    session: u64,
    generation: u64,
    window: Option<String>,
    pane: String,
    mirror: SharedMirror,
    mirror_output_sequence: Arc<AtomicU64>,
    source_event_sequence: u64,
    source_output_sequence: u64,
    gaps: u64,
    alt_active: bool,
    suppressed_replies: u64,
    frame_signal: Arc<(Mutex<u64>, Condvar)>,
    size_signal: Arc<(Mutex<(u16, u16)>, Condvar)>,
    frame_subscribers: Arc<Mutex<FrameSubscribers>>,
}

fn new_session_state(
    info: SessionInfo,
    observation: &ObservationStream,
    mirror: SharedMirror,
    cols: u16,
    rows: u16,
) -> SessionState {
    let start = observation.start_output_sequence();
    SessionState {
        session: info.session,
        generation: observation.generation(),
        window: info.window_label,
        pane: info.pane_id,
        mirror,
        mirror_output_sequence: Arc::new(AtomicU64::new(start)),
        source_event_sequence: observation.start_event_sequence(),
        source_output_sequence: start,
        gaps: 0,
        alt_active: false,
        suppressed_replies: 0,
        frame_signal: Arc::new((Mutex::new(start), Condvar::new())),
        size_signal: Arc::new((Mutex::new((cols, rows)), Condvar::new())),
        frame_subscribers: Arc::new(Mutex::new(FrameSubscribers::default())),
    }
}

fn status_snapshot(registry: &Registry, capabilities: MirrorCapabilities) -> Value {
    let registry = registry.lock().unwrap();
    let sessions: Vec<Value> = registry
        .values()
        .map(|state| {
            let size = *state.size_signal.0.lock().unwrap();
            json!({
                "session": state.session,
                "generation": state.generation,
                "window": state.window,
                "pane": state.pane,
                "eventSequence": state.source_event_sequence,
                "cols": size.0,
                "rows": size.1,
                "outputSequence": state.source_output_sequence,
                "gaps": state.gaps,
                "altActive": state.alt_active,
                "suppressedReplies": state.suppressed_replies,
            })
        })
        .collect();
    json!({ "sessions": sessions, "capabilities": capabilities })
}

#[derive(Clone)]
pub struct Runtime {
    runtime_root: PathBuf,
    sidecar_id: &'static str,
    factory: MirrorFactory,
    capabilities: MirrorCapabilities,
    registry: Registry,
    control: Arc<Mutex<ControlClient>>,
    prepared: Arc<Mutex<HashMap<String, ObservationStream>>>,
    checkpoints: Option<Arc<CheckpointStore>>,
}

impl Runtime {
    pub fn connect(
        home: PathBuf,
        runtime_root: PathBuf,
        sidecar_id: &'static str,
        factory: MirrorFactory,
    ) -> io::Result<Self> {
        let control = ControlClient::connect(&runtime_root)?;
        let checkpoints = match std::env::var(KEY_ENV) {
            Ok(encoded) if !encoded.is_empty() => Some(Arc::new(CheckpointStore::new(
                &home,
                sidecar_id,
                key_from_base64(&encoded)?,
            )?)),
            _ => None,
        };
        let capabilities = factory(1, 1).capabilities();
        Ok(Self {
            runtime_root,
            sidecar_id,
            factory,
            capabilities,
            registry: Arc::new(Mutex::new(HashMap::new())),
            control: Arc::new(Mutex::new(control)),
            prepared: Arc::new(Mutex::new(HashMap::new())),
            checkpoints,
        })
    }

    fn subscribe(&self, info: SessionInfo, cols: u16, rows: u16) -> io::Result<bool> {
        let key = (info.window_label.clone(), info.pane_id.clone());
        if self.registry.lock().unwrap().contains_key(&key) {
            return Ok(false);
        }
        let observation = ObservationStream::subscribe(&self.runtime_root, info.session)?;
        let mirror = create_session_mirror(
            self.factory,
            self.checkpoints.as_deref(),
            &key,
            cols,
            rows,
            false,
        );
        self.registry.lock().unwrap().insert(
            key.clone(),
            new_session_state(info, &observation, mirror.clone(), cols, rows),
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
                .prepare_observer(&window, &pane, self.sidecar_id)
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
        let mirror = create_session_mirror(
            self.factory,
            self.checkpoints.as_deref(),
            &key,
            cols,
            rows,
            true,
        );
        self.registry.lock().unwrap().insert(
            key.clone(),
            new_session_state(info, &observation, mirror.clone(), cols, rows),
        );
        self.start_consumer(observation, mirror, key);
        Ok(())
    }

    fn start_consumer(&self, observation: ObservationStream, mirror: SharedMirror, key: PaneKey) {
        let registry = self.registry.clone();
        let checkpoint = self.checkpoints.as_ref().map(|store| {
            start_checkpoint_worker(store.clone(), self.sidecar_id, key.clone(), mirror.clone())
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
        let mirror_output_sequence = state.mirror_output_sequence.clone();
        let generation = state.generation;
        let event_sequence = state.source_event_sequence;
        let mirror = state.mirror.lock().unwrap();
        let sequence = mirror_output_sequence.load(Ordering::Acquire);
        let paint = mirror.rehydrate();
        let frame = FrameReply::full(&mirror.frame(), sequence);
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
        result_ok(status_snapshot(&self.registry, self.capabilities))
    }

    fn wait_size(&self, request: &Value) -> Value {
        let Some(key) = pane_key(request) else {
            return result_error("INVALID_PARAMS", "window and pane are required");
        };
        let Some(cols) = request
            .get("cols")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0 && *value <= u16::MAX as u64)
        else {
            return result_error("INVALID_PARAMS", "valid cols are required");
        };
        let Some(rows) = request
            .get("rows")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0 && *value <= u16::MAX as u64)
        else {
            return result_error("INVALID_PARAMS", "valid rows are required");
        };
        let timeout_ms = request
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .unwrap_or(10_000)
            .min(30_000);
        let signal = {
            let registry = self.registry.lock().unwrap();
            let Some(state) = registry.get(&key) else {
                return result_error("NOT_FOUND", "no live terminal-state mirror for this key");
            };
            state.size_signal.clone()
        };
        let wanted = (cols as u16, rows as u16);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let (size, ready) = &*signal;
        let mut size = size.lock().unwrap();
        while *size != wanted {
            let now = Instant::now();
            if now >= deadline {
                return result_error(
                    "TIMEOUT",
                    "terminal size was not observed before the deadline",
                );
            }
            let (next, timeout) = ready.wait_timeout(size, deadline - now).unwrap();
            size = next;
            if timeout.timed_out() && *size != wanted {
                return result_error(
                    "TIMEOUT",
                    "terminal size was not observed before the deadline",
                );
            }
        }
        result_ok(json!({ "cols": size.0, "rows": size.1 }))
    }

    fn frame(&self, request: &Value) -> Value {
        frame_request(&self.registry, request)
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
                "frame": FrameReply::full(&checkpoint.frame, checkpoint.sequence),
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
        let mirror_output_sequence = state.mirror_output_sequence.clone();
        let mirror = state.mirror.lock().unwrap();
        let sequence = mirror_output_sequence.load(Ordering::Acquire);
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
            "terminal.waitSize" => self.wait_size(request),
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
                    eprintln!("{}: service connection failed: {error}", runtime.sidecar_id);
                }
            });
        }
    }
}

fn create_session_mirror(
    factory: MirrorFactory,
    checkpoints: Option<&CheckpointStore>,
    key: &PaneKey,
    cols: u16,
    rows: u16,
    restore_archive: bool,
) -> SharedMirror {
    let mut mirror = factory(cols, rows);
    if restore_archive {
        let window = key.0.as_deref().unwrap_or("__no-window__");
        if let Some(checkpoint) = checkpoints
            .and_then(|store| store.read(window, &key.1).ok())
            .flatten()
        {
            mirror.feed(&checkpoint.paint);
            mirror.feed(&b"\r\n".repeat(rows as usize));
        }
    }
    Arc::new(Mutex::new(mirror))
}

fn frame_timeout_ms(request: &Value) -> u64 {
    request
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .unwrap_or(FRAME_TIMEOUT_DEFAULT_MS)
        .min(FRAME_TIMEOUT_MAX_MS)
}

/// `^[a-z0-9._#-]{1,64}$`
fn valid_subscriber(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'#' | b'-')
        })
}

fn wait_for_sequence(signal: &(Mutex<u64>, Condvar), wanted: u64, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let (sequence, ready) = signal;
    let mut sequence = sequence.lock().unwrap();
    while *sequence < wanted {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let (next, _) = ready.wait_timeout(sequence, deadline - now).unwrap();
        sequence = next;
    }
    true
}

fn frame_request(registry: &Registry, request: &Value) -> Value {
    let Some(key) = pane_key(request) else {
        return result_error("INVALID_PARAMS", "window and pane are required");
    };
    let Some(subscriber) = request
        .get("subscriber")
        .and_then(Value::as_str)
        .filter(|value| valid_subscriber(value))
    else {
        return result_error(
            "INVALID_PARAMS",
            "subscriber is required and must match ^[a-z0-9._#-]{1,64}$",
        );
    };
    let offset = request.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let after = request.get("afterSequence").and_then(Value::as_u64);
    let timeout = Duration::from_millis(frame_timeout_ms(request));
    let (mirror, progress, signal, subscribers) = {
        let registry = registry.lock().unwrap();
        let Some(state) = registry.get(&key) else {
            return result_error("NOT_FOUND", "no live terminal-state mirror for this key");
        };
        (
            state.mirror.clone(),
            state.mirror_output_sequence.clone(),
            state.frame_signal.clone(),
            state.frame_subscribers.clone(),
        )
    };
    if let Some(after) = after
        && !wait_for_sequence(&signal, after, timeout)
    {
        return result_error(
            "TIMEOUT",
            "terminal output sequence was not applied before the deadline",
        );
    }
    let (frame, output_sequence) = {
        let mirror = mirror.lock().unwrap();
        let offset = if mirror.alt_active() {
            0
        } else {
            usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(mirror.history_size())
        };
        (mirror.frame_at(offset), progress.load(Ordering::Acquire))
    };
    let mut subscribers = subscribers.lock().unwrap();
    let (reply, baseline) = delta(subscribers.baseline(subscriber), &frame, output_sequence);
    subscribers.record(subscriber, baseline);
    match serde_json::to_value(reply) {
        Ok(value) => result_ok(value),
        Err(error) => result_error("FRAME_FAILED", &error.to_string()),
    }
}

fn remove_session_if_owner(registry: &Registry, key: &PaneKey, mirror: &SharedMirror) -> bool {
    let mut sessions = registry.lock().unwrap();
    let owned = sessions
        .get(key)
        .is_some_and(|state| Arc::ptr_eq(&state.mirror, mirror));
    if owned {
        sessions.remove(key);
    }
    owned
}

fn consume_observations(
    mut observations: ObservationStream,
    mirror: SharedMirror,
    registry: Registry,
    key: PaneKey,
    checkpoint: Option<Sender<CheckpointEvent>>,
) {
    while let Ok(Some(frame)) = observations.next_frame() {
        if !apply_observation(frame, &mirror, &registry, &key, checkpoint.as_ref()) {
            break;
        }
    }
    remove_session_if_owner(&registry, &key, &mirror);
}

fn apply_observation(
    frame: ObservationFrame,
    mirror: &SharedMirror,
    registry: &Registry,
    key: &PaneKey,
    checkpoint: Option<&Sender<CheckpointEvent>>,
) -> bool {
    match frame {
        ObservationFrame::Output {
            event_sequence,
            through_sequence,
            bytes,
            ..
        } => {
            let mirror_output_sequence = {
                let states = registry.lock().unwrap();
                let Some(state) = states.get(key) else {
                    return false;
                };
                state.mirror_output_sequence.clone()
            };
            let (alt_active, suppressed_replies) = {
                let mut mirror = mirror.lock().unwrap();
                mirror.feed(&bytes);
                mirror_output_sequence.store(through_sequence, Ordering::Release);
                (mirror.alt_active(), mirror.suppressed_replies())
            };
            let mut states = registry.lock().unwrap();
            let Some(state) = states.get_mut(key) else {
                return false;
            };
            state.source_event_sequence = event_sequence;
            state.source_output_sequence = through_sequence;
            state.alt_active = alt_active;
            state.suppressed_replies = suppressed_replies;
            let (sequence, ready) = &*state.frame_signal;
            *sequence.lock().unwrap() = through_sequence;
            ready.notify_all();
            if let Some(checkpoint) = checkpoint {
                let _ = checkpoint.send(CheckpointEvent::Dirty {
                    generation: state.generation,
                    sequence: through_sequence,
                });
            }
            true
        }
        ObservationFrame::Resize {
            event_sequence,
            cols,
            rows,
        } => {
            mirror.lock().unwrap().resize(cols, rows);
            let mut states = registry.lock().unwrap();
            let Some(state) = states.get_mut(key) else {
                return false;
            };
            state.source_event_sequence = event_sequence;
            let (size, ready) = &*state.size_signal;
            *size.lock().unwrap() = (cols, rows);
            ready.notify_all();
            true
        }
        ObservationFrame::Gap {
            through_event_sequence,
            through_sequence,
            ..
        } => {
            let mut states = registry.lock().unwrap();
            let Some(state) = states.get_mut(key) else {
                return false;
            };
            state.gaps += 1;
            state.source_event_sequence = through_event_sequence;
            state.source_output_sequence = through_sequence;
            let (sequence, ready) = &*state.frame_signal;
            *sequence.lock().unwrap() = through_sequence;
            ready.notify_all();
            true
        }
        ObservationFrame::End { .. } => {
            let states = registry.lock().unwrap();
            let Some(state) = states.get(key) else {
                return false;
            };
            if let Some(checkpoint) = checkpoint {
                let _ = checkpoint.send(CheckpointEvent::Final {
                    generation: state.generation,
                    sequence: state.source_output_sequence,
                });
            }
            false
        }
        ObservationFrame::Opened { .. } => {
            if let Some(state) = registry.lock().unwrap().get_mut(key) {
                state.gaps += 1;
            }
            false
        }
    }
}

enum CheckpointEvent {
    Dirty { generation: u64, sequence: u64 },
    Final { generation: u64, sequence: u64 },
}

fn start_checkpoint_worker(
    store: Arc<CheckpointStore>,
    sidecar_id: &'static str,
    key: PaneKey,
    mirror: SharedMirror,
) -> Sender<CheckpointEvent> {
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("{sidecar_id}-checkpoint"))
        .spawn(move || checkpoint_worker(store, key, mirror, receive))
        .expect("checkpoint worker thread");
    send
}

fn checkpoint_worker(
    store: Arc<CheckpointStore>,
    key: PaneKey,
    mirror: SharedMirror,
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

fn write_response<W: Write>(writer: &Arc<Mutex<W>>, response: &Value) -> io::Result<()> {
    let mut writer = writer
        .lock()
        .map_err(|_| io::Error::other("terminal response writer lock is poisoned"))?;
    writeln!(writer, "{response}")
}

fn dispatch_response<W, F>(writer: Arc<Mutex<W>>, response: F)
where
    W: Write + Send + 'static,
    F: FnOnce() -> Value + Send + 'static,
{
    std::thread::spawn(move || {
        let response = response();
        if let Err(error) = write_response(&writer, &response) {
            eprintln!("terminal response failed: {error}");
        }
    });
}

fn command_response(runtime: &Runtime, id: &str, command: &str, request: &Value) -> Value {
    let result = runtime.command(command, request);
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
}

fn serve_connection(connection: Stream, runtime: &Runtime, token: &str) -> io::Result<()> {
    let (recv, send) = connection.split();
    let send = Arc::new(Mutex::new(send));
    let reader = BufReader::new(recv);
    let mut greeted = false;
    for line in reader.lines() {
        let line = line?;
        let request: Value = serde_json::from_str(&line)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let id = request
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let command = request
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let response = if command == "system.hello" {
            let protocol = request.pointer("/args/protocol").and_then(Value::as_u64);
            let received_token = request.pointer("/args/token").and_then(Value::as_str);
            if protocol != Some(proto::CONTROL_PROTOCOL as u64) || received_token != Some(token) {
                envelope_error(&id, "GREETING", "invalid protocol or token")
            } else {
                greeted = true;
                envelope_ok(
                    &id,
                    json!({
                        "protocol": proto::CONTROL_PROTOCOL,
                        "identity": runtime.sidecar_id,
                        "commands": { "commands": [
                            { "name": "terminal.prepareSession", "owner": "plugin" },
                            { "name": "terminal.ensureSession", "owner": "plugin" },
                            { "name": "terminal.rehydrate", "owner": "plugin" },
                            { "name": "terminal.resize", "owner": "plugin" },
                            { "name": "terminal.waitSize", "owner": "plugin" },
                            { "name": "terminal.status", "owner": "plugin" },
                            { "name": "terminal.archived", "owner": "plugin" },
                            { "name": "terminal.archive", "owner": "plugin" },
                            { "name": "terminal.retire", "owner": "plugin" },
                            { "name": "terminal.frame", "owner": "plugin" }
                        ], "unserved": [] },
                        "language": "en", "languages": ["en"]
                    }),
                )
            }
        } else if !greeted {
            envelope_error(&id, "GREETING", "system.hello is required")
        } else {
            let request = request
                .pointer("/args/request")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let runtime = runtime.clone();
            let writer = send.clone();
            dispatch_response(writer, move || command_response(&runtime, &id, &command, &request));
            continue;
        };
        write_response(&send, &response)?;
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

pub fn bind_service(home: &Path, sidecar_id: &str) -> io::Result<Listener> {
    std::fs::create_dir_all(home)?;
    let path = proto::service_socket_path(home, sidecar_id);
    reclaim_stale_service_socket(Path::new(&path))?;
    let name = crate::transport_name::local_name(&path)?;
    ListenerOptions::new().name(name).create_sync()
}

#[cfg(unix)]
fn reclaim_stale_service_socket(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("service endpoint is not a socket: {}", path.display()),
        ));
    }
    let address = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "service socket path is not UTF-8",
        )
    })?;
    let name = crate::transport_name::local_name(address)?;
    match Stream::connect(name) {
        Ok(connection) => {
            drop(connection);
            Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("service endpoint is live: {}", path.display()),
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn reclaim_stale_service_socket(_path: &Path) -> io::Result<()> {
    Ok(())
}

pub fn service_token_path(runtime_root: &Path, sidecar_id: &str) -> PathBuf {
    runtime_root.join(format!("{sidecar_id}-p1.token"))
}

pub fn run_service(
    sidecar_id: &'static str,
    factory: MirrorFactory,
    arguments: impl IntoIterator<Item = String>,
) -> io::Result<()> {
    let (home, runtime_root) = parse_roots(arguments)?;
    let runtime = Runtime::connect(home, runtime_root.clone(), sidecar_id, factory)?;
    let listener = bind_service(&runtime_root, sidecar_id)?;
    let token = load_or_create_token(&runtime_root, sidecar_id)?;
    println!(
        "{}",
        json!({
            "protocol": proto::CONTROL_PROTOCOL,
            "socket": proto::service_socket_path(&runtime_root, sidecar_id),
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

fn load_or_create_token(home: &Path, sidecar_id: &str) -> io::Result<String> {
    let path = service_token_path(home, sidecar_id);
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
    pub fn connect(home: &Path, sidecar_id: &str, token: &str) -> io::Result<Self> {
        let path = proto::service_socket_path(home, sidecar_id);
        let name = crate::transport_name::local_name(&path)?;
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
    use crate::mirror::{FrameLine, FrameRun, TerminalFrame, TerminalModes};
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(prefix: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("{prefix}{:x}{:x}", std::process::id(), nonce));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(unix)]
    fn socket_test_root() -> PathBuf {
        test_root("sk")
    }

    #[cfg(unix)]
    #[test]
    fn service_bind_reclaims_a_socket_left_by_a_gone_listener() {
        let root = socket_test_root();
        let path = proto::service_socket_path(&root, "terminal-state");
        let first = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(first);
        assert!(Path::new(&path).exists());
        let second = bind_service(&root, "terminal-state").unwrap();
        drop(second);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn service_bind_never_unlinks_a_live_listener() {
        let root = socket_test_root();
        let first = bind_service(&root, "terminal-state").unwrap();
        let error = bind_service(&root, "terminal-state").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        drop(first);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn empty_frame(offset: usize, history: usize, alt: bool) -> TerminalFrame {
        TerminalFrame {
            cols: 4,
            rows: 1,
            cursor: (0, 0),
            cursor_visible: true,
            alt_active: alt,
            history_size: history,
            offset,
            modes: TerminalModes::default(),
            lines: vec![FrameLine {
                y: 0,
                wrapped: false,
                runs: vec![FrameRun {
                    text: "row".into(),
                    fg: "default".into(),
                    bg: "default".into(),
                    attrs: 0,
                    n: 3,
                    wide: false,
                    link: None,
                }],
            }],
        }
    }

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
        fn rehydrate(&self) -> Vec<u8> {
            vec![]
        }
        fn cold_paint(&self) -> Vec<u8> {
            vec![]
        }
        fn frame_at(&self, offset: usize) -> TerminalFrame {
            empty_frame(offset, 0, false)
        }
        fn history_size(&self) -> usize {
            0
        }
        fn modes(&self) -> TerminalModes {
            TerminalModes::default()
        }
        fn capabilities(&self) -> MirrorCapabilities {
            MirrorCapabilities::default()
        }
        fn alt_active(&self) -> bool {
            false
        }
        fn suppressed_replies(&self) -> u64 {
            0
        }
    }

    /// A mirror with a fixed screen; echoes the offset it is asked for so clamping is observable.
    struct ScriptedMirror {
        history: usize,
        alt: bool,
    }

    struct PaintMirror {
        paint: Vec<u8>,
    }

    impl TerminalStateMirror for PaintMirror {
        fn feed(&mut self, bytes: &[u8]) {
            self.paint.extend_from_slice(bytes);
        }
        fn resize(&mut self, _cols: u16, _rows: u16) {}
        fn rehydrate(&self) -> Vec<u8> {
            self.paint.clone()
        }
        fn cold_paint(&self) -> Vec<u8> {
            self.paint.clone()
        }
        fn frame_at(&self, offset: usize) -> TerminalFrame {
            empty_frame(offset, 0, false)
        }
        fn history_size(&self) -> usize {
            0
        }
        fn modes(&self) -> TerminalModes {
            TerminalModes::default()
        }
        fn capabilities(&self) -> MirrorCapabilities {
            MirrorCapabilities::default()
        }
        fn alt_active(&self) -> bool {
            false
        }
        fn suppressed_replies(&self) -> u64 {
            0
        }
    }

    fn paint_mirror(_cols: u16, _rows: u16) -> Box<dyn TerminalStateMirror> {
        Box::new(PaintMirror { paint: vec![] })
    }

    impl TerminalStateMirror for ScriptedMirror {
        fn feed(&mut self, _bytes: &[u8]) {}
        fn resize(&mut self, _cols: u16, _rows: u16) {}
        fn rehydrate(&self) -> Vec<u8> {
            vec![]
        }
        fn cold_paint(&self) -> Vec<u8> {
            vec![]
        }
        fn frame_at(&self, offset: usize) -> TerminalFrame {
            empty_frame(offset, self.history, self.alt)
        }
        fn history_size(&self) -> usize {
            self.history
        }
        fn modes(&self) -> TerminalModes {
            TerminalModes::default()
        }
        fn capabilities(&self) -> MirrorCapabilities {
            MirrorCapabilities::default()
        }
        fn alt_active(&self) -> bool {
            self.alt
        }
        fn suppressed_replies(&self) -> u64 {
            0
        }
    }

    fn test_registry(
        mirror: &SharedMirror,
        progress: Arc<AtomicU64>,
        size: (u16, u16),
    ) -> (PaneKey, Registry) {
        let key = (Some("window".to_string()), "pane".to_string());
        let start = progress.load(Ordering::Acquire);
        let registry: Registry = Arc::new(Mutex::new(HashMap::from([(
            key.clone(),
            SessionState {
                session: 1,
                generation: 1,
                window: key.0.clone(),
                pane: key.1.clone(),
                mirror: mirror.clone(),
                mirror_output_sequence: progress,
                source_event_sequence: 0,
                source_output_sequence: 0,
                gaps: 0,
                alt_active: false,
                suppressed_replies: 0,
                frame_signal: Arc::new((Mutex::new(start), Condvar::new())),
                size_signal: Arc::new((Mutex::new(size), Condvar::new())),
                frame_subscribers: Arc::new(Mutex::new(FrameSubscribers::default())),
            },
        )])));
        (key, registry)
    }

    fn scripted(history: usize, alt: bool) -> SharedMirror {
        Arc::new(Mutex::new(Box::new(ScriptedMirror { history, alt })))
    }

    #[test]
    fn a_new_session_replays_the_archive_before_live_output() {
        let home = test_root("checkpoint-archive-");
        let store = CheckpointStore::new(&home, "soksak-sidecar-terminal-test", [7; 32]).unwrap();
        store
            .write(
                "window",
                "pane",
                4,
                20,
                b"archived-screen\n",
                &empty_frame(0, 0, false),
            )
            .unwrap();
        let key = (Some("window".to_string()), "pane".to_string());

        let restored = create_session_mirror(paint_mirror, Some(&store), &key, 80, 24, true);
        restored.lock().unwrap().feed(b"fresh-shell");
        let mut expected = b"archived-screen\n".to_vec();
        expected.extend_from_slice(&b"\r\n".repeat(24));
        expected.extend_from_slice(b"fresh-shell");
        assert_eq!(
            restored.lock().unwrap().rehydrate(),
            expected,
        );

        let warm = create_session_mirror(paint_mirror, Some(&store), &key, 80, 24, false);
        assert!(warm.lock().unwrap().rehydrate().is_empty());
        std::fs::remove_dir_all(home).unwrap();
    }

    fn frame_of(registry: &Registry, subscriber: &str, extra: Value) -> Value {
        let mut request = json!({ "window": "window", "pane": "pane", "subscriber": subscriber });
        if let (Some(target), Some(source)) = (request.as_object_mut(), extra.as_object()) {
            for (key, value) in source {
                target.insert(key.clone(), value.clone());
            }
        }
        frame_request(registry, &request)
    }

    #[test]
    fn ended_consumer_cannot_remove_a_replacement_mirror() {
        let old = scripted(0, false);
        let replacement = scripted(0, false);
        let (key, registry) = test_registry(&old, Arc::new(AtomicU64::new(0)), (1, 1));
        registry.lock().unwrap().get_mut(&key).unwrap().mirror = replacement.clone();

        assert!(!remove_session_if_owner(&registry, &key, &old));
        assert!(Arc::ptr_eq(
            &registry.lock().unwrap().get(&key).unwrap().mirror,
            &replacement,
        ));
        assert!(remove_session_if_owner(&registry, &key, &replacement));
        assert!(!registry.lock().unwrap().contains_key(&key));
    }

    #[test]
    fn response_dispatch_does_not_serialize_blocked_commands() {
        let writer = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (first_started_send, first_started_receive) = mpsc::channel();
        let (release_first_send, release_first_receive) = mpsc::channel();
        let (second_started_send, second_started_receive) = mpsc::channel();
        let (done_send, done_receive) = mpsc::channel();

        let first_done = done_send.clone();
        dispatch_response(writer.clone(), move || {
            first_started_send.send(()).unwrap();
            release_first_receive.recv().unwrap();
            first_done.send(()).unwrap();
            json!({ "id": "first" })
        });
        first_started_receive.recv().unwrap();
        dispatch_response(writer, move || {
            second_started_send.send(()).unwrap();
            done_send.send(()).unwrap();
            json!({ "id": "second" })
        });

        second_started_receive
            .recv_timeout(Duration::from_millis(100))
            .expect("a blocked request serialized the next command");
        release_first_send.send(()).unwrap();
        done_receive.recv_timeout(Duration::from_secs(1)).unwrap();
        done_receive.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn sidecar_endpoints_are_independent() {
        let home = Path::new("/identity");
        assert_ne!(
            proto::service_socket_path(home, "soksak-sidecar-terminal-vt100"),
            proto::service_socket_path(home, "soksak-sidecar-terminal-ghostty")
        );
    }

    #[test]
    fn service_startup_does_not_subscribe_unrequested_sessions() {
        let source = include_str!("runtime.rs");
        let run_service = source.split("pub fn run_service").nth(1).unwrap();
        let body = run_service.split("fn parse_roots").next().unwrap();
        assert!(!body.contains("subscribe_existing"));
    }

    #[test]
    fn slow_engine_feed_does_not_hold_the_session_registry() {
        let (entered_send, entered_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        let mirror: SharedMirror = Arc::new(Mutex::new(Box::new(BlockingMirror {
            entered: entered_send,
            release: release_receive,
        })));
        let (key, registry) = test_registry(&mirror, Arc::new(AtomicU64::new(0)), (1, 1));
        let applying = {
            let registry = registry.clone();
            let mirror = mirror.clone();
            let key = key.clone();
            std::thread::spawn(move || {
                apply_observation(
                    ObservationFrame::Output {
                        event_sequence: 1,
                        from_sequence: 0,
                        through_sequence: 1,
                        bytes: vec![b'x'],
                    },
                    &mirror,
                    &registry,
                    &key,
                    None,
                )
            })
        };
        entered_receive.recv().unwrap();
        assert!(
            registry.try_lock().is_ok(),
            "engine feed held the session registry"
        );
        let (status_send, status_receive) = mpsc::channel();
        let status_registry = registry.clone();
        std::thread::spawn(move || {
            status_send
                .send(status_snapshot(
                    &status_registry,
                    MirrorCapabilities::default(),
                ))
                .unwrap()
        });
        assert!(
            status_receive
                .recv_timeout(Duration::from_millis(100))
                .is_ok(),
            "status waited for a busy engine mirror"
        );
        release_send.send(()).unwrap();
        assert!(applying.join().unwrap());
    }

    #[test]
    fn mirror_snapshot_and_output_sequence_advance_atomically() {
        let (entered_send, entered_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        let mirror: SharedMirror = Arc::new(Mutex::new(Box::new(BlockingMirror {
            entered: entered_send,
            release: release_receive,
        })));
        let progress = Arc::new(AtomicU64::new(0));
        let (key, registry) = test_registry(&mirror, progress.clone(), (1, 1));
        let applying = {
            let mirror = mirror.clone();
            let registry = registry.clone();
            let key = key.clone();
            std::thread::spawn(move || {
                apply_observation(
                    ObservationFrame::Output {
                        event_sequence: 1,
                        from_sequence: 0,
                        through_sequence: 37,
                        bytes: vec![b'x'],
                    },
                    &mirror,
                    &registry,
                    &key,
                    None,
                )
            })
        };
        entered_receive.recv().unwrap();
        let snapshot = {
            let mirror = mirror.clone();
            let progress = progress.clone();
            std::thread::spawn(move || {
                let mirror = mirror.lock().unwrap();
                let paint = mirror.rehydrate();
                let sequence = progress.load(Ordering::Acquire);
                (paint, sequence)
            })
        };
        release_send.send(()).unwrap();
        assert!(applying.join().unwrap());
        assert_eq!(snapshot.join().unwrap().1, 37);
    }

    #[test]
    fn resize_observation_releases_exact_size_waiters() {
        let (entered_send, _entered_receive) = mpsc::channel();
        let (_release_send, release_receive) = mpsc::channel();
        let mirror: SharedMirror = Arc::new(Mutex::new(Box::new(BlockingMirror {
            entered: entered_send,
            release: release_receive,
        })));
        let (key, registry) = test_registry(&mirror, Arc::new(AtomicU64::new(0)), (90, 24));
        let waiting = {
            let signal = registry
                .lock()
                .unwrap()
                .get(&key)
                .unwrap()
                .size_signal
                .clone();
            std::thread::spawn(move || {
                let (size, ready) = &*signal;
                let mut size = size.lock().unwrap();
                while *size != (54, 24) {
                    size = ready.wait(size).unwrap();
                }
                *size
            })
        };
        assert!(apply_observation(
            ObservationFrame::Resize {
                event_sequence: 1,
                cols: 54,
                rows: 24
            },
            &mirror,
            &registry,
            &key,
            None,
        ));
        let status = status_snapshot(&registry, MirrorCapabilities::default());
        let session = &status["sessions"][0];
        assert_eq!(session["cols"], 54);
        assert_eq!(session["rows"], 24);
        assert_eq!(session["eventSequence"], 1);
        assert_eq!(waiting.join().unwrap(), (54, 24));
    }

    #[test]
    fn frame_response_carries_the_exact_applied_output_sequence() {
        let mirror = scripted(0, false);
        let (_, registry) = test_registry(&mirror, Arc::new(AtomicU64::new(37)), (4, 1));
        let response = frame_of(&registry, "viewer", json!({}));
        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["data"]["outputSequence"], 37);
        assert_eq!(response["data"]["full"], true);
        assert_eq!(response["data"]["lines"][0]["runs"][0]["text"], "row");
        assert!(response["data"].get("output_sequence").is_none());
    }

    #[test]
    fn frame_requires_a_subscriber_and_clamps_timeout() {
        let mirror = scripted(0, false);
        let (_, registry) = test_registry(&mirror, Arc::new(AtomicU64::new(0)), (4, 1));
        let missing = frame_request(&registry, &json!({ "window": "window", "pane": "pane" }));
        assert_eq!(missing["code"], "INVALID_PARAMS");
        for bad in ["Bad Name", "", "a/b", &"x".repeat(65)] {
            let refused = frame_of(&registry, bad, json!({}));
            assert_eq!(refused["code"], "INVALID_PARAMS", "{bad:?}");
        }
        let accepted = frame_of(&registry, "viewer.1#a-b_c", json!({}));
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert_eq!(frame_timeout_ms(&json!({})), 10_000);
        assert_eq!(frame_timeout_ms(&json!({ "timeoutMs": 99_999 })), 30_000);
        assert_eq!(frame_timeout_ms(&json!({ "timeoutMs": 0 })), 0);
        assert_eq!(frame_timeout_ms(&json!({ "timeoutMs": 250 })), 250);
    }

    #[test]
    #[allow(non_snake_case)]
    fn frame_times_out_with_TIMEOUT_when_after_sequence_is_not_reached() {
        let mirror = scripted(0, false);
        let (_, registry) = test_registry(&mirror, Arc::new(AtomicU64::new(0)), (4, 1));
        let started = Instant::now();
        let response = frame_of(
            &registry,
            "viewer",
            json!({ "afterSequence": 5, "timeoutMs": 20 }),
        );
        assert_eq!(response["ok"], false);
        assert_eq!(response["code"], "TIMEOUT");
        assert!(started.elapsed() >= Duration::from_millis(20));
        let reached = frame_of(&registry, "viewer", json!({ "afterSequence": 0 }));
        assert_eq!(reached["ok"], true, "{reached}");
    }

    #[test]
    fn frame_offset_is_clamped_to_history_and_zero_in_alt() {
        let primary = scripted(5, false);
        let (_, registry) = test_registry(&primary, Arc::new(AtomicU64::new(0)), (4, 1));
        assert_eq!(
            frame_of(&registry, "v", json!({ "offset": 99 }))["data"]["offset"],
            5
        );
        assert_eq!(
            frame_of(&registry, "v", json!({ "offset": 3 }))["data"]["offset"],
            3
        );
        assert_eq!(frame_of(&registry, "v", json!({}))["data"]["offset"], 0);
        assert_eq!(
            frame_of(&registry, "v", json!({}))["data"]["historySize"],
            5
        );

        let alt = scripted(5, true);
        let (_, registry) = test_registry(&alt, Arc::new(AtomicU64::new(0)), (4, 1));
        let reply = frame_of(&registry, "v", json!({ "offset": 3 }));
        assert_eq!(reply["data"]["offset"], 0);
        assert_eq!(reply["data"]["altActive"], true);
    }

    #[test]
    fn frame_baselines_are_evicted_beyond_eight_subscribers() {
        let mirror = scripted(0, false);
        let (_, registry) = test_registry(&mirror, Arc::new(AtomicU64::new(0)), (4, 1));
        for index in 0..9 {
            let first = frame_of(&registry, &format!("s{index}"), json!({}));
            assert_eq!(
                first["data"]["full"], true,
                "s{index} first request is full"
            );
        }
        assert_eq!(frame_of(&registry, "s8", json!({}))["data"]["full"], false);
        let evicted = frame_of(&registry, "s0", json!({}));
        assert_eq!(evicted["data"]["full"], true, "s0 lost its baseline");
        assert_eq!(frame_of(&registry, "s2", json!({}))["data"]["full"], false);
        let unchanged = frame_of(&registry, "s2", json!({}));
        assert!(unchanged["data"]["lines"].as_array().unwrap().is_empty());
    }

    #[test]
    fn status_reports_capabilities() {
        let registry: Registry = Arc::new(Mutex::new(HashMap::new()));
        let status = status_snapshot(&registry, MirrorCapabilities { hyperlinks: true });
        assert_eq!(status["capabilities"]["hyperlinks"], true);
        assert_eq!(status["sessions"], json!([]));
        let none = status_snapshot(&registry, MirrorCapabilities::default());
        assert_eq!(none["capabilities"]["hyperlinks"], false);
    }
}
