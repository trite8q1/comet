# Composer completions: `/` commands and `@` file mentions on every surface

The desktop composer (`crates/ui/src/composer.rs`) has two completion popups: `/`
lists the active harness's advertised commands, `@` searches the session checkout
and inserts a file chip. This document fixes the rules both surfaces share so the
phone (`apps/ios/Comet`) is an adapter over the same design rather than a fork.
Slash-command discovery and routing are specified in `ARCHITECTURE.md` §10; this
document covers the composer side of both features and everything about mentions.

Rule of the port: macOS is the source of truth for *behavior*. Its implementation
was read critically; §8 lists what is a rule (kept everywhere) and what is an
accident of gpui or of a past bug (dropped or adapted).

## 1. Layers

```
engine  ─ ListCommands {harness, cwd}            ─ SearchFiles {query, chatId | spaceId(+path)}
          catalog of one harness on one device     fuzzy walk of one verified checkout, ≤8 hits
            │                                           │
rules   ─ token grammars · rank/filter · accept · dismiss · freshness · states · link format
          (pure functions, no UI, no harness names; one copy per language, pinned by shared vectors)
            │                                           │
surface ─ composer.rs (gpui)                         ComposerView / NewSessionView (SwiftUI)
          renders state, routes keys/taps, calls RPCs, owns the editor widget
```

- **Engine** owns everything that needs a filesystem or a CLI: catalogs are answered by
  the harness's own wire (§10.4), file search walks a checkout the engine has verified
  belongs to a local chat or space (`crates/engine/src/rpc.rs` `file_search_root`),
  honors `.gitignore`, ranks, and caps at 8 results. No surface re-implements search.
- **Rules** are pure and surface-neutral. On the desktop they live in `composer.rs`
  (`slash_token`, `mention_token`, `filter_commands`, `local_file_link`,
  `file_mention_links`, `mention_display_labels`, `TextProjection`). On the phone they
  are ported one-to-one into `SlashCommands.swift`, `FileMentions.swift`, and
  `MentionDraft.swift`, and pinned by unit tests that reuse the desktop test vectors.
- **Surfaces** render state and translate platform input. They never decide what a
  token is, what a chip serializes to, or which catalog to show.

Boundary rules, executable by `scripts/verify-skills.sh`:

- No harness identifier is matched in any composer completion path (desktop `crates/ui`,
  engine `rpc.rs`, phone `Composer/`). The catalog and the search root are inputs.
- `SKILL.md` is parsed only inside `crates/harness`.
- The mention link format is defined once per surface, in the rules layer, and every
  producer/consumer (composer insert, transcript projection) goes through it.

## 2. Composer input model

Both surfaces expose the same abstract input to the rules layer:

| Concept | Desktop | Phone |
| --- | --- | --- |
| Raw text | `ComposerInput.content` — Markdown, chips are `[label](comet-file:…)` links | `MentionDraft.serialized()` — computed from the attributed draft on demand |
| Display text | `TextProjection.display` — chips collapsed to `\u{00A0}@label\u{00A0}` | `String(draft.text.characters)` — the attributed draft *is* the display |
| Caret | byte offset into raw text | character offset into display text |
| Chip identity | link byte range in raw text | attributed run carrying `FileMentionAttribute` |

The desktop keeps raw text as the source of truth and projects chips for display. The
phone inverts this: the `AttributedString` the editor edits is the source of truth, and
the raw prompt is derived at send time. Both directions are total and deterministic, so
the same draft produces the same sent prompt. The inversion is forced by the platform:
on iOS 26 only `TextEditor` binds an `AttributedString` with a selection
(`TextField` cannot), and there is no paint hook to project text under a native editor.

Token grammars run over the **display** text with the caret as a character offset.
Chips can never sit inside a token because the caret is snapped to chip boundaries
(§5.3) and a chip's display text starts and ends with a non-breaking space, which both
grammars treat as whitespace.

### 2.1 Token grammars (identical on both surfaces)

`slashToken(text, cursor)` (desktop `slash_token`):

- `/` must be the first character of the draft. The token runs to the first whitespace.
- The caret must be inside the token (`0 < cursor ≤ end`); in the argument it is closed.
- The query (`/` to caret) must not contain another `/` (a typed path is not a command).

`mentionToken(text, cursor)` (desktop `mention_token`):

