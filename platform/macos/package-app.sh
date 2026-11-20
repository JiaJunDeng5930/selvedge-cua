#!/bin/bash
set -euo pipefail

if [[ $# != 2 || "$2" != *.app ]]; then
    echo 'Usage: package-app.sh BUILT_EXECUTABLE OUTPUT.app' >&2
    exit 64
fi

platform_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source=$1
output_dir="$(cd "$(dirname "$2")" && pwd)"
app="$output_dir/$(basename "$2")"
plist="$platform_dir/Info.plist"
identifier="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist")"
executable="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist")"
identity=${SELVEDGE_CODESIGN_IDENTITY:--}
signing_options=()
if [[ "$identity" != - ]]; then
    signing_options=(--options runtime --timestamp)
fi

staging="$(mktemp -d "$output_dir/.selvedge-app.XXXXXX")"
trap 'rm -rf "$staging"' EXIT
bundle="$staging/$(basename "$app")"
png="$platform_dir/../../assets/selvedge-cua.png"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
/usr/bin/sips -s format icns "$png" --out "$bundle/Contents/Resources/Selvedge.icns" >/dev/null
install -m 644 "$plist" "$bundle/Contents/Info.plist"
install -m 755 "$source" "$bundle/Contents/MacOS/$executable"
sdk_version="$(xcrun --sdk macosx --show-sdk-version)"
if [[ ${sdk_version%%.*} -ge 26 ]]; then
    speech="$bundle/Contents/MacOS/selvedge-apple-speech"
    architectures="$(lipo -archs "$source")"
    speech_archives=()
    for arch in $architectures; do
        thin_speech="$staging/selvedge-apple-speech-$arch"
        xcrun --sdk macosx swiftc -parse-as-library -O -target "$arch-apple-macosx14.0" \
            -module-cache-path "$staging/swift-cache" \
            "$platform_dir/AppleSpeechTranscriber.swift" -o "$thin_speech"
        speech_archives+=("$thin_speech")
    done
    lipo -create "${speech_archives[@]}" -output "$speech"
    /usr/bin/codesign --force --sign "$identity" --identifier "$identifier.apple-speech" \
        ${signing_options[@]+"${signing_options[@]}"} "$speech"
fi
/usr/bin/codesign --force --sign "$identity" --identifier "$identifier" ${signing_options[@]+"${signing_options[@]}"} "$bundle"
/usr/bin/codesign --verify --deep --strict "$bundle"

# Keep the live bundle path until the replacement has passed signature verification.
if [[ -d "$app" ]]; then
    /bin/mv "$app" "$staging/previous.app"
fi
if ! /bin/mv "$bundle" "$app"; then
    if [[ -d "$staging/previous.app" ]]; then
        /bin/mv "$staging/previous.app" "$app"
    fi
    exit 1
fi
printf '%s\n' "$app/Contents/MacOS/$executable"
