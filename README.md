# Selvedge CUA MCP

Selvedge runs computer-use scripts against the macOS desktop. It exposes one
MCP tool, `run({code})`, over stdio. Each call runs an async JavaScript function
body in embedded QuickJS and returns JSON and any captured PNGs.

## Build and run

Build on macOS with Xcode Command Line Tools, Rust, and the Rocq 9.2 toolchain
used to build [Rocq2Rust](https://github.com/OpenSourceVerif/Rocq2Rust). Follow
the compiler's installation instructions, including its MetaRocq overlay, and
run `make build` in the compiler checkout. The Cargo build expects a sibling
`rocq2rust` checkout by default. For a different location:

```sh
export ROCQ2RUST_PLUGIN=/absolute/path/to/rocq2rust/_build/rocq2rust-plugin
cargo build --release --locked
```

Configure the MCP client to launch `target/release/selvedge-cua-mcp` by
absolute path with stdio transport. The running binary uses macOS 14 or
later and the host application's Screen Recording and Accessibility
permissions. JavaScript, Rocq, and extraction are built into the executable
or used during the build. There are no language runtimes to install alongside
the binary.

If a build at the configured path repeatedly times out in ScreenCaptureKit
while an identical copy works on the same unlocked desktop, install the
verified build at a fresh path and point the MCP client at that executable.
Reconnect the client so it launches the replacement. An existing MCP
process continues using its original executable until it exits.

## Write a script

A script can return any JSON-serializable value. Native calls return promises
and report errors as rejections. The following example assumes the script
already knows the target application's PID as `targetPid`.

```js
const ui = await readUI({ pid: targetPid });
const windowId = ui.window.windowId;
const observation = observeScreen({
  target: { windowId },
  durationMs: 700,
  preserveInputBoundaries: true,
});
await observation.started;
const point = ui.focused.position;
await pointer({
  windowId,
  position: t => ({ x: point.x + 20 + t / 4, y: point.y + 10 + t * t / 2000 }),
  durationMs: 250,
  buttons: [0],
});
return await observation.finished;
```

| Function | Options and behavior |
| --- | --- |
| `observeScreen` | `{durationMs?, intervalMs?, imageBudget?, preserveInputBoundaries?, target?}`. A zero duration gives one frame. The default target is the main display. Set `target` to `{displayId}`, `{windowId}`, or `{region: {x, y, width, height}}`. Await `.started` for the first frame's time. Await the observation directly or via `.finished` for the image manifest. Set `preserveInputBoundaries: true` to keep identical samples taken after different completed input dispatches. |
| `readUI` | `{pid?, maxDepth?, maxNodes?, fields?}`. The default PID is the foreground app. Returns its focused element, window tree, input snapshot and observation time. A matching on-screen window also exposes `windowId`. |
| `pointer` | `{windowId?, position, durationMs?, intervalMs?, buttons?, hold?, operation?}`. A new trajectory requires a window ID. The function receives the elapsed milliseconds and returns `{x, y}`. The default duration is zero and the default sample interval is 16 ms. Buttons 0, 1 and 2 correspond to left, right and middle. |
| `keyboard` | `{pid?, events, hold?, operation?}`. Supply the PID for a new keyboard operation. Each event is `{atMs?, key, down}`. Physical key names use `KeyboardEvent.code` names such as `KeyA`, `ShiftLeft` and `ArrowDown`. |
| `scroll` | `{windowId?, x, y, events, operation?}`. Supply a window ID or a window-bound operation. Each event is `{atMs?, dx?, dy?}` in pixels. Positive deltas scroll right and down. Passing an operation preserves its held controls. |
| `insertText` | `{target, text}`. Pass the `target` returned by `readUI` for the focused editable text field in the target application. |
| `performUIAction` | `{target, action, value?}`. Use an action exposed by `readUI`. `AXSetValue` accepts a JSON string, number or boolean on writable controls. |

Omit `readUI.fields` for all node attributes. Pass a list containing any of
`role`, `subrole`, `title`, `description`, `value`, `position`, `size`,
`enabled`, `identifier`, `help`, `placeholder`, `actions`, and `settable`
to limit the attributes read and returned. Each node still provides its
`role` and `target`, the window tree retains its children, and the root
still includes a matching `windowId`.

Times with the `AtMs` suffix are measured in milliseconds since the server
started. Event offsets are relative to the start of their input plan. Pointer
and scroll coordinates use global desktop logical points, with the origin at
the top-left of the main display. Input plans allow up to 30 seconds and 1024
trajectory samples or explicit events. Observation allows up to 30 seconds,
24 frames and an image budget of 1–12. Frame rates depend on macOS delivery.

A successful input call reports the dispatch times and held controls. Set
`hold: true` to keep controls down and get an operation reference. Pass that
reference in a later input call to continue the same action, including across
MCP calls. `pointer({operation})` releases the controls without moving.
A cancelled plan stops between dispatches and releases the controls still
recorded as held. A plan interrupted after dispatch begins includes a partial
receipt in the rejection message. An application may react after the dispatch
completes.

The MCP result has `structuredContent.result` and
`structuredContent.observations`. The latter contains each successful
observation and its `imageOffset` and `imageCount` among PNG attachments.
Add `imageOffset` to a tile's `imageIndex` to locate the attachment. The tile's
`imageRect` is the pixel rectangle in that PNG, and its `desktopRect` describes
the source in desktop points. Use the manifest's `viewport` for the last real
capture. Each tile carries its representative capture time and the bounds of
the sample interval it represents. These times record macOS capture delivery.

Consecutive equivalent captures share one interval. If the sampled sequence is
A, A, A, B, A, the three intervals show whole frames A, B, A. Aligned scrolling
content can appear as a mosaic with per-strip source coordinates. In a
sequence of shifts in the same direction, varying detected bounds can share
one mosaic when the sampled pixels confirm a translation over common bounds.
Sustained animation may omit intermediate frames to stay within the image budget.
Whole frames and scroll parts can share an attachment when its layout
preserves their modeled detail. Packing minimizes estimated image tokens
among the available row and column layouts. Actual token accounting depends
on the MCP client and model. The observation rejects if required evidence
cannot fit within the image budget at that detail.

## Locked Use on macOS

A configured Locked Use helper can open a protected session for a script that
starts while the current console desktop is locked. Selvedge operates on the
same desktop and applications. The helper uses Open Computer Use's signed
macOS authorization component and display guardian. Once the platform
components have been installed and approved, individual scripts require no
unlock prompt.

Set the MCP server's arguments to
`--locked-use-helper /absolute/path/to/the/signed/helper`. The helper path is
set by the operator who starts the MCP server. Scripts use the same `run`
tool and seven JavaScript functions. A script that does no desktop work does
not start a helper. See the [macOS build and deployment instructions](platform/locked-use/README.md).

While a protected session is active, `observeScreen` requires
`target: {windowId}`. Use `readUI({pid})` to find the application window.
The window-only filter captures the requested application window separately
from the physical display. Input may stay held between calls inside a script.
The server releases its held input before ending a helper-managed script.
It reports successful protected completion after the helper confirms the
original session has relocked. Local takeover and guard failure revoke the
session.

## Source and validation

[`Requirement.v`](crates/rules/formal/Requirement.v) derives the desktop
contracts from engineering needs. The executable rules, including
[`ObservationRules.v`](crates/rules/formal/ObservationRules.v), are in the same
directory. The Cargo build checks the Rocq
objects, audits theorem assumptions, and extracts the functions used by the
Rust adapters. The system clock, image measurements, JavaScript engine,
and macOS responses are external inputs to those rules.

```sh
cargo clippy --workspace --locked -- -D warnings
cargo build --locked
python3 tests/acceptance.py target/debug/selvedge-cua-mcp
python3 tests/locked_use_acceptance.py target/debug/selvedge-cua-mcp
python3 tests/acceptance.py target/debug/selvedge-cua-mcp --desktop
```

The desktop acceptance opens an AppKit fixture in the active, unlocked
desktop. It drives the fixture through MCP stdio and checks screenshots,
a continuous drag with one undo, text insertion, accessibility actions,
cancellation and scrolling. The fixture runs in the background and the test
compares the foreground application and hardware cursor before and after
the scripted input. Run it while the pointer is stationary for that comparison.

Mouse and scroll dispatch require macOS window-local event routing. AppKit
applications can receive it in the background. The routing capability is
checked before dispatch, and application support for routed events varies.
Keyboard events are directed to the target process. Accessibility actions
act on the retained element directly. Text insertion checks the requested
field's focus, then writes its accessible selection where supported.
Other fields receive process-directed text after a focus check. Focus can
change between that check and delivery. The receipt reports
`accessibilitySelection`, `focusedProcess`, or `noOp`.
