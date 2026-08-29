# Installing Zeron on iPhone/iPad with SideStore (no TestFlight)

SideStore installs and **auto-refreshes** the app over Wi‑Fi, signing it with
your own Apple ID. With a **paid** Apple Developer account the signature lasts
**~1 year** (a free Apple ID expires after 7 days and SideStore re-signs it in
the background). No App Store, no TestFlight, and — after a one-time setup —
no cable.

> Honest caveat: SideStore's *initial* setup (creating the pairing file that
> lets it re-sign over the air) needs **one** USB connection to a computer.
> Everything after that, including installing Zeron and every refresh, is
> wireless. There is no way to run an unsigned app on stock iOS, so some Apple
> ID signing step is unavoidable on every path — SideStore just automates it.

SideStore itself is third-party tooling and its exact steps change between
versions — always follow the current official guide at
<https://docs.sidestore.io> for SideStore setup; this doc covers only the
Zeron-specific parts.

## 1. Build an installable `.ipa` (on the Mac)

```sh
DEVELOPMENT_TEAM=YOURTEAMID BUNDLE_ID=de.yourname.zeron \
  EXPORT_METHOD=development scripts/build-ios.sh device
# → target/ios-build/export/Zeron.ipa
```

SideStore re-signs the app with your Apple ID on install, so the important part
is getting a clean `.ipa`; the export signing is just to produce it. Pick a
**unique** `BUNDLE_ID` (the default `sh.zeron.ios` belongs to the upstream
project). Get the file onto the device however you like — AirDrop, or save it
to the Files app / iCloud Drive.

## 2. Set up SideStore (one-time)

Follow <https://docs.sidestore.io>. In short:

1. Install SideStore's own app on the device (via their web installer / a
   desktop helper).
2. Generate a **pairing file** — the one step that needs a USB cable once — and
   import it into SideStore.
3. Open SideStore → sign in with your **Apple ID** (use the account tied to your
   paid Developer membership so apps get the 1-year signing).

SideStore installs a small on-device VPN/WireGuard profile; that's what lets it
refresh apps in the background over Wi‑Fi. Leave it enabled.

## 3. Install Zeron

In SideStore → **My Apps → +** (or the app's import), pick the `Zeron.ipa` from
step 1. SideStore signs it under your account and installs it. It appears on the
Home Screen like any app.

Refreshing: open SideStore every so often (or let its background refresh run);
with a paid account you have a year of headroom, so this is rarely urgent.

## 4. Connect it to your Mac mesh

Bring the mesh up on the Mac:

```sh
scripts/local-mesh.sh
```

It prints an **Edge URL / User id / Org id** and a `zeron://dev?…` link plus a
scannable QR. On the device, connect in any of these ways (all cable-free):

- **Scan the QR** with the Camera app (or open the `zeron://dev?…` link) — the
  app fills in the dev server and connects. This is handled by
  `AppModel.handleDeepLink`.
- In the app's sign-in screen, tap **“Use a self-hosted server”** and enter the
  three values by hand.

Make sure the **Tailscale** app is installed and connected on the device (same
tailnet as the Mac). If iOS refuses the plain‑`http` Tailscale address, use the
`tailscale serve` HTTPS recipe in [`local-mesh.md`](local-mesh.md#tailscale-https-if-plain-http-is-refused)
and re-run the mesh with `MESH_EDGE_SCHEME=https`.

## Notes & limits

- **Apple ID app-ID limits** still apply (a handful of distinct app IDs per
  account; SideStore reuses one for Zeron, so this is a non-issue for a single
  app).
- SideStore needs the device and the machine that made the pairing file to be
  reachable for the *first* pairing only; refreshes use the on-device tunnel.
- This is the same `.ipa` you'd ship to TestFlight — if you later want fully
  hands-off OTA for several people, the TestFlight workflow
  (`.github/workflows/testflight.yml`) is still there; SideStore is the
  no-TestFlight route for yourself and a couple of devices.
- Prefer a pure-Apple path with no third-party tooling? See
  [`ota-install.md`](ota-install.md): serve the ad-hoc `.ipa` over an HTTPS
  `tailscale serve` link and install via Safari. It needs each device's UDID
  registered but no SideStore.
