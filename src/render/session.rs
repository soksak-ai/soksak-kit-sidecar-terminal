//! The render sessions: one thread per pane paints its mirror into the ring
//! and signals frames over the mach channel; the runtime's `surface.*`
//! commands steer it. Nothing here writes to the pty — input belongs to the
//! application, rendering belongs here (P3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use soksak_contract_surface::{DamageRect, Message};

use super::channel::SurfaceChannel;
use super::instances::{pack_bgra, Palette};
use super::native::Canvas;
use super::painter::{Painter, Preedit};
use super::surface_ring::SurfaceRing;
use crate::mirror::{TerminalCursorAnimation, TerminalCursorShape, TerminalCursorStyle};
use crate::TerminalStateMirror;

pub type SharedMirror = Arc<Mutex<Box<dyn TerminalStateMirror>>>;
pub type FrameSignal = Arc<(Mutex<u64>, Condvar)>;

/// A refusal, named. The runtime wraps it in its response grammar.
#[derive(Debug)]
pub struct SurfaceError {
    pub code: &'static str,
    pub message: String,
}

fn refuse(code: &'static str, message: impl Into<String>) -> SurfaceError {
    SurfaceError { code, message: message.into() }
}

/// What one pane's render thread is steered by.
struct PaneControl {
    pane: String,
    stop: AtomicBool,
    paused: AtomicBool,
    dirty: AtomicBool,
    overlay: Mutex<OverlayState>,
    returns: Mutex<Vec<u8>>,
    signal: FrameSignal,
    pending: Mutex<Option<(Painter, SurfaceRing, f64)>>,
    paints: AtomicU64,
    sends: AtomicU64,
    acquire_misses: AtomicU64,
    last_error: Mutex<Option<String>>,
    cursor_row: AtomicU16,
    cursor_col: AtomicU16,
    cursor_visible: AtomicBool,
    cursor_shape: AtomicU8,
    cursor_blinking: AtomicBool,
    cursor_interval_ms: AtomicU64,
    cursor_phase: AtomicU8,
}

#[derive(Default)]
struct OverlayState {
    offset: usize,
    preedit: Option<(String, usize)>,
}

struct FontChoice {
    family: String,
    pt: f64,
    palette: Palette,
}

struct CursorAnimation {
    active: bool,
    on: bool,
    interval: Duration,
}

impl CursorAnimation {
    fn new() -> Self {
        Self { active: false, on: true, interval: Duration::ZERO }
    }

    fn observe(
        &mut self,
        style: TerminalCursorStyle,
        policy: TerminalCursorAnimation,
        visible: bool,
        activity: bool,
    ) {
        let active = visible && style.blinking && policy.interval_ms > 0;
        if activity || active != self.active {
            self.on = true;
        }
        self.active = active;
        self.interval = Duration::from_millis(policy.interval_ms as u64);
    }

    fn next_tick(&self) -> Option<Duration> {
        self.active.then_some(self.interval)
    }

    fn cursor_on(&self) -> bool {
        !self.active || self.on
    }

    fn phase(&self) -> u8 {
        if !self.active { 0 } else if self.on { 1 } else { 2 }
    }

    fn tick(&mut self) -> bool {
        if !self.active {
            return false;
        }
        self.on = !self.on;
        true
    }
}

pub struct SurfaceSessions {
    canvas: Mutex<Option<Arc<Canvas>>>,
    channel: Mutex<Option<Arc<SurfaceChannel>>>,
    identifier: Mutex<Option<String>>,
    panes: Arc<Mutex<HashMap<String, Arc<PaneControl>>>>,
    fonts: Mutex<HashMap<String, FontChoice>>,
}

impl Default for SurfaceSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl SurfaceSessions {
    pub fn new() -> Self {
        Self {
            canvas: Mutex::new(None),
            channel: Mutex::new(None),
            identifier: Mutex::new(None),
            panes: Arc::new(Mutex::new(HashMap::new())),
            fonts: Mutex::new(HashMap::new()),
        }
    }

    fn canvas(&self) -> Result<Arc<Canvas>, SurfaceError> {
        let mut held = self.canvas.lock().unwrap();
        if let Some(canvas) = held.as_ref() {
            return Ok(Arc::clone(canvas));
        }
        let canvas = Arc::new(
            Canvas::create().map_err(|error| refuse("METAL_UNAVAILABLE", error))?,
        );
        *held = Some(Arc::clone(&canvas));
        Ok(canvas)
    }

