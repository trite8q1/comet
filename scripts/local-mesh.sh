#!/usr/bin/env bash
# Local mesh: run the whole Zeron stack on one machine (your MacBook) and drive
# it from other devices — the iOS app included — over your private network
# (Tailscale, or plain LAN). No Cloudflare account, no WorkOS.
#
# It brings up two long-lived processes and wires them together in dev-auth mode
# (bearer == "user@org", mirroring scripts/e2e-smoke.sh):
#
#   1. the edge      — `wrangler dev` (Worker + Durable Objects), bound to a
#                      routable interface so the phone can reach it;
#   2. the host engine — `zeron headless`, the device that actually runs the
#                      agents. It hosts its DeviceRoom, so remote viewports
#                      (the phone) can queue commands into session docs and the
#                      host drains them.
#
# The phone joins the SAME rooms as a peer viewport and continues your sessions.
# Ctrl-C tears both down.
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
#   MESH_HOST         address advertised to the phone (default: auto-detect the
#                     Tailscale IP, else the LAN IP)
#   MESH_EDGE_SCHEME  http | https      (default: http; set https when a
#                                        `tailscale serve` TLS proxy fronts the
#                                        edge — see docs/local-mesh.md)
#   MESH_ADVERTISED_PORT  port the phone dials (default: MESH_PORT; set to 443
#                                        behind a `tailscale serve` proxy)
#   MESH_BIND_IP      wrangler --ip     (default: 0.0.0.0 — all interfaces)
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
MESH_EDGE_SCHEME="${MESH_EDGE_SCHEME:-http}"
MESH_ADVERTISED_PORT="${MESH_ADVERTISED_PORT:-$MESH_PORT}"
MESH_BIND_IP="${MESH_BIND_IP:-0.0.0.0}"

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

# ── auto-detect the address the phone should dial ──────────────────────────
tailscale_ip() {
  local ts
  ts="$(tailscale ip -4 2>/dev/null | head -1)" && [[ -n "$ts" ]] && { echo "$ts"; return; }
  local app=/Applications/Tailscale.app/Contents/MacOS/Tailscale
  [[ -x "$app" ]] && ts="$("$app" ip -4 2>/dev/null | head -1)" && [[ -n "$ts" ]] && { echo "$ts"; return; }
  return 1
}
lan_ip() {
  local ip
  ip="$(ipconfig getifaddr en0 2>/dev/null)" || true
  [[ -z "$ip" ]] && ip="$(ipconfig getifaddr en1 2>/dev/null)" || true
  [[ -z "$ip" ]] && ip="$(hostname -I 2>/dev/null | awk '{print $1}')" || true
  echo "${ip:-127.0.0.1}"
}
if [[ -z "${MESH_HOST:-}" ]]; then
  if MESH_HOST="$(tailscale_ip)"; then :; else
    MESH_HOST="$(lan_ip)"
    warn "Tailscale not detected — advertising the LAN IP ${MESH_HOST} (same-Wi-Fi only)."
  fi
fi
PHONE_EDGE_URL="${MESH_EDGE_SCHEME}://${MESH_HOST}:${MESH_ADVERTISED_PORT}"

# ── preflight ──────────────────────────────────────────────────────────────
command -v npx >/dev/null 2>&1 || die "npx/node not found — needed to run the edge (\`wrangler dev\`)."
command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust to build the engine."
[[ -d "$ROOT/edge/node_modules" ]] || warn "edge/node_modules missing — first run installs it (\`cd edge && npm ci\`)."
if [[ "$MESH_HARNESS" == "claude-code" ]] && ! command -v claude >/dev/null 2>&1; then
  warn "harness=claude-code but the \`claude\` CLI isn't on PATH — the host can't run turns until it is."
fi

# ── process lifecycle ──────────────────────────────────────────────────────
EDGE_PID="" ENGINE_PID="" LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/zeron-mesh-logs.XXXXXX")"
cleanup() {
  say "shutting down…"
  [[ -n "$ENGINE_PID" ]] && kill "$ENGINE_PID" 2>/dev/null || true
  # wrangler runs in its own process group (npx → wrangler → workerd children).
  [[ -n "$EDGE_PID" ]] && kill -- -"$EDGE_PID" 2>/dev/null || true
  sleep 1
  [[ -n "$ENGINE_PID" ]] && kill -9 "$ENGINE_PID" 2>/dev/null || true
  [[ -n "$EDGE_PID" ]] && kill -9 -- -"$EDGE_PID" 2>/dev/null || true
  rm -rf "$LOG_DIR"
}
trap cleanup EXIT INT TERM

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
  say "edge: starting wrangler dev on ${MESH_BIND_IP}:$MESH_PORT (dev auth)"
  # Monitor mode gives the job its own process group on macOS and Linux without
  # the Linux-only `setsid` (same trick as scripts/e2e-smoke.sh).
  set -m
  bash -c "cd '$ROOT/edge' && exec npx wrangler dev --ip '$MESH_BIND_IP' --port '$MESH_PORT' --var AUTH_MODE:dev" \
    >"$LOG_DIR/edge.log" 2>&1 &
  EDGE_PID=$!
  set +m
  wait_for "edge /health" 120 curl -sf -m 3 "$EDGE_LOOPBACK/health"
  say "edge: healthy"
fi

# ── 2. build the engine ────────────────────────────────────────────────────
ZERON="$ROOT/target/release/zeron"
if [[ "${SKIP_BUILD:-0}" == "1" && -x "$ZERON" ]]; then
  say "engine: reusing $ZERON (SKIP_BUILD=1)"
else
  say "engine: building release binary (first build takes a while)…"
  (cd "$ROOT" && cargo build --release -p zeron)
fi
[[ -x "$ZERON" ]] || die "engine binary not found at $ZERON"

# ── 3. host engine (headless, dev scope) ───────────────────────────────────
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

  ${D}The dev sign-in is reached via launch args (there is no dev button in the UI).${R}
  ${D}• Real device: in Xcode → Product → Scheme → Edit Scheme → Run → Arguments,${R}
  ${D}  add:  -setmode dev  -setedge ${PHONE_EDGE_URL}  -setuser ${MESH_USER}  -setorg ${MESH_ORG}${R}
  ${D}  Run once with those; the app persists them and reconnects on every launch.${R}
  ${D}• Simulator: scripts/build-ios.sh sim  (auto-launches against http://127.0.0.1:${MESH_PORT}).${R}

  ${B}Attach the desktop UI to this same engine${R} (optional):
      ZERON_IPC_PORT=${MESH_IPC_PORT} ${ZERON}
  ${D}It connects to the running host instead of embedding its own engine.${R}

  ${B}Logs${R}:  edge → $LOG_DIR/edge.log   engine → $LOG_DIR/engine.log
  ${D}Durable Object state persists under edge/.wrangler/state across restarts.${R}

  ${Y}Plain http over Tailscale not accepted by iOS?${R} Front the edge with a
  ${D}TLS proxy and re-run with https — see docs/local-mesh.md (\"Tailscale HTTPS\").${R}

  Press ${B}Ctrl-C${R} to stop the mesh.
EOF

# Keep the mesh alive; surface a crash of a process we own instead of hanging.
# (A reused external edge has no EDGE_PID — only the engine is monitored then.)
alive() {
  kill -0 "$ENGINE_PID" 2>/dev/null || return 1
  [[ -z "$EDGE_PID" ]] || kill -0 "$EDGE_PID" 2>/dev/null || return 1
  return 0
}
while alive; do sleep 2; done
warn "a mesh process exited — see logs in $LOG_DIR"
