# soksak-kit-sidecar-terminal

Shared recovery runtime for every implementation of soksak-spec-sidecar-terminal.

This kit owns PTY observation, source ordering, service transport, session lifecycle and recovery
status. A terminal-state sidecar supplies a `TerminalStateMirror` implementation and its installed
sidecar name.
The kit defines no terminal semantics and selects no engine.

Every service accepts the control contract's `SOKSAK_PROCESS_LABEL` at its process boundary,
validates it once, and publishes it in the protocol-2 announcement and greeting. The label is for
public process inventory and monitoring only; it does not change component identity, sockets,
dependencies, ownership, or the operating system's executable-derived process name.

`SOKSAK_SIDECAR_NAME` identifies the current service's materialized process and owns its service
socket/token. `SOKSAK_SIDECAR_BINDINGS` is the environment-selected component-id to process-name
map; the PTY client resolves `soksak-sidecar-pty` from that map. Own identity and peer discovery are
separate facts and neither is inferred from the executable path.

Live handoff snapshots publish mirror paint and its absolute PTY output sequence atomically. A
snapshot can therefore be followed by `pty.attachLease` without replaying or dropping bytes.
Checkpoint commits are serialized per pane across threads and processes. Their
`(generation, sequence)` position only advances; an older background write cannot replace a newer
explicit archive. Readers observe only the atomically renamed file, never an in-progress file.
A new PTY generation replays that archive into its engine before applying live output, preserving
the old screen as scrollback. It advances one viewport, clears that new viewport and homes its
cursor before live output so a fresh shell cannot overwrite archived visible rows or inherit the
old cursor. Reattaching to a live generation never replays it.
`terminal.frame` publishes the viewport as runs together with the exact output sequence applied to
that mirror under the same lock, so callers never infer renderer progress from a request coordinate.
Each `subscriber` receives a full picture first and changed rows afterwards; a resize, an offset
change or an alternate-screen switch forces a full picture again. `offset` scrolls the viewport into
history and is clamped to `historySize`. `terminal.status` reports `capabilities.hyperlinks` for the
engine behind the sidecar.

The mirror publishes engine-owned cursor shape and blink state as terminal state. It publishes the
provider/user blink interval separately as animation policy. Warm rehydrate restores DECSCUSR but
does not serialize animation phase or policy. The shared native painter uses the declared terminal
theme's `cursor` and `cursorAccent` colors for block, underline and bar shapes; adapters do not
re-parse CSI. Its condition variable has a deadline only while the engine says a visible cursor is
blinking. Steady or hidden cursors wait only for explicit output/control events, and output activity
resets the blink phase. The cursor state is present in frames and encrypted checkpoints; the old
shape-less checkpoint form is not accepted through a compatibility path.

The native painter receives one explicit `light|dark` base palette from the host and resolves the
engine's OSC 4/10/11/12 state over it. A null override means no terminal override: OSC
104/110/111/112 therefore reveals the current base rather than a cached earlier theme. A changed
effective palette invalidates every row and advances the rendered frame before `surface.state`
publishes `themeMode`, `baseTheme`, `terminalOverrides` and `effectiveTheme`. Providers expose
engine color state through `TerminalStateMirror.theme_overrides`; adapters do not parse OSC.
`surface.theme` validates one complete replacement base, preserves active engine overrides,
updates the resize palette and wakes the render thread. It does not reopen the surface or poll.

## Verification

```sh
make lock
make verify
```

`make lock` is the owner operation that projects a changed `Cargo.toml` into `Cargo.lock` while
preserving the existing dependency resolution. Normal build and verification remain `--locked`.

`rust-toolchain.toml` and `.python-version` are the exact toolchain owners. Make rejects a mismatched
version or architecture before dependency materialization, then runs the Rust suite. Release Actions
inject the immutable spec package by release-train URL and
SHA-256 and run this same command; they do not checkout or rebuild spec source.

Terminal status reports each recovery mirror's last observed columns, rows, source event sequence,
absolute output sequence, and gap count. Callers use these facts to identify the first boundary that
did not advance.
