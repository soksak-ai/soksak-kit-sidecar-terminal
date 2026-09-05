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
`surface.focus` changes only presentation: focus loss stops the blink deadline and paints a steady
hollow block, while the engine-owned shape and blinking value remain in state. Focus gain restores
that engine presentation. The Metal cursor border is shared by every provider.

The native painter receives one explicit `light|dark` base palette from the host and resolves the
engine's OSC 4/10/11/12 state over it. A null override means no terminal override: OSC
104/110/111/112 therefore reveals the current base rather than a cached earlier theme. A changed
effective palette invalidates every row and advances the rendered frame before `surface.state`
publishes `themeMode`, `baseTheme`, `terminalOverrides` and `effectiveTheme`. Providers expose
engine color state through `TerminalStateMirror.theme_overrides`; adapters do not parse OSC.
`surface.theme` validates one complete replacement base, preserves active engine overrides,
updates the resize palette and wakes the render thread. It does not reopen the surface or poll.

Native selection uses `soksak-contract-surface` 0.0.6. The common mirror owns gesture identity,
monotonic sequence, stale-owner refusal, viewport-offset translation and complete snapshots. An
engine adapter owns begin/update/clear, selected text and inclusive row ranges; no generic cell-text
fallback exists. The painter consumes those engine ranges and applies the declared selection
foreground/background. `surface.selection` publishes the same snapshot through `surface.state` and
wakes a paint only for mutations.
The same state response publishes the engine's complete mouse-tracking and alternate-scroll modes;
the Plugin routes gestures from that fact rather than a provider name or DOM-position guess.
DEC private mode 9 X10 tracking and private mode 1001 highlight tracking are independent public
facts (`mouseX10` and `mouseHighlight`). Neither is represented by setting click, drag or motion
tracking. Every live tracking fact can select the engine-owned wheel-report route; X10 pointer
routing is press-only, while highlight routing admits press and release for provider-native
encoding.

The surfaces of a pane's ring are the box the application handed over, in whole device pixels.
The cell is the font's advance rounded up to whole device pixels — the size the painter steps by
— and the grid is what fits of that cell in the box; `cellW`/`cellH` in every answer are that
cell. What no cell covers, right of the last column and below the last row, is painted with the
background the cells are painted with, dimmed as they are. A ring sized to whole cells left a
strip of the document behind the surface on screen, a different width on every card, and a grid
counted in the fractional advance ran past the box by up to a column (measured 2026-09-05).

Native wheel input keeps pixel/line/page units until this Kit normalizes them against the pane's
measured cell box. Fractional pixel deltas stay in a per-pane accumulator. Shift forces local
scrollback; otherwise current engine modes choose mouse report, alternate scroll or scrollback.
The engine adapter alone encodes input bytes. This Kit returns them under the strict surface result
and never writes the PTY.

Native pointer input uses the same measured cell box. Shift bypasses terminal capture. Press follows
every tracking mode. Release follows highlight/click/drag/motion modes but not X10, held movement
follows drag or motion mode, and no-button movement follows motion mode only. The selected engine
adapter encodes the report; ignored input returns no bytes.

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
did not advance. It also reports cumulative observed output bytes and the last output observation's
source range, byte count, and SHA-256. A sequence that advances without matching observed bytes is
therefore a directly visible source-delivery defect rather than an apparently successful frame.
