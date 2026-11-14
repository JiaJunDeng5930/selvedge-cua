#!/usr/bin/env bash
set -euo pipefail

upstream_revision=e25be21caa0ea489316f6be6635d2a4766c4cd0b
upstream_repository=${SELVEDGE_OCU_REPOSITORY:-https://github.com/iFurySt/open-computer-use.git}
bridge_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

smoke=0
if [[ "${1:-}" == --smoke ]]; then
    smoke=1
    shift
fi
if [[ $# != 1 ]]; then
    echo 'Usage: build.sh [--smoke] NEW_OUTPUT_DIRECTORY' >&2
    exit 64
fi
if [[ "$smoke" == 0 && "${OPEN_COMPUTER_USE_CODESIGN_IDENTITY:-}" != 'Developer ID Application: '* ]]; then
    echo 'Set OPEN_COMPUTER_USE_CODESIGN_IDENTITY to an existing Developer ID Application identity.' >&2
    exit 64
fi

# Upstream signing scripts otherwise change the user keychain search list.
unset OPEN_COMPUTER_USE_CODESIGN_KEYCHAIN OPEN_COMPUTER_USE_PROVISIONING_PROFILE
unset OPEN_COMPUTER_USE_APP_VARIANT
mkdir "$1"
output="$(cd "$1" && pwd)"
source="$output/source"
mkdir "$output/module-cache"
export CLANG_MODULE_CACHE_PATH="$output/module-cache"
export SWIFTPM_MODULECACHE_OVERRIDE="$output/module-cache"

git init --quiet "$source"
git -C "$source" fetch --quiet --depth=1 -- "$upstream_repository" "$upstream_revision"
git -C "$source" checkout --quiet --detach FETCH_HEAD
test "$(git -C "$source" rev-parse HEAD)" = "$upstream_revision"
git -C "$source" apply --unidiff-zero "$bridge_dir/upstream.patch"
cp "$bridge_dir/SelvedgeLockedUseBridge.swift" "$source/apps/OpenComputerUse/Sources/OpenComputerUse/"
cd "$source"

if [[ "$smoke" == 1 ]]; then
    # A nested SwiftPM sandbox is unavailable under the workspace sandbox.
    swift build --disable-sandbox --product OpenComputerUse >&2
    helper="$(swift build --disable-sandbox --show-bin-path)/OpenComputerUse"
else
    export OPEN_COMPUTER_USE_APP_OUTPUT_DIR="$output"
    export OPEN_COMPUTER_USE_CODESIGN_MODE=identity
    export OPEN_COMPUTER_USE_INCLUDE_LOCKED_USE=1
    scripts/build-open-computer-use-app.sh release >&2
    app="$output/Open Computer Use.app"
    scripts/verify-locked-use-bundle.sh "$app" >&2
    helper="$app/Contents/MacOS/OpenComputerUse"
fi

# This first command exits before reading the session or contacting any Broker.
if [[ "$(printf 'finish\n' | "$helper" --selvedge-locked-use-bridge)" != '{"state":"ok"}' ]]; then
    echo 'Locked Use bridge did not acknowledge first-command finish.' >&2
    exit 1
fi
if [[ "$smoke" == 1 ]]; then
    rejection="$(printf 'begin\nfinish\n' | "$helper" --selvedge-locked-use-bridge)"
    expected=$'{"error":"Locked Use requires a hardened, same-team signed Selvedge CUA MCP parent."}\n{"state":"ok"}'
    if [[ "$rejection" != "$expected" ]]; then
        echo 'Locked Use bridge did not reject an unsigned parent before admission.' >&2
        exit 1
    fi
fi
printf '%s\n' "$helper"
