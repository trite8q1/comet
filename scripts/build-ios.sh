#!/usr/bin/env bash
# Build the Zeron iOS app (apps/ios). Requires macOS with Xcode 26+ (iOS 26 SDK
# — the app uses Liquid Glass APIs). A thin wrapper around `xcodebuild`; for
# day-to-day work you can still just open Zeron.xcodeproj.
#
# On-device install is over-the-air only (no cable): this produces the ad-hoc
# .ipa, scripts/ota-serve.sh serves it over your tailnet. See docs/ota-install.md.
#
# Modes:
#   ipa   Archive + export an ad-hoc .ipa for OTA install (default). Needs your
#         own Apple Team; pick a unique bundle id (the default sh.zeron.ios
#         belongs to the upstream project). → target/ios-build/export/*.ipa
#   sim   Build and launch in the iOS Simulator against your local mesh edge in
#         dev mode (no signing) — the fast on-Mac smoke test.
#
# Usage:
#   DEVELOPMENT_TEAM=ABCDE12345 BUNDLE_ID=de.you.zeron scripts/build-ios.sh ipa
#   scripts/build-ios.sh sim
#
# Env:
#   DEVELOPMENT_TEAM  Apple Developer Team ID   (ipa: required)
#   BUNDLE_ID         product bundle id          (default: sh.zeron.ios)
#   CONFIG            Debug | Release            (default: Release; sim: Debug)
#   DERIVED           derived-data / output dir  (default: target/ios-build)
#   EXPORT_METHOD     ipa export method          (default: release-testing, the
#                     Xcode 15.3+ name for ad-hoc; older Xcode uses "ad-hoc")
#   SIMULATOR         simulator device name      (sim; default: iPhone 17 Pro)
#   RUN               sim: 1 = boot+launch, 0 = build only  (default: 1)
#   MESH_PORT/USER/ORG  sim dev-mode target      (defaults: 27640 / login / personal)

set -euo pipefail

[[ "$(uname -s)" == "Darwin" ]] || { echo "build-ios.sh needs macOS + Xcode." >&2; exit 1; }
command -v xcodebuild >/dev/null 2>&1 || { echo "xcodebuild not found — install Xcode 26+." >&2; exit 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROJECT="$ROOT/apps/ios/Zeron.xcodeproj"
SCHEME="Zeron"
CMD="${1:-ipa}"
DERIVED="${DERIVED:-$ROOT/target/ios-build}"
BUNDLE_ID="${BUNDLE_ID:-sh.zeron.ios}"

# Route xcodebuild through xcbeautify/xcpretty when present, preserving its exit
# code (never the formatter's) via pipefail.
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
  ipa)
    CONFIG="${CONFIG:-Release}"
    [[ -n "${DEVELOPMENT_TEAM:-}" ]] || { echo "set DEVELOPMENT_TEAM=<your Apple Team ID> to sign the IPA." >&2; exit 1; }
    ARCHIVE="$DERIVED/$SCHEME.xcarchive"
    echo "▸ archiving $SCHEME ($CONFIG) team=$DEVELOPMENT_TEAM bundle=$BUNDLE_ID"
    xcb -project "$PROJECT" -scheme "$SCHEME" -configuration "$CONFIG" \
      -destination "generic/platform=iOS" \
      -archivePath "$ARCHIVE" \
      DEVELOPMENT_TEAM="$DEVELOPMENT_TEAM" \
      PRODUCT_BUNDLE_IDENTIFIER="$BUNDLE_ID" \
      CODE_SIGN_STYLE=Automatic \
      archive

    METHOD="${EXPORT_METHOD:-release-testing}"
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
    echo "▸ exporting ad-hoc .ipa (method=$METHOD)"
    xcb -exportArchive -archivePath "$ARCHIVE" \
      -exportOptionsPlist "$OPTS" -exportPath "$DERIVED/export"
    IPA="$(/usr/bin/find "$DERIVED/export" -name '*.ipa' | head -1)"
    echo "✓ ipa: ${IPA:-$DERIVED/export}"
    echo "  next: scripts/ota-serve.sh   (serve it over Tailscale; see docs/ota-install.md)"
    ;;

  sim)
    CONFIG="${CONFIG:-Debug}"
    SIMULATOR="${SIMULATOR:-iPhone 17 Pro}"
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

  *)
    echo "usage: scripts/build-ios.sh [ipa|sim]" >&2
    exit 1
    ;;
esac