    fn channel(&self, sidecar_id: &str, identifier: &str) -> Result<Arc<SurfaceChannel>, SurfaceError> {
        {
            let known = self.identifier.lock().unwrap();
            if let Some(existing) = known.as_ref() {
                if existing != identifier {
                    return Err(refuse(
                        "IDENTIFIER_MISMATCH",
                        format!("this process serves {existing}, not {identifier}"),
                    ));
                }
            }
        }
        let mut held = self.channel.lock().unwrap();
        if let Some(channel) = held.as_ref() {
            // One Hello is both the liveness probe and the current application's
            // reply-right registration. A successful existing channel needs no
            // second Hello from surface.open.
            if channel
                .send(&Message::Hello { sidecar_id: sidecar_id.to_string() }, &[])
                .is_ok()
            {
                return Ok(Arc::clone(channel));
            }
            *held = None;
        }
        let channel = Arc::new(
            SurfaceChannel::open(identifier)
                .map_err(|error| refuse("CHANNEL_UNAVAILABLE", error))?,
        );
        channel
            .send(&Message::Hello { sidecar_id: sidecar_id.to_string() }, &[])
            .map_err(|error| refuse("CHANNEL_SEND_FAILED", error))?;
        *held = Some(Arc::clone(&channel));
        *self.identifier.lock().unwrap() = Some(identifier.to_string());
        self.spawn_reader(Arc::clone(&channel));
        Ok(channel)
    }

    /// The one reader of application-to-sidecar messages: released surfaces
    /// route to their pane and wake its thread.
    fn spawn_reader(&self, channel: Arc<SurfaceChannel>) {
        let panes = PanesHandle(Arc::clone(&self.panes));
        std::thread::spawn(move || loop {
            match channel.recv(500) {
                Ok(Some(Message::Released { pane, ring_index })) => {
                    if let Some(control) = panes.get(&pane) {
                        control.returns.lock().unwrap().push(ring_index);
                        control.wake();
                    }
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(_) => return,
            }
        });
    }

    /// Dispatch one surface.* command. `wiring` is resolved by the runtime
    /// from its registry for the commands that need a live session.
    pub fn command(
        &self,
        sidecar_id: &str,
        command: &str,
        request: &Value,
        wiring: Option<(SharedMirror, FrameSignal)>,
    ) -> Result<Value, SurfaceError> {
        match command {
            "surface.open" => self.open(sidecar_id, request, wiring),
            "surface.state" => self.state(request),
            "surface.resize" => self.resize(request),
            "surface.setPaused" => self.set_paused(request),
            "surface.preedit" => self.preedit(request),
            "surface.scroll" => self.scroll(request, wiring),
            "surface.read" => self.read(request, wiring),
            "surface.close" => self.close(request),
            "surface.selection" | "surface.hover" | "surface.theme" => {
                Err(refuse("NOT_YET_SERVED", format!("{command} arrives with the overlay pass")))
            }
            _ => Err(refuse("UNKNOWN_COMMAND", "unknown surface command")),
        }
    }

    fn pane_of(&self, request: &Value) -> Result<String, SurfaceError> {
        request
            .get("pane")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| refuse("INVALID_PARAMS", "pane is required"))
    }

    fn control_of(&self, pane: &str) -> Result<Arc<PaneControl>, SurfaceError> {
        self.panes
            .lock()
            .unwrap()
            .get(pane)
            .cloned()
            .ok_or_else(|| refuse("NOT_FOUND", format!("no surface renders {pane}")))
    }

