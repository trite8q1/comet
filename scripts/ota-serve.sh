#!/usr/bin/env bash
# Over-the-air install of the Zeron iOS app over your tailnet — no cable, no
# store. The Mac serves a signed .ipa + manifest.plist over a real HTTPS URL
# (`tailscale serve` terminates TLS with a valid *.ts.net cert, which is exactly
# what iOS requires for itms-services://), and the device installs by opening an
# install link / scanning a QR. This is the only on-device install path.
#
# Requires macOS + Xcode, a paid Apple Developer account, and Tailscale with
# HTTPS certificates enabled for your tailnet (admin console → DNS → MagicDNS +
# "Enable HTTPS"). The target device's UDID must be registered in your Apple
# Developer account so the ad-hoc profile includes it (see docs/ota-install.md).
#
# Usage:
#   DEVELOPMENT_TEAM=ABCDE12345 BUNDLE_ID=de.you.zeron scripts/ota-serve.sh
#   IPA=path/to/Zeron.ipa scripts/ota-serve.sh        # serve a prebuilt ad-hoc IPA
#
# Env:
#   IPA               prebuilt ad-hoc .ipa to serve (default: build/reuse one)
#   DEVELOPMENT_TEAM  Apple Team ID (required when building the IPA)
#   BUNDLE_ID         product bundle id override (when building)
#   REBUILD=1         rebuild the IPA even if a previous export exists
#   OTA_PORT          loopback port for the static server (default: 27650)
#   MESH_HOST         override the advertised MagicDNS host (default: auto)
#   TAILSCALE_BIN     path to the tailscale CLI (default: auto-detect)

set -euo pipefail

