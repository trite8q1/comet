# Sidebar organization: philosophies, options, decisions

Living design doc for the left sidebar's project/chat organization.
Companion to the `left-sidebar-v1` branch. ADRs get extracted to `docs/adr/`
once a direction is locked.

- Status: synthesis locked (see ADR 0003), implemented on `left-sidebar-v1`
- Updated: 2026-08-31

## Where we are (after left-sidebar-v1)

Organize modes in the sidebar view-options menu:

| Mode | Origin | Behavior |
|---|---|---|
| Group by project (default) | this branch | One folder per Space (device-scoped). Same repo on two machines = two folders, device named in the header. |
| Group by project, all devices | this branch | One folder per project name, merged across devices. Device tagged per chat when a folder spans machines. |
| By device | pre-existing | Sections per device, local promoted first. |
| In one list | pre-existing (old default) | Flat recency feed. |

Sort (Last updated / Created) and Show (Branch / Pull request / Harness) are
pre-existing and unchanged. Folder rank = its most-recent chat under the
current Sort, so activity moves whole folders ("folder jump").

## The tension

Two goods that pull against each other:

1. **Spatial stability.** Folders in fixed positions build muscle memory
   (Codex macOS, Cursor agent window). Nothing jumps while you work.
2. **Activity surfacing.** The thing that just finished should be easy to
   find (our current recency behavior, chat-app style).

Today we have only 2, applied to folders. A completed run yanks its whole
folder to the top.

## Philosophy A: two modes (classic)

Default mode: everything fixed. Projects hold manual positions
(drag-and-drop), chats inside are stable. A separate "activity" mode switches
to the recency-driven list.

- Pro: each mode is pure; matches Codex/Cursor precedent.
- Con: a new mode axis on top of Organize + Sort. More surface, and the
  two modes largely duplicate what Organize already expresses.

## Philosophy B: one folder mode, stable, merged (modern)

Drop "Group by project" (per-device folders). Keep only "Group by project,
all devices" as THE folder view: the device is secondary metadata (a tag on
the chat row), not structure. Folders sit in fixed, drag-and-droppable
positions. Sort keeps meaning inside folders. No separate activity mode.

- Pro: one folder concept, no duplicate-project confusion, drag has an
  obvious meaning, device stays visible where it matters (per chat).
- Con: loses "most recent thing at the very top of the sidebar" while in
  folder view; needs an answer for how activity stays findable.

## Synthesis (recommended)

Philosophy A's two modes already exist as Organize modes. We don't need a
new mode axis:

- **"In one list" IS activity mode.** The flat recency feed, unchanged.
- **"Group by project, all devices" becomes the stable workspace view.**
  Folders never reorder on activity. Manual order via drag-and-drop,
  persisted. Sort applies to chats inside folders (and to the flat list).
- **Per-device "Group by project" is removed** from the menu (persisted
  value normalizes to the merged mode, the same mechanism previously used
  for the legacy ByProject value).
- **Activity surfaces without movement**: status dots on chat rows, plus an
  aggregate attention dot on the folder header when a nested chat is
  Working / AwaitingInput / Completed-unseen (helper already exists:
  `attention_rank`, `crates/proto/src/view.rs:75`). A collapsed folder still
  shows its dot, so nothing gets lost.

Ordering rules for the stable folder view:

- First materialization seeds folder order by recency (what you would have
  seen anyway), then it stays fixed.
- New projects insert at the top (you just started working there).
- Drag-and-drop reorders; order persists in settings (revive the legacy
  `space_order` field, `crates/ui/src/settings.rs:228`).
- "No project" stays pinned last, not draggable.

## Reusable components (inventory)

| Need | Existing piece | Where |
|---|---|---|
| Persisted manual order | legacy `space_order: Vec<String>` (kept for file compat, currently unread) | `crates/ui/src/settings.rs:228` |
| Drag payload + ghost + drop slot + slide tween | surface-tab strip reorder (`RightTabDrag`, `SurfaceTabGhost`, `update_right_tab_drag_over`) | `crates/ui/src/shell.rs:6425-6530` |
| Drop-slot math (x-axis; needs a y/variable-height variant for folders) | `terminal::panel::drop_index(rel, slot, count)` | `crates/ui/src/terminal/panel.rs:91` |
| Reorder glide animation | FLIP resort in the sidebar list | `crates/ui/src/shell.rs` (`render_chat_sidebar`) |
| Folder rendering, disclosure, per-folder pager | this branch | `crates/ui/src/shell/spaces.rs` (`render_folder_rows`) |
| Aggregate urgency for a folder dot | `attention_rank` | `crates/proto/src/view.rs:75` |
| Legacy-value normalization pattern | old ByProject→InOneList normalization (removed on this branch, same technique applies) | `crates/ui/src/settings.rs` (`clamped`) |

## Decisions (locked 2026-08-31)

- [x] Synthesis locked: stable merged folders + "In one list" as the
      activity feed. Per-device folders removed from the menu.
- [x] Merged-folder identity stays the lowercased display name. A rename
      re-buckets (the folder re-enters at the top as a "new" project, the
      old key lingers harmlessly); accepted as a rare, self-healing case.
- [x] Manual positions live in a new `sidebar_folder_order` settings field
      (name keys, topmost first). The legacy `space_order` field stays
      untouched — its entries are space ids, a different key space.
- [x] Folder attention dot AND the collapsed "(count)" coexist.
- [x] Drag-and-drop ships in v1 (menu label: "By project").

## Decision log

| Date | Decision |
|---|---|
| 2026-08-30 | Folder views added; folders default. Device labels only when informative; local device wears the composer's "Local" tag treatment. |
| 2026-08-30 | Chat rows in folders drop the "project @ device" subline (compact rows). |
| 2026-08-31 | Folder-jump problem identified: folder rank rides its newest chat. Two philosophies captured; synthesis proposed, not yet locked. |
| 2026-08-31 | Synthesis locked and implemented (ADR 0003): single "By project" folder mode (merged, default), stable manual order seeded from recency (new projects prepend, "No project" pinned last), drag-and-drop reorder persisted in `sidebar_folder_order`, attention dot on folder headers. Sort keeps ordering chats inside folders. |
| 2026-08-31 | Archived zone nests its chats in project folders when the folder view is active: same header/disclosure/per-folder pager as the top zone, dimmed whole; independent collapse state (`archived-folder:` keys). Folder order there is plain recency — no manual order, no drag, no dots (they serve active work). The same project appearing active on top and archived below is lifecycle, not duplication. |
