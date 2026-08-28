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

Terminal-sidecar owner gates install `soksak-sidecar-pty@0.0.6` through
`scripts/install_pty_release.py`. The installer accepts an explicit release target, verifies the
owner release identity, source commit, artifact size and SHA-256, and extracts regular files only.
`release.json` names each artifact by bare `file`; the installer downloads
`https://github.com/soksak-ai/soksak-sidecar-pty/releases/download/v0.0.6/<file>` and refuses a
document that carries a `url` key. It never discovers a Core checkout or builds a PTY provider from
source.

The pinned `soksak-sidecar-pty@0.0.6` was published with `url` keys in its release document, so the
installer refuses it; the pin moves to the first PTY release published without `url` (0.0.8 or later),
and until then the owner gate that installs the PTY fails by that refusal.

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

## Verification

```sh
make lock
make verify
```

`make lock` is the owner operation that projects a changed `Cargo.toml` into `Cargo.lock` while
preserving the existing dependency resolution. Normal build and verification remain `--locked`.

`rust-toolchain.toml` and `.python-version` are the exact toolchain owners. Make rejects a mismatched
version or architecture before dependency materialization, then runs both the Rust suite and the PTY
release-installer suite. Release Actions inject the immutable spec package by release-train URL and
SHA-256 and run this same command; they do not checkout or rebuild spec source.

Terminal status reports each recovery mirror's last observed columns, rows, source event sequence,
absolute output sequence, and gap count. Callers use these facts to identify the first boundary that
did not advance.
