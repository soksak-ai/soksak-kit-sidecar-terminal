//! Local wire facts used by terminal-state sidecars.
//!
//! The PTY and terminal-state sidecars each own one identity-scoped socket. Both speak the shared
//! control envelope before a connection becomes a byte stream. Paths name installed sidecars; the
//! contract id is not a process identity.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub const PTY_PROTOCOL_VERSION: u32 = 1;
pub const PTY_INTERFACE_ID: &str = "soksak-spec-sidecar-pty";

pub const OBSERVATION_FRAME_OUTPUT: u8 = 0;
pub const OBSERVATION_FRAME_GAP: u8 = 1;
pub const OBSERVATION_FRAME_RESIZE: u8 = 2;
pub const OBSERVATION_FRAME_END: u8 = 3;
pub const OBSERVATION_FRAME_OPENED: u8 = 4;

pub fn pty_socket_path(runtime_root: &Path, sidecar_name: &str) -> String {
    soksak_contract_control::address(
        runtime_root,
        &format!("{sidecar_name}-p{PTY_PROTOCOL_VERSION}"),
        cfg!(windows),
    )
}

pub fn pty_token_path(runtime_root: &Path, sidecar_name: &str) -> PathBuf {
    runtime_root.join(format!("{sidecar_name}-p{PTY_PROTOCOL_VERSION}.token"))
}

pub fn service_socket_path(runtime_root: &Path, sidecar_id: &str) -> String {
    soksak_contract_control::address(runtime_root, &format!("{sidecar_id}-p1"), cfg!(windows))
}

pub fn hello(id: &str, token: &str) -> Value {
    json!({
        "id": id,
        "command": "system.hello",
        "args": { "protocol": soksak_contract_control::PROTOCOL, "token": token },
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
    fn endpoints_name_the_installed_sidecars() {
        let runtime = Path::new("/runtime");
        assert_eq!(
            pty_socket_path(runtime, "soksak-sidecar-pty"),
            "/runtime/soksak-sidecar-pty-p1.sock"
        );
        assert_eq!(
            service_socket_path(runtime, "soksak-sidecar-terminal-vt100"),
            "/runtime/soksak-sidecar-terminal-vt100-p1.sock"
        );
    }

    #[test]
    fn requests_use_the_shared_control_envelope() {
        assert_eq!(hello("greeting", "secret")["command"], "system.hello");
        assert_eq!(hello("greeting", "secret")["args"]["protocol"], 2);
        let request = request("r1", "pty.observe", json!({ "session": 7 }));
        assert_eq!(request["args"]["request"]["session"], 7);
    }
}
