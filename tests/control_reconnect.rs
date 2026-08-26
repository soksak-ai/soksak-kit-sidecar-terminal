#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;

use serde_json::{json, Value};
use soksak_kit_sidecar_terminal::daemon::ControlClient;

// Serves greetings and one command per connection, then drops the connection: the shape a unit that
// was replaced leaves behind for whoever still holds a socket to the one that went.
fn serve(listener: UnixListener, rounds: usize) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for _ in 0..rounds {
            let Ok((stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut writer = stream;
            let mut line = String::new();
            // greeting
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let hello: Value = serde_json::from_str(line.trim()).expect("greeting");
            let id = hello.get("id").and_then(Value::as_str).unwrap_or("greeting").to_string();
            writeln!(writer, "{}", json!({"id": id, "ok": true, "result": {"code": "OK", "data": {}}})).expect("greet");
            writer.flush().expect("flush");
            line.clear();
            // one command, then the connection ends
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).expect("request");
            let id = request.get("id").and_then(Value::as_str).unwrap_or("x").to_string();
            writeln!(writer, "{}", json!({"id": id, "ok": true, "result": {"code": "OK", "data": {"token": "observer"}}})).expect("reply");
            writer.flush().expect("flush");
        }
    })
}

fn stage(root: &Path) -> UnixListener {
    std::fs::write(soksak_kit_sidecar_terminal::proto::pty_token_path(root), "token\n").expect("token");
    UnixListener::bind(soksak_kit_sidecar_terminal::proto::pty_socket_path(root)).expect("bind")
}

// A request on a connection the unit already ended is not a failure to hand back: the unit is there,
// the connection is not, and the request reaches it by connecting again.
#[test]
fn a_request_on_an_ended_connection_reaches_the_unit_again() {
    let home = std::env::temp_dir().join(format!("soksak-reconnect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("home");
    let listener = stage(&home);
    let served = serve(listener, 3);

    let mut client = ControlClient::connect(&home).expect("connect");
    let first = client.request("pty.prepareObserver", json!({})).expect("first request");
    assert_eq!(first.get("token").and_then(Value::as_str), Some("observer"));

    // The unit ended this connection. The next request must still reach it.
    let second = client.request("pty.prepareObserver", json!({})).expect("second request");
    assert_eq!(second.get("token").and_then(Value::as_str), Some("observer"));

    drop(served);
    let _ = std::fs::remove_dir_all(&home);
}
