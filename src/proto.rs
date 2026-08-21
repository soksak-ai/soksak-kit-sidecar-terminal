//! Local wire facts used by this unit.
//!
//! The PTY and terminal-state units each own one identity-scoped socket. Both speak the shared
//! control envelope before a connection becomes a byte stream. Paths name installed units; the
//! contract id is not a process identity.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const CONTROL_PROTOCOL: u32 = 1;
pub const PTY_PROTOCOL_VERSION: u32 = 1;
pub const PTY_UNIT_NAME: &str = "soksak-sidecar-pty";

pub const OBSERVATION_FRAME_OUTPUT: u8 = 0;
pub const OBSERVATION_FRAME_GAP: u8 = 1;
pub const OBSERVATION_FRAME_RESIZE: u8 = 2;
pub const OBSERVATION_FRAME_END: u8 = 3;
pub const OBSERVATION_FRAME_OPENED: u8 = 4;

pub fn pty_socket_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join(format!("{PTY_UNIT_NAME}-p{PTY_PROTOCOL_VERSION}.sock"))
}

pub fn pty_token_path(runtime_root: &Path) -> PathBuf {
    runtime_root.join(format!("{PTY_UNIT_NAME}-p{PTY_PROTOCOL_VERSION}.token"))
}

pub fn service_socket_path(runtime_root: &Path, unit_name: &str) -> PathBuf {
    runtime_root.join(format!("{unit_name}-p1.sock"))
}

pub fn hello(id: &str, token: &str) -> Value {
    json!({
        "id": id,
        "command": "system.hello",
        "args": { "protocol": CONTROL_PROTOCOL, "token": token },
    })
}

pub fn request(id: &str, command: &str, request: Value) -> Value {
    json!({ "id": id, "command": command, "args": { "request": request } })
}

pub fn response_data(reply: &Value) -> Result<&Value, String> {
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("request refused")
            .to_string());
    }
    Ok(reply
        .get("result")
        .and_then(|result| result.get("data"))
        .unwrap_or(&Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_name_the_installed_units() {
        let runtime = Path::new("/runtime");
        assert_eq!(
            pty_socket_path(runtime),
            Path::new("/runtime/soksak-sidecar-pty-p1.sock")
        );
        assert_eq!(
            service_socket_path(runtime, "soksak-sidecar-terminal-vt100"),
            Path::new("/runtime/soksak-sidecar-terminal-vt100-p1.sock")
        );
    }

    #[test]
    fn requests_use_the_shared_control_envelope() {
        assert_eq!(hello("greeting", "secret")["command"], "system.hello");
        let request = request("r1", "pty.observe", json!({ "session": 7 }));
        assert_eq!(request["args"]["request"]["session"], 7);
    }
}
