# Running the whole stack locally (Mac + iPhone over Tailscale)

Run every part of Zeron on one Mac and continue your sessions from the iOS app
over your private network. No Cloudflare account, no WorkOS — the edge runs
locally in **dev auth** (bearer == `user@org`), exactly like `scripts/e2e-smoke.sh`.

```
 iPhone (Zeron.app, dev mode)                          MacBook
        │                                    ┌───────────────────────────────┐
        │  Tailscale (100.x)                 │  wrangler dev  ── Durable      │
        └──────────────►  edge :27640  ◄─────┤     (edge/)       Objects     │
                              ▲              │        ▲                       │
                         loopback            │        │ loopback              │
                              └──────────────┤  zeron headless  ── claude CLI │
                                             │   (host device, runs agents)   │
                                             └───────────────────────────────┘
```

The phone is a **viewport**: it queues commands into the session docs; the Mac
host device drains them and runs the agent. No engine runs on the phone.

## 0. Prerequisites (once)

- **Rust** (`cargo`) — builds the engine/desktop binary.
- **Node** — runs the edge: `cd edge && npm ci`.
- **Xcode 26+** (iOS 26 SDK) — builds the iOS app.
- **Tailscale** on both the Mac and the iPhone, same tailnet (LAN also works if
  both are on the same Wi‑Fi).
- **`claude` CLI** signed in on the Mac (for the default `claude-code` harness).

## 1. Bring up the mesh

```sh
scripts/local-mesh.sh
```

First run builds the release engine (a few minutes) and starts the edge + a
headless host device. It prints the exact **Edge URL / User id / Org id** to
enter on the phone. Leave it running; `Ctrl-C` tears it down. Durable Object
state persists under `edge/.wrangler/state`, so your sessions survive restarts.

Useful overrides:

```sh
MESH_USER=nico MESH_ORG=personal MESH_HARNESS=claude-code scripts/local-mesh.sh
SKIP_BUILD=1 scripts/local-mesh.sh          # reuse target/release/zeron
MESH_HOST=100.101.102.103 scripts/local-mesh.sh   # pin the advertised address
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

**Real iPhone** — needs your own Apple Team (the bundled `sh.zeron.ios` /
team belongs to the upstream project, so pick a unique bundle id):

```sh
DEVELOPMENT_TEAM=YOURTEAMID BUNDLE_ID=com.you.zeron scripts/build-ios.sh device
xcrun devicectl device install app --device <name> target/ios-build/export/Zeron.ipa
```

The app has **no dev-mode button** — dev sign-in is driven by launch arguments,
which the app then persists (`AppModel.restore`). Set them once in Xcode
(*Product → Scheme → Edit Scheme → Run → Arguments*):

```
-setmode dev  -setedge http://<mac-tailscale-ip>:27640  -setuser <user>  -setorg <org>
```

Run once from Xcode with those; afterwards a normal tap on the icon reconnects
on its own. `local-mesh.sh` prints the precise values for your machine.

## Tailscale HTTPS (if plain http is refused)

iOS App Transport Security allows the app's cleartext `http://` to private
addresses (`NSAllowsLocalNetworking`), which covers LAN ranges. If your device
refuses the Tailscale (`100.64/10`) address over http, terminate TLS with a
Tailscale proxy and dial `https://` instead:

```sh
# on the Mac (MagicDNS + HTTPS must be enabled for the tailnet)
tailscale serve --bg --https=443 http://127.0.0.1:27640
```

Then point the app at your MagicDNS name and re-run the mesh advertising https:

```sh
MESH_HOST=<your-host>.<tailnet>.ts.net MESH_EDGE_SCHEME=https MESH_ADVERTISED_PORT=443 \
  scripts/local-mesh.sh
```

The app already prefers `wss://` for a `https` edge and has a plain‑HTTPS pull
fallback for networks that strip WebSocket upgrades, so this path is the most
robust.

## Troubleshooting

- **Phone shows nothing / no devices** — user id and org must match *exactly* on
  the engine (`ZERON_EDGE_TOKEN=user@org`, `ZERON_ORG_ID=org`) and the phone
  (`-setuser` / `-setorg`). Check `edge.log` / `engine.log` (paths printed on
  startup), or run the desktop app with `ZERON_IPC_PORT=27654 … zeron sync` for
  per-room connection state.
- **`wrangler dev` not reachable from the phone** — it must bind a routable
  interface; the script passes `--ip 0.0.0.0`. Confirm the Mac's firewall
  allows the port, and that you used the Tailscale IP, not `127.0.0.1`.
- **Host never runs a turn** — the `claude` CLI must be installed and signed in
  on the Mac; `MESH_HARNESS=mock` is a no-CLI smoke alternative.
