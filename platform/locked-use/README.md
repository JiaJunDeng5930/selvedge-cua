# macOS Locked Use bridge

Selvedge uses [Open Computer Use](https://github.com/iFurySt/open-computer-use/tree/e25be21caa0ea489316f6be6635d2a4766c4cd0b)
for guarded access to the existing macOS console session. The patch adds a
bridge mode to the signed Open Computer Use executable and reports the
Broker's existing stop reason on the authenticated owner's end reply. The
Broker, Guardian, watchdog, authorization plug-in and administrator installer
retain their upstream admission and recovery policies. The pinned source is
distributed under the [upstream MIT license](LICENSE.open-computer-use).

## Build and check

A build requires macOS with Xcode and Swift 6.2 or newer. The build retrieves the
pinned upstream commit into a new output directory. A development smoke check
compiles the bridge, sends a first-command `finish`, and verifies that an
untrusted parent attempting `begin` is rejected before looking up the macOS
session. It can run without a signing identity or an installed Broker.

```sh
platform/locked-use/build.sh --smoke /absolute/path/to/new-smoke-output
```

To build the deployable bundle, use an existing Developer ID Application identity.
The script builds and verifies the four same-team signed Open Computer Use
components and prints the absolute path to the bridge executable. It leaves
the macOS authorization policy, privacy settings and installed services alone.

```sh
export OPEN_COMPUTER_USE_CODESIGN_IDENTITY='Developer ID Application: Example (TEAMID)'
platform/locked-use/build.sh /absolute/path/to/new-release-output

# After building the Selvedge Rust executable with cargo build --release --locked
SELVEDGE_CODESIGN_IDENTITY="$OPEN_COMPUTER_USE_CODESIGN_IDENTITY" \
  platform/macos/package-app.sh target/release/selvedge-cua-mcp \
  'target/release/Selvedge CUA MCP.app'
```

## Deploy on a test Mac

From the active console account, place both signed app bundles in
operator-managed locations. Deploy the signed components
from the bundle that contains this bridge. The upstream installer stages that
same signed app and records its team and signing identifier for both Broker
roles. If Open Computer Use Locked Use is already installed, follow the
upstream disable/enable procedure before using the new build so the installed
Broker can report the authenticated stop reason. Installation, system policy
changes and permission prompts require an intentional administrator action.
See the [upstream deployment and recovery instructions](https://github.com/iFurySt/open-computer-use/blob/e25be21caa0ea489316f6be6635d2a4766c4cd0b/docs/locked-use.en.md).

```sh
helper='/absolute/path/to/Open Computer Use.app/Contents/MacOS/OpenComputerUse'
"$helper" locked-use enable
"$helper" locked-use status
'/absolute/path/to/Selvedge CUA MCP.app/Contents/MacOS/selvedge-cua-mcp' --locked-use-helper "$helper"
```

Grant the upstream permissions called out by `locked-use status` and ensure the
Selvedge process has the Accessibility and Screen Recording permissions its
ordinary desktop operations need. Start the server in that same GUI login.
When needed, uninstall with `"$helper" locked-use disable`, which invokes
upstream restoration of the original policy.

The bridge verifies the running parent executable's same-team signature,
fixed Selvedge signing identifier and hardened runtime. The existing Broker
independently verifies the bridge's running app signature in both its agent
and client roles. The parent check authenticates an executable. Anyone who can
launch and drive the configured signed MCP server can request desktop access,
so restrict access to that server and its stdio connection to intended clients.
Keep operator launch configuration and installed binaries under trusted control.

## Lease behavior

The stdin protocol accepts `begin`, `check` and `finish`, each on its own line.
An already unlocked console is ordinary only when the signed Broker confirms
it is idle and no guards are active. A locked console requires the existing
protected acquisition path. Protected capture uses an application window
filter, since the Guardian covers the physical displays. The Rust server
drains desktop work before sending `finish`. The bridge reports a confirmed
lock only after matching the original console session and observing authenticated
Broker confirmation that the shields were released. If protected access ended
before cooperative finish, the response includes the stop reason along with
any confirmed relock. Parent loss closes the lease and leaves recovery to the
Guardian, watchdog and Broker.

The development smoke checks cover compilation and harmless protocol commands.
Signed protected acquisition, local takeover, screen capture and relock require
end-to-end validation on an explicitly configured test Mac.
