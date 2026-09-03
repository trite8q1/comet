# comet — Architecture

A ground-up native rewrite of [comet](../comet) — a multi-device controller for coding agents
(Claude Code / Codex) — in Rust, with a gpui UI. Fresh app; no backwards compatibility required.

**Pillars (from the goal):**
- Optional sync uses Loro CRDT docs (loro-mirror model) through Cloudflare Durable Objects; the same docs persist locally when sync is disabled.
- Durable Objects stay **TypeScript** (decision + evidence: `docs/research/durable-objects-language.md`).
  Everything device-side is Rust.
- Feature parity with comet **except token-usage display** (poor fit for CRDTs; excluded).
- Frontend is **gpui** (pinned Zed rev). Virtualization + markdown techniques ported from
  **mugen + pretext** (`docs/research/mugen-pretext.md`).
- One binary, **headed or headless**. Smooth transitions/animations matching the original
  (catalog in `docs/research/feature-inventory.md` §1.12).

## 1. Topology (unchanged shape, new materials)

```
gpui UI ─ in-proc/localhost RPC ─ engine A ══ DeviceRoom DO relay ══ engine B ─ RPC ─ gpui UI
                    │       optional edge Worker: auth, rooms, R2        │
                    └── optional chat2 sync ──  ChatRoom DO (per chat) ──┘
                                          └─ Workspace registry room ────┘
```

- **Engine = backend** (was `@comet/backend`): runs agents, owns auth, terminals, repos/worktrees,
  diff sync, doc hosting. Pure Rust daemon, fully functional headless.
- **UI = viewport** (was Electron): gpui app rendering engine state. Talks the same typed RPC whether the engine is in-process or a separate daemon. Organized around **spaces** — (device, folder) pairs, local or synced according to the active profile. The sidebar is the data: an attention-sorted Sessions list, filtered by a searchable spaces dropdown ("All spaces" included) that also hosts space management. The horizontal tabs are a **device-local viewport** onto that list (`ui-settings.json` `openTabs`, cross-space): closing a tab is local-only — archiving is an explicit sidebar action — and a sidebar click (re)opens a session as a tab. The new-session canvas carries a space picker (defaulting to the sidebar filter, else the last selected space); new sessions are minted onto the picked space's device via relay-forwardable RPCs.
- **Edge (TypeScript, ported from comet `apps/edge`)**: Worker + ChatRoom DO (per chat, the
  chat2 row protocol; the legacy SessionRoom DO remains deployed only for pre-cutover clients —
  no current client dials it) + DeviceRoom DO (per device) + R2 attachments + WorkOS JWKS auth.
  Absorbs the old `apps/server` responsibilities (WorkOS code exchange/refresh, orgs) so
  **Postgres, the Hono server, and the WebRTC/signaling stack are all gone**.

### Headed / headless
Single binary `comet`:
- `comet` — headed. If a local engine daemon is already listening on the IPC port, connect to it;
  otherwise run the engine **in-process** (RPC over an in-memory duplex — same protocol, zero
  serialization shortcuts, so the boundary stays honest) **and serve that same engine on the IPC
  port**. The embedded engine is not private: any other viewport can attach to the running app
  without it first being restarted as a daemon. Binding is best-effort — if the port is taken the
  window still opens, having lost only the ability to host peers.
- `comet headless` — engine only. A clean installation immediately serves its local profile over localhost IPC; when a saved account selects the synced profile at startup and a bearer is available, it also hosts its DeviceRoom for remote control. A VPS can run this while a laptop's UI drives it.

### Local-first workspace profiles

Authentication and workspace selection are deliberately separate state machines:

- `AuthState` is live credential state: `SignedOut`, `NeedsOrganization`, or `SignedIn`. It may change after login, refresh, revocation, or logout.
- `WorkspaceScope` is the immutable storage and transport boundary captured once at engine startup: `Local`, `Synced`, or explicit `Development`.

The engine never re-resolves an open store because `AuthState` changed. This prevents a sign-in, token refresh, or revocation from silently swapping databases or attaching online transports to a runtime that started local-only.

| Startup condition | `WorkspaceScope` | Online transports |
| --- | --- | --- |
| WorkOS enabled, no parseable saved `session.json` | `Local` | Disabled |
| Parseable saved WorkOS session | `Synced` | Enabled when a bearer is available; organization onboarding completes before opening the store when needed |
| WorkOS disabled without a dev bearer | `Development` | Disabled |
| Explicit non-empty dev bearer | `Development` | Enabled |

`comet login` and `comet logout` operate on `session.json` while the engine is stopped. Login selects `Synced` for the next start; logout selects `Local` for the next start. The UI may update live authentication status, but the active `WorkspaceScope` still changes only after restart.

The resolved profile selects the session snapshots, registry snapshot, run journals, and attachment cache that may contain workspace data:

| Scope | Store and journals | Uploads |
| --- | --- | --- |
| `Local` | `{data_dir}/profiles/local/` | `{data_dir}/profiles/local/uploads/` |
| `Synced` | `{data_dir}/orgs/{org_id}/{user_id}/` | `{data_dir}/orgs/{org_id}/{user_id}/uploads/` |
| `Development` | `{data_dir}/orgs/{org_id}/{user_id}/` | `{data_dir}/orgs/{org_id}/{user_id}/uploads/` |

The synced and development store roots preserve the historical cloud layout while their attachment caches are account-scoped. Local identity lives in `{data_dir}/local-profile.json`; its UUID is stable across restarts and is not an account or development identity.

Older releases wrote every synced and development attachment to `{data_dir}/uploads/`, and persisted those absolute paths in transcripts. On upgrade, the first synced or development account that opens this legacy cache claims it in `{data_dir}/legacy-uploads-owner.json`. That account may read the cache as a compatibility fallback, but all new staging and commits use its account-scoped uploads root; other accounts cannot read or write the legacy cache.

Device identity and machine resources remain device-scoped under the common data directory: `device-id`, repository registration, managed worktrees, agent credentials/accounts, and UI settings. They are available across profiles, but they do not contain or expose another profile's transcripts or attachments.

#### Privacy boundary and follow-ups

This first local-first change does not upload, import, link, or delete local sessions when a user signs in. Local attachments remain jailed under the local upload root and are not readable through the synced attachment cache. Returning to local-only mode reopens the same local identity and data.

The following product work is intentionally deferred:

1. Explicit session selection and copy between local and synced profiles, including attachment copying, provenance, and conflict behavior.
2. Browsing both scopes simultaneously or switching the visible scope without restarting the engine.
3. A supported self-hosted backend contract covering authentication modes, room APIs, authorization, persistence, and blob storage. Current endpoint and bearer overrides remain development/deployment seams, not a promised compatibility surface.

## 2. Data model — all Loro

Two persistent doc kinds. When sync is enabled, session docs ride the chat2 row protocol (loro updates as append-only rows + Range-resumable checkpoints, ChatRoom DO) and the registry rides its own row-frame protocol; local-only profiles persist the same docs without joining rooms:

