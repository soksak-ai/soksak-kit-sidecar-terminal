//! Client for the installed PTY sidecar.
//!
//! The sidecar speaks one control-envelope socket. pty.observe returns one response and then framed
//! source events on that connection. This client keeps control and observer connections separate.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use interprocess::local_socket::{RecvHalf, SendHalf, Stream, prelude::*};
use serde_json::{Value, json};

use crate::proto;

// ended reports the failures that are a connection going, not a unit refusing.
fn ended(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
    )
}

fn other(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn sidecar_name() -> io::Result<String> {
    std::env::var(soksak_contract_control::SIDECAR_NAME_ENVIRONMENT)
        .map_err(|_| other("PTY sidecar name is not declared"))
}

fn read_token(runtime_root: &Path) -> io::Result<String> {
    let path = proto::pty_token_path(runtime_root, &sidecar_name()?);
    let token = std::fs::read_to_string(&path).map_err(|error| {
        other(format!(
            "PTY token unreadable at {}: {error}",
            path.display()
        ))
    })?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(other(format!("PTY token empty at {}", path.display())));
    }
    Ok(token)
}

fn connect(runtime_root: &Path) -> io::Result<(RecvHalf, SendHalf)> {
    let name = sidecar_name()?;
    let path = proto::pty_socket_path(runtime_root, &name);
    let name = crate::transport_name::local_name(&path)?;
    Stream::connect(name)
        .map_err(|error| other(format!("cannot reach PTY socket {}: {error}", path)))
        .map(Stream::split)
}

fn write_line(writer: &mut SendHalf, value: &Value) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

