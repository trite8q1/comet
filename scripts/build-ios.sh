#!/usr/bin/env bash
# Build the Zeron iOS app (apps/ios). Requires macOS with Xcode 26+ (iOS 26 SDK
# — the app uses Liquid Glass APIs). This is a thin, opinionated wrapper around
# `xcodebuild`; for day-to-day work you can still just open Zeron.xcodeproj.
#
# Subcommands:
#   sim       Build for the iOS Simulator and (RUN=1, the default) boot it,
#             install, and launch straight against your local mesh edge in dev
#             mode — the fastest way to see it working, no code signing needed.
#   device    Archive and export a signed .ipa for a real iPhone. Needs your
#             own Apple Team: set DEVELOPMENT_TEAM (and usually a unique
#             BUNDLE_ID, since the default sh.zeron.ios belongs to another team).
#   archive   Just produce the .xcarchive (no export).
#   test      Run ZeronTests on the simulator.
#
# Usage:
#   scripts/build-ios.sh sim
#   RUN=0 scripts/build-ios.sh sim
#   DEVELOPMENT_TEAM=ABCDE12345 BUNDLE_ID=com.you.zeron scripts/build-ios.sh device
#
# Env:
#   CONFIG           Debug | Release           (default: Release; sim uses Debug)
#   SIMULATOR        simulator device name     (default: iPhone 17 Pro)
#   DERIVED          derived-data / output dir (default: target/ios-build)
#   RUN              sim: 1 = boot+launch, 0 = build only   (default: 1)
#   MESH_PORT        edge port the sim connects to          (default: 27640)
#   MESH_USER/ORG    dev identity the sim signs in with     (default: login/personal)
#   DEVELOPMENT_TEAM Apple Developer Team ID   (device/archive; required)
#   BUNDLE_ID        product bundle id override (device/archive)
#   EXPORT_METHOD    development | ad-hoc | app-store        (default: development)

set -euo pipefail