- The token starts after the last whitespace before the caret; the `@` is the last `@`
  in that token before the caret; nothing between `@` and the caret may be another `@`.
- The `@` must begin the token: at offset 0, or preceded by whitespace or one of `(`, `[`,
  `{`. `mail@example.com`, `word@file`, `path/@file` never open.
- The token range extends past the caret to the next whitespace (or end of text), so
  accepting replaces the whole word the user was completing.
- Query = text between `@` and the caret.

The two grammars are mutually exclusive by shape (`/` at offset 0 vs `@` at a token
boundary). One popup slot renders whichever token is live; slash wins when both could
match, exactly as the desktop event router does.

### 2.2 Accept replacement (shared by both popups)

`tokenReplacement(text, range, inserted)` (desktop `replace_plain_token` /
`replace_mention`):

- The token range is replaced by `inserted`.
- If the character after the range is whitespace other than `\n`/`\r`, no separator is
  added and the caret lands after that existing separator; otherwise a single space is
  appended and the caret lands after it. Arguments (slash) or prose (mention) follow.
- For slash, `inserted` is `/name`. For mentions, `inserted` is one chip run (§5).
- Desktop: one non-coalescing undo step. Phone: the native editor's undo applies.

### 2.3 Dismiss and reopen (shared)

- Dismiss hides the popup for **this exact token text** (`range` + text). Moving the
  caret within the token keeps it closed; any edit that changes the token text reopens.
- The token disappearing (caret leaves, text edited so the grammar no longer matches)
  resets all popup state: results, error, loading, selection, in-flight generation.
- Desktop dismiss: Escape or mouse-down outside. Phone: no dismiss control (there is no
  Escape, and no reliable outside-tap that does not also move the caret); the card
  closes when its token does, and `dismiss(in:)` stays in the models for parity.

## 3. Command panel

Specified in `ARCHITECTURE.md` §10.2–§10.6; the phone already implements it in
`SlashCommands.swift` (`SlashCommandsModel`) and `SlashCommandPopup`. Invariants restated
so the mention work does not erode them:

- Catalog key is `(deviceId, harness, cwd)`; switching any component swaps the list and
  never shows the previous key's rows.
- One `ListCommands` per popup open, never per keystroke, never a second while one is in
  flight for the same key. Cached rows show immediately; the reply replaces them.
- Filter: `match_rank` (0 prefix, 1 substring, case-insensitive, empty query matches all
  at rank 1) over name and aliases, best rank wins, catalog order breaks ties.
- States in order: hidden → loading (only when the key has no rows) → failed (only when
  the key has no rows) → no commands → no matches → rows.
- Accept inserts `/name` + separator (§2.2). The prompt is sent as plain text through the
  durable command queue; the harness owns translation.

Parity fix carried by this work: the phone surfaced raw `error.localizedDescription`
for a failed probe. Both popups now use one error mapper (§4.5) so the version-skew and
offline cases read the same on both surfaces.

## 4. File search

### 4.1 RPC contract (unchanged, engine-owned)

`SearchFiles` (`crates/engine/src/rpc.rs`, forwardable over the device room):

```
params: { query: String (≤256 chars), chatId?: String, spaceId?: String, path?: String }
reply : [ { path: String, isDir: Bool } ]   // FileSearchMatch, ≤ 8, ranked
```

