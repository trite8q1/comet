#!/usr/bin/env bash
# Local mesh: run the whole Zeron stack on one Mac and drive it from the iOS app
# over HTTPS on your tailnet. `tailscale serve` terminates TLS and proxies to the
# loopback-only local edge. No Cloudflare account, no WorkOS.
#
# It brings up two long-lived processes and wires them together in dev-auth mode
# (bearer == "user@org", mirroring scripts/e2e-smoke.sh):
#
#   1. the edge      — `wrangler dev` (Worker + Durable Objects), bound only to
#                      loopback and exposed through `tailscale serve` HTTPS;
#   2. the host engine — `zeron headless`, the device that actually runs the
#                      agents. It hosts its DeviceRoom, so remote viewports
#                      (the phone) can queue commands into session docs and the
#                      host drains them.
#
# The phone joins the SAME rooms as a peer viewport and continues your sessions.
# Ctrl-C tears the processes and Serve handler down.
#
# Usage: scripts/local-mesh.sh
#
# Env (all optional — sensible defaults):
#   MESH_USER         dev user id       (default: your macOS login name)
#   MESH_ORG          dev org id        (default: personal)
#   MESH_HARNESS      default harness   (default: claude-code; e.g. codex, mock)
#   MESH_PORT         edge port         (default: 27640)
#   MESH_IPC_PORT     engine IPC port   (default: 27654)
#   MESH_DATA_DIR     engine data dir   (default: ~/.zeron-mesh — isolated from
#                                        your normal ~/.zeron local profile)
#   MESH_DEVICE_NAME  host device name  (default: <hostname>-mesh)
#   TAILSCALE_BIN     path to the tailscale CLI (default: auto-detect)
#   SKIP_BUILD=1      reuse an existing target/release/zeron instead of building

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"

MESH_USER="${MESH_USER:-$(id -un)}"
MESH_ORG="${MESH_ORG:-personal}"
MESH_HARNESS="${MESH_HARNESS:-claude-code}"
MESH_PORT="${MESH_PORT:-27640}"
MESH_IPC_PORT="${MESH_IPC_PORT:-27654}"
MESH_DATA_DIR="${MESH_DATA_DIR:-$HOME/.zeron-mesh}"
MESH_DEVICE_NAME="${MESH_DEVICE_NAME:-$(hostname -s 2>/dev/null || hostname)-mesh}"

# Dev-auth identity shared by the engine and the phone. The bearer IS the
# "user@org" string (edge AUTH_MODE=dev); both sides must present the same pair
# to land in the same registry/chat rooms.
BEARER="${MESH_USER}@${MESH_ORG}"
EDGE_LOOPBACK="http://127.0.0.1:${MESH_PORT}" # the host engine talks to the edge locally

# ── pretty output ──────────────────────────────────────────────────────────
if [[ -t 1 ]]; then B="$(tput bold)"; D="$(tput dim)"; G="$(tput setaf 2)"; Y="$(tput setaf 3)"; R="$(tput sgr0)"; else B=""; D=""; G=""; Y=""; R=""; fi
say()  { printf '%s\n' "${B}▸ $*${R}"; }
warn() { printf '%s\n' "${Y}!  $*${R}" >&2; }
die()  { printf '%s\n' "${Y}✗  $*${R}" >&2; exit 1; }
urlenc() { python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1], safe=""))' "$1"; }

# ── tailscale HTTPS endpoint the phone should dial ─────────────────────────
command -v python3 >/dev/null 2>&1 || die "python3 not found — needed to read Tailscale status."
TS="${TAILSCALE_BIN:-}"
if [[ -z "$TS" ]]; then
  if command -v tailscale >/dev/null 2>&1; then TS=tailscale
  elif [[ -x /Applications/Tailscale.app/Contents/MacOS/Tailscale ]]; then
    TS=/Applications/Tailscale.app/Contents/MacOS/Tailscale
  else die "tailscale CLI not found — install Tailscale or set TAILSCALE_BIN."; fi
fi
TS_STATUS="$("$TS" status --json 2>/dev/null)" || die "couldn't read Tailscale status — is Tailscale running?"
TS_STATE="$(printf '%s' "$TS_STATUS" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("BackendState", ""))')"
[[ "$TS_STATE" == "Running" ]] || die "Tailscale is not connected (state: ${TS_STATE:-unknown})."
MESH_HOST="$(printf '%s' "$TS_STATUS" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("Self", {}).get("DNSName", "").rstrip("."))')"
[[ -n "$MESH_HOST" ]] || die "couldn't determine the Tailscale MagicDNS name — enable MagicDNS for the tailnet."
PHONE_EDGE_URL="https://${MESH_HOST}"
# One-tap dev sign-in for the app (handled by AppModel.handleDeepLink). Scan the
# QR with the Camera app, or open the link on the device — no typing, no cable.
DEEPLINK="zeron://dev?edge=$(urlenc "$PHONE_EDGE_URL")&user=$(urlenc "$MESH_USER")&org=$(urlenc "$MESH_ORG")"