fn read_line(reader: &mut BufReader<RecvHalf>) -> io::Result<Value> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "PTY sidecar closed the connection",
        ));
    }
    serde_json::from_str(line.trim())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn greet(reader: &mut BufReader<RecvHalf>, writer: &mut SendHalf, token: &str) -> io::Result<()> {
    write_line(writer, &proto::hello("greeting", token))?;
    let reply = read_line(reader)?;
    proto::response_data(&reply).map_err(other)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub session: u64,
    pub generation: u64,
    pub pane_id: String,
    pub window_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotLease {
    pub token: String,
    pub session: u64,
    pub after_sequence: u64,
    pub expires_at_ms: i64,
}

pub struct ControlClient {
    runtime_root: PathBuf,
    writer: SendHalf,
    reader: BufReader<RecvHalf>,
    request_sequence: u64,
}

impl ControlClient {
    pub fn connect(runtime_root: &Path) -> io::Result<Self> {
        let (reader, writer) = Self::greeted(runtime_root)?;
        Ok(Self {
            runtime_root: runtime_root.to_path_buf(),
            writer,
            reader,
            request_sequence: 0,
        })
    }

    fn greeted(runtime_root: &Path) -> io::Result<(BufReader<RecvHalf>, SendHalf)> {
        let token = read_token(runtime_root)?;
        let (recv, mut send) = connect(runtime_root)?;
        let mut reader = BufReader::new(recv);
        greet(&mut reader, &mut send, &token)?;
        Ok((reader, send))
    }

    // A connection the unit ended is not the unit going: the socket is where the unit is, and a
    // request that finds the connection gone reaches it by greeting a new one. Only a request that
    // fails on a connection it just opened is a unit that is not there.
    pub fn request(&mut self, command: &str, request: Value) -> io::Result<Value> {
        match self.send_once(command, request.clone()) {
            Ok(value) => Ok(value),
            Err(error) if ended(&error) => {
                let (reader, writer) = Self::greeted(&self.runtime_root)?;
                self.reader = reader;
                self.writer = writer;
                self.send_once(command, request)
            }
            Err(error) => Err(error),
        }
    }

    fn send_once(&mut self, command: &str, request: Value) -> io::Result<Value> {
        self.request_sequence += 1;
        let id = format!("terminal-state-{}", self.request_sequence);
        write_line(&mut self.writer, &proto::request(&id, command, request))?;
        let reply = read_line(&mut self.reader)?;
        proto::response_data(&reply).cloned().map_err(other)
    }

    pub fn list_sessions(&mut self) -> io::Result<Vec<SessionInfo>> {
        let data = self.request("pty.sessions", json!({}))?;
        let sessions = data
            .as_array()
            .ok_or_else(|| other("pty.sessions did not return a session array"))?;
        Ok(sessions.iter().filter_map(parse_session_info).collect())
    }

    pub fn lease(&mut self, session: u64, after_sequence: u64) -> io::Result<SnapshotLease> {
        let data = self.request(
            "pty.lease",
            json!({
                "session": session,
                "afterSequence": after_sequence,
            }),
        )?;
        Ok(SnapshotLease {
            token: data
                .get("token")
                .and_then(Value::as_str)
                .ok_or_else(|| other("pty.lease returned no token"))?
                .to_string(),
            session: data
                .get("session")
                .and_then(Value::as_u64)
                .ok_or_else(|| other("pty.lease returned no session"))?,
            after_sequence: data
                .get("afterSequence")
                .and_then(Value::as_u64)
                .ok_or_else(|| other("pty.lease returned no source sequence"))?,
            expires_at_ms: data
                .get("expiresAtMs")
                .and_then(Value::as_i64)
                .ok_or_else(|| other("pty.lease returned no expiry"))?,
        })
    }

    pub fn prepare_observer(
        &mut self,
        window_label: &str,
        pane_id: &str,
        provider: &str,
    ) -> io::Result<String> {
        let data = self.request(
            "pty.prepareObserver",
            json!({
                "windowLabel": window_label,
                "paneId": pane_id,
                "provider": provider,
            }),
        )?;
        data.get("token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| other("pty.prepareObserver returned no token"))
    }
}

fn parse_session_info(value: &Value) -> Option<SessionInfo> {
    Some(SessionInfo {
        session: value.get("session")?.as_u64()?,
        generation: value.get("generation")?.as_u64()?,
        pane_id: value.get("paneId")?.as_str()?.to_string(),
        window_label: value
            .get("windowLabel")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationFrame {
    Output {
        event_sequence: u64,
        from_sequence: u64,
        through_sequence: u64,
        bytes: Vec<u8>,
    },
    Gap {
        from_event_sequence: u64,
        through_event_sequence: u64,
        from_sequence: u64,
        through_sequence: u64,
    },
    Resize {
        event_sequence: u64,
        cols: u16,
        rows: u16,
    },
    End {
        event_sequence: u64,
        exit_code: i32,
    },
    Opened {
        session: u64,
        generation: u64,
        event_sequence: u64,
        output_sequence: u64,
    },
}

pub struct ObservationStream {
    reader: BufReader<RecvHalf>,
    _writer: SendHalf,
    session: u64,
    generation: u64,
    start_event_sequence: u64,
    start_output_sequence: u64,
}

pub struct LiveStream {
    reader: BufReader<RecvHalf>,
    _writer: SendHalf,
    pub session: u64,
    pub start_sequence: u64,
}

pub struct PreparedObservationStream {
    inner: ObservationStream,
}

impl PreparedObservationStream {
    pub fn connect(runtime_root: &Path, token: &str) -> io::Result<Self> {
        let auth = read_token(runtime_root)?;
        let (recv, mut send) = connect(runtime_root)?;
        let mut reader = BufReader::new(recv);
        greet(&mut reader, &mut send, &auth)?;
        write_line(
            &mut send,
            &proto::request(
                "observe-prepared",
                "pty.observePrepared",
                json!({ "token": token }),
            ),
        )?;
        let reply = read_line(&mut reader)?;
        proto::response_data(&reply).map_err(other)?;
        let temporary = ObservationStream {
            reader,
            _writer: send,
            session: 0,
            generation: 0,
            start_event_sequence: 0,
            start_output_sequence: 0,
        };
        Ok(Self { inner: temporary })
    }

    pub fn into_inner(self) -> ObservationStream {
        self.inner
    }
}

impl ObservationStream {
    pub fn receive_opened(&mut self) -> io::Result<()> {
        let Some(ObservationFrame::Opened {
            session,
            generation,
            event_sequence,
            output_sequence,
        }) = self.next_frame()?
        else {
            return Err(other("prepared observer received no opened event"));
        };
        self.session = session;
        self.generation = generation;
        self.start_event_sequence = event_sequence;
        self.start_output_sequence = output_sequence;
        Ok(())
    }
}

impl LiveStream {
    pub fn attach_lease(runtime_root: &Path, token: &str) -> io::Result<Self> {
        let auth = read_token(runtime_root)?;
        let (recv, mut send) = connect(runtime_root)?;
        let mut reader = BufReader::new(recv);
        greet(&mut reader, &mut send, &auth)?;
        write_line(
            &mut send,
            &proto::request("attach-lease", "pty.attachLease", json!({ "token": token })),
        )?;
        let reply = read_line(&mut reader)?;
        let data = proto::response_data(&reply).map_err(other)?;
        Ok(Self {
            reader,
            _writer: send,
            session: data
                .get("session")
                .and_then(Value::as_u64)
                .ok_or_else(|| other("attach lease returned no session"))?,
            start_sequence: data
                .get("startSeq")
                .and_then(Value::as_u64)
                .ok_or_else(|| other("attach lease returned no start sequence"))?,
        })
    }
}

impl Read for LiveStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buffer)
    }
}

impl ObservationStream {
    pub fn subscribe(runtime_root: &Path, session: u64) -> io::Result<Self> {
        let token = read_token(runtime_root)?;
        let (recv, mut send) = connect(runtime_root)?;
        let mut reader = BufReader::new(recv);
        greet(&mut reader, &mut send, &token)?;
        write_line(
            &mut send,
            &proto::request("observe", "pty.observe", json!({ "session": session })),
        )?;
        let reply = read_line(&mut reader)?;
        let data = proto::response_data(&reply).map_err(other)?;
        Ok(Self {
            reader,
            _writer: send,
            session: data
                .get("session")
                .and_then(Value::as_u64)
                .ok_or_else(|| other("observe response has no session"))?,
            generation: data
                .get("generation")
                .and_then(Value::as_u64)
                .ok_or_else(|| other("observe response has no generation"))?,
            start_event_sequence: data
                .get("startEventSequence")
                .and_then(Value::as_u64)
                .ok_or_else(|| other("observe response has no startEventSequence"))?,
            start_output_sequence: data
                .get("startOutputSequence")
                .and_then(Value::as_u64)
                .ok_or_else(|| other("observe response has no startOutputSequence"))?,
        })
    }

    pub fn session(&self) -> u64 {
        self.session
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn start_event_sequence(&self) -> u64 {
        self.start_event_sequence
    }
    pub fn start_output_sequence(&self) -> u64 {
        self.start_output_sequence
    }

    pub fn next_frame(&mut self) -> io::Result<Option<ObservationFrame>> {
        let mut header = [0u8; 5];
        match self.reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let length = u32::from_be_bytes(header[1..5].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; length];
        self.reader.read_exact(&mut payload)?;
        decode_observation_frame(header[0], &payload).map(Some)
    }
}

pub fn decode_observation_frame(kind: u8, payload: &[u8]) -> io::Result<ObservationFrame> {
    let u64_at = |offset: usize| -> u64 {
        u64::from_be_bytes(payload[offset..offset + 8].try_into().unwrap())
    };
    match kind {
        proto::OBSERVATION_FRAME_OUTPUT if payload.len() >= 24 => Ok(ObservationFrame::Output {
            event_sequence: u64_at(0),
            from_sequence: u64_at(8),
            through_sequence: u64_at(16),
            bytes: payload[24..].to_vec(),
        }),
        proto::OBSERVATION_FRAME_GAP if payload.len() == 32 => Ok(ObservationFrame::Gap {
            from_event_sequence: u64_at(0),
            through_event_sequence: u64_at(8),
            from_sequence: u64_at(16),
            through_sequence: u64_at(24),
        }),
        proto::OBSERVATION_FRAME_RESIZE if payload.len() == 12 => Ok(ObservationFrame::Resize {
            event_sequence: u64_at(0),
            cols: u16::from_be_bytes(payload[8..10].try_into().unwrap()),
            rows: u16::from_be_bytes(payload[10..12].try_into().unwrap()),
        }),
        proto::OBSERVATION_FRAME_END if payload.len() == 12 => Ok(ObservationFrame::End {
            event_sequence: u64_at(0),
            exit_code: i32::from_be_bytes(payload[8..12].try_into().unwrap()),
        }),
        proto::OBSERVATION_FRAME_OPENED if payload.len() == 32 => Ok(ObservationFrame::Opened {
            session: u64_at(0),
            generation: u64_at(8),
            event_sequence: u64_at(16),
            output_sequence: u64_at(24),
        }),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid PTY observation frame kind={kind} length={}",
                payload.len()
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_output_with_absolute_source_coordinates() {
        let mut payload = vec![0u8; 28];
        payload[0..8].copy_from_slice(&7u64.to_be_bytes());
        payload[8..16].copy_from_slice(&900_000u64.to_be_bytes());
        payload[16..24].copy_from_slice(&900_004u64.to_be_bytes());
        payload[24..].copy_from_slice(b"aaaa");
        assert_eq!(
            decode_observation_frame(0, &payload).unwrap(),
            ObservationFrame::Output {
                event_sequence: 7,
                from_sequence: 900_000,
                through_sequence: 900_004,
                bytes: b"aaaa".to_vec(),
            }
        );
    }

    #[test]
    fn refuses_a_malformed_observation_frame() {
        assert!(decode_observation_frame(2, &[0; 11]).is_err());
        assert!(decode_observation_frame(99, &[]).is_err());
    }
}
