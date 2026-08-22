# soksak-kit-sidecar-terminal

Shared recovery runtime for every implementation of soksak-spec-sidecar-terminal.

This kit owns PTY observation, source ordering, service transport, session lifecycle and recovery
status. A terminal-state sidecar supplies a `TerminalStateMirror` implementation and its installed
sidecar name.
The kit defines no terminal semantics and selects no engine.

## Verification

```sh
cargo test --locked
```
Terminal status reports each recovery mirror's last observed columns, rows, and source event
sequence. Callers use these facts to identify the first resize boundary that did not advance.