    /// The render counters for one pane: how many paints happened, how many
    /// frame-ready signals went out, and what failed last.
    fn state(&self, request: &Value) -> Result<Value, SurfaceError> {
        let pane = self.pane_of(request)?;
        let control = self.control_of(&pane)?;
        let signal_seq = *control.signal.0.lock().unwrap();
        let cursor_shape = match control.cursor_shape.load(Ordering::Acquire) {
            1 => "underline",
            2 => "bar",
            _ => "block",
        };
        let cursor_phase = match control.cursor_phase.load(Ordering::Acquire) {
            1 => "on",
            2 => "off",
            _ => "steady",
        };
        Ok(json!({
            "pane": pane,
            "paints": control.paints.load(Ordering::Acquire),
            "sends": control.sends.load(Ordering::Acquire),
            "acquireMisses": control.acquire_misses.load(Ordering::Acquire),
            "lastError": control.last_error.lock().unwrap().clone(),
            "signalSequence": signal_seq,
            "paused": control.paused.load(Ordering::Acquire),
            "stopped": control.stop.load(Ordering::Acquire),
            "cursorRow": control.cursor_row.load(Ordering::Acquire),
            "cursorColumn": control.cursor_col.load(Ordering::Acquire),
            "cursorVisible": control.cursor_visible.load(Ordering::Acquire),
            "cursorShape": cursor_shape,
            "cursorBlinking": control.cursor_blinking.load(Ordering::Acquire),
            "cursorAnimation": {
                "intervalMs": control.cursor_interval_ms.load(Ordering::Acquire),
                "phase": cursor_phase,
            },
        }))
    }

    fn open(
        &self,
        sidecar_id: &str,
        request: &Value,
        wiring: Option<(SharedMirror, FrameSignal)>,
    ) -> Result<Value, SurfaceError> {
        let (mirror, signal) =
            wiring.ok_or_else(|| refuse("NOT_FOUND", "no live terminal-state mirror for this key"))?;
        let identifier = request
            .get("identifier")
            .and_then(Value::as_str)
            .ok_or_else(|| refuse("INVALID_PARAMS", "identifier is required"))?;
        let pane = self.pane_of(request)?;
        let pixel_w = number(request, "pixelW")?;
        let pixel_h = number(request, "pixelH")?;
        let scale = number(request, "scale")?;
        let font = request.get("font").ok_or_else(|| refuse("INVALID_PARAMS", "font is required"))?;
        let family = font
            .get("family")
            .and_then(Value::as_str)
            .ok_or_else(|| refuse("INVALID_PARAMS", "font.family is required"))?;
        let pt = number(font, "pt")?;
        let palette = parse_theme(
            request.get("theme").ok_or_else(|| refuse("INVALID_PARAMS", "theme is required"))?,
        )?;

        let canvas = self.canvas()?;
        let channel = self.channel(sidecar_id, identifier)?;
        let metrics = canvas
            .font_metrics(family, pt, scale)
            .map_err(|error| refuse("FONT_UNAVAILABLE", error))?;
        let (cols, rows) = grid_for(pixel_w, pixel_h, scale, metrics.cell_w, metrics.cell_h);
        let cell_w = metrics.cell_w.ceil() as u32;
        let cell_h = metrics.cell_h.ceil() as u32;

        let painter = Painter::new(
            Arc::clone(&canvas),
            family,
            pt,
            scale,
            cols,
            rows,
            palette.clone(),
        )
        .map_err(|error| refuse("PAINTER_UNAVAILABLE", error))?;
        let ring = SurfaceRing::new(&canvas, cols as u32 * cell_w, rows as u32 * cell_h, rows)
            .map_err(|error| refuse("RING_UNAVAILABLE", error))?;
        let ports = ring.mach_ports().map_err(|error| refuse("RING_PORTS_UNAVAILABLE", error))?;
        channel
            .send(
                &Message::Ring {
                    pane: pane.clone(),
                    pixel_w: cols as u32 * cell_w,
                    pixel_h: rows as u32 * cell_h,
                    scale,
                    cell_w: metrics.cell_w,
                    cell_h: metrics.cell_h,
                },
                &ports,
            )
            .map_err(|error| refuse("CHANNEL_SEND_FAILED", error))?;

        let control = Arc::new(PaneControl {
            pane: pane.clone(),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            dirty: AtomicBool::new(true),
            overlay: Mutex::new(OverlayState::default()),
            returns: Mutex::new(Vec::new()),
            signal,
            pending: Mutex::new(None),
            paints: AtomicU64::new(0),
            sends: AtomicU64::new(0),
            acquire_misses: AtomicU64::new(0),
            last_error: Mutex::new(None),
            cursor_row: AtomicU16::new(0),
            cursor_col: AtomicU16::new(0),
            cursor_visible: AtomicBool::new(false),
            cursor_shape: AtomicU8::new(0),
            cursor_blinking: AtomicBool::new(false),
            cursor_interval_ms: AtomicU64::new(0),
            cursor_phase: AtomicU8::new(0),
        });
        supersede(&self.panes, &pane, Arc::clone(&control));
        self.fonts
            .lock()
            .unwrap()
            .insert(pane.clone(), FontChoice { family: family.to_string(), pt, palette: palette.clone() });
        spawn_render_thread(control, mirror, painter, ring, channel);
        Ok(json!({ "cols": cols, "rows": rows, "cellW": metrics.cell_w, "cellH": metrics.cell_h }))
    }