# ── preflight ──────────────────────────────────────────────────────────────
command -v npx >/dev/null 2>&1 || die "npx/node not found — needed to run the edge (\`wrangler dev\`)."
command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust to build the engine."
command -v curl >/dev/null 2>&1 || die "curl not found — needed to check the edge."
[[ -d "$ROOT/edge/node_modules" ]] || warn "edge/node_modules missing — first run installs it (\`cd edge && npm ci\`)."
if [[ "$MESH_HARNESS" == "claude-code" ]] && ! command -v claude >/dev/null 2>&1; then
  warn "harness=claude-code but the \`claude\` CLI isn't on PATH — the host can't run turns until it is."
fi
SERVE_STATUS="$("$TS" serve status --json 2>&1)" || die "couldn't read the current tailscale serve configuration."
if [[ "$SERVE_STATUS" == *"Serve is not enabled"* ]]; then
  printf '%s\n' "$SERVE_STATUS" >&2
  die "Tailscale Serve must be enabled for this device before starting the mesh."
fi
if ! printf '%s' "$SERVE_STATUS" | python3 -c '
import json, sys
data = json.load(sys.stdin)
tcp = data.get("TCP", {})
web = data.get("Web", {})
busy = "443" in tcp or any(key.endswith(":443") for key in web)
raise SystemExit(1 if busy else 0)
'; then
  die "tailscale serve HTTPS port 443 is already in use — stop the existing handler (including ota-serve.sh) before starting the mesh."
fi