1. **Session doc** (per chat) — the transcript + durable command queue. Schema is a Rust port of
   `packages/session-doc` (same container names/shapes so the edge's tail materializer keeps
   working): `meta` map, `messages` list (parts as list-of-maps with **LoroText bodies** — the
   measured 1.03× oplog shape; never LWW value rewrites), `commands` list with ledger rules 1–3
   (append-only per-device entries; host-only outcomes; dedupe/TTL/supersede evaluation).
   Continuation splitting at 256KB, render-only tool parts (full inputs stay in the host's local
   run journal), tail/diff sidecars. Constants carried over (`STREAM_COMMIT_MS=120`,
   `DO_FLUSH_MS=5s`, compaction at 8MB, retain 30d, tail 64).

2. **Workspace registry doc** (per profile) — the `registry1` snapshot stores spaces (id, deviceId, path, name?, gitDetected, checkoutId), the chats index (id, deviceId, title, archived, cwd, branch, checkoutId, spaceId, lastSeenAt, lastMessagePreview/At, config), devices, session-status rows, and checkout-diff summary pointers. A space is a device+folder pair in the active profile; the owning device's `SpacesSync` stamps git presence so branch pickers and the diff sidebar can gate without another RPC. Local scope keeps the registry entirely in its profile store. Synced and development scopes join `/registry/{orgId}/ws`, backed by the private per-user room `reg1/{orgId}/{userId}`; rows are never visible to every member of an organization.

   Writer discipline: each device writes its own device and session-status rows, rows for chats it hosts, and git stamps for spaces it owns. Creates, renames, archives, and seen marks are LWW sets accepted from any device. `deleteSpace` tombstones the space and every chat/session row in it in one commit. Presence uses ephemeral room frames rather than durable heartbeat writes.

   *Why one registry and not N tiny docs:* the sidebar needs one subscription for the whole list (grouping, resort animations, unseen markers). Its rows contain indexes rather than transcripts, so one local snapshot and, when enabled, one room connection remain bounded and cheap.

3. **Mirror layer** (`comet-doc` crate) — Rust equivalent of loro-mirror: typed structs for the
   schema, **incremental** application of `doc.subscribe` diffs into cached state (no full
   re-hydration per change — this is also what fixes comet's known O(transcript) re-projection
   inefficiency, remaining-work item 1a), and a diff-reconcile write path (evaluate `lorosurgeon`
   0.2.x as a dep; our schema is small enough to hand-roll if it doesn't fit). The UI renders
   mirror state directly with per-entry change notifications — the "endgame" the TS
   implementation documented but never reached.

### Command plane
Send/steer/interrupt/respondInput = durable command entries in the session doc (`QueueCommand`),
executed by the chat's **host** device (executor gated on chat ownership; mark-processed BEFORE
execute; steer with no live run dispatches as the next turn). Offline sends queue in the doc.
This is comet's proven design, kept verbatim.

## 3. Cargo workspace

```
comet/
  Cargo.toml                 # workspace
  crates/
    proto/        comet-proto    # wire types: AgentEvent, ToolCall, RunRequest, Model,
                                 # entities, RPC envelopes (serde; ndjson framing);
                                 # `view` = the pure derivations both frontends share
                                 # (sort orders, staleness gating, grouping, boot gate)
    doc/          comet-doc      # session-doc + workspace-registry schemas, mirror layer,
                                 # parts fold, continuations, command ledger, sidecars
    sync/         comet-sync     # loro room client (join/VV backfill/fragments/backoff),
                                 # ephemeral presence, DocsStore (SQLite snapshots +
                                 # processed-command ledger)
    harness/      comet-harness  # Harness trait + claude-code (stream-json subprocess),
                                 # codex (app-server JSON-RPC), mock; steering mailbox,
                                 # requestInput, models/reasoning/options catalogs
    engine/       comet-engine   # sessions engine (pub/sub, run journal, recovery, stall
                                 # watchdog), doc host + command executor, repos/worktrees,
                                 # checkout-diff sync, terminals (portable-pty), uploads,
                                 # agent accounts (cred swap), auth (WorkOS via edge),
                                 # device-room host/peers, identity
    rpc/          comet-rpc      # UiRpc/ControlRpc: typed req/resp/stream over WS (tokio-
                                 # tungstenite) + in-memory transport; device-room virtual
                                 # sockets ({s,k,to,from} frames)
    theme/        comet-theme    # source-neutral theme schema + built-in/custom registry,
                                 # validation, provenance, and local VS Code compiler
    ui/           comet-ui       # gpui app: shell, sidebar, conversation, composer,
                                 # terminal view, diff pane, settings, animation kit
  apps/
    comet/                       # the binary (headed default, `headless` subcommand)
  edge/                          # TypeScript Worker + DOs (ported from comet/apps/edge,
                                 # + auth-exchange routes absorbed from apps/server)
  docs/                          # this file + research reports
```

Engine async runtime: **tokio** throughout; the UI bridges via `gpui_tokio` (`Tokio::spawn`
futures surfaced as gpui `Task`s). In-process mode runs the engine on its own tokio runtime
thread; the UI never blocks on it.

## 4. UI plan (gpui) — parity + smoothness

Reference: `docs/research/gpui.md`, `docs/research/mugen-pretext.md`,
feature spec `docs/research/feature-inventory.md` §1.

- **Deps**: `gpui` + `gpui_platform` pinned to one Zed rev (Apache-2.0). **We do not use Zed's
  GPL crates** (`markdown`, `ui`, `theme`, `editor`) — markdown, components, and theme are ours.
- **Transcript**: gpui `list()` + `ListState::new(n, ListAlignment::Bottom, overdraw)` (sum-tree
  offsets, follow-tail). On top of it, port the mugen behaviors that gpui doesn't give us:
  - stick-to-bottom **spring** with feed-forward tracking of streaming growth; interrupt from
    *user input* (wheel-up / drag), re-engage within a 70px band; own-send re-engages + smooth
    scrolls;
  - **block-granularity rows** (one row = one markdown block / tool group, not one message) with
    stable ids `msgId#blockId`; live turn stays unsplit, re-splits on persist; optimistic echo
    rows share the client-minted id so persistence never flickers;
  - row height memoization keyed by (row id, content length, width) so a streamed token
    re-measures one row;
  - scroll-anchor absorption for above-viewport height changes.