    fn resize(&self, request: &Value) -> Result<Value, SurfaceError> {
        let pane = self.pane_of(request)?;
        let control = self.control_of(&pane)?;
        let pixel_w = number(request, "pixelW")?;
        let pixel_h = number(request, "pixelH")?;
        let scale = number(request, "scale")?;
        let fonts = self.fonts.lock().unwrap();
        let font = fonts
            .get(&pane)
            .ok_or_else(|| refuse("NOT_FOUND", format!("no surface renders {pane}")))?;
        let canvas = self.canvas()?;
        let (painter, ring, (cols, rows)) = prepare_render(
            &canvas, &font.family, font.pt, scale, pixel_w, pixel_h, font.palette.clone(),
        )
        .map_err(|error| refuse("RESIZE_UNAVAILABLE", error))?;
        drop(fonts);
        *control.pending.lock().unwrap() = Some((painter, ring, scale));
        control.dirty.store(true, Ordering::Release);
        control.wake();
        Ok(json!({ "cols": cols, "rows": rows }))
    }

    fn set_paused(&self, request: &Value) -> Result<Value, SurfaceError> {
        let pane = self.pane_of(request)?;
        let control = self.control_of(&pane)?;
        let paused = request
            .get("paused")
            .and_then(Value::as_bool)
            .ok_or_else(|| refuse("INVALID_PARAMS", "paused is required"))?;
        control.paused.store(paused, Ordering::Release);
        if !paused {
            control.dirty.store(true, Ordering::Release);
        }
        control.wake();
        Ok(json!({}))
    }

    fn preedit(&self, request: &Value) -> Result<Value, SurfaceError> {
        let pane = self.pane_of(request)?;
        let control = self.control_of(&pane)?;
        let text = request.get("text").and_then(Value::as_str).unwrap_or("");
        let caret = request.get("caret").and_then(Value::as_u64).unwrap_or(0) as usize;
        {
            let mut overlay = control.overlay.lock().unwrap();
            overlay.preedit =
                if text.is_empty() { None } else { Some((text.to_string(), caret)) };
        }
        control.dirty.store(true, Ordering::Release);
        control.wake();
        Ok(json!({}))
    }

    fn scroll(
        &self,
        request: &Value,
        wiring: Option<(SharedMirror, FrameSignal)>,
    ) -> Result<Value, SurfaceError> {
        let pane = self.pane_of(request)?;
        let control = self.control_of(&pane)?;
        let (mirror, _) =
            wiring.ok_or_else(|| refuse("NOT_FOUND", "no live terminal-state mirror for this key"))?;
        let history = { mirror.lock().unwrap().history_size() };
        let mut overlay = control.overlay.lock().unwrap();
        let current = overlay.offset as i64;
        let wanted = if let Some(offset) = request.get("offset").and_then(Value::as_i64) {
            offset
        } else if let Some(lines) = request.get("lines").and_then(Value::as_i64) {
            current - lines
        } else if let Some(edge) = request.get("edge").and_then(Value::as_str) {
            match edge {
                "top" => history as i64,
                "bottom" => 0,
                _ => return Err(refuse("INVALID_PARAMS", "edge is top or bottom")),
            }
        } else {
            return Err(refuse("INVALID_PARAMS", "offset, lines or edge is required"));
        };
        overlay.offset = wanted.clamp(0, history as i64) as usize;
        let offset = overlay.offset;
        drop(overlay);
        control.dirty.store(true, Ordering::Release);
        control.wake();
        Ok(json!({ "offset": offset, "historySize": history }))
    }