[[ "$(uname -s)" == "Darwin" ]] || { echo "build-ios.sh needs macOS + Xcode." >&2; exit 1; }
command -v xcodebuild >/dev/null 2>&1 || { echo "xcodebuild not found — install Xcode 26+." >&2; exit 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT="$ROOT/apps/ios/Zeron.xcodeproj"
SCHEME="Zeron"
CMD="${1:-sim}"
SIMULATOR="${SIMULATOR:-iPhone 17 Pro}"
DERIVED="${DERIVED:-$ROOT/target/ios-build}"
BUNDLE_ID="${BUNDLE_ID:-sh.zeron.ios}"

# Route xcodebuild through xcbeautify/xcpretty when present, preserving its exit
# code (never the formatter's) via PIPESTATUS.
xcb() {
  if command -v xcbeautify >/dev/null 2>&1; then
    set -o pipefail; xcodebuild "$@" | xcbeautify
  elif command -v xcpretty >/dev/null 2>&1; then
    set -o pipefail; xcodebuild "$@" | xcpretty
  else
    xcodebuild "$@"
  fi
}

case "$CMD" in
  sim)
    CONFIG="${CONFIG:-Debug}"
    echo "▸ building $SCHEME ($CONFIG) for Simulator: $SIMULATOR"
    xcb -project "$PROJECT" -scheme "$SCHEME" -configuration "$CONFIG" \
      -destination "platform=iOS Simulator,name=$SIMULATOR" \
      -derivedDataPath "$DERIVED" build

    if [[ "${RUN:-1}" != "1" ]]; then
      echo "✓ built (RUN=0, not launching)."; exit 0
    fi

    APP="$DERIVED/Build/Products/$CONFIG-iphonesimulator/$SCHEME.app"
    [[ -d "$APP" ]] || { echo "built app not found at $APP" >&2; exit 1; }

    MESH_PORT="${MESH_PORT:-27640}"
    MESH_USER="${MESH_USER:-$(id -un)}"
    MESH_ORG="${MESH_ORG:-personal}"
    # The simulator shares the Mac's network stack, so the local edge is reachable
    # on loopback — no Tailscale needed just to try it.
    EDGE="http://127.0.0.1:$MESH_PORT"

    echo "▸ booting simulator + launching against $EDGE (user=$MESH_USER org=$MESH_ORG)"
    xcrun simctl boot "$SIMULATOR" 2>/dev/null || true
    open -a Simulator || true
    xcrun simctl install booted "$APP"
    # -setmode/-setedge/-setuser/-setorg drive the app into dev mode; it persists
    # them, so later manual launches reconnect on their own (AppModel.restore).
    xcrun simctl launch --console-pty booted "$BUNDLE_ID" \
      -setmode dev -setedge "$EDGE" -setuser "$MESH_USER" -setorg "$MESH_ORG"
    ;;

  archive|device)
    CONFIG="${CONFIG:-Release}"
    [[ -n "${DEVELOPMENT_TEAM:-}" ]] || { echo "set DEVELOPMENT_TEAM=<your Apple Team ID> for a signed build." >&2; exit 1; }
    ARCHIVE="$DERIVED/$SCHEME.xcarchive"
    echo "▸ archiving $SCHEME ($CONFIG) team=$DEVELOPMENT_TEAM bundle=$BUNDLE_ID"
    xcb -project "$PROJECT" -scheme "$SCHEME" -configuration "$CONFIG" \
      -destination "generic/platform=iOS" \
      -archivePath "$ARCHIVE" \
      DEVELOPMENT_TEAM="$DEVELOPMENT_TEAM" \
      PRODUCT_BUNDLE_IDENTIFIER="$BUNDLE_ID" \
      CODE_SIGN_STYLE=Automatic \
      archive
    echo "✓ archive: $ARCHIVE"
    [[ "$CMD" == "archive" ]] && exit 0

    METHOD="${EXPORT_METHOD:-development}"
    OPTS="$DERIVED/ExportOptions.plist"
    cat >"$OPTS" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>method</key><string>$METHOD</string>
  <key>teamID</key><string>$DEVELOPMENT_TEAM</string>
  <key>signingStyle</key><string>automatic</string>
  <key>stripSwiftSymbols</key><true/>
  <key>destination</key><string>export</string>
</dict>
</plist>
PLIST
    echo "▸ exporting .ipa (method=$METHOD)"
    xcb -exportArchive -archivePath "$ARCHIVE" \
      -exportOptionsPlist "$OPTS" -exportPath "$DERIVED/export"
    IPA="$(/usr/bin/find "$DERIVED/export" -name '*.ipa' | head -1)"
    echo "✓ ipa: ${IPA:-$DERIVED/export}"
    cat <<EOF

  Install on a connected iPhone (iOS 17+):
      xcrun devicectl device install app --device <name-or-udid> "$IPA"
  or drag it onto the device in Xcode → Window → Devices and Simulators.

  Then connect it to your mesh: in Xcode edit the Run scheme's launch arguments
  once (Product → Scheme → Edit Scheme → Run → Arguments):
      -setmode dev  -setedge http://<mac-tailscale-ip>:${MESH_PORT:-27640}  -setuser <user>  -setorg <org>
  Run once with those; the app persists them. (scripts/local-mesh.sh prints the
  exact values for your machine.)
EOF
    ;;

  test)
    CONFIG="${CONFIG:-Debug}"
    echo "▸ testing $SCHEME on Simulator: $SIMULATOR"
    xcb -project "$PROJECT" -scheme "$SCHEME" -configuration "$CONFIG" \
      -destination "platform=iOS Simulator,name=$SIMULATOR" \
      -derivedDataPath "$DERIVED" test
    ;;

  *)
    echo "usage: scripts/build-ios.sh [sim|device|archive|test]" >&2
    exit 1
    ;;
esac
