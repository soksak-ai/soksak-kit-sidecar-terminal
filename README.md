# soksak-kit-sidecar-terminal

Shared recovery runtime for every implementation of soksak-spec-sidecar-terminal.

This kit owns PTY observation, source ordering, service transport, session lifecycle and recovery
status. A terminal-state sidecar supplies a `TerminalStateMirror` implementation and its installed
sidecar name.
The kit defines no terminal semantics and selects no engine.

Terminal-sidecar owner gates install `soksak-sidecar-pty@0.0.4` through
`scripts/install_pty_release.py`. The installer accepts an explicit release target, verifies the
owner release identity, source commit, artifact size and SHA-256, and extracts regular files only.
It never discovers a Core checkout or builds a PTY provider from source.

Live handoff snapshots publish mirror paint and its absolute PTY output sequence atomically. A
snapshot can therefore be followed by `pty.attachLease` without replaying or dropping bytes.

## Verification

```sh
cargo test --locked
python3 scripts/test_install_pty_release.py
```
Terminal status reports each recovery mirror's last observed columns, rows, and source event
sequence. Callers use these facts to identify the first resize boundary that did not advance.