    fn read(
        &self,
        request: &Value,
        wiring: Option<(SharedMirror, FrameSignal)>,
    ) -> Result<Value, SurfaceError> {
        let pane = self.pane_of(request)?;
        let control = self.control_of(&pane)?;
        let (mirror, _) =
            wiring.ok_or_else(|| refuse("NOT_FOUND", "no live terminal-state mirror for this key"))?;
        let offset = control.overlay.lock().unwrap().offset;
        let mirror = mirror.lock().unwrap();
        let rows = mirror.rows();
        let wanted = request
            .get("lines")
            .and_then(Value::as_u64)
            .map(|lines| lines.min(rows as u64) as u16)
            .unwrap_or(rows);
        let mut text = String::new();
        for row in (rows - wanted)..rows {
            let cells = mirror.line_cells(row as i32 - offset as i32);
            let mut line = String::new();
            for cell in &cells {
                if cell.spacer {
                    continue;
                }
                line.push(cell.ch);
            }
            text.push_str(line.trim_end());
            text.push('\n');
        }
        Ok(json!({ "text": text }))
    }

    fn close(&self, request: &Value) -> Result<Value, SurfaceError> {
        let pane = self.pane_of(request)?;
        let control = self.control_of(&pane)?;
        control.stop.store(true, Ordering::Release);
        control.wake();
        self.panes.lock().unwrap().remove(&pane);
        self.fonts.lock().unwrap().remove(&pane);
        Ok(json!({}))
    }
}

impl PaneControl {
    fn wake(&self) {
        let (_, ready) = &*self.signal;
        ready.notify_all();
    }
}

/// A shareable view over the pane map for the reader thread.
struct PanesHandle(Arc<Mutex<HashMap<String, Arc<PaneControl>>>>);

impl PanesHandle {
    fn get(&self, pane: &str) -> Option<Arc<PaneControl>> {
        self.0.lock().unwrap().get(pane).cloned()
    }
}

fn number(value: &Value, key: &str) -> Result<f64, SurfaceError> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|number| *number > 0.0)
        .ok_or_else(|| refuse("INVALID_PARAMS", format!("{key} is required")))
}

fn grid_for(pixel_w: f64, pixel_h: f64, scale: f64, cell_w: f64, cell_h: f64) -> (u16, u16) {
    let device_w = pixel_w * scale;
    let device_h = pixel_h * scale;
    let cols = ((device_w / cell_w).floor() as i64).max(1) as u16;
    let rows = ((device_h / cell_h).floor() as i64).max(1) as u16;
    (cols, rows)
}

fn parse_color(value: &Value, key: &str) -> Result<u32, SurfaceError> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| refuse("INVALID_PARAMS", format!("theme.{key} is required")))?;
    parse_hex(text).ok_or_else(|| refuse("INVALID_PARAMS", format!("theme.{key} is not a color")))
}

fn parse_hex(text: &str) -> Option<u32> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(pack_bgra((value >> 16) as u8, (value >> 8) as u8, value as u8, 255))
}

fn parse_theme(theme: &Value) -> Result<Palette, SurfaceError> {
    let fg = parse_color(theme, "fg")?;
    let bg = parse_color(theme, "bg")?;
    let cursor = parse_color(theme, "cursor")?;
    let cursor_accent = parse_color(theme, "cursorAccent")?;
    let ansi_values = theme
        .get("ansi")
        .and_then(Value::as_array)
        .ok_or_else(|| refuse("INVALID_PARAMS", "theme.ansi is required"))?;
    if ansi_values.len() != 256 {
        return Err(refuse("INVALID_PARAMS", "theme.ansi must hold 256 colors"));
    }
    let mut ansi = [bg; 256];
    for (index, entry) in ansi_values.iter().enumerate() {
        let text = entry
            .as_str()
            .ok_or_else(|| refuse("INVALID_PARAMS", format!("theme.ansi[{index}] is not a color")))?;
        ansi[index] = parse_hex(text)
            .ok_or_else(|| refuse("INVALID_PARAMS", format!("theme.ansi[{index}] is not a color")))?;
    }
    Ok(Palette { fg, bg, cursor, cursor_accent, ansi })
}

/// One painter and one ring for one pixel box: the same construction whether
/// the pane opens or resizes. The caller sends the ring and repaints all.
pub fn prepare_render(
    canvas: &Arc<Canvas>,
    family: &str,
    pt: f64,
    scale: f64,
    pixel_w: f64,
    pixel_h: f64,
    palette: Palette,
) -> Result<(Painter, SurfaceRing, (u16, u16)), String> {
    let metrics = canvas.font_metrics(family, pt, scale)?;
    let (cols, rows) = grid_for(pixel_w, pixel_h, scale, metrics.cell_w, metrics.cell_h);
    let cell_w = metrics.cell_w.ceil() as u32;
    let cell_h = metrics.cell_h.ceil() as u32;
    let painter = Painter::new(Arc::clone(canvas), family, pt, scale, cols, rows, palette)?;
    let ring = SurfaceRing::new(canvas, cols as u32 * cell_w, rows as u32 * cell_h, rows)?;
    Ok((painter, ring, (cols, rows)))
}