- **Markdown** (`comet-ui::markdown`): `pulldown-cmark` parsing on `background_spawn` with
  coalescing (Zed's proven pattern), block-level incremental re-parse of the streaming tail
  (incremark's O(delta) idea: only re-parse from the last stable block boundary), monochrome
  theme where **numbers drive layout, colors are paint**. Code blocks: monospace, no wrap ⇒
  height = lines × line-height (layout independent of highlight); syntax highlighting via
  `synoptic`/`syntect`-class tokenizer run time-sliced in the background, colors applied as text
  runs (paint-only). Streaming **fade-in veil** on newly appended text via `with_animation`
  opacity (paint-layer, never affects layout). `prefers-reduced-motion` honored.
- **Composer**: hand-rolled gpui text input (start from Zed's `examples/input.rs`: IME, selection,
  clipboard, key actions), compact↔expanded auto-flip by measured text width, auto-grow 76–260px,
  Enter/Shift+Enter, Send→Steer→Stop morph, drafts + attachments per chat, drag-drop/paste
  images, QuestionPanel (paged, 1-9 keys, 220ms auto-advance) replacing the composer while input
  is requested. Pickers (harness/model, traits, repo w/ folder browser, branch w/ worktree
  toggle) as gpui popovers with `menu-in` scale/fade.
- **Terminal**: `alacritty_terminal` (vte state machine, MIT/Apache) + `portable-pty` on the
  engine side; custom gpui grid element; tabs w/ drag-reorder (150ms sliding transforms), height
  drag 160px–55vh, 12ms input coalescing / 80ms resize debounce, 1MB replay, detach ≠ close.
- **Diff pane**: unified-patch parser → virtualized file/hunk/line rows, per-file collapse
  (180ms height tween), time-sliced highlight, 200ms width transition on the pane itself.
- **Animation kit** (`comet-ui::motion`): small helpers over gpui `Animation` reproducing the
  comet catalog — `fade-in` (0.5s, cubic-bezier(0.16,1,0.3,1), translateY 4→0), `splash-out`,
  `comet-pulse` staggered cell wave (boot splash + loaders), `gradient-spin-pulse` matrix
  spinner (WorkingIndicator + rotating flavour word), `menu-in`/`dialog-in` scale-fades, 200ms
  ease-out width/height transitions for sidebar/panes, sidebar-resort **slide animation**
  (we own the list, so animate row positions directly — the View Transitions equivalent, 260ms
  cubic-bezier(0.22,1,0.36,1)), reduced-motion switch.
- **Theme**: independent light/dark resolved variants, theme-owned semantic/syntax/terminal
  palettes, optional interaction-accent overlays, and a device-local surface preference that
  resolves each variant's recommended frost/opaque treatment without changing theme selection.
  Forced frost derives contrast-checked tints from mapped theme surfaces. Local VS Code
  file/package compilation and imported/linked custom families retain last-known-good
  persistence. Colors remain paint-only; hairline borders and bundled Geist/Geist Mono remain
  shared presentation foundations.

## 5. Engine plan

Direct ports of comet behaviors (spec: feature-inventory §3):
- **Sessions engine**: per-session broadcast hub; on-disk run journal (resumable `seq` replay,
  crash auto-resume); persistent steerable sessions (steering mailbox at step/turn boundary; idle
  reaper; 10min stall watchdog); recovery stamps `aborted`.
- **Doc host**: per-chat handle (join room, VV backfill, write user entries + stream assistant
  segments at 120ms commits, drain commands host-only with processed-ledger idempotence, publish
  diff sidecar, presence); warm-open recent chats (14d/cap 30); nudge-driven cold open; SQLite
  snapshot store.
- **Harness** (research pending — `docs/research/harness.md`): trait mirroring comet's
  `HarnessShape`; Claude Code via `claude` CLI stream-json in/out (control protocol for
  permissions/AskUserQuestion→requestInput, resume, steering); Codex via app-server JSON-RPC or
  `codex exec --json`; model/reasoning/option catalogs ported from `packages/harness`.
- **Repos/diffs**: git2 or `git` subprocess (subprocess — matches comet, avoids libgit2 edge
  cases); worktrees under `~/.comet/worktrees`; fs watchers (`notify`) + 2min repair; diff
  capture (patch + numstat + untracked, 3MiB cap, sha256) → workspace registry summary + DO diff
  sidecar.
- **Agent accounts**: credential-slot swap (macOS Keychain via `security-framework`, files
  elsewhere), plan labels, usage probes, paste-code/browser-poll OAuth flows.
- **Auth**: WorkOS through edge routes (`/auth/exchange`, `/auth/refresh`, orgs); loopback
  callback server headed, paste-code headless; dev mode (no key ⇒ bearer = configured user id).

## 6. Edge plan (TypeScript, `edge/`)

Port `comet/apps/edge` nearly verbatim (it is already Loro-native and smoke-tested: session room
w/ hibernation + two-level compaction + daily alarm backups, device room byte relay + nudges +
sidecar slots, R2 attachments, JWKS auth). Additions:
1. Private per-user registry rooms (`/registry/{orgId}/ws` → `reg1/{orgId}/{userId}`) with authenticated row sync and ephemeral device presence.
2. `/auth/*` routes absorbed from `apps/server` (WorkOS API key in Worker secret).
3. Drop `/seed` migration path and legacy sync anything (fresh app).
Hibernation hygiene: no idle timers (flush timer only while dirty), auto-response ping/pong —
per `docs/research/durable-objects-language.md`.

## 7. Parity exclusions & deliberate changes

- **Excluded**: token-usage display (profile heatmap, lifetime stats, per-message token columns,
  `WatchUsage`). Rate-limit meters on agent accounts are *kept* (separate concern; probed from
  CLIs, not CRDT-synced).
- **Changed**: Postgres entity sync/server → workspace registry + edge; Electron/React/mugen → gpui with
  ported techniques; Node harness SDKs → subprocess protocols; WebRTC → device-room relay (comet
  had already made this move); mobile app → out of scope for this repo.
- **Kept verbatim**: session-doc schema shape + constants, command ledger rules, edge DO design,
  render-parts privacy policy, UX behaviors and animation timings.

## 8. Milestones

Status legend: ✅ shipped · 🟡 shipped with named gaps (see `docs/PARITY.md`).

- ✅ **M0 Scaffold** — workspace builds; `proto`/`doc` crates with ledger + parts + continuation
  unit tests; gpui hello-window runs.
- ✅ **M1 Doc + sync core** — `comet-doc` mirror over loro 1.13; room client syncs with the edge
  running under `wrangler dev`; Rust⇄edge⇄Rust convergence test (M1 exit: two Rust peers converge
  through a real SessionRoom DO, tail endpoint serves).
- ✅ **M2 Engine core** — Claude harness end-to-end headless: `comet headless` + dev auth runs a
  turn, journal + doc writes, recovery test.
- ✅ **M3 UI core** — shell (sidebar/panes/header), transcript (virtualized, markdown, streaming,
  stick-to-bottom), composer (send/steer/stop, question panel); local chat fully usable headed.
- ✅ **M4 Multi-device** — device-room host/client virtual sockets, remote device control, workspace
  registry sync, WorkOS auth + org gate, presence. Proven live by `scripts/e2e-smoke.sh`:
  two headless engines against a real edge — B queues a run into the chat doc, the durable
  nudge wakes host A, A executes (mock harness), transcript + session status sync back to B.
- 🟡 **M5 Full surface** — terminals, diff pane, repo/branch/folder pickers + worktrees,
  agent accounts UI, settings (devices/shortcuts/archived), Codex harness. Gaps: composer
  attachment UI (engine upload RPCs exist), Cursor harness.
- 🟡 **M6 Polish** — wire reconciliation (proto AuthState on the wire, `LocalDevice`),
  two-device e2e smoke, keyboard map, clippy/fmt sweep, Linux packaging
  (`scripts/package-linux.sh` + release profile), macOS bundling config (`dist/macos/`,
  not executed — needs a Mac). Gaps: prefers-reduced-motion, engine hardening
  (instance lock, watchdogs), edge production deploy.

## 9. Open questions (tracked, non-blocking)

1. loro-protocol Rust client ⇄ TS edge interop — verify at M1; fallback is a ~300-line hand-rolled
   client (the frame protocol is small and we control both ends).
2. `lorosurgeon` fit for the mirror write path vs hand-rolled reconcile.
3. Cursor harness (comet has it; CLI surface for Rust TBD) — parity item, scheduled after Codex.
4. Text shaping performance for analytic row heights: gpui measures shaped text natively (Rust ⇒
   cheap), so we start with gpui `list()` measurement + memoization rather than porting pretext's
   full analytic kernel; revisit only if cold-open of huge transcripts measures slow.

## 10. Agent Skills as slash commands (every surface, native CLI parity)

Goal: whatever a user can invoke by name in the active agent's own CLI — Agent Skills
(`SKILL.md` + optional `scripts/`, `references/`, `assets/`), custom commands, and the CLI's
built-ins — is invocable from every comet composer under the same name, with the same
arguments, discovered the same way, and activated the same way (explicit vs. model-driven).
Nothing more: comet does not invent a skills catalog, does not read `SKILL.md` itself, and
does not offer a skill on a harness whose CLI would not offer it either.

### 10.1 Vocabulary

- **Invocable** — one entry the harness lets the user invoke by name from the prompt. Wire
  type: `comet_proto::SlashCommand { name, description, input_hint }`. Skills, custom
  commands, and built-ins are all invocables; comet carries no separate "skill" type because
  no CLI exposes one on its wire (Claude's `initialize` lists them together, ACP's
  `availableCommands` likewise). Skills are the invocables whose backing is a `SKILL.md`; the
  harness knows that, comet does not need to.
- **Catalog** — the ordered invocable list for one `(device, HarnessId)`. Never merged across
  harnesses.
- **Invocation** — the prompt text `/name` or `/name args` as the user sends it. It is what
  persists in the session doc as the user message (the CLIs' own transcripts show the same).
- **Translation** — the adapter-side mapping from an invocation to that harness's native
  wire form, applied at run time inside `Harness::run`.

### 10.2 Surfaces

| Surface | Code | Trigger | Catalog source |
| --- | --- | --- | --- |
| Desktop composer (gpui) | `crates/ui/src/composer.rs` (`SlashState`, `slash_cache`) | `/` as the first character of the prompt (`slash_token`) | `ListCommands {harness, cwd}` targeted at the chat's (or picked space's) host device; cached per `(device, harness, cwd)` for the composer's life |
| Phone composer (SwiftUI) | `apps/ios/Comet/Composer/SlashCommands.swift` (logic) + `ComposerView.swift` (view) | same rule, same token grammar | same RPC over the device-room relay (`WorkspaceStore.listCommands`, the `listModels` pattern); cached per `(deviceId, harness, cwd)` |

Both surfaces behave identically: the popup lists the catalog filtered by the typed prefix
(name match, plus aliases where the harness advertises them), each row shows description and
argument hint, accept replaces the token with `/name ` and leaves the cursor for arguments,
Escape dismisses for that token only, every open revalidates the key's list (§10.4
"Freshness"), and the prompt is sent as plain text through the durable command queue
(`QueueCommand{run|steer}`) — no new RPC, no new doc field. `comet headless`
has no composer and therefore no surface here; the binary's subcommands never send prompts.

The popup is a completion aid, not a gate: a user may type `/name args` in full and send. The
harness decides what an unknown `/foo` means, exactly as its CLI would.

### 10.3 Harness registry — which catalog

The active harness is an *input* to this layer, never derived by it: `ChatConfig.harness`
for an existing chat, `ResolvedRunConfig.harness` on the new-chat canvas (desktop), and
`chat.config.harness` (phone). Models are catalogued per harness (`ListModels {harness}`,
`HarnessCatalog` on the phone), so picking a model family picks its harness — Claude models
run on Claude Code, GPT models on Codex, Grok on Grok Build, and so on. The skill layer asks
"which `HarnessId`?" and nothing else; it never falls back to a default harness's catalog
when the resolved harness is unknown (the popup stays empty until resolution).

The engine's `HarnessRegistry` maps `HarnessId → Arc<dyn Harness>`. `ListCommands {harness}`
resolves exactly that one slot and calls its `commands()`. No other harness is instantiated,
probed, or consulted for the request.

### 10.4 Discovery — the CLI is the authority

`Harness::commands()` is the single discovery seam. Every adapter answers it from the
agent's **own wire**, so the agent process applies its documented discovery paths,
precedence, enablement, and user-invocable filtering — comet inherits them instead of
re-implementing them (and cannot drift from them across CLI versions). A filesystem scan of
`SKILL.md` trees is permitted only for a harness whose wire exposes no listing at all, lives
inside that adapter, and must follow that harness's documented paths and precedence order.

| Harness | Wire | Discovery call | Skills appear as | Enablement / filtering |
| --- | --- | --- | --- | --- |
| Claude Code | stream-json control channel | `initialize` control request → `response.commands[]` (`name`, `description`, `argumentHint`, `aliases`) | `name` (user/project skills), `plugin:name` (plugin skills, bare name in `aliases`); indistinguishable from custom commands and built-ins on the wire | The CLI omits `user-invocable: false` skills and disabled plugins; `disable-model-invocation: true` skills are listed (explicit invocation is their point). cwd-dependent: the probe's directory decides which project skills appear |
| Codex | app-server JSON-RPC | `skills/list` → `data[].skills[]` (`name`, `description`, `path`, `scope`, `enabled`, `pluginId`, optional `interface.{displayName, shortDescription, …}`) | `name`; repo-scoped copies listed first, so dedupe-by-name keeps the one Codex would run | `skills/list` returns disabled skills flagged `enabled: false` (`[[skills.config]]` opt-out in `config.toml`); the adapter drops them. `policy.allow_implicit_invocation` affects model activation only, not listing |
| OpenCode | HTTP | `GET /command` | `name`, tagged `source: "skill"` beside `command` and `mcp` entries — skills are explicitly invocable in OpenCode | Server-side; comet applies no `source` filter |
| Grok, Hermes, Pi (ACP) | ACP v1 | `session/new` → `session/update: available_commands_update`; the `initialize` `_meta` scan is only a fallback for an agent that refuses sessions before login (Grok's handshake list is a partial, skill-free 7 entries) | Grok: bare `name`, qualified (`user:goal`, `vercel:workflow`) only where names collide, `_meta` carrying scope/path/pluginName; Pi: `skill:name`; Hermes: none — its ACP adapter advertises nine built-ins only | Agent-side (Pi drops `skill:` under `enableSkillCommands: false` and hides `source: "extension"`; Hermes reaches its skills through the model's skills tool, never as invocables). Grok's catalog is cwd-dependent |
| Cursor | `@cursor/sdk` shim | No wire listing (1.0.28 exposes no skills API or skill input; `cursor-agent`'s own listing needs `cursor-agent login`, separate from the SDK's credentials) → adapter-local `SKILL.md` scan under this section's exception — the one place Comet reads `SKILL.md`, to be replaced by the wire the moment the SDK lists skills; a tripwire test fails on any SDK pin bump until the scan is re-validated against that version — over Cursor's documented roots: built-in `~/.cursor/skills-cursor`, then project, then user `.agents/skills` + `.cursor/skills` (+ `.claude`/`.codex` compat), later root winning a shared name | the `SKILL.md`'s directory name — what Cursor's own palette submits | `metadata.surfaces` without `cli` dropped; Codex's built-in skill names dropped; `disable-model-invocation` skills kept (user-invocable only). Custom commands are not offered: the CLI expands their body client-side |
| Mock | — | Tests register their own `Harness` impls with fixed catalogs | — | — |

Discovery is a short-lived probe (no model turn, no API cost), cached per harness instance,
and never blocks a run. A failing probe surfaces as the popup's error row when there is no
list to show; it never falls back to another harness's list or to a comet-side scan.

**Freshness.** Every CLI re-reads its skills on launch; Comet's analogue is the popup open.
The adapter cache (`comet_harness::commands::CommandCache`) holds one entry per cwd with a
30-second time-to-live: within it a call is answered from memory, after it the next call
re-probes. A re-probe that fails keeps serving the last good entry and logs the failure, so
a transient CLI hiccup never blanks a list that was fine a moment ago; a probe that never
succeeded is retried on every call. Cursor's scan is a directory walk and stays uncached.
Both composers revalidate on open: cached rows for the key show immediately, one
`ListCommands` is sent per open (never per keystroke, and never a second one while the
first is in flight), and the reply replaces the rows. An error is displayed only when the
key has no rows at all.

**One discovery path.** The probe is the only source of the catalog. The former
`AgentEvent::AvailableCommands` run-time event (ACP `available_commands_update`, OpenCode's
and Cursor's per-run lists) was consumed by nobody and is retired: adapters no longer emit
it. The enum variant stays decode-only: the run journal skips a line it cannot decode and
reuses its sequence number, which would hide the next real event from subscribers, so an
old journal line must still decode (and fold to nothing) — pinned by a journal test.

**Discovery is cwd-scoped.** Every CLI that lists invocables resolves project-level skills
relative to a directory (Claude Code `.claude/skills` and `.claude/commands`, Codex
`.agents/skills` and `.codex/skills`, OpenCode `.opencode/skill`, Grok `.agents/skills` and
`.cursor/skills`, Cursor's project roots). The catalog therefore carries the directory the
run would execute in: `ListCommands {harness, cwd?}` → `Harness::commands(cwd: Option<&Path>)`.
`cwd` is the chat's `cwd` for an existing chat and the picked space's path on the new-chat
canvas; `None` (an old caller) probes the way the CLI would when started from the engine's
own directory. Adapters run their probe in `cwd` (child process directory, `skills/list
{cwds}`, `GET /command?directory=`, `session/new {cwd}`, the scan's project root) and cache
per `cwd`. Both composers key their cache by `(device, harness, cwd)`.

### 10.5 Slash routing — translation lives in the adapter

The composer sends `/name args` as text. Each adapter's `run()` (and steer path) translates
a leading invocation into the form its CLI would send for the same user action:

| Harness | Native user action | Comet translation |
| --- | --- | --- |
| Claude Code | `/name args` typed in the TUI; the CLI expands skills/commands from prompt text (documented for `-p` too: "include `/skill-name` in the prompt string and Claude Code expands it before running") | Pass through unchanged as the user message, on the first prompt and on every steer |
| Codex | `$name args` typed in the TUI → `input` = a text item (mention left inline) FOLLOWED BY a `skill` item (`name`, `path`) | `/name` matching a listed, enabled skill → text item `$name args` then `{"type":"skill", name, path}` in that order, on `turn/start` and `turn/steer`; otherwise plain text. The skill list comes from `skills/list` on the live session (its cwd is the run's cwd) |
| OpenCode | `/name args` in the TUI → `POST /session/{id}/command`; the endpoint resolves commands and skills alike | Known `/name` (via `known_invocation`) → command endpoint with the catalog's own name + trimmed `arguments`; anything else → `prompt_async` |
| ACP agents | `/name args` as `session/prompt` text — ACP defines no command RPC, and each agent parses the leading `/` itself (Grok natively; pi-acp intercepts its built-ins and forwards the rest, `skill:name` included, to pi; Hermes runs its nine built-ins and sends anything else to the model) | Pass through unchanged, prompt and steer paths alike |
| Cursor | `/name args` typed in the CLI: its palette submits the invocation as plain user text, and its ACP server forwards an unmatched `/name` untouched | Pass through unchanged as the user message |

The invocation grammar is shared: `comet_harness::commands::split_invocation(prompt) ->
Option<(name, args)>` (leading `/`, name to the first whitespace, rest trimmed). Adapters use
it and match the name against **their own** catalog; a `/name` unknown to the catalog is left
as text so the CLI can react as it would natively. Activation semantics are the harness's:
comet never injects `SKILL.md` content, never pre-expands a command, and never marks a skill
as model-invocable or not.

### 10.6 Isolation

- One catalog per `(device, HarnessId, cwd)`: the adapter cache is per harness instance on
  that device, per cwd; the engine's `ListCommands` touches one registry slot on the
  targeted device; both composers key their cache by `(device, harness, cwd)`.
- Switching device swaps the list the same way switching harness does: a request goes to
  the new device, and the previous device's entries are never shown, even while it loads.
- Switching the harness in the composer swaps the list; the previous harness's entries are
  never shown under the new one, even while the new catalog loads.
- The mock harness is the only harness allowed to carry an injected catalog, and only in
  tests.
- No shared skill root: a skill installed for Codex (`~/.codex/skills`) is not offered on
  Claude Code unless Claude Code's own CLI reports it, and vice versa. Cross-agent roots such
  as `~/.agents/skills` are visible only through the CLIs that read them.

### 10.7 Verification — the loop every workstream runs

`scripts/verify-skills.sh` is the gate; `scripts/verify-skills.sh --live` adds the ignored
real-CLI tests for whichever agents are installed on the machine. It proves:

1. **Correct catalog per harness** — fixture CLIs (`crates/harness/tests/fixtures/fake-*.sh`)
   answer discovery with harness-shaped payloads including skills; tests assert names, hints,
   dedupe, and enablement filtering per adapter.
2. **No cross-harness leakage** — an engine test registers two harnesses with disjoint
   catalogs and asserts `ListCommands` returns exactly the resolved harness's list; a composer
   test asserts a harness switch never renders the previous list; adapter tests assert an
   unknown `/name` is not translated.
3. **Slash parity** — per adapter, the fixture CLI records the wire frame produced for
   `/name args` and the test asserts it matches the native shape (Claude: unchanged text;
   Codex: skill input item + text; OpenCode: command endpoint; ACP: unchanged text).
4. **Architecture and code quality** — `cargo fmt --check`, clippy with no new diagnostics on
   lines added relative to `main` (`scripts/clippy-new-warnings.py`), and a guard that fails when a `HarnessId` variant is matched inside the
   slash paths of `crates/ui`, `crates/engine/src/rpc.rs`, or the phone composer, or when
   `SKILL.md` is parsed outside `crates/harness` — the "adapters only, no feature forks" rule
   as an executable check.

Rule: no feature lands without a test in this loop that failed before the change and passes
after it. Live tests are additive evidence, never the only evidence.

### 10.8 Workstreams (parallel, isolated)

Each workstream owns one adapter or surface, touches nothing outside it except its fixture
and its rows in the tables above, and ships through §10.7:

- **Codex** — `enabled` filtering; native skill input item on invocation; fixture + tests.
- **Cursor** — determine the SDK's listing/invocation surface for skills; implement discovery
  and translation per finding, or record implicit-only parity (empty catalog) with evidence.
- **ACP (Grok, Hermes, Pi)** — live-verify skills in `availableCommands`, hint fields, and
  invocation forms; extend `parse_commands` and fixtures where the agents differ.
- **Claude Code + OpenCode** — verify skills in the handshake list and passthrough; add
  fixture skills and parity tests; aliases if the CLI's own popup matches on them.
- **Phone composer** — the `/` popup over relayed `ListCommands`.

## 11. Native plan mode (every harness, CLI parity)

Goal: a user can enter and leave the active harness's **own** plan mode from every comet
composer, watch the harness's current plan update live in a card in the chat thread, and
answer the harness's own approval gate from that card — on macOS and iOS alike. Comet does
not invent a planner, does not synthesize plan prompts, and does not approve a plan on the
user's behalf. Adding a harness means adding an adapter (+ its icon); no plan machinery
changes.

### 11.1 Vocabulary

- **Plan mode** — the harness's native "explore and propose, do not edit" mode: Claude Code's
  `plan` permission mode, Codex's `plan` collaboration mode, Cursor's `plan` agent mode,
  OpenCode's `plan` agent, an ACP agent's `plan` session mode. Comet carries one bit,
  `plan_mode`, because that is the common denominator every CLI exposes as a toggle
  (Shift+Tab in Claude/Grok/Codex, Tab in OpenCode, `--mode plan` in Cursor).
- **Requested mode** — `ChatConfig.plan_mode` (LWW, any device): the mode the user asked for,
  carried exactly like `model` and `reasoning`. It rides every `RunRequest.plan_mode`.
- **Reported mode** — what the harness says it is in (`AgentEvent::PlanModeChanged`). The
  host reconciles the requested mode to the reported mode on every report, so an agent that
  exits plan mode itself (approved `ExitPlanMode`, OpenCode `plan_exit`, Grok
  `current_mode_update`) flips the toggle on every device.
- **Plan** — the harness's current plan text (markdown) as the harness produced it: the plan
  file Claude/OpenCode write, the Codex `plan` item, Cursor's `createPlan` argument, Grok's
  `exit_plan_mode` `plan_content`. Comet never edits it.
- **Plan exit request** — the harness asking the user whether to leave plan mode with this
  plan (Claude `ExitPlanMode` → `can_use_tool`; OpenCode `plan_exit` → its "Build Agent"
  question; Grok `exit_plan_mode` → `session/request_permission`). Some CLIs have no
  agent-initiated gate (Cursor, Codex): there the user leaves plan mode with the toggle, as
  in the CLI.
- **Decision** — `PlanDecision { approved, feedback }`: approve, or keep planning with optional
  feedback.

### 11.2 Harness contracts — the CLI is the authority

Verified on this machine (2026-09-02: Claude Code 2.1.258, Codex 0.151–0.152, Cursor SDK 1.0.28,
OpenCode 1.18.10, Grok 1.0.13, Hermes 0.13.0). The adapter speaks the wire the CLI itself
speaks; nothing below is emulated.

| Harness | Enter / leave | Reported mode | Plan text | Exit gate |
| --- | --- | --- | --- | --- |
| Claude Code | launch `--permission-mode plan`; live `control_request {subtype:"set_permission_mode", mode:"plan"\|"default"}` | `system/init.permissionMode`; `EnterPlanMode` tool result | the plan file the CLI has the model write (`~/.claude/plans/<name>.md`, or `plansDirectory`); the adapter reads it after each Write/Edit on `**/plans/*.md` while in plan mode. `ExitPlanMode`'s `can_use_tool` input carries `plan` + `planFilePath`, injected from disk by the CLI | `can_use_tool` for `ExitPlanMode`: approve = `{"behavior":"allow","updatedInput":…,"updatedPermissions":[{"type":"setMode","mode":"default","destination":"session"}]}`; keep planning = `{"behavior":"deny","message":<feedback or the CLI's rejection sentence>}` and the model continues in plan mode within the same turn |
| Codex (app-server) | **none**: 0.151–0.152 expose `collaborationMode {mode: plan\|default}` only read-only in `ThreadSettings`; `thread/start` / `turn/start` take no mode. `plan_mode() == false`, so the toggle is hidden | `thread/settings/updated.threadSettings.collaborationMode` | `plan` thread item: `item/plan/delta` streams, `item/completed` is authoritative | none on the wire (the TUI's own affordance). Tripwire: an ignored live test regenerates the schema (`codex app-server generate-json-schema`) and fails when `TurnStartParams`/`ThreadStartParams` gain `collaborationMode` — the §10.4 Cursor-skills precedent |
| Cursor (SDK shim) | `mode: "agent"\|"plan"` on `Agent({…})` and on every `send(prompt, {mode})` | client-owned; the adapter echoes the mode it sent | `createPlan` tool call `{plan}` | none on the wire; the user leaves with the toggle |
| OpenCode | `agent: "plan"\|"build"` on every `prompt_async` | `plan_enter` / `plan_exit` tool parts completing (what the TUI listens for) | the plan agent may only edit `.opencode/plans/*.md` (or `<data>/plans/*.md`); the adapter reads the file after each edit/write part on `**/plans/*.md` | `plan_exit` asks a `question` (header "Build Agent", Yes/No) → SSE `question.asked` whose `tool.callID` is the `plan_exit` part. Yes → the server injects its own synthetic build message and switches the agent; No → the tool is rejected and the model keeps planning |
| Grok Build (ACP) | `session/set_mode {modeId:"plan"}` / `{modeId:"default"}` (both accepted although `session/new` advertises no `modes`; other ids are silently ignored) | `session/update current_mode_update {currentModeId}` (also replayed by `session/load`) | the agent writes `~/.grok/sessions/<cwd>/<session>/plan.md` through its `search_replace` edit tool (the adapter re-reads it after each edit); the `exit_plan_mode` tool call carries no text — the plan reaches the client on the gate request as `planContent` | `enter_plan_mode`/`exit_plan_mode` tools; the exit arrives as the extension reverse request `_x.ai/exit_plan_mode {sessionId, toolCallId, planContent}` (live-captured, `crates/harness/tests/fixtures/grok-plan-mode.json`), answered `{"approved": bool, "abandoned": false}`; a method-not-found reply cancels the turn |
| Hermes, pi (ACP) | generic: supported only when `session/new.modes.availableModes` lists a `plan` id; `session/set_mode` | `current_mode_update` | ACP `plan` update (already the live todo chip) | generic `request_permission` when the agent raises one |
| Mock | scripted | scripted | scripted | scripted |

**Feedback delivery.** "Keep planning" feedback is the user's next MESSAGE on every harness,
delivered by the engine through the ordinary steer path (`RespondPlanExit{feedback}` →
reject the gate → `Steer{prompt: feedback}`): a visible user entry, a segment split, and the
CLI's own boundary delivery. The adapter only rejects its gate — Claude's deny carries the
CLI's generic rejection sentence, OpenCode answers "No", Grok answers `approved: false`. This
is what each CLI does itself: Claude Code's dialog denies with that sentence and sends the
typed feedback as a user message (`feedbackIsFromUser`); OpenCode's and Grok's users type it
after answering No. Raw feedback inside a tool error read to the model as an injected
instruction (2026-09-02).

The composer takes typed feedback for a parked gate on every harness ("Describe what to
change in the plan…"). Delivery follows the harness's steering: a step-boundary steerer
(Claude) takes the message inside the turn, right after the rejected gate. A turn-boundary
agent (Grok, OpenCode, Cursor, Hermes, pi) would queue it behind a turn that, after a
rejection, asks its own follow-up question or raises the gate again — the feedback would wait
behind the user forever — so the host cancels that turn first (the same abort Claude's TUI
does) and the feedback opens the next turn on the resumed session. Grok's ACP gate reply
carries no feedback field (live-probed: ~30 candidate names, none reached the "The user
said:" branch), so this is the native path there. A message sent while a run is parked on a
question or gate is queued and the session keeps reading AwaitingInput, never Working.
That cancel is a *replacement*, not a Stop: the engine suppresses the cancelled run's terminal
Idle (`interrupt_for_replacement`), so the Working→Idle edge — the "done" chime and the
settled sidebar dot, on every device — never fires for a turn the user did not end. The same
path covers a live run restarted for a changed run configuration.

**Question panels never submit on their own.** A pick on the last page stays put; the user
presses Submit (or Skip, which resolves the request with no answers — the "declined" signal
every adapter already carries). Auto-advance between pages remains.

**Gate tools are the card, not chips.** The tool calls that ARE the gate (`ExitPlanMode` /
`EnterPlanMode`, `exit_plan_mode` / `enter_plan_mode`, `plan_exit` / `plan_enter`,
`createPlan`) never fold into tool chips: the plan card is their rendering, and a rejected
gate would otherwise read as a failed tool. Adapters still derive `PlanUpdated` /
`PlanModeChanged` from them.

**Agent questions on extension channels.** Grok asks its user questions through the ACP
extension request `_x.ai/ask_user_question {sessionId, toolCallId, questions[{question,
options[{label, description}], multiSelect}], mode}`, answered `{"outcome":"accepted",
"answers":{<question text>: <label | [labels]>}}` (live-captured; a reply without `outcome`
fails the tool). It rides the same input bridge as Claude's `AskUserQuestion`.

**Permissions elsewhere unchanged.** Every other `can_use_tool` / approval keeps today's
unattended auto-approve. The one thing that stops being auto-approved is the plan exit gate.

### 11.3 One architecture, per-harness adapters

```
composer toggle ──SetChatConfig{plan_mode}──▶ registry (LWW)            ┐ requested
                └─QueueCommand SetPlanMode──▶ host ─watch──▶ adapter    │
                                                                        ▼
adapter ──PlanModeChanged / PlanUpdated / PlanExitRequested──▶ engine fold ──▶ session doc
                                                                        │       MessagePart::Plan
card Approve/Keep planning ──QueueCommand RespondPlanExit──▶ host ─oneshot──▶ adapter
```

- `comet-proto`: `RunRequest.plan_mode: bool`, `ChatConfig.plan_mode: bool` (both serde-default
  false, additive); `AgentEvent::{PlanModeChanged{active}, PlanUpdated{text, path},
  PlanExitRequested{request_id}, PlanExitResolved{request_id, approved}}`; `PlanDecision`.
  `HarnessDescriptor.plan_mode: bool` (engine registry) so composers gate the toggle by
  descriptor, never by harness id.
- `comet-harness`: `Harness::plan_mode() -> bool` (default false). `RunControls` gains
  `plan_mode: watch::Receiver<bool>` (initial = `request.plan_mode`; the adapter applies each
  change through the CLI's live switch) and `request_plan_exit: Fn(PlanExitRequest) ->
  oneshot::Receiver<PlanDecision>` — the same shape as `request_input`, and like it the ENGINE
  mints the request id and emits `PlanExitRequested`/`PlanExitResolved` (an adapter must never
  emit its own copy; §10-era bug class). Plan-file reading lives in the adapter whose CLI
  keeps a plan file; nothing outside `crates/harness` knows a plan path.
- `comet-doc`: `MessagePart::Plan { id, plan, status, request_id, path }` with `status ∈
  {drafting, awaitingApproval, approved, revising}`. The body field is `plan` (never `text`)
  so a pre-plan desktop build renders an invisible part and iOS drops the unknown kind — the
  `Reasoning` precedent. One plan part per segment, fixed id `plan`, refreshed in place by
  every `PlanUpdated` (the `LIVE_PLAN_TOOL_ID` singleton trick). Text capped at 128 KB.
  Ledger: `SessionCommandPayload::{RespondPlanExit{request_id, approved, feedback},
  SetPlanMode{active}}`. No new RPC method.
- `comet-engine`: `RunHandle` carries the `watch::Sender<bool>` and a `pending_plan_exits`
  map mirroring `pending_inputs`; `respond_plan_exit()` mirrors `respond_input()`;
  `set_plan_mode()` pushes into a live run and is a no-op `Applied` when idle (the next Run
  carries the config value). `PlanExitRequested` sets `SessionStatus::AwaitingInput`; the
  quiesce watchdog counts an `awaitingApproval` plan part as in flight. `RuntimeConfig`
  excludes `plan_mode`: a mode change never replaces the process; routing a Run into a live
  runtime first pushes the request's mode through the watch. On `PlanModeChanged` the host
  writes `ChatConfig.plan_mode` (reconcile). `rpc.rs` stays harness-agnostic.

### 11.4 Session lifecycle

1. Toggle on (idle chat): `SetChatConfig{plan_mode:true}`; the next send's `RunRequest.plan_mode`
   is true; the adapter launches in plan mode (Claude flag, Cursor `mode`, OpenCode `agent`,
   ACP `set_mode` right after `session/new`). The adapter reports `PlanModeChanged(true)` from
   the CLI's own signal where one exists.
2. Toggle while a run is live: `SetChatConfig` + `QueueCommand SetPlanMode`; the host pushes the
   watch; the adapter applies the CLI's live switch (`set_permission_mode`, `session/set_mode`,
   next `send`/`prompt_async` mode). A later Run routed into the same runtime pushes its mode
   first, then steers.
3. Planning: every plan-text change → `PlanUpdated` → the segment's plan part refreshes →
   both UIs repaint the card (STREAM_COMMIT_MS coalescing as for any part).
4. Exit gate: adapter → `request_plan_exit` → engine `PlanExitRequested` → part `awaitingApproval`,
   session `AwaitingInput`. The card shows Approve / Keep planning; the composer's placeholder
   invites feedback and its send resolves the gate with `approved:false` + the text.
   `RespondPlanExit` (ledger, host-executed, idempotent by request id) → resolver → the adapter
   answers the wire → `PlanExitResolved` → part `approved`/`revising`. Approval also yields
   `PlanModeChanged(false)` from the CLI's own signal, which reconciles the toggle.
5. No gate (Cursor, Codex, an ACP agent without one): the plan part stays `drafting` with the
   final text; the user flips the toggle and sends the next message, as in the CLI.
6. Recovery: a gate still parked when its run ends (Stop, an error, the turn finishing) is
   answered "keep planning" as the run tears down, and its part settles `revising` with it —
   never a silent approval, and never a card left actionable on a dead request id. Unlike an
   unanswered question there is no orphan fallback: a later `RespondPlanExit` is rejected,
   because re-asking the gate would be comet inventing an approval. A run revived after an
   engine restart relaunches with the chat's requested mode.

### 11.5 Plan state and sync

The plan part is the plan state: host-written, replicated through the session doc's existing
row protocol to every device and to the phone's `SessionStore` — no sidecar, no meta key, no
second channel. Each plan-mode episode leaves its card in the transcript at the turn that
produced it (as the CLIs' own transcripts do); within an episode the card updates in place.
`ChatConfig.plan_mode` is the only other bit and it already syncs through the registry.

### 11.6 Chat card (both surfaces)

Comet chip card (radius 9, `hairline(0.07)` border, `ink(0.03)` wash; iOS `whiteAlpha` twins):

- Header: the active harness's brand icon tile (`ChatConfig.harness` → `icons::harness_brand_icon`
  / `BrandMark.forHarness`; never a language/file icon), "Plan" label, the plan's first `#`
  heading (else "Plan"), a right-aligned status pill (Drafting… / Awaiting approval / Approved /
  Revising), chevron.
- Body: the plan markdown through the shared markdown renderer (fenced code blocks styled as
  in the diff card); expanded while drafting or awaiting approval, collapsed once approved,
  toggled with the chip fold tween (`Motion.resize` on iOS); reduced motion honored.
- Actions (only `awaitingApproval`): **Approve** and **Keep planning** → `RespondPlanExit`.
- Composer: a "Plan" toggle chip beside model/traits, shown only when the resolved harness
  descriptor has `plan_mode`. Same behavior on the phone (`ComposerChip`).

### 11.7 macOS / iOS sharing

Shared: proto types, doc part + fold, ledger payloads, engine (all Rust), and the phone's
Swift mirrors of exactly those (`MessagePart.plan`, `ChatConfig.planMode`,
`RunRequest.planMode`, `respondPlanExit`/`setPlanMode` commands). Platform-only: the card view
and the toggle chip, each on its platform's existing chip/markdown primitives.

### 11.8 Verification — the loop every workstream runs

`scripts/verify-plan-mode.sh` (mirrors `verify-skills.sh`; `--live` adds ignored real-CLI tests,
`--ios` the phone unit tests):

1. Guards: no `HarnessId::` in the plan paths of `crates/ui/src/transcript.rs`,
   `crates/ui/src/composer.rs`, `crates/engine/src/rpc.rs`, or the phone's plan files; no
   plan-file path handling (`plans/`) and no plan/implement prompt text outside `crates/harness`.
2. Proto/doc: old-wire JSON without `planMode` parses; `Plan` part round-trip; fold lifecycle
   drafting → awaitingApproval → approved/revising; continuation split with a large plan.
3. Engine: `RespondPlanExit` resolves the parked resolver and emits `PlanExitResolved`;
   `SetPlanMode` reaches a live run's watch and is `Applied` when idle; `PlanExitRequested` →
   `AwaitingInput`; reconcile writes `ChatConfig.plan_mode`.
4. Adapters against fixture CLIs: fake-claude `plan` scenario (init `permissionMode`, plan-file
   Write, `ExitPlanMode` `can_use_tool`; the recorded allow/deny payloads and the
   `set_permission_mode` line); fake-acp (`set_mode`, `current_mode_update`, exit
   `request_permission`); fake-cursor-shim (`mode` on run/steer, `createPlan`); opencode fake
   server (`agent` on prompt, `plan_exit` question routing); codex schema tripwire.
5. UI: row building from `Plan` parts; toggle hidden for a descriptor without `plan_mode`;
   iOS `partFrom` decode + card row tests.
6. fmt on changed files, clippy with no new diagnostics (`scripts/clippy-new-warnings.py`).

Rule: no feature lands without a test in this loop that failed before the change and passes
after it.

### 11.9 `/plan` — the one composer-owned slash command

Every CLI with a plan mode also offers it as a slash command: Claude Code's `/plan`
("Enable plan mode or view the current session plan", hint `[open|share|<description>]`) is a
TUI-local command (`local-jsx`) — it never reaches the model over stream-json, so passing the
text through does nothing; Grok's `/plan [description]` ("Enter plan mode") is the same shape;
Codex's `/plan` switches its collaboration mode; Cursor and OpenCode have none. §10 forbids
comet inventing commands, and this is the deliberate exception, for the same reason plan mode
itself is a comet toggle: the command IS the toggle, and the toggle already drives each
harness's own mode switch natively.

- `/plan` enters plan mode for the chat (exactly the composer chip); `/plan <description>`
  enters plan mode and sends the description as the prompt. Leaving plan mode is the chip
  (the CLIs' Shift+Tab); no `/plan off` exists anywhere natively, so none here.
- Offered only where the resolved harness's descriptor has `plan_mode` (§11.6); elsewhere
  `/plan …` is ordinary prompt text, exactly as §10.5 says for an unknown `/name`.
- Listed first in the slash popup with comet's own description ("Enter plan mode",
  hint `[description]`), and it SHADOWS a catalog entry of the same name (Claude and Grok both
  list `plan`): one behavior, one report path (`ChatConfig.plan_mode` → `RunRequest.plan_mode`
  / `SetPlanMode`, reconciled from the harness's reported mode), on every harness.
- One grammar (`comet_harness::commands::split_invocation`), one resolver per surface
  (`composer_builtin` on the desktop, `composerBuiltin` on the phone), pure and tested; neither
  names a harness.

### 11.10 Workstreams (parallel, isolated)

Shared substrate first (proto, harness trait, doc, engine, mock, verify skeleton); then, each
owning only its files and fixtures: **Claude**, **Codex** (decode + tripwire), **Cursor**,
**OpenCode**, **ACP** (Grok live-verify), **desktop composer + card**, **phone composer + card**.