[[ "$(uname -s)" == "Darwin" ]] || { echo "ota-serve.sh needs macOS + Xcode." >&2; exit 1; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OTA_PORT="${OTA_PORT:-27650}"

if [[ -t 1 ]]; then B="$(tput bold)"; D="$(tput dim)"; G="$(tput setaf 2)"; Y="$(tput setaf 3)"; R="$(tput sgr0)"; else B=""; D=""; G=""; Y=""; R=""; fi
say()  { printf '%s\n' "${B}▸ $*${R}"; }
warn() { printf '%s\n' "${Y}!  $*${R}" >&2; }
die()  { printf '%s\n' "${Y}✗  $*${R}" >&2; exit 1; }
urlenc() { python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$1"; }

for tool in python3 unzip plutil sips; do
  command -v "$tool" >/dev/null 2>&1 || die "required tool '$tool' not found."
done

# ── tailscale CLI + host ───────────────────────────────────────────────────
TS="${TAILSCALE_BIN:-}"
if [[ -z "$TS" ]]; then
  if command -v tailscale >/dev/null 2>&1; then TS=tailscale
  elif [[ -x /Applications/Tailscale.app/Contents/MacOS/Tailscale ]]; then
    TS=/Applications/Tailscale.app/Contents/MacOS/Tailscale
  else die "tailscale CLI not found — install Tailscale or set TAILSCALE_BIN."; fi
fi
ts_host() {
  "$TS" status --json 2>/dev/null | python3 -c \
    'import json,sys; print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))' 2>/dev/null
}
HOST="${MESH_HOST:-$(ts_host || true)}"
[[ -n "$HOST" ]] || die "couldn't determine your Tailscale MagicDNS name — is 'tailscale up' done and MagicDNS enabled?"
BASE="https://$HOST"

# ── resolve (or build) an ad-hoc IPA ───────────────────────────────────────
IPA="${IPA:-}"
if [[ -z "$IPA" ]]; then
  existing="$(ls -t "$ROOT/target/ios-build/export/"*.ipa 2>/dev/null | head -1 || true)"
  if [[ -n "$existing" && "${REBUILD:-0}" != "1" ]]; then
    IPA="$existing"; say "reusing $IPA  (REBUILD=1 to rebuild)"
  else
    [[ -n "${DEVELOPMENT_TEAM:-}" ]] || die "set DEVELOPMENT_TEAM (and usually BUNDLE_ID) to build the ad-hoc IPA, or pass IPA=<path>."
    say "building ad-hoc IPA via build-ios.sh…"
    "$ROOT/scripts/build-ios.sh" ipa
    IPA="$(ls -t "$ROOT/target/ios-build/export/"*.ipa | head -1)"
  fi
fi
[[ -f "$IPA" ]] || die "IPA not found: $IPA"

# ── stage the OTA payload ──────────────────────────────────────────────────
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/zeron-ota.XXXXXX")"
PY_PID=""
cleanup() {
  say "stopping OTA server…"
  # Remove only our HTTPS handler; never auto-`reset` (that would wipe other
  # serve config the user may have).
  if ! "$TS" serve --https=443 off >/dev/null 2>&1; then
    warn "couldn't auto-remove the serve handler — run: $TS serve reset"
  fi
  [[ -n "$PY_PID" ]] && kill "$PY_PID" 2>/dev/null || true
  rm -rf "$STAGE"
}
trap cleanup EXIT INT TERM

cp "$IPA" "$STAGE/Zeron.ipa"

# Read the shipped identifiers straight from the IPA so the manifest matches
# exactly (a bundle-id/version mismatch makes iOS reject the install). Resolve
# the top-level app's Info.plist precisely rather than globbing.
APP_PLIST="$(unzip -Z1 "$IPA" | grep -m1 -E '^Payload/[^/]+\.app/Info\.plist$' || true)"
[[ -n "$APP_PLIST" ]] || die "couldn't locate Payload/*.app/Info.plist in the IPA."
unzip -p "$IPA" "$APP_PLIST" > "$STAGE/app.plist" 2>/dev/null || die "couldn't read Info.plist from the IPA."
plx() { plutil -extract "$1" raw -o - "$STAGE/app.plist" 2>/dev/null || true; }
APP_BUNDLE_ID="$(plx CFBundleIdentifier)"; [[ -n "$APP_BUNDLE_ID" ]] || die "IPA has no CFBundleIdentifier."
APP_VERSION="$(plx CFBundleShortVersionString)"; APP_VERSION="${APP_VERSION:-1.0}"
APP_TITLE="$(plx CFBundleDisplayName)"; [[ -n "$APP_TITLE" ]] || APP_TITLE="$(plx CFBundleName)"; APP_TITLE="${APP_TITLE:-Zeron}"

# Optional install artwork (nice, not required by iOS).
ICON_SRC="$ROOT/apps/ios/Zeron/Assets.xcassets/AppIcon.appiconset/AppIcon1024.png"
IMAGE_ASSETS=""
if [[ -f "$ICON_SRC" ]]; then
  sips -z 512 512 "$ICON_SRC" --out "$STAGE/icon-512.png" >/dev/null 2>&1 || true
  sips -z 57 57 "$ICON_SRC" --out "$STAGE/icon-57.png" >/dev/null 2>&1 || true
  IMAGE_ASSETS="
        <dict><key>kind</key><string>display-image</string><key>url</key><string>$BASE/icon-57.png</string></dict>
        <dict><key>kind</key><string>full-size-image</string><key>url</key><string>$BASE/icon-512.png</string></dict>"
fi

cat >"$STAGE/manifest.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>items</key>
  <array>
    <dict>
      <key>assets</key>
      <array>
        <dict><key>kind</key><string>software-package</string><key>url</key><string>$BASE/Zeron.ipa</string></dict>$IMAGE_ASSETS
      </array>
      <key>metadata</key>
      <dict>
        <key>bundle-identifier</key><string>$APP_BUNDLE_ID</string>
        <key>bundle-version</key><string>$APP_VERSION</string>
        <key>kind</key><string>software</string>
        <key>title</key><string>$APP_TITLE</string>
      </dict>
    </dict>
  </array>
</dict>
</plist>
PLIST

ITMS="itms-services://?action=download-manifest&url=$(urlenc "$BASE/manifest.plist")"
ITMS_HTML="${ITMS//&/&amp;}" # the query separator must survive as &amp; in HTML

cat >"$STAGE/index.html" <<HTML
<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Install $APP_TITLE</title>
<style>
  :root { color-scheme: dark; }
  body { margin:0; min-height:100vh; display:grid; place-items:center;
         background:#0a0a0a; color:#ededed;
         font:16px/1.5 -apple-system,system-ui,sans-serif; }
  .card { text-align:center; padding:40px 28px; max-width:360px; }
  h1 { font-size:22px; font-weight:600; margin:16px 0 6px; }
  p { color:#9a9a9a; font-size:14px; margin:6px 0; }
  a.btn { display:block; margin-top:24px; padding:15px; border-radius:16px;
          background:#ededed; color:#0a0a0a; font-weight:600; text-decoration:none; }
  code { color:#c9c9c9; font-size:12px; word-break:break-all; }
</style></head>
<body><div class="card">
  <h1>$APP_TITLE</h1>
  <p>Version $APP_VERSION</p>
  <a class="btn" href="$ITMS_HTML">Install on this device</a>
  <p style="margin-top:20px">Open this page in <b>Safari</b>, then tap Install and
     confirm. After it installs, open $APP_TITLE and connect it to your mesh
     (scan the QR from <code>scripts/local-mesh.sh</code>).</p>
</div></body></html>
HTML

# ── serve it: local static server (correct MIME) + tailscale HTTPS proxy ────
cat >"$STAGE/serve.py" <<'PY'
import http.server, socketserver, sys
port, directory = int(sys.argv[1]), sys.argv[2]
class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k): super().__init__(*a, directory=directory, **k)
    def guess_type(self, path):
        if path.endswith(".plist"): return "text/xml"
        if path.endswith(".ipa"): return "application/octet-stream"
        return super().guess_type(path)
    def log_message(self, *a): pass
with socketserver.TCPServer(("127.0.0.1", port), H) as s:
    s.serve_forever()
PY

python3 "$STAGE/serve.py" "$OTA_PORT" "$STAGE" &
PY_PID=$!
for _ in $(seq 1 20); do
  curl -sf -m 2 "http://127.0.0.1:$OTA_PORT/manifest.plist" >/dev/null 2>&1 && break
  sleep 0.5
done

say "exposing over HTTPS: $BASE  →  127.0.0.1:$OTA_PORT"
if ! "$TS" serve --bg --https=443 "http://127.0.0.1:$OTA_PORT" 2>"$STAGE/serve.err"; then
  sed 's/^/    /' "$STAGE/serve.err" >&2 || true
  die "tailscale serve failed — enable HTTPS certificates for your tailnet (admin console → DNS → MagicDNS + Enable HTTPS)."
fi

# Soft reachability probe (name/cert propagation can lag a second or two).
curl -sf -m 5 "$BASE/manifest.plist" >/dev/null 2>&1 \
  || warn "the public URL isn't answering yet — give it a few seconds, then reload on the device."

cat <<EOF

${G}${B}  Over-the-air install is live.${R}

  On the ${B}device${R} (must be on the same tailnet — Tailscale app connected):
    1. Open ${B}$BASE${R} in ${B}Safari${R}  (scan the QR below).
    2. Tap ${B}Install on this device${R}, then confirm on the Home Screen.

  ${D}Direct install link (Safari only):${R}
    ${D}$ITMS${R}

  App: ${B}$APP_TITLE${R}  bundle ${B}$APP_BUNDLE_ID${R}  v${B}$APP_VERSION${R}
  ${D}The device's UDID must be registered in your Apple account, or install${R}
  ${D}fails with "cannot be installed at this time" — see docs/ota-install.md.${R}

  Press ${B}Ctrl-C${R} to stop serving.
EOF

if command -v qrencode >/dev/null 2>&1; then
  qrencode -t ANSIUTF8 "$BASE/" | sed 's/^/    /'
  echo
else
  printf '  %s\n\n' "${D}(install qrencode for a scannable QR of $BASE — brew install qrencode)${R}"
fi

# Block until the static server dies or the user interrupts.
while [[ -n "$PY_PID" ]] && kill -0 "$PY_PID" 2>/dev/null; do sleep 2; done
warn "static server exited unexpectedly."