fn spawn_render_thread(
    control: Arc<PaneControl>,
    mirror: SharedMirror,
    mut painter: Painter,
    mut ring: SurfaceRing,
    channel: Arc<SurfaceChannel>,
) {
    std::thread::spawn(move || {
        let mut last_seen: u64 = {
            let (seq, _) = &*control.signal;
            *seq.lock().unwrap()
        };
        let mut last_signaled: Option<usize> = None;
        let mut cursor_animation = CursorAnimation::new();
        loop {
            if control.stop.load(Ordering::Acquire) {
                let _ = channel.send(
                    &Message::Ended { pane: control.pane.clone(), reason: "closed".into() },
                    &[],
                );
                return;
            }
            // A pending resize replaces the paint surfaces wholesale: the new
            // ring goes to the application first, then everything repaints.
            if let Some((new_painter, new_ring, new_scale)) = control.pending.lock().unwrap().take() {
                painter = new_painter;
                ring = new_ring;
                last_signaled = None;
                let (cell_w_px, cell_h_px) = painter.cell_size();
                let (pixel_w, pixel_h) = painter.pixel_size();
                match ring.mach_ports() {
                    Ok(ports) => {
                        if let Err(error) = channel.send(
                            &Message::Ring {
                                pane: control.pane.clone(),
                                pixel_w,
                                pixel_h,
                                scale: new_scale,
                                cell_w: cell_w_px as f64,
                                cell_h: cell_h_px as f64,
                            },
                            &ports,
                        ) {
                            *control.last_error.lock().unwrap() =
                                Some(format!("resize ring send: {error}"));
                        }
                    }
                    Err(error) => {
                        *control.last_error.lock().unwrap() =
                            Some(format!("resize ring ports: {error}"));
                    }
                }
                control.dirty.store(true, Ordering::Release);
            }
            // Route returned surfaces first: a release both frees a slot and
            // tells us the previously signaled one is on screen now.
            let returned: Vec<u8> = control.returns.lock().unwrap().drain(..).collect();
            for index in returned {
                if let Some(shown) = last_signaled.take() {
                    let _ = ring.shown(shown);
                }
                let _ = ring.released(index as usize);
            }
            let progressed = {
                let (seq, _) = &*control.signal;
                *seq.lock().unwrap()
            };
            let output_activity = progressed != last_seen;
            let owes = progressed != last_seen || control.dirty.swap(false, Ordering::AcqRel);
            if owes && !control.paused.load(Ordering::Acquire) {
                last_seen = progressed;
                let (offset, preedit) = {
                    let overlay = control.overlay.lock().unwrap();
                    (overlay.offset, overlay.preedit.clone())
                };
                let preedit_value = preedit
                    .map(|(text, cursor)| Preedit { text, cursor });
                let (cursor, cursor_visible, refresh) = {
                    let mirror = mirror.lock().unwrap();
                    let cursor = mirror.cursor();
                    let visible = mirror.modes().show_cursor;
                    let style = mirror.cursor_style();
                    let policy = mirror.cursor_animation();
                    cursor_animation.observe(
                        style,
                        policy,
                        visible && offset == 0 && preedit_value.is_none(),
                        output_activity,
                    );
                    control.cursor_row.store(cursor.0 as u16, Ordering::Release);
                    control.cursor_col.store(cursor.1 as u16, Ordering::Release);
                    control.cursor_visible.store(visible, Ordering::Release);
                    control.cursor_shape.store(match style.shape {
                        TerminalCursorShape::Block => 0,
                        TerminalCursorShape::Underline => 1,
                        TerminalCursorShape::Bar => 2,
                    }, Ordering::Release);
                    control.cursor_blinking.store(style.blinking, Ordering::Release);
                    control.cursor_interval_ms.store(policy.interval_ms as u64, Ordering::Release);
                    control.cursor_phase.store(cursor_animation.phase(), Ordering::Release);
                    let refresh = painter.refresh(
                        &**mirror,
                        offset,
                        preedit_value.as_ref(),
                        cursor_animation.cursor_on(),
                    );
                    (cursor, visible, refresh)
                };
                if let Err(error) = &refresh {
                    *control.last_error.lock().unwrap() = Some(format!("refresh: {error}"));
                }
                if refresh.is_ok() {
                    if let Ok(slot) = ring.acquire() {
                        let (surface, state) = ring.target(slot);
                        if let Ok(rows) = painter.paint_into(surface, state) {
                            control.paints.fetch_add(1, Ordering::AcqRel);
                            if !rows.is_empty() || last_signaled.is_none() {
                                let (cell_w, cell_h) = painter.cell_size();
                                let (width, _) = painter.pixel_size();
                                let damage: Vec<DamageRect> = spans(&rows)
                                    .into_iter()
                                    .map(|(start, count)| {
                                        (
                                            0u16,
                                            start * cell_h,
                                            width as u16,
                                            count * cell_h,
                                        )
                                    })
                                    .collect();
                                let seq = ring.signal(slot);
                                last_signaled = Some(slot);
                                match channel.send(
                                    &Message::FrameReady {
                                        pane: control.pane.clone(),
                                        ring_index: slot as u8,
                                        seq,
                                        cursor_row: cursor.0 as u16,
                                        cursor_col: cursor.1 as u16,
                                        cursor_visible,
                                        damage,
                                    },
                                    &[],
                                ) {
                                    Ok(()) => {
                                        control.sends.fetch_add(1, Ordering::AcqRel);
                                    }
                                    Err(error) => {
                                        *control.last_error.lock().unwrap() =
                                            Some(format!("frameReady send: {error}"));
                                    }
                                }
                                let _ = cell_w;
                            } else {
                                // Nothing changed for this slot: hand it back.
                                let _ = ring.released(slot);
                            }
                        }
                    } else {
                        control.acquire_misses.fetch_add(1, Ordering::AcqRel);
                    }
                }
            }
            let (seq, ready) = &*control.signal;
            let guard = seq.lock().unwrap();
            if *guard == last_seen
                && !control.dirty.load(Ordering::Acquire)
                && !control.stop.load(Ordering::Acquire)
            {
                if !control.paused.load(Ordering::Acquire) {
                    if let Some(interval) = cursor_animation.next_tick() {
                        let (_guard, result) = ready.wait_timeout(guard, interval).unwrap();
                        if result.timed_out() && cursor_animation.tick() {
                            control.dirty.store(true, Ordering::Release);
                        }
                        continue;
                    }
                }
                let _guard = ready.wait(guard).unwrap();
            }
        }
    });
}

