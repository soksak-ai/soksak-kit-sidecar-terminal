# soksak-kit-sidecar-terminal

Shared recovery runtime for every implementation of soksak-spec-sidecar-terminal.

This kit owns PTY observation, source ordering, service transport, session lifecycle and recovery
status. An engine unit supplies a TerminalStateMirror implementation and its installed unit name.
The kit defines no terminal semantics and selects no engine.
