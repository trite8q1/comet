#!/usr/bin/env bash
# One-command demo: boots a seeded engine daemon + the headed app, offline.
# Made for judging look & feel with real input — no edge, no auth needed.
#
#   scripts/dev-demo.sh            # build, seed demo data, open the app
#   scripts/dev-demo.sh --slow     # pace mock streams (~10s) to watch streaming
#   scripts/dev-demo.sh --reset    # wipe the demo state and reseed from scratch
#
# Everything lives under /tmp/comet-demo-*; re-runs reuse it. Ctrl-C cleans up.
#
# The seed is a small, realistic workspace: three devices (this machine plus a
# "Mac Studio" and a "Cloud VPS"), the same repo alive on more than one of
# them, chats spread from minutes to weeks old, two live mock runs, a few
# archived chats, and a couple of project-less scratch chats. Every row is
# real data in the daemon, so each chat can be opened, archived, unarchived,
# and renamed exactly like production data.
set -euo pipefail
cd "$(dirname "$0")/.."

DAEMON_DIR=/tmp/comet-demo-daemon
UI_DIR=/tmp/comet-demo-ui
IPC=27921
DELAY=""
for arg in "$@"; do
  case "$arg" in
    --slow) DELAY=350 ;;
    --reset) rm -rf "$DAEMON_DIR" "$UI_DIR" ;;
    *) echo "unknown flag: $arg (expected --slow or --reset)" >&2; exit 2 ;;
  esac
done

echo "▸ building (first run takes a few minutes)…"
cargo build -p comet -q

echo "▸ starting engine daemon on :$IPC"
env COMET_DATA_DIR="$DAEMON_DIR" COMET_IPC_PORT=$IPC COMET_HARNESS=mock \
  ${DELAY:+COMET_MOCK_DELAY_MS=$DELAY} RUST_LOG=warn \
  ./target/debug/comet headless &
DAEMON_PID=$!
trap 'kill $DAEMON_PID 2>/dev/null || true' EXIT
for _ in $(seq 1 40); do
  (exec 3<>/dev/tcp/127.0.0.1/$IPC) 2>/dev/null && { exec 3>&-; break; }
  sleep 0.25
done

probe() { cargo run -q -p comet-rpc --example rpc_probe -- "ws://127.0.0.1:$IPC" "$@"; }
mutate() { probe Mutate "$1" >/dev/null; }
new_id() { uuidgen | tr 'A-Z' 'a-z'; }