fn spans(rows: &[u16]) -> Vec<(u16, u16)> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        let start = rows[index];
        let mut end = start;
        while index + 1 < rows.len() && rows[index + 1] == end + 1 {
            index += 1;
            end = rows[index];
        }
        spans.push((start, end - start + 1));
        index += 1;
    }
    spans
}

/// The service is the only opener. A second open for the same pane is a newer
/// world — a dead application's render session never sends close, so the
/// replacement supersedes it and the old thread is stopped, never a refusal.
fn supersede(
    panes: &Mutex<HashMap<String, Arc<PaneControl>>>,
    pane: &str,
    control: Arc<PaneControl>,
) -> Option<Arc<PaneControl>> {
    let previous = panes.lock().unwrap().insert(pane.to_string(), control);
    if let Some(previous) = &previous {
        previous.stop.store(true, Ordering::Release);
        previous.wake();
    }
    previous
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(pane: &str) -> Arc<PaneControl> {
        Arc::new(PaneControl {
            pane: pane.to_string(),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            overlay: Mutex::new(OverlayState::default()),
            returns: Mutex::new(Vec::new()),
            signal: Arc::new((Mutex::new(0), Condvar::new())),
            pending: Mutex::new(None),
            paints: AtomicU64::new(0),
            sends: AtomicU64::new(0),
            acquire_misses: AtomicU64::new(0),
            last_error: Mutex::new(None),
            cursor_row: AtomicU16::new(0),
            cursor_col: AtomicU16::new(0),
            cursor_visible: AtomicBool::new(false),
            cursor_shape: AtomicU8::new(0),
            cursor_blinking: AtomicBool::new(false),
            cursor_interval_ms: AtomicU64::new(0),
            cursor_phase: AtomicU8::new(0),
        })
    }

    #[test]
    fn a_reopened_pane_supersedes_the_dead_render_session() {
        let panes = Mutex::new(HashMap::new());
        let first = control("tab-a.1");
        let second = control("tab-a.1");

        assert!(supersede(&panes, "tab-a.1", Arc::clone(&first)).is_none());
        let superseded = supersede(&panes, "tab-a.1", Arc::clone(&second))
            .expect("the first session is handed back");
        assert!(Arc::ptr_eq(&superseded, &first));
        assert!(first.stop.load(Ordering::Acquire), "the old thread is told to stop");
        assert!(!second.stop.load(Ordering::Acquire));
        assert!(Arc::ptr_eq(panes.lock().unwrap().get("tab-a.1").unwrap(), &second));
    }

    #[test]
    fn surface_state_exposes_the_engine_cursor_and_renderer_animation() {
        let sessions = SurfaceSessions::new();
        let cursor = control("tab-a.1");
        cursor.cursor_row.store(3, Ordering::Release);
        cursor.cursor_col.store(7, Ordering::Release);
        cursor.cursor_visible.store(true, Ordering::Release);
        cursor.cursor_shape.store(2, Ordering::Release);
        cursor.cursor_blinking.store(true, Ordering::Release);
        cursor.cursor_interval_ms.store(750, Ordering::Release);
        cursor.cursor_phase.store(1, Ordering::Release);
        sessions.panes.lock().unwrap().insert("tab-a.1".into(), cursor);
        let state = sessions.state(&json!({ "pane": "tab-a.1" })).expect("surface state");
        assert_eq!(state["cursorShape"], "bar");
        assert_eq!(state["cursorBlinking"], true);
        assert_eq!(state["cursorVisible"], true);
        assert_eq!((state["cursorRow"].as_u64(), state["cursorColumn"].as_u64()), (Some(3), Some(7)));
        assert_eq!(state["cursorAnimation"]["intervalMs"], 750);
        assert_eq!(state["cursorAnimation"]["phase"], "on");
    }

    #[test]
    fn surface_state_exposes_base_override_and_effective_theme() {
        let sessions = SurfaceSessions::new();
        sessions.panes.lock().unwrap().insert("tab-a.1".into(), control("tab-a.1"));
        let state = sessions.state(&json!({ "pane": "tab-a.1" })).expect("surface state");
        assert_eq!(state["themeMode"], "light");
        assert_eq!(state["baseTheme"]["foreground"], "#111111");
        assert_eq!(state["terminalOverrides"]["foreground"], Value::Null);
        assert_eq!(state["terminalOverrides"]["ansi"].as_array().map(Vec::len), Some(256));
        assert_eq!(state["effectiveTheme"]["foreground"], "#111111");
    }

    #[test]
    fn terminal_theme_keeps_the_declared_cursor_colors() {
        let theme = json!({
            "fg": "#112233", "bg": "#445566", "cursor": "#778899",
            "cursorAccent": "#aabbcc", "selectionBg": "#000000",
            "selectionFg": "#ffffff", "ansi": vec!["#010203"; 256],
        });
        let palette = parse_theme(&theme).expect("complete theme");
        assert_eq!(palette.cursor, pack_bgra(0x77, 0x88, 0x99, 255));
        assert_eq!(palette.cursor_accent, pack_bgra(0xaa, 0xbb, 0xcc, 255));
    }

    #[test]
    fn cursor_animation_ticks_only_for_engine_blinking_state() {
        use crate::mirror::{
            TerminalCursorAnimation, TerminalCursorShape, TerminalCursorStyle,
        };

        let steady = TerminalCursorStyle {
            shape: TerminalCursorShape::Bar,
            blinking: false,
        };
        let blinking = TerminalCursorStyle { blinking: true, ..steady };
        let policy = TerminalCursorAnimation { interval_ms: 750 };
        let mut animation = CursorAnimation::new();
        animation.observe(steady, policy, true, true);
        assert_eq!(animation.next_tick(), None);
        assert!(animation.cursor_on());

        animation.observe(blinking, policy, true, true);
        assert_eq!(animation.next_tick(), Some(Duration::from_millis(750)));
        assert!(animation.tick());
        assert!(!animation.cursor_on());
        assert!(animation.tick());
        assert!(animation.cursor_on());

        animation.observe(blinking, policy, false, false);
        assert_eq!(animation.next_tick(), None);
    }
}