# ── process lifecycle ──────────────────────────────────────────────────────
EDGE_PID="" ENGINE_PID="" SERVE_CMD_PID="" SERVE_ACTIVE=0 LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/zeron-mesh-logs.XXXXXX")"
cleanup() {
  say "shutting down…"
  [[ -n "$SERVE_CMD_PID" ]] && kill "$SERVE_CMD_PID" 2>/dev/null || true
  if [[ "$SERVE_ACTIVE" == "1" ]]; then
    "$TS" serve --https=443 off >/dev/null 2>&1 \
      || warn "couldn't remove the tailscale serve handler — run: $TS serve --https=443 off"
  fi
  [[ -n "$ENGINE_PID" ]] && kill "$ENGINE_PID" 2>/dev/null || true
  # wrangler runs in its own process group (npx → wrangler → workerd children).
  [[ -n "$EDGE_PID" ]] && kill -- -"$EDGE_PID" 2>/dev/null || true
  sleep 1
  [[ -n "$ENGINE_PID" ]] && kill -9 "$ENGINE_PID" 2>/dev/null || true
  [[ -n "$EDGE_PID" ]] && kill -9 -- -"$EDGE_PID" 2>/dev/null || true
  rm -rf "$LOG_DIR"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

wait_for() { # wait_for <desc> <timeout_s> <cmd...>
  local what="$1" timeout="$2"; shift 2 || true
  local waited=0
  until "$@" >/dev/null 2>&1; do
    sleep 1; waited=$((waited + 1))
    [[ "$waited" -ge "$timeout" ]] && die "timed out waiting for $what (see $LOG_DIR)"
  done
}

# ── 1. edge (wrangler dev, dev auth) ───────────────────────────────────────
if curl -sf -m 3 "$EDGE_LOOPBACK/health" 2>/dev/null | grep -q '"auth":"dev"'; then
  say "edge: reusing healthy dev worker on :$MESH_PORT"
else
  say "edge: starting wrangler dev on loopback :$MESH_PORT (dev auth)"
  # Monitor mode gives the job its own process group on macOS and Linux without
  # the Linux-only `setsid` (same trick as scripts/e2e-smoke.sh).
  set -m
  bash -c "cd '$ROOT/edge' && exec npx wrangler dev --ip 127.0.0.1 --port '$MESH_PORT' --var AUTH_MODE:dev" \
    >"$LOG_DIR/edge.log" 2>&1 &
  EDGE_PID=$!
  set +m
  wait_for "edge /health" 120 curl -sf -m 3 "$EDGE_LOOPBACK/health"
  say "edge: healthy"
fi

# ── 2. expose the edge over tailscale HTTPS ────────────────────────────────
say "edge: exposing $EDGE_LOOPBACK at $PHONE_EDGE_URL"
"$TS" serve --bg --https=443 "$EDGE_LOOPBACK" >"$LOG_DIR/serve.log" 2>&1 &
SERVE_CMD_PID=$!
for _ in $(seq 1 40); do
  if grep -q "Serve is not enabled" "$LOG_DIR/serve.log" 2>/dev/null; then
    sed 's/^/    /' "$LOG_DIR/serve.log" >&2 || true
    kill "$SERVE_CMD_PID" 2>/dev/null || true
    wait "$SERVE_CMD_PID" 2>/dev/null || true
    SERVE_CMD_PID=""
    die "Tailscale Serve must be enabled for this device before starting the mesh."
  fi
  kill -0 "$SERVE_CMD_PID" 2>/dev/null || break
  sleep 0.25
done
if kill -0 "$SERVE_CMD_PID" 2>/dev/null; then
  sed 's/^/    /' "$LOG_DIR/serve.log" >&2 || true
  kill "$SERVE_CMD_PID" 2>/dev/null || true
  wait "$SERVE_CMD_PID" 2>/dev/null || true
  SERVE_CMD_PID=""
  die "tailscale serve did not finish configuring HTTPS within ten seconds."
fi
if ! wait "$SERVE_CMD_PID"; then
  SERVE_CMD_PID=""
  sed 's/^/    /' "$LOG_DIR/serve.log" >&2 || true
  "$TS" serve --https=443 off >/dev/null 2>&1 || true
  die "tailscale serve failed — enable MagicDNS and HTTPS certificates for the tailnet."
fi
SERVE_CMD_PID=""
SERVE_ACTIVE=1
curl -sf -m 5 "$PHONE_EDGE_URL/health" >/dev/null 2>&1 \
  || warn "the HTTPS edge isn't answering yet — certificate or MagicDNS propagation may take a few seconds."

# ── 3. build the engine ────────────────────────────────────────────────────
ZERON="$ROOT/target/release/zeron"
if [[ "${SKIP_BUILD:-0}" == "1" && -x "$ZERON" ]]; then
  say "engine: reusing $ZERON (SKIP_BUILD=1)"
else
  say "engine: building release binary (first build takes a while)…"
  (cd "$ROOT" && cargo build --release -p zeron)
fi
[[ -x "$ZERON" ]] || die "engine binary not found at $ZERON"

# ── 4. host engine (headless, dev scope) ───────────────────────────────────
mkdir -p "$MESH_DATA_DIR"
say "engine: starting host device '$MESH_DEVICE_NAME' (data: $MESH_DATA_DIR)"
ZERON_DATA_DIR="$MESH_DATA_DIR" \
ZERON_IPC_PORT="$MESH_IPC_PORT" \
ZERON_DEVICE_NAME="$MESH_DEVICE_NAME" \
ZERON_EDGE_URL="$EDGE_LOOPBACK" \
ZERON_EDGE_TOKEN="$BEARER" \
ZERON_ORG_ID="$MESH_ORG" \
ZERON_HARNESS="$MESH_HARNESS" \
RUST_LOG="${RUST_LOG:-info}" \
  "$ZERON" headless >"$LOG_DIR/engine.log" 2>&1 &
ENGINE_PID=$!

wait_for "engine IPC :$MESH_IPC_PORT" 60 bash -c "exec 3<>/dev/tcp/127.0.0.1/$MESH_IPC_PORT"
say "engine: up (IPC :$MESH_IPC_PORT)"

# ── connection panel ───────────────────────────────────────────────────────
cat <<EOF

${G}${B}  Local mesh is up.${R}

  ${B}Connect the Zeron iOS app${R} (dev mode) with:
      Edge URL : ${B}${PHONE_EDGE_URL}${R}
      User id  : ${B}${MESH_USER}${R}
      Org id   : ${B}${MESH_ORG}${R}

  ${B}Point the app at this mesh${R} (works on the OTA-installed app):
  ${D}• Scan the QR below with the Camera app, or open this link on the device:${R}
      ${B}${DEEPLINK}${R}
  ${D}• Or in the app tap "Use a self-hosted server" and enter the three values.${R}
  ${D}• Or, when running in the Simulator, pass the launch args instead:${R}
  ${D}    -setmode dev  -setedge ${PHONE_EDGE_URL}  -setuser ${MESH_USER}  -setorg ${MESH_ORG}${R}
  ${D}  (scripts/build-ios.sh sim wires these up automatically against loopback.)${R}

  ${B}Attach the desktop UI to this same engine${R} (optional):
      ZERON_IPC_PORT=${MESH_IPC_PORT} ${ZERON}
  ${D}It connects to the running host instead of embedding its own engine.${R}

  ${B}Logs${R}:  edge → $LOG_DIR/edge.log   engine → $LOG_DIR/engine.log
  ${D}Runtime route: $PHONE_EDGE_URL → tailscale serve → $EDGE_LOOPBACK${R}
  ${D}Durable Object state persists under edge/.wrangler/state across restarts.${R}

  Press ${B}Ctrl-C${R} to stop the mesh and its tailscale serve handler.
EOF

if command -v qrencode >/dev/null 2>&1; then
  qrencode -t ANSIUTF8 "$DEEPLINK" | sed 's/^/    /'
  echo
else
  printf '  %s\n\n' "${D}(install qrencode for a scannable QR of the link above: brew install qrencode)${R}"
fi

# Keep the mesh alive; surface a crash of a process we own instead of hanging.
# (A reused external edge has no EDGE_PID — only the engine is monitored then.)
alive() {
  kill -0 "$ENGINE_PID" 2>/dev/null || return 1
  [[ -z "$EDGE_PID" ]] || kill -0 "$EDGE_PID" 2>/dev/null || return 1
  return 0
}
while alive; do sleep 2; done
warn "a mesh process exited — see logs in $LOG_DIR"
