# Over-the-air install over Tailscale (no cable, no store)

Install the Zeron app on your iPhone/iPad straight from your Mac over an HTTPS
link served on your tailnet. `tailscale serve` gives you a real, trusted
`*.ts.net` certificate, which is exactly what iOS requires for an
`itms-services://` install — so there is no self-signed-cert workaround and no
computer tether.

```sh
DEVELOPMENT_TEAM=YOURTEAMID BUNDLE_ID=de.you.zeron scripts/ota-serve.sh
```

It builds an ad-hoc `.ipa` (or reuses/serves one you pass via `IPA=`), writes a
`manifest.plist` whose ids are read straight from the IPA, serves both over
`https://<your-host>.<tailnet>.ts.net`, and prints an install link + QR. Open
that URL in **Safari** on the device, tap **Install**, done. `Ctrl-C` stops
serving and removes the `tailscale serve` handler.

## Prerequisites

1. **Paid Apple Developer account** (99 $/yr) — ad-hoc distribution needs it.
2. **Tailscale HTTPS enabled** for your tailnet: admin console → **DNS** →
   enable **MagicDNS** and **HTTPS Certificates**. Both the Mac and the device
   must be signed in to the same tailnet.
3. **The device's UDID registered** in your Apple account, and included in the
   ad-hoc provisioning profile. With Xcode automatic signing, a device you have
   registered (Xcode → Settings → Accounts, or the Developer portal → Devices)
   is picked up by the managed ad-hoc profile automatically.

### Getting the UDID without a cable

The UDID isn't shown in Settings directly. The usual cable-free way is to open a
UDID-reporting page on the device that installs a small configuration profile
and shows the UDID (several free services do this), then paste it into the
Apple Developer portal → **Devices**. After that, rebuild so the profile
includes it (`REBUILD=1 scripts/ota-serve.sh`).

## What it produces

- `Zeron.ipa` — the ad-hoc signed app.
- `manifest.plist` — the OTA descriptor: `software-package` (the IPA URL),
  optional `display-image`/`full-size-image` (from the app icon), and metadata
  (`bundle-identifier`, `bundle-version`, `title`) read out of the IPA so they
  can't drift.
- `index.html` — a small landing page with the `itms-services://…&url=<manifest>`
  Install button.

All served by a tiny local static server (correct MIME types for `.plist` and
`.ipa`) that `tailscale serve` fronts with TLS on `https://<host>` (port 443).

## Connecting the installed app to your mesh

OTA only installs the app. To point it at your local engine, bring the mesh up
and use the in-app dev sign-in (see [`local-mesh.md`](local-mesh.md)):

```sh
scripts/local-mesh.sh     # prints a zeron://dev?… link + QR
```

Scan that QR (or tap **“Use a self-hosted server”** in the app) and you're on
your Mac's sessions.

## Troubleshooting

- **“Cannot be installed at this time” / “not available”** — the device UDID
  isn't in the profile. Register it (above) and rebuild with `REBUILD=1`.
- **`tailscale serve failed`** — HTTPS certificates aren't enabled for the
  tailnet, or `tailscale up` hasn't completed. Enable HTTPS in the admin
  console, confirm `tailscale status` shows this machine.
- **Safari does nothing on the Install link** — `itms-services://` links only
  work in Safari, over a *trusted* HTTPS origin. Make sure you opened the
  `*.ts.net` URL (not an IP) and that the cert is valid (it is, with Tailscale
  HTTPS).
- **The public URL 404s or hangs** — give MagicDNS/cert propagation a few
  seconds and reload; confirm the Mac's static server is up (the script waits
  for it before serving).
- **Left something serving** — the script removes its handler on exit; to clear
  everything manually: `tailscale serve reset`.

## Updating the app

When you rebuild (a new app version, or the profile changed), re-run
`REBUILD=1 scripts/ota-serve.sh` and open the link again on each device. The
signature is valid for ~1 year, so between rebuilds nothing expires.