Exactly one of `chatId` / `spaceId`. `path` is accepted only with `spaceId` and names an
existing linked worktree the user picked for a not-yet-created chat; the engine verifies
it against the space repository before walking. With `chatId` and an empty query the
engine prepends *featured* paths (files the chat's tools touched most recently). Paths
are checkout-relative, `/`-separated, directories flagged by `isDir`.

The scope a surface sends:

| Surface state | Scope |
| --- | --- |
| Existing chat | `chat(chatId)` on `chat.deviceId` |
| New chat, space picked, reusing an existing worktree | `space(spaceId, path: worktreePath)` on `space.deviceId` |
| New chat, space picked, otherwise | `space(spaceId, path: nil)` on `space.deviceId` |
| No chat and no space | no scope: the popup renders "No files available" without an RPC |

The desktop adds `targetDeviceId` because its engine forwards; the phone's relay client
already addresses one device per connection, so it sends none (same as `ListCommands`).

### 4.2 Request discipline (desktop rules, kept)

- **Debounce 80 ms** after the token changes before sending; a token change during the
  wait cancels the pending send.
- **Generation guard**: every token change bumps a request generation; a reply is applied
  only if its generation is current *and* a token is still live.
- **Refining keeps rows**: when a token changes into another token, the previous results
  stay visible until the new reply lands (no skeleton flash). A fresh open (no token →
  token) clears results and shows the loading state.
- **Selection resets** to the first row on each new result set (desktop keyboard cursor);
  on the phone a tap chooses, so "active row" is not rendered but the model keeps the
  same shape for parity tests.
- Retry on transport failure: the desktop retries once after 250 ms to ride out a cold
  relay dial. The phone's `DeviceRelayClient.call` already retries `notConnected` /
  `hostOffline` up to three times, so no extra retry is layered on top.

### 4.3 Popup states (desktop `render_file_mention_popup`, in its order)

1. loading, and no rows yet → skeleton (phone: spinner + "Searching files…")
2. error → the mapped message in the danger tone
3. no rows → "No files available" when the query is empty, else "No matching files"
4. rows → icon (folder / document) · file name · muted directory, truncated tail

A failure never renders as "No matching files" (desktop user report: cross-device
failures are actionable and the empty state hid them).

Every state (rows, loading, error, empty) sits under the card's caption — "COMMANDS" on
the `/` card, "FILES" on the `@` one — and the row list hugs its measured height, capped
at 180pt.

### 4.4 Row rendering

`path.rsplitOnce("/")` → (directory, name); name in the body size, directory muted and
truncated. A directory match shows the folder icon. Tapping a row accepts it.

### 4.5 Error mapping (one mapper, both popups)

| Failure | Message |
| --- | --- |
| unknown method (host daemon predates the RPC) | "The session's device runs an older comet — update it to search its files" / "… to list commands" |
| device offline / not connected / timeout | "The session's device is unreachable" |
| anything else | "File search failed" / "Couldn't load this agent's commands" |

Phone detection: `RelayError.rpc(message)` whose message starts with `unknown method` is
the version-skew case; `.hostOffline`, `.notConnected`, `.timeout` are unreachable.

## 5. Chip model

A chip is one file mention: `FileMention { path: String, isDir: Bool }`, `path`
checkout-relative without a trailing slash. Everything else is derived.

### 5.1 Labels

- Default label: the basename.
- When two chips in the same draft share a basename, each uses the shortest path suffix
  (whole components) that is unique among the draft's chips (`mention_display_labels`).
  `src/one/mod.rs` + `src/two/mod.rs` → `one/mod.rs`, `two/mod.rs`. Suffix comparison is
  by path component, never by substring (`foo/mod.rs` vs `bar/oomod.rs` stay `mod.rs` /
  `oomod.rs`).
- Labels are recomputed over the whole draft whenever a chip is added or removed.

### 5.2 Display text

A chip displays as `\u{00A0}@label\u{00A0}` with spaces inside the label replaced by
`\u{00A0}`. The side bearings keep the chip a single word for both grammars and give the
wash room. Style: mono font, code text color, rounded code wash (desktop
`theme.font_mono` / `code_text` / `code_wash`; phone `Theme.mono` /
`Theme.inlineCodeText` / `Theme.inlineCodeWash`).

### 5.3 Atomicity

- The caret never rests inside a chip. A selection landing inside snaps to the nearer
  boundary (desktop `normalize_range`: midpoint rule; phone: same rule on
  `AttributedTextSelection`).
- A range selection overlapping a chip expands to cover the whole chip.
- Deleting into a chip deletes the whole chip.
- Text typed adjacent to a chip is never part of it (phone: `inheritedByAddedText = false`
  on the attribute key).

Phone realization, in `MentionDraft`:

- The chip run carries `FileMentionAttribute` (value: `FileMention`, the label it was
  rendered with, and a per-chip id so two chips for one file never coalesce into one
  run when they come to touch). A formatting definition (`AttributedTextFormattingDefinition`)
  derives font, foreground and background from the attribute's presence, so typed text
  can never inherit chip styling and chip styling can never be lost while the attribute
  is present.
- **Reconcile rule** on every text change: a chip run is valid iff its characters equal
  the canonical display of its stored label. Any run that differs (a backspace ate its
  trailing bearing, a character was typed into it) is removed entirely. Then labels are
  recomputed (§5.1) and runs whose label changed are rewritten. This one rule implements
  whole-chip deletion and keeps the draft canonical without tracking edit intents.

### 5.4 Tooltip

The desktop shows the full path in a hover tooltip after 420 ms. Hover does not exist
on the phone; a chip's full path is visible in the popup row that inserted it and in the
sent bubble's context menu (Copy copies the raw prompt). No long-press affordance is
added; nothing depends on it.

## 6. Serialization

The prompt that reaches the command queue is Markdown text. A chip serializes to the
strict link the desktop already emits and the transcript already reads:

```
[<label-escaped>](comet-file:<percent-encoded path>[/])
```

- `label-escaped` is the **basename** (never the deduped display label) with `\`, `[`,
  `]` backslash-escaped.
- The target is the path percent-encoded byte-wise: `A–Z a–z 0–9 - . _ ~ /` pass through,
  every other byte becomes `%XX` (uppercase hex). A directory gets a trailing `/`.
- Example: `src/a file#[x].rs` → `[a file#\[x\].rs](comet-file:src/a%20file%23%5Bx%5D.rs)`;
  directory `src/components` → `[components](comet-file:src/components/)`.

Parsing (`file_mention_links` / `fileMentionLinks`) is strict and is the only way a link
becomes a chip anywhere:

- `[` … `](` … `)` with backslash escapes honored in the label; the target must start
  with `comet-file:`.
- The decoded path must be non-empty, relative (no leading `/`), contain no `\`, no
  control characters, no empty / `.` / `..` component.
- Re-encoding the decoded target must reproduce the encoded target byte-for-byte (a
  non-canonical encoding is not a mention).
- The label must equal the escaped basename of the decoded path.
- Anything failing these stays ordinary text: `[a.rs](../a.rs)`, `[other](src/a.rs)`,
  `https://…` links, and prose that merely says `comet-file:`.

Serialization is the whole submission story: **no new RPC, no new doc field, no
attachment**. The engine and the harness adapters see a user message containing a
Markdown link, exactly as the desktop sends today. Slash invocations and the attachment
trailer (`withAttachments`) compose with it unchanged: chips are inline text.

### 6.1 Transcript projection

Sent user messages are projected for display on both surfaces
(`sent_mention_display`): every valid mention link collapses to its chip display text
(§5.2) with labels deduped across the message; everything else is untouched. Messages
without a valid link take the zero-cost path. The phone's `UserBubble` renders the
projection as attributed text with the existing inline-code wash renderer. Badges and
attachment trailers are split off *before* projection, as the desktop does.

## 7. Harness boundaries

- The composer never knows which harness it is talking to beyond the opaque
  `harness` string it forwards to `ListCommands`. Mentions do not even carry that: the
  search root comes from the chat or space row, never from the harness.
- The engine validates every search root against rows it hosts; a remote client cannot
  turn `SearchFiles` into an arbitrary path probe.
- Agents receive the Markdown link as text. Whether a CLI resolves `comet-file:` links,
  reads the path, or ignores it is that harness's business; comet does not rewrite the
  link per harness (no adapter does today).

## 8. What the port keeps and what it drops

Kept as rules: both grammars verbatim · match ranking · accept/separator/caret rule ·
dismiss-by-token-text · freshness (one probe per open) · 80 ms debounce · generation
guard · refine-keeps-rows · state order and copy · error mapping and its version-skew
case · strict link grammar and validator · basename labels with unique suffixes ·
chip display text and styling · chip atomicity · transcript projection.

Dropped or adapted as accidents of the desktop widget or its history:

| Desktop detail | Why it is not a rule | Phone |
| --- | --- | --- |
| Hover tooltip state machine (420 ms, generations, popup bounds) | pointer-only affordance | none (§5.4) |
| Extra 250 ms transport retry inside the composer | compensates for the desktop RPC client having no retry | relay client retries already |
| Popup scrollbar hover/drag state, `ScrollHandle` resets | gpui has no native scroll view | `ScrollView` |
| Keyboard row cursor (↑/↓, Tab/Enter accept) | needs a hardware keyboard | tap accepts; the model keeps `active` for parity |
| Mouse-down-outside dismiss | pointer affordance | none; the card closes with its token |
| `targetDeviceId` on every params object | desktop engine forwards across devices | relay is already per-device |
| Raw-text-as-truth with a paint-time projection | the only way to draw chips under a hand-rolled gpui input | attributed draft as truth, raw derived (§2) |
| Undo coalescing constants | editor implementation detail | native undo |
| `SentMentionSpan.path` carrying a trailing `/` for directories | leaks the encoding into a display type | `FileMention.isDir` |

## 9. Phone implementation map

New files (all under `apps/ios/Comet/Composer/` unless noted):

| File | Owns |
| --- | --- |
| `FileSearch.swift` | wire: `FileSearchMatch`, `FileSearchScope`, `fileSearchParams`, `FileSearchResult`; `completionErrorMessage` (§4.5) for both popups |
| `FileMentions.swift` | rules: `FileMention`, link encode/decode/validate (§6), `mentionDisplayLabels`, `mentionDisplayText`, `mentionToken`, `FileMentionsModel` (token, results, active, loading, error, dismissed, generation → `MentionPopup` state), `sentMentionDisplay` (§6.1) |
| `MentionDraft.swift` | editor model: `FileMentionAttribute`, `AttributeScopes.CometAttributes`, `MentionFormatting` (formatting definition), `MentionDraft` (attributed text; `display`, `caret(of:)`, `apply(_: TokenReplacement)`, `insertMention`, `reconcile`, `snapped(selection:)`, `serialized`, `isEmpty`); `TokenReplacement` |
| `ComposerEditor` (in `ComposerView.swift`) | the `TextEditor` bound to the attributed draft: placeholder, growth to 7 lines then internal scroll, focus, formatting definition, reconcile and snap hooks. Replaces the `TextField` inside `ComposerShell` |
| `FileMentionPopup` (in `ComposerView.swift`) | the `@` card: states of §4.3, rows of §4.4, ✕ |
| `Sync/WorkspaceStore.swift`, `App/AppModel.swift`, `App/DemoDataset.swift` | `searchFiles(deviceId:scope:query:)` over the relay; demo stand-in |
| `Transcript/TranscriptView.swift` `UserBubble` | chip projection (§6.1) |
| `CometTests/FileMentionsTests.swift`, `CometTests/MentionDraftTests.swift` | desktop vectors ported; reconcile / snap / serialize round-trips |

`SlashCommands.swift` gains `slashReplacement(text:token:command:) -> TokenReplacement`
(the existing `slashAccept` becomes a thin wrapper so its tests stand) and the shared
error mapper replaces the raw relay message. `ComposerShell` takes
`draft: Binding<AttributedString>` and `selection: Binding<AttributedTextSelection>`;
`ComposerView` and `NewSessionView` hold one `MentionDraft` each, run `syncSlash` and
`syncMentions` on text/selection/key changes, and send `draft.serialized()`.

Editor swap risk: `ComposerShell`'s focus-driven expansion, tap-to-focus surface, and
"re-clear after send" workaround were built around `TextField`. The `ComposerEditor`
must preserve them; the model layer is unit-tested, the editor is verified by building
and by manual review on a device.

Layout rule for the popup slot, on both phone surfaces: the popup + composer stack is a
bottom **safe-area inset**, never a sibling in a stack that also holds a greedy view. A
sibling stack SHARES its height, and with the keyboard up there is not enough of it — the
stack then shrinks the one child that can shrink, the popup's row list (`maxHeight: 180`),
to zero, and the card renders as a bare ✕ strip (the §4.3 states never get to draw). An
inset is sized to its own content, so the rows keep their height. On the new-session
canvas the inset still has to take that height from somewhere, and the mark + "What are
we building?" hold a ~126pt floor under the canvas: while a popup is open the decoration
is hidden, so the card gets its full height instead of two rows.

## 10. Verification

- `scripts/verify-skills.sh --ios` runs `CometTests/SlashCommandsTests`,
  `CometTests/FileMentionsTests`, and `CometTests/MentionDraftTests`.
- Static guards extend to the new files: no harness ids in the phone's rules files
  (`SlashCommands.swift`, `FileSearch.swift`, `FileMentions.swift`, `MentionDraft.swift`);
  `comet-file:` is spelled in exactly one place per surface (`composer.rs`,
  `FileMentions.swift`).
- Vectors shared with the desktop tests: grammar (`Fix @src/com`, `mail@example.com`,
  `word@file`, `path/@file`, `See (@lib`), link round-trips (`src/a file#[x].rs`,
  `src/components/`), rejections (§6), label dedupe, projection spans, popup state order,
  dismissal, generation staleness.