if [[ ! -f "$DAEMON_DIR/.demo-seeded" ]]; then
  echo "▸ seeding demo workspace (about a minute — every row is a real mutation)"
  LOCAL=$(probe LocalDevice '{}' | python3 -c 'import json,sys;print(json.load(sys.stdin)["deviceId"])')

  # Two foreign devices. They never heartbeat, so they read as offline — a
  # laptop that is closed and a server that is idle, which is realistic.
  STUDIO=demo-mac-studio
  VPS=demo-cloud-vps
  mutate "{\"op\":\"upsertDevice\",\"deviceId\":\"$STUDIO\",\"name\":\"Mac Studio\",\"platform\":\"macos\"}"
  mutate "{\"op\":\"upsertDevice\",\"deviceId\":\"$VPS\",\"name\":\"Cloud VPS\",\"platform\":\"linux\"}"

  # space <device-id> <path>  → prints the new space id. Same folder name on
  # several devices = the same project alive on several machines.
  space() {
    local sid; sid=$(new_id)
    mutate "{\"op\":\"createSpace\",\"spaceId\":\"$sid\",\"deviceId\":\"$1\",\"path\":\"$2\",\"gitDetected\":true}"
    echo "$sid"
  }
  S_COMET=$(space "$LOCAL" "$HOME/github/comet")
  S_SOCCER=$(space "$LOCAL" "$HOME/github/soccertcg")
  S_AETHER=$(space "$LOCAL" "$HOME/github/aether")
  S_STUDIO_COMET=$(space "$STUDIO" "/Users/nico/dev/comet")
  S_STUDIO_DESIGN=$(space "$STUDIO" "/Users/nico/dev/design-system")
  S_VPS_COMET=$(space "$VPS" "/srv/comet")
  S_VPS_MAPS=$(space "$VPS" "/srv/gonzocity-maps")
  # Each probe is a fresh one-shot connection; let the space writes commit
  # before chats reference them, or createChat races to "no such space".
  sleep 1

  NOW=$(date +%s)
  # chat <space-id|-> <harness> <branch> <age-minutes> <action> <title>
  #   space-id "-"  → project-less scratch chat hosted on this machine
  #   age-minutes   → minutes since the last message (created earlier)
  #   action        → idle | run (live mock stream) | archive
  # Archived chats are stamped in call order, so archive the oldest first: the
  # most recently put-away chat sits on top of the archived shelf.
  chat() {
    local space=$1 harness=$2 branch=$3 age=$4 action=$5 title=$6
    local id; id=$(new_id)
    local config="{\"harness\":\"$harness\",\"model\":null,\"reasoning\":null,\"sandbox\":\"workspace-write\"}"
    if [[ $space == - ]]; then
      mutate "{\"op\":\"createChat\",\"chatId\":\"$id\",\"deviceId\":\"$LOCAL\",\"config\":$config}"
    else
      mutate "{\"op\":\"createChat\",\"chatId\":\"$id\",\"spaceId\":\"$space\",\"config\":$config}"
    fi
    mutate "{\"op\":\"renameChat\",\"chatId\":\"$id\",\"title\":\"$title\"}"
    mutate "{\"op\":\"setChatBranch\",\"chatId\":\"$id\",\"branch\":\"$branch\"}"
    if [[ $action == run ]]; then
      probe QueueCommand "{\"chatId\":\"$id\",\"command\":{\"kind\":\"run\",\"messageId\":\"$(uuidgen)\",\"request\":{\"prompt\":\"Walk me through the streaming pipeline\",\"model\":null,\"reasoning\":null,\"modelOptions\":{},\"cwd\":\"/tmp\",\"sandbox\":\"workspace-write\",\"autoApprove\":true,\"resume\":null}}}" >/dev/null
      sleep 1
    fi
    # Fresh chats were created half an hour before their last message; older
    # ones a few days before.
    local last=$(( NOW - age * 60 )) created
    if (( age < 120 )); then created=$(( last - 1800 )); else created=$(( last - 3 * 24 * 3600 )); fi
    mutate "{\"op\":\"setChatActivity\",\"chatId\":\"$id\",\"lastMessageAt\":$(( last * 1000 )),\"createdAt\":$(( created * 1000 ))}"
    if [[ $action == archive ]]; then
      mutate "{\"op\":\"setChatArchived\",\"chatId\":\"$id\",\"archived\":true}"
    fi
    printf '.'
  }

  # ── this machine ────────────────────────────────────────────────────────
  chat "$S_COMET"  mock        comet/main                  0     run     "Native Comet Rust Rewrite"
  chat "$S_COMET"  claude-code feat/sidebar-folders        25    idle    "Project folders in the sidebar"
  chat "$S_COMET"  codex       fix/scroll-fade-flicker     180   idle    "Fix scroll-fade flicker on archive"
  chat "$S_COMET"  claude-code chore/deps-2026-08          2880  idle    "Bump workspace deps"
  chat "$S_SOCCER" mock        comet/rebalance-stat-caps   0     run     "Rebalance player stat caps"
  chat "$S_SOCCER" cursor      feat/premium-tcg            90    idle    "Craft premium TCG experience"
  chat "$S_SOCCER" claude-code fix/pack-odds               1560  idle    "Pack odds off by one"
  chat "$S_AETHER" codex       aether/main                 4320  idle    "Repo bootstrap and CI"
  chat "$S_AETHER" claude-code feat/auth                   10080 idle    "Auth flow spike"
  chat -           claude-code main                        400   idle    "Scratch: env probing"

  # ── Mac Studio ──────────────────────────────────────────────────────────
  chat "$S_STUDIO_COMET"  claude-code feat/metal-renderer   60    idle "Port renderer to Metal"
  chat "$S_STUDIO_COMET"  codex       fix/gpui-leak         360   idle "Fix GPUI memory leak"
  chat "$S_STUDIO_COMET"  claude-code perf/startup          1320  idle "Profile cold-start time"
  chat "$S_STUDIO_DESIGN" claude-code feat/token-pipeline   200   idle "Token pipeline overhaul"
  chat "$S_STUDIO_DESIGN" cursor      chore/dark-mode-audit 1680  idle "Dark mode contrast audit"

  # ── Cloud VPS ───────────────────────────────────────────────────────────
  chat "$S_VPS_COMET" codex       ci/nightly       120  idle "Nightly release pipeline"
  chat "$S_VPS_COMET" claude-code fix/flaky-sync   540  idle "Reproduce flaky sync test"
  chat "$S_VPS_MAPS"  claude-code feat/gpx-import  240  idle "GPX import support"
  chat "$S_VPS_MAPS"  codex       deploy/staging   1980 idle "Staging deploy"
  chat "$S_VPS_MAPS"  claude-code perf/tile-cache  3120 idle "Tile cache warmup"

  # ── archived (oldest first; the last one lands on top of the shelf) ─────
  chat "$S_AETHER"       claude-code spike/graphql      20160 archive "GraphQL gateway spike"
  chat -                 claude-code main               17280 archive "Scratch: harness flags"
  chat "$S_VPS_MAPS"     cursor      proto/vector-tiles 15840 archive "Prototype vector tiles"
  chat "$S_STUDIO_COMET" claude-code spike/renderer     12000 archive "Old renderer spike"
  chat "$S_COMET"        codex       research/sync-libs 8640  archive "Evaluate sync libraries"
  echo
  touch "$DAEMON_DIR/.demo-seeded"
fi

echo "▸ opening comet (composer is live — type into it; --slow shows streaming)"
COMET_DATA_DIR="$UI_DIR" COMET_IPC_PORT=$IPC RUST_LOG=warn ./target/debug/comet
