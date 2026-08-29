# Running the whole stack locally (Mac + iPhone over Tailscale)

Run every part of Zeron on one Mac and continue your sessions from the iOS app
over HTTPS on your tailnet. No Cloudflare account, no WorkOS. The edge runs
locally in **dev auth** (bearer == `user@org`), exactly like `scripts/e2e-smoke.sh`.

```
 iPhone (Zeron.app, dev mode)                           MacBook
        │                                    ┌────────────────────────────────┐
        │  HTTPS / WSS                       │  tailscale serve :443          │
        └──────────────►  *.ts.net  ────────►│       │                        │
                                             │       ▼ loopback               │
                                             │  wrangler dev :27640 ── DOs    │
                                             │       ▲                        │
                                             │       │ loopback               │
                                             │  zeron headless ── claude CLI  │
                                             └────────────────────────────────┘
```

The phone is a **viewport**: it queues commands into the session docs; the Mac
host device drains them and runs the agent. No engine runs on the phone.

## 0. Prerequisites (once)

- **Rust** (`cargo`) — builds the engine/desktop binary.
- **Node** — runs the edge: `cd edge && npm ci`.
- **Xcode 26+** (iOS 26 SDK) — builds the iOS app.
- **Tailscale** on both the Mac and the iPhone, on the same tailnet. Enable
  **MagicDNS**, **HTTPS Certificates**, and Tailscale Serve for the Mac. If Serve
  still needs approval, the script prints the tailnet authorization URL.
- **`claude` CLI** signed in on the Mac (for the default `claude-code` harness).

## 1. Bring up the mesh

```sh
scripts/local-mesh.sh
```

First run builds the release engine (a few minutes), starts the loopback-only
edge and a headless host device, then exposes the edge at the Mac's trusted
`https://<host>.<tailnet>.ts.net` URL with `tailscale serve`. It prints the exact
**Edge URL / User id / Org id** to enter on the phone. Leave it running;
`Ctrl-C` tears down the processes and the Serve handler. Durable Object state
persists under `edge/.wrangler/state`, so your sessions survive restarts.

Useful overrides:

```sh
MESH_USER=nico MESH_ORG=personal MESH_HARNESS=claude-code scripts/local-mesh.sh
SKIP_BUILD=1 scripts/local-mesh.sh          # reuse target/release/zeron
TAILSCALE_BIN=/path/to/tailscale scripts/local-mesh.sh
```

It uses a dedicated data dir (`~/.zeron-mesh`) so it never touches your normal
local-only `~/.zeron` profile.

## 2. The desktop UI (optional)

The headless host already runs the agents; the desktop app is just another
viewport. Attach it to the *same* running engine (rather than embedding its own)
by matching the IPC port:

```sh
ZERON_IPC_PORT=27654 target/release/zeron
```

Create your spaces/sessions here (or on the phone) — they live in this dev
profile and show up on every peer.

For a full production desktop app bundle (`Zeron.app` + `.dmg`):

```sh
scripts/package-macos.sh
# signed + notarized (removes the Gatekeeper prompt):
CODESIGN_IDENTITY="Developer ID Application: … (TEAMID)" \
NOTARY_KEY_PATH=AuthKey.p8 NOTARY_KEY_ID=XXXX NOTARY_ISSUER_ID=… \
  scripts/package-macos.sh
```

## 3. Run the engine as a background service (optional)

Instead of keeping `local-mesh.sh` in a terminal, install the engine as a
launchd service that captures the same env and restarts on login:

```sh
ZERON_EDGE_URL=http://127.0.0.1:27640 \
ZERON_EDGE_TOKEN=nico@personal ZERON_ORG_ID=personal \
ZERON_HARNESS=claude-code ZERON_DATA_DIR=$HOME/.zeron-mesh \
  target/release/zeron daemon install     # start | stop | restart | status | uninstall
```

(You'd still run the edge separately — via `local-mesh.sh` or your own
`wrangler dev`.)

## 4. Build & connect the iOS app

**Fastest check — Simulator** (no signing, connects to the local edge on
loopback automatically):

```sh
scripts/build-ios.sh sim
```

**Real iPhone/iPad** — installed **over the air** (no cable): build the ad-hoc
IPA and serve it over your tailnet. Needs your own Apple Team (the bundled
`sh.zeron.ios` belongs to the upstream project, so pick a unique bundle id):

```sh
DEVELOPMENT_TEAM=YOURTEAMID BUNDLE_ID=de.you.zeron scripts/ota-serve.sh
```

The installer and runtime both use Tailscale HTTPS port 443. Stop
`local-mesh.sh` before running the installer, stop the installer after the app
is installed, then restart `local-mesh.sh`. Mesh state persists across the
restart.

See [`ota-install.md`](ota-install.md) for prerequisites (tailnet HTTPS, UDID
registration) and the full walkthrough.

**Point the app at your mesh** — three ways, no launch args required:

- **Scan the QR / open the link** that `local-mesh.sh` prints
  (`zeron://dev?edge=…&user=…&org=…`) — handled by `AppModel.handleDeepLink`.
- Tap **“Use a self-hosted server”** on the sign-in screen and enter Edge URL,
  User id, Org id by hand.
- When launching from **Xcode/Simulator**, pass the launch args instead
  (*Product → Scheme → Edit Scheme → Run → Arguments*), which the app persists:
  `-setmode dev  -setedge https://<host>.<tailnet>.ts.net  -setuser <user>  -setorg <org>`.

`local-mesh.sh` prints the precise values (and the QR) for your machine. The app
uses `wss://` for live room traffic and its HTTPS pull fallback when a network
strips WebSocket upgrades.

## Network path

The phone never dials Wrangler or a `100.x` address directly. `local-mesh.sh`
binds Wrangler to `127.0.0.1:27640`, detects the Mac's MagicDNS name, and runs:

```sh
tailscale serve --bg --https=443 http://127.0.0.1:27640
```

The script refuses to replace an existing Serve handler on HTTPS port 443 and
removes only the handler it started when it exits.

## Troubleshooting

- **Phone shows nothing / no devices** — user id and org must match *exactly* on
  the engine (`ZERON_EDGE_TOKEN=user@org`, `ZERON_ORG_ID=org`) and the phone
  (`-setuser` / `-setorg`). Check `edge.log` / `engine.log` (paths printed on
  startup), or run the desktop app with `ZERON_IPC_PORT=27654 … zeron sync` for
  per-room connection state.
- **HTTPS edge not reachable from the phone** — confirm both devices are on the
  same tailnet, MagicDNS and HTTPS certificates are enabled, and the printed
  `*.ts.net` URL opens in Safari. `tailscale serve status` should show the
  loopback proxy on port 443.
- **HTTPS port 443 is already in use** — stop `ota-serve.sh` or the other Serve
  handler before starting the mesh. The script will not replace it.
- **Host never runs a turn** — the `claude` CLI must be installed and signed in
  on the Mac; `MESH_HARNESS=mock` is a no-CLI smoke alternative.
